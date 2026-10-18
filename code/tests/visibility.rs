//! Observer-relative perception integration tests.
//!
//! Two surfaces are exercised against the real engine renderers:
//!
//! - the **engine seam** — [`handle_look`] → `build_room_view`/`emit_scene`
//!   filters a concealed occupant using the engine `Concealment`/`Perception`
//!   carriers (nothing in this file reimplements the scene), and
//! - the pack's **`who` filter** ([`intercept_who`]) — the one global list the
//!   engine does not make observer-aware.
//!
//! The pack's own sync of `TbaHidden`/`TbaSneaking` into `Concealment` is
//! covered end-to-end by the `app_with_sync` cases and in `mib_tba::stealth`.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeSet, HashMap};

use mib_core::prelude::*;
use mib_stdlib::content::Sector;
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
        outdoor: false,
        weather_zone: None,
        ambient: None,
        ambient_stamp: None,
        ambient_phases: Default::default(),
        ambient_weather: Default::default(),
        time: None,
        weather_state: None,
    }
}

fn no_exits() -> Exits {
    Exits {
        map: HashMap::new(),
        hidden: BTreeSet::new(),
    }
}

/// The app under test: [`intercept_who`] in `PreRoute`, and a `Route` system
/// that delegates every non-handled `look`/`who` to the **real** stdlib
/// handlers. `sync` additionally wires the pack's carrier sync systems.
fn app_with(sync: bool) -> App {
    let mut app = App::new();
    app.init_resource::<HandledCommands>();
    app.init_resource::<CommandSequence>();
    app.init_resource::<VisibilityPolicy>();
    app.init_resource::<Routed>();
    app.add_message::<CommandReceived>();
    app.add_message::<OutputEvent>();
    app.configure_sets(Update, (CommandSet::PreRoute, CommandSet::Route).chain());
    if sync {
        app.add_systems(
            Update,
            (sync_concealment, sync_perception, intercept_who)
                .chain()
                .in_set(CommandSet::PreRoute),
        );
    } else {
        app.add_systems(Update, intercept_who.in_set(CommandSet::PreRoute));
    }
    app.add_systems(Update, engine_route.in_set(CommandSet::Route));
    app
}

fn test_app() -> App {
    app_with(false)
}

/// The `Route` stand-in: skip handled commands, and send `look`/`who` through
/// the real stdlib handlers (mirrors `route_commands`'s dispatch for these two
/// verbs). `who` normally never reaches here — the interceptor marks it.
#[allow(clippy::too_many_arguments)]
fn engine_route(
    mut reader: MessageReader<CommandReceived>,
    handled: Res<HandledCommands>,
    mut routed: ResMut<Routed>,
    players: Query<(Entity, &Player, &Location)>,
    who_players: Query<(&Player, &Location)>,
    rooms: Query<(&Room, &Exits)>,
    npcs: Query<(Entity, &Npc, &Location)>,
    concealment: Query<&Concealment>,
    perception: Query<&Perception>,
    policy: Res<VisibilityPolicy>,
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
                    &concealment,
                    &perception,
                    &policy,
                    &items,
                    &connections,
                    &discovered,
                    &mut output,
                );
            }
            Command::Who => {
                routed.0 += 1;
                handle_who(msg.session, &who_players, &mut output);
            }
            _ => {}
        }
    }
}

fn spawn_room(app: &mut App, key: &str, name: &str, exits: Exits) -> Entity {
    app.world_mut().spawn((test_room(key, name), exits)).id()
}

/// Spawn an observer/occupant with a live `Connection` so the GMCP gates can be
/// exercised. The receiver is dropped; the engine only reads capabilities.
fn spawn_player(app: &mut App, room: Entity, name: &str, gmcp: bool) -> Entity {
    let (egress, _rx) = tokio::sync::mpsc::channel(1);
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
            Connection {
                session_id: 1,
                frontend: FrontendKind::Telnet,
                capabilities: FrontendCapabilities {
                    ansi: true,
                    gmcp_like: gmcp,
                    streaming: false,
                    mxp: false,
                },
                egress,
            },
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

fn conceal(app: &mut App, entity: Entity, concealment: Concealment) {
    app.world_mut().entity_mut(entity).insert(concealment);
}

fn perceive_as(app: &mut App, entity: Entity, perception: Perception) {
    app.world_mut().entity_mut(entity).insert(perception);
}

fn send_line(app: &mut App, session: Entity, line: &str) -> CommandId {
    let id = app.world_mut().resource_mut::<CommandSequence>().next_id();
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(
            id,
            session,
            1,
            PlayerInput::RawLine(line.to_owned()),
        ));
    id
}

fn send_command(app: &mut App, session: Entity, command: Command) -> CommandId {
    let id = app.world_mut().resource_mut::<CommandSequence>().next_id();
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(
            id,
            session,
            1,
            PlayerInput::Command(command),
        ));
    id
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

fn error_kinds(payloads: &[OutboundPayload]) -> Vec<ErrorKind> {
    payloads
        .iter()
        .filter_map(|payload| match payload {
            OutboundPayload::Error { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect()
}

fn gmcp_player_names(payloads: &[OutboundPayload]) -> Vec<String> {
    payloads
        .iter()
        .find_map(|payload| match payload {
            OutboundPayload::GmcpLike { namespace, body } if namespace == "Room.Players" => Some(
                body.as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|value| value["name"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn gmcp_npc_names(payloads: &[OutboundPayload]) -> Vec<String> {
    payloads
        .iter()
        .find_map(|payload| match payload {
            OutboundPayload::GmcpLike { namespace, body } if namespace == "Room.Info" => Some(
                body["npcs"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|value| value.as_str().map(str::to_owned))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn routed(app: &App) -> usize {
    app.world().resource::<Routed>().0
}

fn who_line(payloads: &[OutboundPayload]) -> Option<String> {
    texts(payloads)
        .into_iter()
        .find(|line| line.starts_with("Players online:"))
}

// ---------------------------------------------------------------------------
// The engine seam: look / Scene / GMCP
// ---------------------------------------------------------------------------

/// A concealed player is absent from another observer's `Scene`; the stdlib
/// renderer (not a pack reimplementation) applied the filter.
#[test]
fn concealed_player_absent_from_anothers_scene() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    conceal(
        &mut app,
        bob,
        Concealment {
            hidden: true,
            invisible: false,
            stealth: 5,
        },
    );

    send_line(&mut app, alice, "look");
    run(&mut app);

    assert_eq!(routed(&app), 1, "the engine renderer handled the look");
    let view = scene(&drain(&mut app)).expect("a Scene was emitted");
    assert!(
        !view.occupants.contains(&"Bob".to_string()),
        "concealed Bob is hidden, got {:?}",
        view.occupants
    );
    assert!(
        view.occupants.contains(&"Alice".to_string()),
        "self is kept"
    );
}

/// The same concealment is filtered from both GMCP enumerations: the player
/// list and the invisible NPC in `Room.Info.npcs`.
#[test]
fn concealed_occupants_absent_from_gmcp() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice", true);
    let bob = spawn_player(&mut app, room, "Bob", false);
    let elder = spawn_npc(&mut app, room, "Elder Marrow", "Wise.");
    conceal(
        &mut app,
        bob,
        Concealment {
            hidden: true,
            invisible: false,
            stealth: 5,
        },
    );
    conceal(
        &mut app,
        elder,
        Concealment {
            hidden: false,
            invisible: true,
            stealth: 0,
        },
    );

    send_line(&mut app, alice, "look");
    run(&mut app);

    let payloads = drain(&mut app);
    assert_eq!(
        gmcp_player_names(&payloads),
        vec!["Alice".to_string()],
        "Room.Players hides the concealed player"
    );
    assert!(
        gmcp_npc_names(&payloads).is_empty(),
        "Room.Info.npcs hides the invisible NPC, got {:?}",
        gmcp_npc_names(&payloads)
    );
}

/// A concealed player is not a targeted `look` target.
#[test]
fn concealed_player_is_not_a_targeted_look_target() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    conceal(
        &mut app,
        bob,
        Concealment {
            hidden: true,
            invisible: false,
            stealth: 1,
        },
    );

    send_line(&mut app, alice, "look Bob");
    run(&mut app);

    assert!(
        error_kinds(&drain(&mut app)).contains(&ErrorKind::NoSuchTarget),
        "concealed Bob is not resolvable with `look`"
    );
}

/// A `SEE_INVIS` detector sees the concealed occupant — and only the detector.
#[test]
fn true_sight_detector_sees_concealed_player() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    let carol = spawn_player(&mut app, room, "Carol", false);
    conceal(
        &mut app,
        bob,
        Concealment {
            hidden: true,
            invisible: false,
            stealth: 5,
        },
    );
    perceive_as(
        &mut app,
        alice,
        Perception {
            sight: SEE_INVIS,
            detect: u8::MAX,
        },
    );

    // Alice (true sight) sees Bob; Carol (no perception) does not.
    send_line(&mut app, alice, "look");
    run(&mut app);
    let alice_view = scene(&drain(&mut app)).expect("Alice's scene");
    assert!(
        alice_view.occupants.contains(&"Bob".to_string()),
        "true sight reveals Bob, got {:?}",
        alice_view.occupants
    );

    send_line(&mut app, carol, "look");
    run(&mut app);
    let carol_view = scene(&drain(&mut app)).expect("Carol's scene");
    assert!(
        !carol_view.occupants.contains(&"Bob".to_string()),
        "Bob stays hidden from Carol, got {:?}",
        carol_view.occupants
    );
}

// ---------------------------------------------------------------------------
// The pack's `who` filter
// ---------------------------------------------------------------------------

/// `who` excludes concealed players and lists the rest.
#[test]
fn who_excludes_concealed_players() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    let _carol = spawn_player(&mut app, room, "Carol", false);
    conceal(
        &mut app,
        bob,
        Concealment {
            hidden: true,
            invisible: false,
            stealth: 5,
        },
    );

    send_line(&mut app, alice, "who");
    run(&mut app);

    assert_eq!(routed(&app), 0, "who is intercepted, not routed");
    assert_eq!(
        who_line(&drain(&mut app)).as_deref(),
        Some("Players online: Alice, Carol (2)")
    );
}

/// A concealed asker still sees themselves in `who`.
#[test]
fn who_includes_concealed_asker() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let _alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    conceal(
        &mut app,
        bob,
        Concealment {
            hidden: true,
            invisible: false,
            stealth: 5,
        },
    );

    send_line(&mut app, bob, "who");
    run(&mut app);

    assert_eq!(
        who_line(&drain(&mut app)).as_deref(),
        Some("Players online: Alice, Bob (2)")
    );
}

/// A detector sees a concealed player in `who`, matching the scene projection.
#[test]
fn who_shows_a_concealed_player_to_a_detector() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    conceal(
        &mut app,
        bob,
        Concealment {
            hidden: true,
            invisible: false,
            stealth: 5,
        },
    );
    perceive_as(
        &mut app,
        alice,
        Perception {
            sight: SEE_INVIS,
            detect: u8::MAX,
        },
    );

    send_line(&mut app, alice, "who");
    run(&mut app);

    assert_eq!(
        who_line(&drain(&mut app)).as_deref(),
        Some("Players online: Alice, Bob (2)")
    );
}

/// The typed `Command::Who` form is intercepted just like the raw line.
#[test]
fn who_typed_command_is_intercepted() {
    let mut app = test_app();
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    conceal(
        &mut app,
        bob,
        Concealment {
            hidden: true,
            invisible: false,
            stealth: 5,
        },
    );

    send_command(&mut app, alice, Command::Who);
    run(&mut app);

    assert_eq!(routed(&app), 0);
    assert_eq!(
        who_line(&drain(&mut app)).as_deref(),
        Some("Players online: Alice (1)")
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

/// `TbaPlugin` registers the `who` interceptor with no manual wiring.
#[test]
fn tba_plugin_registers_the_who_interceptor() {
    let mut app = App::new();
    app.add_plugins(TbaPlugin);
    app.init_resource::<CommandSequence>();
    app.init_resource::<RoomOccupants>();
    app.init_resource::<VisibilityPolicy>();
    let room = app
        .world_mut()
        .spawn((test_room("camp", "Camp"), no_exits()))
        .id();
    let alice = spawn_player(&mut app, room, "Alice", false);

    let id = send_line(&mut app, alice, "who");
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();

    assert!(
        app.world().resource::<HandledCommands>().is_handled(id),
        "TbaPlugin's interceptor marked the who handled"
    );
    assert_eq!(
        who_line(&drain(&mut app)).as_deref(),
        Some("Players online: Alice (1)")
    );
}

// ---------------------------------------------------------------------------
// The pack's carrier sync
// ---------------------------------------------------------------------------

/// The sync mirrors `TbaHidden` into `Concealment` and removes it when the
/// pack state clears — end-to-end through `who`.
#[test]
fn sync_mirrors_pack_hidden_state_into_concealment() {
    let mut app = app_with(true);
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    app.world_mut().entity_mut(bob).insert(TbaHidden);

    run(&mut app);
    assert_eq!(
        app.world().get::<Concealment>(bob),
        Some(&Concealment {
            hidden: true,
            invisible: false,
            stealth: 1,
        }),
        "TbaHidden syncs a Concealment (no level ⇒ stealth 1)"
    );

    send_line(&mut app, alice, "who");
    run(&mut app);
    assert_eq!(
        who_line(&drain(&mut app)).as_deref(),
        Some("Players online: Alice (1)"),
        "the synced Concealment filters who"
    );

    app.world_mut().entity_mut(bob).remove::<TbaHidden>();
    run(&mut app);
    assert!(
        app.world().get::<Concealment>(bob).is_none(),
        "clearing the pack state removes the carrier"
    );
}

/// `TbaSneaking` syncs the same carrier, so sneak and hide filter identically.
#[test]
fn sync_mirrors_pack_sneak_state_into_concealment() {
    let mut app = app_with(true);
    let room = spawn_room(&mut app, "camp", "Camp", no_exits());
    let _alice = spawn_player(&mut app, room, "Alice", false);
    let bob = spawn_player(&mut app, room, "Bob", false);
    app.world_mut().entity_mut(bob).insert(TbaSneaking);

    run(&mut app);
    assert!(
        app.world().get::<Concealment>(bob).is_some(),
        "TbaSneaking syncs a Concealment"
    );
}
