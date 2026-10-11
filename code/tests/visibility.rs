//! Stealth visibility integration tests (the `CommandSet::PreRoute`
//! interceptor in `mib_tba::visibility`).
//!
//! These drive the real [`intercept_visibility_commands`] system against a
//! small ECS app whose `Route` system calls the **real stdlib**
//! [`handle_look`]/[`handle_who`] — the exact functions `route_commands`
//! dispatches to. So a pass-through test proves the stdlib rendered the scene,
//! and a marked-handled test proves the interceptor's filtered output replaced
//! it. A `Routed` counter records how many commands reached the stdlib router.
//!
//! Required semantics: a stealthed player is absent from another actor's `look`
//! scene and from `who`, present for themselves; a normal `look` with no
//! stealthed player present is unchanged (stdlib Scene); `look <stealthed
//! player>` is denied; `look <npc>`/`<item>`/`<direction>` still reach the
//! stdlib; `l` works; `who` still lists non-stealthed players.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeSet, HashMap};

use mib_core::prelude::*;
use mib_stdlib::content::{RoomTag, Sector};
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[derive(Resource, Default)]
struct Routed(usize);

fn test_room(key: &str, name: &str) -> Room {
    Room {
        key: key.into(),
        name: name.to_owned(),
        description: FormattedText::plain("A quiet test room."),
        num: 0,
        area: "test".into(),
        coord: (0, 0, 0),
        tags: BTreeSet::new(),
        media: Vec::new(),
        sector: Sector::Inside,
        extra_descriptions: Vec::new(),
    }
}

fn no_exits() -> Exits {
    Exits {
        map: HashMap::new(),
        hidden: BTreeSet::new(),
    }
}

/// The interceptor exactly as [`TbaPlugin`] wires it (in `PreRoute`), with a
/// `Route` system that delegates every non-handled `look`/`who` to the stdlib
/// handlers — the real renderers, not a reimplementation.
fn test_app() -> App {
    let mut app = App::new();
    app.init_resource::<HandledCommands>();
    app.init_resource::<CommandSequence>();
    app.init_resource::<Routed>();
    app.add_message::<CommandReceived>();
    app.add_message::<OutputEvent>();
    app.configure_sets(Update, (CommandSet::PreRoute, CommandSet::Route).chain());
    app.add_systems(
        Update,
        intercept_visibility_commands.in_set(CommandSet::PreRoute),
    );
    app.add_systems(Update, stdlib_route.in_set(CommandSet::Route));
    app
}

/// The `Route` stand-in: skip handled commands, and send `look`/`who` through
/// the real stdlib handlers (mirrors `route_commands`'s dispatch for these two
/// verbs).
#[allow(clippy::too_many_arguments)]
fn stdlib_route(
    mut reader: MessageReader<CommandReceived>,
    handled: Res<HandledCommands>,
    mut routed: ResMut<Routed>,
    players: Query<(&Player, &Location)>,
    rooms: Query<(&Room, &Exits)>,
    npcs: Query<(Entity, &Npc, &Location)>,
    items: ItemQuery<'_, '_>,
    connections: Query<&Connection>,
    discovered: Query<&Discovered>,
    mut output: MessageWriter<OutputEvent>,
) {
    for msg in reader.read() {
        if handled.is_handled(msg.id) {
            continue;
        }
        let command = match &msg.input {
            PlayerInput::Command(command) => command.clone(),
            PlayerInput::RawLine(line) => match parse_line(line) {
                Ok(command) => command,
                Err(_) => continue,
            },
        };
        match command {
            Command::Look { target } => {
                routed.0 += 1;
                handle_look(
                    msg.session,
                    target,
                    &players,
                    &rooms,
                    &npcs,
                    &items,
                    &connections,
                    &discovered,
                    &mut output,
                );
            }
            Command::Who => {
                routed.0 += 1;
                handle_who(msg.session, &players, &mut output);
            }
            _ => {}
        }
    }
}

fn spawn_room(app: &mut App, key: &str, name: &str, exits: Exits) -> Entity {
    app.world_mut().spawn((test_room(key, name), exits)).id()
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
        ))
        .id()
}

fn spawn_npc(app: &mut App, room: Entity, name: &str, persona: &str) -> Entity {
    app.world_mut()
        .spawn((
            Npc {
                name: name.to_owned(),
                persona: persona.to_owned(),
            },
            Location { room },
        ))
        .id()
}

fn spawn_item(app: &mut App, room: Entity, name: &str, description: &str) -> Entity {
    app.world_mut()
        .spawn((
            Item {
                def_id: ItemId::from("test_item"),
                name: name.to_owned(),
                description: description.to_owned(),
                value: None,
                tags: Vec::new(),
            },
            Location { room },
        ))
        .id()
}

fn hide(app: &mut App, entity: Entity) {
    app.world_mut().entity_mut(entity).insert(TbaHidden);
}

fn sneak(app: &mut App, entity: Entity) {
    app.world_mut().entity_mut(entity).insert(TbaSneaking);
}

fn send(app: &mut App, session: Entity, input: PlayerInput) -> CommandId {
    let id = app.world_mut().resource_mut::<CommandSequence>().next_id();
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(id, session, 1, input));
    id
}

fn send_line(app: &mut App, session: Entity, line: &str) -> CommandId {
    send(app, session, PlayerInput::RawLine(line.to_owned()))
}

fn send_command(app: &mut App, session: Entity, command: Command) -> CommandId {
    send(app, session, PlayerInput::Command(command))
}

fn run(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn drain(app: &mut App) -> Vec<OutboundPayload> {
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .map(|event| event.payload)
        .collect()
}

fn scene(payloads: &[OutboundPayload]) -> Option<RoomView> {
    payloads.iter().find_map(|payload| match payload {
        OutboundPayload::Scene { room } => Some(room.clone()),
        _ => None,
    })
}

fn texts(payloads: &[OutboundPayload]) -> Vec<String> {
    payloads
        .iter()
        .filter_map(|payload| match payload {
            OutboundPayload::Text { body, .. } | OutboundPayload::Error { body, .. } => {
                Some(body.render_plain())
            }
            _ => None,
        })
        .collect()
}

fn routed(app: &App) -> usize {
    app.world().resource::<Routed>().0
}

fn occupants(app: &mut App) -> Vec<String> {
    let payloads = drain(app);
    scene(&payloads).expect("a Scene was emitted").occupants
}

// ---------------------------------------------------------------------------
// Bare `look`
// ---------------------------------------------------------------------------

/// A stealthed player does not appear in another actor's bare `look` scene, and
/// the interceptor marks the command handled (the stdlib router never runs).
#[test]
fn hidden_player_absent_from_anothers_look() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    hide(&mut app, bob);

    send_line(&mut app, alice, "look");
    run(&mut app);

    assert_eq!(routed(&app), 0, "interceptor marked the look handled");
    assert_eq!(occupants(&mut app), vec!["Alice"], "Bob is hidden");
}

/// The same holds for a player who is only `TbaSneaking`.
#[test]
fn sneaking_player_absent_from_anothers_look() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    sneak(&mut app, bob);

    send_line(&mut app, alice, "look");
    run(&mut app);

    assert_eq!(routed(&app), 0);
    assert_eq!(occupants(&mut app), vec!["Alice"]);
}

/// A stealthed player sees themselves (and visible room-mates) in their own
/// `look`. When the asker is the only stealthed occupant, the stdlib scene is
/// already correct, so the interceptor leaves it alone (no reimplementation).
#[test]
fn hidden_player_present_for_self() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let _alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    hide(&mut app, bob);

    send_line(&mut app, bob, "look");
    run(&mut app);

    assert_eq!(
        routed(&app),
        1,
        "only the asker is stealthed: stdlib is correct"
    );
    assert_eq!(
        occupants(&mut app),
        vec!["Alice", "Bob"],
        "Bob sees himself"
    );
}

/// A stealthed player does not see another stealthed player: with two stealthed
/// occupants the interceptor takes over and filters the other one out.
#[test]
fn hidden_player_does_not_see_another_hidden_player() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let bob = spawn_player(&mut app, room, "Bob");
    let carol = spawn_player(&mut app, room, "Carol");
    hide(&mut app, bob);
    hide(&mut app, carol);

    send_line(&mut app, bob, "look");
    run(&mut app);

    assert_eq!(
        routed(&app),
        0,
        "another stealthed occupant forces filtering"
    );
    assert_eq!(occupants(&mut app), vec!["Bob"], "Carol stays hidden");
}

/// With no stealthed player in the room, bare `look` is untouched: the command
/// reaches the stdlib router, which emits the normal Scene.
#[test]
fn normal_look_without_hidden_passes_through_to_stdlib() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let _bob = spawn_player(&mut app, room, "Bob");

    send_line(&mut app, alice, "look");
    run(&mut app);

    assert_eq!(routed(&app), 1, "stdlib router handled the look");
    assert_eq!(occupants(&mut app), vec!["Alice", "Bob"]);
}

/// `l` is the alias of `look` and follows the same path.
#[test]
fn look_alias_l_filters_hidden_players() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    hide(&mut app, bob);

    send_line(&mut app, alice, "l");
    run(&mut app);

    assert_eq!(routed(&app), 0, "l is intercepted like look");
    assert_eq!(occupants(&mut app), vec!["Alice"]);
}

/// A hidden occupant in a different room does not trigger interception.
#[test]
fn hidden_player_in_another_room_does_not_intercept() {
    let mut app = test_app();
    let camp = spawn_room(&mut app, "camp", "Camp", no_exits());
    let ridge = spawn_room(&mut app, "ridge", "Ridge", no_exits());
    let alice = spawn_player(&mut app, camp, "Alice");
    let bob = spawn_player(&mut app, ridge, "Bob");
    hide(&mut app, bob);

    send_line(&mut app, alice, "look");
    run(&mut app);

    assert_eq!(routed(&app), 1, "no stealthed player in Alice's room");
    assert_eq!(occupants(&mut app), vec!["Alice"]);
}

// ---------------------------------------------------------------------------
// Targeted `look`
// ---------------------------------------------------------------------------

/// `look <stealthed player>` is denied, and the interceptor handles it.
#[test]
fn look_hidden_player_is_denied() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    hide(&mut app, bob);

    send_line(&mut app, alice, "look Bob");
    run(&mut app);

    assert_eq!(routed(&app), 0, "the interceptor denied the look");
    let payloads = drain(&mut app);
    assert!(
        texts(&payloads)
            .iter()
            .any(|line| line.contains("You don't see them here.")),
        "denial text: {:?}",
        texts(&payloads)
    );
}

/// `look at <stealthed player>` (the `at` form) is denied too.
#[test]
fn look_at_hidden_player_is_denied() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    sneak(&mut app, bob);

    send_line(&mut app, alice, "look at Bob");
    run(&mut app);

    assert_eq!(routed(&app), 0);
    assert!(
        texts(&drain(&mut app))
            .iter()
            .any(|line| line.contains("You don't see them here."))
    );
}

/// A stealthed player can still look at themselves.
#[test]
fn look_self_when_hidden_passes_through() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let bob = spawn_player(&mut app, room, "Bob");
    hide(&mut app, bob);

    send_line(&mut app, bob, "look Bob");
    run(&mut app);

    assert_eq!(routed(&app), 1, "self is never hidden from self");
    assert!(
        texts(&drain(&mut app))
            .iter()
            .any(|line| line.contains("A fellow traveler."))
    );
}

/// `look <visible player>` still reaches the stdlib.
#[test]
fn look_visible_player_passes_through() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let _bob = spawn_player(&mut app, room, "Bob");

    send_line(&mut app, alice, "look Bob");
    run(&mut app);

    assert_eq!(routed(&app), 1);
    assert!(
        texts(&drain(&mut app))
            .iter()
            .any(|line| line.contains("A fellow traveler."))
    );
}

/// `look <npc>` is untouched by the interceptor and the stdlib returns the NPC
/// persona's first sentence.
#[test]
fn look_npc_passes_through() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    spawn_npc(
        &mut app,
        room,
        "Elder Marrow",
        "Elder Marrow is the village elder: patient, wry, faintly cryptic.",
    );

    send_line(&mut app, alice, "look elder marrow");
    run(&mut app);

    assert_eq!(routed(&app), 1);
    assert!(
        texts(&drain(&mut app))
            .iter()
            .any(|line| line.contains("Elder Marrow is the village elder"))
    );
}

/// `look <item>` is untouched and the stdlib returns the item description.
#[test]
fn look_item_passes_through() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    spawn_item(
        &mut app,
        room,
        "rusty sword",
        "A pitted, rust-flecked blade.",
    );

    send_line(&mut app, alice, "look sword");
    run(&mut app);

    assert_eq!(routed(&app), 1);
    assert!(
        texts(&drain(&mut app))
            .iter()
            .any(|line| line.contains("A pitted, rust-flecked blade."))
    );
}

/// `look <direction>` reaches the stdlib (which reports no such target, the
/// same as before this interceptor existed) rather than being denied as a
/// stealthed player.
#[test]
fn look_direction_passes_through() {
    let mut app = test_app();
    let camp = spawn_room(&mut app, "camp", "Camp", no_exits());
    let ridge = spawn_room(&mut app, "ridge", "Ridge", no_exits());
    let mut exits = no_exits();
    exits.map.insert(ExitKey::North, ridge);
    app.world_mut().entity_mut(camp).insert(exits);
    let alice = spawn_player(&mut app, camp, "Alice");

    send_line(&mut app, alice, "look north");
    run(&mut app);

    assert_eq!(routed(&app), 1, "direction look is stdlib's");
    assert!(
        texts(&drain(&mut app))
            .iter()
            .any(|line| line.contains("You see no north here."))
    );
}

/// A hidden player does not make a dark room's `look` render as if it were lit.
#[test]
fn hidden_player_in_dark_room_still_renders_darkness() {
    let mut app = test_app();
    let mut room = test_room("cave", "Cave");
    room.tags.insert(RoomTag::Dark);
    let cave = app.world_mut().spawn((room, no_exits())).id();
    let alice = spawn_player(&mut app, cave, "Alice");
    let bob = spawn_player(&mut app, cave, "Bob");
    hide(&mut app, bob);

    send_line(&mut app, alice, "look");
    run(&mut app);

    assert_eq!(
        routed(&app),
        0,
        "interceptor handles it (a hidden occupant exists)"
    );
    let view = scene(&drain(&mut app)).expect("dark scene");
    assert_eq!(view.name, "A Dark Room");
    assert!(view.occupants.is_empty(), "nothing is visible in the dark");
}

// ---------------------------------------------------------------------------
// `who`
// ---------------------------------------------------------------------------

/// `who` excludes stealthed players and lists the rest.
#[test]
fn who_excludes_hidden_players() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    let _carol = spawn_player(&mut app, room, "Carol");
    hide(&mut app, bob);

    send_line(&mut app, alice, "who");
    run(&mut app);

    assert_eq!(routed(&app), 0, "who is always intercepted");
    let lines = texts(&drain(&mut app));
    assert!(
        lines
            .iter()
            .any(|line| line == "Players online: Alice, Carol (2)"),
        "who line: {lines:?}"
    );
}

/// A stealthed asker still sees themselves in `who`.
#[test]
fn who_includes_hidden_asker() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let _alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    hide(&mut app, bob);

    send_line(&mut app, bob, "who");
    run(&mut app);

    let lines = texts(&drain(&mut app));
    assert!(
        lines
            .iter()
            .any(|line| line == "Players online: Alice, Bob (2)"),
        "who line: {lines:?}"
    );
}

/// A `TbaSneaking` player is excluded from `who` too.
#[test]
fn who_excludes_sneaking_players() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    sneak(&mut app, bob);

    send_line(&mut app, alice, "who");
    run(&mut app);

    let lines = texts(&drain(&mut app));
    assert!(
        lines.iter().any(|line| line == "Players online: Alice (1)"),
        "who line: {lines:?}"
    );
}

/// The typed `Command::Who` form is intercepted just like the raw line.
#[test]
fn who_typed_command_is_intercepted() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    hide(&mut app, bob);

    send_command(&mut app, alice, Command::Who);
    run(&mut app);

    assert_eq!(routed(&app), 0);
    let lines = texts(&drain(&mut app));
    assert!(lines.iter().any(|line| line == "Players online: Alice (1)"));
}

/// `TbaPlugin` registers the interceptor: a bare plugin app intercepts `who`
/// with no manual system wiring.
#[test]
fn tba_plugin_registers_the_interceptor() {
    let mut app = App::new();
    app.add_plugins(TbaPlugin);
    app.init_resource::<CommandSequence>();
    app.init_resource::<RoomOccupants>();
    let room = app
        .world_mut()
        .spawn((test_room("camp", "Camp"), no_exits()))
        .id();
    let alice = spawn_player(&mut app, room, "Alice");

    let id = send_line(&mut app, alice, "who");
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();

    assert!(
        app.world().resource::<HandledCommands>().is_handled(id),
        "TbaPlugin's interceptor marked the who handled"
    );
    assert!(
        texts(&drain(&mut app))
            .iter()
            .any(|line| line == "Players online: Alice (1)"),
        "TbaPlugin's interceptor emitted the who line"
    );
}

/// A login-state session's `who` is not intercepted (it must reach the login
/// flow).
#[test]
fn who_in_login_state_passes_through() {
    let mut app = test_app();
    let session = app
        .world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: "Newbie".to_owned(),
            },
            Session {
                state: SessionState::Connected,
            },
        ))
        .id();

    send_line(&mut app, session, "who");
    run(&mut app);

    assert_eq!(routed(&app), 1, "non-Playing sessions are left alone");
}
