//! Scenario tests on real tbaMUD mob triggers: guild guards, the Builder
//! Academy tutorial quest, and the questmaster.
//!
//! Each expectation is worked out by hand from the C (`dg_scripts.c`,
//! `dg_variables.c`, `dg_mobcmd.c`, `dg_comm.c`) for the world and event set
//! up in the test, not read back from the VM. The reasoning is in the comment
//! above each test. Events are fired the way `dg_triggers.c` would: the
//! event's variables go onto the instance, then `script_driver` runs it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::scene::Scene as CommonScene;
use common::world::{commands, fire, global_var, install_record, local_var, run_waits, uid};
use dg_vm::driver::{Mode, script_driver};
use dg_vm::host::{BaseHost, WorldHost};
use dg_vm::mock::MockHost;
use dg_vm::types::Uid;

/// The standard scene plus a mob that carries the trigger.
struct Scene {
    inner: CommonScene,
    owner: Uid,
}

impl std::ops::Deref for Scene {
    type Target = CommonScene;
    fn deref(&self) -> &CommonScene {
        &self.inner
    }
}

impl std::ops::DerefMut for Scene {
    fn deref_mut(&mut self) -> &mut CommonScene {
        &mut self.inner
    }
}

impl Scene {
    fn new(owner_vnum: i32, owner_name: &str) -> Self {
        let mut inner = CommonScene::new();
        let owner = inner.add_owner_mob(owner_vnum, owner_name);
        Self { inner, owner }
    }

    /// A global variable on a character's script.
    fn set_global(&mut self, ch: Uid, name: &str, value: &str) {
        self.fx
            .host
            .ensure_script(ch)
            .expect("character exists")
            .global_vars
            .add_var(name, value, 0);
    }
}

fn say_all(host: &MockHost) -> Vec<String> {
    commands(host)
}

// ---------------------------------------------------------------------
// Guild guards 3000-3003 (leave triggers, `0 q 100`).
//
// dg_triggers.c leave_mtrigger adds `actor` and `direction` (dirs[dir]) and
// returns script_driver's value, so `return 0` blocks the move. The script
// compares %direction% to the guard's side with `==` (string compare when
// not both numbers), then `%actor.class%` (sprinttype over pc_class_types:
// "Magic User", "Cleric", "Thief", "Warrior") against the guild's class with
// `!=`. A non-member heading into the guild gets `return 0`, a `msend` to
// the actor ("The guard humiliates you, and blocks your way.\n\r", sub_write
// appends "\n\r") and an `mechoaround` to everyone else in the room with a
// descriptor, with %actor.hisher% = HSHR = "his" for SEX_MALE.
// ---------------------------------------------------------------------

const CLASS_NAMES: [&str; 4] = ["Magic User", "Cleric", "Thief", "Warrior"];

fn guild_guard(vnum: i32, blocked_dir: &str, guild_class: i64) {
    let record = real!(vnum);
    for class in 0..4i64 {
        for dir in ["north", "east", "south", "west"] {
            let mut scene = Scene::new(3024, "the guard");
            let (owner, player, watcher) = (scene.owner, scene.fx.player, scene.watcher);
            let id = install_record(scene.host(), owner, &record);
            scene.host().set_num(player, "class", class);
            let result = fire(
                scene.host(),
                owner,
                id,
                &[("actor", uid(player)), ("direction", dir.to_owned())],
            );
            let label = format!(
                "trigger {vnum} class {} dir {dir}",
                CLASS_NAMES[class as usize]
            );
            if dir == blocked_dir && class != guild_class {
                assert_eq!(result, 0, "{label}");
                assert_eq!(
                    scene.fx.host.effects,
                    vec![
                        format!(
                            "send {player}: The guard humiliates you, and blocks your way.\n\r"
                        ),
                        format!(
                            "send {watcher}: The guard humiliates Tester, and blocks his way.\n\r"
                        ),
                    ],
                    "{label}"
                );
            } else {
                assert_eq!(result, 1, "{label}");
                assert!(scene.fx.host.effects.is_empty(), "{label}");
            }
            assert!(scene.fx.host.mud_commands.is_empty(), "{label}");
            assert!(scene.fx.host.logs.is_empty(), "{label}");
            assert_eq!(scene.fx.host.depth, 0);
            assert!(scene.fx.host.waits.is_empty());
        }
    }
}

#[test]
fn guild_guard_3000_blocks_non_mages_going_south() {
    guild_guard(3000, "south", 0);
}

#[test]
fn guild_guard_3001_blocks_non_clerics_going_north() {
    guild_guard(3001, "north", 1);
}

#[test]
fn guild_guard_3002_blocks_non_thieves_going_east() {
    guild_guard(3002, "east", 2);
}

#[test]
fn guild_guard_3003_blocks_non_warriors_going_east() {
    guild_guard(3003, "east", 3);
}

// ---------------------------------------------------------------------
// Tutorial quest 9 (greet), 10 (speech "yes"), 11 (receive); zone 0.
// ---------------------------------------------------------------------

const SOLVED: &str = "solved_tutorial_quest_zone_0";

// Trigger 9, `0 g 100`, a player arrives from the south who has not solved
// the quest. greet_mtrigger passes `direction` = dirs[rev_dir[dir]]; the
// script tests `%direction% == south && %actor.varexists(solved...)%`: the
// varexists is "0", so `south == south && 0` is false and the else branch
// runs. `wait 1 sec`: process_wait takes "1 sec", sscanf "%ld %c" gives 1 and
// 's', so 1 * PASSES_PER_SEC = 10 pulses, and the run ends there returning
// the default 1 with the trigger still active (depth != 0, so it cannot fire
// again). On resume (curr_state = the line after the wait) the two `say`s go
// to command_interpreter as the mob, then the end of the script frees the
// variables and idles the trigger.
#[test]
fn tutorial_9_greets_a_newcomer_after_one_second() {
    let record = real!(9);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    let id = install_record(scene.host(), owner, &record);
    let ret = fire(
        scene.host(),
        owner,
        id,
        &[("actor", uid(player)), ("direction", "south".to_owned())],
    );
    assert_eq!(ret, 1);
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 10)]);
    assert!(say_all(&scene.fx.host).is_empty());
    // Waiting: the trigger is busy and keeps its variables.
    let trigger = scene.fx.host.script(owner).unwrap().trigger(id).unwrap();
    assert!(trigger.is_busy());
    assert_eq!(
        local_var(&scene.fx.host, owner, id, "direction").as_deref(),
        Some("south")
    );

    assert_eq!(run_waits(scene.host(), 5), 1);
    assert_eq!(
        say_all(&scene.fx.host),
        vec![
            "say Hello, Tester. Could you find me the magic eight ball?",
            "say Please say yes, Tester.",
        ]
    );
    assert!(scene.fx.host.waits.is_empty());
    let trigger = scene.fx.host.script(owner).unwrap().trigger(id).unwrap();
    assert!(!trigger.is_busy());
    assert!(trigger.vars.is_empty());
    assert_eq!(scene.fx.host.depth, 0);
    assert!(scene.fx.host.logs.is_empty());
}

// Trigger 9 again, now the player returns from the south and already has the
// variable. `%actor.varexists(x)%` is "1" because the player's script has the
// global, so the first branch runs: wait 1 sec (10 pulses), say, wait 2 sec
// (20 pulses), say, then `rdelete name %actor.id%`: the id substitutes to the
// bare number, atoi gives it, find_char finds the player and the first
// global named so (context 0) is unlinked. A player arriving from the north
// takes the else branch even with the variable.
#[test]
fn tutorial_9_lets_a_returning_player_retry_and_clears_the_flag() {
    let record = real!(9);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    scene.set_global(player, SOLVED, "1");
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("direction", "south".to_owned())];
    assert_eq!(fire(scene.host(), owner, id, &vars), 1);
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 10)]);

    scene.host().waits.clear();
    assert_eq!(dg_vm::driver::resume(scene.host(), owner, id), 0);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say you have already completed this quest."]
    );
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 20)]);
    assert_eq!(
        global_var(&scene.fx.host, player, SOLVED).as_deref(),
        Some("1")
    );

    scene.host().waits.clear();
    dg_vm::driver::resume(scene.host(), owner, id);
    assert_eq!(
        say_all(&scene.fx.host),
        vec![
            "say you have already completed this quest.",
            "say but you can try it again if you so desire. Would you like to find the magic eight ball again?",
        ]
    );
    assert_eq!(global_var(&scene.fx.host, player, SOLVED), None);
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.logs.is_empty());

    // From the north the first condition is false whatever the player has.
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    scene.set_global(player, SOLVED, "1");
    let id = install_record(scene.host(), owner, &record);
    fire(
        scene.host(),
        owner,
        id,
        &[("actor", uid(player)), ("direction", "north".to_owned())],
    );
    run_waits(scene.host(), 5);
    assert_eq!(
        say_all(&scene.fx.host),
        vec![
            "say Hello, Tester. Could you find me the magic eight ball?",
            "say Please say yes, Tester.",
        ]
    );
    assert_eq!(
        global_var(&scene.fx.host, player, SOLVED).as_deref(),
        Some("1")
    );
}

// Trigger 10, `0 d 1` with argument "yes": the player has not solved the
// quest, so `if !%actor.varexists(...)%` is `!0`, true. Three waits follow:
// 1 sec, 3 sec and 1 sec = 10, 30, 10 pulses, with a `say` after the first
// two. The third wait ends the script: resuming it finds nothing left
// (curr_state is the C's NULL), so it finishes.
#[test]
fn tutorial_10_gives_the_hint_over_three_waits() {
    let record = real!(10);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("speech", "yes".to_owned())];
    assert_eq!(fire(scene.host(), owner, id, &vars), 1);
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 10)]);
    assert!(say_all(&scene.fx.host).is_empty());

    scene.host().waits.clear();
    dg_vm::driver::resume(scene.host(), owner, id);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say Perfect, Tester. I'll make this easy. It is to the east."]
    );
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 30)]);

    scene.host().waits.clear();
    dg_vm::driver::resume(scene.host(), owner, id);
    assert_eq!(say_all(&scene.fx.host).len(), 2);
    assert_eq!(
        say_all(&scene.fx.host)[1],
        "say I'd go get it myself, but I'm lazy and you need the exercise."
    );
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 10)]);

    scene.host().waits.clear();
    dg_vm::driver::resume(scene.host(), owner, id);
    assert!(scene.fx.host.waits.is_empty());
    let trigger = scene.fx.host.script(owner).unwrap().trigger(id).unwrap();
    assert!(!trigger.is_busy());
    assert_eq!(say_all(&scene.fx.host).len(), 2);
}

// Trigger 10 with the quest already solved: `!1` is 0, the whole body is
// skipped (find_else_end runs to `end`), so nothing is said and no wait is
// queued; the run finishes at once.
#[test]
fn tutorial_10_stays_silent_once_the_quest_is_solved() {
    let record = real!(10);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    scene.set_global(player, SOLVED, "1");
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("speech", "yes".to_owned())];
    assert_eq!(fire(scene.host(), owner, id, &vars), 1);
    assert!(scene.fx.host.waits.is_empty());
    assert!(say_all(&scene.fx.host).is_empty());
    assert!(
        !scene
            .fx
            .host
            .script(owner)
            .unwrap()
            .trigger(id)
            .unwrap()
            .is_busy()
    );
}

// Trigger 11, `0 j 100`: the player hands over object 47 (already in the
// mob's inventory when receive_mtrigger runs). `!varexists && vnum == 47` is
// true: `set` makes a local; `remote name <id>` copies it into the player's
// script globals (the player must have a script); `%purge% %object%` is
// `mpurge <uid>` and extract_obj removes the object at once; `dance` is no
// script command, so command_interpreter(mob, "dance"). Then wait 1 sec.
// After the resumes: say, `nop %actor.exp(50)%` (gain_exp: +50 with no
// level-up, since 1050 < level_exp(11)) and `nop %actor.gold(50)%`
// (increase_gold), the final says and emotes, two more 1-second waits.
#[test]
fn tutorial_11_rewards_the_magic_eight_ball() {
    let record = real!(11);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    let ball = scene.host().add_obj(47, "the magic eight ball");
    scene.host().obj_to_char(ball, owner);
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("object", uid(ball))];
    assert_eq!(fire(scene.host(), owner, id, &vars), 1);

    // Up to the first wait.
    assert_eq!(
        global_var(&scene.fx.host, player, SOLVED).as_deref(),
        Some("1")
    );
    assert!(!scene.fx.host.exists(ball));
    assert_eq!(say_all(&scene.fx.host), vec!["dance"]);
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 10)]);
    assert_eq!(scene.fx.host.num(player, "exp"), 1000);

    run_waits(scene.host(), 10);
    assert_eq!(
        say_all(&scene.fx.host),
        vec![
            "dance",
            "say Thank you, Tester.",
            "say finally, now I can get some answers.",
            "emote shakes the magic eight ball vigorously.",
            "emote does not seem too pleased with his answer.",
        ]
    );
    assert_eq!(scene.fx.host.num(player, "exp"), 1050);
    assert_eq!(scene.fx.host.num(player, "gold"), 550);
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.logs.is_empty());
    assert!(
        !scene
            .fx
            .host
            .script(owner)
            .unwrap()
            .trigger(id)
            .unwrap()
            .is_busy()
    );
}

// Trigger 11, wrong object (a lantern, vnum 3022): the `&&` is false, so the
// else branch: `say I don't want that!` and `junk %object.name%`; an
// object's `name` field is its keyword list (o->name), "lantern" here.
// No wait, nothing remote, the object stays where it is.
#[test]
fn tutorial_11_refuses_any_other_object() {
    let record = real!(11);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    let lantern = scene.fx.inventory[1];
    scene.host().obj_to_char(lantern, owner);
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("object", uid(lantern))];
    assert_eq!(fire(scene.host(), owner, id, &vars), 1);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say I don't want that!", "junk lantern"]
    );
    assert!(scene.fx.host.waits.is_empty());
    assert_eq!(global_var(&scene.fx.host, player, SOLVED), None);
    assert!(scene.fx.host.exists(lantern));
}

// Trigger 11 for a player who has already solved the quest: `!1 && …` is
// false, so even the right object is refused.
#[test]
fn tutorial_11_refuses_the_ball_a_second_time() {
    let record = real!(11);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    scene.set_global(player, SOLVED, "1");
    let ball = scene.host().add_obj(47, "the magic eight ball");
    scene.host().set_text(ball, "alias", "ball magic eight");
    scene.host().obj_to_char(ball, owner);
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("object", uid(ball))];
    fire(scene.host(), owner, id, &vars);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say I don't want that!", "junk ball magic eight"]
    );
    assert!(scene.fx.host.exists(ball));
}

// Trigger 11 with a mob as the actor, a character that has no script at all
// (a playing PC always owns one, `read_saved_vars`; a trigger-less mob does
// not). `varexists` is "0" (`if (SCRIPT(c))` fails), the object is vnum 47, so
// the branch runs. process_remote ends `if (sc_remote==NULL) return;`, so the
// variable is stored nowhere, while the rest of the branch goes on (`set`,
// purge, `dance`, then the wait stops the run).
#[test]
fn remote_to_a_character_without_a_script_stores_nothing() {
    let record = real!(11);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let owner = scene.owner;
    let wanderer = scene.host().add_mob(3200, "a wanderer");
    let room = scene.fx.room;
    scene.host().char_to_room(wanderer, room);
    let ball = scene.host().add_obj(47, "the magic eight ball");
    scene.host().obj_to_char(ball, owner);
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(wanderer)), ("object", uid(ball))];
    fire(scene.host(), owner, id, &vars);
    assert!(scene.fx.host.script(wanderer).is_none());
    assert!(!scene.fx.host.exists(ball));
    assert_eq!(say_all(&scene.fx.host), vec!["dance"]);
    assert!(scene.fx.host.logs.is_empty());
}

// Trigger 11 for a player: the same branch stores the variable on the PC's
// script (enter_player_game always allocates it).
#[test]
fn remote_to_a_player_stores_the_quest_flag() {
    let record = real!(11);
    let mut scene = Scene::new(1317, "the tutorial guide");
    let (owner, player) = (scene.owner, scene.fx.player);
    let ball = scene.host().add_obj(47, "the magic eight ball");
    scene.host().obj_to_char(ball, owner);
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("object", uid(ball))];
    fire(scene.host(), owner, id, &vars);
    assert_eq!(
        global_var(&scene.fx.host, player, SOLVED).as_deref(),
        Some("1")
    );
    assert_eq!(say_all(&scene.fx.host), vec!["dance"]);
}

// ---------------------------------------------------------------------
// The questmaster, triggers 138 (greet), 139 (speech), 143 (receive) and 144
// (load); zone 1. The quest flag is the player's global `on_quest_zone_1`,
// "<day> <hour>" as `set ... %time.day% %time.hour%` wrote it. `time.day` is
// `time_info.day + 1` and `time.hour` is `time_info.hours`.
// ---------------------------------------------------------------------

const QUEST: &str = "on_quest_zone_1";

fn questmaster() -> Scene {
    let mut scene = Scene::new(3, "the questmaster");
    // Wednesday the 5th, 15:00 (`time_info.day` is 4).
    scene.host().time = dg_vm::types::MudTime {
        hours: 15,
        day: 4,
        month: 0,
        year: 600,
    };
    scene
}

// Trigger 138 for a player with no quest and under 50 quest points:
// `%actor.is_pc% && %actor.questpoints% < 50` is `1 && 0 < 50`, so `wait 2 sec`
// (20 pulses). After the wait, `!%actor.varexists(on_quest_zone_1)%` is
// `!0`, so the mob says the welcome line.
#[test]
fn questmaster_greets_a_player_without_a_quest() {
    let record = real!(138);
    let mut scene = questmaster();
    let (owner, player) = (scene.owner, scene.fx.player);
    let id = install_record(scene.host(), owner, &record);
    assert_eq!(
        fire(
            scene.host(),
            owner,
            id,
            &[("actor", uid(player)), ("direction", "south".into())]
        ),
        1
    );
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 20)]);
    assert!(say_all(&scene.fx.host).is_empty());
    run_waits(scene.host(), 3);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say Welcome Tester, are you interested in a simple quest?"]
    );
    assert!(scene.fx.host.logs.is_empty());
}

// Trigger 138 when the quest began two ticks ago. `extract day 1 <v>` and
// `extract hour 2 <v>` pick words 1 and 2 of "5 13" (day 5, hour 13).
// current_ticks = (5/24)+15 = 0+15 = 15 (integer division), start_ticks =
// (5/24)+13 = 13, ticks_passed = 15-13 = 2, which is not > 10, so the mob
// asks how the quest is going. The flag stays.
#[test]
fn questmaster_asks_after_a_recent_quest() {
    let record = real!(138);
    let mut scene = questmaster();
    let (owner, player) = (scene.owner, scene.fx.player);
    scene.set_global(player, QUEST, "5 13");
    let id = install_record(scene.host(), owner, &record);
    fire(scene.host(), owner, id, &[("actor", uid(player))]);
    run_waits(scene.host(), 3);
    assert_eq!(
        say_all(&scene.fx.host),
        vec![
            "say How is your quest going Tester, do you have a quest token or the quest mobs head for me?"
        ]
    );
    assert_eq!(
        global_var(&scene.fx.host, player, QUEST).as_deref(),
        Some("5 13")
    );
}

// Trigger 138 when the quest began at "1 3": start_ticks = (1/24)+3 = 3,
// ticks_passed = 15-3 = 12 > 10, so the flag is deleted (`rdelete name <id>`)
// and the welcome is said.
#[test]
fn questmaster_expires_an_old_quest_on_greet() {
    let record = real!(138);
    let mut scene = questmaster();
    let (owner, player) = (scene.owner, scene.fx.player);
    scene.set_global(player, QUEST, "1 3");
    let id = install_record(scene.host(), owner, &record);
    fire(scene.host(), owner, id, &[("actor", uid(player))]);
    run_waits(scene.host(), 3);
    assert_eq!(global_var(&scene.fx.host, player, QUEST), None);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say Welcome Tester, are you interested in a simple quest?"]
    );
}

// Trigger 138 with 50 quest points (`50 < 50` is false), and with a mob as the
// actor (`is_pc` is 0): the body is skipped, no wait at all. `&&` evaluates both
// sides, and for an NPC the `questpoints` branch is guarded by `!IS_NPC(c)`
// (dg_variables.c), so it falls to the end of the char fields and logs
// "unknown char field" once.
#[test]
fn questmaster_ignores_veterans_and_mobs_on_greet() {
    let record = real!(138);
    let mut scene = questmaster();
    let (owner, player, guard) = (scene.owner, scene.fx.player, scene.fx.guard);
    scene.host().set_num(player, "qp", 50);
    let id = install_record(scene.host(), owner, &record);
    fire(scene.host(), owner, id, &[("actor", uid(player))]);
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.logs.is_empty());
    fire(scene.host(), owner, id, &[("actor", uid(guard))]);
    assert!(scene.fx.host.waits.is_empty());
    assert!(say_all(&scene.fx.host).is_empty());
    assert_eq!(
        scene.fx.host.logs,
        vec!["Trigger: Questmaster Greet - 3, VNum 138. unknown char field: 'questpoints'"]
    );
}

fn quest_assignment(speech: &str, rolls: &[i64]) -> (Scene, u64) {
    let record = trigger_139();
    let mut scene = questmaster();
    let (owner, player) = (scene.owner, scene.fx.player);
    {
        let texts = &mut scene.fx.host.globals.texts;
        texts.insert("mobproto:15".to_owned(), "a quest mob".to_owned());
        texts.insert("objproto:15".to_owned(), "a quest token".to_owned());
        texts.insert("objproto:16".to_owned(), "a quest timer".to_owned());
        // Object 15 can be taken (ITEM_WEAR_TAKE is wear bit 0).
        scene
            .fx
            .host
            .globals
            .nums
            .insert("objproto:15:wear".to_owned(), 1);
    }
    for vnum in [106, 108] {
        scene.host().add_room(vnum, "a quest room");
    }
    scene.host().rolls = rolls.iter().copied().collect();
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("speech", speech.to_owned())];
    fire(scene.host(), owner, id, &vars);
    (scene, id)
}

fn trigger_139() -> common::corpus::Record {
    common::corpus::trigger(139).unwrap_or_else(|| {
        common::corpus::skip_message("trigger 139");
        panic!("corpus missing")
    })
}

// Trigger 139 (`0 d 1 *`), the player says "yes": `%actor% == %self%` is
// false and the player has no quest flag and 0 qp, so the word loop starts.
// `eval word %speech.car%` is "yes"; `yes /= yes` (str_str finds the right
// side inside the left) is true: the mob says the prompt and halts.
#[test]
fn questmaster_prompts_after_yes() {
    if common::corpus::cached().is_none() {
        common::corpus::skip_message("questmaster_prompts_after_yes");
        return;
    }
    let (scene, _) = quest_assignment("yes", &[]);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say Very well Tester. Would you like to find an object or hunt a mobile?"]
    );
    assert!(scene.fx.host.logs.is_empty());
}

// Trigger 139, the player says "hunt". `yes /= hunt` is false (no "hunt"
// inside "yes"). `eval loadroom 99 + %random.265%` with the roll 7 is 106.
// `mobile /= hunt` is false but `hunt /= hunt` is true, so:
//   `%at% 106 %load% m 15` -> `mat 106 mload  m 15` (the %at% and %load%
//     expansions are "mat " and "mload ", trailing space included): do_mat
//     takes the room and passes the rest, " mload  m 15" (one_argument leaves
//     the space), to command_interpreter as the mob;
//   `say Go kill ...`;
//   `%load% obj 16 %actor% inv` -> `mload  obj 16 }1 inv`: object 16 goes to
//     the actor's inventory (find_eq_pos_script("inv") is -1) and
//     `lastloaded` is set on the mob's globals;
//   `msend` tells the actor and `mechoaround` tells the watcher;
//   `set on_quest_zone_1 5 15` and `remote ... <id>` give the flag "5 15" to
//     the player's globals; `halt`.
#[test]
fn questmaster_assigns_a_hunt() {
    if common::corpus::cached().is_none() {
        common::corpus::skip_message("questmaster_assigns_a_hunt");
        return;
    }
    let (scene, id) = quest_assignment("hunt", &[7]);
    let (owner, player, watcher, room) =
        (scene.owner, scene.fx.player, scene.watcher, scene.fx.room);
    assert_eq!(
        scene.fx.host.mud_commands,
        vec![
            (owner, " mload  m 15".to_owned()),
            (
                owner,
                "say Go kill the quest mob and bring me its head Tester. You only have 10 minutes!"
                    .to_owned()
            ),
        ]
    );
    assert_eq!(scene.fx.host.in_room(owner), Some(room));
    let timer = scene.fx.host.carrying(player)[0];
    assert_eq!(scene.fx.host.vnum(timer), 16);
    assert_eq!(
        global_var(&scene.fx.host, owner, "lastloaded"),
        Some(uid(timer))
    );
    assert_eq!(
        scene.fx.host.effects,
        vec![
            format!("send {player}: the questmaster gives you the quest timer.\n\r"),
            format!("send {watcher}: the questmaster gives Tester the quest timer.\n\r"),
        ]
    );
    assert_eq!(
        global_var(&scene.fx.host, player, QUEST).as_deref(),
        Some("5 15")
    );
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.rolls.is_empty());
    assert!(
        !scene
            .fx
            .host
            .script(owner)
            .unwrap()
            .trigger(id)
            .unwrap()
            .is_busy()
    );
    assert!(scene.fx.host.logs.is_empty());
}

// Trigger 139, the player says "find": the `elseif object /= find || find /=
// find` branch. The mob says the token line, `%load% o 15` is `mload o 15`
// (is_abbrev("o", "obj")): object 15 can be taken, so it goes into the mob's
// inventory; `%at% 106 drop quest_token_zone_1` runs " drop quest_token_zone_1"
// as the mob; the timer goes to the player and the flag is set.
#[test]
fn questmaster_assigns_a_search() {
    if common::corpus::cached().is_none() {
        common::corpus::skip_message("questmaster_assigns_a_search");
        return;
    }
    let (scene, _) = quest_assignment("find", &[7]);
    let (owner, player) = (scene.owner, scene.fx.player);
    assert_eq!(
        scene.fx.host.mud_commands,
        vec![
            (
                owner,
                "say Go find the quest token and return it to me. You only have 10 minutes Tester!"
                    .to_owned()
            ),
            (owner, " drop quest_token_zone_1".to_owned()),
        ]
    );
    let held: Vec<i32> = scene
        .fx
        .host
        .carrying(owner)
        .iter()
        .map(|o| scene.fx.host.vnum(*o))
        .collect();
    assert_eq!(held, vec![15]);
    assert_eq!(scene.fx.host.vnum(scene.fx.host.carrying(player)[0]), 16);
    assert_eq!(
        global_var(&scene.fx.host, player, QUEST).as_deref(),
        Some("5 15")
    );
}

// Trigger 139 with a two-word speech, "please hunt": the first pass sees
// "please", matches nothing, and `set word %rest.car%` / `set rest %rest.cdr%`
// move on; `done` re-tests `%word%` ("hunt", true) and jumps back to the
// `while`. Each pass runs `eval loadroom 99 + %random.265%`, so both rolls
// (3, then 9) are consumed and the hunt mob goes to 99+9 = 108.
#[test]
fn questmaster_walks_the_words_of_the_speech() {
    if common::corpus::cached().is_none() {
        common::corpus::skip_message("questmaster_walks_the_words_of_the_speech");
        return;
    }
    let (scene, id) = quest_assignment("please hunt", &[3, 9]);
    let owner = scene.owner;
    assert!(scene.fx.host.rolls.is_empty());
    assert_eq!(
        scene.fx.host.mud_commands[0],
        (owner, " mload  m 15".to_owned())
    );
    assert_eq!(scene.fx.host.mud_commands.len(), 2);
    let _ = id;
}

// Trigger 139 refusals, all silent `halt`s (every PC owns a script, so the
// flag lookups are real): the speaker is the mob itself;
// the player already has a fresh flag (ticks_passed 15-14 = 1, not > 10);
// the player has more than 50 quest points.
#[test]
fn questmaster_ignores_the_wrong_speakers() {
    let record = real!(139);
    let cases: [(&str, Option<&str>, i64); 3] = [
        ("self", None, 0),
        ("player", Some("5 14"), 0),
        ("player", None, 51),
    ];
    for (who, flag, qp) in cases {
        let mut scene = questmaster();
        let (owner, player) = (scene.owner, scene.fx.player);
        if let Some(flag) = flag {
            scene.set_global(player, QUEST, flag);
        }
        scene.host().set_num(player, "qp", qp);
        let id = install_record(scene.host(), owner, &record);
        let actor = if who == "self" { owner } else { player };
        let vars = [("actor", uid(actor)), ("speech", "yes".to_owned())];
        assert_eq!(fire(scene.host(), owner, id, &vars), 1);
        assert!(say_all(&scene.fx.host).is_empty(), "{who} {flag:?} {qp}");
        assert!(scene.fx.host.waits.is_empty());
    }
}

// Trigger 139 with an expired flag ("1 3": 12 ticks): `rdelete` clears it,
// then the word loop runs as for a fresh player.
#[test]
fn questmaster_clears_an_expired_flag_before_assigning() {
    let record = real!(139);
    let mut scene = questmaster();
    let (owner, player) = (scene.owner, scene.fx.player);
    scene.set_global(player, QUEST, "1 3");
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("speech", "yes".to_owned())];
    fire(scene.host(), owner, id, &vars);
    assert_eq!(global_var(&scene.fx.host, player, QUEST), None);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say Very well Tester. Would you like to find an object or hunt a mobile?"]
    );
}

fn receive_scene(object_vnum: i32, on_quest: bool) -> (Scene, u64, Uid) {
    let record = common::corpus::trigger(143).expect("corpus");
    let mut scene = questmaster();
    let (owner, player) = (scene.owner, scene.fx.player);
    if on_quest {
        scene.set_global(player, QUEST, "5 14");
    }
    let item = scene.host().add_obj(object_vnum, "a handed item");
    scene.host().set_text(item, "alias", "handed item");
    scene.host().obj_to_char(item, owner);
    let id = install_record(scene.host(), owner, &record);
    let vars = [("actor", uid(player)), ("object", uid(item))];
    fire(scene.host(), owner, id, &vars);
    (scene, id, item)
}

// Trigger 143 (`0 j 100`) from a player with no quest flag: the mob says so
// and halts: the give goes through (value 1), no wait.
#[test]
fn questmaster_refuses_a_gift_from_a_stranger() {
    if common::corpus::cached().is_none() {
        common::corpus::skip_message("questmaster_refuses_a_gift_from_a_stranger");
        return;
    }
    let (scene, _, item) = receive_scene(15, false);
    assert_eq!(
        say_all(&scene.fx.host),
        vec!["say You are not even on a quest Tester."]
    );
    assert!(scene.fx.host.waits.is_empty());
    assert!(scene.fx.host.exists(item));
}

// Trigger 143 for a quester handing over object 15 (or 17): `wait 1 sec`
// first (10 pulses). Then the flag is deleted, "Well done", `%actor.exp(50)%`
// (1000 -> 1050), `%actor.gold(50)%` (500 -> 550), the object is purged and
// `%actor.questpoints(1)%` adds a point.
#[test]
fn questmaster_rewards_the_quest_item() {
    if common::corpus::cached().is_none() {
        common::corpus::skip_message("questmaster_rewards_the_quest_item");
        return;
    }
    for vnum in [15, 17] {
        let (mut scene, id, item) = receive_scene(vnum, true);
        let (owner, player) = (scene.owner, scene.fx.player);
        assert_eq!(scene.fx.host.waits, vec![(owner, id, 10)]);
        assert!(say_all(&scene.fx.host).is_empty());
        run_waits(scene.host(), 3);
        assert_eq!(
            say_all(&scene.fx.host),
            vec![
                "say Well done, Tester.",
                "say you have earned 50 gold, 50 xp, 1 questpoint Tester.",
            ]
        );
        assert_eq!(global_var(&scene.fx.host, player, QUEST), None);
        assert_eq!(scene.fx.host.num(player, "exp"), 1050);
        assert_eq!(scene.fx.host.num(player, "gold"), 550);
        assert_eq!(scene.fx.host.num(player, "qp"), 1);
        assert!(!scene.fx.host.exists(item));
        assert!(scene.fx.host.logs.is_empty());
    }
}

// Trigger 143 with the wrong object. The script says "I don't want that!" and
// `return 0.` (atoi("0.") is 0) but only after `wait 1 sec`. The receive
// trigger's own call returned at the wait with the default 1, and a resumed
// run's value is thrown away (trig_wait_event), so the give is never
// refused. Running the second half directly shows the script's value.
#[test]
fn questmaster_return_zero_comes_too_late_to_refuse_the_gift() {
    if common::corpus::cached().is_none() {
        common::corpus::skip_message("questmaster_return_zero_comes_too_late");
        return;
    }
    let (mut scene, id, item) = receive_scene(99, true);
    let (owner, player) = (scene.owner, scene.fx.player);
    assert_eq!(scene.fx.host.waits, vec![(owner, id, 10)]);
    scene.host().waits.clear();
    assert_eq!(script_driver(scene.host(), owner, id, Mode::Restart), 0);
    assert_eq!(say_all(&scene.fx.host), vec!["say I don't want that!"]);
    assert!(scene.fx.host.exists(item));
    assert_eq!(
        global_var(&scene.fx.host, player, QUEST).as_deref(),
        Some("5 14")
    );
}

// Trigger 144 (`0 n 100`): the quest mob loads, `%load% obj 17` is `mload obj
// 17` with no target: object 17 (takeable) goes to the mob's inventory and
// `lastloaded` names it.
#[test]
fn quest_mob_loads_its_head() {
    let record = real!(144);
    let mut scene = Scene::new(15, "the quest mob");
    let owner = scene.owner;
    scene
        .fx
        .host
        .globals
        .texts
        .insert("objproto:17".to_owned(), "a head".to_owned());
    scene
        .fx
        .host
        .globals
        .nums
        .insert("objproto:17:wear".to_owned(), 1);
    let id = install_record(scene.host(), owner, &record);
    assert_eq!(fire(scene.host(), owner, id, &[]), 1);
    let held = scene.fx.host.carrying(owner);
    assert_eq!(held.len(), 1);
    assert_eq!(scene.fx.host.vnum(held[0]), 17);
    assert_eq!(
        global_var(&scene.fx.host, owner, "lastloaded"),
        Some(uid(held[0]))
    );
    assert!(scene.fx.host.mud_commands.is_empty());
}
