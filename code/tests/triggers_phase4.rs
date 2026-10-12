//! Issue #15 Phase 4 integration tests: `wait` timers and movement/gates,
//! driven from the **real** `content/tba.triggers.*` records.
//!
//! Four flows:
//!
//! - **`wait` ordering** (`tba_trig_9`, zone 0): the already-completed branch
//!   says two lines separated by a `wait`, and the resume order is preserved.
//! - **`%teleport%` gate** (`tba_trig_66`, zone 0): a greet that sends the
//!   player down and then teleports them to room 1300, plus a `%force% look`.
//! - **`%door%` gate** (`tba_trig_43`, zone 0): the real `wait until 21:00`
//!   shopkeeper script, stepped through the waits until it mutates the runtime
//!   door state for rooms 225 and 367.
//! - **`leave` gate** (`tba_trig_3000`, zone 30): a guild guard's `return 0`
//!   marks the move handled, and a timer whose owner is purged is cancelled.
//!
//! Tests may unwrap/expect — a panic here is a test failure.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use mib_core::prelude::*;
use mib_protocol::command::ExitKey;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use mib_tba::triggers::content::{TriggerDef, TriggerId, TriggersFile};
use mib_tba::triggers::effects::{
    PendingTriggerOps, TbaTriggerVars, TriggerDropLog, trigger_effects_system,
};
use mib_tba::triggers::events::{
    TriggerLastRoom, trigger_command_system, trigger_greet_system, trigger_leave_system,
};
use mib_tba::triggers::index::TbaTriggers;
use mib_tba::triggers::timers::{TriggerTimers, trigger_timer_system};

fn content_path(name: &str) -> String {
    format!("{}/../content/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn load_triggers(zone: u32) -> TriggersFile {
    let body = std::fs::read_to_string(content_path(&format!("tba.triggers.{zone}.yaml")))
        .expect("trigger shard");
    serde_yaml::from_str(&body).expect("triggers parse")
}

fn run_update(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn run_fixed(app: &mut App) {
    app.world_mut().run_schedule(FixedUpdate);
    app.world_mut().flush();
}

/// Advance the fixed clock and run one fixed tick, so `wait` timers fire.
fn tick_fixed(app: &mut App, seconds: f32) {
    app.world_mut()
        .resource_mut::<Time<Fixed>>()
        .advance_by(std::time::Duration::from_secs_f32(seconds));
    run_fixed(app);
}

fn lines(app: &mut App) -> Vec<String> {
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .filter_map(|event| match event.payload {
            OutboundPayload::Text { body, .. } | OutboundPayload::Error { body, .. } => {
                Some(body.render_plain())
            }
            _ => None,
        })
        .collect()
}

fn room(app: &mut App, num: u32) -> Entity {
    app.world_mut()
        .spawn((
            Room {
                key: format!("tba.{num}").into(),
                name: format!("Room {num}"),
                description: FormattedText::default(),
                num,
                area: "test".into(),
                coord: (0, 0, 0),
                tags: BTreeSet::new(),
                media: Vec::new(),
                sector: mib_stdlib::content::Sector::Field,
                extra_descriptions: Vec::new(),
            },
            RoomState::default(),
        ))
        .id()
}

fn spawn_player(app: &mut App, room: Entity, name: &str, class: &str) -> Entity {
    let entity = app
        .world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: name.to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room },
            TbaProgress::new(class),
        ))
        .id();
    app.world_mut().resource_mut::<OnlinePlayers>().map.insert(
        1,
        OnlinePlayer {
            entity,
            name: name.to_owned(),
            room,
            account_id: 1,
        },
    );
    entity
}

fn spawn_npc(app: &mut App, room: Entity, name: &str, key: &str) -> Entity {
    app.world_mut()
        .spawn((
            Npc {
                name: name.to_owned(),
                persona: String::new(),
            },
            NpcKey(NpcId::from(key)),
            Location { room },
        ))
        .id()
}

fn command(app: &mut App, session: Entity, line: &str) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
            session,
            session_id: 1,
            input: PlayerInput::RawLine(line.to_owned()),
        });
}

/// A base app wired like `TbaPlugin` wires the trigger systems.
fn test_app(defs: BTreeMap<TriggerId, TriggerDef>) -> App {
    let mut app = App::new();
    app.init_resource::<TbaRng>();
    app.init_resource::<TriggerLastRoom>();
    app.init_resource::<PendingTriggerOps>();
    app.init_resource::<TriggerTimers>();
    app.init_resource::<TriggerDropLog>();
    app.init_resource::<OnlinePlayers>();
    app.init_resource::<HandledCommands>();
    app.init_resource::<TbaObjectClock>();
    app.insert_resource(Time::<Fixed>::from_hz(10.0));
    app.insert_resource(TbaTriggers::build(defs));
    app.add_message::<CommandReceived>();
    app.add_message::<OutputEvent>();
    app.add_message::<QuestEvent>();
    app.add_message::<NpcDefeated>();
    app.add_systems(
        Update,
        (
            trigger_leave_system,
            trigger_command_system,
            trigger_effects_system,
        )
            .chain(),
    );
    app.add_systems(
        FixedUpdate,
        (
            trigger_timer_system,
            trigger_greet_system,
            trigger_effects_system,
        )
            .chain(),
    );
    app
}

fn fixture(def: &TriggerDef, attach: &str) -> TriggerDef {
    let mut def = def.clone();
    def.attach = attach.to_owned();
    def
}

/// The real `tba_trig_9` already-completed branch: two `say`s separated by a
/// `wait 2 sec`, then an `rdelete`. Order must survive the suspension.
#[test]
fn wait_sequences_preserve_order() {
    let file = load_triggers(0);
    let greet = fixture(
        file.triggers
            .get(&TriggerId::from_vnum(9))
            .expect("tba_trig_9"),
        "npc:tba_npc_1317",
    );
    let mut defs = BTreeMap::new();
    defs.insert(TriggerId::from_vnum(9), greet);
    let mut app = test_app(defs);

    let previous = room(&mut app, 2);
    let here = room(&mut app, 3);
    app.world_mut().entity_mut(previous).insert(Exits {
        map: [(ExitKey::South, here)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = spawn_player(&mut app, previous, "Aria", "warrior");
    let _npc = spawn_npc(&mut app, here, "the questmaster", "tba_npc_1317");
    app.world_mut().entity_mut(actor).insert(TbaTriggerVars {
        remote: [("solved_tutorial_quest_zone_0".to_owned(), "1".to_owned())]
            .into_iter()
            .collect(),
        ..TbaTriggerVars::default()
    });

    // First sight, then the room change.
    run_fixed(&mut app);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: here });
    run_fixed(&mut app);
    assert!(
        lines(&mut app).is_empty(),
        "the greet suspends on `wait 1 sec`"
    );

    tick_fixed(&mut app, 1.0);
    let first = lines(&mut app).join("\n");
    assert!(
        first.contains("you have already completed this quest."),
        "first line after 1s: {first}"
    );
    assert!(
        !first.contains("but you can try it again"),
        "the second line waits 2 more seconds: {first}"
    );

    tick_fixed(&mut app, 1.0);
    assert!(
        lines(&mut app).is_empty(),
        "the second line is not due after 2s total"
    );
    tick_fixed(&mut app, 1.0);
    let second = lines(&mut app).join("\n");
    assert!(
        second.contains("but you can try it again"),
        "second line after 3s total: {second}"
    );

    // The `rdelete` after the second wait deletes the persisted flag.
    let vars = app.world().get::<TbaTriggerVars>(actor).expect("component");
    assert!(
        !vars.remote.contains_key("solved_tutorial_quest_zone_0"),
        "rdelete removed the flag: {:?}",
        vars.remote
    );
}

/// The real `tba_trig_66`: a greet from the `down` direction teleports the
/// player to room 1300 and forces a `look`.
#[test]
fn teleport_gate_moves_the_player() {
    let file = load_triggers(0);
    let greet = fixture(
        file.triggers
            .get(&TriggerId::from_vnum(66))
            .expect("tba_trig_66"),
        "npc:tba_npc_66",
    );
    let mut defs = BTreeMap::new();
    defs.insert(TriggerId::from_vnum(66), greet);
    let mut app = test_app(defs);

    let above = room(&mut app, 100);
    let here = room(&mut app, 101);
    let destination = room(&mut app, 1300);
    app.world_mut().entity_mut(above).insert(Exits {
        map: [(ExitKey::Down, here)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = spawn_player(&mut app, above, "Aria", "warrior");
    let _npc = spawn_npc(&mut app, here, "the guide", "tba_npc_66");

    run_fixed(&mut app);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: here });
    run_fixed(&mut app);

    let location = app.world().get::<Location>(actor).expect("location");
    assert_eq!(
        location.room, destination,
        "the trigger teleported the player to room 1300"
    );

    let forced: Vec<CommandReceived> = app
        .world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .drain()
        .collect();
    assert!(
        forced
            .iter()
            .any(|msg| matches!(&msg.input, PlayerInput::RawLine(line) if line == "look")),
        "the `%force% %actor% look` was injected: {forced:?}"
    );
}

/// The real `tba_trig_43`: `wait until 21:00` then a chain of waits that
/// eventually runs `%door% 225 up flags ab`.
#[test]
fn door_gate_mutates_runtime_state() {
    let file = load_triggers(0);
    let mut script = fixture(
        file.triggers
            .get(&TriggerId::from_vnum(43))
            .expect("tba_trig_43"),
        "npc:tba_npc_109",
    );
    // The record is a `global`/`random` script; make it fire on a command so
    // the real body runs through the real systems.
    script.types = vec!["command".to_owned()];
    script.arglist = "*".to_owned();
    let mut defs = BTreeMap::new();
    defs.insert(TriggerId::from_vnum(43), script);
    let mut app = test_app(defs);

    let room_109 = room(&mut app, 109);
    let door_room = room(&mut app, 225);
    let _other = room(&mut app, 367);
    let actor = spawn_player(&mut app, room_109, "Aria", "warrior");
    let _npc = spawn_npc(&mut app, room_109, "the shopkeeper", "tba_npc_109");
    app.world_mut().resource_mut::<TbaObjectClock>().hour_of_day = 20;

    command(&mut app, actor, "look");
    run_update(&mut app);
    // `wait until 21:00` suspends immediately.
    assert!(
        !app.world()
            .get::<RoomState>(door_room)
            .expect("room state")
            .doors
            .contains_key(&ExitKey::Up),
        "the door is untouched before 21:00"
    );

    // Reach 21:00 and step the wait chain until the door changes.
    app.world_mut().resource_mut::<TbaObjectClock>().hour_of_day = 21;
    for _ in 0..40 {
        let closed = app
            .world()
            .get::<RoomState>(door_room)
            .and_then(|state| state.doors.get(&ExitKey::Up))
            .is_some_and(|door| door.closed);
        if closed {
            break;
        }
        tick_fixed(&mut app, 5.0);
    }
    let state = app.world().get::<RoomState>(door_room).expect("room state");
    let up = state.doors.get(&ExitKey::Up).expect("the door was mutated");
    assert!(up.closed, "`flags ab` sets the closed bit");
    assert!(!up.locked, "`flags ab` clears the locked bit");
}

/// The real `tba_trig_3000` guild guard: a non-mage walking south is blocked.
#[test]
fn leave_trigger_blocks_movement() {
    let file = load_triggers(30);
    let guard = fixture(
        file.triggers
            .get(&TriggerId::from_vnum(3000))
            .expect("tba_trig_3000"),
        "npc:tba_npc_3024",
    );
    let mut defs = BTreeMap::new();
    defs.insert(TriggerId::from_vnum(3000), guard);
    let mut app = test_app(defs);

    let here = room(&mut app, 1);
    let south = room(&mut app, 2);
    app.world_mut().entity_mut(here).insert(Exits {
        map: [(ExitKey::South, south)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = spawn_player(&mut app, here, "Aria", "warrior");
    let _npc = spawn_npc(&mut app, here, "the guildguard", "tba_npc_3024");

    command(&mut app, actor, "south");
    run_update(&mut app);

    let handled = app.world().resource::<HandledCommands>();
    assert!(
        handled.is_handled(CommandId::default()),
        "the guard blocked the move"
    );
}

/// A timer whose owner is purged is cancelled and counted, never resuming into
/// a dead entity.
#[test]
fn timer_is_cancelled_when_its_owner_is_purged() {
    let mut app = test_app(BTreeMap::new());
    let room = room(&mut app, 1);
    let actor = spawn_player(&mut app, room, "Aria", "warrior");
    let owner = app.world_mut().spawn_empty().id();
    let mut timers = app.world_mut().resource_mut::<TriggerTimers>();
    timers.schedule(PendingTimer {
        owner,
        actor,
        room,
        trigger: TriggerId::from_vnum(1),
        pc: 0,
        locals: BTreeMap::new(),
        ctx: TriggerContext::default(),
        wait: TriggerWait::Seconds(1),
        remaining_secs: 1.0,
        remaining_ticks: 0,
        target_mud_hour: None,
    });
    app.world_mut().despawn(owner);

    tick_fixed(&mut app, 1.0);

    assert!(app.world().resource::<TriggerTimers>().is_empty());
    let drops = app.world().resource::<TriggerDropLog>();
    assert_eq!(drops.count("timer_cancelled"), 1);
    assert!(app.world().get_entity(actor).is_ok());
}
