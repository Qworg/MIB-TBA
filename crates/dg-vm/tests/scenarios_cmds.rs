//! Scenario tests on real tbaMUD triggers that exercise the script command
//! families: load, purge, teleport, force, damage, echo, send, at, door, `dg_cast`, `dg_affect`, `attach`, `detach`, `transform`, waits
//! and the variables left behind.
//!
//! Expectations come from walking the C by hand (`dg_scripts.c`,
//! `dg_misc.c`, `dg_mobcmd.c`, `dg_objcmd.c`, `dg_wldcmd.c`, `dg_comm.c`);
//! the reasoning is above each test. See `scenarios_lang.rs` for the cast.
//! Each event is fired through the dispatch function the game calls for it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::corpus::Record;
use common::scene::{
    Scene, act_line, assert_clean, echo_all, echo_lines, heard, heard_raw, other_effects, said,
    sub_line, vnums,
};
use common::world::{PERCENT_ROLL, global_var, install, install_record, make_proto, run_waits};
use dg_vm::dispatch;
use dg_vm::driver::resume;
use dg_vm::host::{BaseHost, WorldHost};
use dg_vm::mock::MockHost;
use dg_vm::types::{AttachType, ROOM_ID_BASE, Uid};

/// `TAR_CHAR_ROOM | TAR_FIGHT_VICT`, the targets of an offensive spell.
const OFFENSIVE: u32 = (1 << 1) | (1 << 4);
/// `MAG_DAMAGE`.
const MAG_DAMAGE: u32 = 1;

fn call_magic(spell: i32, level: i32, caster: Uid, victim: Uid, room: Uid) -> String {
    format!(
        "call_magic spell={spell} level={level} caster={caster} victim={victim} obj=- room={room}"
    )
}

/// One round of combat for `mob`: the fight trigger is offered the blow
/// (`fight_mtrigger`, which wins its percent roll).
fn fight_round(host: &mut MockHost, mob: Uid) {
    host.rolls.push_front(PERCENT_ROLL);
    dispatch::fight_mtrigger(host, mob);
}

/// Resume the one pending wait and return the pulses it asked for next.
fn step(host: &mut MockHost) -> Vec<(Uid, u64, i64)> {
    let due = std::mem::take(&mut host.waits);
    for (owner, id, _) in &due {
        resume(host, *owner, *id);
    }
    host.waits.clone()
}

// ---------------------------------------------------------------------
// Trigger 65, mob death `0 f 100`: a typo that becomes a command.
//
// `%send% %actor% ...` is msend to the killer ("his": the mob is male).
// `%echoround%` is not a variable (the script means echoaround), so it
// substitutes to nothing and the line " <actor id> <name> curses Tester ..."
// reaches the interpreter: msend never sees it, the mob's own command
// interpreter does (a leading space and the actor's id, as var_subst leaves
// it). `%echo%` is mecho, heard by the whole room. `%teleport% %self% 0`
// moves the mob to room 0.
// ---------------------------------------------------------------------
#[test]
fn trigger_65_curses_the_killer_and_vanishes() {
    let record = real!(65);
    let mut scene = Scene::new();
    let (player, watcher, room) = (scene.player(), scene.watcher, scene.room());
    let void = scene.host().add_room(0, "The Void");
    let mob = scene.add_owner_mob(7001, "a goblin");
    scene.host().set_num(mob, "sex", 1);
    install_record(scene.host(), mob, &record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    assert_eq!(dispatch::death_mtrigger(scene.host(), mob, Some(player)), 1);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec![
            "a goblin curses at you before drawing his final breath.",
            "a goblin's body fades out of existence.",
        ]
    );
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec!["a goblin's body fades out of existence."]
    );
    assert_eq!(
        said(&scene.fx.host),
        vec![(
            mob,
            format!(" }}{player} a goblin curses Tester before drawing his final breath.")
        )]
    );
    assert_eq!(scene.fx.host.list(void, "people"), vec![mob]);
    assert!(!scene.fx.host.list(room, "people").contains(&mob));
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
    // msend to the killer, then mecho (everyone, the mob at the head of the
    // room's people last in the C's TO_ROOM / TO_CHAR split): Watcher, Tester.
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                player,
                "a goblin curses at you before drawing his final breath."
            ),
            sub_line(watcher, "a goblin's body fades out of existence."),
            sub_line(player, "a goblin's body fades out of existence."),
        ]
    );
    assert!(scene.fx.host.waits.is_empty());
}

// ---------------------------------------------------------------------
// Trigger 2, mob death `0 f 100`: load an object and a copy of the mob.
//
// `say` goes to the interpreter. `%load% obj 1` is `mload obj 1` with no
// target: a takeable object (ITEM_WEAR_TAKE) goes into the mob's inventory,
// anything else onto the floor. `%load% mob %self.vnum%`
// is `mload mob 7002`: a new mob appears in the mob's room.
// ---------------------------------------------------------------------
#[test]
fn trigger_2_reloads_the_mob_and_drops_the_wings() {
    let record = real!(2);
    let mut scene = Scene::new();
    let (player, room) = (scene.player(), scene.room());
    let mob = scene.add_owner_mob(7002, "an ogre");
    for (key, name) in [("mobproto:7002", "an ogre"), ("objproto:1", "the wings")] {
        scene
            .host()
            .globals
            .texts
            .insert(key.to_owned(), name.to_owned());
    }
    scene
        .host()
        .globals
        .nums
        .insert("objproto:1:wear".to_owned(), 1);
    install_record(scene.host(), mob, &record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    assert_eq!(dispatch::death_mtrigger(scene.host(), mob, Some(player)), 1);
    assert_eq!(
        said(&scene.fx.host),
        vec![(
            mob,
            "say You got the best of me, Tester. But I'll be back.".to_owned()
        )]
    );
    let carried = scene.fx.host.list(mob, "inventory");
    assert_eq!(vnums(&scene.fx.host, &carried), vec![1]);
    let people = scene.fx.host.list(room, "people");
    let copies: Vec<&Uid> = people
        .iter()
        .filter(|ch| **ch != mob && scene.fx.host.num(**ch, "vnum") == 7002)
        .collect();
    assert_eq!(copies.len(), 1, "one fresh copy of the mob");
    // The copy stands at the head of the people of the room.
    assert_eq!(
        people,
        vec![*copies[0], mob, scene.watcher, scene.guard(), player]
    );
    assert_eq!(
        vnums(&scene.fx.host, &scene.fx.host.list(room, "contents")),
        vec![3020]
    );
    assert!(scene.fx.host.waits.is_empty());
    // Loading sends nothing to anyone.
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 70, mob fight `0 k 100`: dg_cast, a global, and a long wait.
//
// First firing: `already_cast` is unset, so the else branch runs:
// `dg_cast 'magic missile' Tester`. A mob casts as itself at its own level
// (30): call_magic(mob, victim, NULL, spell, 30). Then `set` and `global`
// leave already_cast = 1 on the mob's script. Second firing: the variable
// exists, so `wait 30 s` (30 * 10 = 300 pulses); on resume `unset
// already_cast` removes the global. Third firing casts again.
// ---------------------------------------------------------------------
#[test]
fn trigger_70_casts_once_then_rests_for_thirty_seconds() {
    let record = real!(70);
    let mut scene = Scene::new();
    let (player, room) = (scene.player(), scene.room());
    let mob = scene.add_owner_mob(7003, "a mage");
    scene
        .host()
        .add_spell("magic missile", 32, OFFENSIVE, MAG_DAMAGE);
    let id = install_record(scene.host(), mob, &record);
    scene.host().set_link(mob, "fighting", Some(player));
    let cast = call_magic(32, 30, mob, player, room);

    fight_round(scene.host(), mob);
    assert_eq!(scene.fx.host.effects, vec![cast.clone()]);
    assert_eq!(
        global_var(&scene.fx.host, mob, "already_cast").as_deref(),
        Some("1")
    );
    assert!(scene.fx.host.waits.is_empty());

    fight_round(scene.host(), mob);
    assert_eq!(scene.fx.host.waits, vec![(mob, id, 300)]);
    assert_eq!(
        scene.fx.host.effects.len(),
        1,
        "no second cast while resting"
    );
    assert!(step(scene.host()).is_empty());
    assert_eq!(global_var(&scene.fx.host, mob, "already_cast"), None);

    fight_round(scene.host(), mob);
    assert_eq!(scene.fx.host.effects, vec![cast.clone(), cast]);
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 5001, mob fight `0 k 10`: switch with fall-through.
//
// `switch %actor.level%` jumps to the first `case` equal to the level
// (numeric compare) and runs on from there; the other `case` lines are
// labels, so `case 9 / case 10 / case 11` share one body and `case 1/2/3`
// share an empty one that ends at `break`. No match runs `default`. Level 16
// nests an if/else on the alignment. The caster is the mob (level 30).
// ---------------------------------------------------------------------
#[test]
fn trigger_5001_picks_a_spell_by_level() {
    let record = real!(5001);
    let spells = [
        ("magic missile", 4),
        ("shocking grasp", 8),
        ("lightning bolt", 10),
        ("dispel good", 16),
        ("dispel evil", 17),
        ("harm", 22),
        ("fireball", 40),
    ];
    // (level, align, the spell cast or none)
    let table: [(i64, i64, Option<&str>); 10] = [
        (1, 0, None),
        (3, 0, None),
        (4, 0, Some("magic missile")),
        (8, 0, Some("shocking grasp")),
        (10, 0, Some("lightning bolt")),
        (16, 100, Some("dispel good")),
        (16, -100, Some("dispel evil")),
        (22, 0, Some("harm")),
        (23, 0, Some("fireball")),
        (40, 0, Some("fireball")),
    ];
    for (level, align, expected) in table {
        let mut scene = Scene::new();
        let (player, room) = (scene.player(), scene.room());
        let mob = scene.add_owner_mob(5004, "a mage");
        for (number, (name, _)) in spells.iter().enumerate() {
            scene
                .host()
                .add_spell(name, 100 + number as i32, OFFENSIVE, MAG_DAMAGE);
        }
        scene.host().set_num(player, "level", level);
        scene.host().set_num(player, "align", align);
        install_record(scene.host(), mob, &record);
        scene.host().set_link(mob, "fighting", Some(player));
        fight_round(scene.host(), mob);
        let want: Vec<String> = expected
            .map(|name| {
                let number = spells.iter().position(|(n, _)| *n == name).unwrap();
                call_magic(100 + number as i32, 30, mob, player, room)
            })
            .into_iter()
            .collect();
        assert_eq!(scene.fx.host.effects, want, "level {level} align {align}");
        assert!(scene.fx.host.logs.is_empty(), "level {level}");
        assert_clean(&scene.fx.host);
    }
}

// ---------------------------------------------------------------------
// Trigger 332, room drop `2 h 100`: wait, eval, switch, dg_cast, purge.
//
// The first line is `%%send%%`: var_subst turns `%%` into a literal `%`, so
// the line reaches wld_command_interpreter as "%send% <id> As you drop ..."
// and is logged as an unknown world command. `wait 1` is one pulse. Then
// `eval worth %object.cost% / 100` (integer division) selects the case. A
// room has no caster of its own, so dg_cast loads the proxy mob 1 (named
// "The gods"), casts at DG_SPELL_LEVEL 25 and extracts it. `clot minor` is
// not a spell here, so it only logs. `%purge% %object%` is wpurge.
// ---------------------------------------------------------------------
fn drop_offering(record: &Record, cost: i64) -> (Scene, u64) {
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    scene
        .host()
        .globals
        .texts
        .insert("mobproto:1".to_owned(), "a caster proxy".to_owned());
    for (number, name) in [
        (1, "magic missile"),
        (2, "cure light"),
        (3, "refresh"),
        (4, "invisibility"),
    ] {
        scene.host().add_spell(name, number, OFFENSIVE, MAG_DAMAGE);
    }
    let dagger = scene.fx.inventory[0];
    scene.host().set_num(dagger, "cost", cost);
    // perform_drop runs `drop_wtrigger` before `obj_from_char`, and the
    // trigger tests `obj->carried_by != actor`: the dagger is still carried.
    let id = install_record(scene.host(), room, record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    assert_eq!(
        dispatch::drop_wtrigger(scene.host(), dagger, player),
        1,
        "the first run ends at the wait with the default 1: the drop goes on"
    );
    assert_eq!(scene.fx.host.waits, vec![(room, id, 1)]);
    assert!(heard(&scene.fx.host, player).is_empty());
    assert_eq!(
        scene.fx.host.logs,
        vec![
            "Room 3001 :: Unknown world cmd: '%send% }1 As you drop the a dagger, a loud humming starts to come from the walls.'"
        ]
    );
    assert_eq!(scene.fx.host.link(dagger, "carried_by"), Some(player));
    // The first run returned 1 at the wait: the drop completes.
    scene.host().obj_to_room(dagger, room);
    assert_eq!(run_waits(scene.host(), 5), 1);
    (scene, id)
}

/// The proxy caster's id, read off the `extract_char` effect that follows
/// each cast.
fn proxy_of(host: &MockHost) -> Uid {
    let text = host
        .effects
        .iter()
        .find_map(|effect| effect.strip_prefix("extract_char "))
        .expect("the proxy was extracted");
    text.parse().unwrap()
}

#[test]
fn trigger_332_rewards_an_offering_by_its_worth() {
    let record = real!(332);
    // (cost, message, spells in order, purged)
    let cases: [(i64, &str, Vec<i32>, bool); 4] = [
        (50, "Your offering was NOT sufficient.", vec![1], false),
        (150, "Your offering was just sufficient.", vec![2], true),
        (250, "Your offering was sufficient.", vec![3], true),
        (999, "Your offering was as it must be.", vec![4], true),
    ];
    for (cost, message, spells, purged) in cases {
        let (scene, _) = drop_offering(&record, cost);
        let (player, room) = (scene.player(), scene.room());
        let dagger = scene.fx.inventory[0];
        assert_eq!(heard(&scene.fx.host, player), vec![message], "cost {cost}");
        let proxy = proxy_of(&scene.fx.host);
        let mut want: Vec<String> = vec![sub_line(player, message)];
        want.extend(spells.iter().flat_map(|spell| {
            [
                call_magic(*spell, 25, proxy, player, room),
                format!("extract_char {proxy}"),
            ]
        }));
        if purged {
            want.push(format!("extract_obj {dagger}"));
        }
        assert_eq!(scene.fx.host.effects, want, "cost {cost}");
        assert!(scene.fx.host.mud_commands.is_empty());
        assert!(scene.fx.host.waits.is_empty());
        let in_room = scene.fx.host.list(room, "contents").contains(&dagger);
        assert_eq!(in_room, !purged, "cost {cost}");
        // The proxy named itself after the gods: only the unknown spell of
        // the 150 case logs, and it logs nothing else.
        let extra = scene.fx.host.logs.len() - 1;
        assert_eq!(extra, usize::from(cost == 150), "cost {cost}");
        assert_clean(&scene.fx.host);
    }
}

#[test]
fn trigger_332_logs_the_spell_the_game_does_not_have() {
    let (scene, _) = drop_offering(&real!(332), 150);
    assert_eq!(
        scene.fx.host.logs[1],
        "Trigger: Room Drop - Dg_cast for dropping objects by cost, VNum 332. dg_cast: invalid spell name (dg_cast 'clot minor' }1)"
    );
}

// ---------------------------------------------------------------------
// Trigger 5304, mob fight `0 k 5`: send, echoaround, eval, damage.
//
// msend to the actor, mechoaround to the others (the actor's own text uses
// the mob's name), `eval fire_damage %actor.level%` copies 10, and
// `%damage%` is mdamage: hit 100 -> 90 (10 is not above max_hit >> 2 = 25).
// ---------------------------------------------------------------------
#[test]
fn trigger_5304_breathes_fire_for_the_victims_level() {
    let record = real!(5304);
    let mut scene = Scene::new();
    let (player, watcher) = (scene.player(), scene.watcher);
    let mob = scene.add_owner_mob(5309, "an efreeti");
    install_record(scene.host(), mob, &record);
    scene.host().set_link(mob, "fighting", Some(player));
    fight_round(scene.host(), mob);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["You are engulfed in fire as an efreeti breathes on you."]
    );
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec!["an efreeti engulfs Tester with a hot breath of fire."]
    );
    assert_eq!(scene.fx.host.num(player, "hit"), 90);
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                player,
                "You are engulfed in fire as an efreeti breathes on you."
            ),
            sub_line(
                watcher,
                "an efreeti engulfs Tester with a hot breath of fire."
            ),
        ]
    );
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 1114, mob load `0 n 100`: dg_affect on self.
//
// do_dg_affect: half_chop splits the words ("dg_affect", target, property,
// value, duration; the doubled space is skipped), the value "on" is atoi 0,
// the duration must be positive and the affect gets duration - 1. The
// property is looked up in apply_types (no) and then affected_bits:
// BLIND is bit 1, SLEEP bit 15. location APPLY_NONE is 0. `%echo%` (mecho)
// follows.
// ---------------------------------------------------------------------
#[test]
fn trigger_1114_blinds_and_sleeps_itself() {
    let record = real!(1114);
    let mut scene = Scene::new();
    let (player, watcher) = (scene.player(), scene.watcher);
    let mob = scene.add_owner_mob(1115, "a spider");
    install_record(scene.host(), mob, &record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    dispatch::load_mtrigger(scene.host(), mob);
    // mecho is `sub_write(.., TO_ROOM)` then `TO_CHAR`: the room's people head
    // first (the spider, then Watcher, the guard, Tester; sub_write ends in
    // "\n\r"), and the spider has no descriptor.
    assert_eq!(
        scene.fx.host.effects,
        vec![
            format!("affect_to_char {mob} location=0 modifier=0 duration=0 bit=1"),
            format!("affect_to_char {mob} location=0 modifier=0 duration=1 bit=15"),
            format!("send {watcher}: sleeping now\n\r"),
            format!("send {player}: sleeping now\n\r"),
        ]
    );
    assert_eq!(scene.fx.host.num(mob, "aff:1"), 1);
    assert_eq!(scene.fx.host.num(mob, "aff:15"), 1);
    assert_eq!(heard(&scene.fx.host, player), vec!["sleeping now"]);
    assert_eq!(heard(&scene.fx.host, watcher), vec!["sleeping now"]);
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 1936, mob greet+greet-all `0 gh 50`: three waits, an affect, kill.
//
// Each `wait 1 s` is 10 pulses. After the first: the mob `emote`s and
// blinds the actor for 1 (affect duration 0). After the second: msend and
// mechoaround. After the third: msend, mechoaround and `kill Tester`, a game
// command (mkill is the script one). Three waits in all, then it ends.
// ---------------------------------------------------------------------
#[test]
fn trigger_1936_blinds_the_newcomer_and_attacks() {
    let record = real!(1936);
    let mut scene = Scene::new();
    let (player, watcher) = (scene.player(), scene.watcher);
    let mob = scene.add_owner_mob(1936, "Selvetarm");
    let id = install_record(scene.host(), mob, &record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    assert_eq!(dispatch::greet_mtrigger(scene.host(), player, 0), 1);
    assert_eq!(scene.fx.host.waits, vec![(mob, id, 10)]);
    assert!(said(&scene.fx.host).is_empty());

    assert_eq!(step(scene.host()), vec![(mob, id, 10)]);
    assert_eq!(
        said(&scene.fx.host),
        vec![(
            mob,
            "emote suddenly raises his arms and utters: Lloth, Goddess of Darkness, cloud the eyes of your enemies!".to_owned()
        )]
    );
    assert_eq!(
        scene.fx.host.effects,
        vec![format!(
            "affect_to_char {player} location=0 modifier=0 duration=0 bit=1"
        )]
    );

    assert_eq!(step(scene.host()), vec![(mob, id, 10)]);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec![
            "A dark mist suddenly swirls around you, the chittering of many spiders filling your ears!"
        ]
    );
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec![
            "A dark mist suddenly swirls around Tester, the chittering of spiders filling the air!"
        ]
    );

    assert!(step(scene.host()).is_empty());
    assert_eq!(
        heard(&scene.fx.host, player)[1..],
        ["You have been blinded!"]
    );
    assert_eq!(
        heard(&scene.fx.host, watcher)[1..],
        ["Tester has been blinded."]
    );
    assert_eq!(said(&scene.fx.host).len(), 2);
    assert_eq!(said(&scene.fx.host)[1], (mob, "kill Tester".to_owned()));
    assert_eq!(
        scene.fx.host.effects,
        vec![
            format!("affect_to_char {player} location=0 modifier=0 duration=0 bit=1"),
            sub_line(
                player,
                "A dark mist suddenly swirls around you, the chittering of many spiders filling your ears!"
            ),
            sub_line(
                watcher,
                "A dark mist suddenly swirls around Tester, the chittering of spiders filling the air!"
            ),
            sub_line(player, "You have been blinded!"),
            sub_line(watcher, "Tester has been blinded."),
        ]
    );
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 1113, room command `test`: walk the people, mixed mobs and PCs.
//
// `%self.people%` starts at the head (Watcher, guard, Tester). A player
// takes the first branch, a mob gets `dg_affect ... sleep on 20` (duration
// 20 - 1 = 19). Every `%send% detta ...` is wsend to a name nobody has, which
// logs "no target found for wsend": two lines per person, six in all.
// ---------------------------------------------------------------------
#[test]
fn trigger_1113_puts_every_mob_to_sleep() {
    let record = real!(1113);
    let mut scene = Scene::new();
    let (room, player, guard) = (scene.room(), scene.player(), scene.guard());
    install_record(scene.host(), room, &record);
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "test", ""),
        1
    );
    assert_eq!(
        scene.fx.host.effects,
        vec![format!(
            "affect_to_char {guard} location=0 modifier=0 duration=19 bit=15"
        )]
    );
    assert_eq!(scene.fx.host.num(guard, "aff:15"), 1);
    assert_eq!(scene.fx.host.num(player, "aff:15"), 0);
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.waits.is_empty());
    assert_eq!(
        scene.fx.host.logs,
        vec!["Room 3001 :: no target found for wsend"; 6]
    );
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 2700, room enter `2 g 100`: equipment by position name.
//
// `%actor.eq(rwrist)%` is the item worn at WEAR_WRIST_R (14), `lwrist` the
// one at WEAR_WRIST_L (15); an empty slot gives "" whose `.vnum` is "".
// Parenthesised conditions work like any other. The shackle (2708) in
// either slot, or the global Zn27_shacklepass, lets the actor in (wsend
// only). Otherwise: wsend, `%force% %actor% sit` (wforce hands the
// interpreter " sit"), a local copied to the actor with `remote`, and
// `return 0`.
// ---------------------------------------------------------------------
fn shackle_scene(record: &Record, setup: impl FnOnce(&mut Scene)) -> (Scene, i32) {
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    setup(&mut scene);
    install_record(scene.host(), room, record);
    let ret = dispatch::enter_wtrigger(scene.host(), room, player, 0);
    (scene, ret)
}

#[test]
fn trigger_2700_lets_shackle_wearers_through() {
    let record = real!(2700);
    let pass = "A magical force seems to ebb, allowing you to pass.";
    for slot in [14, 15] {
        let (scene, ret) = shackle_scene(&record, |scene| {
            let player = scene.player();
            let shackle = scene.fx.host.add_obj(2708, "a shackle");
            scene.host().equip_char(player, shackle, slot);
        });
        assert_eq!(ret, 1, "slot {slot}");
        assert_eq!(heard(&scene.fx.host, scene.player()), vec![pass]);
        assert_eq!(
            scene.fx.host.effects,
            vec![sub_line(scene.player(), pass)],
            "slot {slot}"
        );
        assert!(scene.fx.host.mud_commands.is_empty());
        assert!(scene.fx.host.logs.is_empty());
    }
    let (scene, ret) = shackle_scene(&record, |scene| {
        let player = scene.player();
        scene
            .host()
            .ensure_script(player)
            .unwrap()
            .global_vars
            .add_var("Zn27_shacklepass", "1", 0);
    });
    assert_eq!(ret, 1);
    assert_eq!(scene.fx.host.effects, vec![sub_line(scene.player(), pass)]);
    assert!(scene.fx.host.mud_commands.is_empty());
    assert_clean(&scene.fx.host);
}

#[test]
fn trigger_2700_repels_everyone_else_and_makes_them_sit() {
    let record = real!(2700);
    // A different item on the wrist does not count.
    let (scene, ret) = shackle_scene(&record, |scene| {
        let player = scene.player();
        let bangle = scene.fx.host.add_obj(2709, "a bangle");
        scene.host().equip_char(player, bangle, 14);
    });
    let player = scene.player();
    assert_eq!(ret, 0);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["A powerful force seems to repel you, preventing you from entering."]
    );
    assert_eq!(said(&scene.fx.host), vec![(player, " sit".to_owned())]);
    assert_eq!(
        scene.fx.host.effects,
        vec![sub_line(
            player,
            "A powerful force seems to repel you, preventing you from entering."
        )]
    );
    assert_eq!(
        global_var(&scene.fx.host, player, "Zn27_shacklereject").as_deref(),
        Some("1")
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 19, room random `2 b 100`, and 20, mob speech `0 d 100`: `at`.
//
// 19: `%at% 33 %echo% ...` is "wat 33 wecho ...": do_wat runs the command
// with the room set to 33, so the player Rumble standing there hears it
// (act() capitalises it: "At'd to a room"). `%at% Rumble ...` finds the
// location through the character.
// 20: the mob `mat`s to Rumble and the line goes to the game's interpreter
// with the mob standing in Rumble's room; afterwards it is back. do_mat
// hands command_interpreter what one_argument leaves after the location, so
// the line keeps its leading space. `mecho` is no game command, but the
// interpreter is what receives it.
// ---------------------------------------------------------------------
fn rumble(scene: &mut Scene) -> Uid {
    let far = scene.host().add_room(33, "The Far Room");
    let rumble = scene.host().add_player("Rumble");
    common::world::dress_player(scene.host(), rumble, 20);
    scene.host().char_to_room(rumble, far);
    rumble
}

#[test]
fn trigger_19_echoes_into_another_room() {
    let record = real!(19);
    let mut scene = Scene::new();
    let rumble = rumble(&mut scene);
    let room = scene.room();
    install_record(scene.host(), room, &record);
    dispatch::script_trigger_check(scene.host());
    assert_eq!(
        heard(&scene.fx.host, rumble),
        vec!["At'd to a room", "At'd to Rumble"]
    );
    assert!(heard(&scene.fx.host, scene.player()).is_empty());
    assert_eq!(
        scene.fx.host.effects,
        vec![
            act_line(rumble, "At'd to a room"),
            act_line(rumble, "At'd to Rumble"),
        ]
    );
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

#[test]
fn trigger_20_relays_speech_from_the_rumbles_room() {
    let record = real!(20);
    let mut scene = Scene::new();
    rumble(&mut scene);
    let (player, room) = (scene.player(), scene.room());
    let mob = scene.add_owner_mob(7005, "a spy");
    install_record(scene.host(), mob, &record);
    dispatch::speech_mtrigger(scene.host(), player, "hello there");
    assert_eq!(
        said(&scene.fx.host),
        vec![
            (mob, " say Tester says, 'hello there' 1".to_owned()),
            (mob, " mecho Tester says, 'hello there' 2".to_owned()),
        ]
    );
    assert_eq!(scene.fx.host.link(mob, "room"), Some(room));
    assert!(scene.fx.host.list(room, "people").contains(&mob));
    assert!(
        !scene
            .fx
            .host
            .list(ROOM_ID_BASE + 33, "people")
            .contains(&mob)
    );
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 11838, object command `shoot`: nested substitution, `at`, send.
//
// `eval dir %arg%` copies "north". `eval direction %%self.room.%dir%(vnum)%%`
// substitutes to "eval direction %self.room.north(vnum)%" (`%%` is a
// literal %), which eval then substitutes again: the vnum of the room north
// of the object's room, 3002; `(room)` gives that room's id. `%where.people%`
// is the first person there, the player Victim. osend to Victim; `%at%
// 3002 %damage% Victim 20` is "oat 3002 odamage <id> 20" (the damage is done
// from the far room: hit 100 -> 80, 20 is under max_hit >> 2 = 25); the actor
// is told, with `%people.himher%` = "him", and the room hears the rest.
// oat clones the object from its prototype, so the prototype must exist.
// ---------------------------------------------------------------------
#[test]
fn trigger_11838_shoots_the_person_next_door() {
    let record = real!(11838);
    let mut scene = Scene::new();
    let (player, watcher, room) = (scene.player(), scene.watcher, scene.room());
    let north = scene.fx.neighbours[0];
    let victim = scene.host().add_player("Victim");
    common::world::dress_player(scene.host(), victim, 10);
    scene.host().char_to_room(victim, north);
    let gun = scene.fx.host.add_obj(3800, "a crossbow");
    // The trigger's numeric argument is 1: it listens only while worn.
    scene.host().equip_char(player, gun, 17);
    scene
        .host()
        .globals
        .texts
        .insert("objproto:3800".to_owned(), "a crossbow".to_owned());
    install_record(scene.host(), gun, &record);
    assert_eq!(
        dispatch::command_otrigger(scene.host(), player, "shoot", " north"),
        1
    );
    assert_eq!(
        heard(&scene.fx.host, victim),
        vec!["You are suddenly shot by Tester."]
    );
    assert_eq!(scene.fx.host.num(victim, "hit"), 80);
    // osend to Victim; oat clones the crossbow into room 3002 to run the
    // damage there and extracts the clone again; osend to the shooter and
    // oechoaround for the rest of the room.
    let clone = scene.fx.host.effects.get(1).cloned().unwrap_or_default();
    assert!(clone.starts_with("extract_obj "), "{clone}");
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(victim, "You are suddenly shot by Tester."),
            clone,
            sub_line(player, "You shoot Victim and damage him."),
            sub_line(watcher, "Tester shoots Victim and damages him"),
        ]
    );
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["You shoot Victim and damage him."]
    );
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec!["Tester shoots Victim and damages him"]
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert!(scene.fx.host.list(room, "people").contains(&player));
    assert_clean(&scene.fx.host);
}

#[test]
fn trigger_11838_wants_a_direction() {
    let record = real!(11838);
    let mut scene = Scene::new();
    let player = scene.player();
    let gun = scene.fx.host.add_obj(3800, "a crossbow");
    scene.host().equip_char(player, gun, 17);
    install_record(scene.host(), gun, &record);
    assert_eq!(
        dispatch::command_otrigger(scene.host(), player, "shoot", " sideways"),
        1
    );
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["You must specify a direction - north, east, west, south, up, or down."]
    );
    assert_eq!(
        scene.fx.host.effects,
        vec![sub_line(
            player,
            "You must specify a direction - north, east, west, south, up, or down."
        )]
    );
}

// ---------------------------------------------------------------------
// Trigger 13, object command `sl` (4 = in the room): contains, force, remote.
//
// `set argcheck 'bed` stores the quote too, so `%argcheck.contains('%arg%)%`
// asks whether "'<arg>" occurs in "'bed" (case-blind): true for "bed" and
// for "be", false for "chair". `%cmd.mudcommand%` is the first table entry
// "sl" abbreviates, "sleep". Then `%force% %actor% sleep` (oforce: the
// line " sleep"), and a variable copied to the actor's script by `remote`.
// ---------------------------------------------------------------------
fn comfy_bed(record: &Record, arg: &str) -> (Scene, Uid) {
    let mut scene = Scene::new();
    let player = scene.player();
    let commands = scene.fx.host.globals.texts["commands"].clone();
    scene
        .host()
        .globals
        .texts
        .insert("commands".to_owned(), format!("{commands} sleep"));
    let bed = scene.add_room_obj(1401, "a comfy bed");
    install_record(scene.host(), bed, record);
    let argument = format!(" {arg}");
    assert_eq!(
        dispatch::command_otrigger(scene.host(), player, "sl", &argument),
        1
    );
    (scene, player)
}

#[test]
fn trigger_13_puts_the_sleeper_in_the_bed() {
    let record = real!(13);
    for arg in ["bed", "be", "BED", "Be"] {
        let (scene, player) = comfy_bed(&record, arg);
        assert_eq!(
            said(&scene.fx.host),
            vec![(player, " sleep".to_owned())],
            "{arg}"
        );
        assert_eq!(
            global_var(&scene.fx.host, player, "laying_in_comfy_bed_14").as_deref(),
            Some("1")
        );
        assert!(scene.fx.host.effects.is_empty());
        assert!(scene.fx.host.waits.is_empty());
        assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
        assert_clean(&scene.fx.host);
    }
    let (scene, player) = comfy_bed(&record, "chair");
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.effects.is_empty());
    assert_eq!(
        global_var(&scene.fx.host, player, "laying_in_comfy_bed_14"),
        None
    );
}

// ---------------------------------------------------------------------
// Trigger 52, room command `l`: contains, load, and a wait.
//
// Same `'bridge` trick. Matching: wsend, wechoaround, `wload mob 207` (the
// mob appears in the room at once), `wait 1 sec` (10 pulses) and then wecho.
// Otherwise `return 0`.
// ---------------------------------------------------------------------
#[test]
fn trigger_52_lets_the_dog_out_from_under_the_bridge() {
    let record = real!(52);
    let mut scene = Scene::new();
    let (room, player, watcher) = (scene.room(), scene.player(), scene.watcher);
    scene
        .host()
        .globals
        .texts
        .insert("mobproto:207".to_owned(), "a filthy dog".to_owned());
    let id = install_record(scene.host(), room, &record);
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "look", " bridge"),
        1
    );
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["As you look at the bridge a small form staggers out from underneath it."]
    );
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec!["As Tester peers under the bridge a small form emerges."]
    );
    let people = scene.fx.host.list(room, "people");
    assert_eq!(people.len(), 4);
    assert_eq!(scene.fx.host.num(people[0], "vnum"), 207);
    assert_eq!(scene.fx.host.waits, vec![(room, id, 10)]);
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                player,
                "As you look at the bridge a small form staggers out from underneath it."
            ),
            sub_line(
                watcher,
                "As Tester peers under the bridge a small form emerges."
            ),
        ]
    );

    run_waits(scene.host(), 5);
    let line = "A filthy dog crawls out from underneath the bridge. It is covered in MUD.";
    assert_eq!(heard(&scene.fx.host, player)[1..], [line]);
    assert_eq!(heard(&scene.fx.host, watcher)[1..], [line]);
    // The dog (no descriptor) now heads the room's people, so the echo reaches
    // Watcher, then Tester.
    assert_eq!(
        scene.fx.host.effects[2..],
        echo_lines(&scene.fx.host, room, line, "\r\n")
    );
    assert_eq!(scene.fx.host.effects.len(), 4);
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);

    // Looking at anything else is not caught.
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    install_record(scene.host(), room, &record);
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "look", " tree"),
        0
    );
    assert!(scene.fx.host.effects.is_empty());
}

// ---------------------------------------------------------------------
// Trigger 30, room speech `2 d 100`: every text function.
//
// speech = "hello big test world": strlen 20, trim the same, car "hello",
// cdr "big test world", mudcommand: the first table entry the whole text
// abbreviates (none, so ""), contains(test) 1, charat(3) the third
// character 'l'. All through wecho.
// ---------------------------------------------------------------------
#[test]
fn trigger_30_demonstrates_the_text_functions() {
    let record = real!(30);
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    install_record(scene.host(), room, &record);
    dispatch::speech_wtrigger(scene.host(), player, "hello big test world");
    assert_eq!(
        heard(&scene.fx.host, player),
        vec![
            "SPEECH:     hello big test world",
            "STRLEN:     20",
            "TRIM:       hello big test world",
            "CAR:        hello",
            "CDR:        big test world",
            "MUDCOMMAND: ",
            "CONTAINS:   1",
            "CHARAT 3:   l",
        ]
    );
    assert_eq!(
        scene.fx.host.effects,
        echo_all(
            &scene.fx.host,
            room,
            &[
                "SPEECH:     hello big test world",
                "STRLEN:     20",
                "TRIM:       hello big test world",
                "CAR:        hello",
                "CDR:        big test world",
                "MUDCOMMAND: ",
                "CONTAINS:   1",
                "CHARAT 3:   l",
            ],
            "\r\n"
        )
    );
    assert!(scene.fx.host.mud_commands.is_empty());
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.logs.is_empty());
}

// ---------------------------------------------------------------------
// Trigger 48, object command `join` (7): inventory(vnum), purge, transform.
//
// In room 133 with objects 189 (the staff, which carries the trigger) and
// 191 (the orb) in the inventory: `%actor.inventory(191)%` is the orb's id,
// `%purge%` of it is opurge (extract_obj). `switch %random.2%`: 1 ->
// `%transform% 12` (otransform: the staff becomes object 12), 2 -> default,
// `%transform% 192`. osend / oechoaround follow, with `%actor.hisher%`.
// `detach 48 %self.id%` removes the trigger. Missing items or the wrong room
// only send a refusal.
// ---------------------------------------------------------------------
/// The id of the object `otransform` read from the prototype and extracted
/// again: the target of the second `extract_obj` effect.
fn temporary_object(host: &MockHost) -> Uid {
    host.effects
        .get(2)
        .and_then(|effect| effect.strip_prefix("extract_obj "))
        .and_then(|text| text.parse().ok())
        .expect("the temporary object is extracted")
}

fn join_scene(
    record: &Record,
    in_room_133: bool,
    with_orb: bool,
    roll: i64,
) -> (Scene, Uid, Uid, Uid, u64) {
    let mut scene = Scene::new();
    let player = scene.player();
    let room133 = scene.host().add_room(133, "The Workshop");
    let friend = scene.host().add_player("Friend");
    common::world::dress_player(scene.host(), friend, 5);
    scene.host().char_to_room(friend, room133);
    if in_room_133 {
        scene.host().char_to_room(player, room133);
    }
    for (vnum, name) in [(12, "a wizard's staff"), (192, "a broken staff")] {
        scene
            .host()
            .globals
            .texts
            .insert(format!("objproto:{vnum}"), name.to_owned());
    }
    let staff = scene.fx.host.add_obj(189, "a staff");
    scene.host().obj_to_char(staff, player);
    let orb = scene.fx.host.add_obj(191, "an orb");
    if with_orb {
        scene.host().obj_to_char(orb, player);
    }
    let id = install_record(scene.host(), staff, record);
    scene.host().rolls.push_back(roll);
    assert_eq!(
        dispatch::command_otrigger(scene.host(), player, "join", " orb staff"),
        1
    );
    (scene, staff, orb, friend, id)
}

#[test]
fn trigger_48_joins_the_orb_onto_the_staff() {
    let record = real!(48);
    let (scene, staff, orb, friend, _) = join_scene(&record, true, true, 1);
    let player = scene.player();
    // opurge, otransform (which reads a fresh object `o`, moves its fields
    // over the staff and ends in `extract_obj(o)`), osend (to the actor),
    // oechoaround (the others in room 133; sub_write ends in "\n\r"), then
    // the detach, which is silent.
    let temp = temporary_object(&scene.fx.host);
    assert!(temp != staff && temp != orb);
    assert_eq!(
        scene.fx.host.effects,
        vec![
            format!("extract_obj {orb}"),
            format!("transform_obj {staff} 12"),
            format!("extract_obj {temp}"),
            sub_line(
                player,
                "As you join the orb to the staff it clicks into place."
            ),
            sub_line(friend, "Tester places an orb onto his staff."),
        ]
    );
    assert_eq!(
        vnums(&scene.fx.host, &scene.fx.host.carrying(player)),
        vec![12, 3021, 3022]
    );
    assert_eq!(scene.fx.host.num(staff, "vnum"), 12);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["As you join the orb to the staff it clicks into place."]
    );
    assert_eq!(
        heard(&scene.fx.host, friend),
        vec!["Tester places an orb onto his staff."]
    );
    assert!(
        scene.fx.host.script(staff).is_none(),
        "the trigger detached itself and the empty script was freed"
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

#[test]
fn trigger_48_breaks_the_staff_on_the_other_roll() {
    let record = real!(48);
    let (scene, staff, orb, friend, _) = join_scene(&record, true, true, 2);
    assert_eq!(scene.fx.host.num(staff, "vnum"), 192);
    let (player, temp) = (scene.player(), temporary_object(&scene.fx.host));
    assert_eq!(
        scene.fx.host.effects,
        vec![
            format!("extract_obj {orb}"),
            format!("transform_obj {staff} 192"),
            format!("extract_obj {temp}"),
            sub_line(
                player,
                "As you try to join the orb to the staff it turns in your hands and snaps in half."
            ),
            sub_line(
                friend,
                "Tester tries to place an orb onto his staff until the staff twists in his hands and snaps in half."
            ),
        ]
    );
    assert!(scene.fx.host.script(staff).is_none());
    assert_eq!(
        heard(&scene.fx.host, scene.player()),
        vec!["As you try to join the orb to the staff it turns in your hands and snaps in half."]
    );
    assert_eq!(
        heard(&scene.fx.host, friend),
        vec![
            "Tester tries to place an orb onto his staff until the staff twists in his hands and snaps in half."
        ]
    );
}

#[test]
fn trigger_48_refuses_without_the_orb_or_the_room() {
    let record = real!(48);
    let (scene, staff, _, _, id) = join_scene(&record, true, false, 1);
    assert_eq!(
        heard(&scene.fx.host, scene.player()),
        vec!["You do not have the required items to do this."]
    );
    assert_eq!(
        scene.fx.host.effects,
        vec![sub_line(
            scene.player(),
            "You do not have the required items to do this."
        )]
    );
    assert!(scene.fx.host.script(staff).unwrap().trigger(id).is_some());
    let (scene, _, _, _, _) = join_scene(&record, false, true, 1);
    assert_eq!(
        scene.fx.host.effects,
        vec![sub_line(scene.player(), "You can not do that here.")]
    );
    assert_eq!(scene.fx.host.rolls.len(), 1, "the roll is never taken");
    assert!(other_effects(&scene.fx.host).is_empty());
}

// ---------------------------------------------------------------------
// Trigger 1916 (mob leave `0 q 100`) attaching trigger 1917 to the room.
//
// `eval here %self.room%`, `attach 1917 %here.id%`: a room's id is
// ROOM_ID_BASE + vnum, find_room takes it, the room gets a new instance of
// the room-leave trigger 1917 (attach does not check the type against the
// owner). `return 0` only sets the return value, the script goes on: emote,
// `wait 1 s` (10), emote, `wait 2 s` (20), emote, `wait 1 s` (10), then
// `kill Tester`. The run returns 0 to the dispatcher at the first wait.
// ---------------------------------------------------------------------
#[test]
fn trigger_1916_attaches_the_web_and_attacks_over_three_waits() {
    let record = real!(1916);
    let web = real!(1917);
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    let mob = scene.add_owner_mob(1925, "an orb spider");
    let id = install_record(scene.host(), mob, &record);
    make_proto(
        scene.host(),
        web.vnum,
        &web.name,
        AttachType::Wld,
        &web.flags,
        web.narg,
        &web.arglist,
        &web.body,
    );
    scene.host().rolls.push_back(PERCENT_ROLL);
    assert_eq!(dispatch::leave_mtrigger(scene.host(), player, 0), 0);
    let room_script = scene.fx.host.script(room).expect("the room has a script");
    assert!(room_script.is_attached(1917));
    assert_eq!(room_script.trigger_ids().len(), 1);
    assert_eq!(
        said(&scene.fx.host),
        vec![(mob, "emote cackles ominously.".to_owned())]
    );
    assert_eq!(scene.fx.host.waits, vec![(mob, id, 10)]);

    assert_eq!(step(scene.host()), vec![(mob, id, 20)]);
    assert_eq!(step(scene.host()), vec![(mob, id, 10)]);
    assert!(step(scene.host()).is_empty());
    let lines: Vec<String> = said(&scene.fx.host)
        .into_iter()
        .map(|(_, line)| line)
        .collect();
    assert_eq!(
        lines,
        vec![
            "emote cackles ominously.",
            "emote snarls: Fool! None lay eyes upon the entry to Lost Ahjuutal and escape!",
            "emote casts a web suddenly all around you.",
            "kill Tester",
        ]
    );
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 50, room `ab`: `wait until`, a room-number lookup, wpurge by name.
//
// SECS_PER_MUD_HOUR 75 and PASSES_PER_SEC 10 make an hour 750 pulses.
// `wait until 05:00` is minute 300, pulse-of-day 300 * 750 / 60 = 3750. At
// game hour 3 with pulse 100 the current pulse-of-day is 100 % 750 + 3 * 750
// = 2350, so the wait is 3750 - 2350 = 1400 pulses. Then wecho, `wload obj
// 69`, and `%findobj.291(70)%`: the number of objects of vnum 70 in room
// 291 (1), so `wpurge signclosed` removes the named object from THIS room.
// `wait until 21:00` is 1260 * 750 / 60 = 15750, minus 2350 = 13400 pulses.
// After it: wecho, `wload obj 70`; no object 69 in room 291, so nothing is
// purged.
// ---------------------------------------------------------------------
#[test]
fn trigger_50_opens_and_closes_the_shop_by_the_clock() {
    let record = real!(50);
    let mut scene = Scene::new();
    let (room, player) = (scene.room(), scene.player());
    for vnum in [69, 70] {
        scene
            .host()
            .globals
            .texts
            .insert(format!("objproto:{vnum}"), "a sign".to_owned());
    }
    let shop = scene.host().add_room(291, "The Butcher");
    let sign = scene.fx.host.add_obj(70, "a closed sign");
    scene.host().obj_to_room(sign, shop);
    let closed = scene.add_room_obj(71, "signclosed");
    scene.host().pulse = 100;
    scene.host().time.hours = 3;
    let id = install_record(scene.host(), room, &record);
    dispatch::script_trigger_check(scene.host());
    assert_eq!(scene.fx.host.waits, vec![(room, id, 1400)]);
    assert!(heard(&scene.fx.host, player).is_empty());

    assert_eq!(step(scene.host()), vec![(room, id, 13400)]);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec![
            "The butcher's assistant steps out of the shop to the North and turns the sign over from \"closed\" to \"open.\""
        ]
    );
    let contents = scene.fx.host.list(room, "contents");
    assert_eq!(vnums(&scene.fx.host, &contents), vec![3020, 69]);
    assert!(
        scene.fx.host.entity(closed).is_none(),
        "the named sign is purged"
    );
    assert_eq!(scene.fx.host.list(shop, "contents"), vec![sign]);
    let open = "The butcher's assistant steps out of the shop to the North and turns the sign over from \"closed\" to \"open.\"";
    let mut effects = echo_lines(&scene.fx.host, room, open, "\r\n");
    effects.push(format!("extract_obj {closed}"));
    assert_eq!(scene.fx.host.effects, effects);

    assert!(step(scene.host()).is_empty());
    assert_eq!(heard(&scene.fx.host, player).len(), 2);
    assert_eq!(
        heard(&scene.fx.host, player)[1],
        "The butcher's assistant steps out of the shop to the North and turns the sign over from \"open\" to \"closed.\""
    );
    let shut = "The butcher's assistant steps out of the shop to the North and turns the sign over from \"open\" to \"closed.\"";
    effects.extend(echo_lines(&scene.fx.host, room, shut, "\r\n"));
    assert_eq!(scene.fx.host.effects, effects);
    let contents = scene.fx.host.list(room, "contents");
    assert_eq!(vnums(&scene.fx.host, &contents), vec![3020, 69, 70]);
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 17, room command `move` (`2 c 100`): the `%door%` command.
//
// `%door%` is `wdoor` (do_wdoor, dg_wldcmd.c): `two_arguments` takes the room
// and the direction, `one_argument` the field, and `value` is the rest after
// skip_spaces. arg "chest": the sender hears the first line (wsend, sub_write:
// "\n\r"), the watcher the echoaround. Then, in room 14520, direction down
// (dirs index 5):
//   flags ab   -> exit_info = asciiflag_conv("ab") = 1 + 2 = 3 (the exit is
//                 CREATEd with every field zero first);
//   name       -> keyword "trapdoor";
//   room 14521 -> to_room = real_room(14521);
//   description-> general_description = value + "\r\n";
//   key 14500  -> key = atoi("14500").
// and the mirror exit up (index 4) of room 14521 the same way, to 14520. The
// field is matched with search_block(field, door_field, FALSE): exact, so the
// comment lines (`*`) do nothing. The trigger returns 1.
// ---------------------------------------------------------------------
// `if %arg% == chest` is eval_op `==` through str_cmp, which ignores case,
// and %arg% keeps the player's case, so "Chest" opens the trapdoor too.
#[test]
fn trigger_17_builds_both_ends_of_a_trapdoor() {
    let record = real!(17);
    for arg in ["chest", "Chest", "CHEST"] {
        trapdoor_case(&record, arg);
    }
}

fn trapdoor_case(record: &Record, arg: &str) {
    let mut scene = Scene::new();
    let (player, watcher) = (scene.player(), scene.watcher);
    let upper = scene.host().add_room(14520, "A Storeroom");
    let lower = scene.host().add_room(14521, "A Cellar");
    for ch in [player, watcher] {
        scene.host().char_to_room(ch, upper);
    }
    install_record(scene.host(), upper, record);
    let argument = format!(" {arg}");
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "move", &argument),
        1,
        "{arg}"
    );

    let host = &scene.fx.host;
    assert_eq!(
        heard_raw(host, player),
        vec!["You move the chest across the floor, revealing a trapdoor underneath!!\n\r"]
    );
    assert_eq!(
        heard_raw(host, watcher),
        vec!["Tester has discovered a hidden trapdoor under a chest!\n\r"]
    );
    // down from 14520
    assert_eq!(host.link(upper, "exit:5"), Some(lower));
    assert_eq!(host.num(upper, "exit:5:flags"), 3);
    assert_eq!(host.num(upper, "exit:5:key"), 14500);
    assert_eq!(host.text(upper, "exit:5:keyword"), "trapdoor");
    assert_eq!(
        host.text(upper, "exit:5:desc"),
        "A wooden ladder leads down into the darkness.\r\n"
    );
    // up from 14521
    assert_eq!(host.link(lower, "exit:4"), Some(upper));
    assert_eq!(host.num(lower, "exit:4:flags"), 3);
    assert_eq!(host.num(lower, "exit:4:key"), 14500);
    assert_eq!(host.text(lower, "exit:4:keyword"), "trapdoor");
    assert_eq!(
        host.text(lower, "exit:4:desc"),
        "A wooden ladder leads up into a storeroom.\r\n"
    );
    assert_eq!(
        host.effects,
        vec![
            sub_line(
                player,
                "You move the chest across the floor, revealing a trapdoor underneath!!"
            ),
            sub_line(
                watcher,
                "Tester has discovered a hidden trapdoor under a chest!"
            ),
        ]
    );
    assert!(host.mud_commands.is_empty());
    assert!(host.waits.is_empty());
    // nothing else was touched
    assert!(host.link(upper, "exit:4").is_none());
    assert!(host.link(lower, "exit:5").is_none());
    assert!(host.logs.is_empty(), "{:?}", host.logs);
    assert_clean(host);
}

// Trigger 17 with another argument: only the else branch runs (wsend "Move
// what ?!") and no exit appears.
#[test]
fn trigger_17_builds_nothing_for_another_argument() {
    let record = real!(17);
    let mut scene = Scene::new();
    let player = scene.player();
    let upper = scene.host().add_room(14520, "A Storeroom");
    scene.host().add_room(14521, "A Cellar");
    scene.host().char_to_room(player, upper);
    install_record(scene.host(), upper, &record);
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "move", " table"),
        1
    );
    assert_eq!(heard(&scene.fx.host, player), vec!["Move what ?!"]);
    assert_eq!(
        scene.fx.host.effects,
        vec![sub_line(player, "Move what ?!")]
    );
    assert!(scene.fx.host.link(upper, "exit:5").is_none());
    assert!(scene.fx.host.logs.is_empty());
}

// ---------------------------------------------------------------------
// Trigger 23840, room command `pu` (`2 c 100`): door room, name with a space,
// `wait 60s`, then `purge`.
//
// `pull /= %cmd%`: the player typed "pull" and str_str("pull", "pull") finds it
// (so would the abbreviation "pu" that the argument list matches); `torch /= %arg%` likewise,
// so the branch runs: wsend, wechoaround, `wdoor 23840 up room 23841` (the
// exit is created with zero fields, then to_room = real_room(23841)),
// `name trap door` (value is the rest of the line: "trap door"). `wait 60s`:
// sscanf("%ld %c") reads 60 and 's' from "60s", so 60 * PASSES_PER_SEC = 600
// pulses. After it, wecho (act: "\r\n", first letter capitalised) and
// `wdoor 23840 up purge` frees the exit: the direction has no exit again.
// ---------------------------------------------------------------------
#[test]
fn trigger_23840_opens_a_trapdoor_for_a_minute_then_purges_it() {
    let record = real!(23840);
    let mut scene = Scene::new();
    let (player, watcher) = (scene.player(), scene.watcher);
    let room = scene.host().add_room(23840, "A Torch Room");
    let above = scene.host().add_room(23841, "Above");
    for ch in [player, watcher] {
        scene.host().char_to_room(ch, room);
    }
    let id = install_record(scene.host(), room, &record);
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "pull", " torch"),
        1
    );
    assert_eq!(scene.fx.host.waits, vec![(room, id, 600)]);
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["The torch turns easily in your hand revealing a trap door."]
    );
    assert_eq!(
        heard(&scene.fx.host, watcher),
        vec!["As Tester pulls the torch a trap door swings open leading upwards."]
    );
    assert_eq!(scene.fx.host.link(room, "exit:4"), Some(above));
    assert_eq!(scene.fx.host.text(room, "exit:4:keyword"), "trap door");
    assert_eq!(scene.fx.host.num(room, "exit:4:flags"), 0);
    assert_eq!(scene.fx.host.num(room, "exit:4:key"), 0);

    assert!(step(scene.host()).is_empty());
    assert_eq!(
        heard_raw(&scene.fx.host, player)[1],
        "The trap door slams itself closed as if by magic.\r\n"
    );
    assert!(scene.fx.host.link(room, "exit:4").is_none());
    let slam = "The trap door slams itself closed as if by magic.";
    assert_eq!(
        scene.fx.host.effects,
        vec![
            sub_line(
                player,
                "The torch turns easily in your hand revealing a trap door."
            ),
            sub_line(
                watcher,
                "As Tester pulls the torch a trap door swings open leading upwards."
            ),
            act_line(player, slam),
            act_line(watcher, slam),
        ]
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// ---------------------------------------------------------------------
// Trigger 23400, room command `push`: `wdoor 23441 west room 23471` when room
// 23471 does not exist.
//
// flags a -> exit_info = 1, name door -> keyword "door". `room`:
// real_room(23471) is NOWHERE, so `newexit->to_room = NOWHERE` and
// wld_log(room, "wdoor: invalid door target (arg == %s)", value) writes
// "Room 23441 :: wdoor: invalid door target (arg == 23471)". The exit stays
// (it exists, leading nowhere).
// ---------------------------------------------------------------------
#[test]
fn trigger_23400_logs_a_door_to_a_room_that_does_not_exist() {
    let record = real!(23400);
    let mut scene = Scene::new();
    let player = scene.player();
    let room = scene.host().add_room(23441, "A Button Room");
    scene.host().char_to_room(player, room);
    install_record(scene.host(), room, &record);
    assert_eq!(
        dispatch::command_wtrigger(scene.host(), player, "push", " button"),
        1
    );
    assert_eq!(scene.fx.host.num(room, "exit:3:flags"), 1);
    assert_eq!(scene.fx.host.text(room, "exit:3:keyword"), "door");
    assert!(scene.fx.host.link(room, "exit:3").is_none());
    assert_ne!(scene.fx.host.num(room, "exit:3:nowhere"), 0);
    assert_eq!(
        scene.fx.host.effects,
        vec![sub_line(
            player,
            "You push the button, and reveal a secret door!"
        )]
    );
    assert_eq!(
        scene.fx.host.logs,
        vec!["Room 23441 :: wdoor: invalid door target (arg == 23471)"]
    );
}

// Trigger 23402, room enter: `wdoor 23471 east purge 0` in two other rooms.
// After `wait 2 sec` (20 pulses) both exits are freed (`purge` is field 0 and
// the extra word is ignored), and the wecho follows.
#[test]
fn trigger_23402_purges_the_secret_doors_after_two_seconds() {
    let record = real!(23402);
    let mut scene = Scene::new();
    let player = scene.player();
    let a = scene.host().add_room(23471, "East Room");
    let b = scene.host().add_room(23441, "West Room");
    scene.host().set_link(a, "exit:1", Some(b));
    scene.host().set_link(b, "exit:3", Some(a));
    scene.host().char_to_room(player, a);
    let id = install_record(scene.host(), a, &record);
    assert_eq!(dispatch::enter_wtrigger(scene.host(), a, player, 0), 1);
    assert_eq!(scene.fx.host.waits, vec![(a, id, 20)]);
    assert!(scene.fx.host.link(a, "exit:1").is_some());
    assert!(step(scene.host()).is_empty());
    assert!(scene.fx.host.link(a, "exit:1").is_none());
    assert!(scene.fx.host.link(b, "exit:3").is_none());
    assert_eq!(
        heard(&scene.fx.host, player),
        vec!["A wall slams down shut!"]
    );
    assert_eq!(
        scene.fx.host.effects,
        vec![act_line(player, "A wall slams down shut!")]
    );
    assert!(scene.fx.host.logs.is_empty());
}

// ---------------------------------------------------------------------
// `wait until` when the hour is already past, and the `t` unit: trigger 23700.
//
// A mob in room 23607 (`%inroom.vnum% == 23607`): `context %self.id%`,
// `set trader_leaving 1`, `global trader_leaving`; the switch takes `case
// 23607`: say, rest, `wait until 07:00`. min = 7*60 = 420; ntime = 420 * 75 *
// 10 / 60 = 5250. At game hour 22, pulse 40: when = 40 % 750 + 22 * 750 =
// 16540, which is >= ntime, so process_wait rolls over to the next day:
// when = (SECS_PER_MUD_DAY * PASSES_PER_SEC) - when + ntime = 18000 - 16540 +
// 5250 = 6710. After it: `stand`, `wait 1 t` = 1 * PULSES_PER_MUD_HOUR = 750.
// Then three `wait 6 s` (60), `say Let's go!`, `wait 1 s` (10), `south`, and
// `break` leaves the switch (to the `done`), after which the `else` skips the
// other branch.
// ---------------------------------------------------------------------
#[test]
fn trigger_23700_waits_until_dawn_across_midnight_then_one_hour() {
    let record = real!(23700);
    let mut scene = Scene::new();
    let room = scene.host().add_room(23607, "A Camp");
    let trader = scene.fx.host.add_mob(23700, "the trader");
    scene.host().char_to_room(trader, room);
    scene.host().pulse = 40;
    scene.host().time.hours = 22;
    let id = install_record(scene.host(), trader, &record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    dispatch::script_trigger_check(scene.host());
    assert_eq!(
        common::world::commands(&scene.fx.host),
        vec!["say We'll leave at dawn.", "rest"]
    );
    assert_eq!(scene.fx.host.waits, vec![(trader, id, 6710)]);

    assert_eq!(step(scene.host()), vec![(trader, id, 750)]);
    assert_eq!(common::world::commands(&scene.fx.host)[2], "stand");

    let mut waits = Vec::new();
    for _ in 0..5 {
        waits.push(step(scene.host()).first().map(|w| w.2));
    }
    assert_eq!(waits, vec![Some(60), Some(60), Some(60), Some(10), None]);
    assert_eq!(
        common::world::commands(&scene.fx.host)[3..],
        [
            "say Are you ready to find adventure, men ?",
            "say Are you ready to fight every hardheaded orc on the way ?",
            "say Don your gear, and follow  'll bring the name of Aldin to Radidan.",
            "say Let's go!",
            "south",
        ]
    );
    assert_eq!(
        global_var(&scene.fx.host, trader, "trader_leaving").as_deref(),
        Some("1")
    );
    assert!(scene.fx.host.effects.is_empty());
    // `context %self.id%` set sc->context to the mob's id and
    // `process_global(.., sc->context)` stored the global under it.
    let script = scene.fx.host.script(trader).unwrap();
    assert_eq!(script.context, trader);
    assert_eq!(
        script.global_vars.get("trader_leaving").unwrap().context,
        trader
    );
    assert!(scene.fx.host.logs.is_empty(), "{:?}", scene.fx.host.logs);
    assert_clean(&scene.fx.host);
}

// A global saved under a context is visible only while sc->context matches
// (dg_variables.c get_local_var: `vd->context==0 || vd->context==sc->context`),
// and script_driver resets `sc->context = 0` on every TRIG_NEW run. Trigger A
// does `context 77`, `set v 5`, `global v`: v is stored under context 77 and
// the script keeps context 77 when it ends. Trigger B (a new run) then starts
// with context 0, so `%v%` does not resolve (77 is neither 0 nor 0) and it
// takes the else branch; afterwards the script's context is still 0.
#[test]
fn a_new_run_resets_the_context_of_globals() {
    let mut scene = Scene::new();
    let mob = scene.add_owner_mob(3300, "a clerk");
    install(
        scene.host(),
        mob,
        9101,
        AttachType::Mob,
        "g",
        100,
        "",
        "context 77\nset v 5\nglobal v",
    );
    install(
        scene.host(),
        mob,
        9102,
        AttachType::Mob,
        "b",
        100,
        "",
        "if %v%\nsay seen\nelse\nsay hidden\nend",
    );
    let player = scene.player();
    scene.host().rolls.push_back(PERCENT_ROLL);
    assert_eq!(dispatch::greet_mtrigger(scene.host(), player, 0), 1);
    let script = scene.fx.host.script(mob).unwrap();
    assert_eq!(script.context, 77);
    assert_eq!(script.global_vars.get("v").unwrap().context, 77);
    assert_eq!(global_var(&scene.fx.host, mob, "v").as_deref(), Some("5"));

    scene.host().rolls.push_back(PERCENT_ROLL);
    dispatch::random_mtrigger(scene.host(), mob);
    assert_eq!(common::world::commands(&scene.fx.host), vec!["say hidden"]);
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.logs.is_empty());
    assert_eq!(scene.fx.host.script(mob).unwrap().context, 0);
    assert_clean(&scene.fx.host);
}

// Trigger 23700 in room 23602 with the global already set: `case 23602` waits
// 2 s (20 pulses) and falls through the following empty cases (`case 23601`,
// ... find_case matches the first, the driver ignores `case` lines) to the
// shared `south`, then `break`.
#[test]
fn trigger_23700_falls_through_empty_cases() {
    let record = real!(23700);
    let mut scene = Scene::new();
    let room = scene.host().add_room(23602, "A Road");
    let trader = scene.fx.host.add_mob(23700, "the trader");
    scene.host().char_to_room(trader, room);
    scene
        .host()
        .ensure_script(trader)
        .unwrap()
        .global_vars
        .add_var("trader_leaving", "1", 0);
    let id = install_record(scene.host(), trader, &record);
    scene.host().rolls.push_back(PERCENT_ROLL);
    dispatch::script_trigger_check(scene.host());
    assert_eq!(scene.fx.host.waits, vec![(trader, id, 20)]);
    assert!(common::world::commands(&scene.fx.host).is_empty());
    assert!(step(scene.host()).is_empty());
    assert_eq!(common::world::commands(&scene.fx.host), vec!["south"]);
    assert!(scene.fx.host.effects.is_empty());
    assert!(scene.fx.host.logs.is_empty());
    assert_clean(&scene.fx.host);
}
