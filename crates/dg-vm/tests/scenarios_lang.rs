//! Scenario tests on real tbaMUD triggers chosen to cover the language:
//! control flow, variables, text functions, entity walks, the command
//! families of mobs, objects and rooms.
//!
//! Every expectation is worked out by hand from the C (`dg_scripts.c`,
//! `dg_variables.c`, `dg_mobcmd.c`, `dg_objcmd.c`, `dg_wldcmd.c`,
//! `dg_misc.c`, `dg_comm.c`) for the world and event set up in the test, not
//! read back from the VM. The reasoning is in the comment above each test.
//! Each event is fired through the dispatch function the game calls for it
//! (`dg_triggers.c`), so the type tests, the percent roll and the event
//! variables are the ones of the real path.
//!
//! The standard cast (see `common::scene`): room 3001 holds "Tester" (male
//! player, level 10), "Watcher" (a second player), the guard mob "a city
//! guard" (no descriptor) and a bench. Only characters with a descriptor
//! receive text.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::corpus::Record;
use common::scene::{
    Scene, act_line, assert_clean, echo_all, heard, heard_raw, other_effects, said, sub_line, vnums,
};
use common::world::{
    PERCENT_ROLL, commands, global_var, install, install_record, local_var, run_waits,
};
use dg_vm::dispatch;
use dg_vm::driver::resume;
use dg_vm::host::{BaseHost, WorldHost};
use dg_vm::mock::MockHost;
use dg_vm::types::{AttachType, Uid};

/// Resume the pending waits and return those queued next.
fn step(host: &mut MockHost) -> Vec<(Uid, u64, i64)> {
    let due = std::mem::take(&mut host.waits);
    for (owner, id, _) in &due {
        resume(host, *owner, *id);
    }
    host.waits.clone()
}

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
    install_record(scene.host(), room, &record);
    dispatch::speech_wtrigger(scene.host(), player, "go north now");
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
    // Every echo goes to Tester first and to the head of the room, Watcher, last.
    assert_eq!(
        scene.fx.host.effects,
        echo_all(&scene.fx.host, room, &expected, "\r\n")
    );
    assert!(scene.fx.host.mud_commands.is_empty());
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
    install_record(scene.host(), flipper, record);
    scene.host().rolls.push_back(roll);
    let ret = dispatch::command_otrigger(scene.host(), player, "flip", &format!(" {arg}"));
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
                landed.clone()
            ]
        );
        // oechoaround (all but the flipper), osend, then oecho (the whole room).
        let (player, watcher) = (scene.player(), scene.watcher);
        let mut effects = vec![
            sub_line(watcher, "Tester flips a coin high up into the air."),
            sub_line(player, "You flip the coin up into the air."),
        ];
        effects.extend(echo_all(&scene.fx.host, scene.room(), &[&landed], "\n\r"));
        assert_eq!(scene.fx.host.effects, effects);
        assert!(scene.fx.host.rolls.is_empty());
        assert!(scene.fx.host.mud_commands.is_empty());
        assert!(scene.fx.host.waits.is_empty());
        assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
        assert_clean(&scene.fx.host);
    }
    // A prefix of "quarter" also matches.
    let (scene, ret) = flip(&real!(37), "quar", 1);
    assert_eq!(ret, 1);
    assert_eq!(heard(&scene.fx.host, scene.player()).len(), 2);
}

#[test]
fn trigger_37_refuses_to_flip_anything_else() {
    let record = real!(37);
    let (scene, ret) = flip(&record, "sword", 1);
    assert_eq!(ret, 1);
    assert_eq!(
        scene.fx.host.effects,
        vec![sub_line(scene.player(), "flip What?")]
    );
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
    install_record(scene.host(), gun, &record);

    for (shot, hit) in [(1, 50), (2, 40)] {
        scene.host().effects.clear();
        dispatch::script_trigger_check(scene.host());
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
        // oechoaround, osend; odamage deals 10 without a pain message.
        assert_eq!(
            scene.fx.host.effects,
            vec![
                sub_line(
                    watcher,
                    "Tester points a shotgun at a city guard and pulls the trigger."
                ),
                sub_line(
                    player,
                    "You point a shotgun at a city guard and pull the trigger."
                ),
            ]
        );
        assert!(scene.fx.host.mud_commands.is_empty());
        assert!(scene.fx.host.waits.is_empty());
        assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
        assert_clean(&scene.fx.host);
    }

    scene.host().effects.clear();
    dispatch::script_trigger_check(scene.host());
    assert!(scene.fx.host.effects.is_empty());
    assert_eq!(scene.fx.host.num(guard, "hit"), 40);
    // The only trigger is gone and so is the script with its variable.
    // process_detach: `if (!TRIGGERS(SCRIPT(o))) extract_script(o, OBJ_TRIGGER)`.
    assert!(scene.fx.host.script(gun).is_none());
    assert_eq!(global_var(&scene.fx.host, gun, "shotgun_rounds"), None);
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
    install_record(scene.host(), mob, &record);
    dispatch::script_trigger_check(scene.host());
    assert_eq!(
        said(&scene.fx.host),
        vec![(mob, "emote savagely devours a corpse.".to_owned())]
    );
    assert_eq!(scene.fx.host.effects, vec![format!("extract_obj {corpse}")]);
    assert_eq!(scene.fx.host.list(scene.room(), "contents"), vec![bench]);
    assert!(scene.fx.host.waits.is_empty());
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
    install_record(scene.host(), mob, &record);
    dispatch::script_trigger_check(scene.host());
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
    install_record(scene.host(), bag, &record);
    let ret = dispatch::command_otrigger(scene.host(), player, "testing", "");
    assert_eq!(ret, 1);
    let expected = vec!["firing", "contains: 12", "contains: 11"];
    assert_eq!(heard(&scene.fx.host, player), expected);
    assert_eq!(heard(&scene.fx.host, watcher), expected);
    assert_eq!(
        scene.fx.host.effects,
        echo_all(&scene.fx.host, scene.room(), &expected, "\n\r")
    );
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// Trigger 1376 once more with the line ends as written. `oecho` is two
// `sub_write` calls (dg_objcmd.c do_oecho), and sub_write_to_char ends every
// message in "\n\r" (dg_comm.c).
#[test]
fn oecho_lines_end_in_newline_return() {
    let record = real!(1376);
    let mut scene = Scene::new();
    let (player, watcher) = (scene.player(), scene.watcher);
    let bag = scene.add_room_obj(3600, "a bag");
    install_record(scene.host(), bag, &record);
    dispatch::command_otrigger(scene.host(), player, "testing", "");
    assert_eq!(heard_raw(&scene.fx.host, player), vec!["firing\n\r"]);
    assert_eq!(heard_raw(&scene.fx.host, watcher), vec!["firing\n\r"]);
    assert_eq!(
        scene.fx.host.effects,
        vec![sub_line(player, "firing"), sub_line(watcher, "firing")]
    );
}

// Trigger 53 with the line ends as written. wecho is act_to_room(), whose
// act() calls end the line in "\r\n" (comm.c perform_act), the opposite of
// sub_write.
#[test]
fn wecho_lines_end_in_return_newline() {
    let record = real!(53);
    let mut scene = Scene::new();
    let (room, player, watcher) = (scene.room(), scene.player(), scene.watcher);
    install_record(scene.host(), room, &record);
    dispatch::speech_wtrigger(scene.host(), player, "go");
    // `go`: car "go", cdr ""; then the loop echoes "go" and "" once more.
    let expected = vec![
        "The first word is: go\r\n",
        "The rest of the string is: \r\n",
        "The first word is: go\r\n",
        "The remaining text is: \r\n",
    ];
    assert_eq!(heard_raw(&scene.fx.host, player), expected);
    assert_eq!(heard_raw(&scene.fx.host, watcher), expected);
    // act_to_room: `act(.., TO_ROOM)` reaches everyone but the head of
    // room->people (Watcher) first, then `act(.., TO_CHAR)` reaches the head.
    let mut effects = Vec::new();
    for text in [
        "The first word is: go",
        "The rest of the string is: ",
        "The first word is: go",
        "The remaining text is: ",
    ] {
        effects.push(act_line(player, text));
        effects.push(act_line(watcher, text));
    }
    assert_eq!(scene.fx.host.effects, effects);
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
    scene.host().rolls.push_back(PERCENT_ROLL);
    scene.host().rolls.push_back(4);
    let ret = dispatch::enter_wtrigger(scene.host(), room, player, 0);
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
    // wechoaround (all but the actor), wsend; wdamage 4 is too small to hurt.
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                scene.watcher,
                "A magic eight ball drops from the sky striking Tester on the head."
            ),
            sub_line(
                player,
                "A magic eight ball drops from the sky striking you on the head."
            ),
        ]
    );
    assert!(scene.fx.host.waits.is_empty());
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
    assert_eq!(scene.fx.host.effects.len(), 2);
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.logs.is_empty());
}

// ---------------------------------------------------------------------
// Trigger 12, room enter `2 g 100`: `%load% obj` in a room.
//
// `wait 1 sec` (10 pulses), then wechoaround, wsend, `wload obj 1394` (the
// object lands in the room), `%damage% %actor% %random.5%` (wdamage) with the roll 3 the test
// supplies: hit 100 -> 97.
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
    scene.host().rolls.push_back(PERCENT_ROLL);
    scene.host().rolls.push_back(3);
    assert_eq!(dispatch::enter_wtrigger(scene.host(), room, player, 0), 1);
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
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                watcher,
                "A magic eight ball drops from the sky striking Tester on the head."
            ),
            sub_line(
                player,
                "A magic eight ball drops from the sky striking you on the head."
            ),
        ]
    );
    assert!(scene.fx.host.waits.is_empty());
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
    install_record(scene.host(), mob, &record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    scene.host().rolls.push_back(7);
    assert_eq!(dispatch::greet_mtrigger(scene.host(), player, 0), 1);
    assert_eq!(
        commands(&scene.fx.host),
        vec![
            "emote starts to twist and contort into something different.",
            "say Transform complete to mob 106.",
        ]
    );
    // do_mtransform sends no message: the transform, then `extract_char(m)`
    // of the fresh mob it copied from (never the mob itself).
    let extracted = scene.fx.host.effects.get(1).cloned().unwrap_or_default();
    let temp: Uid = extracted
        .strip_prefix("extract_char ")
        .and_then(|text| text.parse().ok())
        .expect("the temporary mob is extracted");
    assert_ne!(temp, mob);
    assert_eq!(
        scene.fx.host.effects,
        vec![
            format!("transform_mob {mob} 106 true"),
            format!("extract_char {temp}"),
        ]
    );
    assert!(scene.fx.host.is_dead(temp));
    assert_eq!(scene.fx.host.text(mob, "name"), "a hill giant");
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.rolls.is_empty());
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
    dispatch::script_trigger_check(scene.host());
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
    // Each line goes to the room as it is when the line runs: Watcher's turn
    // (wsend "You are next!", then the echo, then the echoaround for Tester),
    // the guard's turn (echo and echoaround for Tester only: the guard has no
    // descriptor), and Tester's own turn.
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(watcher, "You are next!"),
            act_line(player, "I am targetting Watcher."),
            act_line(watcher, "I am targetting Watcher."),
            sub_line(
                player,
                "Watcher is struck by a bolt of lightning. Leaving only a pile of ash."
            ),
            act_line(player, "I am targetting a city guard."),
            sub_line(
                player,
                "a city guard is struck by a bolt of lightning. Leaving only a pile of ash."
            ),
            sub_line(player, "You are next!"),
            act_line(player, "I am targetting Tester."),
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
    install_record(scene.host(), room, &record);
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "l", " window"),
        1
    );
    let expected = vec!["Tester typed l to look at the window."];
    assert_eq!(heard(&scene.fx.host, player), expected);
    assert_eq!(heard(&scene.fx.host, watcher), expected);
    assert_eq!(
        scene.fx.host.effects,
        vec![
            act_line(player, "Tester typed l to look at the window."),
            act_line(watcher, "Tester typed l to look at the window."),
        ]
    );
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

#[test]
fn trigger_25_lets_every_other_command_through() {
    let record = real!(25);
    for (cmd, arg) in [("l", "table"), ("say", "window"), ("l", "")] {
        let mut scene = Scene::new();
        let (room, player) = (scene.room(), scene.player());
        install_record(scene.host(), room, &record);
        let argument = format!(" {arg}");
        assert_eq!(
            dispatch::command_wtrigger(scene.host(), player, cmd, &argument),
            0,
            "{cmd} {arg}"
        );
        assert!(scene.fx.host.effects.is_empty(), "{cmd} {arg}");
        assert!(scene.fx.host.mud_commands.is_empty(), "{cmd} {arg}");
        assert!(scene.fx.host.logs.is_empty(), "{cmd} {arg}");
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
/// The four descriptive lines that open trigger 26, each logged as an
/// unknown world command.
const TRIGGER_26_HEADER: [&str; 4] = [
    "Room 3001 :: Unknown world cmd: 'Name: 'Blocks Mobs Not Following',  VNum: [   26], RNum: [   26]'",
    "Room 3001 :: Unknown world cmd: 'Trigger Intended Assignment: Rooms'",
    "Room 3001 :: Unknown world cmd: 'Trigger Type: Enter , Numeric Arg: 100, Arg list: None'",
    "Room 3001 :: Unknown world cmd: 'Commands:'",
];

fn blocks_mobs(record: &Record, setup: impl FnOnce(&mut Scene) -> Uid) -> (Scene, i32) {
    let mut scene = Scene::new();
    let room = scene.room();
    let actor = setup(&mut scene);
    install_record(scene.host(), room, record);
    let ret = dispatch::enter_wtrigger(scene.host(), room, actor, 0);
    (scene, ret)
}

#[test]
fn trigger_26_blocks_a_mob_that_follows_nobody() {
    let (scene, ret) = blocks_mobs(&real!(26), |scene| scene.guard());
    assert_eq!(ret, 0);
    assert_eq!(scene.fx.host.logs, TRIGGER_26_HEADER);
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.waits.is_empty());
}

#[test]
fn trigger_26_lets_followers_and_players_in() {
    let (scene, ret) = blocks_mobs(&real!(26), |scene| {
        let (guard, player) = (scene.guard(), scene.player());
        scene.host().set_link(guard, "master", Some(player));
        guard
    });
    assert_eq!(ret, 1);
    assert_eq!(scene.fx.host.logs, TRIGGER_26_HEADER);
    assert!(scene.fx.host.effects.is_empty());
    let (scene, ret) = blocks_mobs(&real!(26), |scene| scene.player());
    assert_eq!(ret, 1);
    assert_eq!(scene.fx.host.logs, TRIGGER_26_HEADER);
    assert!(scene.fx.host.effects.is_empty());
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
    install_record(scene.host(), room, record);
    let argument = format!(" {arg}");
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "target", &argument),
        1
    );
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
        // wecho: the whole room hears each line.
        assert_eq!(
            scene.fx.host.effects,
            echo_all(
                &scene.fx.host,
                scene.room(),
                &[
                    &format!("Tester is targetting {arg} IS_PC: {flag}"),
                    verdict
                ],
                "\r\n"
            )
        );
        assert!(scene.fx.host.mud_commands.is_empty());
        assert!(scene.fx.host.logs.is_empty());
    }
    let scene = target(&real!(34), "bench");
    assert_eq!(
        heard(&scene.fx.host, scene.player()),
        vec!["Tester is targetting bench IS_PC: -1", "It is an object."]
    );
    assert_eq!(
        scene.fx.host.effects,
        echo_all(
            &scene.fx.host,
            scene.room(),
            &["Tester is targetting bench IS_PC: -1", "It is an object."],
            "\r\n"
        )
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
    install_record(scene.host(), room, record);
    let ret = dispatch::enter_wtrigger(scene.host(), room, player, 0);
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
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                scene.player(),
                "An invisible barrier blocks you. A voice rings out \"No men allowed.\""
            ),
            sub_line(
                scene.watcher,
                "Tester walks into an invisible barrier and a voice booms \"No men allowed.\""
            ),
        ]
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
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                scene.player(),
                "An invisible barrier blocks you. A voice rings out \"No... things allowed.\""
            ),
            sub_line(
                scene.watcher,
                "Tester walks into an invisible barrier and a voice booms \"No... things allowed.\""
            ),
        ]
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
    assert_eq!(
        scene.fx.host.effects,
        vec![
            act_line(scene.player(), "Ladies are always welcome!"),
            act_line(scene.watcher, "Ladies are always welcome!"),
        ]
    );
    assert!(scene.fx.host.logs.is_empty());
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
    install_record(scene.host(), room, &record);
    assert_eq!(dispatch::door_wtrigger(scene.host(), player, 0, 0), 0);
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
    // wechoaround, two wsends, wdamage (1 point: no message), wecho.
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                watcher,
                "As Tester tries to open the door to the north a bucket of water dumps on his head."
            ),
            sub_line(player, "Splash!!"),
            sub_line(
                player,
                "A bucket of water drops on top of your head as you open the door."
            ),
            act_line(player, "The door slams shut again."),
            act_line(watcher, "The door slams shut again."),
        ]
    );
    assert!(
        scene.fx.host.script(room).is_none(),
        "the trigger is detached and the empty script freed"
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
    assert_eq!(dispatch::door_wtrigger(scene.host(), player, 1, 0), 1);
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.script(room).unwrap().trigger(id).is_some());
    assert!(other_effects(&scene.fx.host).is_empty());
}

// ---------------------------------------------------------------------
// Trigger 32, mob greet `0 g 100`: does the actor carry object 1300?
//
// Three worlds. (a) Tester wields vnum 1300 (slot 16): `%actor.eq(wield)%` is
// the object id (find_eq_pos_script("wield") is 16), so the first `if` runs,
// `wep.vnum==1300` sets has_it and the inventory is skipped:
// "say Tester has that special item.". (b) Nothing wanted anywhere: the
// weapon is 3023, `!%has_it%` is `!` over an empty operand (eval_op "!" gives
// `!*rhs` = 1), and the inventory walk takes `eval i %actor.inventory%` (the
// head of ch->carrying: the dagger, since obj_to_char pushes at the head),
// then `set next %i.next_in_list%` (the lantern), neither is vnum 1300, neither
// is a CONTAINER, `set i %next%`; after the lantern next is "" and `while %i%`
// ends; has_it is still unset: "doesnt have". (c) The watcher carries 1300 as
// the LAST of three items: the walk goes item by item and `break` inside the
// `if` finds the loop's `done` (find_done skips nested while/switch blocks
// and returns the first `done`), after which `if %has_it%` says "has that".
// ---------------------------------------------------------------------
fn greeted_by(record: &Record, scene: &mut Scene, mob: Uid, actor: Uid) -> Vec<String> {
    install_record(scene.host(), mob, record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    assert_eq!(dispatch::greet_mtrigger(scene.host(), actor, 0), 1);
    assert_clean(&scene.fx.host);
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.waits.is_empty());
    commands(&scene.fx.host)
}

#[test]
fn trigger_32_finds_the_wielded_item_first() {
    let record = real!(32);
    let mut scene = Scene::new();
    let (player, sword) = (scene.player(), scene.fx.equipment[0]);
    scene.host().set_num(sword, "vnum", 1300);
    let mob = scene.add_owner_mob(3200, "a checker");
    assert_eq!(
        greeted_by(&record, &mut scene, mob, player),
        vec!["say Tester has that special item."]
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
}

#[test]
fn trigger_32_walks_the_whole_inventory_and_finds_nothing() {
    let record = real!(32);
    let mut scene = Scene::new();
    let player = scene.player();
    let mob = scene.add_owner_mob(3200, "a checker");
    assert_eq!(
        greeted_by(&record, &mut scene, mob, player),
        vec!["say Tester doesnt have that crappy item."]
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
}

#[test]
fn trigger_32_breaks_out_of_the_walk_on_the_last_item() {
    let record = real!(32);
    let mut scene = Scene::new();
    let watcher = scene.watcher;
    // obj_to_char pushes at the head: the list is 3042, 3041, 1300.
    for vnum in [1300, 3041, 3042] {
        let item = scene.fx.host.add_obj(vnum, "a thing");
        scene.host().obj_to_char(item, watcher);
    }
    let mob = scene.add_owner_mob(3200, "a checker");
    assert_eq!(
        greeted_by(&record, &mut scene, mob, watcher),
        vec!["say Watcher has that special item."]
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
}

// ---------------------------------------------------------------------
// Trigger 40, room greet `2 g 100`: scatter a player and everything carried.
//
// Rolls (rand_number(1, 99)) in the order the script asks: teleport 7
// (room 20307), the dagger 3 (2303), the lantern 4 (2304), then the equipment
// by ascending slot: the cloak (slot 12) 9 (20309), the sword (slot 16) 2
// (20302). Walk: `wait 1 sec` (10), wsend; `wait 2 sec` (20);
// `eval stunned %actor.hitp% -1` is 100 - 1 = 99; wdamage 99 leaves 1 hit
// point (script_damage: GET_HIT -= dam). wteleport moves Tester to 20307.
// `while %actor.inventory%` takes the head (the dagger; type UNDEFINED, not a
// CONTAINER), `set item_to_purge %actor.inventory%` is its id, `wpurge <id>`
// extracts it and `wat 2303 wload obj 3021` puts a new one in room 2303; again
// for the lantern, which has become the head. The equipment loop reads
// `%actor.eq(%i%)%` with the nested `%i%` for i = 0..17; for slots 12 and 16
// it sends "You drop a cloak" / "You drop a sword" and loads copies into
// 20309 and 20302. `set item_to_purge %%actor.eq(%i%)%%` is the quirk: var_subst
// turns `%%` into a literal `%` and only the inner `%i%` is replaced, so the
// variable holds the TEXT `%actor.eq(12)%`, `wpurge %actor.eq(12)%` matches
// no name in the room and logs "wpurge: bad argument": the worn items are not
// removed. `%force% %actor% look` ends the run: do_wforce's one_argument leaves the
// space in front of the line, so the interpreter gets " look".
// ---------------------------------------------------------------------
#[test]
fn trigger_40_scatters_the_inventory_and_drops_the_equipment_text() {
    let record = real!(40);
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    for vnum in [20307, 20309, 20302, 2303, 2304] {
        scene.host().add_room(vnum, "A Room");
    }
    for vnum in [3021, 3022, 3023, 3024] {
        scene
            .host()
            .globals
            .texts
            .insert(format!("objproto:{vnum}"), "a copy".to_owned());
    }
    scene.host().rolls = [PERCENT_ROLL, 7, 3, 4, 9, 2].into_iter().collect();
    let id = install_record(scene.host(), room, &record);
    assert_eq!(dispatch::enter_wtrigger(scene.host(), room, player, 0), 1);
    assert_eq!(scene.fx.host.waits, vec![(room, id, 10)]);
    assert_eq!(step(scene.host()), vec![(room, id, 20)]);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec![
            "You feel you must not have been worthy when a powerful force hurls you back through the gates."
        ]
    );
    assert!(step(scene.host()).is_empty());

    let host = &scene.fx.host;
    assert_eq!(host.num(player, "hit"), 1);
    assert_eq!(host.in_room(player), host.real_room(20307));
    assert!(host.carrying(player).is_empty());
    let loaded = |vnum: i32| -> Vec<i64> {
        let r = host.real_room(vnum).expect("room");
        vnums(host, &host.list(r, "contents"))
    };
    assert_eq!(loaded(2303), vec![3021]);
    assert_eq!(loaded(2304), vec![3022]);
    assert_eq!(loaded(20309), vec![3024]);
    assert_eq!(loaded(20302), vec![3023]);
    // The worn items were never purged.
    assert_eq!(host.equipment(player, 12), Some(scene.fx.equipment[1]));
    assert_eq!(host.equipment(player, 16), Some(scene.fx.equipment[0]));
    // wsend ends in "\n\r" (sub_write); script_damage's send_char_pos adds
    // "That really did HURT!" (dam 99 > max_hit >> 2, act: "\r\n") and, with
    // 1 hit point left (< 25), the BLEEDING line (no colour for this player).
    // The purge of the inventory extracts the head each time: the dagger,
    // then the lantern. Loading and teleporting log no effect, and nobody but
    // the victim hears anything.
    let [dagger, lantern] = scene.fx.inventory[..] else {
        panic!("the fixture carries two items");
    };
    assert_eq!(
        host.effects,
        vec![
            sub_line(
                player,
                "You feel you must not have been worthy when a powerful force hurls you back through the gates."
            ),
            act_line(player, "That really did HURT!"),
            act_line(
                player,
                "You wish that your wounds would stop BLEEDING so much!"
            ),
            format!("extract_obj {dagger}"),
            format!("extract_obj {lantern}"),
            sub_line(player, "You drop a cloak"),
            sub_line(player, "You drop a sword"),
        ]
    );
    assert!(heard(host, scene.watcher).is_empty());
    assert_eq!(
        host.logs,
        vec![
            "Room 3001 :: wpurge: bad argument",
            "Room 3001 :: wpurge: bad argument"
        ]
    );
    assert_eq!(host.mud_commands, vec![(player, " look".to_owned())]);
    assert!(host.rolls.is_empty());
    assert_clean(host);
}

// ---------------------------------------------------------------------
// Trigger 1396, room speech `2 d 100`: the loop guard of script_driver.
//
// `while %i%<32` runs its body for i = 0..31. At each `done` whose condition
// still holds, `cl->loops++` and on the 30th `cl->loops = 0`,
// `process_wait(.., "wait 1", cl)` and the run returns (ret_val 1). The 30th
// `done` sees i = 30 (the body of pass 30 ended with `eval i %i%+1`), so the
// body ran for i = 0..29 and the echoes are slots 12 (the cloak) and 16 (the
// sword), in slot order. `%echo%` is wecho: act_to_room acts TO_ROOM first
// (everyone but the head of room->people, Watcher: Tester hears it) and then
// TO_CHAR to the head (Watcher), so Tester's line precedes Watcher's. The
// resumed run (curr_state is the line after the `while`) does i = 30 and 31,
// finds nothing in those slots, and at the `done` with i = 32 the condition
// fails and the loop ends.
// ---------------------------------------------------------------------
#[test]
fn trigger_1396_yields_on_the_thirtieth_pass_of_a_loop() {
    let record = real!(1396);
    let mut scene = Scene::new();
    let (room, player, watcher) = (scene.room(), scene.player(), scene.watcher);
    let id = install_record(scene.host(), room, &record);
    dispatch::speech_wtrigger(scene.host(), player, "test");
    let line = |who, text: &str| format!("send {who}: {text}\r\n");
    let echoes = vec![
        line(player, "In slot 12 you are wearing a cloak"),
        line(watcher, "In slot 12 you are wearing a cloak"),
        line(player, "In slot 16 you are wearing a sword"),
        line(watcher, "In slot 16 you are wearing a sword"),
    ];
    assert_eq!(scene.fx.host.effects, echoes);
    assert_eq!(scene.fx.host.waits, vec![(room, id, 1)]);
    assert_eq!(
        local_var(&scene.fx.host, room, id, "i").as_deref(),
        Some("30")
    );

    assert_eq!(run_waits(scene.host(), 5), 1);
    assert_eq!(scene.fx.host.effects, echoes);
    assert!(scene.fx.host.waits.is_empty());
    assert_eq!(local_var(&scene.fx.host, room, id, "i"), None);
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 1134, object command `map` (`1 c 3`): a 45-pass loop and a map.
//
// The first loop sets `x11`..`x55` to "." with `while %display% < 56` from
// 11: 45 passes, 44 of which end in a `done` whose condition still holds.
// The 30th such `done` has cl->loops == 30, so the run yields with `wait 1`
// (cl->loops back to 0, GET_TRIG_LOOPS 30) and `display` is 41: x11..x40 are
// set. The resumed run does the 14 remaining passes (cl->loops 14, no second
// yield) and the `done` with display 56 ends the loop. Then four sections
// read the exits of the room (`%here.north%` and the rest, `here` being the
// room number 3001), each with a three-pass loop over the four directions
// that records the sector of every room one step beyond: GET_TRIG_LOOPS ends
// at 44 + 4 * 3 = 56, under the cap of 100. The sector name's first letter
// goes to x23 / x34 / x43 / x32 (north City, east Field, south Mountains, west
// Hills); the neighbours' only exit leads back to the centre, whose x33 is
// overwritten with X. Five `%send%` lines (osend, "\n\r") show the 5x5 grid.
// ---------------------------------------------------------------------
#[test]
fn trigger_1134_yields_after_thirty_passes_and_draws_the_map() {
    let record = real!(1134);
    let mut scene = Scene::new();
    let (player, room) = (scene.player(), scene.room());
    for (dir, sector) in [(0, 1), (1, 2), (2, 5), (3, 4)] {
        let neighbour = scene.fx.neighbours[dir];
        scene.host().set_num(neighbour, "sector", sector);
    }
    let map = scene.fx.host.add_obj(3510, "a map");
    scene.host().obj_to_char(map, player);
    let id = install_record(scene.host(), map, &record);
    assert_eq!(
        dispatch::command_otrigger(scene.host(), player, "map", ""),
        1
    );
    assert_eq!(scene.fx.host.waits, vec![(map, id, 1)]);
    assert_eq!(
        local_var(&scene.fx.host, map, id, "display").as_deref(),
        Some("41")
    );
    assert_eq!(
        local_var(&scene.fx.host, map, id, "x40").as_deref(),
        Some(".")
    );
    assert_eq!(local_var(&scene.fx.host, map, id, "x41"), None);
    assert!(scene.fx.host.effects.is_empty());

    assert_eq!(run_waits(scene.host(), 5), 1);
    assert!(scene.fx.host.waits.is_empty());
    let rows = [
        ". . . . .",
        ". . C . .",
        ". H X F .",
        ". . M . .",
        ". . . . .",
    ];
    assert_eq!(
        scene.fx.host.effects,
        rows.iter()
            .map(|row| sub_line(player, row))
            .collect::<Vec<_>>()
    );
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert!(scene.fx.host.exists(room));
    assert_clean(&scene.fx.host);
}

// `==` compares non-numbers with str_cmp, `/=` and `.contains()` search with
// str_str: all ignore case (`LOWER`). The room trigger below is offered the
// command `test HeLLo`; `hello == HeLLo` holds, so `!=` does not, `hello /=
// HeLLo` holds (the right side occurs in the left), so does `HeLLo /= ELL`,
// `.contains(HEL)` finds the text and `.contains(xyz)` does not. Numbers
// still compare as numbers when both are all digits (`10 == 010` holds), and
// as text when one is not (`10 == 10.0`: "10" and "10.0" differ).
#[test]
fn comparisons_and_contains_ignore_case() {
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    install(
        scene.host(),
        room,
        9002,
        AttachType::Wld,
        "c",
        100,
        "test",
        "if %arg% == hello\n%send% %actor% eq\nend\n\
         if %arg% != hello\n%send% %actor% ne\nend\n\
         if hello /= %arg%\n%send% %actor% left-has-right\nend\n\
         if %arg% /= ELL\n%send% %actor% has-ell\nend\n\
         if %arg% /= xyz\n%send% %actor% has-xyz\nend\n\
         if %arg.contains(HEL)%\n%send% %actor% contains\nend\n\
         if %arg.contains(xyz)%\n%send% %actor% never\nend\n\
         if 10 == 010\n%send% %actor% numeric\nend\n\
         if 10 == 10.0\n%send% %actor% never-float\nend",
    );
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "test", " HeLLo"),
        1
    );
    assert_eq!(
        scene.fx.host.effects,
        ["eq", "left-has-right", "has-ell", "contains", "numeric"]
            .iter()
            .map(|text| sub_line(player, text))
            .collect::<Vec<_>>()
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// A loop that never ends: `while 1` and a `global n` that records how far it
// got. GET_TRIG_LOOPS counts every pass of the instance's run and is reset
// only by TRIG_NEW; cl->loops is reset at each forced wait. Passes 30, 60 and
// 90 end in `wait 1` (n = 30, 60, 90). Pass 100 leaves cl->loops at 10, so
// the 30 test fails and `GET_TRIG_LOOPS >= 100` logs "Trigger VNum %d has
// looped 100 times!!!" and breaks out of the run: the `wecho` after `done`
// never happens, and nothing is waiting any more.
#[test]
fn a_loop_that_never_ends_is_stopped_at_a_hundred_passes() {
    let mut scene = Scene::new();
    let room = scene.room();
    let id = install(
        scene.host(),
        room,
        9001,
        AttachType::Wld,
        "g",
        100,
        "",
        "set n 0\nwhile 1\neval n %n%+1\nglobal n\ndone\n%echo% finished",
    );
    let player = scene.player();
    assert_eq!(dispatch::enter_wtrigger(scene.host(), room, player, 0), 1);
    for expected in ["30", "60", "90"] {
        assert_eq!(scene.fx.host.waits, vec![(room, id, 1)]);
        assert_eq!(
            global_var(&scene.fx.host, room, "n").as_deref(),
            Some(expected)
        );
        assert!(scene.fx.host.logs.is_empty());
        let due = std::mem::take(&mut scene.host().waits);
        for (owner, id, _) in due {
            resume(scene.host(), owner, id);
        }
    }
    assert!(scene.fx.host.waits.is_empty());
    assert_eq!(
        global_var(&scene.fx.host, room, "n").as_deref(),
        Some("100")
    );
    assert_eq!(
        scene.fx.host.logs,
        vec!["Trigger VNum 9001 has looped 100 times!!!"]
    );
    assert!(scene.fx.host.effects.is_empty());
    assert_clean(&scene.fx.host);
}
