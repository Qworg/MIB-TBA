//! Issue #15 integration tests: the DG trigger evaluation core against a small
//! ECS app.
//!
//! These drive the real [`trigger_command_system`] and [`trigger_greet_system`]
//! systems (the same shape `tests/socials.rs` uses): a `TbaTriggers` resource
//! parsed from in-test definitions, a room with a player and an NPC, and the
//! messages the systems read/write. They cover a speech trigger responding with
//! substituted text, an `arglist` mismatch staying silent, a greet firing on a
//! room change, and an unsupported construct being reported (as a drop) rather
//! than executed.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use mib_tba::triggers::effects::{PendingTriggerOps, TriggerDropLog, trigger_effects_system};
use mib_tba::triggers::quests::trigger_quest_bridge_system;

fn trigger(vnum: i32, attach: &str, types: &[&str], arglist: &str, script: &str) -> TriggerDef {
    TriggerDef {
        vnum,
        name: format!("test trigger {vnum}"),
        attach: attach.to_owned(),
        types: types.iter().map(|ty| (*ty).to_owned()).collect(),
        narg: 100,
        arglist: arglist.to_owned(),
        script: script.to_owned(),
    }
}

fn test_app(defs: Vec<TriggerDef>) -> App {
    let mut app = App::new();
    app.init_resource::<TbaRng>();
    app.init_resource::<TriggerLastRoom>();
    app.init_resource::<PendingTriggerOps>();
    let mut map = BTreeMap::new();
    for def in defs {
        map.insert(TriggerId::from_vnum(def.vnum), def);
    }
    app.insert_resource(TbaTriggers::build(map));
    app.add_message::<CommandReceived>();
    app.add_message::<OutputEvent>();
    app.add_message::<QuestEvent>();
    app.add_message::<NpcDefeated>();
    app.add_systems(
        Update,
        (trigger_command_system, trigger_effects_system).chain(),
    );
    app.add_systems(
        FixedUpdate,
        (trigger_greet_system, trigger_effects_system).chain(),
    );
    app.add_systems(Update, trigger_quest_bridge_system);
    app
}

fn spawn_room(app: &mut App) -> Entity {
    app.world_mut().spawn_empty().id()
}

fn spawn_player(app: &mut App, room: Entity, name: &str) -> Entity {
    app.world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: name.to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room },
            TbaProgress::new("warrior"),
        ))
        .id()
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

fn typed_command(app: &mut App, session: Entity, command: Command) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
            session,
            session_id: 1,
            input: PlayerInput::Command(command),
        });
}

fn run_update(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn run_fixed(app: &mut App) {
    app.world_mut().run_schedule(FixedUpdate);
    app.world_mut().flush();
}

/// Every output line, oldest first.
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

fn text(app: &mut App) -> String {
    lines(app).join("\n")
}

#[test]
fn speech_trigger_responds_with_substituted_text() {
    let mut app = test_app(vec![trigger(
        1,
        "npc:tba_npc_5",
        &["speech"],
        "help",
        "say Can you help me, %actor.name%? I am %self.name%.\n",
    )]);
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let _npc = spawn_npc(&mut app, room, "the questmaster", "tba_npc_5");

    command(&mut app, actor, "say can you help me?");
    run_update(&mut app);

    let all = text(&mut app);
    assert!(
        all.contains("Can you help me, Aria? I am the questmaster."),
        "substituted speech line: {all}"
    );
}

#[test]
fn speech_trigger_ignores_a_non_matching_arglist() {
    let mut app = test_app(vec![trigger(
        1,
        "npc:tba_npc_5",
        &["speech"],
        "help",
        "say I can help.\n",
    )]);
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let _npc = spawn_npc(&mut app, room, "the questmaster", "tba_npc_5");

    command(&mut app, actor, "say good morning");
    run_update(&mut app);
    assert!(text(&mut app).is_empty(), "arglist mismatch stays silent");
}

#[test]
fn command_trigger_matches_the_command_word() {
    let mut app = test_app(vec![trigger(
        2,
        "npc:tba_npc_5",
        &["command"],
        "bow",
        "say You bow before %actor.name%.\n",
    )]);
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let _npc = spawn_npc(&mut app, room, "the questmaster", "tba_npc_5");

    command(&mut app, actor, "bow");
    run_update(&mut app);
    assert!(
        text(&mut app).contains("You bow before Aria."),
        "command trigger fired"
    );
}

#[test]
fn greet_trigger_fires_when_a_player_enters() {
    let mut app = test_app(vec![trigger(
        3,
        "npc:tba_npc_5",
        &["greet"],
        "",
        "say Hello, %actor.name%.\n",
    )]);
    let first_room = spawn_room(&mut app);
    let second_room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, first_room, "Aria");
    let _npc = spawn_npc(&mut app, second_room, "the greeter", "tba_npc_5");

    // First sight records the room without greeting.
    run_fixed(&mut app);
    assert!(text(&mut app).is_empty(), "no greeting on first sight");

    // Entering the NPC's room fires the greet.
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: second_room });
    run_fixed(&mut app);
    assert!(
        text(&mut app).contains("Hello, Aria."),
        "greet fired on entry"
    );
}

#[test]
fn act_trigger_fires_on_an_emote() {
    let mut app = test_app(vec![trigger(
        5,
        "npc:tba_npc_5",
        &["act"],
        "*",
        "say %actor.name% acts before %self.name%.\n",
    )]);
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let _npc = spawn_npc(&mut app, room, "the questmaster", "tba_npc_5");

    command(&mut app, actor, "emote waves");
    run_update(&mut app);
    assert!(
        text(&mut app).contains("Aria acts before the questmaster."),
        "act trigger fired"
    );
}

#[test]
fn structured_custom_command_matches_the_verb() {
    let mut app = test_app(vec![trigger(
        6,
        "npc:tba_npc_5",
        &["command"],
        "portal",
        "say A portal opens for %actor.name%.\n",
    )]);
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let _npc = spawn_npc(&mut app, room, "the gatekeeper", "tba_npc_5");

    typed_command(
        &mut app,
        actor,
        Command::Custom {
            verb: "portal".to_owned(),
            args: Vec::new(),
        },
    );
    run_update(&mut app);
    assert!(
        text(&mut app).contains("A portal opens for Aria."),
        "structured command trigger fired"
    );
}

#[test]
fn unsupported_construct_is_reported_not_executed() {
    // `%teleport%` is parsed-but-deferred; only the `say` after it runs.
    let mut app = test_app(vec![trigger(
        4,
        "npc:tba_npc_5",
        &["speech"],
        "*",
        "%teleport% %actor% 3001\nsay The teleport was deferred.\n",
    )]);
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let _npc = spawn_npc(&mut app, room, "the questmaster", "tba_npc_5");

    command(&mut app, actor, "say anything");
    run_update(&mut app);

    let all = text(&mut app);
    assert!(all.contains("The teleport was deferred."), "say ran: {all}");
    let log = app.world().resource::<TriggerDropLog>();
    assert_eq!(
        log.count("teleport"),
        1,
        "the deferred construct is counted"
    );
}

#[test]
fn purge_self_despawns_the_trigger_owner() {
    let mut app = test_app(vec![trigger(
        7,
        "npc:tba_npc_5",
        &["command"],
        "vanish",
        "%purge% %self%\n",
    )]);
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let npc = spawn_npc(&mut app, room, "the questmaster", "tba_npc_5");

    command(&mut app, actor, "vanish");
    run_update(&mut app);

    assert!(
        app.world().get_entity(npc).is_err(),
        "the effect drain purged the owner"
    );
}

#[test]
fn load_with_an_unresolved_recipient_is_a_named_drop() {
    let mut app = test_app(vec![trigger(
        8,
        "npc:tba_npc_5",
        &["speech"],
        "*",
        "%load% obj 1300 %victim%\n",
    )]);
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let _npc = spawn_npc(&mut app, room, "the questmaster", "tba_npc_5");

    command(&mut app, actor, "say anything");
    run_update(&mut app);

    let log = app.world().resource::<TriggerDropLog>();
    assert_eq!(
        log.count("load_target"),
        1,
        "the unresolved recipient dropped"
    );
}
