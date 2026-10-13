//! Intake for item actions: the `get`, `drop`, `give`, `receive`, `bribe`,
//! `wear`, `remove` and `consume` triggers, and the verbs that carry them.
//!
//! Expectations come from `act.item.c` / `act.other.c` / `spell_parser.c` and
//! from the dispatch notes in `dg-vm`, never from what the code returns.
//!
//! The app has the DG runtime, the item intake and a stand-in for the engine's
//! router. The stand-in calls the engine's own `handle_get`, `handle_drop` and
//! `handle_put` (the real item code, on the real components) for the commands
//! the engine parses, skips handled commands and marks each command handled
//! afterwards exactly as `route_commands` does for a raw line. The real router
//! needs a connection, a session table and a dozen unrelated resources; the
//! stand-in keeps the property the tests are about (the engine acts on what it
//! resolves, after the intake stage) and the pack's `equip` handler runs as it
//! does in the game.
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
    mib_tba::dg::intake::register(&mut app);
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
    assert_eq!(s.app.world().get::<DgNpcGold>(s.guard).unwrap().0, 30);
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
    assert_eq!(s.app.world().get::<DgNpcGold>(s.guard).unwrap().0, 30);
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
    let alice_id = id_of_after(&mut s.app, s.alice);
    assert!(said.contains("You eat a loaf of bread."), "{said}");
    assert!(s.app.world().get_entity(bread).is_err(), "eaten");
    let _ = alice_id;
}

fn id_of_after(app: &mut App, entity: Entity) -> String {
    id_of(app, entity)
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
fn quaffing_a_potion_consumes_it_and_records_the_spells_the_pack_cannot_cast() {
    let mut s = scene();
    provisions(&mut s.app);
    let potion = item_carried(&mut s.app, s.alice, 53, "a bubbling potion");
    obj_trigger(&mut s.app, 100, "s", 53, ALLOW);
    let said = say(&mut s.app, s.alice, "quaff potion");
    assert_eq!(seen_gone(&mut s.app, potion), None);
    assert!(said.contains("You quaff a bubbling potion."), "{said}");
    assert!(s.app.world().get_entity(potion).is_err());
    assert_eq!(s.app.world().resource::<DgLog>().count("potion_spells"), 1);
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
