//! Issue #17 integration tests for the Circle social emotes.
//!
//! These drive the real [`handle_social_commands`] system against a small ECS
//! app (the same shape `tests/shops.rs` uses): a `tba.socials` resource, a room
//! with an actor, an optional target, and the position/level components the
//! gate reads. They cover target found/not-found, the actor-only broadcast, the
//! self-target line, the `socials` listing, and the sleeping-actor gate.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;

fn social() -> mib_tba::content::TbaSocialDef {
    mib_tba::content::TbaSocialDef {
        sort_as: "wave".into(),
        hide: false,
        min_char_position: 5,
        min_victim_position: 0,
        min_level_char: 0,
        char_no_arg: Some("You wave.".to_owned()),
        others_no_arg: Some("$n waves.".to_owned()),
        char_found: Some("You wave at $N.".to_owned()),
        others_found: Some("$n waves at $N.".to_owned()),
        vict_found: Some("$n waves at you.".to_owned()),
        not_found: Some("Wave at who?".to_owned()),
        char_auto: Some("You wave at yourself.".to_owned()),
        others_auto: Some("$n waves at $mself.".to_owned()),
        char_body_found: Some("You wave at $N's $t.".to_owned()),
        others_body_found: Some("$n waves at $N's $t.".to_owned()),
        vict_body_found: Some("$n waves at your $t.".to_owned()),
        char_obj_found: Some("You wave $p.".to_owned()),
        others_obj_found: Some("$n waves $p.".to_owned()),
    }
}

fn content() -> TbaContent {
    let mut content = TbaContent::default();
    content.socials.insert("wave".into(), social());
    content
}

fn test_app() -> App {
    let mut app = App::new();
    app.insert_resource(content());
    app.add_message::<OutputEvent>();
    app.add_message::<CommandReceived>();
    app.add_systems(Update, handle_social_commands);
    app
}

fn spawn_room(app: &mut App) -> Entity {
    app.world_mut().spawn_empty().id()
}

fn spawn_player(app: &mut App, room: Entity, name: &str) -> Entity {
    let mut combatant = fallback_combatant();
    combatant.position = Position::Standing;
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
            combatant,
            TbaProgress::new("warrior"),
        ))
        .id()
}

fn spawn_npc(app: &mut App, room: Entity, name: &str) -> Entity {
    let mut combatant = fallback_combatant();
    combatant.position = Position::Standing;
    app.world_mut()
        .spawn((
            Npc {
                name: name.to_owned(),
                persona: String::new(),
            },
            Location { room },
            combatant,
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

fn run(app: &mut App) {
    app.world_mut().run_schedule(Update);
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
fn social_with_a_target_sends_actor_room_and_victim_lines() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Bob");
    let _victim = spawn_player(&mut app, room, "Alice");

    command(&mut app, actor, "social wave alice");
    run(&mut app);

    let all = text(&mut app);
    assert!(all.contains("You wave at Alice."), "actor line: {all}");
    // The room line excludes both actor and victim, so it is not emitted here
    // (only the two players are in the room). The victim got their own line.
    assert!(all.contains("Bob waves at you."), "victim line: {all}");

    // A third player in the room receives the folded `$n`/`$N` room line.
    let _witness = spawn_player(&mut app, room, "Carol");
    command(&mut app, actor, "social wave alice");
    run(&mut app);
    let all = text(&mut app);
    assert!(all.contains("Bob waves at Alice."), "room line: {all}");
}

#[test]
fn unknown_target_uses_the_not_found_line() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Bob");

    command(&mut app, actor, "social wave nobody");
    run(&mut app);
    assert!(text(&mut app).contains("Wave at who?"));
}

#[test]
fn self_target_uses_the_auto_line() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Bob");

    command(&mut app, actor, "social wave bob");
    run(&mut app);
    assert!(text(&mut app).contains("You wave at yourself."));
}

#[test]
fn socials_lists_every_available_social() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Bob");

    command(&mut app, actor, "socials");
    run(&mut app);
    let all = text(&mut app);
    assert!(all.contains("1 socials are available"), "{all}");
    assert!(all.contains("wave"), "{all}");
}

#[test]
fn sleeping_actor_is_refused() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Bob");
    app.world_mut()
        .get_mut::<TbaCombatant>(actor)
        .expect("combatant")
        .position = Position::Sleeping;

    command(&mut app, actor, "social wave");
    run(&mut app);
    let all = text(&mut app);
    assert!(all.contains("in no position"), "{all}");
    assert!(!all.contains("You wave."), "the social did not run: {all}");
}

#[test]
fn npc_target_receives_the_room_line_only() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Bob");
    spawn_npc(&mut app, room, "the goblin");
    let _witness = spawn_player(&mut app, room, "Carol");

    command(&mut app, actor, "social wave goblin");
    run(&mut app);
    let all = text(&mut app);
    assert!(all.contains("You wave at the goblin."), "{all}");
    assert!(all.contains("Bob waves at the goblin."), "{all}");
}

#[test]
fn body_part_requires_the_body_strings() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Bob");
    let _victim = spawn_player(&mut app, room, "Alice");

    command(&mut app, actor, "social wave alice nose");
    run(&mut app);
    let all = text(&mut app);
    assert!(all.contains("You wave at Alice's nose."), "{all}");
}

#[test]
fn missing_social_is_refused() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Bob");
    command(&mut app, actor, "social moonwalk");
    run(&mut app);
    assert!(text(&mut app).contains("That action is not supported."));
}
