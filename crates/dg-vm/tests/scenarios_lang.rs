//! Scenario tests on real tbaMUD triggers chosen to cover the language:
//! control flow, variables, text functions, entity walks, the command
//! families of mobs, objects and rooms.
//!
//! Every expectation is worked out by hand from the C (`dg_scripts.c`,
//! `dg_variables.c`, `dg_mobcmd.c`, `dg_objcmd.c`, `dg_wldcmd.c`,
//! `dg_misc.c`, `dg_comm.c`) for the world and event set up in the test, not
//! read back from the VM. The reasoning is in the comment above each test.
//! Events are fired the way `dg_triggers.c` would: the event's variables go
//! onto the instance, then `script_driver` runs it.
//!
//! The standard cast (see `common::scene`): room 3001 holds "Tester" (male
//! player, level 10), "Watcher" (a second player), the guard mob "a city
//! guard" (no descriptor) and a bench. Only characters with a descriptor
//! receive text.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::corpus::Record;
use common::scene::{Scene, assert_clean, heard, other_effects, said, vnums};
use common::world::{commands, fire, global_var, install_record, run_waits, uid};
use dg_vm::host::BaseHost;
use dg_vm::types::Uid;

// ---------------------------------------------------------------------
// Trigger 53, room speech `2 d 1`: car, cdr and a while loop.
//
// `%echo%` is `wecho` in a room trigger. `speech` = "go north now":
// car = first word "go", cdr = the rest after skipping spaces, "north now".
// `set word go`, `set rest north now`. The loop runs while `%word%` is
// non-empty: it echoes both, then `set word %rest.car%` ("north"),
// `set rest %rest.cdr%` ("now"); second pass echoes north/now, then word
// "now", rest "" (cdr of "now" is empty); third pass echoes now and an empty
// remainder, then word = "".car = "" which ends the loop. The comment lines
// (`*`) do nothing. wecho is act_to_room(): act() capitalises the first
// letter (CAP) and ends the line in "\r\n", so the lowercase "the first word"
// of the script arrives as "The first word". Every echo reaches both players.
// ---------------------------------------------------------------------
#[test]
fn trigger_53_walks_the_words_of_a_speech_with_car_and_cdr() {
    let record = real!(53);
    let mut scene = Scene::new();
    let (room, player, watcher) = (scene.room(), scene.player(), scene.watcher);
    let id = install_record(scene.host(), room, &record);
    let ret = fire(
        scene.host(),
        room,
        id,
        &[
            ("actor", uid(player)),
            ("speech", "go north now".to_owned()),
        ],
    );
    assert_eq!(ret, 1);
    let expected = vec![
        "The first word is: go",
        "The rest of the string is: north now",
        "The first word is: go",
        "The remaining text is: north now",
        "The first word is: north",
        "The remaining text is: now",
        "The first word is: now",
        "The remaining text is: ",
    ];
    assert_eq!(heard(&scene.fx.host, player), expected);
    assert_eq!(heard(&scene.fx.host, watcher), expected);
    assert!(scene.fx.host.logs.is_empty());
    assert!(scene.fx.host.waits.is_empty());
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 37, object command `flip` (carried): `/=`, switch, %random%.
//
// `coin /= %arg%` is str_str("coin", arg): true when arg occurs in "coin"
// (case-blind). With arg "coin" the `||` is true. oechoaround goes to
// everyone in the carrier's room but the actor, osend to the actor, oecho to
// everyone. `%random.2%` is rand_number(1, 2): case 1 -> heads, case 2 ->
// tails; each case ends with `break`, which jumps past `done`. With arg
// "sword" neither `/=` holds ("sword" is not in "coin" or "quarter"), so the
// else branch sends "flip What?".
// ---------------------------------------------------------------------
fn flip(record: &Record, arg: &str, roll: i64) -> (Scene, i32) {
    let mut scene = Scene::new();
    let (player, flipper) = (scene.player(), scene.fx.host.add_obj(3500, "a quarter"));
    scene.host().obj_to_char(flipper, player);
    let id = install_record(scene.host(), flipper, record);
    scene.host().rolls.push_back(roll);
    let ret = fire(
        scene.host(),
        flipper,
        id,
        &[
            ("actor", uid(player)),
            ("arg", arg.to_owned()),
            ("cmd", "flip".to_owned()),
        ],
    );
    (scene, ret)
}

#[test]
fn trigger_37_flips_a_coin_to_heads_or_tails() {
    for (roll, face) in [(1, "heads"), (2, "tails")] {
        let record = real!(37);
        let (scene, ret) = flip(&record, "coin", roll);
        assert_eq!(ret, 1);
        let landed =
            format!("The coin falls to the ground, bounces, rolls, and ends up showing {face}!");
        assert_eq!(
            heard(&scene.fx.host, scene.player()),
            vec![
                "You flip the coin up into the air.".to_owned(),
                landed.clone()
            ]
        );
        assert_eq!(
            heard(&scene.fx.host, scene.watcher),
            vec![
                "Tester flips a coin high up into the air.".to_owned(),
                landed
            ]
        );
        assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
        assert_clean(&scene.fx.host);
    }
    // A prefix of "quarter" also matches.
    let (scene, _) = flip(&real!(37), "quar", 1);
    assert_eq!(heard(&scene.fx.host, scene.player()).len(), 2);
}

#[test]
fn trigger_37_refuses_to_flip_anything_else() {
    let record = real!(37);
    let (scene, ret) = flip(&record, "sword", 1);
    assert_eq!(ret, 1);
    assert_eq!(heard(&scene.fx.host, scene.player()), vec!["flip What?"]);
    assert!(heard(&scene.fx.host, scene.watcher).is_empty());
    assert!(scene.fx.host.rolls.len() == 1, "no roll is taken");
}

// ---------------------------------------------------------------------
// Trigger 45, object random `1 b 100`: a shotgun with two rounds.
//
// The object is wielded by Tester, who is fighting the guard. `set actor
// %self.worn_by%` makes a local of the wearer's id, `%actor.fighting%` is the
// guard's id (true). `eval shotgun_rounds %shotgun_rounds% + 1`: the variable
// is unset, so the line is "eval shotgun_rounds  + 1" and the expression
// " + 1" is lhs "" + rhs 1 = 1 (atoi("") is 0). `global` moves it to the
// object's script (context 0). 1 > 2 is false, so: oechoaround to the room
// but the wearer, osend to the wearer, then odamage the guard 10 (hit 60 ->
// 50; 10 is not above max_hit >> 2 = 15 and 50 is not below it, so no pain
// messages). The second shot leaves 40 and rounds 2 (2 > 2 is false). The
// third firing makes 3 > 2: `detach 45 %self.id%` removes the trigger (the
// object's script goes with its last trigger) and `halt` stops the script.
// ---------------------------------------------------------------------
#[test]
fn trigger_45_shotgun_fires_twice_then_detaches() {
    let record = real!(45);
    let mut scene = Scene::new();
    let (player, guard, watcher) = (scene.player(), scene.guard(), scene.watcher);
    let gun = scene.fx.host.add_obj(3501, "a shotgun");
    scene.host().equip_char(player, gun, 16);
    scene.host().set_link(player, "fighting", Some(guard));
    let id = install_record(scene.host(), gun, &record);

    for (shot, hit) in [(1, 50), (2, 40)] {
        scene.host().effects.clear();
        assert_eq!(fire(scene.host(), gun, id, &[]), 1);
        assert_eq!(scene.fx.host.num(guard, "hit"), hit);
        assert_eq!(
            heard(&scene.fx.host, player),
            vec!["You point a shotgun at a city guard and pull the trigger."]
        );
        assert_eq!(
            heard(&scene.fx.host, watcher),
            vec!["Tester points a shotgun at a city guard and pulls the trigger."]
        );
        assert_eq!(
            global_var(&scene.fx.host, gun, "shotgun_rounds"),
            Some(shot.to_string())
        );
        assert_clean(&scene.fx.host);
    }

    scene.host().effects.clear();
    assert_eq!(fire(scene.host(), gun, id, &[]), 1);
    assert!(scene.fx.host.effects.is_empty());
    assert_eq!(scene.fx.host.num(guard, "hit"), 40);
    // The only trigger is gone and so is the script with its variable.
    assert!(
        scene
            .fx
            .host
            .script(gun)
            .is_none_or(|script| script.trigger(id).is_none())
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 3010, mob random `0 b 100`: the janitor-dog eats a corpse.
//
// `set inroom %self.room%`, `set item %inroom.contents%`: the room's first
// object. The loop saves `next_item` (the next in the list) before testing
// `%item.vnum(65535)%` (1 only when the vnum matches). Contents are oldest
// first: bench (3020), corpse (65535). The bench is skipped; the corpse
// makes the mob `emote` (a game command, so mud_command), `mpurge` it
// (extract_obj at once) and `halt`.
// ---------------------------------------------------------------------
#[test]
fn trigger_3010_devours_a_corpse_in_the_room() {
    let record = real!(3010);
    let mut scene = Scene::new();
    let mob = scene.add_owner_mob(3062, "a fido");
    let bench = scene.fx.bench;
    let corpse = scene.add_room_obj(65535, "the corpse of a rat");
    let id = install_record(scene.host(), mob, &record);
    assert_eq!(fire(scene.host(), mob, id, &[]), 1);
    assert_eq!(
        said(&scene.fx.host),
        vec![(mob, "emote savagely devours a corpse.".to_owned())]
    );
    assert_eq!(scene.fx.host.effects, vec![format!("extract_obj {corpse}")]);
    assert_eq!(scene.fx.host.list(scene.room(), "contents"), vec![bench]);
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// The same walk with nothing to eat: every item is skipped, the loop ends
// when `next_item` is empty, and the mob does nothing.
#[test]
fn trigger_3010_ignores_a_room_without_a_corpse() {
    let record = real!(3010);
    let mut scene = Scene::new();
    let mob = scene.add_owner_mob(3062, "a fido");
    scene.add_room_obj(3021, "a dagger");
    let id = install_record(scene.host(), mob, &record);
    assert_eq!(fire(scene.host(), mob, id, &[]), 1);
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.effects.is_empty());
    assert_eq!(scene.fx.host.list(scene.room(), "contents").len(), 2);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 1376, object command `testing`: walking a container.
//
// `%self.contents%` is the head of the object's contents; obj_to_obj puts a
// new object at the head, so after putting 11 and then 12 inside the walk
// sees 12 then 11. `eval in_bag ...` copies the id, `set next_in_bag
// %in_bag.next_in_list%` is taken before the echo (oecho, to everyone in the
// object's room).
// ---------------------------------------------------------------------
#[test]
fn trigger_1376_lists_what_a_container_holds() {
    let record = real!(1376);
    let mut scene = Scene::new();
    let (player, watcher) = (scene.player(), scene.watcher);
    let bag = scene.add_room_obj(3600, "a bag");
    for vnum in [11, 12] {
        let item = scene.fx.host.add_obj(vnum, "a coin");
        scene.host().obj_to_obj(item, bag);
    }
    let id = install_record(scene.host(), bag, &record);
    let ret = fire(
        scene.host(),
        bag,
        id,
        &[("actor", uid(player)), ("cmd", "testing".to_owned())],
    );
    assert_eq!(ret, 1);
    let expected = vec!["firing", "contains: 12", "contains: 11"];
    assert_eq!(heard(&scene.fx.host, player), expected);
    assert_eq!(heard(&scene.fx.host, watcher), expected);
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 189, room enter `2 g 100`: load the eight ball once, then wait.
//
// `%self.contents.vnum%` is the vnum of the first object in the room. With
// the bench (3020) first, `3020 != 47` is numeric and true, so `wload obj
// 47` puts a new object at the end of the room. `wait 2 sec` is 20 pulses.
// After the wait: wechoaround (everybody but the actor), wsend, and `wdamage
// %actor% %random.5%` = rand_number(1, 5), here 4: hit 100 -> 96 (4 is not
// above 100 >> 2). With a 47 already first, the condition is false and
// nothing is loaded.
// ---------------------------------------------------------------------
fn eight_ball_scene(record: &Record, first_is_ball: bool) -> (Scene, Uid, u64) {
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    scene
        .host()
        .globals
        .texts
        .insert("objproto:47".to_owned(), "a magic eight ball".to_owned());
    if first_is_ball {
        let bench = scene.fx.bench;
        scene.host().extract_obj(bench);
        scene.add_room_obj(47, "ball");
    }
    let id = install_record(scene.host(), room, record);
    scene.host().rolls.push_back(4);
    let ret = fire(scene.host(), room, id, &[("actor", uid(player))]);
    assert_eq!(ret, 1);
    assert_eq!(scene.fx.host.waits, vec![(room, id, 20)]);
    (scene, room, id)
}

#[test]
fn trigger_189_loads_the_ball_and_drops_it_on_the_newcomer() {
    let (mut scene, room, _) = eight_ball_scene(&real!(189), false);
    let (player, watcher) = (scene.player(), scene.watcher);
    let contents = scene.fx.host.list(room, "contents");
    assert_eq!(vnums(&scene.fx.host, &contents), vec![3020, 47]);
    assert!(heard(&scene.fx.host, player).is_empty());

    assert_eq!(run_waits(scene.host(), 5), 1);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["A magic eight ball drops from the sky striking you on the head."]
    );
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec!["A magic eight ball drops from the sky striking Tester on the head."]
    );
    assert_eq!(scene.fx.host.num(player, "hit"), 96);
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

#[test]
fn trigger_189_does_not_load_a_second_ball() {
    let (mut scene, room, _) = eight_ball_scene(&real!(189), true);
    let contents = scene.fx.host.list(room, "contents");
    assert_eq!(vnums(&scene.fx.host, &contents), vec![47]);
    run_waits(scene.host(), 5);
    assert_eq!(scene.fx.host.num(scene.player(), "hit"), 96);
    assert_eq!(scene.fx.host.list(room, "contents").len(), 1);
    assert!(scene.fx.host.logs.is_empty());
}

// ---------------------------------------------------------------------
// Trigger 12, room enter `2 g 100`: `%load% obj` in a room.
//
// `wait 1 sec` (10 pulses), then wechoaround, wsend, `wload obj 1394` (the
// object lands in the room), `wdamage %actor% 3`: hit 100 -> 97.
// ---------------------------------------------------------------------
#[test]
fn trigger_12_drops_the_ball_on_the_head() {
    let record = real!(12);
    let mut scene = Scene::new();
    let (room, player, watcher) = (scene.room(), scene.player(), scene.watcher);
    scene
        .host()
        .globals
        .texts
        .insert("objproto:1394".to_owned(), "a magic eight ball".to_owned());
    let id = install_record(scene.host(), room, &record);
    scene.host().rolls.push_back(3);
    assert_eq!(fire(scene.host(), room, id, &[("actor", uid(player))]), 1);
    assert_eq!(scene.fx.host.waits, vec![(room, id, 10)]);
    run_waits(scene.host(), 5);
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec!["A magic eight ball drops from the sky striking Tester on the head."]
    );
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["A magic eight ball drops from the sky striking you on the head."]
    );
    let contents = scene.fx.host.list(room, "contents");
    assert_eq!(vnums(&scene.fx.host, &contents), vec![3020, 1394]);
    assert_eq!(scene.fx.host.num(player, "hit"), 97);
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 21, mob greet `0 g 100`: transform into a random mob.
//
// `emote` is a game command. `eval number %random.100% + 99`: with a roll of
// 7 that is 106. `%transform% 106` is `mtransform 106`: a digit argument
// keeps the hit points (keep_hp), the mob takes the prototype's strings but
// keeps its vnum. Then `say` with the number.
// ---------------------------------------------------------------------
#[test]
fn trigger_21_transforms_into_the_rolled_mob() {
    let record = real!(21);
    let mut scene = Scene::new();
    let mob = scene.add_owner_mob(7000, "a changeling");
    let player = scene.player();
    scene
        .host()
        .globals
        .texts
        .insert("mobproto:106".to_owned(), "a hill giant".to_owned());
    let id = install_record(scene.host(), mob, &record);
    scene.host().rolls.push_back(7);
    assert_eq!(fire(scene.host(), mob, id, &[("actor", uid(player))]), 1);
    assert_eq!(
        commands(&scene.fx.host),
        vec![
            "emote starts to twist and contort into something different.",
            "say Transform complete to mob 106.",
        ]
    );
    assert_eq!(
        scene.fx.host.effects[0],
        format!("transform_mob {mob} 106 true")
    );
    assert_eq!(scene.fx.host.text(mob, "name"), "a hill giant");
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 24, room `bg` (global, random): the lightning bolt loop.
//
// `eval person %self.people%` is the room's first person; `wait 1 sec`
// (10 pulses). The loop: wsend, wecho, wechoaround, `wteleport %person% 3`,
// `wforce %person% look` (a game command run as that person), then the next
// first person. People are head first: Watcher, the guard, Tester, so the
// victims go in that order and each wforce runs in room 3 (the command log
// notes who ran it). wecho is act() to the room and then to the first
// person: whoever is still in the room hears it, the guard has no
// descriptor, so each player hears the echo while present. do_wforce hands
// command_interpreter what one_argument leaves after the target, the line
// with its leading space (the interpreter skips it), so the log shows " look".
// ---------------------------------------------------------------------
#[test]
fn trigger_24_strikes_everyone_with_lightning_in_turn() {
    let record = real!(24);
    let mut scene = Scene::new();
    let (room, player, watcher, guard) =
        (scene.room(), scene.player(), scene.watcher, scene.guard());
    scene.host().add_room(3, "The Void");
    let id = install_record(scene.host(), room, &record);
    assert_eq!(fire(scene.host(), room, id, &[]), 1);
    assert_eq!(scene.fx.host.waits, vec![(room, id, 10)]);
    assert_eq!(
        scene.fx.host.list(room, "people"),
        vec![watcher, guard, player]
    );

    assert_eq!(run_waits(scene.host(), 5), 1);
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec!["You are next!", "I am targetting Watcher."]
    );
    assert_eq!(
        heard(&scene.fx.host, player),
        vec![
            "I am targetting Watcher.",
            "Watcher is struck by a bolt of lightning. Leaving only a pile of ash.",
            "I am targetting a city guard.",
            "a city guard is struck by a bolt of lightning. Leaving only a pile of ash.",
            "You are next!",
            "I am targetting Tester.",
        ]
    );
    assert_eq!(
        said(&scene.fx.host),
        vec![
            (watcher, " look".to_owned()),
            (guard, " look".to_owned()),
            (player, " look".to_owned()),
        ]
    );
    assert!(scene.fx.host.list(room, "people").is_empty());
    let void = dg_vm::types::ROOM_ID_BASE + 3;
    assert_eq!(scene.fx.host.list(void, "people").len(), 3);
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 25, room command `l`: `.mudcommand`, `/=` and `return 0`.
//
// `%cmd.mudcommand%` is the first entry of the command table that `l`
// abbreviates: "look". `window /= %arg%` holds when arg occurs in "window".
// Both true: `wecho` of "<actor> typed <cmd> to <mudcommand> at the <arg>."
// and the trigger returns 1. Anything else takes the else branch, `return 0`
// (let the command go on) without a word.
// ---------------------------------------------------------------------
#[test]
fn trigger_25_catches_looking_at_the_window() {
    let record = real!(25);
    let mut scene = Scene::new();
    let (room, player, watcher) = (scene.room(), scene.player(), scene.watcher);
    let id = install_record(scene.host(), room, &record);
    let vars = [
        ("actor", uid(player)),
        ("cmd", "l".to_owned()),
        ("arg", "window".to_owned()),
    ];
    assert_eq!(fire(scene.host(), room, id, &vars), 1);
    let expected = vec!["Tester typed l to look at the window."];
    assert_eq!(heard(&scene.fx.host, player), expected);
    assert_eq!(heard(&scene.fx.host, watcher), expected);
    assert_clean(&scene.fx.host);
}

#[test]
fn trigger_25_lets_every_other_command_through() {
    let record = real!(25);
    for (cmd, arg) in [("l", "table"), ("say", "window"), ("l", "")] {
        let mut scene = Scene::new();
        let (room, player) = (scene.room(), scene.player());
        let id = install_record(scene.host(), room, &record);
        let vars = [
            ("actor", uid(player)),
            ("cmd", cmd.to_owned()),
            ("arg", arg.to_owned()),
        ];
        assert_eq!(fire(scene.host(), room, id, &vars), 0, "{cmd} {arg}");
        assert!(scene.fx.host.effects.is_empty(), "{cmd} {arg}");
    }
}

// ---------------------------------------------------------------------
// Trigger 26, room enter `2 g 100`: keep mobs out unless they follow.
//
// The body starts with four lines of descriptive text that are not script
// commands; each reaches wld_command_interpreter, which logs "Room N ::
// Unknown world cmd: '<line>'" (wld_log prefixes the room number). Then
// `!%actor.is_pc%` is true for a mob; `!%actor.master%` is true when it
// follows nobody, and `return 0` blocks it. A follower, or a player, passes
// (the default return of 1).
// ---------------------------------------------------------------------
fn blocks_mobs(record: &Record, setup: impl FnOnce(&mut Scene) -> Uid) -> (Scene, i32) {
    let mut scene = Scene::new();
    let room = scene.room();
    let actor = setup(&mut scene);
    let id = install_record(scene.host(), room, record);
    let ret = fire(scene.host(), room, id, &[("actor", uid(actor))]);
    (scene, ret)
}

#[test]
fn trigger_26_blocks_a_mob_that_follows_nobody() {
    let (scene, ret) = blocks_mobs(&real!(26), |scene| scene.guard());
    assert_eq!(ret, 0);
    assert_eq!(
        scene.fx.host.logs,
        vec![
            "Room 3001 :: Unknown world cmd: 'Name: 'Blocks Mobs Not Following',  VNum: [   26], RNum: [   26]'",
            "Room 3001 :: Unknown world cmd: 'Trigger Intended Assignment: Rooms'",
            "Room 3001 :: Unknown world cmd: 'Trigger Type: Enter , Numeric Arg: 100, Arg list: None'",
            "Room 3001 :: Unknown world cmd: 'Commands:'",
        ]
    );
}

#[test]
fn trigger_26_lets_followers_and_players_in() {
    let (scene, ret) = blocks_mobs(&real!(26), |scene| {
        let (guard, player) = (scene.guard(), scene.player());
        scene.host().set_link(guard, "master", Some(player));
        guard
    });
    assert_eq!(ret, 1);
    assert_eq!(scene.fx.host.logs.len(), 4);
    let (scene, ret) = blocks_mobs(&real!(26), |scene| scene.player());
    assert_eq!(ret, 1);
    assert_eq!(scene.fx.host.logs.len(), 4);
}

// ---------------------------------------------------------------------
// Trigger 34, room command `target`: is_pc on a name, elseif chains.
//
// `arg` is plain text. A field on a plain-text variable looks the text up as
// a name: in a room trigger get_char_by_room, then get_obj_by_room. So
// "guard" is the mob (is_pc 0), "Watcher" a player (1), "bench" an object.
// An object answers is_pc with "-1" (dg_variables.c), which equals neither
// 1 nor 0, so the else branch says it is an object.
// ---------------------------------------------------------------------
fn target(record: &Record, arg: &str) -> Scene {
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    let id = install_record(scene.host(), room, record);
    let vars = [("actor", uid(player)), ("arg", arg.to_owned())];
    assert_eq!(fire(scene.host(), room, id, &vars), 1);
    scene
}

#[test]
fn trigger_34_tells_players_mobs_and_objects_apart() {
    let record = real!(34);
    for (arg, flag, verdict) in [
        ("guard", "0", "It is a mob."),
        ("Watcher", "1", "It is a player."),
    ] {
        let scene = target(&record, arg);
        assert_eq!(
            heard(&scene.fx.host, scene.player()),
            vec![
                format!("Tester is targetting {arg} IS_PC: {flag}"),
                verdict.to_owned()
            ]
        );
        assert!(scene.fx.host.logs.is_empty());
    }
    let scene = target(&real!(34), "bench");
    assert_eq!(
        heard(&scene.fx.host, scene.player()),
        vec!["Tester is targetting bench IS_PC: -1", "It is an object."]
    );
    assert!(scene.fx.host.logs.is_empty());
}

// ---------------------------------------------------------------------
// Trigger 55, room enter: if / elseif / else on `%actor.sex%`.
//
// `%actor.sex%` is the word from sexes[]: "neutral", "male", "female". A
// male actor takes the first branch: `return 0` first, then wsend and
// wechoaround (the return value is only set, the script goes on). A female
// takes the elseif: wecho. Anything else ends in the else branch (the text
// after `else` on its line is ignored). A non-player skips everything.
// ---------------------------------------------------------------------
fn barrier(record: &Record, sex: i64) -> (Scene, i32) {
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    scene.host().set_num(player, "sex", sex);
    let id = install_record(scene.host(), room, record);
    let ret = fire(scene.host(), room, id, &[("actor", uid(player))]);
    (scene, ret)
}

#[test]
fn trigger_55_bars_men_and_neutrals_and_welcomes_women() {
    let record = real!(55);
    let (scene, ret) = barrier(&record, 1);
    assert_eq!(ret, 0);
    assert_eq!(
        heard(&scene.fx.host, scene.player()),
        vec!["An invisible barrier blocks you. A voice rings out \"No men allowed.\""]
    );
    assert_eq!(
        heard(&scene.fx.host, scene.watcher),
        vec!["Tester walks into an invisible barrier and a voice booms \"No men allowed.\""]
    );

    let (scene, ret) = barrier(&record, 0);
    assert_eq!(ret, 0);
    assert_eq!(
        heard(&scene.fx.host, scene.player()),
        vec!["An invisible barrier blocks you. A voice rings out \"No... things allowed.\""]
    );
    assert_eq!(
        heard(&scene.fx.host, scene.watcher),
        vec!["Tester walks into an invisible barrier and a voice booms \"No... things allowed.\""]
    );

    let (scene, ret) = barrier(&record, 2);
    assert_eq!(ret, 1);
    assert_eq!(
        heard(&scene.fx.host, scene.player()),
        vec!["Ladies are always welcome!"]
    );
    assert_eq!(
        heard(&scene.fx.host, scene.watcher),
        vec!["Ladies are always welcome!"]
    );
}

// ---------------------------------------------------------------------
// Trigger 59, room door `2 r 100`: detach self, damage, return 0.
//
// `%cmd% == open` holds. wechoaround (the actor's text uses
// `%actor.hisher%` = "his" for a male), two wsends, `wdamage %actor% 1`
// (hit 100 -> 99), wecho, then `detach 59 %self.id%` takes the trigger off
// the room; the running instance carries on and `return 0` still counts, so
// the door stays shut. Another command does nothing and returns 1.
// ---------------------------------------------------------------------
#[test]
fn trigger_59_drenches_the_first_opener_once() {
    let record = real!(59);
    let mut scene = Scene::new();
    let (room, player, watcher) = (scene.room(), scene.player(), scene.watcher);
    let id = install_record(scene.host(), room, &record);
    let vars = [
        ("actor", uid(player)),
        ("cmd", "open".to_owned()),
        ("direction", "north".to_owned()),
    ];
    assert_eq!(fire(scene.host(), room, id, &vars), 0);
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec![
            "As Tester tries to open the door to the north a bucket of water dumps on his head.",
            "The door slams shut again.",
        ]
    );
    assert_eq!(
        heard(&scene.fx.host, player),
        vec![
            "Splash!!",
            "A bucket of water drops on top of your head as you open the door.",
            "The door slams shut again.",
        ]
    );
    assert_eq!(scene.fx.host.num(player, "hit"), 99);
    assert!(
        scene
            .fx
            .host
            .script(room)
            .is_none_or(|script| script.trigger(id).is_none()),
        "the trigger is detached"
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

#[test]
fn trigger_59_ignores_other_door_commands() {
    let record = real!(59);
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    let id = install_record(scene.host(), room, &record);
    let vars = [
        ("actor", uid(player)),
        ("cmd", "close".to_owned()),
        ("direction", "north".to_owned()),
    ];
    assert_eq!(fire(scene.host(), room, id, &vars), 1);
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.script(room).unwrap().trigger(id).is_some());
    assert!(other_effects(&scene.fx.host).is_empty());
}
