//! Intake for item actions: the `get`, `drop`, `give`, `receive`, `bribe`,
//! `wear`, `remove` and `consume` triggers, and the verbs that carry them.
//!
//! Expectations come from `act.item.c` / `act.other.c` / `spell_parser.c` and
//! from the dispatch notes in `dg-vm`, never from what the code returns.
//!
//! The app has the DG runtime, the item intake and a stand-in for the engine's
//! router. The intake carries out `get`, `drop`, `put`, `equip` and `unequip`
//! itself and marks them handled; the stand-in calls the engine's own
//! `handle_get`, `handle_drop` and `handle_put` (the real item code, on the real
//! components) for whatever is left unhandled, skips handled commands and marks
//! each command handled afterwards exactly as `route_commands` does for a raw
//! line, so a command the intake failed to take over would be done twice and
//! show. The real router needs a connection, a session table and a dozen
//! unrelated resources. The pack's `equip` handler runs as it does in the game.
//!
//! Triggers prove they ran, and with what, by promoting their event variables
//! to globals with `global`. Tests may unwrap/expect.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_arguments)]

mod dg_common;

use dg_common::*;
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::dg::DgLog;
use mib_tba::dg::intake::items::give::DgNpcGold;
use mib_tba::equipment::EquipmentVetoes;
use mib_tba::prelude::*;

// -- the app ---------------------------------------------------------------------

/// Makes the stand-in leave the whole frame to the command gate.
#[derive(Resource, Default)]
struct GateTakesEverything(bool);

fn gate_takes_everything(
    flag: Res<GateTakesEverything>,
    mut reader: MessageReader<CommandReceived>,
    mut handled: ResMut<HandledCommands>,
) {
    for msg in reader.read() {
        if flag.0 {
            handled.mark_handled(msg.id);
        }
    }
}

/// The engine's router, for the commands the engine parses.
fn engine_stand_in(
    mut reader: MessageReader<CommandReceived>,
    mut handled: ResMut<HandledCommands>,
    players: Query<(&Player, &Location)>,
    items: ItemQuery,
    containers: ContainerQuery,
    mut commands: Commands,
    mut events: MessageWriter<QuestEvent>,
    mut output: MessageWriter<OutputEvent>,
) {
    for msg in reader.read() {
        if handled.is_handled(msg.id) {
            continue;
        }
        let command = match &msg.input {
            PlayerInput::Command(command) => Some(command.clone()),
            PlayerInput::RawLine(line) => parse_line(line).ok(),
        };
        match command {
            Some(Command::Get { item, from }) => handle_get(
                msg.session,
                &item,
                from.as_deref(),
                &players,
                &items,
                &containers,
                &mut commands,
                &mut events,
                &mut output,
            ),
            Some(Command::Drop { item }) => {
                handle_drop(
                    msg.session,
                    &item,
                    0.0,
                    &players,
                    &items,
                    &mut commands,
                    &mut output,
                );
            }
            Some(Command::Put { item, into }) => handle_put(
                msg.session,
                &item,
                &into,
                &players,
                &items,
                &containers,
                &mut commands,
                &mut output,
            ),
            _ => {}
        }
        // `route_commands` marks every raw line handled once it has run.
        if matches!(msg.input, PlayerInput::RawLine(_)) {
            handled.mark_handled(msg.id);
        }
    }
}

fn items_app() -> App {
    let mut app = app();
    app.init_resource::<HandledCommands>();
    app.init_resource::<CommandSequence>();
    app.init_resource::<GateTakesEverything>();
    app.add_message::<CommandReceived>();
    app.configure_sets(Update, (CommandSet::PreRoute, CommandSet::Route).chain());
    app.configure_sets(
        Update,
        (
            mib_tba::dg::intake::DgIntake::CommandGate,
            mib_tba::dg::intake::DgIntake::PreCommit,
        )
            .chain()
            .in_set(CommandSet::PreRoute),
    );
    mib_tba::dg::intake::items::register(&mut app);
    app.add_systems(
        Update,
        (
            gate_takes_everything.in_set(mib_tba::dg::intake::DgIntake::CommandGate),
            engine_stand_in.in_set(CommandSet::Route),
            handle_equipment_commands.after(engine_stand_in),
        ),
    );
    app
}

/// Type a line as `who`, run a frame, and return what the game said.
fn say(app: &mut App, who: Entity, line: &str) -> String {
    let id = app.world_mut().resource_mut::<CommandSequence>().next_id();
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(
            id,
            who,
            1,
            PlayerInput::RawLine(line.to_owned()),
        ));
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
    output(app).join("\n")
}

struct Scene {
    app: App,
    temple: Entity,
    alice: Entity,
    guard: Entity,
}

fn scene() -> Scene {
    let mut app = items_app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    app.world_mut().entity_mut(alice).insert(TbaGold(100));
    let guard = npc(&mut app, temple, 10, "the guard");
    Scene {
        app,
        temple,
        alice,
        guard,
    }
}

/// The recorded value of a global variable on an entity.
fn seen(app: &mut App, owner: Entity, name: &str) -> Option<String> {
    let id = uid(app, owner);
    global(app, id, name)
}

fn id_of(app: &mut App, entity: Entity) -> String {
    format!("{}{}", dg_vm::types::UID_CHAR, uid(app, entity))
}

fn on_ground(app: &App, item: Entity) -> bool {
    app.world().get::<GroundItem>(item).is_some() && app.world().get::<CarriedBy>(item).is_none()
}

fn carried_by(app: &App, item: Entity) -> Option<Entity> {
    app.world().get::<CarriedBy>(item).map(|carried| carried.0)
}

fn worn(app: &App, holder: Entity, slot: WearSlot) -> Option<Entity> {
    app.world()
        .get::<TbaEquipment>(holder)
        .and_then(|equipment| equipment.worn(slot))
}

/// An object trigger (`flags` is the type letter) that records its actor and
/// returns `ret`.
fn obj_trigger(app: &mut App, vnum: i32, flags: &str, item_vnum: u32, body: &str) {
    define(app, vnum, "obj", flags, 100, body);
    attach(app, &format!("item:tba_item_{item_vnum}"), &[vnum]);
}

const REFUSE: &str = "global actor\nglobal victim\nglobal object\nglobal command\nreturn 0";
const ALLOW: &str = "global actor\nglobal victim\nglobal object\nglobal command\nreturn 1";

// -- get ---------------------------------------------------------------------------

#[test]
fn get_trigger_sees_the_actor_and_a_refusal_leaves_the_object() {
    let mut s = scene();
    let sword = item_on_ground(&mut s.app, s.temple, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "g", 5, REFUSE);

    say(&mut s.app, s.alice, "get sword");
    let alice_id = id_of(&mut s.app, s.alice);
    assert_eq!(seen(&mut s.app, sword, "actor"), Some(alice_id));
    assert!(on_ground(&s.app, sword), "the refused object stays");
    assert_eq!(carried_by(&s.app, sword), None);
}

#[test]
fn get_trigger_that_allows_lets_the_engine_take_the_object() {
    let mut s = scene();
    let sword = item_on_ground(&mut s.app, s.temple, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "g", 5, ALLOW);

    let said = say(&mut s.app, s.alice, "get sword");
    assert!(seen(&mut s.app, sword, "actor").is_some(), "it ran");
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    assert!(said.contains("You pick up a rusty sword."), "{said}");
}

#[test]
fn get_trigger_sees_the_object_the_engine_acts_on() {
    // Two matches: `get sword` takes the first, `get 2.sword` the second.
    let mut s = scene();
    let first = item_on_ground(&mut s.app, s.temple, 5, "a rusty sword");
    let second = item_on_ground(&mut s.app, s.temple, 6, "a rusty sword");
    obj_trigger(&mut s.app, 100, "g", 5, ALLOW);
    obj_trigger(&mut s.app, 101, "g", 6, REFUSE);

    say(&mut s.app, s.alice, "get 2.sword");
    assert!(seen(&mut s.app, second, "actor").is_some());
    assert!(seen(&mut s.app, first, "actor").is_none());
    assert!(on_ground(&s.app, second), "the second sword was refused");
    assert!(on_ground(&s.app, first), "the first was never asked");

    say(&mut s.app, s.alice, "get sword");
    assert!(seen(&mut s.app, first, "actor").is_some());
    assert_eq!(carried_by(&s.app, first), Some(s.alice));
    assert!(on_ground(&s.app, second));
}

#[test]
fn get_from_a_container_asks_the_contained_object() {
    let mut s = scene();
    let chest = item_on_ground(&mut s.app, s.temple, 7, "a wooden chest");
    let gem = item_inside(&mut s.app, chest, 8, "a green gem");
    s.app.world_mut().entity_mut(chest).insert(Container {
        capacity: 10,
        contents: vec![gem],
    });
    obj_trigger(&mut s.app, 100, "g", 8, REFUSE);

    say(&mut s.app, s.alice, "get gem from chest");
    assert!(seen(&mut s.app, gem, "actor").is_some());
    assert_eq!(
        s.app.world().get::<ContainedBy>(gem).map(|c| c.0),
        Some(chest),
        "a refused object stays in the container"
    );

    // Allowed: the engine takes it.
    attach(&mut s.app, "item:tba_item_8", &[]);
    let gem2 = item_inside(&mut s.app, chest, 9, "a red gem");
    s.app
        .world_mut()
        .get_mut::<Container>(chest)
        .unwrap()
        .contents
        .push(gem2);
    obj_trigger(&mut s.app, 101, "g", 9, ALLOW);
    say(&mut s.app, s.alice, "get red from chest");
    assert!(seen(&mut s.app, gem2, "actor").is_some());
    assert_eq!(carried_by(&s.app, gem2), Some(s.alice));
}

#[test]
fn get_does_not_ask_for_what_the_engine_would_not_take() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let other = item_on_ground(&mut s.app, s.temple, 6, "a rusty sword");
    obj_trigger(&mut s.app, 100, "g", 6, REFUSE);

    // Already carrying a sword: the engine says so and takes nothing.
    let said = say(&mut s.app, s.alice, "get sword");
    assert!(said.contains("already carrying"), "{said}");
    assert!(seen(&mut s.app, other, "actor").is_none());
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    // Nothing by that name: no trigger, the engine answers.
    let said = say(&mut s.app, s.alice, "get banana");
    assert!(said.contains("don't see"), "{said}");
}

// -- drop and put ----------------------------------------------------------------------

#[test]
fn drop_trigger_refusal_keeps_the_object_and_allowing_drops_it() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "h", 5, REFUSE);

    say(&mut s.app, s.alice, "drop sword");
    let alice_id = id_of(&mut s.app, s.alice);
    assert_eq!(seen(&mut s.app, sword, "actor"), Some(alice_id));
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));

    attach(&mut s.app, "item:tba_item_5", &[]);
    s.app
        .world_mut()
        .resource_mut::<mib_tba::dg::script::DgProtos>()
        .set_attachments("item:tba_item_5", vec![]);
    obj_trigger(&mut s.app, 101, "h", 6, ALLOW);
    let ball = item_carried(&mut s.app, s.alice, 6, "a red ball");
    let said = say(&mut s.app, s.alice, "drop ball");
    assert!(seen(&mut s.app, ball, "actor").is_some());
    assert!(on_ground(&s.app, ball));
    assert!(said.contains("You drop a red ball."), "{said}");
}

#[test]
fn drop_asks_the_object_then_the_room() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "h", 5, ALLOW);
    define(&mut s.app, 200, "wld", "h", 100, REFUSE);
    attach(&mut s.app, "room:tba.1", &[200]);

    say(&mut s.app, s.alice, "drop sword");
    let alice_id = id_of(&mut s.app, s.alice);
    let sword_id = id_of(&mut s.app, sword);
    assert!(
        seen(&mut s.app, sword, "actor").is_some(),
        "drop_otrigger ran"
    );
    assert_eq!(seen(&mut s.app, s.temple, "actor"), Some(alice_id));
    assert_eq!(seen(&mut s.app, s.temple, "object"), Some(sword_id));
    assert_eq!(carried_by(&s.app, sword), Some(s.alice), "the room refused");
}

#[test]
fn an_object_refusal_stops_the_drop_before_the_room_is_asked() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "h", 5, REFUSE);
    define(&mut s.app, 200, "wld", "h", 100, ALLOW);
    attach(&mut s.app, "room:tba.1", &[200]);

    say(&mut s.app, s.alice, "drop sword");
    assert!(seen(&mut s.app, s.temple, "actor").is_none());
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
}

#[test]
fn a_cursed_object_is_refused_after_its_triggers() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    define_item(&mut s.app, 5, TbaItemKind::Weapon, &[], &["sword"]);
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_5"))
        .unwrap()
        .extra_flags
        .push(ItemExtraFlag::NoDrop);
    obj_trigger(&mut s.app, 100, "h", 5, ALLOW);

    let said = say(&mut s.app, s.alice, "drop sword");
    assert!(seen(&mut s.app, sword, "actor").is_some());
    assert!(said.contains("it must be CURSED"), "{said}");
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
}

#[test]
fn put_asks_the_drop_trigger_of_the_object() {
    let mut s = scene();
    let chest = item_on_ground(&mut s.app, s.temple, 7, "a wooden chest");
    s.app.world_mut().entity_mut(chest).insert(Container {
        capacity: 10,
        contents: vec![],
    });
    let gem = item_carried(&mut s.app, s.alice, 8, "a green gem");
    obj_trigger(&mut s.app, 100, "h", 8, REFUSE);

    say(&mut s.app, s.alice, "put gem in chest");
    assert!(seen(&mut s.app, gem, "actor").is_some());
    assert_eq!(carried_by(&s.app, gem), Some(s.alice), "refused");

    s.app
        .world_mut()
        .resource_mut::<mib_tba::dg::script::DgProtos>()
        .set_attachments("item:tba_item_8", vec![]);
    let gem2 = item_carried(&mut s.app, s.alice, 9, "a red gem");
    obj_trigger(&mut s.app, 101, "h", 9, ALLOW);
    let said = say(&mut s.app, s.alice, "put red in chest");
    assert!(seen(&mut s.app, gem2, "actor").is_some());
    assert!(
        said.contains("You put a red gem in a wooden chest."),
        "{said}"
    );
    assert_eq!(
        s.app.world().get::<ContainedBy>(gem2).map(|c| c.0),
        Some(chest)
    );
}

// -- give --------------------------------------------------------------------------------

#[test]
fn give_to_an_npc_runs_give_then_receive_and_the_npc_carries_the_object() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "i", 5, ALLOW);
    define(&mut s.app, 300, "mob", "j", 100, ALLOW);
    attach(&mut s.app, "npc:tba_npc_10", &[300]);

    let said = say(&mut s.app, s.alice, "give sword guard");
    let (alice_id, guard_id, sword_id) = (
        id_of(&mut s.app, s.alice),
        id_of(&mut s.app, s.guard),
        id_of(&mut s.app, sword),
    );
    assert_eq!(seen(&mut s.app, sword, "actor"), Some(alice_id.clone()));
    assert_eq!(seen(&mut s.app, sword, "victim"), Some(guard_id));
    assert_eq!(seen(&mut s.app, s.guard, "actor"), Some(alice_id));
    assert_eq!(seen(&mut s.app, s.guard, "object"), Some(sword_id));
    assert_eq!(carried_by(&s.app, sword), Some(s.guard));
    assert!(
        said.contains("You give a rusty sword to the guard."),
        "{said}"
    );
}

#[test]
fn give_accepts_the_to_of_the_c_grammar() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    say(&mut s.app, s.alice, "give sword to guard");
    assert_eq!(carried_by(&s.app, sword), Some(s.guard));
}

#[test]
fn a_give_trigger_that_refuses_stops_the_gift_before_the_mob_hears_of_it() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "i", 5, REFUSE);
    define(&mut s.app, 300, "mob", "j", 100, ALLOW);
    attach(&mut s.app, "npc:tba_npc_10", &[300]);

    say(&mut s.app, s.alice, "give sword guard");
    assert!(seen(&mut s.app, sword, "actor").is_some());
    assert!(
        seen(&mut s.app, s.guard, "actor").is_none(),
        "receive never ran"
    );
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
}

#[test]
fn a_receive_trigger_that_refuses_keeps_the_object_with_the_giver() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "i", 5, ALLOW);
    define(&mut s.app, 300, "mob", "j", 100, REFUSE);
    attach(&mut s.app, "npc:tba_npc_10", &[300]);

    say(&mut s.app, s.alice, "give sword guard");
    assert!(seen(&mut s.app, s.guard, "actor").is_some());
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
}

#[test]
fn give_to_a_player_moves_the_object_and_tells_the_quests() {
    let mut s = scene();
    let bob = player(&mut s.app, s.temple, "Bob", 2);
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let said = say(&mut s.app, s.alice, "give sword bob");
    assert_eq!(carried_by(&s.app, sword), Some(bob));
    assert!(said.contains("You give a rusty sword to Bob."), "{said}");
    assert!(said.contains("Alice gives you a rusty sword."), "{said}");
    let collected: Vec<_> = s
        .app
        .world_mut()
        .resource_mut::<Messages<QuestEvent>>()
        .drain()
        .collect();
    assert!(collected.iter().any(
        |event| matches!(event, QuestEvent::Collected { player, item } if *player == bob && item.as_ref() == "tba_item_5")
    ));
}

#[test]
fn give_names_the_object_the_giver_would_name_with_a_count() {
    let mut s = scene();
    let first = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let second = item_carried(&mut s.app, s.alice, 6, "a rusty sword");
    obj_trigger(&mut s.app, 100, "i", 5, ALLOW);
    obj_trigger(&mut s.app, 101, "i", 6, ALLOW);

    // `2.sword` is the second match in the order the C lists a character's
    // things: newest first, so the first sword.
    say(&mut s.app, s.alice, "give 2.sword guard");
    assert!(seen(&mut s.app, first, "actor").is_some());
    assert!(seen(&mut s.app, second, "actor").is_none());
    assert_eq!(carried_by(&s.app, first), Some(s.guard));
    assert_eq!(carried_by(&s.app, second), Some(s.alice));
}

#[test]
fn give_a_number_of_objects() {
    let mut s = scene();
    let a = item_carried(&mut s.app, s.alice, 5, "a coin purse");
    let b = item_carried(&mut s.app, s.alice, 6, "a coin purse");
    let c = item_carried(&mut s.app, s.alice, 7, "a coin purse");
    say(&mut s.app, s.alice, "give 2 purse guard");
    let given = [a, b, c]
        .into_iter()
        .filter(|item| carried_by(&s.app, *item) == Some(s.guard))
        .count();
    assert_eq!(given, 2);
}

#[test]
fn give_says_why_it_cannot() {
    let mut s = scene();
    item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    assert!(say(&mut s.app, s.alice, "give").contains("Give what to who?"));
    assert!(say(&mut s.app, s.alice, "give sword").contains("To who?"));
    assert!(say(&mut s.app, s.alice, "give sword nobody").contains("No one by that name here."));
    assert!(say(&mut s.app, s.alice, "give sword me").contains("What's the point of that?"));
    assert!(
        say(&mut s.app, s.alice, "give banana guard").contains("You don't seem to have a banana.")
    );
    assert!(say(&mut s.app, s.alice, "give 3").contains("What do you want to give 3 of?"));
}

#[test]
fn coins_leave_the_purse_and_reach_the_npc_and_the_bribe_trigger_runs() {
    let mut s = scene();
    define(
        &mut s.app,
        301,
        "mob",
        "m",
        20,
        "global actor\nglobal amount",
    );
    attach(&mut s.app, "npc:tba_npc_10", &[301]);

    let said = say(&mut s.app, s.alice, "give 30 coins guard");
    assert_eq!(s.app.world().get::<TbaGold>(s.alice).unwrap().0, 70);
    assert_eq!(s.app.world().get::<DgNpcGold>(s.guard).unwrap().gold, 30);
    let alice_id = id_of(&mut s.app, s.alice);
    assert_eq!(seen(&mut s.app, s.guard, "actor"), Some(alice_id));
    assert_eq!(seen(&mut s.app, s.guard, "amount"), Some("30".to_owned()));
    assert!(said.contains("Okay."), "{said}");
}

#[test]
fn a_bribe_below_the_trigger_amount_does_not_run_it() {
    let mut s = scene();
    define(&mut s.app, 301, "mob", "m", 50, "global amount");
    attach(&mut s.app, "npc:tba_npc_10", &[301]);

    say(&mut s.app, s.alice, "give 30 coins guard");
    assert_eq!(s.app.world().get::<DgNpcGold>(s.guard).unwrap().gold, 30);
    assert!(seen(&mut s.app, s.guard, "amount").is_none());
}

#[test]
fn coins_to_a_player_and_the_refusals() {
    let mut s = scene();
    let bob = player(&mut s.app, s.temple, "Bob", 2);
    s.app.world_mut().entity_mut(bob).insert(TbaGold(5));
    let said = say(&mut s.app, s.alice, "give 1 coin bob");
    assert_eq!(s.app.world().get::<TbaGold>(s.alice).unwrap().0, 99);
    assert_eq!(s.app.world().get::<TbaGold>(bob).unwrap().0, 6);
    assert!(said.contains("Alice gives you 1 gold coin."), "{said}");

    let said = say(&mut s.app, s.alice, "give 1000 coins bob");
    assert!(said.contains("You don't have that many coins!"), "{said}");
    assert_eq!(s.app.world().get::<TbaGold>(s.alice).unwrap().0, 99);
    let said = say(&mut s.app, s.alice, "give 0 coins bob");
    assert!(said.contains("we are jolly funny today"), "{said}");
    let said = say(&mut s.app, s.alice, "give -5 coins bob");
    assert!(said.contains("we are jolly funny today"), "{said}");
    assert_eq!(s.app.world().get::<TbaGold>(bob).unwrap().0, 6);
}

// -- wear, wield, hold and remove -------------------------------------------------------------

fn gear(app: &mut App) {
    define_item(
        app,
        20,
        TbaItemKind::Armor,
        &[WearSlot::Body],
        &["vest", "leather"],
    );
    for vnum in [21, 22, 23] {
        define_item(
            app,
            vnum,
            TbaItemKind::Armor,
            &[WearSlot::FingerR, WearSlot::FingerL],
            &["ring"],
        );
    }
    define_item(app, 24, TbaItemKind::Armor, &[WearSlot::About], &["cloak"]);
    define_item(
        app,
        25,
        TbaItemKind::Armor,
        &[WearSlot::WristR, WearSlot::WristL],
        &["bracelet"],
    );
    define_item(app, 30, TbaItemKind::Weapon, &[WearSlot::Wield], &["sword"]);
    define_item(app, 31, TbaItemKind::Light, &[], &["torch"]);
    define_item(app, 32, TbaItemKind::Wand, &[], &["wand"]);
    define_item(
        app,
        33,
        TbaItemKind::Weapon,
        &[WearSlot::Wield],
        &["hammer"],
    );
    app.world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_33"))
        .unwrap()
        .weight = 40;
    define_item(app, 34, TbaItemKind::Armor, &[WearSlot::Head], &["helm"]);
    app.world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_34"))
        .unwrap()
        .min_level = 5;
}

#[test]
fn wear_puts_an_object_on_the_one_place_it_fits() {
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_carried(&mut s.app, s.alice, 20, "a leather vest");
    let said = say(&mut s.app, s.alice, "wear vest");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Body), Some(vest));
    assert!(
        said.contains("You wear a leather vest on your body."),
        "{said}"
    );
    // It is worn, not carried, to the scripts.
    run(&mut s.app, |host| {
        use dg_vm::WorldHost;
        let id = host.uid(vest).unwrap();
        assert_eq!(host.obj_carried_by(id), None);
        assert!(host.obj_worn_by(id).is_some());
    });
}

#[test]
fn rings_fill_the_right_finger_then_the_left_then_say_no() {
    let mut s = scene();
    gear(&mut s.app);
    let a = item_carried(&mut s.app, s.alice, 21, "a jade ring");
    let b = item_carried(&mut s.app, s.alice, 22, "a ruby ring");
    let c = item_carried(&mut s.app, s.alice, 23, "a gold ring");
    say(&mut s.app, s.alice, "wear jade");
    say(&mut s.app, s.alice, "wear ruby");
    assert_eq!(worn(&s.app, s.alice, WearSlot::FingerR), Some(a));
    assert_eq!(worn(&s.app, s.alice, WearSlot::FingerL), Some(b));
    let said = say(&mut s.app, s.alice, "wear gold");
    assert!(
        said.contains("You're already wearing something on both of your ring fingers."),
        "{said}"
    );
    assert_eq!(carried_by(&s.app, c), Some(s.alice));
    assert!(worn(&s.app, s.alice, WearSlot::Hold).is_none());
}

#[test]
fn an_explicit_body_part_is_honoured_or_refused() {
    let mut s = scene();
    gear(&mut s.app);
    let cloak = item_carried(&mut s.app, s.alice, 24, "a grey cloak");
    let bracelet = item_carried(&mut s.app, s.alice, 25, "a silver bracelet");
    let said = say(&mut s.app, s.alice, "wear cloak body");
    assert!(
        said.contains("You can't wear a grey cloak there."),
        "{said}"
    );
    assert!(worn(&s.app, s.alice, WearSlot::About).is_none());
    let said = say(&mut s.app, s.alice, "wear cloak nonsense");
    assert!(
        said.contains("'nonsense'?  What part of your body is THAT?"),
        "{said}"
    );
    let said = say(&mut s.app, s.alice, "wear cloak about");
    assert!(
        said.contains("You wear a grey cloak around your body."),
        "{said}"
    );
    assert_eq!(worn(&s.app, s.alice, WearSlot::About), Some(cloak));
    say(&mut s.app, s.alice, "wear bracelet wrist");
    assert_eq!(worn(&s.app, s.alice, WearSlot::WristR), Some(bracelet));
}

#[test]
fn wield_and_hold_use_their_own_positions() {
    let mut s = scene();
    gear(&mut s.app);
    let sword = item_carried(&mut s.app, s.alice, 30, "a long sword");
    let torch = item_carried(&mut s.app, s.alice, 31, "a torch");
    let wand = item_carried(&mut s.app, s.alice, 32, "a twisted wand");
    let hammer = item_carried(&mut s.app, s.alice, 33, "a war hammer");
    let said = say(&mut s.app, s.alice, "wield sword");
    assert!(said.contains("You wield a long sword."), "{said}");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Wield), Some(sword));
    let said = say(&mut s.app, s.alice, "hold torch");
    assert!(said.contains("You light a torch and hold it."), "{said}");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Light), Some(torch));
    let said = say(&mut s.app, s.alice, "hold wand");
    assert!(said.contains("You grab a twisted wand."), "{said}");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Hold), Some(wand));
    let said = say(&mut s.app, s.alice, "wield hammer");
    assert!(said.contains("It's too heavy for you to use."), "{said}");
    assert_eq!(carried_by(&s.app, hammer), Some(s.alice));
    item_carried(&mut s.app, s.alice, 20, "a leather vest");
    let said = say(&mut s.app, s.alice, "wield vest");
    assert!(said.contains("You can't wield that."), "{said}");
}

#[test]
fn a_taken_position_and_a_missing_object_are_said() {
    let mut s = scene();
    gear(&mut s.app);
    item_carried(&mut s.app, s.alice, 20, "a leather vest");
    item_worn(&mut s.app, s.alice, WearSlot::Body, 24, "a padded jerkin");
    let said = say(&mut s.app, s.alice, "wear vest");
    assert!(
        said.contains("You're already wearing something on your body."),
        "{said}"
    );
    assert!(say(&mut s.app, s.alice, "wear").contains("Wear what?"));
    assert!(say(&mut s.app, s.alice, "wear banana").contains("You don't seem to have a banana."));
    assert!(say(&mut s.app, s.alice, "wield").contains("Wield what?"));
    assert!(say(&mut s.app, s.alice, "hold").contains("Hold what?"));
    assert!(say(&mut s.app, s.alice, "remove").contains("Remove what?"));
}

#[test]
fn an_object_above_the_wearers_level_is_refused() {
    let mut s = scene();
    gear(&mut s.app);
    let helm = item_carried(&mut s.app, s.alice, 34, "a steel helm");
    let said = say(&mut s.app, s.alice, "wear helm");
    assert!(
        said.contains("You are not experienced enough to use that."),
        "{said}"
    );
    assert!(worn(&s.app, s.alice, WearSlot::Head).is_none());
    s.app
        .world_mut()
        .get_mut::<TbaProgress>(s.alice)
        .unwrap()
        .level = 5;
    say(&mut s.app, s.alice, "wear helm");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Head), Some(helm));
}

#[test]
fn wear_all_wears_what_fits() {
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_carried(&mut s.app, s.alice, 20, "a leather vest");
    let cloak = item_carried(&mut s.app, s.alice, 24, "a grey cloak");
    let sword = item_carried(&mut s.app, s.alice, 30, "a long sword");
    say(&mut s.app, s.alice, "wear all");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Body), Some(vest));
    assert_eq!(worn(&s.app, s.alice, WearSlot::About), Some(cloak));
    assert!(
        worn(&s.app, s.alice, WearSlot::Wield).is_none(),
        "wear does not wield"
    );
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    let said = say(&mut s.app, s.alice, "wear all");
    assert!(
        said.contains("You don't seem to have anything wearable."),
        "{said}"
    );
}

#[test]
fn a_wear_trigger_sees_the_actor_and_a_refusal_keeps_the_object_off() {
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_carried(&mut s.app, s.alice, 20, "a leather vest");
    obj_trigger(&mut s.app, 100, "j", 20, REFUSE);
    say(&mut s.app, s.alice, "wear vest");
    let alice_id = id_of(&mut s.app, s.alice);
    assert_eq!(seen(&mut s.app, vest, "actor"), Some(alice_id));
    assert!(worn(&s.app, s.alice, WearSlot::Body).is_none());

    define(&mut s.app, 101, "obj", "j", 100, ALLOW);
    attach(&mut s.app, "item:tba_item_20", &[101]);
    let cloak = item_carried(&mut s.app, s.alice, 24, "a grey cloak");
    obj_trigger(&mut s.app, 102, "j", 24, ALLOW);
    say(&mut s.app, s.alice, "wear cloak");
    assert!(seen(&mut s.app, cloak, "actor").is_some());
    assert_eq!(worn(&s.app, s.alice, WearSlot::About), Some(cloak));
}

#[test]
fn wield_and_hold_ask_the_wear_trigger_too() {
    let mut s = scene();
    gear(&mut s.app);
    let sword = item_carried(&mut s.app, s.alice, 30, "a long sword");
    let wand = item_carried(&mut s.app, s.alice, 32, "a twisted wand");
    obj_trigger(&mut s.app, 100, "j", 30, REFUSE);
    obj_trigger(&mut s.app, 101, "j", 32, REFUSE);
    say(&mut s.app, s.alice, "wield sword");
    say(&mut s.app, s.alice, "hold wand");
    assert!(seen(&mut s.app, sword, "actor").is_some());
    assert!(seen(&mut s.app, wand, "actor").is_some());
    assert!(worn(&s.app, s.alice, WearSlot::Wield).is_none());
    assert!(worn(&s.app, s.alice, WearSlot::Hold).is_none());
}

#[test]
fn equip_asks_the_wear_trigger_and_a_refusal_blocks_it() {
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_carried(&mut s.app, s.alice, 20, "a leather vest");
    obj_trigger(&mut s.app, 100, "j", 20, REFUSE);
    let said = say(&mut s.app, s.alice, "equip vest");
    assert!(seen(&mut s.app, vest, "actor").is_some());
    assert!(worn(&s.app, s.alice, WearSlot::Body).is_none());
    assert!(!said.contains("You equip"), "{said}");
    assert!(
        s.app
            .world()
            .resource::<HandledCommands>()
            .is_handled(CommandId(1)),
        "a refusal marks the command handled"
    );
}

#[test]
fn equip_still_works_when_the_trigger_allows_it() {
    // The router marks a raw `equip` handled after it has run; the pack's
    // handler must still act on it.
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_carried(&mut s.app, s.alice, 20, "a leather vest");
    obj_trigger(&mut s.app, 100, "j", 20, ALLOW);
    let said = say(&mut s.app, s.alice, "equip vest");
    assert!(seen(&mut s.app, vest, "actor").is_some());
    assert_eq!(worn(&s.app, s.alice, WearSlot::Body), Some(vest));
    assert!(said.contains("You equip a leather vest (body)."), "{said}");
}

#[test]
fn equip_without_any_trigger_is_unchanged() {
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_carried(&mut s.app, s.alice, 20, "a leather vest");
    say(&mut s.app, s.alice, "equip vest");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Body), Some(vest));
    assert!(s.app.world().resource::<EquipmentVetoes>().is_empty());
}

#[test]
fn remove_asks_the_remove_trigger() {
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_worn(&mut s.app, s.alice, WearSlot::Body, 20, "a leather vest");
    obj_trigger(&mut s.app, 100, "l", 20, REFUSE);
    say(&mut s.app, s.alice, "remove vest");
    let alice_id = id_of(&mut s.app, s.alice);
    assert_eq!(seen(&mut s.app, vest, "actor"), Some(alice_id));
    assert_eq!(
        worn(&s.app, s.alice, WearSlot::Body),
        Some(vest),
        "still on"
    );

    define(&mut s.app, 101, "obj", "l", 100, ALLOW);
    attach(&mut s.app, "item:tba_item_20", &[101]);
    // A fresh trigger set is picked up by a fresh object.
    let cloak = item_worn(&mut s.app, s.alice, WearSlot::About, 24, "a grey cloak");
    obj_trigger(&mut s.app, 102, "l", 24, ALLOW);
    let said = say(&mut s.app, s.alice, "remove cloak");
    assert!(seen(&mut s.app, cloak, "actor").is_some());
    assert!(worn(&s.app, s.alice, WearSlot::About).is_none());
    assert_eq!(carried_by(&s.app, cloak), Some(s.alice));
    assert!(said.contains("You stop using a grey cloak."), "{said}");
}

#[test]
fn unequip_asks_the_remove_trigger_too() {
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_worn(&mut s.app, s.alice, WearSlot::Body, 20, "a leather vest");
    obj_trigger(&mut s.app, 100, "l", 20, REFUSE);
    say(&mut s.app, s.alice, "unequip body");
    assert!(seen(&mut s.app, vest, "actor").is_some());
    assert_eq!(worn(&s.app, s.alice, WearSlot::Body), Some(vest));

    let cloak = item_worn(&mut s.app, s.alice, WearSlot::About, 24, "a grey cloak");
    obj_trigger(&mut s.app, 101, "l", 24, ALLOW);
    say(&mut s.app, s.alice, "unequip about");
    assert!(seen(&mut s.app, cloak, "actor").is_some());
    assert!(worn(&s.app, s.alice, WearSlot::About).is_none());
}

#[test]
fn remove_by_name_count_and_all() {
    let mut s = scene();
    gear(&mut s.app);
    let a = item_worn(&mut s.app, s.alice, WearSlot::FingerR, 21, "a jade ring");
    let b = item_worn(&mut s.app, s.alice, WearSlot::FingerL, 22, "a ruby ring");
    let vest = item_worn(&mut s.app, s.alice, WearSlot::Body, 20, "a leather vest");
    // Equipment is walked in position order: right finger first.
    say(&mut s.app, s.alice, "remove 2.ring");
    assert_eq!(worn(&s.app, s.alice, WearSlot::FingerR), Some(a));
    assert!(worn(&s.app, s.alice, WearSlot::FingerL).is_none());
    assert_eq!(carried_by(&s.app, b), Some(s.alice));
    let said = say(&mut s.app, s.alice, "remove banana");
    assert!(
        said.contains("You don't seem to be using a banana."),
        "{said}"
    );
    say(&mut s.app, s.alice, "remove all");
    assert!(worn(&s.app, s.alice, WearSlot::Body).is_none());
    assert!(worn(&s.app, s.alice, WearSlot::FingerR).is_none());
    assert_eq!(carried_by(&s.app, vest), Some(s.alice));
    let said = say(&mut s.app, s.alice, "remove all");
    assert!(said.contains("You're not using anything."), "{said}");
}

#[test]
fn a_cursed_object_cannot_be_removed() {
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_worn(&mut s.app, s.alice, WearSlot::Body, 20, "a leather vest");
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_20"))
        .unwrap()
        .extra_flags
        .push(ItemExtraFlag::NoDrop);
    let said = say(&mut s.app, s.alice, "remove vest");
    assert!(said.contains("it must be CURSED"), "{said}");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Body), Some(vest));
}

// -- eat, drink, quaff ---------------------------------------------------------------------

fn provisions(app: &mut App) {
    define_item(app, 50, TbaItemKind::Food, &[], &["bread", "loaf"]);
    define_item(app, 51, TbaItemKind::DrinkContainer, &[], &["flask"]);
    define_item(app, 52, TbaItemKind::Fountain, &[], &["fountain"]);
    define_item(app, 53, TbaItemKind::Potion, &[], &["potion"]);
    for (vnum, values) in [(51, [10, 5, 1, 0]), (52, [-1, -1, 0, 0])] {
        app.world_mut()
            .resource_mut::<TbaContent>()
            .items
            .get_mut(&ItemId::from(format!("tba_item_{vnum}")))
            .unwrap()
            .values = values;
    }
}

#[test]
fn eating_runs_the_consume_trigger_then_uses_the_food_up() {
    let mut s = scene();
    provisions(&mut s.app);
    let bread = item_carried(&mut s.app, s.alice, 50, "a loaf of bread");
    obj_trigger(&mut s.app, 100, "s", 50, ALLOW);
    let said = say(&mut s.app, s.alice, "eat bread");
    assert!(said.contains("You eat a loaf of bread."), "{said}");
    assert!(s.app.world().get_entity(bread).is_err(), "eaten");
}

#[test]
fn the_consume_trigger_gets_the_actor_and_the_command() {
    let mut s = scene();
    provisions(&mut s.app);
    let bread = item_carried(&mut s.app, s.alice, 50, "a loaf of bread");
    obj_trigger(&mut s.app, 100, "s", 50, REFUSE);
    let said = say(&mut s.app, s.alice, "eat bread");
    let alice_id = id_of(&mut s.app, s.alice);
    assert_eq!(seen(&mut s.app, bread, "actor"), Some(alice_id));
    assert_eq!(seen(&mut s.app, bread, "command"), Some("eat".to_owned()));
    assert!(
        s.app.world().get_entity(bread).is_ok(),
        "a refusal keeps the food"
    );
    assert!(!said.contains("You eat"), "{said}");
}

#[test]
fn drinking_leaves_the_container_and_a_refusal_stops_it() {
    let mut s = scene();
    provisions(&mut s.app);
    let flask = item_carried(&mut s.app, s.alice, 51, "a leather flask");
    obj_trigger(&mut s.app, 100, "s", 51, REFUSE);
    let said = say(&mut s.app, s.alice, "drink flask");
    assert_eq!(seen(&mut s.app, flask, "command"), Some("drink".to_owned()));
    assert!(!said.contains("You drink"), "{said}");

    s.app
        .world_mut()
        .resource_mut::<mib_tba::dg::script::DgProtos>()
        .set_attachments("item:tba_item_51", vec![]);
    let flask2 = item_carried(&mut s.app, s.alice, 51, "a battered flask");
    obj_trigger(&mut s.app, 101, "s", 51, ALLOW);
    let said = say(&mut s.app, s.alice, "drink battered");
    assert!(said.contains("You drink the beer."), "{said}");
    assert_eq!(
        carried_by(&s.app, flask2),
        Some(s.alice),
        "the container stays"
    );
    assert!(s.app.world().resource::<DgLog>().count("drink_level") >= 1);
}

#[test]
fn sip_and_taste_of_a_drink_container_take_a_sip() {
    let mut s = scene();
    provisions(&mut s.app);
    let flask = item_carried(&mut s.app, s.alice, 51, "a leather flask");
    obj_trigger(&mut s.app, 100, "s", 51, ALLOW);
    let said = say(&mut s.app, s.alice, "sip flask");
    assert!(said.contains("It tastes like beer."), "{said}");
    assert_eq!(seen(&mut s.app, flask, "command"), Some("drink".to_owned()));
    let said = say(&mut s.app, s.alice, "taste flask");
    assert!(said.contains("It tastes like beer."), "{said}");
}

#[test]
fn a_fountain_in_the_room_can_be_drunk_from_but_a_flask_on_the_floor_cannot() {
    let mut s = scene();
    provisions(&mut s.app);
    let fountain = item_on_ground(&mut s.app, s.temple, 52, "a marble fountain");
    obj_trigger(&mut s.app, 100, "s", 52, ALLOW);
    let said = say(&mut s.app, s.alice, "drink fountain");
    assert!(said.contains("You drink the water."), "{said}");
    assert_eq!(
        seen(&mut s.app, fountain, "command"),
        Some("drink".to_owned())
    );
    item_on_ground(&mut s.app, s.temple, 51, "a leather flask");
    let said = say(&mut s.app, s.alice, "drink flask");
    assert!(
        said.contains("You have to be holding that to drink from it."),
        "{said}"
    );
    assert!(say(&mut s.app, s.alice, "drink").contains("Drink from what?"));
    assert!(say(&mut s.app, s.alice, "drink ghost").contains("You can't find it!"));
}

#[test]
fn an_empty_container_is_empty_and_runs_no_trigger() {
    let mut s = scene();
    provisions(&mut s.app);
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_51"))
        .unwrap()
        .values = [10, 0, 1, 0];
    let flask = item_carried(&mut s.app, s.alice, 51, "a leather flask");
    obj_trigger(&mut s.app, 100, "s", 51, ALLOW);
    let said = say(&mut s.app, s.alice, "drink flask");
    assert!(said.contains("It is empty."), "{said}");
    assert!(seen(&mut s.app, flask, "actor").is_none());
}

#[test]
fn quaffing_a_potion_consumes_it_and_casts_its_spells_through_the_host() {
    let mut s = scene();
    provisions(&mut s.app);
    let potion = item_carried(&mut s.app, s.alice, 53, "a bubbling potion");
    obj_trigger(&mut s.app, 100, "s", 53, ALLOW);
    let said = say(&mut s.app, s.alice, "quaff potion");
    assert_eq!(seen_gone(&mut s.app, potion), None);
    assert!(said.contains("You quaff a bubbling potion."), "{said}");
    assert!(s.app.world().get_entity(potion).is_err());
    // The spells go to the host's `call_magic`, which another stream fills in.
    assert_eq!(s.app.world().resource::<DgLog>().count("potion_spells"), 0);
}

/// The potion is gone, so a global cannot be read from it any more.
fn seen_gone(app: &mut App, entity: Entity) -> Option<String> {
    app.world().get_entity(entity).ok().map(|_| String::new())
}

#[test]
fn quaff_trigger_sees_the_command_and_a_refusal_keeps_the_potion() {
    let mut s = scene();
    provisions(&mut s.app);
    let potion = item_carried(&mut s.app, s.alice, 53, "a bubbling potion");
    obj_trigger(&mut s.app, 100, "s", 53, REFUSE);
    let said = say(&mut s.app, s.alice, "quaff potion");
    assert_eq!(
        seen(&mut s.app, potion, "command"),
        Some("quaff".to_owned())
    );
    assert!(s.app.world().get_entity(potion).is_ok());
    assert!(!said.contains("You quaff"), "{said}");
}

#[test]
fn a_held_potion_is_quaffed_first_and_leaves_the_equipment() {
    let mut s = scene();
    provisions(&mut s.app);
    let held = item_worn(&mut s.app, s.alice, WearSlot::Hold, 53, "a bubbling potion");
    let spare = item_carried(&mut s.app, s.alice, 53, "a bubbling potion");
    say(&mut s.app, s.alice, "quaff potion");
    assert!(s.app.world().get_entity(held).is_err());
    assert!(s.app.world().get_entity(spare).is_ok());
    assert!(worn(&s.app, s.alice, WearSlot::Hold).is_none());
}

#[test]
fn eat_and_quaff_refuse_the_wrong_kind_without_asking_the_trigger() {
    let mut s = scene();
    provisions(&mut s.app);
    gear(&mut s.app);
    let sword = item_carried(&mut s.app, s.alice, 30, "a long sword");
    let bread = item_carried(&mut s.app, s.alice, 50, "a loaf of bread");
    obj_trigger(&mut s.app, 100, "s", 30, ALLOW);
    obj_trigger(&mut s.app, 101, "s", 50, ALLOW);
    assert!(say(&mut s.app, s.alice, "eat sword").contains("You can't eat THAT!"));
    assert!(say(&mut s.app, s.alice, "quaff bread").contains("You can only quaff potions."));
    assert!(seen(&mut s.app, sword, "actor").is_none());
    assert!(seen(&mut s.app, bread, "actor").is_none());
    assert!(say(&mut s.app, s.alice, "eat").contains("Eat what?"));
    assert!(say(&mut s.app, s.alice, "quaff").contains("What do you want to quaff?"));
    assert!(say(&mut s.app, s.alice, "eat banana").contains("You don't seem to have a banana."));
}

#[test]
fn tasting_food_nibbles_it_and_leaves_it() {
    let mut s = scene();
    provisions(&mut s.app);
    let bread = item_carried(&mut s.app, s.alice, 50, "a loaf of bread");
    obj_trigger(&mut s.app, 100, "s", 50, ALLOW);
    let said = say(&mut s.app, s.alice, "taste bread");
    assert!(
        said.contains("You nibble a little bit of a loaf of bread."),
        "{said}"
    );
    assert_eq!(seen(&mut s.app, bread, "command"), Some("eat".to_owned()));
    assert!(s.app.world().get_entity(bread).is_ok());
}

// -- the stage in front of the router --------------------------------------------------------

#[test]
fn a_command_the_gate_took_over_asks_no_one() {
    let mut s = scene();
    gear(&mut s.app);
    let sword = item_on_ground(&mut s.app, s.temple, 5, "a rusty sword");
    let vest = item_carried(&mut s.app, s.alice, 20, "a leather vest");
    obj_trigger(&mut s.app, 100, "g", 5, ALLOW);
    obj_trigger(&mut s.app, 101, "j", 20, ALLOW);
    s.app.world_mut().resource_mut::<GateTakesEverything>().0 = true;

    say(&mut s.app, s.alice, "get sword");
    say(&mut s.app, s.alice, "equip vest");
    assert!(seen(&mut s.app, sword, "actor").is_none());
    assert!(seen(&mut s.app, vest, "actor").is_none());
    assert!(on_ground(&s.app, sword));
    assert!(
        worn(&s.app, s.alice, WearSlot::Body).is_none(),
        "equip skipped too"
    );
}

#[test]
fn nothing_happens_for_someone_who_is_not_playing() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    s.app.world_mut().get_mut::<Session>(s.alice).unwrap().state = SessionState::Connected;
    say(&mut s.app, s.alice, "give sword guard");
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
}

#[test]
fn a_despawned_receiver_or_object_does_not_panic() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    s.app.world_mut().despawn(s.guard);
    let said = say(&mut s.app, s.alice, "give sword guard");
    assert!(said.contains("No one by that name here."), "{said}");
    s.app.world_mut().despawn(sword);
    say(&mut s.app, s.alice, "give sword bob");
    say(&mut s.app, s.alice, "wear all");
    say(&mut s.app, s.alice, "remove all");
}

// -- ordering, resolution and the rest of the review ----------------------------------------------

/// Type several lines in one frame, as a client that sends a packet of them.
fn say_all(app: &mut App, who: Entity, lines: &[&str]) -> String {
    for line in lines {
        let id = app.world_mut().resource_mut::<CommandSequence>().next_id();
        app.world_mut()
            .resource_mut::<Messages<CommandReceived>>()
            .write(CommandReceived::new(
                id,
                who,
                1,
                PlayerInput::RawLine((*line).to_owned()),
            ));
    }
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
    output(app).join("\n")
}

fn give_save_id(app: &mut App, item: Entity, id: &str) {
    app.world_mut()
        .entity_mut(item)
        .insert(SaveId(smol_str::SmolStr::new(id)));
}

fn saved_worn(app: &App, holder: Entity) -> Vec<(WearSlot, String)> {
    app.world()
        .get::<TbaEquipmentSave>(holder)
        .map(|save| save.worn.iter().map(|(k, v)| (*k, v.clone())).collect())
        .unwrap_or_default()
}

fn combatant_at(position: Position) -> TbaCombatant {
    TbaCombatant {
        hp: 10,
        max_hp: 10,
        mana: 0,
        max_mana: 0,
        move_points: 10,
        max_move_points: 10,
        hitroll: 0,
        damroll: 0,
        armor_class: 0,
        thaco: 20,
        attributes: TbaAttributes::neutral(),
        saves: TbaSaves::default(),
        position,
        target: None,
    }
}

#[test]
fn commands_of_one_frame_run_in_the_order_they_were_typed() {
    let mut s = scene();
    provisions(&mut s.app);
    gear(&mut s.app);
    let bread = item_on_ground(&mut s.app, s.temple, 50, "a loaf of bread");
    let said = say_all(&mut s.app, s.alice, &["get bread", "eat bread"]);
    assert!(said.contains("You pick up a loaf of bread."), "{said}");
    assert!(said.contains("You eat a loaf of bread."), "{said}");
    assert!(s.app.world().get_entity(bread).is_err());

    let sword = item_on_ground(&mut s.app, s.temple, 30, "a long sword");
    say_all(&mut s.app, s.alice, &["get sword", "wield sword"]);
    assert_eq!(worn(&s.app, s.alice, WearSlot::Wield), Some(sword));

    let potion = item_on_ground(&mut s.app, s.temple, 53, "a bubbling potion");
    say_all(&mut s.app, s.alice, &["get potion", "quaff potion"]);
    assert!(s.app.world().get_entity(potion).is_err());

    // Equip is in the same order, and a drop after an equip finds nothing to drop.
    let vest = item_on_ground(&mut s.app, s.temple, 20, "a leather vest");
    let said = say_all(
        &mut s.app,
        s.alice,
        &["get vest", "equip vest", "drop vest"],
    );
    assert_eq!(worn(&s.app, s.alice, WearSlot::Body), Some(vest));
    assert!(said.contains("You aren't carrying that."), "{said}");
    assert_eq!(carried_by(&s.app, vest), Some(s.alice));

    let ball = item_on_ground(&mut s.app, s.temple, 6, "a red ball");
    say_all(&mut s.app, s.alice, &["get ball", "give ball guard"]);
    assert_eq!(carried_by(&s.app, ball), Some(s.guard));
}

#[test]
fn the_object_that_was_asked_is_the_object_that_moves() {
    // Two matching swords on the floor: the trigger of the second refuses and
    // the first is untouched, then the first is taken and the second is still
    // there; `drop` and `put` count the same way.
    let mut s = scene();
    gear(&mut s.app);
    let first = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let second = item_carried(&mut s.app, s.alice, 6, "a rusty sword");
    obj_trigger(&mut s.app, 100, "h", 6, REFUSE);
    obj_trigger(&mut s.app, 101, "h", 5, ALLOW);
    say(&mut s.app, s.alice, "drop 2.sword");
    assert!(seen(&mut s.app, second, "actor").is_some());
    assert!(seen(&mut s.app, first, "actor").is_none());
    assert_eq!(carried_by(&s.app, second), Some(s.alice));
    assert_eq!(carried_by(&s.app, first), Some(s.alice));
    say(&mut s.app, s.alice, "drop sword");
    assert!(seen(&mut s.app, first, "actor").is_some());
    assert!(on_ground(&s.app, first));

    let bag = item_carried(&mut s.app, s.alice, 7, "a leather bag");
    s.app.world_mut().entity_mut(bag).insert(Container {
        capacity: 5,
        contents: Vec::new(),
    });
    let a = item_carried(&mut s.app, s.alice, 8, "a green gem");
    let b = item_carried(&mut s.app, s.alice, 9, "a green gem");
    obj_trigger(&mut s.app, 102, "h", 9, REFUSE);
    say(&mut s.app, s.alice, "put 2.gem in bag");
    assert!(seen(&mut s.app, b, "actor").is_some());
    assert!(seen(&mut s.app, a, "actor").is_none());
    assert_eq!(carried_by(&s.app, b), Some(s.alice));
    say(&mut s.app, s.alice, "put gem in bag");
    assert_eq!(s.app.world().get::<ContainedBy>(a).map(|c| c.0), Some(bag));
    assert_eq!(
        s.app.world().get::<Container>(bag).unwrap().contents,
        vec![a]
    );
}

#[test]
fn equip_and_wear_count_the_way_the_command_does() {
    let mut s = scene();
    gear(&mut s.app);
    let first = item_carried(&mut s.app, s.alice, 21, "a jade ring");
    let second = item_carried(&mut s.app, s.alice, 22, "a ruby ring");
    obj_trigger(&mut s.app, 100, "j", 22, ALLOW);
    obj_trigger(&mut s.app, 101, "j", 21, REFUSE);
    say(&mut s.app, s.alice, "equip 2.ring");
    assert!(seen(&mut s.app, second, "actor").is_some());
    assert!(seen(&mut s.app, first, "actor").is_none());
    assert_eq!(worn(&s.app, s.alice, WearSlot::FingerR), Some(second));
    // `equip 3.ring` finds nothing, and no trigger runs for it.
    let said = say(&mut s.app, s.alice, "equip 3.ring");
    assert!(said.contains("You aren't carrying that."), "{said}");
    assert!(seen(&mut s.app, first, "actor").is_none());
    // `wear 2.ring` counts the same way: it is not the ring `wear 1.ring` takes.
    let mut s = scene();
    gear(&mut s.app);
    item_carried(&mut s.app, s.alice, 21, "a jade ring");
    item_carried(&mut s.app, s.alice, 22, "a ruby ring");
    say(&mut s.app, s.alice, "wear 2.ring");
    let by_count = worn(&s.app, s.alice, WearSlot::FingerR).unwrap();
    say(&mut s.app, s.alice, "wear 1.ring");
    let other = worn(&s.app, s.alice, WearSlot::FingerL).unwrap();
    assert_ne!(by_count, other);
}

#[test]
fn eat_counts_with_a_prefix() {
    let mut s = scene();
    provisions(&mut s.app);
    let first = item_carried(&mut s.app, s.alice, 50, "a loaf of bread");
    let second = item_carried(&mut s.app, s.alice, 50, "a loaf of bread");
    let _ = first;
    say(&mut s.app, s.alice, "eat 2.bread");
    let gone = [first, second]
        .iter()
        .filter(|entity| s.app.world().get_entity(**entity).is_err())
        .count();
    assert_eq!(gone, 1);
}

#[test]
fn put_takes_an_object_from_the_floor_through_its_triggers() {
    let mut s = scene();
    let bag = item_carried(&mut s.app, s.alice, 7, "a leather bag");
    s.app.world_mut().entity_mut(bag).insert(Container {
        capacity: 5,
        contents: Vec::new(),
    });
    let gem = item_on_ground(&mut s.app, s.temple, 8, "a green gem");
    obj_trigger(&mut s.app, 100, "g", 8, REFUSE);
    say(&mut s.app, s.alice, "put gem in bag");
    assert!(seen(&mut s.app, gem, "actor").is_some(), "get ran");
    assert!(on_ground(&s.app, gem));
    assert!(s.app.world().get::<ContainedBy>(gem).is_none());

    let ring = item_on_ground(&mut s.app, s.temple, 9, "a gold ring");
    obj_trigger(&mut s.app, 101, "h", 9, REFUSE);
    say(&mut s.app, s.alice, "put ring in bag");
    assert!(seen(&mut s.app, ring, "actor").is_some(), "drop ran");
    assert!(on_ground(&s.app, ring));

    let coin = item_on_ground(&mut s.app, s.temple, 10, "a silver cup");
    let said = say(&mut s.app, s.alice, "put cup in bag");
    assert_eq!(
        s.app.world().get::<ContainedBy>(coin).map(|c| c.0),
        Some(bag)
    );
    assert!(
        said.contains("You put a silver cup in a leather bag."),
        "{said}"
    );
    let said = say(&mut s.app, s.alice, "put bag in bag");
    assert!(said.contains("can't put something inside itself"), "{said}");
}

#[test]
fn a_worn_object_cannot_be_dropped_or_put() {
    let mut s = scene();
    gear(&mut s.app);
    let sword = item_worn(&mut s.app, s.alice, WearSlot::Wield, 30, "a long sword");
    obj_trigger(&mut s.app, 100, "h", 30, ALLOW);
    let said = say(&mut s.app, s.alice, "drop sword");
    assert!(said.contains("You aren't carrying that."), "{said}");
    assert!(
        seen(&mut s.app, sword, "actor").is_none(),
        "no drop trigger"
    );
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    assert_eq!(worn(&s.app, s.alice, WearSlot::Wield), Some(sword));
}

#[test]
fn a_handled_equipment_listing_is_not_answered() {
    let mut s = scene();
    gear(&mut s.app);
    item_worn(&mut s.app, s.alice, WearSlot::Body, 20, "a leather vest");
    let said = say(&mut s.app, s.alice, "equipment");
    assert!(said.contains("a leather vest"), "{said}");
    s.app.world_mut().resource_mut::<GateTakesEverything>().0 = true;
    let said = say(&mut s.app, s.alice, "equipment");
    assert!(!said.contains("a leather vest"), "{said}");
}

#[test]
fn unequip_cannot_take_a_cursed_object_off() {
    let mut s = scene();
    gear(&mut s.app);
    let ring = item_worn(&mut s.app, s.alice, WearSlot::FingerR, 21, "a jade ring");
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_21"))
        .unwrap()
        .extra_flags
        .push(ItemExtraFlag::NoDrop);
    obj_trigger(&mut s.app, 100, "l", 21, ALLOW);
    let said = say(&mut s.app, s.alice, "unequip finger");
    assert!(said.contains("it must be CURSED"), "{said}");
    assert_eq!(worn(&s.app, s.alice, WearSlot::FingerR), Some(ring));
    assert!(seen(&mut s.app, ring, "actor").is_none());
}

#[test]
fn messages_repeat_the_name_without_its_count() {
    let mut s = scene();
    gear(&mut s.app);
    provisions(&mut s.app);
    for line in [
        "wear 2.sword",
        "wield 2.sword",
        "hold 2.sword",
        "eat 2.sword",
        "quaff 2.sword",
    ] {
        let said = say(&mut s.app, s.alice, line);
        assert!(
            said.contains("You don't seem to have a sword."),
            "{line}: {said}"
        );
    }
    let said = say(&mut s.app, s.alice, "give 2.sword guard");
    assert!(said.contains("You don't seem to have a sword."), "{said}");
    let said = say(&mut s.app, s.alice, "remove 2.sword");
    assert!(
        said.contains("You don't seem to be using a sword."),
        "{said}"
    );
}

#[test]
fn zero_dot_name_finds_the_player() {
    let mut s = scene();
    let bob = player(&mut s.app, s.temple, "Bob", 2);
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    // An NPC with the same keyword is first in the room, but `0.` skips it.
    let said = say(&mut s.app, s.alice, "give sword 0.bob");
    assert_eq!(carried_by(&s.app, sword), Some(bob), "{said}");
}

#[test]
fn a_character_below_resting_can_do_none_of_it() {
    let mut s = scene();
    gear(&mut s.app);
    s.app
        .world_mut()
        .entity_mut(s.alice)
        .insert(combatant_at(Position::Sleeping));
    let sword = item_carried(&mut s.app, s.alice, 30, "a long sword");
    obj_trigger(&mut s.app, 100, "j", 30, ALLOW);
    let said = say(&mut s.app, s.alice, "wield sword");
    assert!(said.contains("In your dreams, or what?"), "{said}");
    assert!(seen(&mut s.app, sword, "actor").is_none());
    s.app
        .world_mut()
        .get_mut::<TbaCombatant>(s.alice)
        .unwrap()
        .position = Position::Stunned;
    let said = say(&mut s.app, s.alice, "drop sword");
    assert!(said.contains("think about the stars"), "{said}");
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    s.app
        .world_mut()
        .get_mut::<TbaCombatant>(s.alice)
        .unwrap()
        .position = Position::Resting;
    say(&mut s.app, s.alice, "wield sword");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Wield), Some(sword));
}

#[test]
fn worn_state_reaches_the_persisted_shadow() {
    let mut s = scene();
    gear(&mut s.app);
    provisions(&mut s.app);
    let vest = item_carried(&mut s.app, s.alice, 20, "a leather vest");
    give_save_id(&mut s.app, vest, "item:vest");
    say(&mut s.app, s.alice, "wear vest");
    assert_eq!(
        saved_worn(&s.app, s.alice),
        vec![(WearSlot::Body, "item:vest".to_owned())]
    );
    say(&mut s.app, s.alice, "remove vest");
    assert!(saved_worn(&s.app, s.alice).is_empty());
    say(&mut s.app, s.alice, "equip vest");
    assert_eq!(saved_worn(&s.app, s.alice).len(), 1);
    say(&mut s.app, s.alice, "unequip body");
    assert!(saved_worn(&s.app, s.alice).is_empty());

    let potion = item_carried(&mut s.app, s.alice, 53, "a bubbling potion");
    give_save_id(&mut s.app, potion, "item:potion");
    say(&mut s.app, s.alice, "hold potion");
    assert_eq!(saved_worn(&s.app, s.alice).len(), 1);
    say(&mut s.app, s.alice, "quaff potion");
    assert!(s.app.world().get_entity(potion).is_err());
    assert!(saved_worn(&s.app, s.alice).is_empty());
}

#[test]
fn necklaces_and_bracelets_take_their_second_position_too() {
    let mut s = scene();
    gear(&mut s.app);
    define_item(
        &mut s.app,
        26,
        TbaItemKind::Armor,
        &[WearSlot::Neck1, WearSlot::Neck2],
        &["amulet"],
    );
    let n1 = item_carried(&mut s.app, s.alice, 26, "a bone amulet");
    let n2 = item_carried(&mut s.app, s.alice, 26, "a jade amulet");
    let n3 = item_carried(&mut s.app, s.alice, 26, "a gold amulet");
    say(&mut s.app, s.alice, "wear bone");
    say(&mut s.app, s.alice, "wear jade");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Neck1), Some(n1));
    assert_eq!(worn(&s.app, s.alice, WearSlot::Neck2), Some(n2));
    let said = say(&mut s.app, s.alice, "wear gold");
    assert!(
        said.contains("You can't wear anything else around your neck."),
        "{said}"
    );
    assert_eq!(carried_by(&s.app, n3), Some(s.alice));

    let b1 = item_carried(&mut s.app, s.alice, 25, "a silver bracelet");
    let b2 = item_carried(&mut s.app, s.alice, 25, "a copper bracelet");
    say(&mut s.app, s.alice, "wear silver");
    say(&mut s.app, s.alice, "wear copper");
    assert_eq!(worn(&s.app, s.alice, WearSlot::WristR), Some(b1));
    assert_eq!(worn(&s.app, s.alice, WearSlot::WristL), Some(b2));
}

#[test]
fn a_bare_wear_takes_the_last_position_the_object_fits() {
    let mut s = scene();
    gear(&mut s.app);
    define_item(
        &mut s.app,
        27,
        TbaItemKind::Armor,
        &[WearSlot::Head, WearSlot::Legs],
        &["hood"],
    );
    item_carried(&mut s.app, s.alice, 27, "a long hood");
    say(&mut s.app, s.alice, "wear hood");
    assert!(worn(&s.app, s.alice, WearSlot::Legs).is_some());
    assert!(worn(&s.app, s.alice, WearSlot::Head).is_none());
}

#[test]
fn a_weapon_above_the_wielders_level_is_refused() {
    let mut s = scene();
    gear(&mut s.app);
    define_item(
        &mut s.app,
        35,
        TbaItemKind::Weapon,
        &[WearSlot::Wield],
        &["blade"],
    );
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_35"))
        .unwrap()
        .min_level = 50;
    let blade = item_carried(&mut s.app, s.alice, 35, "a fine blade");
    let said = say(&mut s.app, s.alice, "wield blade");
    assert!(said.contains("not experienced enough"), "{said}");
    assert!(worn(&s.app, s.alice, WearSlot::Wield).is_none());
    assert_eq!(carried_by(&s.app, blade), Some(s.alice));
}

#[test]
fn a_cursed_object_is_not_given_away() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    define_item(&mut s.app, 5, TbaItemKind::Weapon, &[], &["sword"]);
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_5"))
        .unwrap()
        .extra_flags
        .push(ItemExtraFlag::NoDrop);
    let said = say(&mut s.app, s.alice, "give sword guard");
    assert!(
        said.contains("You can't let go of a rusty sword!!  Yeech!"),
        "{said}"
    );
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
}

#[test]
fn eating_poisoned_food_says_so_and_a_keyword_alone_names_a_thing() {
    let mut s = scene();
    provisions(&mut s.app);
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_50"))
        .unwrap()
        .values = [4, 0, 0, 1];
    // "loaf" is a keyword and not part of the short description.
    let bread = item_carried(&mut s.app, s.alice, 50, "a golden crust");
    let said = say(&mut s.app, s.alice, "eat loaf");
    assert!(said.contains("Oops, that tasted rather strange!"), "{said}");
    assert!(s.app.world().get_entity(bread).is_err());
}

#[test]
fn drinking_looks_through_the_carried_objects_before_the_room() {
    let mut s = scene();
    provisions(&mut s.app);
    let lying = item_on_ground(&mut s.app, s.temple, 52, "a stone fountain");
    let held = item_carried(&mut s.app, s.alice, 51, "a stone flask");
    obj_trigger(&mut s.app, 100, "s", 51, ALLOW);
    obj_trigger(&mut s.app, 101, "s", 52, ALLOW);
    say(&mut s.app, s.alice, "drink stone");
    assert!(seen(&mut s.app, held, "actor").is_some());
    assert!(seen(&mut s.app, lying, "actor").is_none());
    say(&mut s.app, s.alice, "drink 2.stone");
    assert!(seen(&mut s.app, lying, "actor").is_some());
}

#[test]
fn removal_ends_when_the_trigger_took_the_object_off_itself() {
    // A trigger that refuses is already covered; this one allows and the object
    // is still worn afterwards, so it comes off.
    let mut s = scene();
    gear(&mut s.app);
    let vest = item_worn(&mut s.app, s.alice, WearSlot::Body, 20, "a leather vest");
    obj_trigger(&mut s.app, 100, "l", 20, ALLOW);
    let said = say(&mut s.app, s.alice, "remove vest");
    assert!(said.contains("You stop using a leather vest."), "{said}");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Body), None);
    assert_eq!(carried_by(&s.app, vest), Some(s.alice));
}

#[test]
fn a_command_forced_before_the_stage_reads_is_gated_with_the_rest() {
    // A system in front of the item intake writes a command, as a script's
    // `force` does; it passes the same triggers before the router sees it.
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    obj_trigger(&mut s.app, 100, "h", 5, REFUSE);
    let alice = s.alice;
    s.app.add_systems(
        Update,
        (move |mut seq: ResMut<CommandSequence>, mut writer: MessageWriter<CommandReceived>| {
            writer.write(CommandReceived::new(
                seq.next_id(),
                alice,
                1,
                PlayerInput::RawLine("drop sword".to_owned()),
            ));
        })
        .before(mib_tba::dg::intake::items::items_intake)
        .in_set(mib_tba::dg::intake::DgIntake::PreCommit),
    );
    say(&mut s.app, s.alice, "look");
    assert!(seen(&mut s.app, sword, "actor").is_some());
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
}

// -- review round 2 ----------------------------------------------------------------------------

fn typed(app: &mut App, who: Entity, input: PlayerInput) -> String {
    let id = app.world_mut().resource_mut::<CommandSequence>().next_id();
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(id, who, 1, input));
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
    output(app).join("\n")
}

#[test]
fn put_refuses_a_cursed_object_for_a_container_on_the_floor() {
    let mut s = scene();
    let chest = item_on_ground(&mut s.app, s.temple, 7, "a wooden chest");
    s.app.world_mut().entity_mut(chest).insert(Container {
        capacity: 10,
        contents: vec![],
    });
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    define_item(&mut s.app, 5, TbaItemKind::Weapon, &[], &["sword"]);
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_5"))
        .unwrap()
        .extra_flags
        .push(ItemExtraFlag::NoDrop);
    let said = say(&mut s.app, s.alice, "put sword in chest");
    assert!(
        said.contains("You can't get a rusty sword out of your hand."),
        "{said}"
    );
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    assert!(s.app.world().get::<ContainedBy>(sword).is_none());
}

#[test]
fn put_of_a_cursed_object_into_a_carried_bag_is_counted_as_unmodelled() {
    let mut s = scene();
    let bag = item_carried(&mut s.app, s.alice, 7, "a leather bag");
    s.app.world_mut().entity_mut(bag).insert(Container {
        capacity: 10,
        contents: vec![],
    });
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    define_item(&mut s.app, 5, TbaItemKind::Weapon, &[], &["sword"]);
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_5"))
        .unwrap()
        .extra_flags
        .push(ItemExtraFlag::NoDrop);
    say(&mut s.app, s.alice, "put sword in bag");
    assert_eq!(
        s.app.world().get::<ContainedBy>(sword).map(|c| c.0),
        Some(bag)
    );
    assert_eq!(
        s.app
            .world()
            .resource::<DgLog>()
            .count("put_curse_container"),
        1
    );
}

#[test]
fn put_into_a_full_container_is_refused() {
    let mut s = scene();
    let chest = item_on_ground(&mut s.app, s.temple, 7, "a wooden chest");
    let gem = item_inside(&mut s.app, chest, 8, "a green gem");
    s.app.world_mut().entity_mut(chest).insert(Container {
        capacity: 1,
        contents: vec![gem],
    });
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let said = say(&mut s.app, s.alice, "put sword in chest");
    assert!(said.contains("can't hold any more"), "{said}");
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    assert_eq!(
        s.app.world().get::<Container>(chest).unwrap().contents,
        vec![gem]
    );
}

#[test]
fn get_from_a_container_empties_its_contents_and_tells_the_quests() {
    let mut s = scene();
    let chest = item_on_ground(&mut s.app, s.temple, 7, "a wooden chest");
    let gem = item_inside(&mut s.app, chest, 8, "a green gem");
    s.app.world_mut().entity_mut(chest).insert(Container {
        capacity: 10,
        contents: vec![gem],
    });
    say(&mut s.app, s.alice, "get gem from chest");
    assert_eq!(carried_by(&s.app, gem), Some(s.alice));
    assert!(s.app.world().get::<ContainedBy>(gem).is_none());
    assert!(
        s.app
            .world()
            .get::<Container>(chest)
            .unwrap()
            .contents
            .is_empty()
    );
    let events: Vec<QuestEvent> = s
        .app
        .world_mut()
        .resource_mut::<Messages<QuestEvent>>()
        .drain()
        .collect();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, QuestEvent::Collected { .. })),
        "{events:?}"
    );
}

#[test]
fn drop_stamps_the_time_the_object_landed() {
    let mut s = scene();
    let ball = item_carried(&mut s.app, s.alice, 6, "a red ball");
    tick_pulses(&mut s.app, 30);
    say(&mut s.app, s.alice, "drop ball");
    let at = s.app.world().get::<GroundItem>(ball).unwrap().spawned_at;
    assert!((at - 3.0).abs() < 0.01, "{at}");
}

#[test]
fn structured_commands_and_the_grab_verb_reach_the_intake() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let said = typed(
        &mut s.app,
        s.alice,
        PlayerInput::Command(Command::Give {
            item: "sword".to_owned(),
            to: "guard".to_owned(),
        }),
    );
    assert_eq!(carried_by(&s.app, sword), Some(s.guard), "{said}");

    let said = typed(
        &mut s.app,
        s.alice,
        PlayerInput::Command(Command::Give {
            item: "20 coins".to_owned(),
            to: "guard".to_owned(),
        }),
    );
    assert_eq!(
        s.app.world().get::<TbaGold>(s.alice).unwrap().0,
        80,
        "{said}"
    );
    assert_eq!(s.app.world().get::<DgNpcGold>(s.guard).unwrap().gold, 20);

    let ball = item_on_ground(&mut s.app, s.temple, 6, "a red ball");
    typed(
        &mut s.app,
        s.alice,
        PlayerInput::Command(Command::Get {
            item: "ball".to_owned(),
            from: None,
        }),
    );
    assert_eq!(carried_by(&s.app, ball), Some(s.alice));

    gear(&mut s.app);
    let torch = item_carried(&mut s.app, s.alice, 32, "a twisted wand");
    let said = typed(
        &mut s.app,
        s.alice,
        PlayerInput::Command(Command::Custom {
            verb: "grab".to_owned(),
            args: vec!["wand".to_owned()],
        }),
    );
    assert!(said.contains("You grab a twisted wand"), "{said}");
    assert_eq!(worn(&s.app, s.alice, WearSlot::Hold), Some(torch));
}

#[test]
fn a_command_written_during_the_stage_is_read_in_the_same_stage() {
    let mut s = scene();
    let sword = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let ball = item_carried(&mut s.app, s.alice, 6, "a red ball");
    obj_trigger(&mut s.app, 100, "h", 5, REFUSE);
    obj_trigger(&mut s.app, 101, "h", 6, ALLOW);
    // The first command is the player's; once it has run, a "force" writes the
    // second, which must pass the triggers in this very stage.
    let alice = s.alice;
    let mut cursor = bevy::ecs::message::MessageCursor::<CommandReceived>::default();
    let mut forced = false;
    s.app
        .world_mut()
        .resource_mut::<Messages<CommandReceived>>();
    let id = s
        .app
        .world_mut()
        .resource_mut::<CommandSequence>()
        .next_id();
    s.app
        .world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(
            id,
            alice,
            1,
            PlayerInput::RawLine("drop sword".to_owned()),
        ));
    mib_tba::dg::intake::items::run_stage(s.app.world_mut(), &mut cursor, &mut |world| {
        if !forced {
            forced = true;
            let id = world.resource_mut::<CommandSequence>().next_id();
            world
                .resource_mut::<Messages<CommandReceived>>()
                .write(CommandReceived::new(
                    id,
                    alice,
                    1,
                    PlayerInput::RawLine("drop ball".to_owned()),
                ));
        }
    });
    assert!(seen(&mut s.app, sword, "actor").is_some());
    assert!(
        seen(&mut s.app, ball, "actor").is_some(),
        "the forced command ran its trigger"
    );
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    assert!(on_ground(&s.app, ball));
}

#[test]
fn commands_still_arriving_after_the_last_round_are_marked_handled() {
    let mut s = scene();
    let alice = s.alice;
    let mut cursor = bevy::ecs::message::MessageCursor::<CommandReceived>::default();
    let mut last = None;
    let id = s
        .app
        .world_mut()
        .resource_mut::<CommandSequence>()
        .next_id();
    s.app
        .world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(
            id,
            alice,
            1,
            PlayerInput::RawLine("look".to_owned()),
        ));
    mib_tba::dg::intake::items::run_stage(s.app.world_mut(), &mut cursor, &mut |world| {
        let id = world.resource_mut::<CommandSequence>().next_id();
        last = Some(id);
        world
            .resource_mut::<Messages<CommandReceived>>()
            .write(CommandReceived::new(
                id,
                alice,
                1,
                PlayerInput::RawLine("look".to_owned()),
            ));
    });
    let last = last.unwrap();
    assert!(s.app.world().resource::<HandledCommands>().is_handled(last));
}

#[test]
fn commands_over_the_rate_cap_are_dropped() {
    let mut s = scene();
    let mut config = Config::default();
    config.game.command_rate_per_sec = 0.001;
    config.game.command_burst = 2;
    s.app.insert_resource(config);
    s.app
        .world_mut()
        .entity_mut(s.alice)
        .insert(CommandRate::new());
    let a = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let b = item_carried(&mut s.app, s.alice, 6, "a red ball");
    let c = item_carried(&mut s.app, s.alice, 9, "a green gem");
    say(&mut s.app, s.alice, "drop sword");
    say(&mut s.app, s.alice, "drop ball");
    let said = say(&mut s.app, s.alice, "drop gem");
    assert!(on_ground(&s.app, a) && on_ground(&s.app, b));
    assert_eq!(
        carried_by(&s.app, c),
        Some(s.alice),
        "over the cap: dropped"
    );
    assert!(said.contains("too quickly"), "{said}");
}

#[test]
fn a_hidden_actor_is_not_named_to_the_room() {
    let mut s = scene();
    let bob = player(&mut s.app, s.temple, "Bob", 2);
    s.app.world_mut().entity_mut(s.alice).insert(Concealment {
        invisible: true,
        ..Default::default()
    });
    provisions(&mut s.app);
    item_carried(&mut s.app, s.alice, 50, "a loaf of bread");
    send(&mut s.app, s.alice, "eat bread");
    let events: Vec<OutputEvent> = s
        .app
        .world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .collect();
    let heard: Vec<String> = events
        .into_iter()
        .filter(|event| event.session == bob)
        .filter_map(|event| match event.payload {
            OutboundPayload::Text { body, .. } => Some(body.render_plain()),
            _ => None,
        })
        .collect();
    assert!(
        heard.iter().all(|line| !line.contains("Alice")),
        "{heard:?}"
    );
}

#[test]
fn a_visible_actor_is_named_to_the_room() {
    let mut s = scene();
    let bob = player(&mut s.app, s.temple, "Bob", 2);
    provisions(&mut s.app);
    item_carried(&mut s.app, s.alice, 50, "a loaf of bread");
    send(&mut s.app, s.alice, "eat bread");
    let heard: Vec<String> = s
        .app
        .world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .filter(|event| event.session == bob)
        .filter_map(|event| match event.payload {
            OutboundPayload::Text { body, .. } => Some(body.render_plain()),
            _ => None,
        })
        .collect();
    assert!(
        heard.iter().any(|line| line.contains("Alice eats")),
        "{heard:?}"
    );
}

#[test]
fn give_all_in_the_structured_form_gives_everything() {
    let mut s = scene();
    let a = item_carried(&mut s.app, s.alice, 5, "a rusty sword");
    let b = item_carried(&mut s.app, s.alice, 6, "a red ball");
    typed(
        &mut s.app,
        s.alice,
        PlayerInput::Command(Command::Give {
            item: "all".to_owned(),
            to: "guard".to_owned(),
        }),
    );
    assert_eq!(carried_by(&s.app, a), Some(s.guard));
    assert_eq!(carried_by(&s.app, b), Some(s.guard));
}

#[test]
fn the_missing_count_message_drops_the_ordinal() {
    let mut s = scene();
    let said = say(&mut s.app, s.alice, "give 2 3.sword guard");
    assert!(
        said.contains("You don't seem to have any swords."),
        "{said}"
    );
}

#[test]
fn last_dot_name_prefers_the_carried_object() {
    let mut s = scene();
    provisions(&mut s.app);
    let lying = item_on_ground(&mut s.app, s.temple, 52, "a stone fountain");
    let held = item_carried(&mut s.app, s.alice, 51, "a stone flask");
    obj_trigger(&mut s.app, 100, "s", 51, ALLOW);
    obj_trigger(&mut s.app, 101, "s", 52, ALLOW);
    say(&mut s.app, s.alice, "drink last.stone");
    assert!(seen(&mut s.app, held, "actor").is_some());
    assert!(seen(&mut s.app, lying, "actor").is_none());
}

#[test]
fn a_potion_casts_up_to_three_spells_at_its_level() {
    use mib_tba::dg::intake::items::consume::potion_spells;
    assert_eq!(
        potion_spells([12, 3, 4, 5]),
        vec![(3, 12), (4, 12), (5, 12)]
    );
    assert_eq!(potion_spells([12, 3, 0, 5]), vec![(3, 12)]);
    assert_eq!(potion_spells([12, 0, 4, 5]), Vec::<(i32, i32)>::new());
}

/// Type a line and run a frame, leaving the output to be read.
fn send(app: &mut App, who: Entity, line: &str) {
    let id = app.world_mut().resource_mut::<CommandSequence>().next_id();
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(
            id,
            who,
            1,
            PlayerInput::RawLine(line.to_owned()),
        ));
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}
