//! End-to-end behaviour tests: boot the real content and drive a player's
//! commands through the engine router and every intake.
//!
//! Tests may unwrap/expect.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod playtest;

use mib_protocol::command::ExitKey;
use playtest::Play;

#[test]
fn a_logged_in_player_can_look() {
    let mut play = Play::new("Tester");
    let out = play.send("look");
    assert!(!out.is_empty(), "look produced output: {out:?}");
}

/// A `command` trigger that answers in two parts separated by `wait`: the
/// second line must not appear until the wait elapses.
#[test]
fn a_wait_suspends_and_resumes_in_order() {
    let mut play = Play::new("Aria");
    let room = play.room();
    play.spawn_npc(room, "tba_npc_5");
    play.define_trigger(
        9001,
        "mob",
        "c",
        0,
        "hello",
        "say first line\nwait 1 sec\nsay second line\n",
    );
    play.attach("npc:tba_npc_5", &[9001]);

    let out = play.send("hello");
    assert!(
        out.iter().any(|line| line.contains("first line")),
        "the first line ran: {out:?}"
    );
    assert!(
        !out.iter().any(|line| line.contains("second line")),
        "the second waits: {out:?}"
    );

    play.tick(1.0);
    let out = play.text();
    assert!(
        out.iter().any(|line| line.contains("second line")),
        "the second line resumed after the wait: {out:?}"
    );
}

/// A `leave` trigger that returns zero blocks the move.
#[test]
fn a_leave_trigger_can_block_a_move() {
    let mut play = Play::new("Aria");
    // Vnums outside the content, so the room gets a script id.
    let here = play.spawn_room(65530);
    let there = play.spawn_room(65531);
    play.link(here, ExitKey::North, there);
    play.spawn_npc(here, "tba_npc_5");
    play.define_trigger(
        9001,
        "mob",
        "q",
        100,
        "*",
        "say you shall not pass\nreturn 0\n",
    );
    play.attach("npc:tba_npc_5", &[9001]);
    play.move_player(here);

    let out = play.send("north");
    let session = play.session();
    assert_eq!(
        play.location(session),
        Some(here),
        "the leave trigger blocked the move"
    );
    assert!(
        out.iter().any(|line| line.contains("shall not pass")),
        "the trigger spoke: {out:?}"
    );
}

/// The real `tba_trig_66` greets, teleports from the `down` direction and
/// forces a `look`.
#[test]
#[ignore = "the greet dispatch path in the headless playtest harness needs work; covered by the dg_intake_commands suite"]
fn a_greet_can_teleport_and_force_a_command() {
    let mut play = Play::new("Aria");
    let above = play.spawn_room(65530);
    let here = play.spawn_room(65531);
    play.link(above, ExitKey::Down, here);
    let destination = play.room_by_key("tba.1300");
    play.spawn_npc(here, "tba_npc_66");
    // The converter does not attach `tba_trig_66` to the unplaced NPC.
    play.attach("npc:tba_npc_66", &[66]);
    play.move_player(above);

    let _ = play.send("down");
    play.tick(0.1);
    let session = play.session();
    assert_eq!(
        play.location(session),
        Some(destination),
        "the greet teleported the player to room 1300"
    );
    let out = play.text();
    assert!(
        out.iter().any(|line| line.contains("welcome")),
        "the greet spoke: {out:?}"
    );
}

/// A `command` trigger that `%purge%`s its own owner must not wedge the run.
#[test]
fn a_trigger_that_purges_itself_is_cleaned_up() {
    let mut play = Play::new("Aria");
    let room = play.room();
    let npc = play.spawn_npc(room, "tba_npc_5");
    play.define_trigger(9001, "mob", "c", 0, "*", "%purge% %self%\n");
    play.attach("npc:tba_npc_5", &[9001]);

    play.send("ping");
    assert!(
        play.app.world().get_entity(npc).is_err(),
        "the owner was purged"
    );
    // Another command still works.
    let out = play.send("look");
    assert!(!out.is_empty(), "the game still runs: {out:?}");
}
