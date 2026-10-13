//! The command interpreter a mob script sees: `BaseHost::mud_command` for an
//! NPC actor (the NPC side of each command, ported from tbaMUD's `act.*.c`),
//! for a forced player (injected into the engine's pipeline), and
//! `mud_command_name` (`cmd_info[]` order).
//!
//! Expectations come from the C source (`interpreter.c`, `act.comm.c`,
//! `act.item.c`, `act.movement.c`, `act.offensive.c`, `act.social.c`,
//! `dg_triggers.c`), never from what the code happens to return. Triggers
//! prove they ran by setting a global.
//!
//! Tests may unwrap/expect: a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dg_common;

use dg_common::*;
use dg_vm::{BaseHost, Uid};
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::dg::script::DgProtos;
use mib_tba::prelude::*;
use mib_tba::triggers::content::TriggerDef;
use smol_str::SmolStr;

// -- fixtures ----------------------------------------------------------------

/// Run one command line as `who`.
fn cmd(app: &mut App, who: Uid, line: &str) {
    run(app, |host| host.mud_command(who, line));
}

/// `mudcommand` over the host, which needs no world.
fn name_of(prefix: &str) -> Option<String> {
    let mut app = app();
    run(&mut app, |host| host.mud_command_name(prefix))
}

/// Define a trigger whose argument list is `arglist`.
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
    assert!(app.world_mut().resource_mut::<DgProtos>().add(&def));
}

/// Connect `from` to `to` through `dir`.
fn connect(app: &mut App, from: Entity, dir: ExitKey, to: Entity) {
    let world = app.world_mut();
    if world.get::<Exits>(from).is_none() {
        world.entity_mut(from).insert(Exits::default());
    }
    world.get_mut::<Exits>(from).unwrap().map.insert(dir, to);
}

/// A two-way link.
fn link(app: &mut App, a: Entity, dir: ExitKey, b: Entity, back: ExitKey) {
    connect(app, a, dir, b);
    connect(app, b, back, a);
}

/// Put a door on one side of an exit, with the runtime state it starts in.
fn door(
    app: &mut App,
    room: Entity,
    dir: ExitKey,
    name: Option<&str>,
    key: Option<u32>,
    closed: bool,
    locked: bool,
) {
    let world = app.world_mut();
    if world.get::<Doors>(room).is_none() {
        world.entity_mut(room).insert(Doors::default());
    }
    world.get_mut::<Doors>(room).unwrap().by_dir.insert(
        dir,
        Door {
            key: key.map(|vnum| ItemId::from(format!("tba_item_{vnum}"))),
            name: name.map(str::to_owned),
            pickable: true,
            difficulty: 0,
            default_closed: closed,
            default_locked: locked,
        },
    );
    world.get_mut::<RoomState>(room).unwrap().doors.insert(
        dir,
        DoorState {
            closed,
            locked,
            to: None,
        },
    );
}

fn door_state(app: &App, room: Entity, dir: ExitKey) -> (bool, bool) {
    let state = &app.world().get::<RoomState>(room).unwrap().doors[&dir];
    (state.closed, state.locked)
}

fn room_of(app: &App, who: Entity) -> Entity {
    app.world().get::<Location>(who).unwrap().room
}

fn set_position(app: &mut App, who: Entity, position: Position) {
    app.world_mut().entity_mut(who).insert(TbaCombatant {
        hp: 10,
        max_hp: 10,
        mana: 0,
        max_mana: 0,
        move_points: 100,
        max_move_points: 100,
        hitroll: 0,
        damroll: 0,
        armor_class: 0,
        thaco: 20,
        attributes: TbaAttributes::default(),
        saves: TbaSaves::default(),
        position,
        target: None,
    });
}

fn position_of(app: &App, who: Entity) -> Position {
    app.world()
        .get::<TbaCombatant>(who)
        .map_or(Position::Standing, |c| c.position)
}

fn target_of(app: &App, who: Entity) -> Option<Entity> {
    app.world().get::<TbaCombatant>(who).and_then(|c| c.target)
}

/// A container item lying in the room.
fn container(app: &mut App, room: Entity, vnum: u32, name: &str) -> Entity {
    define_item(app, vnum, TbaItemKind::Container, &[], &[name]);
    let chest = item_on_ground(app, room, vnum, name);
    app.world_mut().entity_mut(chest).insert(Container {
        capacity: 10,
        contents: Vec::new(),
    });
    chest
}

fn put_inside(app: &mut App, chest: Entity, vnum: u32, name: &str) -> Entity {
    let item = item_inside(app, chest, vnum, name);
    app.world_mut()
        .get_mut::<Container>(chest)
        .unwrap()
        .contents
        .push(item);
    item
}

fn social(
    name: &str,
    char_no_arg: Option<&str>,
    others_no_arg: Option<&str>,
    char_found: Option<&str>,
    others_found: Option<&str>,
    vict_found: Option<&str>,
) -> (SmolStr, SocialDef) {
    (
        SmolStr::new(name),
        SocialDef {
            sort_as: SmolStr::new(name),
            hide: false,
            min_char_position: 5,
            min_victim_position: 0,
            min_level_char: 0,
            char_no_arg: char_no_arg.map(str::to_owned),
            others_no_arg: others_no_arg.map(str::to_owned),
            char_found: char_found.map(str::to_owned),
            others_found: others_found.map(str::to_owned),
            vict_found: vict_found.map(str::to_owned),
            not_found: Some("Nobody like that here.".to_owned()),
            char_auto: Some("You hug yourself.".to_owned()),
            others_auto: Some("$n hugs $mself.".to_owned()),
            char_body_found: None,
            others_body_found: None,
            vict_body_found: None,
            char_obj_found: None,
            others_obj_found: None,
        },
    )
}

fn install_socials(app: &mut App, socials: Vec<(SmolStr, SocialDef)>) {
    app.world_mut().resource_mut::<TbaContent>().socials = socials.into_iter().collect();
}

/// A room, a player and a guard NPC in it.
struct Scene {
    app: App,
    room: Entity,
    alice: Entity,
    guard: Entity,
    alice_id: Uid,
    guard_id: Uid,
}

/// A new test app.
fn fresh() -> App {
    app()
}

fn scene() -> Scene {
    scene_in(fresh())
}

/// [`scene`] in an app the test has already put trigger attachments in.
fn scene_in(mut app: App) -> Scene {
    // Mobs are level 1 and up in the world files; a level-0 mob would not
    // match the level-1 commands (`:`, `holler`, ...).
    for vnum in 1..=9 {
        define_npc_level(&mut app, vnum, 10);
    }
    let room = room(&mut app, 100);
    let alice = player(&mut app, room, "Alice", 7);
    let guard = npc(&mut app, room, 1, "Guard");
    let alice_id = uid(&mut app, alice);
    let guard_id = uid(&mut app, guard);
    Scene {
        app,
        room,
        alice,
        guard,
        alice_id,
        guard_id,
    }
}

fn texts(app: &mut App) -> Vec<String> {
    output(app)
}

/// Whether the character facts the dispatch functions read (`CharStat::Pos`
/// for "awake") are answered by the host. Triggers whose C tests `AWAKE` can
/// only be exercised once they are.
fn chars_modelled() -> bool {
    let mut app = fresh();
    let room = room(&mut app, 1);
    let mob = npc(&mut app, room, 1, "Mob");
    let id = uid(&mut app, mob);
    run(&mut app, |host| {
        dg_vm::fields_char::CharHost::char_stat(host, id, dg_vm::fields_char::CharStat::Pos) > 4
    })
}

// -- mudcommand: cmd_info[] order -------------------------------------------

#[test]
fn mudcommand_matches_the_first_cmd_info_entry_the_prefix_begins() {
    assert_eq!(name_of("n").as_deref(), Some("north"));
    assert_eq!(name_of("no").as_deref(), Some("north"));
    assert_eq!(name_of("s").as_deref(), Some("south"));
    assert_eq!(name_of("e").as_deref(), Some("east"));
    assert_eq!(name_of("w").as_deref(), Some("west"));
    assert_eq!(name_of("u").as_deref(), Some("up"));
    assert_eq!(name_of("d").as_deref(), Some("down"));
    assert_eq!(name_of("nw").as_deref(), Some("nw"));
    assert_eq!(name_of("northe").as_deref(), Some("northeast"));
    assert_eq!(name_of("sa").as_deref(), Some("sacrifice"));
    assert_eq!(name_of("say").as_deref(), Some("say"));
    assert_eq!(name_of("sh").as_deref(), Some("shout"));
    assert_eq!(name_of("show").as_deref(), Some("show"));
    assert_eq!(name_of("t").as_deref(), Some("tell"));
    assert_eq!(name_of("ta").as_deref(), Some("take"));
    assert_eq!(name_of("g").as_deref(), Some("get"));
    assert_eq!(name_of("gi").as_deref(), Some("give"));
    assert_eq!(name_of("l").as_deref(), Some("look"));
    assert_eq!(name_of("k").as_deref(), Some("kill"));
    assert_eq!(name_of("kic").as_deref(), Some("kick"));
    assert_eq!(name_of("o").as_deref(), Some("open"));
    assert_eq!(name_of("em").as_deref(), Some("emote"));
    assert_eq!(name_of("'").as_deref(), Some("'"));
    assert_eq!(name_of(":").as_deref(), Some(":"));
}

#[test]
fn mudcommand_is_case_sensitive_and_knows_no_socials() {
    assert_eq!(name_of("North"), None, "strncmp is case-sensitive");
    assert_eq!(name_of("smile"), None, "socials are not in cmd_info[]");
    assert_eq!(name_of("zzzz"), None);
    assert_eq!(name_of("northeastt"), None, "longer than any name");
    assert_eq!(
        name_of("").as_deref(),
        Some("RESERVED"),
        "an empty prefix matches the first entry"
    );
}

// -- hostile input -----------------------------------------------------------

#[test]
fn hostile_lines_do_nothing_and_do_not_panic() {
    let mut s = scene();
    let g = s.guard_id;
    for line in [
        "",
        "   ",
        "\t",
        "xyzzy",
        "xyzzy with args",
        "say",
        "say   ",
        "'",
        ":",
        "get",
        "get nothing",
        "give",
        "give sword",
        "give 99999999999999999999 coins alice",
        "give -5 coins alice",
        "drop 99999999999999999999 coins",
        "drop all.",
        "put",
        "put a b",
        "wear",
        "wear all.",
        "remove",
        "open",
        "open nothing",
        "lock",
        "kill",
        "kill nobody",
        "assist",
        "assist nobody",
        "follow",
        "follow nobody",
        "tell",
        "tell nobody hi",
        "whisper alice",
        "wake nobody",
        "sit chair",
        "enter",
        "enter nothing",
        "north",
        "\u{1F600}",
        "é",
        "say \u{1F600}",
        "flee",
        "unfollow",
        "smile nobody",
        "%$#@",
    ] {
        cmd(&mut s.app, g, line);
    }
    assert_eq!(room_of(&s.app, s.guard), s.room);
}

#[test]
fn a_gone_actor_does_nothing() {
    let mut s = scene();
    let g = s.guard_id;
    s.app.world_mut().despawn(s.guard);
    cmd(&mut s.app, g, "say hello");
    assert!(texts(&mut s.app).is_empty());
    cmd(&mut s.app, 0xDEAD_BEEF, "say hello");
}

#[test]
fn an_unknown_word_is_huh_for_the_npc_alone_and_logs_nothing() {
    let mut s = scene();
    cmd(&mut s.app, s.guard_id, "frobnicate");
    assert!(texts(&mut s.app).is_empty(), "the room hears nothing");
    assert!(log_lines(&s.app).is_empty());
}

// -- speech --------------------------------------------------------------------

#[test]
fn say_tells_the_room_and_runs_the_speech_triggers_of_the_others() {
    if !chars_modelled() {
        return;
    }
    let mut s = scene();
    let listener = npc(&mut s.app, s.room, 2, "Clerk");
    define_arg(
        &mut s.app,
        10,
        "mob",
        "d",
        1,
        "*",
        "set heard %speech%\nglobal heard",
    );
    attach(&mut s.app, "npc:tba_npc_2", &[10]);
    let listener_id = uid(&mut s.app, listener);

    cmd(&mut s.app, s.guard_id, "say Hello there");
    assert_eq!(texts(&mut s.app), ["Guard says, 'Hello there'"]);
    assert_eq!(
        global(&mut s.app, listener_id, "heard").as_deref(),
        Some("Hello there")
    );
}

#[test]
fn say_does_not_fire_the_speakers_own_speech_trigger() {
    let mut s = scene();
    define_arg(
        &mut s.app,
        11,
        "mob",
        "d",
        1,
        "*",
        "set heard 1\nglobal heard",
    );
    attach(&mut s.app, "npc:tba_npc_1", &[11]);
    let g = uid(&mut s.app, s.guard);
    cmd(&mut s.app, g, "say Hello");
    assert_eq!(global(&mut s.app, g, "heard"), None);
}

#[test]
fn say_runs_the_rooms_speech_trigger() {
    let mut app = fresh();
    define_arg(
        &mut app,
        12,
        "wld",
        "d",
        1,
        "*",
        "set heard %speech%\nglobal heard",
    );
    attach(&mut app, "room:tba.100", &[12]);
    let mut s = scene_in(app);
    let room_id = uid(&mut s.app, s.room);
    cmd(&mut s.app, s.guard_id, "say open sesame");
    assert_eq!(
        global(&mut s.app, room_id, "heard").as_deref(),
        Some("open sesame")
    );
}

#[test]
fn a_sleeping_mob_hears_no_speech() {
    if !chars_modelled() {
        return;
    }
    let mut s = scene();
    let listener = npc(&mut s.app, s.room, 2, "Clerk");
    define_arg(
        &mut s.app,
        10,
        "mob",
        "d",
        1,
        "*",
        "set heard 1\nglobal heard",
    );
    attach(&mut s.app, "npc:tba_npc_2", &[10]);
    let listener_id = uid(&mut s.app, listener);
    set_position(&mut s.app, listener, Position::Sleeping);
    cmd(&mut s.app, s.guard_id, "say hello");
    assert_eq!(global(&mut s.app, listener_id, "heard"), None);
}

#[test]
fn the_apostrophe_is_say_and_a_bare_say_still_fires_speech_triggers() {
    if !chars_modelled() {
        return;
    }
    let mut s = scene();
    cmd(&mut s.app, s.guard_id, "'Good day");
    assert_eq!(texts(&mut s.app), ["Guard says, 'Good day'"]);
    cmd(&mut s.app, s.guard_id, "' Good day");
    assert_eq!(texts(&mut s.app), ["Guard says, 'Good day'"]);

    let listener = npc(&mut s.app, s.room, 2, "Clerk");
    define_arg(
        &mut s.app,
        13,
        "mob",
        "d",
        1,
        "*",
        "set heard 1\nglobal heard",
    );
    attach(&mut s.app, "npc:tba_npc_2", &[13]);
    let listener_id = uid(&mut s.app, listener);
    cmd(&mut s.app, s.guard_id, "say");
    assert!(texts(&mut s.app).is_empty(), "an empty say says nothing");
    assert_eq!(
        global(&mut s.app, listener_id, "heard").as_deref(),
        Some("1")
    );
}

#[test]
fn emote_and_its_colon_alias_show_the_name_and_the_text() {
    let mut s = scene();
    cmd(&mut s.app, s.guard_id, "emote scratches his chin.");
    assert_eq!(texts(&mut s.app), ["Guard scratches his chin."]);
    cmd(&mut s.app, s.guard_id, ":grins");
    assert_eq!(texts(&mut s.app), ["Guard grins"]);
    cmd(&mut s.app, s.guard_id, "em nods.");
    assert_eq!(texts(&mut s.app), ["Guard nods."]);
    cmd(&mut s.app, s.guard_id, "emote");
    assert!(texts(&mut s.app).is_empty());
}

#[test]
fn an_emote_is_offered_to_the_act_triggers_of_the_npcs_that_see_it() {
    let mut s = scene();
    let watcher = npc(&mut s.app, s.room, 2, "Clerk");
    define_arg(
        &mut s.app,
        14,
        "mob",
        "e",
        0,
        "dances",
        "set saw %arg%\nglobal saw",
    );
    attach(&mut s.app, "npc:tba_npc_2", &[14]);
    let watcher_id = uid(&mut s.app, watcher);
    cmd(&mut s.app, s.guard_id, "emote dances wildly");
    assert_eq!(
        global(&mut s.app, watcher_id, "saw").as_deref(),
        Some("Guard dances wildly")
    );
}

#[test]
fn tell_reaches_a_player_anywhere_and_not_an_npc() {
    let mut s = scene();
    let other = room(&mut s.app, 200);
    player(&mut s.app, other, "Bob", 8);
    cmd(&mut s.app, s.guard_id, "tell bob come here");
    assert_eq!(texts(&mut s.app), ["Guard tells you, 'come here'"]);
    cmd(&mut s.app, s.guard_id, "tell alice hi");
    assert_eq!(texts(&mut s.app), ["Guard tells you, 'hi'"]);
    cmd(&mut s.app, s.guard_id, "tell");
    cmd(&mut s.app, s.guard_id, "tell alice");
    cmd(&mut s.app, s.guard_id, "tell nobody hi");
    assert!(texts(&mut s.app).is_empty());
}

#[test]
fn whisper_goes_to_the_target_and_the_room_sees_a_whisper() {
    let mut s = scene();
    player(&mut s.app, s.room, "Bob", 8);
    cmd(&mut s.app, s.guard_id, "whisper bob the key is hidden");
    let lines = texts(&mut s.app);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.contains(&"Guard whispers to you, 'the key is hidden'".to_owned()));
    assert!(lines.contains(&"Guard whispers something to Bob.".to_owned()));
    cmd(&mut s.app, s.guard_id, "ask alice who");
    let lines = texts(&mut s.app);
    assert!(
        lines.contains(&"Guard asks you, 'who'".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Guard asks Alice a question.".to_owned()),
        "{lines:?}"
    );
}

#[test]
fn shout_and_the_other_channels_reach_every_player_awake_in_range() {
    let mut s = scene();
    let far = room(&mut s.app, 900);
    player(&mut s.app, far, "Bob", 8);
    cmd(&mut s.app, s.guard_id, "shout Fire!");
    let lines = texts(&mut s.app);
    assert_eq!(
        lines
            .iter()
            .filter(|l| *l == "Guard shouts, 'Fire!'")
            .count(),
        2,
        "{lines:?}"
    );
    cmd(&mut s.app, s.guard_id, "gossip news");
    let lines = texts(&mut s.app);
    assert_eq!(
        lines
            .iter()
            .filter(|l| *l == "Guard gossips, 'news'")
            .count(),
        2,
        "{lines:?}"
    );
    cmd(&mut s.app, s.guard_id, "auction a sword");
    assert_eq!(texts(&mut s.app).len(), 2);
    cmd(&mut s.app, s.guard_id, "shout");
    assert!(texts(&mut s.app).is_empty());
}

#[test]
fn a_soundproof_room_keeps_shouts_in() {
    let mut s = scene();
    s.app
        .world_mut()
        .get_mut::<Room>(s.room)
        .unwrap()
        .tags
        .insert(RoomTag::Soundproof);
    cmd(&mut s.app, s.guard_id, "shout Fire!");
    assert!(texts(&mut s.app).is_empty());
}

// -- positions gate commands ---------------------------------------------------

#[test]
fn the_minimum_position_column_gates_each_command() {
    let mut s = scene();
    set_position(&mut s.app, s.guard, Position::Sleeping);
    // `say` needs POS_RESTING; `tell` (POS_DEAD) works asleep.
    cmd(&mut s.app, s.guard_id, "say hello");
    assert!(texts(&mut s.app).is_empty(), "no talking in your sleep");
    cmd(&mut s.app, s.guard_id, "tell alice hello");
    assert_eq!(texts(&mut s.app), ["Guard tells you, 'hello'"]);
    set_position(&mut s.app, s.guard, Position::Resting);
    cmd(&mut s.app, s.guard_id, "say hello");
    assert!(!texts(&mut s.app).is_empty());
    // Moving needs POS_STANDING.
    let north = room(&mut s.app, 101);
    link(&mut s.app, s.room, ExitKey::North, north, ExitKey::South);
    cmd(&mut s.app, s.guard_id, "north");
    assert_eq!(room_of(&s.app, s.guard), s.room);
    set_position(&mut s.app, s.guard, Position::Standing);
    cmd(&mut s.app, s.guard_id, "north");
    assert_eq!(room_of(&s.app, s.guard), north);
}

#[test]
fn a_dead_npc_can_do_nothing_that_needs_a_position() {
    let mut s = scene();
    set_position(&mut s.app, s.guard, Position::Dead);
    cmd(&mut s.app, s.guard_id, "say hi");
    cmd(&mut s.app, s.guard_id, "north");
    cmd(&mut s.app, s.guard_id, "sit");
    assert!(texts(&mut s.app).is_empty());
    assert_eq!(position_of(&s.app, s.guard), Position::Dead);
}

#[test]
fn stand_sit_rest_sleep_and_wake_follow_the_c_state_machine() {
    let mut s = scene();
    let g = s.guard_id;
    cmd(&mut s.app, g, "sit");
    assert_eq!(position_of(&s.app, s.guard), Position::Sitting);
    assert_eq!(texts(&mut s.app), ["Guard sits down."]);
    cmd(&mut s.app, g, "sit");
    assert!(
        texts(&mut s.app).is_empty(),
        "sitting already: only the NPC is told"
    );
    cmd(&mut s.app, g, "rest");
    assert_eq!(position_of(&s.app, s.guard), Position::Resting);
    assert_eq!(texts(&mut s.app), ["Guard rests."]);
    cmd(&mut s.app, g, "sleep");
    assert_eq!(position_of(&s.app, s.guard), Position::Sleeping);
    assert_eq!(texts(&mut s.app), ["Guard lies down and falls asleep."]);
    cmd(&mut s.app, g, "stand");
    assert_eq!(
        position_of(&s.app, s.guard),
        Position::Sleeping,
        "wake up first"
    );
    cmd(&mut s.app, g, "wake");
    assert_eq!(position_of(&s.app, s.guard), Position::Sitting);
    assert_eq!(texts(&mut s.app), ["Guard awakens."]);
    cmd(&mut s.app, g, "stand");
    assert_eq!(position_of(&s.app, s.guard), Position::Standing);
    assert_eq!(texts(&mut s.app), ["Guard clambers to their feet."]);
    cmd(&mut s.app, g, "rest");
    assert_eq!(texts(&mut s.app), ["Guard sits down and rests."]);
    cmd(&mut s.app, g, "stand");
    assert_eq!(
        texts(&mut s.app),
        ["Guard stops resting, and clambers on their feet."]
    );
}

#[test]
fn an_npc_that_has_no_combat_state_yet_gets_one_when_it_lies_down() {
    let mut s = scene();
    assert!(s.app.world().get::<TbaCombatant>(s.guard).is_none());
    cmd(&mut s.app, s.guard_id, "sleep");
    texts(&mut s.app);
    cmd(&mut s.app, s.guard_id, "say hi");
    assert!(texts(&mut s.app).is_empty(), "asleep, so it cannot speak");
}

#[test]
fn wake_wakes_another_character_and_refuses_the_awake() {
    let mut s = scene();
    let sleeper = npc(&mut s.app, s.room, 2, "Clerk");
    set_position(&mut s.app, sleeper, Position::Sleeping);
    cmd(&mut s.app, s.guard_id, "wake clerk");
    assert_eq!(position_of(&s.app, sleeper), Position::Sitting);
    assert!(texts(&mut s.app).is_empty());
    cmd(&mut s.app, s.guard_id, "wake clerk");
    assert_eq!(position_of(&s.app, sleeper), Position::Sitting);
}

#[test]
fn sit_on_furniture_needs_furniture() {
    let mut s = scene();
    define_item(&mut s.app, 20, TbaItemKind::Furniture, &[], &["chair"]);
    define_item(&mut s.app, 21, TbaItemKind::Treasure, &[], &["rock"]);
    item_on_ground(&mut s.app, s.room, 20, "a wooden chair");
    item_on_ground(&mut s.app, s.room, 21, "a rock");
    cmd(&mut s.app, s.guard_id, "sit rock");
    assert!(texts(&mut s.app).is_empty());
    assert_ne!(position_of(&s.app, s.guard), Position::Sitting);
    cmd(&mut s.app, s.guard_id, "sit chair");
    assert_eq!(texts(&mut s.app), ["Guard sits down upon a wooden chair."]);
    assert_eq!(position_of(&s.app, s.guard), Position::Sitting);
}

// -- items ---------------------------------------------------------------------

fn carried_by(app: &App, item: Entity) -> Option<Entity> {
    app.world().get::<CarriedBy>(item).map(|c| c.0)
}

fn on_ground_in(app: &App, item: Entity) -> Option<Entity> {
    app.world()
        .get::<GroundItem>(item)
        .and_then(|_| app.world().get::<Location>(item))
        .map(|l| l.room)
}

fn gold_of(app: &App, who: Entity) -> u32 {
    app.world()
        .get::<TbaGold>(who)
        .map(|g| g.0)
        .or_else(|| {
            app.world()
                .get::<mib_tba::dg::mobcmd::TbaNpcGold>(who)
                .map(|g| g.0)
        })
        .unwrap_or(0)
}

#[test]
fn get_and_take_pick_up_from_the_floor_with_the_c_messages() {
    let mut s = scene();
    define_item(
        &mut s.app,
        30,
        TbaItemKind::Treasure,
        &[],
        &["sword", "iron"],
    );
    let sword = item_on_ground(&mut s.app, s.room, 30, "an iron sword");
    cmd(&mut s.app, s.guard_id, "get sword");
    assert_eq!(carried_by(&s.app, sword), Some(s.guard));
    assert!(s.app.world().get::<GroundItem>(sword).is_none());
    assert_eq!(texts(&mut s.app), ["Guard gets an iron sword."]);

    cmd(&mut s.app, s.guard_id, "drop sword");
    assert_eq!(on_ground_in(&s.app, sword), Some(s.room));
    assert_eq!(texts(&mut s.app), ["Guard drops an iron sword."]);
    cmd(&mut s.app, s.guard_id, "take sword");
    assert_eq!(carried_by(&s.app, sword), Some(s.guard));
}

#[test]
fn get_all_and_all_dot_take_what_matches() {
    let mut s = scene();
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["coin"]);
    define_item(&mut s.app, 31, TbaItemKind::Treasure, &[], &["gem"]);
    let a = item_on_ground(&mut s.app, s.room, 30, "a coin");
    let b = item_on_ground(&mut s.app, s.room, 30, "a coin");
    let c = item_on_ground(&mut s.app, s.room, 31, "a gem");
    cmd(&mut s.app, s.guard_id, "get all.coin");
    assert_eq!(carried_by(&s.app, a), Some(s.guard));
    assert_eq!(carried_by(&s.app, b), Some(s.guard));
    assert_eq!(carried_by(&s.app, c), None);
    cmd(&mut s.app, s.guard_id, "get all");
    assert_eq!(carried_by(&s.app, c), Some(s.guard));
    cmd(&mut s.app, s.guard_id, "get 2 coin");
    cmd(&mut s.app, s.guard_id, "drop all");
    for item in [a, b, c] {
        assert_eq!(on_ground_in(&s.app, item), Some(s.room));
    }
}

#[test]
fn get_from_a_container_and_put_into_one() {
    let mut s = scene();
    let chest = container(&mut s.app, s.room, 40, "chest");
    define_item(&mut s.app, 41, TbaItemKind::Treasure, &[], &["ring"]);
    let ring = put_inside(&mut s.app, chest, 41, "a ring");
    cmd(&mut s.app, s.guard_id, "get ring chest");
    assert_eq!(carried_by(&s.app, ring), Some(s.guard));
    assert!(s.app.world().get::<ContainedBy>(ring).is_none());
    assert!(
        s.app
            .world()
            .get::<Container>(chest)
            .unwrap()
            .contents
            .is_empty(),
        "the container forgets it"
    );
    assert_eq!(texts(&mut s.app), ["Guard gets a ring from chest."]);

    cmd(&mut s.app, s.guard_id, "put ring chest");
    assert_eq!(
        s.app.world().get::<ContainedBy>(ring).map(|c| c.0),
        Some(chest)
    );
    assert_eq!(
        s.app.world().get::<Container>(chest).unwrap().contents,
        vec![ring]
    );
    assert_eq!(texts(&mut s.app), ["Guard puts a ring in chest."]);
    cmd(&mut s.app, s.guard_id, "put ring chest");
}

#[test]
fn a_full_container_refuses() {
    let mut s = scene();
    let chest = container(&mut s.app, s.room, 40, "chest");
    s.app
        .world_mut()
        .get_mut::<Container>(chest)
        .unwrap()
        .capacity = 1;
    define_item(&mut s.app, 41, TbaItemKind::Treasure, &[], &["ring"]);
    put_inside(&mut s.app, chest, 41, "a ring");
    let extra = item_carried(&mut s.app, s.guard, 41, "another ring");
    cmd(&mut s.app, s.guard_id, "put ring chest");
    assert_eq!(carried_by(&s.app, extra), Some(s.guard));
}

#[test]
fn the_get_trigger_can_refuse_the_pickup() {
    let mut s = scene();
    define(
        &mut s.app,
        50,
        "obj",
        "g",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut s.app, "item:tba_item_30", &[50]);
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_on_ground(&mut s.app, s.room, 30, "a sword");
    let sword_id = uid(&mut s.app, sword);
    cmd(&mut s.app, s.guard_id, "get sword");
    assert_eq!(global(&mut s.app, sword_id, "ran").as_deref(), Some("1"));
    assert_eq!(carried_by(&s.app, sword), None, "refused");
    assert_eq!(on_ground_in(&s.app, sword), Some(s.room));
    assert!(texts(&mut s.app).is_empty());
}

#[test]
fn the_drop_trigger_can_refuse_and_the_world_drop_trigger_can_too() {
    let mut s = scene();
    define(
        &mut s.app,
        51,
        "obj",
        "h",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut s.app, "item:tba_item_30", &[51]);
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_carried(&mut s.app, s.guard, 30, "a sword");
    let sword_id = uid(&mut s.app, sword);
    cmd(&mut s.app, s.guard_id, "drop sword");
    assert_eq!(global(&mut s.app, sword_id, "ran").as_deref(), Some("1"));
    assert_eq!(
        carried_by(&s.app, sword),
        Some(s.guard),
        "refused by the object"
    );

    // A world trigger on the room: `drop_wtrigger` is offered the object.
    let mut app = fresh();
    define(
        &mut app,
        52,
        "wld",
        "h",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut app, "room:tba.100", &[52]);
    let mut t = scene_in(app);
    define_item(&mut t.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_carried(&mut t.app, t.guard, 30, "a sword");
    let room_id = uid(&mut t.app, t.room);
    cmd(&mut t.app, t.guard_id, "drop sword");
    assert_eq!(global(&mut t.app, room_id, "ran").as_deref(), Some("1"));
    assert_eq!(carried_by(&t.app, sword), Some(t.guard));
}

#[test]
fn junk_destroys_the_item_and_pays_the_gods_reward() {
    let mut s = scene();
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_carried(&mut s.app, s.guard, 30, "a sword");
    app_set_cost(&mut s.app, 30, 100);
    cmd(&mut s.app, s.guard_id, "junk sword");
    assert!(s.app.world().get_entity(sword).is_err(), "gone");
    let lines = texts(&mut s.app);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines.contains(&"Guard junks a sword.  It vanishes in a puff of smoke!".to_owned()),
        "{lines:?}"
    );
    assert!(lines.contains(&"Guard has been rewarded by the gods!".to_owned()));
}

fn app_set_cost(app: &mut App, vnum: u32, cost: u32) {
    app.world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from(format!("tba_item_{vnum}")))
        .unwrap()
        .cost = cost;
}

#[test]
fn give_hands_an_item_over_with_its_messages_and_triggers() {
    let mut s = scene();
    let clerk = npc(&mut s.app, s.room, 2, "Clerk");
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_carried(&mut s.app, s.guard, 30, "a sword");
    cmd(&mut s.app, s.guard_id, "give sword alice");
    assert_eq!(carried_by(&s.app, sword), Some(s.alice));
    let lines = texts(&mut s.app);
    assert_eq!(lines, ["Guard gives you a sword."]);

    // To an NPC the room sees it.
    let sword2 = item_carried(&mut s.app, s.guard, 30, "a sword");
    cmd(&mut s.app, s.guard_id, "give sword clerk");
    assert_eq!(carried_by(&s.app, sword2), Some(clerk));
    assert_eq!(texts(&mut s.app), ["Guard gives a sword to Clerk."]);
}

#[test]
fn give_to_oneself_or_to_nobody_does_nothing() {
    let mut s = scene();
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_carried(&mut s.app, s.guard, 30, "a sword");
    cmd(&mut s.app, s.guard_id, "give sword guard");
    cmd(&mut s.app, s.guard_id, "give sword nobody");
    cmd(&mut s.app, s.guard_id, "give sword");
    assert_eq!(carried_by(&s.app, sword), Some(s.guard));
}

#[test]
fn the_give_and_receive_triggers_run_and_either_can_refuse() {
    let mut s = scene();
    let clerk = npc(&mut s.app, s.room, 2, "Clerk");
    define(&mut s.app, 53, "obj", "i", 100, "set gave 1\nglobal gave");
    define(
        &mut s.app,
        54,
        "mob",
        "j",
        100,
        "set got %object.vnum%\nglobal got",
    );
    attach(&mut s.app, "item:tba_item_30", &[53]);
    attach(&mut s.app, "npc:tba_npc_2", &[54]);
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_carried(&mut s.app, s.guard, 30, "a sword");
    let sword_id = uid(&mut s.app, sword);
    let clerk_id = uid(&mut s.app, clerk);
    cmd(&mut s.app, s.guard_id, "give sword clerk");
    assert_eq!(global(&mut s.app, sword_id, "gave").as_deref(), Some("1"));
    assert!(global(&mut s.app, clerk_id, "got").is_some(), "receive ran");
    assert_eq!(carried_by(&s.app, sword), Some(clerk));

    // A receive trigger that returns 0 keeps the item with the giver.
    let mut t = scene();
    let clerk = npc(&mut t.app, t.room, 2, "Clerk");
    define(&mut t.app, 55, "mob", "j", 100, "return 0");
    attach(&mut t.app, "npc:tba_npc_2", &[55]);
    define_item(&mut t.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_carried(&mut t.app, t.guard, 30, "a sword");
    uid(&mut t.app, clerk);
    cmd(&mut t.app, t.guard_id, "give sword clerk");
    assert_eq!(carried_by(&t.app, sword), Some(t.guard));

    // So does a give trigger that returns 0.
    let mut u = scene();
    let clerk = npc(&mut u.app, u.room, 2, "Clerk");
    define(&mut u.app, 56, "obj", "i", 100, "return 0");
    attach(&mut u.app, "item:tba_item_30", &[56]);
    define_item(&mut u.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let sword = item_carried(&mut u.app, u.guard, 30, "a sword");
    uid(&mut u.app, clerk);
    cmd(&mut u.app, u.guard_id, "give sword clerk");
    assert_eq!(carried_by(&u.app, sword), Some(u.guard));
}

#[test]
fn give_coins_moves_gold_and_fires_the_bribe_trigger() {
    let mut s = scene();
    let clerk = npc(&mut s.app, s.room, 2, "Clerk");
    define(
        &mut s.app,
        57,
        "mob",
        "m",
        10,
        "set paid %amount%\nglobal paid",
    );
    attach(&mut s.app, "npc:tba_npc_2", &[57]);
    let clerk_id = uid(&mut s.app, clerk);
    s.app
        .world_mut()
        .entity_mut(s.guard)
        .insert(mib_tba::dg::mobcmd::TbaNpcGold(100));

    cmd(&mut s.app, s.guard_id, "give 5 coins clerk");
    assert_eq!(gold_of(&s.app, s.guard), 95);
    assert_eq!(gold_of(&s.app, clerk), 5);
    assert_eq!(
        global(&mut s.app, clerk_id, "paid"),
        None,
        "5 is under the narg"
    );
    assert_eq!(
        texts(&mut s.app),
        ["Guard gives a tiny pile of gold coins to Clerk."]
    );

    cmd(&mut s.app, s.guard_id, "give 20 coins clerk");
    assert_eq!(gold_of(&s.app, clerk), 25);
    assert_eq!(global(&mut s.app, clerk_id, "paid").as_deref(), Some("20"));

    cmd(&mut s.app, s.guard_id, "give 1000 coins clerk");
    assert_eq!(
        gold_of(&s.app, s.guard),
        75,
        "the guard cannot give what it lacks"
    );
    texts(&mut s.app);
    cmd(&mut s.app, s.guard_id, "give 3 coin alice");
    assert_eq!(gold_of(&s.app, s.alice), 3);
    assert_eq!(texts(&mut s.app), ["Guard gives you 3 gold coins."]);
}

#[test]
fn dropping_gold_makes_a_pile_that_picking_up_turns_back_into_gold() {
    let mut s = scene();
    s.app
        .world_mut()
        .entity_mut(s.guard)
        .insert(mib_tba::dg::mobcmd::TbaNpcGold(50));
    cmd(&mut s.app, s.guard_id, "drop 30 coins");
    assert_eq!(gold_of(&s.app, s.guard), 20);
    let pile = {
        let world = s.app.world_mut();
        let mut q = world.query::<(Entity, &Item, &GroundItem)>();
        q.iter(world)
            .find(|(_, item, _)| item.def_id.as_ref() == GOLD_ITEM_ID)
            .map(|(e, item, _)| (e, item.value))
            .unwrap()
    };
    assert_eq!(pile.1, Some(30));
    assert_eq!(
        texts(&mut s.app),
        ["Guard drops a little pile of gold coins."]
    );
    cmd(&mut s.app, s.guard_id, "get coins");
    assert_eq!(gold_of(&s.app, s.guard), 50);
    assert!(s.app.world().get_entity(pile.0).is_err());
}

// -- wear, wield, hold, remove -------------------------------------------------

#[test]
fn wear_wield_hold_and_remove_move_items_through_the_equipment_map() {
    let mut s = scene();
    define_item(
        &mut s.app,
        60,
        TbaItemKind::Armor,
        &[WearSlot::Body],
        &["mail"],
    );
    define_item(
        &mut s.app,
        61,
        TbaItemKind::Weapon,
        &[WearSlot::Wield],
        &["sword"],
    );
    define_item(
        &mut s.app,
        62,
        TbaItemKind::Other,
        &[WearSlot::Hold],
        &["orb"],
    );
    let mail = item_carried(&mut s.app, s.guard, 60, "a chain mail");
    let sword = item_carried(&mut s.app, s.guard, 61, "a sword");
    let orb = item_carried(&mut s.app, s.guard, 62, "an orb");

    cmd(&mut s.app, s.guard_id, "wear mail");
    cmd(&mut s.app, s.guard_id, "wield sword");
    cmd(&mut s.app, s.guard_id, "hold orb");
    let slots = &s.app.world().get::<TbaEquipment>(s.guard).unwrap().slots;
    assert_eq!(slots.get(&WearSlot::Body), Some(&mail));
    assert_eq!(slots.get(&WearSlot::Wield), Some(&sword));
    assert_eq!(slots.get(&WearSlot::Hold), Some(&orb));
    let lines = texts(&mut s.app);
    assert_eq!(
        lines,
        [
            "Guard wears a chain mail on their body.",
            "Guard wields a sword.",
            "Guard grabs an orb."
        ]
    );

    cmd(&mut s.app, s.guard_id, "remove mail");
    assert!(
        s.app
            .world()
            .get::<TbaEquipment>(s.guard)
            .unwrap()
            .worn(WearSlot::Body)
            .is_none()
    );
    assert_eq!(carried_by(&s.app, mail), Some(s.guard));
    assert_eq!(texts(&mut s.app), ["Guard stops using a chain mail."]);
    cmd(&mut s.app, s.guard_id, "remove all");
    assert!(
        s.app
            .world()
            .get::<TbaEquipment>(s.guard)
            .unwrap()
            .slots
            .is_empty()
    );
}

#[test]
fn wear_all_and_a_wear_trigger_refusal() {
    let mut s = scene();
    define(
        &mut s.app,
        58,
        "obj",
        "j",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut s.app, "item:tba_item_60", &[58]);
    define_item(
        &mut s.app,
        60,
        TbaItemKind::Armor,
        &[WearSlot::Body],
        &["mail"],
    );
    define_item(
        &mut s.app,
        63,
        TbaItemKind::Armor,
        &[WearSlot::Head],
        &["helm"],
    );
    let mail = item_carried(&mut s.app, s.guard, 60, "a chain mail");
    let helm = item_carried(&mut s.app, s.guard, 63, "an iron helm");
    let mail_id = uid(&mut s.app, mail);
    cmd(&mut s.app, s.guard_id, "wear all");
    let slots = &s.app.world().get::<TbaEquipment>(s.guard).unwrap().slots;
    assert_eq!(slots.get(&WearSlot::Head), Some(&helm));
    assert_eq!(slots.get(&WearSlot::Body), None, "the trigger said no");
    assert_eq!(global(&mut s.app, mail_id, "ran").as_deref(), Some("1"));
}

#[test]
fn the_remove_trigger_can_refuse() {
    let mut s = scene();
    define(
        &mut s.app,
        59,
        "obj",
        "l",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut s.app, "item:tba_item_60", &[59]);
    define_item(
        &mut s.app,
        60,
        TbaItemKind::Armor,
        &[WearSlot::Body],
        &["mail"],
    );
    let mail = item_worn(&mut s.app, s.guard, WearSlot::Body, 60, "a chain mail");
    let mail_id = uid(&mut s.app, mail);
    cmd(&mut s.app, s.guard_id, "remove mail");
    assert_eq!(global(&mut s.app, mail_id, "ran").as_deref(), Some("1"));
    assert_eq!(
        s.app
            .world()
            .get::<TbaEquipment>(s.guard)
            .unwrap()
            .worn(WearSlot::Body),
        Some(mail)
    );
}

#[test]
fn an_item_that_fits_nowhere_is_refused() {
    let mut s = scene();
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["sword"]);
    let junk = item_carried(&mut s.app, s.guard, 30, "a lump");
    cmd(&mut s.app, s.guard_id, "wear lump");
    cmd(&mut s.app, s.guard_id, "wield lump");
    cmd(&mut s.app, s.guard_id, "hold lump");
    assert!(
        s.app
            .world()
            .get::<TbaEquipment>(s.guard)
            .is_none_or(|e| e.slots.is_empty())
    );
    assert_eq!(carried_by(&s.app, junk), Some(s.guard));
}

#[test]
fn sacrifice_destroys_an_item_and_spills_its_contents() {
    let mut s = scene();
    let chest = container(&mut s.app, s.room, 40, "chest");
    define_item(&mut s.app, 41, TbaItemKind::Treasure, &[], &["ring"]);
    let ring = put_inside(&mut s.app, chest, 41, "a ring");
    cmd(&mut s.app, s.guard_id, "sac chest");
    assert!(s.app.world().get_entity(chest).is_err());
    assert_eq!(on_ground_in(&s.app, ring), Some(s.room));
    assert_eq!(texts(&mut s.app), ["Guard sacrifices chest."]);
}

// -- doors ---------------------------------------------------------------------

/// Two rooms joined north/south by a door on both sides.
struct Doorway {
    s: Scene,
    north: Entity,
}

fn doorway(closed: bool, locked: bool) -> Doorway {
    doorway_in(fresh(), closed, locked)
}

fn doorway_in(base: App, closed: bool, locked: bool) -> Doorway {
    let mut s = scene_in(base);
    let north = room(&mut s.app, 101);
    link(&mut s.app, s.room, ExitKey::North, north, ExitKey::South);
    door(
        &mut s.app,
        s.room,
        ExitKey::North,
        Some("door gate"),
        Some(70),
        closed,
        locked,
    );
    door(
        &mut s.app,
        north,
        ExitKey::South,
        Some("door gate"),
        Some(70),
        closed,
        locked,
    );
    Doorway { s, north }
}

#[test]
fn open_and_close_change_both_sides_and_tell_both_rooms() {
    let mut d = doorway(true, false);
    let bob = player(&mut d.s.app, d.north, "Bob", 8);
    let _ = bob;
    cmd(&mut d.s.app, d.s.guard_id, "open door");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (false, false)
    );
    assert_eq!(
        door_state(&d.s.app, d.north, ExitKey::South),
        (false, false)
    );
    let lines = texts(&mut d.s.app);
    assert!(
        lines.contains(&"Guard opens the door.".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"The door is opened from the other side.".to_owned()),
        "{lines:?}"
    );

    cmd(&mut d.s.app, d.s.guard_id, "close door north");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (true, false)
    );
    assert_eq!(door_state(&d.s.app, d.north, ExitKey::South), (true, false));
    let lines = texts(&mut d.s.app);
    assert!(
        lines.contains(&"Guard closes the door.".to_owned()),
        "{lines:?}"
    );
    assert!(lines.contains(&"The door is closed from the other side.".to_owned()));

    cmd(&mut d.s.app, d.s.guard_id, "o gate");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (false, false)
    );
}

#[test]
fn a_door_in_the_wrong_state_is_refused_without_a_change() {
    let mut d = doorway(false, false);
    cmd(&mut d.s.app, d.s.guard_id, "open door");
    assert!(
        texts(&mut d.s.app).is_empty(),
        "already open: only the NPC is told"
    );
    cmd(&mut d.s.app, d.s.guard_id, "unlock door");
    cmd(&mut d.s.app, d.s.guard_id, "lock door");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (false, false),
        "locking an open door needs it closed first"
    );
    cmd(&mut d.s.app, d.s.guard_id, "open east");
    cmd(&mut d.s.app, d.s.guard_id, "open window");
    assert!(texts(&mut d.s.app).is_empty());
}

#[test]
fn lock_and_unlock_need_the_key_and_a_closed_door() {
    let mut d = doorway(true, false);
    cmd(&mut d.s.app, d.s.guard_id, "lock door");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (true, false),
        "no key"
    );
    define_item(&mut d.s.app, 70, TbaItemKind::Key, &[], &["key"]);
    item_carried(&mut d.s.app, d.s.guard, 70, "a brass key");
    cmd(&mut d.s.app, d.s.guard_id, "lock door");
    assert_eq!(door_state(&d.s.app, d.s.room, ExitKey::North), (true, true));
    assert_eq!(door_state(&d.s.app, d.north, ExitKey::South), (true, true));
    let lines = texts(&mut d.s.app);
    assert_eq!(lines, ["Guard locks the door."]);

    cmd(&mut d.s.app, d.s.guard_id, "open door");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (true, true),
        "locked doors do not open"
    );
    cmd(&mut d.s.app, d.s.guard_id, "unlock door");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (true, false)
    );
    assert_eq!(texts(&mut d.s.app), ["Guard unlocks the door."]);
}

#[test]
fn a_held_key_works_too_and_the_wrong_key_does_not() {
    let mut d = doorway(true, false);
    define_item(&mut d.s.app, 71, TbaItemKind::Key, &[], &["key"]);
    item_carried(&mut d.s.app, d.s.guard, 71, "a rusty key");
    cmd(&mut d.s.app, d.s.guard_id, "lock door");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (true, false)
    );
    define_item(&mut d.s.app, 70, TbaItemKind::Key, &[], &["key"]);
    item_worn(&mut d.s.app, d.s.guard, WearSlot::Hold, 70, "a brass key");
    cmd(&mut d.s.app, d.s.guard_id, "lock door");
    assert_eq!(door_state(&d.s.app, d.s.room, ExitKey::North), (true, true));
}

#[test]
fn pick_never_succeeds_for_an_npc() {
    let mut d = doorway(true, true);
    cmd(&mut d.s.app, d.s.guard_id, "pick door");
    assert_eq!(door_state(&d.s.app, d.s.room, ExitKey::North), (true, true));
}

#[test]
fn a_door_with_one_side_only_changes_one_side() {
    let mut s = scene();
    let north = room(&mut s.app, 101);
    connect(&mut s.app, s.room, ExitKey::North, north);
    connect(&mut s.app, north, ExitKey::South, s.room);
    door(
        &mut s.app,
        s.room,
        ExitKey::North,
        Some("door"),
        None,
        true,
        false,
    );
    cmd(&mut s.app, s.guard_id, "open door");
    assert_eq!(door_state(&s.app, s.room, ExitKey::North), (false, false));
}

#[test]
fn the_door_world_trigger_can_stop_the_command() {
    let mut app = fresh();
    define(
        &mut app,
        60,
        "wld",
        "r",
        100,
        "set cmdv %cmd%\nset dirv %direction%\nglobal cmdv\nglobal dirv\nreturn 0",
    );
    attach(&mut app, "room:tba.100", &[60]);
    let mut d = doorway_in(app, true, false);
    let room_id = uid(&mut d.s.app, d.s.room);
    cmd(&mut d.s.app, d.s.guard_id, "open door");
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (true, false)
    );
    assert_eq!(
        global(&mut d.s.app, room_id, "cmdv").as_deref(),
        Some("open")
    );
    assert_eq!(
        global(&mut d.s.app, room_id, "dirv").as_deref(),
        Some("north")
    );
    assert!(texts(&mut d.s.app).is_empty());
}

#[test]
fn the_door_mob_trigger_can_stop_the_command() {
    if !chars_modelled() {
        return;
    }
    let mut app = fresh();
    define(
        &mut app,
        61,
        "mob",
        "r",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut app, "npc:tba_npc_2", &[61]);
    let mut d = doorway_in(app, true, false);
    let watcher = npc(&mut d.s.app, d.s.room, 2, "Clerk");
    let watcher_id = uid(&mut d.s.app, watcher);
    cmd(&mut d.s.app, d.s.guard_id, "open door");
    assert_eq!(
        global(&mut d.s.app, watcher_id, "ran").as_deref(),
        Some("1")
    );
    assert_eq!(
        door_state(&d.s.app, d.s.room, ExitKey::North),
        (true, false)
    );
}

#[test]
fn a_container_is_not_a_door() {
    let mut s = scene();
    let chest = container(&mut s.app, s.room, 40, "chest");
    cmd(&mut s.app, s.guard_id, "open chest");
    cmd(&mut s.app, s.guard_id, "close chest");
    assert!(texts(&mut s.app).is_empty());
    // A closeable container is a counted gap, not a guess.
    s.app
        .world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_40"))
        .unwrap()
        .values = [10, 1, 0, 0];
    let _ = chest;
    cmd(&mut s.app, s.guard_id, "open chest");
    assert_eq!(
        s.app
            .world()
            .resource::<mib_tba::dg::DgLog>()
            .count("mobcmd.container_door"),
        1
    );
}

// -- movement ------------------------------------------------------------------

#[test]
fn walking_tells_both_rooms_and_abbreviations_follow_the_table() {
    let mut d = doorway(false, false);
    let bob = player(&mut d.s.app, d.north, "Bob", 8);
    let _ = bob;
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.north);
    let lines = texts(&mut d.s.app);
    assert!(
        lines.contains(&"Guard leaves north.".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Guard has arrived.".to_owned()),
        "{lines:?}"
    );
    cmd(&mut d.s.app, d.s.guard_id, "s");
    assert_eq!(
        room_of(&d.s.app, d.s.guard),
        d.s.room,
        "`s` is south, not say"
    );
    cmd(&mut d.s.app, d.s.guard_id, "n");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.north);
    cmd(&mut d.s.app, d.s.guard_id, "south");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    cmd(&mut d.s.app, d.s.guard_id, "west");
    cmd(&mut d.s.app, d.s.guard_id, "up");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room, "no such exit");
}

#[test]
fn a_closed_door_blocks_the_walk_and_a_sneaking_npc_makes_no_noise() {
    let mut d = doorway(true, false);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    assert!(texts(&mut d.s.app).is_empty());
    cmd(&mut d.s.app, d.s.guard_id, "open door");
    texts(&mut d.s.app);
    d.s.app
        .world_mut()
        .entity_mut(d.s.guard)
        .insert(TbaSneaking);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.north);
    assert!(texts(&mut d.s.app).is_empty(), "sneaking");
}

#[test]
fn diagonals_are_off_as_in_the_default_config() {
    let mut s = scene();
    let ne = room(&mut s.app, 102);
    connect(&mut s.app, s.room, ExitKey::Northeast, ne);
    cmd(&mut s.app, s.guard_id, "ne");
    cmd(&mut s.app, s.guard_id, "northeast");
    assert_eq!(room_of(&s.app, s.guard), s.room);
}

#[test]
fn a_fighting_npc_does_not_walk() {
    let mut d = doorway(false, false);
    set_position(&mut d.s.app, d.s.alice, Position::Standing);
    cmd(&mut d.s.app, d.s.guard_id, "kill alice");
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
}

#[test]
fn the_leave_triggers_can_stop_a_walk() {
    // World trigger on the room being left.
    let mut app = fresh();
    define(
        &mut app,
        62,
        "wld",
        "q",
        100,
        "set dirv %direction%\nglobal dirv\nreturn 0",
    );
    attach(&mut app, "room:tba.100", &[62]);
    let mut d = doorway_in(app, false, false);
    let room_id = uid(&mut d.s.app, d.s.room);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    assert_eq!(
        global(&mut d.s.app, room_id, "dirv").as_deref(),
        Some("north")
    );
    assert!(texts(&mut d.s.app).is_empty(), "nobody saw it leave");

    // Object trigger on something lying in the room.
    let mut app = fresh();
    define(
        &mut app,
        63,
        "obj",
        "q",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut app, "item:tba_item_30", &[63]);
    let mut d = doorway_in(app, false, false);
    define_item(&mut d.s.app, 30, TbaItemKind::Treasure, &[], &["rock"]);
    let rock = item_on_ground(&mut d.s.app, d.s.room, 30, "a rock");
    let rock_id = uid(&mut d.s.app, rock);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    assert_eq!(global(&mut d.s.app, rock_id, "ran").as_deref(), Some("1"));
}

#[test]
fn the_leave_mob_trigger_can_stop_a_walk() {
    if !chars_modelled() {
        return;
    }
    let mut app = fresh();
    define(
        &mut app,
        64,
        "mob",
        "q",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut app, "npc:tba_npc_2", &[64]);
    let mut d = doorway_in(app, false, false);
    let watcher = npc(&mut d.s.app, d.s.room, 2, "Clerk");
    let id = uid(&mut d.s.app, watcher);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    assert_eq!(global(&mut d.s.app, id, "ran").as_deref(), Some("1"));
}

#[test]
fn an_entry_trigger_that_refuses_sends_the_mover_back() {
    let mut app = fresh();
    define(
        &mut app,
        65,
        "mob",
        "i",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut app, "npc:tba_npc_1", &[65]);
    let mut d = doorway_in(app, false, false);
    let id = d.s.guard_id;
    cmd(&mut d.s.app, id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    assert_eq!(global(&mut d.s.app, id, "ran").as_deref(), Some("1"));
}

#[test]
fn an_enter_world_trigger_that_refuses_sends_the_mover_back() {
    let mut app = fresh();
    define(
        &mut app,
        66,
        "wld",
        "g",
        100,
        "set dirv %direction%\nglobal dirv\nreturn 0",
    );
    attach(&mut app, "room:tba.101", &[66]);
    let mut d = doorway_in(app, false, false);
    let north_id = uid(&mut d.s.app, d.north);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    assert_eq!(
        global(&mut d.s.app, north_id, "dirv").as_deref(),
        Some("south")
    );
}

#[test]
fn a_greet_trigger_that_refuses_sends_the_mover_back() {
    if !chars_modelled() {
        return;
    }
    let mut app = fresh();
    define(
        &mut app,
        67,
        "mob",
        "g",
        100,
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut app, "npc:tba_npc_2", &[67]);
    let mut d = doorway_in(app, false, false);
    let watcher = npc(&mut d.s.app, d.north, 2, "Clerk");
    let id = uid(&mut d.s.app, watcher);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    assert_eq!(global(&mut d.s.app, id, "ran").as_deref(), Some("1"));
}

#[test]
fn npc_followers_walk_the_exit_themselves_and_players_are_left_to_the_follow_system() {
    let mut d = doorway(false, false);
    let clerk = npc(&mut d.s.app, d.s.room, 2, "Clerk");
    let clerk_id = uid(&mut d.s.app, clerk);
    let _ = clerk_id;
    d.s.app.world_mut().entity_mut(clerk).insert(TbaFollowing {
        leader: d.s.guard,
        last_room: d.s.room,
    });
    d.s.app
        .world_mut()
        .entity_mut(d.s.alice)
        .insert(TbaFollowing {
            leader: d.s.guard,
            last_room: d.s.room,
        });
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, clerk), d.north);
    assert_eq!(room_of(&d.s.app, d.s.alice), d.s.room);
    assert_eq!(
        d.s.app
            .world()
            .get::<TbaFollowing>(clerk)
            .unwrap()
            .last_room,
        d.north,
        "the follow system must not drag it a second time"
    );
}

#[test]
fn a_follower_that_is_not_standing_stays_behind() {
    let mut d = doorway(false, false);
    let clerk = npc(&mut d.s.app, d.s.room, 2, "Clerk");
    d.s.app.world_mut().entity_mut(clerk).insert(TbaFollowing {
        leader: d.s.guard,
        last_room: d.s.room,
    });
    set_position(&mut d.s.app, clerk, Position::Sitting);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, clerk), d.s.room);
}

#[test]
fn deep_water_needs_a_boat() {
    let mut d = doorway(false, false);
    d.s.app.world_mut().get_mut::<Room>(d.north).unwrap().sector =
        mib_stdlib::content::Sector::WaterNoSwim;
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.s.room);
    define_item(&mut d.s.app, 80, TbaItemKind::Boat, &[], &["boat"]);
    item_carried(&mut d.s.app, d.s.guard, 80, "a boat");
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(room_of(&d.s.app, d.s.guard), d.north);
}

#[test]
fn a_tunnel_holds_one_player_fewer_than_its_limit_of_two() {
    let mut d = doorway(false, false);
    d.s.app
        .world_mut()
        .get_mut::<Room>(d.north)
        .unwrap()
        .tags
        .insert(RoomTag::Tunnel);
    player(&mut d.s.app, d.north, "Bob", 8);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(
        room_of(&d.s.app, d.s.guard),
        d.north,
        "one player: room for one more"
    );
    cmd(&mut d.s.app, d.s.guard_id, "south");
    player(&mut d.s.app, d.north, "Cy", 9);
    cmd(&mut d.s.app, d.s.guard_id, "north");
    assert_eq!(
        room_of(&d.s.app, d.s.guard),
        d.s.room,
        "two players fill it"
    );
}

#[test]
fn enter_walks_through_the_exit_whose_door_keyword_is_the_argument() {
    let mut s = scene();
    let inside = room(&mut s.app, 101);
    connect(&mut s.app, s.room, ExitKey::East, inside);
    door(
        &mut s.app,
        s.room,
        ExitKey::East,
        Some("gate"),
        None,
        false,
        false,
    );
    cmd(&mut s.app, s.guard_id, "enter gate");
    assert_eq!(room_of(&s.app, s.guard), inside);
}

// -- combat --------------------------------------------------------------------

#[test]
fn kill_and_hit_aim_both_sides_at_each_other() {
    let mut s = scene();
    set_position(&mut s.app, s.alice, Position::Standing);
    cmd(&mut s.app, s.guard_id, "kill alice");
    assert_eq!(target_of(&s.app, s.guard), Some(s.alice));
    assert_eq!(target_of(&s.app, s.alice), Some(s.guard));
    assert_eq!(position_of(&s.app, s.guard), Position::Fighting);
    assert_eq!(position_of(&s.app, s.alice), Position::Fighting);

    let mut t = scene();
    cmd(&mut t.app, t.guard_id, "hit alice");
    assert_eq!(target_of(&t.app, t.guard), Some(t.alice));
}

#[test]
fn hitting_yourself_and_a_peaceful_room_and_a_missing_target() {
    let mut s = scene();
    cmd(&mut s.app, s.guard_id, "hit guard");
    assert_eq!(texts(&mut s.app), ["Guard hits themself, and says OUCH!"]);
    assert_eq!(target_of(&s.app, s.guard), None);
    s.app
        .world_mut()
        .get_mut::<Room>(s.room)
        .unwrap()
        .tags
        .insert(RoomTag::Peaceful);
    cmd(&mut s.app, s.guard_id, "kill alice");
    assert_eq!(target_of(&s.app, s.guard), None);
    cmd(&mut s.app, s.guard_id, "kill zorg");
}

#[test]
fn assist_joins_the_fight_of_the_character_helped() {
    let mut s = scene();
    let clerk = npc(&mut s.app, s.room, 2, "Clerk");
    set_position(&mut s.app, s.alice, Position::Standing);
    cmd(&mut s.app, s.guard_id, "kill alice");
    texts(&mut s.app);
    let clerk_id = uid(&mut s.app, clerk);
    cmd(&mut s.app, clerk_id, "assist guard");
    assert_eq!(target_of(&s.app, clerk), Some(s.alice));
    assert_eq!(texts(&mut s.app), ["Clerk assists Guard."]);

    // Nobody to help against.
    let mut t = scene();
    let clerk = npc(&mut t.app, t.room, 2, "Clerk");
    let clerk_id = uid(&mut t.app, clerk);
    cmd(&mut t.app, clerk_id, "assist guard");
    assert_eq!(target_of(&t.app, clerk), None);
}

#[test]
fn flee_moves_through_an_open_exit_and_ends_the_fight() {
    let mut d = doorway(false, false);
    set_position(&mut d.s.app, d.s.alice, Position::Standing);
    cmd(&mut d.s.app, d.s.guard_id, "kill alice");
    texts(&mut d.s.app);
    let mut moved = false;
    for _ in 0..40 {
        cmd(&mut d.s.app, d.s.guard_id, "flee");
        if room_of(&d.s.app, d.s.guard) == d.north {
            moved = true;
            break;
        }
    }
    assert!(moved, "one open exit among six tries per attempt");
    assert_eq!(target_of(&d.s.app, d.s.guard), None);
    assert_eq!(target_of(&d.s.app, d.s.alice), None, "the pursuit ends too");
    let lines = texts(&mut d.s.app);
    assert!(lines.contains(&"Guard panics, and attempts to flee!".to_owned()));
}

#[test]
fn flee_with_no_exit_goes_nowhere() {
    let mut s = scene();
    set_position(&mut s.app, s.alice, Position::Standing);
    cmd(&mut s.app, s.guard_id, "kill alice");
    texts(&mut s.app);
    cmd(&mut s.app, s.guard_id, "flee");
    assert_eq!(room_of(&s.app, s.guard), s.room);
    assert_eq!(target_of(&s.app, s.guard), Some(s.alice));
    assert!(texts(&mut s.app).is_empty());
}

// -- following -----------------------------------------------------------------

#[test]
fn follow_and_unfollow_tell_the_leader_and_the_room() {
    let mut s = scene();
    let bob = player(&mut s.app, s.room, "Bob", 8);
    let _ = bob;
    cmd(&mut s.app, s.guard_id, "follow alice");
    let following = s.app.world().get::<TbaFollowing>(s.guard).unwrap();
    assert_eq!(following.leader, s.alice);
    let lines = texts(&mut s.app);
    assert!(
        lines.contains(&"Guard starts following you.".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Guard starts to follow Alice.".to_owned()),
        "{lines:?}"
    );

    cmd(&mut s.app, s.guard_id, "follow alice");
    assert!(
        s.app.world().get::<TbaFollowing>(s.guard).is_some(),
        "already following"
    );
    cmd(&mut s.app, s.guard_id, "unfollow");
    assert!(s.app.world().get::<TbaFollowing>(s.guard).is_none());
    let lines = texts(&mut s.app);
    assert!(
        lines.contains(&"Guard stops following you.".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Guard stops following Alice.".to_owned()),
        "{lines:?}"
    );
    cmd(&mut s.app, s.guard_id, "unfollow");
}

#[test]
fn following_oneself_stops_following_and_loops_are_refused() {
    let mut s = scene();
    let clerk = npc(&mut s.app, s.room, 2, "Clerk");
    let clerk_id = uid(&mut s.app, clerk);
    cmd(&mut s.app, s.guard_id, "fol clerk");
    assert_eq!(
        s.app.world().get::<TbaFollowing>(s.guard).unwrap().leader,
        clerk
    );
    cmd(&mut s.app, clerk_id, "follow guard");
    assert!(s.app.world().get::<TbaFollowing>(clerk).is_none(), "a loop");
    cmd(&mut s.app, s.guard_id, "follow guard");
    assert!(
        s.app.world().get::<TbaFollowing>(s.guard).is_none(),
        "following oneself stops"
    );
}

// -- socials -------------------------------------------------------------------

fn standard_socials() -> Vec<(SmolStr, SocialDef)> {
    let mut hug = social(
        "hug",
        Some("You hug everyone."),
        Some("$n hugs everyone."),
        Some("You hug $N."),
        Some("$n hugs $N."),
        Some("$n hugs you."),
    );
    hug.1.char_body_found = Some("You hug $N around the $t.".to_owned());
    hug.1.others_body_found = Some("$n hugs $N around the $t.".to_owned());
    hug.1.vict_body_found = Some("$n hugs you around the $t.".to_owned());
    vec![
        social(
            "smile",
            Some("You smile."),
            Some("$n smiles."),
            Some("You smile at $N."),
            Some("$n smiles at $N."),
            Some("$n smiles at you."),
        ),
        social("smirk", None, Some("$n smirks."), None, None, None),
        hug,
        social(
            "dance",
            Some("You dance."),
            Some("$n dances."),
            None,
            None,
            None,
        ),
    ]
}

#[test]
fn a_social_with_no_target_shows_the_room_its_no_arg_line() {
    let mut s = scene();
    install_socials(&mut s.app, standard_socials());
    cmd(&mut s.app, s.guard_id, "smile");
    assert_eq!(texts(&mut s.app), ["Guard smiles."]);
}

#[test]
fn a_social_with_a_target_tells_the_victim_and_the_bystanders() {
    let mut s = scene();
    install_socials(&mut s.app, standard_socials());
    player(&mut s.app, s.room, "Bob", 8);
    cmd(&mut s.app, s.guard_id, "smile alice");
    let lines = texts(&mut s.app);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.contains(&"Guard smiles at you.".to_owned()));
    assert!(lines.contains(&"Guard smiles at Alice.".to_owned()));
}

#[test]
fn a_social_aimed_at_oneself_uses_the_auto_lines() {
    let mut s = scene();
    install_socials(&mut s.app, standard_socials());
    cmd(&mut s.app, s.guard_id, "hug guard");
    assert_eq!(texts(&mut s.app), ["Guard hugs themself."]);
}

#[test]
fn a_social_with_a_body_part_fills_in_t() {
    let mut s = scene();
    install_socials(&mut s.app, standard_socials());
    cmd(&mut s.app, s.guard_id, "hug alice shoulder");
    assert_eq!(texts(&mut s.app), ["Guard hugs you around the shoulder."]);
    // A social without body text refuses a body part.
    cmd(&mut s.app, s.guard_id, "smile alice nose");
    assert!(texts(&mut s.app).is_empty());
}

#[test]
fn an_unknown_target_or_missing_char_found_reaches_only_the_actor() {
    let mut s = scene();
    install_socials(&mut s.app, standard_socials());
    cmd(&mut s.app, s.guard_id, "smile nobody");
    cmd(&mut s.app, s.guard_id, "smirk alice");
    assert!(texts(&mut s.app).is_empty());
}

#[test]
fn abbreviations_pick_the_first_social_in_sort_order_and_real_commands_win() {
    let mut s = scene();
    install_socials(&mut s.app, standard_socials());
    cmd(&mut s.app, s.guard_id, "sm");
    assert_eq!(
        texts(&mut s.app),
        ["Guard smiles."],
        "smile sorts before smirk"
    );
    cmd(&mut s.app, s.guard_id, "smir");
    assert_eq!(texts(&mut s.app), ["Guard smirks."]);
    // `da` abbreviates the immortal `date`, which a level-10 mob cannot use, so
    // the search goes on to the socials.
    cmd(&mut s.app, s.guard_id, "da");
    assert_eq!(texts(&mut s.app), ["Guard dances."]);
    // `d` is `down`, a real command, before the social `dance`.
    let below = room(&mut s.app, 103);
    connect(&mut s.app, s.room, ExitKey::Down, below);
    cmd(&mut s.app, s.guard_id, "d");
    assert_eq!(room_of(&s.app, s.guard), below);
}

#[test]
fn a_social_needs_its_minimum_position_and_its_victims() {
    let mut s = scene();
    let mut sleepy = social(
        "nudge",
        Some("You nudge."),
        Some("$n nudges."),
        Some("You nudge $N."),
        Some("$n nudges $N."),
        Some("$n nudges you."),
    );
    sleepy.1.min_victim_position = 5;
    sleepy.1.min_char_position = 8;
    install_socials(&mut s.app, vec![sleepy]);
    let clerk = npc(&mut s.app, s.room, 2, "Clerk");
    set_position(&mut s.app, clerk, Position::Sleeping);
    set_position(&mut s.app, s.guard, Position::Resting);
    cmd(&mut s.app, s.guard_id, "nudge");
    assert!(texts(&mut s.app).is_empty(), "the actor must be standing");
    set_position(&mut s.app, s.guard, Position::Standing);
    cmd(&mut s.app, s.guard_id, "nudge clerk");
    assert!(texts(&mut s.app).is_empty(), "the victim is asleep");
    cmd(&mut s.app, s.guard_id, "nudge alice");
    assert_eq!(texts(&mut s.app), ["Guard nudges you."]);
}

// -- command triggers ----------------------------------------------------------

#[test]
fn a_command_trigger_takes_the_command_over() {
    let mut s = scene();
    let clerk = npc(&mut s.app, s.room, 2, "Clerk");
    define_arg(
        &mut s.app,
        70,
        "mob",
        "c",
        1,
        "say",
        "set cmdv %cmd%\nset argv %arg%\nglobal cmdv\nglobal argv",
    );
    attach(&mut s.app, "npc:tba_npc_2", &[70]);
    let clerk_id = uid(&mut s.app, clerk);
    cmd(&mut s.app, s.guard_id, "say hello there");
    assert!(texts(&mut s.app).is_empty(), "the trigger claimed the say");
    assert_eq!(global(&mut s.app, clerk_id, "cmdv").as_deref(), Some("say"));
    assert_eq!(
        global(&mut s.app, clerk_id, "argv").as_deref(),
        Some("hello there")
    );
}

#[test]
fn a_command_trigger_that_returns_zero_lets_the_command_run() {
    let mut s = scene();
    let clerk = npc(&mut s.app, s.room, 2, "Clerk");
    define_arg(
        &mut s.app,
        71,
        "mob",
        "c",
        1,
        "say",
        "set ran 1\nglobal ran\nreturn 0",
    );
    attach(&mut s.app, "npc:tba_npc_2", &[71]);
    let clerk_id = uid(&mut s.app, clerk);
    cmd(&mut s.app, s.guard_id, "say hello");
    assert_eq!(global(&mut s.app, clerk_id, "ran").as_deref(), Some("1"));
    assert_eq!(texts(&mut s.app), ["Guard says, 'hello'"]);
}

#[test]
fn the_world_command_trigger_sees_the_command_first_and_the_apostrophe_is_its_own_word() {
    let mut app = fresh();
    define_arg(
        &mut app,
        72,
        "wld",
        "c",
        1,
        "'",
        "set cmdv %cmd%\nset argv %arg%\nglobal cmdv\nglobal argv",
    );
    attach(&mut app, "room:tba.100", &[72]);
    let mut s = scene_in(app);
    let room_id = uid(&mut s.app, s.room);
    cmd(&mut s.app, s.guard_id, "'hi there");
    assert!(texts(&mut s.app).is_empty());
    assert_eq!(global(&mut s.app, room_id, "cmdv").as_deref(), Some("'"));
    assert_eq!(
        global(&mut s.app, room_id, "argv").as_deref(),
        Some("hi there")
    );
}

#[test]
fn the_object_command_trigger_of_something_the_npc_holds_can_claim_a_command() {
    let mut app = fresh();
    define_arg(&mut app, 73, "obj", "c", 7, "wave", "set ran 1\nglobal ran");
    attach(&mut app, "item:tba_item_30", &[73]);
    let mut s = scene_in(app);
    define_item(&mut s.app, 30, TbaItemKind::Treasure, &[], &["wand"]);
    let wand = item_carried(&mut s.app, s.guard, 30, "a wand");
    let wand_id = uid(&mut s.app, wand);
    install_socials(
        &mut s.app,
        vec![social(
            "wave",
            Some("You wave."),
            Some("$n waves."),
            None,
            None,
            None,
        )],
    );
    cmd(&mut s.app, s.guard_id, "wave");
    assert_eq!(global(&mut s.app, wand_id, "ran").as_deref(), Some("1"));
    assert!(texts(&mut s.app).is_empty(), "the object claimed it");
}

// -- a forced player -----------------------------------------------------------

fn online(app: &mut App, player_entity: Entity, room: Entity, session: u64) {
    app.init_resource::<OnlinePlayers>();
    app.add_message::<CommandReceived>();
    app.world_mut().resource_mut::<OnlinePlayers>().map.insert(
        session,
        OnlinePlayer {
            entity: player_entity,
            name: "Alice".to_owned(),
            room,
            account_id: 7,
        },
    );
}

fn drained(app: &mut App) -> Vec<CommandReceived> {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .drain()
        .collect()
}

#[test]
fn a_forced_player_gets_the_line_as_its_own_input_with_a_fresh_id_each_time() {
    let mut s = scene();
    online(&mut s.app, s.alice, s.room, 42);
    cmd(&mut s.app, s.alice_id, "say I was forced");
    cmd(&mut s.app, s.alice_id, "north");
    let sent = drained(&mut s.app);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].session, s.alice);
    assert_eq!(sent[0].session_id, 42);
    assert!(matches!(&sent[0].input, PlayerInput::RawLine(line) if line == "say I was forced"));
    assert!(matches!(&sent[1].input, PlayerInput::RawLine(line) if line == "north"));
    assert_ne!(sent[0].id, sent[1].id, "two forced commands in one frame");
    assert_ne!(
        sent[0].id,
        CommandId(0),
        "id 0 would be skipped as already handled"
    );
    assert_ne!(sent[1].id, CommandId(0));
    // Nothing happened yet: the line runs through the pipeline next frame.
    assert!(texts(&mut s.app).is_empty());
}

#[test]
fn a_forced_player_with_no_session_is_a_counted_gap() {
    let mut s = scene();
    s.app.add_message::<CommandReceived>();
    cmd(&mut s.app, s.alice_id, "say hi");
    assert!(drained(&mut s.app).is_empty());
    assert_eq!(
        s.app
            .world()
            .resource::<mib_tba::dg::DgLog>()
            .count("mobcmd.force_player"),
        1
    );
}
