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

fn scene() -> Scene {
    scene_in(app())
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
    let mut app = app();
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
    define_arg(&mut s.app, 10, "mob", "d", 1, "*", "set heard %speech%\nglobal heard");
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
    define_arg(&mut s.app, 11, "mob", "d", 1, "*", "set heard 1\nglobal heard");
    attach(&mut s.app, "npc:tba_npc_1", &[11]);
    let g = uid(&mut s.app, s.guard);
    cmd(&mut s.app, g, "say Hello");
    assert_eq!(global(&mut s.app, g, "heard"), None);
}

#[test]
fn say_runs_the_rooms_speech_trigger() {
    let mut app = app();
    define_arg(&mut app, 12, "wld", "d", 1, "*", "set heard %speech%\nglobal heard");
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
    define_arg(&mut s.app, 10, "mob", "d", 1, "*", "set heard 1\nglobal heard");
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
    define_arg(&mut s.app, 13, "mob", "d", 1, "*", "set heard 1\nglobal heard");
    attach(&mut s.app, "npc:tba_npc_2", &[13]);
    let listener_id = uid(&mut s.app, listener);
    cmd(&mut s.app, s.guard_id, "say");
    assert!(texts(&mut s.app).is_empty(), "an empty say says nothing");
    assert_eq!(global(&mut s.app, listener_id, "heard").as_deref(), Some("1"));
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
    define_arg(&mut s.app, 14, "mob", "e", 0, "dances", "set saw %arg%\nglobal saw");
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
    assert!(lines.contains(&"Guard asks you, 'who'".to_owned()), "{lines:?}");
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
        lines.iter().filter(|l| *l == "Guard shouts, 'Fire!'").count(),
        2,
        "{lines:?}"
    );
    cmd(&mut s.app, s.guard_id, "gossip news");
    let lines = texts(&mut s.app);
    assert_eq!(
        lines.iter().filter(|l| *l == "Guard gossips, 'news'").count(),
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
    assert!(texts(&mut s.app).is_empty(), "sitting already: only the NPC is told");
    cmd(&mut s.app, g, "rest");
    assert_eq!(position_of(&s.app, s.guard), Position::Resting);
    assert_eq!(texts(&mut s.app), ["Guard rests."]);
    cmd(&mut s.app, g, "sleep");
    assert_eq!(position_of(&s.app, s.guard), Position::Sleeping);
    assert_eq!(texts(&mut s.app), ["Guard lies down and falls asleep."]);
    cmd(&mut s.app, g, "stand");
    assert_eq!(position_of(&s.app, s.guard), Position::Sleeping, "wake up first");
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
