//! Script commands run the way the game runs them: a trigger's lines go
//! through `script_driver`, which substitutes `%actor%` and `%self%` and
//! hands what is left to the command interpreters.
//!
//! The load, enter and act triggers these commands raise are the dispatch
//! module's. Where a test needs one to fire it says what the trigger writes
//! and asserts that everything written is right; with the dispatch stubs the
//! trigger does not fire and there is nothing to check.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::support::World;
use crate::driver::{Mode, script_driver};
use crate::host::{BaseHost, WorldHost};
use crate::types::{AttachType, Uid, uid_text};

/// Attach a new trigger with `script` to `owner`; the id of its instance.
fn attach(w: &mut World, owner: Uid, attach: AttachType, types: &str, script: &str) -> u64 {
    let vnum = 1000 + i32::try_from(w.host.protos.len()).unwrap();
    let proto = w.host.add_proto(vnum, attach, types, 100, "", script);
    w.host.attach(owner, &proto).unwrap()
}

/// Give the instance a trigger variable, as the code that fires an event does.
fn set_var(w: &mut World, owner: Uid, id: u64, name: &str, value: &str) {
    w.host
        .script_mut(owner)
        .and_then(|script| script.trigger_mut(id))
        .unwrap()
        .vars
        .add_var(name, value, 0);
}

fn run(w: &mut World, owner: Uid, id: u64) -> i32 {
    script_driver(&mut w.host, owner, id, Mode::New)
}

/// Run `script` as a global trigger of `owner` with `%actor%` set to `actor`.
fn run_script(w: &mut World, owner: Uid, script: &str, actor: Option<Uid>) {
    let kind = AttachType::from(w.host.kind(owner).unwrap());
    let id = attach(w, owner, kind, "a", script);
    if let Some(actor) = actor {
        set_var(w, owner, id, "actor", &uid_text(actor));
    }
    run(w, owner, id);
}

fn logs(w: &World) -> Vec<String> {
    w.host.logs.clone()
}

const MOB: &str = "Mob (the guard, VNum 3000):: ";
const OBJ: &str = "Obj (a lamp, VNum 7000):: ";
const WLD: &str = "Room 3001 :: ";

// ---- an owner purging itself ------------------------------------------

#[test]
fn a_mob_that_purges_itself_does_not_run_another_line() {
    let mut w = World::new();
    let mob = w.mob;
    run_script(&mut w, mob, "mlog before\nmpurge %self%\nmlog after", None);
    assert_eq!(logs(&w), [format!("{MOB}before")]);
    // The mob is only marked: it stays until the end of the pulse.
    assert!(w.host.exists(mob));
    assert!(w.host.is_dead(mob));
    assert!(w.host.owner_purged);
    assert_eq!(w.host.depth, 0);
}

#[test]
fn a_mob_that_purges_another_mob_goes_on() {
    let mut w = World::new();
    let mob = w.mob;
    let rat = w.host.add_mob(3100, "a rat");
    w.host.set_text(rat, "alias", "rat");
    w.host.char_to_room(rat, w.room);
    run_script(&mut w, mob, "mpurge rat\nmlog after", None);
    assert!(w.host.is_dead(rat));
    assert!(!w.host.is_dead(mob));
    assert!(!w.host.owner_purged);
    assert_eq!(logs(&w), [format!("{MOB}after")]);
}

#[test]
fn an_object_that_purges_itself_does_not_run_another_line() {
    let mut w = World::new();
    let lamp = w.obj;
    run_script(&mut w, lamp, "olog before\nopurge %self%\nolog after", None);
    assert_eq!(logs(&w), [format!("{OBJ}before")]);
    assert!(!w.host.exists(lamp));
    assert!(w.host.owner_purged);
    assert_eq!(w.host.depth, 0);
}

#[test]
fn an_object_that_purges_its_container_does_not_run_another_line() {
    let mut w = World::new();
    let (lamp, room) = (w.obj, w.room);
    let sack = w.host.add_obj(7100, "a sack");
    w.host.obj_to_room(sack, room);
    w.host.obj_to_obj(lamp, sack);
    let script = format!("olog before\nopurge {}\nolog after", uid_text(sack));
    run_script(&mut w, lamp, &script, None);
    assert_eq!(logs(&w), [format!("{OBJ}before")]);
    // The container took the running object with it.
    assert!(!w.host.exists(sack));
    assert!(!w.host.exists(lamp));
    assert!(w.host.owner_purged);
}

#[test]
fn an_object_that_purges_something_else_goes_on() {
    let mut w = World::new();
    let (lamp, room) = (w.obj, w.room);
    let sack = w.host.add_obj(7100, "a sack");
    let rock = w.host.add_obj(7102, "a rock");
    w.host.obj_to_room(sack, room);
    w.host.obj_to_room(rock, room);
    w.host.obj_to_obj(lamp, sack);
    let script = format!("opurge {}\nolog after", uid_text(rock));
    run_script(&mut w, lamp, &script, None);
    assert!(!w.host.exists(rock));
    assert!(w.host.exists(lamp));
    assert!(!w.host.owner_purged);
    assert_eq!(logs(&w), [format!("{OBJ}after")]);
}

#[test]
fn a_room_script_purging_everything_goes_on() {
    let mut w = World::new();
    let (room, lamp, mob) = (w.room, w.obj, w.mob);
    run_script(&mut w, room, "wpurge\nwlog after", None);
    assert!(!w.host.exists(lamp));
    assert!(w.host.is_dead(mob));
    assert!(!w.host.owner_purged);
    assert_eq!(logs(&w), [format!("{WLD}after")]);
}

// ---- oat and the owner-purged flag ------------------------------------

#[test]
fn oat_puts_the_flag_back_when_the_copy_purges_itself() {
    let mut w = World::new();
    let lamp = w.obj;
    // The copy is the only lamp in room 3002.
    run_script(&mut w, lamp, "oat 3002 opurge lamp\nolog survived", None);
    assert!(w.host.exists(lamp));
    assert!(!w.host.owner_purged);
    assert_eq!(logs(&w), [format!("{OBJ}survived")]);
    assert_eq!(w.host.object_list, [lamp]);
}

#[test]
fn oat_stops_the_script_when_the_command_purges_the_real_object() {
    let mut w = World::new();
    let lamp = w.obj;
    let script = format!("oat 3002 opurge {}\nolog after", uid_text(lamp));
    run_script(&mut w, lamp, &script, None);
    assert!(!w.host.exists(lamp));
    assert!(w.host.owner_purged);
    assert!(logs(&w).is_empty());
    // The copy went too.
    assert!(w.host.object_list.is_empty());
}

#[test]
fn oat_hands_back_the_flag_it_was_given() {
    let mut w = World::new();
    let lamp = w.obj;
    *w.host.owner_purged_mut() = true;
    w.run_obj(lamp, "oat 3002 olog x");
    assert!(w.host.owner_purged);
    *w.host.owner_purged_mut() = false;
    w.run_obj(lamp, "oat 3002 olog x");
    assert!(!w.host.owner_purged);
}

// ---- force and at with a real target ----------------------------------

#[test]
fn force_reaches_the_actor_named_by_id_for_every_owner() {
    let mut w = World::new();
    let (mob, lamp, room, alice) = (w.mob, w.obj, w.room, w.player);
    run_script(&mut w, mob, "mforce %actor% say hello", Some(alice));
    run_script(&mut w, lamp, "oforce %actor% say hello", Some(alice));
    run_script(&mut w, room, "wforce %actor% say hello", Some(alice));
    // The line keeps the space that followed the name.
    assert_eq!(
        w.host.mud_commands,
        vec![(alice, " say hello".to_owned()); 3]
    );
    assert!(w.host.logs.is_empty());
}

#[test]
fn force_of_the_owner_itself_is_refused() {
    let mut w = World::new();
    let mob = w.mob;
    run_script(&mut w, mob, "mforce %self% say hello", None);
    assert!(w.host.mud_commands.is_empty());
    assert_eq!(logs(&w), [format!("{MOB}mforce: forcing self")]);
}

#[test]
fn at_runs_the_command_for_the_owner_and_puts_it_back() {
    let mut w = World::new();
    let (mob, bob, room, room2) = (w.mob, w.player2, w.room, w.room2);
    w.host.char_to_room(bob, room2);
    run_script(&mut w, mob, "mat %actor.name% say hi", Some(bob));
    assert_eq!(w.host.mud_commands, [(mob, " say hi".to_owned())]);
    assert_eq!(w.host.in_room(mob), Some(room));
    assert!(w.host.logs.is_empty());
}

#[test]
fn oat_and_wat_run_in_the_room_of_the_actor() {
    let mut w = World::new();
    let (lamp, room, bob, room2) = (w.obj, w.room, w.player2, w.room2);
    w.host.char_to_room(bob, room2);
    run_script(&mut w, lamp, "oat %actor.name% olog from there", Some(bob));
    assert_eq!(logs(&w), [format!("{OBJ}from there")]);
    w.clear();
    run_script(&mut w, room, "wat %actor.name% wlog from there", Some(bob));
    assert_eq!(logs(&w), ["Room 3002 :: from there"]);
}

// ---- the names sub_write is given -------------------------------------

#[test]
fn mob_messages_name_the_actor_and_the_mob_as_each_viewer_sees_them() {
    let mut w = World::new();
    let (mob, alice, bob) = (w.mob, w.player, w.player2);
    w.host.set_text(mob, "alias", "guard");
    run_script(
        &mut w,
        mob,
        "mechoaround %actor% ~%actor.name% looks at ~%self.alias% and |%actor.name% shadow shrinks.\n\
         msend %actor% You see ~%self.alias% flinch, ~%actor.name%.",
        Some(alice),
    );
    assert_eq!(
        w.sent_to(bob),
        ["Alice looks at the guard and Alice's shadow shrinks.\n\r"]
    );
    assert_eq!(w.sent_to(alice), ["You see the guard flinch, you.\n\r"]);
}

#[test]
fn mecho_from_a_script_reads_the_actor_by_name() {
    let mut w = World::new();
    let (mob, alice, bob) = (w.mob, w.player, w.player2);
    w.host.set_num(bob, "sex", 1);
    run_script(
        &mut w,
        mob,
        "mecho ~%actor.name% grins at ^%actor.name% own reflection and &%actor.name% leaves.",
        Some(bob),
    );
    assert_eq!(
        w.sent_to(alice),
        ["Bob grins at his own reflection and he leaves.\n\r"]
    );
    assert_eq!(
        w.sent_to(bob),
        ["you grins at your own reflection and you leaves.\n\r"]
    );
}

#[test]
fn object_and_room_messages_name_the_actor_and_the_object() {
    let mut w = World::new();
    let (lamp, room, alice, bob) = (w.obj, w.room, w.player, w.player2);
    run_script(
        &mut w,
        lamp,
        "osend %actor% You touch `%self.name%, ~%actor.name%.\n\
         oechoaround %actor% ~%actor.name% touches `%self.name%.",
        Some(alice),
    );
    assert_eq!(w.sent_to(alice), ["You touch a lamp, you.\n\r"]);
    assert_eq!(w.sent_to(bob), ["Alice touches a lamp.\n\r"]);
    w.clear();
    run_script(
        &mut w,
        room,
        "wsend %actor% ~%actor.name% feels |%actor.name% skin crawl.\n\
         wechoaround %actor% ~%actor.name% shivers.",
        Some(bob),
    );
    assert_eq!(w.sent_to(bob), ["you feels your skin crawl.\n\r"]);
    assert_eq!(w.sent_to(alice), ["Bob shivers.\n\r"]);
}

// ---- load: lastloaded is set before the load trigger ------------------
//
// These tests need the dispatch bodies (`load_mtrigger`, `load_otrigger`,
// `enter_wtrigger`); they are ignored while those are stubs.

/// A trigger for what gets loaded: it reads the loader's `lastloaded` through
/// a variable holding the loader's id and says what it found.
fn load_watcher(w: &mut World, loader: Uid, key: &str, vnum: i32, types: &str, kind: AttachType) {
    let script = format!(
        "set loader {}\n{}log saw %loader.lastloaded%",
        uid_text(loader),
        match kind {
            AttachType::Mob => 'm',
            AttachType::Obj => 'o',
            AttachType::Wld => 'w',
        }
    );
    w.host.add_proto(2000 + vnum, kind, types, 100, "", &script);
    w.host
        .globals
        .nums
        .insert(format!("{key}:{vnum}"), i64::from(2000 + vnum));
}

#[test]
#[ignore = "needs the dispatch bodies: load_mtrigger must fire the load trigger"]
fn mload_records_lastloaded_before_the_new_mob_is_told_it_loaded() {
    let mut w = World::new();
    let mob = w.mob;
    load_watcher(&mut w, mob, "mobtrig", 3100, "n", AttachType::Mob);
    run_script(&mut w, mob, "mload mob 3100\nmlog now %lastloaded%", None);
    let rat = w.host.character_list[0];
    assert_eq!(w.host.vnum(rat), 3100);
    let id = uid_text(rat);
    // The new mob's trigger ran once, and saw the new id.
    assert_eq!(
        logs(&w),
        [
            format!("Mob (a rat, VNum 3100):: saw {id}"),
            format!("{MOB}now {id}")
        ]
    );
}

#[test]
#[ignore = "needs the dispatch bodies: load_otrigger must fire the load trigger"]
fn every_way_mload_obj_places_an_object_tells_it_after_lastloaded_is_set() {
    // No target (lying in the room), a person, a worn position, a container
    // and a target that is neither a person nor a container.
    for line in [
        "mload obj 7102",
        "mload obj 7102 alice",
        "mload obj 7102 sack",
        "mload obj 7102 lamp",
        "mload obj 7101 alice rfinger",
    ] {
        let mut w = World::new();
        let (mob, room) = (w.mob, w.room);
        let sack = w.host.add_obj(7100, "a sack");
        w.host.set_text(sack, "alias", "sack");
        w.host.set_num(sack, "type", 15);
        w.host.obj_to_room(sack, room);
        let vnum = if line.contains("7101") { 7101 } else { 7102 };
        load_watcher(&mut w, mob, "objtrig", vnum, "n", AttachType::Obj);
        run_script(&mut w, mob, &format!("{line}\nmlog now %lastloaded%"), None);
        let object = w.host.object_list[0];
        assert_eq!(w.host.vnum(object), vnum, "{line}");
        let id = uid_text(object);
        let name = if vnum == 7101 { "a ring" } else { "a rock" };
        assert_eq!(
            logs(&w),
            [
                format!("Obj ({name}, VNum {vnum}):: saw {id}"),
                format!("{MOB}now {id}")
            ],
            "{line}"
        );
    }
}

#[test]
#[ignore = "needs the dispatch bodies: load_otrigger must fire the load trigger"]
fn oload_and_wload_record_lastloaded_the_way_their_c_does() {
    let mut w = World::new();
    let (lamp, room, alice) = (w.obj, w.room, w.player);
    load_watcher(&mut w, lamp, "mobtrig", 3100, "n", AttachType::Mob);
    run_script(&mut w, lamp, "oload mob 3100\nolog now %lastloaded%", None);
    let rat = w.host.character_list[0];
    assert_eq!(w.host.vnum(rat), 3100);
    let id = uid_text(rat);
    assert_eq!(
        logs(&w),
        [
            format!("Mob (a rat, VNum 3100):: saw {id}"),
            format!("{OBJ}now {id}")
        ]
    );

    // A room records an object only when it is loaded without a target.
    w.clear();
    run_script(
        &mut w,
        room,
        "wload obj 7102 alice\nwlog [%lastloaded%]",
        None,
    );
    assert_eq!(logs(&w), [format!("{WLD}[]")]);
    assert_eq!(w.host.carrying(alice).len(), 1);
    w.clear();
    run_script(&mut w, room, "wload obj 7102\nwlog [%lastloaded%]", None);
    let rock = w.host.room_contents(room).last().copied().unwrap();
    assert_eq!(logs(&w), [format!("{WLD}[{}]", uid_text(rock))]);
}

#[test]
fn mload_records_lastloaded_for_the_script_that_runs_it() {
    // Without the dispatch bodies: the id is there for the next line.
    let mut w = World::new();
    let mob = w.mob;
    run_script(&mut w, mob, "mload mob 3100\nmlog now %lastloaded%", None);
    let rat = w.host.character_list[0];
    assert_eq!(logs(&w), [format!("{MOB}now {}", uid_text(rat))]);
    w.clear();
    run_script(&mut w, mob, "mload obj 7102\nmlog now %lastloaded%", None);
    let rock = w.host.object_list[0];
    assert_eq!(logs(&w), [format!("{MOB}now {}", uid_text(rock))]);
}

// ---- teleport raises the enter trigger of the room --------------------

/// A room trigger that says who entered.
fn entry_watcher(w: &mut World, room: Uid) {
    attach(w, room, AttachType::Wld, "g", "wlog entered %actor.name%");
}

#[test]
#[ignore = "needs the dispatch bodies: enter_wtrigger must fire the enter trigger"]
fn every_script_teleport_and_goto_tells_the_new_room() {
    for (owner, line, who) in [
        ("mob", "mteleport %actor% 3002", "Alice"),
        ("mob", "mgoto 3002", "the guard"),
        ("obj", "oteleport %actor% 3002", "Alice"),
        ("wld", "wteleport %actor% 3002", "Alice"),
    ] {
        let mut w = World::new();
        let (mob, lamp, room, room2, alice) = (w.mob, w.obj, w.room, w.room2, w.player);
        entry_watcher(&mut w, room2);
        let owner = match owner {
            "mob" => mob,
            "obj" => lamp,
            _ => room,
        };
        run_script(&mut w, owner, line, Some(alice));
        // The trigger ran once and found the traveller already in the room.
        assert_eq!(logs(&w), [format!("Room 3002 :: entered {who}")], "{line}");
        let traveller = if who == "Alice" { alice } else { mob };
        assert_eq!(w.host.in_room(traveller), Some(room2), "{line}");
    }
}

#[test]
fn teleport_moves_the_actor() {
    let mut w = World::new();
    let (mob, room2, alice) = (w.mob, w.room2, w.player);
    run_script(&mut w, mob, "mteleport %actor% 3002", Some(alice));
    assert_eq!(w.host.in_room(alice), Some(room2));
}
