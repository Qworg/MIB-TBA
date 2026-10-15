//! End-to-end behaviour tests: boot the real content and drive a player's
//! commands through the engine router and every intake.
//!
//! Tests may unwrap/expect.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod playtest;

use mib_core::prelude::*;
use mib_protocol::command::ExitKey;
use mib_stdlib::prelude::NpcId;
use mib_tba::combat::NpcDefeated;
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

/// `fight` (flag `k`) and `hitprcnt` (flag `l`) triggers fire from the
/// combat round, once each per round rather than per blow.
#[test]
fn combat_triggers_fire_once_per_round() {
    let mut play = Play::new("Aria");
    let room = play.room();
    play.spawn_npc(room, "tba_npc_5");
    play.define_trigger(9001, "mob", "k", 50, "", "say fight round\n");
    play.define_trigger(9002, "mob", "l", 50, "", "say hurt round\n");
    play.attach("npc:tba_npc_5", &[9001, 9002]);
    // One fixed tick gives the mob its combat state.
    play.tick(0.1);

    // The player starts the fight; the mob retaliates on the next round.
    let _ = play.send("attack tba_npc_5");
    for _ in 0..6 {
        play.round();
    }
    let out = play.text();
    let fight = out
        .iter()
        .filter(|line| line.contains("fight round"))
        .count();
    assert!(fight >= 1, "the fight trigger fired: {out:?}");
    assert!(
        fight <= 6,
        "once per round, not per blow ({fight} in 6 rounds)"
    );
}

/// The curated zones fire through their event paths: zone 74's ambient
/// `random` emote and a real kill's `death` trigger.
#[test]
fn curated_zone_74_random_and_a_death_trigger_fire() {
    let mut play = Play::new("Visitor");
    // Zone 74: `tba_trig_7408` (random, narg 100) emotes on the heartbeat.
    let r74 = play.spawn_room(65530);
    play.spawn_npc(r74, "tba_npc_7408");
    play.move_player(r74);
    play.tick(14.0);
    let out = play.text();
    assert!(
        out.iter().any(|line| line.contains("muses")),
        "zone 74 random fired: {out:?}"
    );

    // A kill runs the victim's `death` trigger (`tba_trig_65`).
    let room = play.room();
    let victim = play.spawn_npc(room, "tba_npc_146");
    let killer = play.session();
    mib_tba::dg::intake::combat::finish_kill(play.app.world_mut(), victim, killer);
    play.app.update();
    let out = play.text();
    assert!(
        out.iter().any(|line| line.contains("curses")),
        "the death trigger fired: {out:?}"
    );
}

/// The zone-1 "Kill the Mice!" quest advances on three mouse kills and pays
/// out on turn-in in front of its giver.
#[test]
fn kill_the_mice_progresses_and_rewards() {
    use mib_stdlib::content::QuestId;
    use mib_stdlib::prelude::{PlayerQuestLog, QuestIndex, QuestProgress};

    let mut play = Play::new("Aria");
    let room = play.room();
    play.spawn_npc(room, "tba_npc_179");
    let session = play.session();
    let quest_id = QuestId::from("tba_quest_100");
    let def = play
        .app
        .world()
        .resource::<QuestIndex>()
        .map
        .get(&quest_id)
        .cloned()
        .expect("Kill the Mice! is installed");
    play.app
        .world_mut()
        .entity_mut(session)
        .insert(PlayerQuestLog {
            quests: [(quest_id.clone(), QuestProgress::fresh(&def))]
                .into_iter()
                .collect(),
        });

    for _ in 0..3 {
        let victim = play.spawn_npc(room, "tba_npc_194");
        play.app
            .world_mut()
            .resource_mut::<Messages<NpcDefeated>>()
            .write(NpcDefeated {
                victim,
                killer: session,
                npc: NpcId::from("tba_npc_194"),
                experience: 0,
                gold: 0,
                alignment: 0,
            });
    }
    play.tick(0.1);
    // The kill bridge runs in `Update`, `advance_quests` in `FixedUpdate`, so
    // the events are credited on the next tick.
    play.tick(0.1);
    let ready = play
        .app
        .world()
        .get::<PlayerQuestLog>(session)
        .and_then(|log| log.quests.get(&quest_id))
        .is_some_and(|progress| progress.ready);
    assert!(ready, "three mice credited and the quest is ready");

    let out = play.send("turnin tba_quest_100");
    let done = play
        .app
        .world()
        .get::<PlayerQuestLog>(session)
        .is_none_or(|log| !log.quests.contains_key(&quest_id));
    assert!(done, "the completed quest left the log: {out:?}");
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
