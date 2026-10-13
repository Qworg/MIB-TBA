//! Intake for commands, speech and movement: the command gate, the pack
//! handlers' respect for a taken-over command, `speech` and `act` triggers,
//! and the leave / enter / entry / greet / door / login triggers.
//!
//! Expectations come from tbaMUD's C (`command_interpreter`, `do_say`,
//! `do_simple_move`, `do_doorcmd`, `enter_player_game`) and from the dispatch
//! notes in `dg-vm`, never from what the code happens to return.
//!
//! The engine's router is not part of these apps: a stand-in system applies a
//! typed `Move` the way `route_commands` does (it skips a command already
//! marked handled, marks each command it routes handled, and sets the mover's `Location` from the room's exits). The
//! real router needs the engine's whole command stack and content, and adds
//! nothing to what is tested here, which is the order of the intake systems
//! around it. The pack handler checked is the real `intercept_who`.
//!
//! Triggers prove they ran by copying their event variables into globals.
//!
//! Tests may unwrap/expect.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dg_common;

use dg_common::*;
use dg_vm::fields_char::{CharHost, CharStat};
use dg_vm::types::POS_SLEEPING;
use dg_vm::types::ScriptMemory;
use dg_vm::{BaseHost, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::dg::intake::DgIntake;
use mib_tba::dg::intake::commands::DgClaimed;
use mib_tba::dg::intake::movement::{DgLastRoom, DgLoggedIn};
use mib_tba::dg::script::DgProtos;
use mib_tba::prelude::*;
use mib_tba::triggers::content::TriggerDef;
use mib_tba::visibility::intercept_who;

// -- harness -----------------------------------------------------------------

/// An object `command` trigger's location mask: the object lies in the room.
const OCMD_ROOM: i32 = 4;

/// The text of a character reference, as `%actor%` expands (`UID_CHAR` and the id).
fn uid_text(id: dg_vm::Uid) -> String {
    format!("{}{id}", dg_vm::types::UID_CHAR)
}

/// The engine router's part in a move: a typed `Move` the intake did not take
/// over puts the mover in the room the exit leads to.
fn apply_moves(
    mut reader: MessageReader<CommandReceived>,
    mut handled: ResMut<HandledCommands>,
    exits: Query<&Exits>,
    mut locations: Query<&mut Location>,
) {
    for msg in reader.read() {
        if handled.is_handled(msg.id) {
            continue;
        }
        // The router drops a line over the rate limit without marking it.
        if let PlayerInput::Command(Command::Say { text }) = &msg.input
            && text.starts_with("flood")
        {
            continue;
        }
        // The router marks every command it routes handled.
        handled.mark_handled(msg.id);
        let PlayerInput::Command(Command::Move { direction }) = &msg.input else {
            continue;
        };
        let Ok(mut location) = locations.get_mut(msg.session) else {
            continue;
        };
        let Some(to) = exits
            .get(location.room)
            .ok()
            .and_then(|exits| exits.map.get(direction).copied())
        else {
            continue;
        };
        location.room = to;
    }
}

/// The DG runtime with every intake system, the command plumbing and the
/// stand-in router.
fn intake_app() -> App {
    let mut app = app();
    app.init_resource::<HandledCommands>();
    app.add_message::<CommandReceived>();
    mib_tba::dg::intake::register(&mut app);
    app.add_systems(
        Update,
        apply_moves
            .after(DgIntake::PreCommit)
            .after(mib_tba::dg::intake::commands::fold_handled),
    );
    app.configure_sets(Update, DgIntake::PostRoute.after(apply_moves));
    app.add_systems(Update, intercept_who.after(DgIntake::CommandGate));
    app
}

/// A trigger prototype with an argument list (`define` has none).
fn define_arg(
    app: &mut App,
    vnum: i32,
    attach: &str,
    flags: &str,
    narg: i32,
    arglist: &str,
    script: &str,
) {
    let def = TriggerDef {
        vnum,
        name: format!("trigger {vnum}"),
        attach_type: attach.to_owned(),
        flags: flags.to_owned(),
        types: Vec::new(),
        narg,
        arglist: arglist.to_owned(),
        script: script.to_owned(),
    };
    assert!(
        app.world_mut().resource_mut::<DgProtos>().add(&def),
        "trigger {vnum} loads"
    );
}

/// A one-way exit.
fn exit(app: &mut App, from: Entity, key: ExitKey, to: Entity) {
    let world = app.world_mut();
    if world.get::<Exits>(from).is_none() {
        world.entity_mut(from).insert(Exits::default());
    }
    world
        .get_mut::<Exits>(from)
        .expect("exits")
        .map
        .insert(key, to);
}

/// Two rooms joined north-south: `(a, b)` with `b` north of `a`.
fn two_rooms(app: &mut App) -> (Entity, Entity) {
    let a = room(app, 1);
    let b = room(app, 2);
    exit(app, a, ExitKey::North, b);
    exit(app, b, ExitKey::South, a);
    (a, b)
}

/// A playing player who is already past the login triggers.
fn seated_player(app: &mut App, in_room: Entity) -> Entity {
    let who = player(app, in_room, "Alice", 7);
    app.world_mut()
        .entity_mut(who)
        .insert((DgLoggedIn, DgLastRoom(in_room)));
    who
}

/// Send a command and run one frame.
fn send(app: &mut App, id: u64, session: Entity, input: PlayerInput) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived::new(CommandId(id), session, id, input));
    app.update();
}

fn line(text: &str) -> PlayerInput {
    PlayerInput::RawLine(text.to_owned())
}

fn typed(command: Command) -> PlayerInput {
    PlayerInput::Command(command)
}

fn go_north() -> PlayerInput {
    typed(Command::Move {
        direction: ExitKey::North,
    })
}

fn is_handled(app: &App, id: u64) -> bool {
    app.world()
        .resource::<HandledCommands>()
        .is_handled(CommandId(id))
}

fn claimed(app: &App, id: u64) -> bool {
    app.world().resource::<DgClaimed>().contains(CommandId(id))
}

fn room_of(app: &App, who: Entity) -> Entity {
    app.world().get::<Location>(who).expect("location").room
}

/// A script that records its event variables, then lets the event go on.
const SEEN: &str = "set cmd %cmd%\nglobal cmd\nset arg %arg%\nglobal arg\n\
                    set actor %actor%\nglobal actor\nset ran 1\nglobal ran";

/// A script that records that it ran, then returns `ret`.
fn ran_then(ret: i32) -> String {
    format!("set ran 1\nglobal ran\nreturn {ret}")
}

/// Whether the host models a character's position yet. The mob triggers that
/// need an awake mob (`speech`, `leave`, `door`, `greet`) read it, and until the
/// character facts are modelled every mob counts as asleep and those triggers
/// cannot fire; a test of such a trigger returns early rather than fail on a
/// fact it does not own.
fn mob_is_awake(app: &mut App, mob: Entity) -> bool {
    let id = uid(app, mob);
    run(app, |host| host.char_stat(id, CharStat::Pos) > POS_SLEEPING)
}

fn ran(app: &mut App, owner: Entity) -> bool {
    let id = uid(app, owner);
    global(app, id, "ran").is_some()
}

// -- the command gate --------------------------------------------------------

/// A room, a guard, a lamp on the floor and a player, each carrying one
/// `command` trigger on `*`, with the given scripts.
fn gate_world(
    world_script: &str,
    mob_script: &str,
    obj_script: &str,
) -> (App, Entity, [Entity; 3]) {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 10, "wld", "c", 0, "*", world_script);
    define_arg(&mut app, 11, "mob", "c", 0, "*", mob_script);
    define_arg(&mut app, 12, "obj", "c", OCMD_ROOM, "*", obj_script);
    attach(&mut app, "room:tba.1", &[10]);
    attach(&mut app, "npc:tba_npc_5", &[11]);
    attach(&mut app, "item:tba_item_9", &[12]);
    let guard = npc(&mut app, a, 5, "the guard");
    let lamp = item_on_ground(&mut app, a, 9, "a lamp");
    let alice = seated_player(&mut app, a);
    (app, alice, [a, guard, lamp])
}

#[test]
fn the_gate_offers_a_command_to_world_then_mob_then_object() {
    // The room passes (returns 0), the guard takes the command over, and the
    // lamp's trigger is never offered it.
    let (mut app, alice, [a, guard, lamp]) = gate_world(&ran_then(0), &ran_then(1), &ran_then(1));
    send(&mut app, 1, alice, line("dance wildly"));
    assert!(ran(&mut app, a), "the world trigger comes first");
    assert!(ran(&mut app, guard), "then the mob's");
    assert!(!ran(&mut app, lamp), "and stops at the first that takes it");
    assert!(is_handled(&app, 1));
    assert!(claimed(&app, 1));

    // Nobody takes it over: all three run, and the command goes on.
    let (mut app, alice, [a, guard, lamp]) = gate_world(&ran_then(0), &ran_then(0), &ran_then(0));
    send(&mut app, 1, alice, line("dance wildly"));
    assert!(ran(&mut app, a) && ran(&mut app, guard) && ran(&mut app, lamp));
    assert!(!claimed(&app, 1));
    assert!(!claimed(&app, 1));
}

#[test]
fn a_world_trigger_that_takes_the_command_stops_the_mob_and_object() {
    let (mut app, alice, [a, guard, lamp]) = gate_world(&ran_then(1), &ran_then(1), &ran_then(1));
    send(&mut app, 1, alice, line("dance"));
    assert!(ran(&mut app, a));
    assert!(!ran(&mut app, guard));
    assert!(!ran(&mut app, lamp));
    assert!(is_handled(&app, 1));
}

#[test]
fn an_object_trigger_is_offered_what_the_others_passed() {
    let (mut app, alice, [a, guard, lamp]) = gate_world(&ran_then(0), &ran_then(0), &ran_then(1));
    send(&mut app, 1, alice, line("dance"));
    assert!(ran(&mut app, a) && ran(&mut app, guard) && ran(&mut app, lamp));
    assert!(is_handled(&app, 1));
}

#[test]
fn the_gate_derives_the_command_word_and_argument_as_the_interpreter_sees_them() {
    let cases = [
        (line("  GET   sword  "), "get", "sword  "),
        (line("say hello there"), "say", "hello there"),
        (
            typed(Command::Say {
                text: "hello there".to_owned(),
            }),
            "say",
            "hello there",
        ),
        (go_north(), "north", ""),
        (
            typed(Command::Open {
                target: Some("door".to_owned()),
            }),
            "open",
            "door",
        ),
        (line("'hi"), "'", "hi"),
    ];
    for (input, cmd, arg) in cases {
        let mut app = intake_app();
        let a = room(&mut app, 1);
        define_arg(&mut app, 11, "mob", "c", 0, "*", SEEN);
        attach(&mut app, "npc:tba_npc_5", &[11]);
        let guard = npc(&mut app, a, 5, "the guard");
        let alice = seated_player(&mut app, a);
        send(&mut app, 1, alice, input);
        let (guard_id, alice_id) = (uid(&mut app, guard), uid(&mut app, alice));
        assert_eq!(global(&mut app, guard_id, "cmd").as_deref(), Some(cmd));
        assert_eq!(
            global(&mut app, guard_id, "arg").as_deref(),
            Some(arg.trim_start())
        );
        assert_eq!(
            global(&mut app, guard_id, "actor"),
            Some(uid_text(alice_id)),
            "{cmd}: the actor is the player"
        );
    }
}

#[test]
fn a_trigger_matches_the_command_it_names_and_no_other() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 11, "mob", "c", 0, "dan", &ran_then(1));
    attach(&mut app, "npc:tba_npc_5", &[11]);
    let guard = npc(&mut app, a, 5, "the guard");
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, line("look"));
    assert!(!ran(&mut app, guard));
    assert!(!claimed(&app, 1));
    // The trigger's word is a prefix of what was typed.
    send(&mut app, 2, alice, line("dance"));
    assert!(ran(&mut app, guard));
    assert!(is_handled(&app, 2));
}

#[test]
fn a_session_that_is_not_playing_is_not_offered_to_triggers() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 11, "mob", "c", 0, "*", &ran_then(1));
    attach(&mut app, "npc:tba_npc_5", &[11]);
    let guard = npc(&mut app, a, 5, "the guard");
    let alice = seated_player(&mut app, a);
    app.world_mut()
        .get_mut::<Session>(alice)
        .expect("session")
        .state = SessionState::Connected;
    send(&mut app, 1, alice, line("dance"));
    assert!(!ran(&mut app, guard));
    assert!(!claimed(&app, 1));
}

#[test]
fn a_taken_over_command_does_not_reach_a_pack_handler() {
    // `who` is answered by the pack's `intercept_who` unless a trigger took it.
    let mut app = intake_app();
    let a = room(&mut app, 1);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, line("who"));
    let shown = output(&mut app);
    assert!(
        shown.iter().any(|text| text.contains("Players online")),
        "{shown:?}"
    );

    define_arg(&mut app, 11, "mob", "c", 0, "who", &ran_then(1));
    attach(&mut app, "npc:tba_npc_5", &[11]);
    let guard = npc(&mut app, a, 5, "the guard");
    send(&mut app, 2, alice, line("who"));
    assert!(ran(&mut app, guard));
    let shown = output(&mut app);
    assert!(
        !shown.iter().any(|text| text.contains("Players online")),
        "the pack answered a command a trigger took over: {shown:?}"
    );
    assert!(is_handled(&app, 2));
}

// -- speech and act ----------------------------------------------------------

const SPEECH: &str = "set speech %speech%\nglobal speech\nset actor %actor%\nglobal actor\n\
                      set ran 1\nglobal ran";

fn say(text: &str) -> PlayerInput {
    typed(Command::Say {
        text: text.to_owned(),
    })
}

#[test]
fn a_say_is_offered_to_the_speech_triggers_of_the_room() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 21, "wld", "d", 0, "hello", SPEECH);
    attach(&mut app, "room:tba.1", &[21]);
    let alice = seated_player(&mut app, a);

    send(&mut app, 1, alice, say("  goodbye all"));
    assert!(!ran(&mut app, a), "the trigger's word is not in the line");

    send(&mut app, 2, alice, say("  well hello there"));
    let (room_id, alice_id) = (uid(&mut app, a), uid(&mut app, alice));
    assert_eq!(
        global(&mut app, room_id, "speech").as_deref(),
        Some("well hello there"),
        "the spoken text, without its leading spaces"
    );
    assert_eq!(global(&mut app, room_id, "actor"), Some(uid_text(alice_id)));
    assert!(!claimed(&app, 2), "speech does not take the say over");

    // A raw `say` line is the same.
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 21, "wld", "d", 0, "hello", SPEECH);
    attach(&mut app, "room:tba.1", &[21]);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, line("say hello"));
    let room_id = uid(&mut app, a);
    assert_eq!(
        global(&mut app, room_id, "speech").as_deref(),
        Some("hello")
    );
}

#[test]
fn a_say_is_offered_to_the_speech_triggers_of_mobs() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 20, "mob", "d", 0, "hello", SPEECH);
    attach(&mut app, "npc:tba_npc_5", &[20]);
    let guard = npc(&mut app, a, 5, "the guard");
    let alice = seated_player(&mut app, a);
    if !mob_is_awake(&mut app, guard) {
        return;
    }
    send(&mut app, 1, alice, say("goodbye"));
    assert!(!ran(&mut app, guard));
    send(&mut app, 2, alice, say("  well hello there"));
    let (guard_id, alice_id) = (uid(&mut app, guard), uid(&mut app, alice));
    assert_eq!(
        global(&mut app, guard_id, "speech").as_deref(),
        Some("well hello there")
    );
    assert_eq!(
        global(&mut app, guard_id, "actor"),
        Some(uid_text(alice_id))
    );
}

#[test]
fn a_say_is_not_offered_to_act_triggers() {
    // `do_say` acts with DG_NO_TRIG: a mob's `act` trigger never sees the line.
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 22, "mob", "e", 0, "says", &ran_then(1));
    attach(&mut app, "npc:tba_npc_5", &[22]);
    let guard = npc(&mut app, a, 5, "the guard");
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, line("say hello"));
    assert!(!ran(&mut app, guard));
}

#[test]
fn a_say_a_command_trigger_took_over_is_not_spoken() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 20, "wld", "d", 0, "hello", &ran_then(1));
    define_arg(&mut app, 21, "mob", "c", 0, "say", &ran_then(1));
    attach(&mut app, "room:tba.1", &[20]);
    attach(&mut app, "npc:tba_npc_5", &[21]);
    npc(&mut app, a, 5, "the guard");
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, line("say hello"));
    assert!(!ran(&mut app, a));
    assert!(is_handled(&app, 1));
}

fn wave() -> SocialDef {
    SocialDef {
        sort_as: "wave".into(),
        hide: false,
        min_char_position: 5,
        min_victim_position: 0,
        min_level_char: 1,
        char_no_arg: Some("You wave.".to_owned()),
        others_no_arg: Some("$n waves.".to_owned()),
        char_found: Some("You wave at $N.".to_owned()),
        others_found: Some("$n waves at $N.".to_owned()),
        vict_found: Some("$n waves at you.".to_owned()),
        not_found: Some("Wave at who?".to_owned()),
        char_auto: Some("You wave at yourself.".to_owned()),
        others_auto: Some("$n waves at $mself.".to_owned()),
        char_body_found: None,
        others_body_found: None,
        vict_body_found: None,
        char_obj_found: None,
        others_obj_found: None,
    }
}

#[test]
fn a_social_is_acted_to_the_mobs_that_see_it() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    app.world_mut()
        .resource_mut::<TbaContent>()
        .socials
        .insert("wave".into(), wave());
    let act = "set arg %arg%\nglobal arg\nset actor %actor%\nglobal actor\n\
               set victim %victim%\nglobal victim\nset ran 1\nglobal ran";
    // The guard is waved at, the clerk looks on, the beggar's trigger matches
    // only the line a bare wave shows.
    define_arg(&mut app, 30, "mob", "e", 0, "waves at you", act);
    define_arg(&mut app, 31, "mob", "e", 0, "waves at", act);
    define_arg(&mut app, 32, "mob", "e", 0, "waves.", act);
    attach(&mut app, "npc:tba_npc_5", &[30]);
    attach(&mut app, "npc:tba_npc_6", &[31]);
    attach(&mut app, "npc:tba_npc_7", &[32]);
    let guard = npc(&mut app, a, 5, "the guard");
    let clerk = npc(&mut app, a, 6, "the clerk");
    let beggar = npc(&mut app, a, 7, "the beggar");
    let alice = seated_player(&mut app, a);

    send(&mut app, 1, alice, line("social wave guard"));
    let (guard_id, clerk_id, beggar_id, alice_id) = (
        uid(&mut app, guard),
        uid(&mut app, clerk),
        uid(&mut app, beggar),
        uid(&mut app, alice),
    );
    // The victim is shown `vict_found`.
    assert_eq!(
        global(&mut app, guard_id, "arg").as_deref(),
        Some("Alice waves at you.")
    );
    assert_eq!(
        global(&mut app, guard_id, "actor"),
        Some(uid_text(alice_id))
    );
    // The bystander is shown `others_found`, with the victim named.
    assert_eq!(
        global(&mut app, clerk_id, "arg").as_deref(),
        Some("Alice waves at the guard.")
    );
    assert_eq!(
        global(&mut app, clerk_id, "actor"),
        Some(uid_text(alice_id))
    );
    assert_eq!(
        global(&mut app, clerk_id, "victim"),
        Some(uid_text(guard_id))
    );
    assert!(!ran(&mut app, beggar));

    // With no target the whole room is shown `others_no_arg`.
    send(&mut app, 2, alice, line("social wave"));
    assert_eq!(
        global(&mut app, beggar_id, "arg").as_deref(),
        Some("Alice waves.")
    );
    assert_eq!(
        global(&mut app, beggar_id, "actor"),
        Some(uid_text(alice_id))
    );
}

// -- movement ----------------------------------------------------------------

const DIRECTION: &str = "set direction %direction%\nglobal direction\nset actor %actor%\nglobal actor\n\
                         set ran 1\nglobal ran";

fn with_return(script: &str, ret: i32) -> String {
    format!("{script}\nreturn {ret}")
}

#[test]
fn a_leave_trigger_that_refuses_keeps_the_player_where_they_are() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(
        &mut app,
        40,
        "wld",
        "q",
        100,
        "*",
        &with_return(DIRECTION, 0),
    );
    attach(&mut app, "room:tba.1", &[40]);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, go_north());
    assert_eq!(room_of(&app, alice), a, "the room barred the way");
    assert_ne!(room_of(&app, alice), b);
    let (room_id, alice_id) = (uid(&mut app, a), uid(&mut app, alice));
    assert_eq!(
        global(&mut app, room_id, "direction").as_deref(),
        Some("north")
    );
    assert_eq!(global(&mut app, room_id, "actor"), Some(uid_text(alice_id)));
    assert!(is_handled(&app, 1), "a refusal takes the command over");
    assert!(claimed(&app, 1));
}

#[test]
fn a_leave_trigger_that_lets_go_does_not_stop_the_move() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 40, "wld", "q", 100, "*", &ran_then(1));
    attach(&mut app, "room:tba.1", &[40]);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, go_north());
    assert!(ran(&mut app, a));
    assert_eq!(room_of(&app, alice), b);
    assert!(!claimed(&app, 1));
}

#[test]
fn a_move_through_no_exit_runs_no_leave_trigger() {
    let mut app = intake_app();
    let (a, _b) = two_rooms(&mut app);
    define_arg(&mut app, 40, "wld", "q", 100, "*", &ran_then(0));
    attach(&mut app, "room:tba.1", &[40]);
    let alice = seated_player(&mut app, a);
    send(
        &mut app,
        1,
        alice,
        typed(Command::Move {
            direction: ExitKey::West,
        }),
    );
    assert!(!ran(&mut app, a));
    assert!(!claimed(&app, 1));
}

#[test]
fn leave_triggers_run_world_then_object_and_the_first_refusal_stops_the_rest() {
    let mut app = intake_app();
    let (a, _b) = two_rooms(&mut app);
    define_arg(&mut app, 41, "wld", "q", 100, "*", &ran_then(0));
    define_arg(&mut app, 42, "obj", "q", 100, "*", &ran_then(1));
    attach(&mut app, "room:tba.1", &[41]);
    attach(&mut app, "item:tba_item_9", &[42]);
    let lamp = item_on_ground(&mut app, a, 9, "a lamp");
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, go_north());
    assert!(ran(&mut app, a));
    assert!(!ran(&mut app, lamp), "the room refused first");
    assert_eq!(room_of(&app, alice), a);

    // The object is consulted when the room lets go.
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 41, "wld", "q", 100, "*", &ran_then(1));
    define_arg(&mut app, 42, "obj", "q", 100, "*", &ran_then(0));
    attach(&mut app, "room:tba.1", &[41]);
    attach(&mut app, "item:tba_item_9", &[42]);
    let lamp = item_on_ground(&mut app, a, 9, "a lamp");
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, go_north());
    assert!(ran(&mut app, a) && ran(&mut app, lamp));
    assert_eq!(room_of(&app, alice), a);
    assert_ne!(room_of(&app, alice), b);
}

#[test]
fn a_mob_leave_trigger_is_asked_before_the_room_and_can_refuse() {
    let mut app = intake_app();
    let (a, _b) = two_rooms(&mut app);
    define_arg(
        &mut app,
        40,
        "mob",
        "q",
        100,
        "*",
        &with_return(DIRECTION, 0),
    );
    define_arg(&mut app, 41, "wld", "q", 100, "*", &ran_then(1));
    attach(&mut app, "npc:tba_npc_5", &[40]);
    attach(&mut app, "room:tba.1", &[41]);
    let guard = npc(&mut app, a, 5, "the guard");
    let alice = seated_player(&mut app, a);
    if !mob_is_awake(&mut app, guard) {
        return;
    }
    send(&mut app, 1, alice, go_north());
    assert_eq!(room_of(&app, alice), a);
    let guard_id = uid(&mut app, guard);
    assert_eq!(
        global(&mut app, guard_id, "direction").as_deref(),
        Some("north")
    );
    assert!(
        !ran(&mut app, a),
        "the mob refused before the room was asked"
    );
    assert!(is_handled(&app, 1));
}

#[test]
fn a_move_nobody_refuses_runs_enter_with_the_direction_it_came_from() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 43, "wld", "g", 100, "*", DIRECTION);
    attach(&mut app, "room:tba.2", &[43]);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, go_north());
    assert_eq!(room_of(&app, alice), b);
    assert!(!claimed(&app, 1));
    let (room_id, alice_id) = (uid(&mut app, b), uid(&mut app, alice));
    // Going north, the player arrives from the south.
    assert_eq!(
        global(&mut app, room_id, "direction").as_deref(),
        Some("south")
    );
    assert_eq!(global(&mut app, room_id, "actor"), Some(uid_text(alice_id)));
    assert_eq!(
        app.world().get::<DgLastRoom>(alice).map(|last| last.0),
        Some(b)
    );
}

#[test]
fn a_greeter_sees_the_player_arrive() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 44, "mob", "g", 100, "*", DIRECTION);
    attach(&mut app, "npc:tba_npc_6", &[44]);
    let greeter = npc(&mut app, b, 6, "the greeter");
    let alice = seated_player(&mut app, a);
    if !mob_is_awake(&mut app, greeter) {
        return;
    }
    send(&mut app, 1, alice, go_north());
    assert_eq!(room_of(&app, alice), b);
    let (greeter_id, alice_id) = (uid(&mut app, greeter), uid(&mut app, alice));
    assert_eq!(
        global(&mut app, greeter_id, "direction").as_deref(),
        Some("south")
    );
    assert_eq!(
        global(&mut app, greeter_id, "actor"),
        Some(uid_text(alice_id))
    );
}

#[test]
fn an_enter_refusal_puts_the_player_back_and_shows_them_the_room() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 43, "wld", "g", 100, "*", &ran_then(0));
    attach(&mut app, "room:tba.2", &[43]);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, go_north());
    assert!(ran(&mut app, b));
    assert_eq!(room_of(&app, alice), a);
    assert_eq!(
        app.world().get::<DgLastRoom>(alice).map(|last| last.0),
        Some(a)
    );
    assert!(
        output(&mut app).iter().any(|text| text.contains("Room 1")),
        "the player is shown the room they are back in"
    );
}

#[test]
fn a_greet_refusal_puts_the_player_back_and_shows_them_the_room() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 43, "wld", "g", 100, "*", &ran_then(1));
    define_arg(&mut app, 44, "mob", "g", 100, "*", &ran_then(0));
    attach(&mut app, "room:tba.2", &[43]);
    attach(&mut app, "npc:tba_npc_6", &[44]);
    let greeter = npc(&mut app, b, 6, "the greeter");
    let alice = seated_player(&mut app, a);
    if !mob_is_awake(&mut app, greeter) {
        return;
    }
    send(&mut app, 1, alice, go_north());
    assert!(ran(&mut app, b), "enter ran before greet");
    assert!(ran(&mut app, greeter));
    assert_eq!(room_of(&app, alice), a);
    assert!(output(&mut app).iter().any(|text| text.contains("Room 1")));
}

#[test]
fn an_npc_that_arrives_by_itself_fires_entry_and_enter() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 45, "mob", "i", 100, "*", &ran_then(1));
    define_arg(&mut app, 47, "wld", "g", 100, "*", DIRECTION);
    attach(&mut app, "npc:tba_npc_5", &[45]);
    attach(&mut app, "room:tba.2", &[47]);
    let walker = npc(&mut app, a, 5, "the walker");
    // The first look only records where everyone is.
    tick_pulses(&mut app, 1);
    assert!(!ran(&mut app, walker));

    app.world_mut()
        .get_mut::<Location>(walker)
        .expect("location")
        .room = b;
    tick_pulses(&mut app, 1);
    let (walker_id, room_id) = (uid(&mut app, walker), uid(&mut app, b));
    assert!(
        ran(&mut app, walker),
        "entry_mtrigger runs for the arriving mob"
    );
    assert_eq!(
        global(&mut app, room_id, "actor"),
        Some(uid_text(walker_id))
    );
    assert_eq!(
        global(&mut app, room_id, "direction").as_deref(),
        Some("south")
    );
    assert_eq!(room_of(&app, walker), b);

    // It stays where it is: nothing fires again.
    app.world_mut()
        .get_mut::<Location>(walker)
        .expect("location")
        .set_changed();
    tick_pulses(&mut app, 1);
    assert_eq!(room_of(&app, walker), b);
}

#[test]
fn an_npc_that_arrives_by_itself_is_greeted() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 46, "mob", "g", 100, "*", DIRECTION);
    attach(&mut app, "npc:tba_npc_6", &[46]);
    let walker = npc(&mut app, a, 5, "the walker");
    let greeter = npc(&mut app, b, 6, "the greeter");
    if !mob_is_awake(&mut app, greeter) {
        return;
    }
    tick_pulses(&mut app, 1);
    app.world_mut()
        .get_mut::<Location>(walker)
        .expect("location")
        .room = b;
    tick_pulses(&mut app, 1);
    let (walker_id, greeter_id) = (uid(&mut app, walker), uid(&mut app, greeter));
    assert_eq!(
        global(&mut app, greeter_id, "actor"),
        Some(uid_text(walker_id))
    );
    assert_eq!(
        global(&mut app, greeter_id, "direction").as_deref(),
        Some("south")
    );
}

#[test]
fn an_entry_refusal_sends_a_walking_npc_back() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 47, "wld", "g", 100, "*", &ran_then(0));
    attach(&mut app, "room:tba.2", &[47]);
    let walker = npc(&mut app, a, 5, "the walker");
    tick_pulses(&mut app, 1);
    app.world_mut()
        .get_mut::<Location>(walker)
        .expect("location")
        .room = b;
    tick_pulses(&mut app, 1);
    assert!(ran(&mut app, b));
    assert_eq!(
        room_of(&app, walker),
        a,
        "enter_wtrigger refused the arrival"
    );
}

#[test]
fn a_move_a_script_made_does_not_fire_entry_or_enter() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 45, "mob", "i", 100, "*", &ran_then(1));
    define_arg(&mut app, 47, "wld", "g", 100, "*", &ran_then(1));
    attach(&mut app, "npc:tba_npc_5", &[45]);
    attach(&mut app, "room:tba.2", &[47]);
    let walker = npc(&mut app, a, 5, "the walker");
    tick_pulses(&mut app, 1);

    // The host changes the location and stamps the mover, as a `teleport` does.
    app.world_mut()
        .get_mut::<Location>(walker)
        .expect("location")
        .room = b;
    run(&mut app, |host| host.stamp_order(walker));
    tick_pulses(&mut app, 1);
    assert!(!ran(&mut app, walker));
    assert!(!ran(&mut app, b));
    assert_eq!(room_of(&app, walker), b);
}

#[test]
fn a_leave_refusal_undoes_a_move_the_game_made_on_its_own() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(
        &mut app,
        40,
        "wld",
        "q",
        100,
        "*",
        &with_return(DIRECTION, 0),
    );
    define_arg(&mut app, 47, "wld", "g", 100, "*", &ran_then(1));
    attach(&mut app, "room:tba.1", &[40]);
    attach(&mut app, "room:tba.2", &[47]);
    let walker = npc(&mut app, a, 7, "the walker");
    tick_pulses(&mut app, 1);
    app.world_mut()
        .get_mut::<Location>(walker)
        .expect("location")
        .room = b;
    tick_pulses(&mut app, 1);
    let room_id = uid(&mut app, a);
    assert_eq!(
        global(&mut app, room_id, "direction").as_deref(),
        Some("north")
    );
    assert_eq!(room_of(&app, walker), a);
    assert!(!ran(&mut app, b), "the walker never arrived");
}

// -- doors ---------------------------------------------------------------------

/// Room 1 with a closed, unlocked door to the north, and a player in it.
fn door_world() -> (App, Entity, Entity) {
    let mut app = intake_app();
    let (a, _b) = two_rooms(&mut app);
    let door = Door {
        key: None,
        name: Some("gate".to_owned()),
        pickable: false,
        difficulty: 0,
        default_closed: true,
        default_locked: false,
    };
    let mut doors = Doors::default();
    doors.by_dir.insert(ExitKey::North, door);
    app.world_mut().entity_mut(a).insert(doors);
    let alice = seated_player(&mut app, a);
    (app, a, alice)
}

fn open_north() -> PlayerInput {
    typed(Command::Open {
        target: Some("north".to_owned()),
    })
}

/// Records the door event, then returns `ret`.
fn door_script(ret: i32) -> String {
    format!(
        "set cmd %cmd%\nglobal cmd\nset direction %direction%\nglobal direction\n\
         set ran 1\nglobal ran\nreturn {ret}"
    )
}

#[test]
fn a_world_door_trigger_that_refuses_stops_the_command() {
    let (mut app, a, alice) = door_world();
    define_arg(&mut app, 51, "wld", "r", 100, "*", &door_script(0));
    attach(&mut app, "room:tba.1", &[51]);
    send(&mut app, 1, alice, open_north());
    let room_id = uid(&mut app, a);
    // The command word is the sub-command's name, the direction the door's.
    assert_eq!(global(&mut app, room_id, "cmd").as_deref(), Some("open"));
    assert_eq!(
        global(&mut app, room_id, "direction").as_deref(),
        Some("north")
    );
    assert!(is_handled(&app, 1));
    assert!(claimed(&app, 1));
}

#[test]
fn a_world_door_trigger_that_lets_go_leaves_the_command_to_the_engine() {
    let (mut app, a, alice) = door_world();
    define_arg(&mut app, 51, "wld", "r", 100, "*", &door_script(1));
    attach(&mut app, "room:tba.1", &[51]);
    send(&mut app, 1, alice, open_north());
    assert!(ran(&mut app, a));
    assert!(!claimed(&app, 1));
}

#[test]
fn each_door_command_has_its_own_sub_command_name() {
    let cases = [
        (
            Command::Open {
                target: Some("north".to_owned()),
            },
            "open",
        ),
        (
            Command::Close {
                target: Some("north".to_owned()),
            },
            "close",
        ),
        (
            Command::Unlock {
                target: Some("north".to_owned()),
            },
            "unlock",
        ),
        (
            Command::Lock {
                target: Some("north".to_owned()),
            },
            "lock",
        ),
        (
            Command::Pick {
                target: Some("north".to_owned()),
            },
            "pick",
        ),
    ];
    for (command, name) in cases {
        let (mut app, a, alice) = door_world();
        // A door that is closed, locked and pickable, with the key in hand,
        // so the engine would carry out any of the five that makes sense; the
        // state each needs is set up below.
        let key_item = ItemId::from("tba_item_8");
        {
            let world = app.world_mut();
            let mut doors = world.get_mut::<Doors>(a).expect("doors");
            let door = doors.by_dir.get_mut(&ExitKey::North).expect("door");
            door.key = Some(key_item);
            door.pickable = true;
            door.default_closed = name != "close";
            door.default_locked = matches!(name, "unlock" | "pick");
        }
        item_carried(&mut app, alice, 8, "a key");
        define_arg(&mut app, 51, "wld", "r", 100, "*", &door_script(0));
        attach(&mut app, "room:tba.1", &[51]);
        send(&mut app, 1, alice, typed(command));
        let room_id = uid(&mut app, a);
        assert_eq!(
            global(&mut app, room_id, "cmd").as_deref(),
            Some(name),
            "{name}"
        );
        assert!(is_handled(&app, 1), "{name}");
    }
}

#[test]
fn a_mob_door_trigger_is_asked_before_the_room() {
    let (mut app, a, alice) = door_world();
    define_arg(&mut app, 50, "mob", "r", 100, "*", &door_script(0));
    define_arg(&mut app, 51, "wld", "r", 100, "*", &ran_then(1));
    attach(&mut app, "npc:tba_npc_5", &[50]);
    attach(&mut app, "room:tba.1", &[51]);
    let guard = npc(&mut app, a, 5, "the guard");
    if !mob_is_awake(&mut app, guard) {
        return;
    }
    send(&mut app, 1, alice, open_north());
    let guard_id = uid(&mut app, guard);
    assert_eq!(global(&mut app, guard_id, "cmd").as_deref(), Some("open"));
    assert!(
        !ran(&mut app, a),
        "the mob refused before the room was asked"
    );
    assert!(is_handled(&app, 1));
}

#[test]
fn a_door_command_the_engine_would_refuse_runs_no_door_trigger() {
    let (mut app, a, alice) = door_world();
    define_arg(&mut app, 51, "wld", "r", 100, "*", &ran_then(0));
    attach(&mut app, "room:tba.1", &[51]);
    // The door is already shut: there is nothing to close (`do_doorcmd` is
    // not reached), and a door that is not there is not one either.
    send(
        &mut app,
        1,
        alice,
        typed(Command::Close {
            target: Some("north".to_owned()),
        }),
    );
    send(
        &mut app,
        2,
        alice,
        typed(Command::Open {
            target: Some("south".to_owned()),
        }),
    );
    assert!(!ran(&mut app, a));
    assert!(!claimed(&app, 1) && !claimed(&app, 2));
}

// -- login -------------------------------------------------------------------

#[test]
fn a_player_entering_the_game_runs_the_room_login_trigger_once() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(
        &mut app,
        60,
        "wld",
        "s",
        100,
        "*",
        "set actor %actor%\nglobal actor\nset n %n%\nglobal n\nset ran 1\nglobal ran",
    );
    attach(&mut app, "room:tba.1", &[60]);
    let alice = player(&mut app, a, "Alice", 7);
    app.update();
    let (room_id, alice_id) = (uid(&mut app, a), uid(&mut app, alice));
    assert_eq!(global(&mut app, room_id, "actor"), Some(uid_text(alice_id)));
    assert!(app.world().get::<DgLoggedIn>(alice).is_some());

    // Once only: a second frame does not run it again.
    run(&mut app, |host| {
        if let Some(script) = host.script_mut(room_id) {
            script.global_vars.clear();
        }
    });
    app.update();
    assert_eq!(global(&mut app, room_id, "ran"), None);
}

#[test]
fn login_runs_the_room_trigger_before_the_greeters() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    // The room's login trigger leaves a mark on the player; the greeter reads
    // it, so it has to run second. A greeting at login has no direction (`none`).
    define_arg(
        &mut app,
        60,
        "wld",
        "s",
        100,
        "*",
        "set order room\nremote order %actor.id%",
    );
    define_arg(
        &mut app,
        61,
        "mob",
        "g",
        100,
        "*",
        "set seen %actor.order%\nglobal seen\nset direction %direction%\nglobal direction\n\
         set ran 1\nglobal ran",
    );
    attach(&mut app, "room:tba.1", &[60]);
    attach(&mut app, "npc:tba_npc_6", &[61]);
    let greeter = npc(&mut app, a, 6, "the greeter");
    player(&mut app, a, "Alice", 7);
    if !mob_is_awake(&mut app, greeter) {
        return;
    }
    app.update();
    let greeter_id = uid(&mut app, greeter);
    assert!(ran(&mut app, greeter));
    assert_eq!(
        global(&mut app, greeter_id, "seen").as_deref(),
        Some("room")
    );
    assert_eq!(
        global(&mut app, greeter_id, "direction").as_deref(),
        Some("none")
    );
}

// -- regressions ---------------------------------------------------------------

/// The text a command shows, leaving out the prompts.
fn shown(app: &mut App) -> Vec<String> {
    output(app)
        .into_iter()
        .filter(|text| text.trim() != ">" && !text.trim().is_empty())
        .collect()
}

/// A counter the trigger bumps each time it runs, then returns `ret`.
fn counting(ret: i32) -> String {
    format!(
        "if %count%\n  eval count %count% + 1\nelse\n  set count 1\nend\nglobal count\nreturn {ret}"
    )
}

fn count_of(app: &mut App, owner: Entity) -> i32 {
    let id = uid(app, owner);
    global(app, id, "count")
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// Record the player in the engine's online table, as the engine does.
fn put_online(app: &mut App, who: Entity, room: Entity) {
    app.init_resource::<OnlinePlayers>();
    app.world_mut().resource_mut::<OnlinePlayers>().map.insert(
        1,
        OnlinePlayer {
            entity: who,
            name: "Alice".to_owned(),
            room,
            account_id: 7,
        },
    );
}

fn online_room(app: &App) -> Entity {
    app.world()
        .resource::<OnlinePlayers>()
        .map
        .values()
        .next()
        .expect("record")
        .room
}

/// Marks the mob a test wants extracted the moment its place is stamped.
#[derive(Component)]
struct DieOnStamp;

#[test]
fn a_mover_a_trigger_extracted_is_left_alone() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 47, "wld", "g", 100, "*", &ran_then(1));
    attach(&mut app, "room:tba.2", &[47]);
    app.world_mut().add_observer(
        |trigger: On<Insert<mib_tba::dg::ids::DgOrder>>,
         marked: Query<(), With<DieOnStamp>>,
         mut commands: Commands| {
            if marked.contains(trigger.event_target()) {
                commands
                    .entity(trigger.event_target())
                    .insert(mib_tba::dg::script::DgDead);
            }
        },
    );
    let walker = npc(&mut app, a, 5, "the walker");
    tick_pulses(&mut app, 1);
    app.world_mut().entity_mut(walker).insert(DieOnStamp);
    app.world_mut()
        .get_mut::<Location>(walker)
        .expect("location")
        .room = b;
    tick_pulses(&mut app, 2);
    assert!(
        app.world().get_entity(walker).is_err(),
        "the extracted walker was swept"
    );
}

#[test]
fn a_relocation_that_follows_no_exit_runs_no_leave_trigger_and_is_not_undone() {
    let mut app = intake_app();
    let (a, _b) = two_rooms(&mut app);
    let c = room(&mut app, 3);
    define_arg(&mut app, 40, "wld", "q", 100, "*", &counting(0));
    attach(&mut app, "room:tba.1", &[40]);
    let alice = seated_player(&mut app, a);
    tick_pulses(&mut app, 1);
    // The death respawn: the engine puts the player in the start room.
    app.world_mut()
        .entity_mut(alice)
        .insert(Location { room: c });
    tick_pulses(&mut app, 2);
    assert_eq!(room_of(&app, alice), c);
    assert_eq!(count_of(&mut app, a), 0, "no leave trigger for a transport");
    assert_eq!(
        app.world().get::<DgLastRoom>(alice).map(|last| last.0),
        Some(c)
    );
}

#[test]
fn a_transported_mob_greets_with_no_direction_and_is_not_sent_back() {
    let mut app = intake_app();
    let (a, _b) = two_rooms(&mut app);
    let c = room(&mut app, 3);
    define_arg(
        &mut app,
        44,
        "mob",
        "g",
        100,
        "*",
        &format!("{DIRECTION}\nreturn 0"),
    );
    attach(&mut app, "npc:tba_npc_6", &[44]);
    let greeter = npc(&mut app, c, 6, "the greeter");
    let alice = seated_player(&mut app, a);
    if !mob_is_awake(&mut app, greeter) {
        return;
    }
    tick_pulses(&mut app, 1);
    app.world_mut()
        .entity_mut(alice)
        .insert(Location { room: c });
    tick_pulses(&mut app, 2);
    assert_eq!(room_of(&app, alice), c, "the result is ignored");
    let greeter_id = uid(&mut app, greeter);
    assert_eq!(
        global(&mut app, greeter_id, "direction").as_deref(),
        Some("none")
    );
}

#[test]
fn a_walking_npc_a_trigger_refused_is_not_offered_the_move_every_tick() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 40, "wld", "q", 100, "*", &counting(0));
    attach(&mut app, "room:tba.1", &[40]);
    let walker = npc(&mut app, a, 7, "the walker");
    tick_pulses(&mut app, 1);
    for _ in 0..20 {
        app.world_mut()
            .entity_mut(walker)
            .insert(Location { room: b });
        tick_pulses(&mut app, 1);
        assert_eq!(room_of(&app, walker), a);
    }
    assert_eq!(count_of(&mut app, a), 1, "the leave trigger ran once");

    // Once the cooldown is over the move is offered again.
    for _ in 0..100 {
        tick_pulses(&mut app, 1);
    }
    app.world_mut()
        .entity_mut(walker)
        .insert(Location { room: b });
    tick_pulses(&mut app, 1);
    assert_eq!(count_of(&mut app, a), 2);
}

#[test]
fn a_refused_leave_of_a_player_the_game_moved_restores_the_online_record() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 40, "wld", "q", 100, "*", &ran_then(0));
    attach(&mut app, "room:tba.1", &[40]);
    let alice = seated_player(&mut app, a);
    put_online(&mut app, alice, a);
    tick_pulses(&mut app, 1);
    // A flee: the pack inserts the room and records it as the player's.
    app.world_mut()
        .entity_mut(alice)
        .insert(Location { room: b });
    app.world_mut()
        .resource_mut::<OnlinePlayers>()
        .map
        .values_mut()
        .for_each(|record| record.room = b);
    tick_pulses(&mut app, 1);
    assert!(ran(&mut app, a));
    assert_eq!(room_of(&app, alice), a);
    assert_eq!(online_room(&app), a);
}

#[test]
fn an_enter_refusal_restores_the_online_record() {
    let mut app = intake_app();
    let (a, _b) = two_rooms(&mut app);
    define_arg(&mut app, 43, "wld", "g", 100, "*", &ran_then(0));
    attach(&mut app, "room:tba.2", &[43]);
    let alice = seated_player(&mut app, a);
    put_online(&mut app, alice, a);
    send(&mut app, 1, alice, go_north());
    assert_eq!(room_of(&app, alice), a);
    assert_eq!(online_room(&app), a);
}

#[test]
fn a_player_who_flees_along_an_exit_gets_leave_and_arrival_triggers() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(
        &mut app,
        40,
        "wld",
        "q",
        100,
        "*",
        &format!("{DIRECTION}\nreturn 1"),
    );
    define_arg(&mut app, 47, "wld", "g", 100, "*", DIRECTION);
    attach(&mut app, "room:tba.1", &[40]);
    attach(&mut app, "room:tba.2", &[47]);
    let alice = seated_player(&mut app, a);
    tick_pulses(&mut app, 1);
    app.world_mut()
        .entity_mut(alice)
        .insert(Location { room: b });
    tick_pulses(&mut app, 1);
    let (a_id, b_id) = (uid(&mut app, a), uid(&mut app, b));
    assert_eq!(
        global(&mut app, a_id, "direction").as_deref(),
        Some("north")
    );
    assert_eq!(
        global(&mut app, b_id, "direction").as_deref(),
        Some("south")
    );
    assert_eq!(room_of(&app, alice), b);
}

#[test]
fn a_walker_is_stamped_as_the_newest_arrival() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    let resident = npc(&mut app, b, 5, "the resident");
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, go_north());
    let (alice_id, resident_id) = (uid(&mut app, alice), uid(&mut app, resident));
    let room_id = uid(&mut app, b);
    let people = run(&mut app, |host| host.room_people(room_id));
    assert_eq!(people.first(), Some(&alice_id), "{people:?} {resident_id}");
}

#[test]
fn no_leave_trigger_runs_for_a_closed_door_or_a_hidden_exit() {
    let (mut app, a, alice) = door_world();
    define_arg(&mut app, 40, "wld", "q", 100, "*", &counting(1));
    attach(&mut app, "room:tba.1", &[40]);
    send(&mut app, 1, alice, go_north());
    assert_eq!(count_of(&mut app, a), 0, "the door is closed");

    app.world_mut()
        .get_mut::<RoomState>(a)
        .expect("state")
        .doors
        .clear();
    app.world_mut().entity_mut(a).remove::<Doors>();
    app.world_mut()
        .get_mut::<Exits>(a)
        .expect("exits")
        .hidden
        .insert(ExitKey::North);
    send(&mut app, 2, alice, go_north());
    assert_eq!(count_of(&mut app, a), 0, "nobody found the exit");
}

/// Room 1 with a locked, pickable door to the north and a player in it.
fn locked_door_world() -> (App, Entity, Entity) {
    let (mut app, a, alice) = door_world();
    let mut doors = Doors::default();
    doors.by_dir.insert(
        ExitKey::North,
        Door {
            key: None,
            name: Some("gate".to_owned()),
            pickable: true,
            difficulty: 0,
            default_closed: true,
            default_locked: true,
        },
    );
    app.world_mut().entity_mut(a).insert(doors);
    (app, a, alice)
}

/// Make the player a thief who knows `pick_lock` and stands ready.
fn make_thief(app: &mut App, who: Entity) {
    let mut content = app.world_mut().resource_mut::<TbaContent>();
    content.classes.insert(
        "thief".into(),
        mib_tba::content::TbaClassDef {
            name: "thief".to_owned(),
            description: String::new(),
            hp_dice: Dice::new(1, 10, 0),
            mana_dice: None,
            xp_table: Vec::new(),
            thaco_table: Vec::new(),
            prime_attribute: None,
            starting_items: Vec::new(),
            granted_skills: vec!["pick_lock".into()],
        },
    );
    app.world_mut().entity_mut(who).insert((
        TbaProgress::new("thief"),
        TbaCombatant {
            hp: 10,
            max_hp: 10,
            mana: 0,
            max_mana: 0,
            move_points: 80,
            max_move_points: 80,
            hitroll: 0,
            damroll: 0,
            armor_class: 100,
            thaco: 15,
            attributes: TbaAttributes::neutral(),
            saves: TbaSaves::default(),
            position: Position::Standing,
            target: None,
        },
    ));
}

/// The pick door trigger's mark in room 1 after `input`, with the player set up by `setup`.
fn pick_cmd_seen(input: &str, setup: fn(&mut App, Entity)) -> (Option<String>, bool) {
    let (mut app, a, alice) = locked_door_world();
    setup(&mut app, alice);
    define_arg(&mut app, 51, "wld", "r", 100, "*", &door_script(0));
    attach(&mut app, "room:tba.1", &[51]);
    send(&mut app, 1, alice, line(input));
    let room_id = uid(&mut app, a);
    (global(&mut app, room_id, "cmd"), claimed(&app, 1))
}

#[test]
fn the_packs_pick_lock_verb_is_offered_to_the_door_triggers_as_pick() {
    let (cmd, taken) = pick_cmd_seen("pick_lock north", make_thief);
    assert_eq!(cmd.as_deref(), Some("pick"));
    assert!(taken);
}

#[test]
fn pick_lock_attempts_the_handler_answers_before_the_roll_run_no_door_trigger() {
    // `ok_pick` (and so `do_doorcmd` and its triggers) is reached only by a
    // character who knows the skill, stands, and names a direction.
    let (cmd, taken) = pick_cmd_seen("pick_lock north", |_, _| {});
    assert_eq!((cmd, taken), (None, false), "a warrior knows no pick_lock");
    let (cmd, taken) = pick_cmd_seen("pick_lock gate", make_thief);
    assert_eq!((cmd, taken), (None, false), "a door name is no direction");
    let (cmd, taken) = pick_cmd_seen("pick_lock north", |app, who| {
        make_thief(app, who);
        app.world_mut()
            .get_mut::<TbaCombatant>(who)
            .expect("combatant")
            .position = Position::Resting;
    });
    assert_eq!((cmd, taken), (None, false), "pick needs POS_STANDING");
}

#[test]
fn picking_a_lock_that_is_not_locked_runs_no_door_trigger() {
    let (mut app, a, alice) = door_world();
    define_arg(&mut app, 51, "wld", "r", 100, "*", &door_script(0));
    attach(&mut app, "room:tba.1", &[51]);
    send(
        &mut app,
        1,
        alice,
        typed(Command::Pick {
            target: Some("north".to_owned()),
        }),
    );
    make_thief(&mut app, alice);
    send(&mut app, 2, alice, line("pick_lock north"));
    assert!(!ran(&mut app, a));
}

#[test]
fn a_bare_say_still_reaches_the_speech_triggers() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 20, "wld", "d", 0, "*", SPEECH);
    attach(&mut app, "room:tba.1", &[20]);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, line("say"));
    assert!(ran(&mut app, a));
    let room_id = uid(&mut app, a);
    assert_eq!(global(&mut app, room_id, "speech").as_deref(), Some(""));
}

#[test]
fn act_check_is_on_for_a_social_walk_and_off_after_a_say() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    let alice = seated_player(&mut app, a);
    app.world_mut()
        .resource_mut::<TbaContent>()
        .socials
        .insert("wave".into(), wave());
    npc(&mut app, a, 5, "the guard");
    send(&mut app, 1, alice, line("social wave"));
    assert!(run(&mut app, |host| host.vm_state().act_check));
    send(&mut app, 2, alice, line("say hi"));
    assert!(!run(&mut app, |host| host.vm_state().act_check));
}

// -- memory triggers ---------------------------------------------------------

/// `memory` trigger letter for a mob.
const MEMORY: &str = "o";

fn remember(app: &mut App, mob: Entity, who: Entity) {
    let (mob_id, who_id) = (uid(app, mob), uid(app, who));
    run(app, |host| {
        host.set_script_mem(
            mob_id,
            vec![ScriptMemory {
                id: who_id,
                cmd: None,
            }],
        );
    });
}

#[test]
fn a_mob_that_remembers_a_player_runs_its_memory_trigger_when_they_arrive() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 70, "mob", MEMORY, 100, "*", &ran_then(1));
    attach(&mut app, "npc:tba_npc_6", &[70]);
    let watcher = npc(&mut app, b, 6, "the watcher");
    let alice = seated_player(&mut app, a);
    if !mob_is_awake(&mut app, watcher) {
        return;
    }
    remember(&mut app, watcher, alice);
    send(&mut app, 1, alice, go_north());
    assert!(ran(&mut app, watcher), "greet_memory_mtrigger ran");
    let watcher_id = uid(&mut app, watcher);
    assert!(run(&mut app, |host| host.script_mem(watcher_id)).is_empty());
}

#[test]
fn a_mob_that_arrives_where_it_remembers_someone_runs_its_memory_trigger() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(&mut app, 70, "mob", MEMORY, 100, "*", &ran_then(1));
    attach(&mut app, "npc:tba_npc_6", &[70]);
    let walker = npc(&mut app, a, 6, "the walker");
    let alice = seated_player(&mut app, b);
    if !mob_is_awake(&mut app, walker) {
        return;
    }
    remember(&mut app, walker, alice);
    tick_pulses(&mut app, 1);
    app.world_mut()
        .entity_mut(walker)
        .insert(Location { room: b });
    tick_pulses(&mut app, 1);
    assert!(ran(&mut app, walker), "entry_memory_mtrigger ran");
}

// -- pack handlers -------------------------------------------------------------

/// A pack handler answers `input`, and stays silent once a `command` trigger
/// has taken the line over.
macro_rules! assert_skips_claimed {
    ($handler:path, $input:expr, $setup:expr) => {
        for taken in [false, true] {
            let mut app = intake_app();
            let setup: fn(&mut App) = $setup;
            setup(&mut app);
            app.add_systems(Update, $handler.after(DgIntake::CommandGate));
            let a = room(&mut app, 1);
            if taken {
                define_arg(&mut app, 90, "wld", "c", 0, "*", &ran_then(1));
                attach(&mut app, "room:tba.1", &[90]);
            }
            let alice = seated_player(&mut app, a);
            send(&mut app, 1, alice, line($input));
            let text = shown(&mut app);
            if taken {
                assert!(claimed(&app, 1));
                assert!(
                    text.is_empty(),
                    "`{}` was answered anyway: {text:?}",
                    $input
                );
            } else {
                assert!(!text.is_empty(), "`{}` is answered when free", $input);
            }
        }
    };
}

#[test]
fn the_time_handler_skips_a_taken_over_command() {
    assert_skips_claimed!(mib_tba::clock::handle_time_command, "time", |_| {});
}

#[test]
fn the_skill_handler_skips_a_taken_over_command() {
    assert_skips_claimed!(mib_tba::skills::handle_skill_commands, "hide", |app| {
        app.init_resource::<VisibilityPolicy>();
        app.init_resource::<RoomOccupants>();
        app.add_message::<PlayerDefeated>();
        app.add_message::<NpcDefeated>();
        app.add_message::<TbaDamage>();
    });
}

#[test]
fn the_social_handler_skips_a_taken_over_command() {
    assert_skips_claimed!(mib_tba::socials::handle_social_commands, "socials", |_| {});
}

#[test]
fn the_party_handler_skips_a_taken_over_command() {
    assert_skips_claimed!(mib_tba::party::handle_party_commands, "group", |app| {
        app.init_resource::<mib_tba::party::TbaParties>();
    });
}

#[test]
fn the_shop_handler_skips_a_taken_over_command() {
    assert_skips_claimed!(mib_tba::shops::handle_shop_commands, "list", |app| {
        app.init_resource::<VisibilityPolicy>();
        app.init_resource::<RoomOccupants>();
    });
}

#[test]
fn the_combat_handler_skips_a_taken_over_command() {
    assert_skips_claimed!(mib_tba::combat::handle_combat_commands, "flee", |app| {
        app.init_resource::<VisibilityPolicy>();
        app.init_resource::<RoomOccupants>();
    });
}

// -- review fixes ------------------------------------------------------------

/// A player the game moved along an exit is told where they are held.
#[test]
fn a_player_held_back_by_a_leave_trigger_is_shown_the_room() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(
        &mut app,
        40,
        "wld",
        "q",
        100,
        "*",
        &with_return(DIRECTION, 0),
    );
    attach(&mut app, "room:tba.1", &[40]);
    let alice = seated_player(&mut app, a);
    tick_pulses(&mut app, 1);
    let _ = shown(&mut app);
    app.world_mut()
        .get_mut::<Location>(alice)
        .expect("location")
        .room = b;
    tick_pulses(&mut app, 1);
    assert_eq!(room_of(&app, alice), a);
    assert!(!shown(&mut app).is_empty(), "the player is told nothing");
}

/// A character that never left keeps its place among the room's people.
#[test]
fn a_refused_leave_keeps_the_walkers_place_in_the_room() {
    let mut app = intake_app();
    let (a, b) = two_rooms(&mut app);
    define_arg(
        &mut app,
        40,
        "wld",
        "q",
        100,
        "*",
        &with_return(DIRECTION, 0),
    );
    attach(&mut app, "room:tba.1", &[40]);
    let walker = npc(&mut app, a, 7, "the walker");
    let resident = npc(&mut app, a, 8, "the resident");
    tick_pulses(&mut app, 1);
    let room_id = uid(&mut app, a);
    let before = run(&mut app, |host| host.room_people(room_id));
    let stamp = app
        .world()
        .get::<mib_tba::dg::ids::DgOrder>(walker)
        .copied();
    app.world_mut()
        .get_mut::<Location>(walker)
        .expect("location")
        .room = b;
    tick_pulses(&mut app, 3);
    assert_eq!(room_of(&app, walker), a);
    let after = run(&mut app, |host| host.room_people(room_id));
    assert_eq!(before, after, "{resident:?}");
    assert_eq!(
        app.world()
            .get::<mib_tba::dg::ids::DgOrder>(walker)
            .copied(),
        stamp,
        "a character that did not leave is not restamped"
    );
}

/// Only a routed say is spoken: the router drops a flooded line unmarked.
#[test]
fn a_say_the_router_dropped_reaches_no_speech_trigger() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 20, "wld", "d", 100, "*", &ran_then(1));
    attach(&mut app, "room:tba.1", &[20]);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, say("flood password"));
    assert!(!ran(&mut app, a), "a dropped line was spoken");
    send(&mut app, 2, alice, say("password"));
    assert!(ran(&mut app, a));
}

/// A refusal marked by any pre-commit system is a claim the pack handlers see.
#[test]
fn a_command_another_pre_commit_system_handled_is_claimed() {
    let mut app = intake_app();
    app.add_systems(
        Update,
        (|mut reader: MessageReader<CommandReceived>, mut handled: ResMut<HandledCommands>| {
            for msg in reader.read() {
                handled.mark_handled(msg.id);
            }
        })
        .in_set(DgIntake::PreCommit),
    );
    let a = room(&mut app, 1);
    let alice = seated_player(&mut app, a);
    send(&mut app, 1, alice, line("look"));
    assert!(claimed(&app, 1));
}

/// A command handled before the gate is not offered to `command` triggers.
#[test]
fn the_gate_skips_a_command_already_handled() {
    let (mut app, alice, [a, ..]) = gate_world(&ran_then(0), &ran_then(0), &ran_then(0));
    app.world_mut()
        .resource_mut::<HandledCommands>()
        .mark_handled(CommandId(1));
    send(&mut app, 1, alice, line("look"));
    assert!(!ran(&mut app, a));
}

/// A player still restoring their bundle has not entered the game.
#[test]
fn a_player_restoring_their_bundle_has_not_logged_in() {
    let mut app = intake_app();
    let a = room(&mut app, 1);
    define_arg(&mut app, 60, "wld", "s", 100, "*", &ran_then(1));
    attach(&mut app, "room:tba.1", &[60]);
    let alice = player(&mut app, a, "Alice", 7);
    app.world_mut()
        .entity_mut(alice)
        .insert(PendingBundleRestore);
    app.update();
    assert!(!ran(&mut app, a));
    assert!(app.world().get::<DgLoggedIn>(alice).is_none());
}
