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

/// The real `tba_trig_66` greets, teleports when the player arrived from the
/// `down` direction (so moved up) and forces a `look`.
#[test]
fn a_greet_can_teleport_and_force_a_command() {
    let mut play = Play::new("Aria");
    let below = play.spawn_room(65530);
    let here = play.spawn_room(65531);
    play.link(below, ExitKey::Up, here);
    let destination = play.room_by_key("tba.1300");
    play.spawn_npc(here, "tba_npc_66");
    // The converter does not attach `tba_trig_66` to the unplaced NPC.
    play.attach("npc:tba_npc_66", &[66]);
    play.move_player(below);

    let mut out = play.send("up");
    play.tick(0.1);
    out.extend(play.text());
    let session = play.session();
    assert_eq!(
        play.location(session),
        Some(destination),
        "the greet teleported the player to room 1300"
    );
    assert!(
        out.iter().any(|line| line.contains("welcome")),
        "the greet spoke: {out:?}"
    );
}

/// The zone-0 Tutorial Quest 1317 opening: the questmaster greets a player who
/// arrives from the south, and accepts a `yes` after its own waits.
#[test]
fn tutorial_1317_greets_and_accepts() {
    let mut play = Play::new("Aria");
    let south = play.spawn_room(65530);
    let room = play.spawn_room(65531);
    play.link(south, ExitKey::North, room);
    play.spawn_npc(room, "tba_npc_1317");
    // The converter attaches the 1300-series triggers, not 9-11.
    play.attach("npc:tba_npc_1317", &[9, 10]);
    play.move_player(south);

    let mut out = play.send("north");
    play.tick(1.0);
    out.extend(play.text());
    assert!(
        out.iter()
            .any(|line| line.contains("Could you find me the magic eight ball")),
        "the greet spoke: {out:?}"
    );

    let mut out = play.send("say yes");
    play.tick(1.0);
    out.extend(play.text());
    assert!(
        out.iter().any(|line| line.contains("I'll make this easy")),
        "the accept speech spoke: {out:?}"
    );
}

/// The real `tba_trig_3000` leaves on the Mages' Guild guard: a warrior
/// walking south into the guild is refused.
#[test]
fn a_guild_guard_blocks_a_wrong_class_from_the_guild() {
    let mut play = Play::new("Aria");
    let entrance = play.room_by_key("tba.3017");
    // The zone reset that places the guard may not have run yet.
    play.spawn_npc(entrance, "tba_npc_3024");
    play.move_player(entrance);

    let out = play.send("south");
    let session = play.session();
    assert_eq!(
        play.location(session),
        Some(entrance),
        "the guild guard refused the warrior: {out:?}"
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
