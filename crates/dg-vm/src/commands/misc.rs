//! `dg_misc.c`: `script_damage` and the damage messages of `send_char_pos`.
//! (`valid_dg_target` is [`crate::host::WorldHost::valid_dg_target`].)

use super::comm::{TO_CHAR, TO_ROOM, act};
use super::shared::{color_level, pos};
use crate::fields_char::CharStat;
use crate::host::DgHost;
use crate::types::{LVL_IMMORT, MUDLOG_BRF, POS_DEAD, POS_INCAP, POS_MORTALLYW, POS_STUNNED, Uid};

/// `KRED` / `KNRM` (`screen.h`).
const KRED: &str = "\x1B[0;31m";
const KNRM: &str = "\x1B[0m";

/// `send_char_pos(ch, dam)`: tell `ch` and the room how a hit left them.
pub fn send_char_pos<H: DgHost + ?Sized>(host: &mut H, ch: Uid, dam: i32) {
    match pos(host, ch) {
        POS_MORTALLYW => {
            act(
                host,
                "$n is mortally wounded, and will die soon, if not aided.",
                true,
                ch,
                TO_ROOM,
            );
            host.cmd_send(
                ch,
                "You are mortally wounded, and will die soon, if not aided.\r\n",
            );
        }
        POS_INCAP => {
            act(
                host,
                "$n is incapacitated and will slowly die, if not aided.",
                true,
                ch,
                TO_ROOM,
            );
            host.cmd_send(
                ch,
                "You are incapacitated and will slowly die, if not aided.\r\n",
            );
        }
        POS_STUNNED => {
            act(
                host,
                "$n is stunned, but will probably regain consciousness again.",
                true,
                ch,
                TO_ROOM,
            );
            host.cmd_send(
                ch,
                "You're stunned, but will probably regain consciousness again.\r\n",
            );
        }
        POS_DEAD => {
            act(host, "$n is dead!  R.I.P.", false, ch, TO_ROOM);
            host.cmd_send(ch, "You are dead!  Sorry...\r\n");
        }
        _ => {
            let quarter = host.char_stat(ch, CharStat::MaxHit) >> 2;
            if dam > quarter {
                act(host, "That really did HURT!", false, ch, TO_CHAR);
            }
            if host.char_stat(ch, CharStat::Hit) < quarter {
                // CCRED / CCNRM at C_SPR: colour when the level is at least 1.
                let on = color_level(host, ch) >= 1;
                let (red, normal) = if on { (KRED, KNRM) } else { ("", "") };
                host.cmd_send(
                    ch,
                    &format!(
                        "{red}You wish that your wounds would stop BLEEDING so much!{normal}\r\n"
                    ),
                );
            }
        }
    }
}

/// `script_damage(vict, dam)`: hurt `vict` for `dam` hit points.
///
/// Hit points are a `sh_int` in the C: the subtraction wraps at 16 bits,
/// which `char_set_stat` receives as the already-truncated value.
pub fn script_damage<H: DgHost + ?Sized>(host: &mut H, vict: Uid, dam: i32) {
    if host.level(vict) >= LVL_IMMORT && dam > 0 {
        host.cmd_send(
            vict,
            "Being the cool immortal you are, you sidestep a trap, obviously placed to kill you.\r\n",
        );
        return;
    }

    let hit = host.char_stat(vict, CharStat::Hit).wrapping_sub(dam) as i16;
    let hit = i32::from(hit).min(host.char_stat(vict, CharStat::MaxHit));
    host.char_set_stat(vict, CharStat::Hit, hit);

    host.char_update_pos(vict);
    send_char_pos(host, vict, dam);

    if pos(host, vict) == POS_DEAD {
        if !host.is_npc(vict) {
            let place = match host.in_room(vict) {
                Some(room) => host.name(room),
                None => "NOWHERE".to_owned(),
            };
            let level = LVL_IMMORT.max(host.cmd_invis_level(vict));
            let text = format!("{} killed by script at {}", host.name(vict), place);
            host.cmd_mudlog(MUDLOG_BRF, level, &text);
        }
        host.cmd_die(vict);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::commands::support::World;

    fn hit(w: &World, ch: Uid) -> i64 {
        w.host.num(ch, "hit")
    }

    #[test]
    fn immortals_sidestep_positive_damage_only() {
        let mut w = World::new();
        let alice = w.player;
        w.host.set_num(alice, "level", 31);
        script_damage(&mut w.host, alice, 5);
        assert_eq!(hit(&w, alice), 20);
        assert_eq!(
            w.sent_to(alice),
            [
                "Being the cool immortal you are, you sidestep a trap, obviously placed to kill you.\r\n"
            ]
        );
        // Zero or negative damage is not a trap: it goes through (and is
        // capped at the maximum).
        w.clear();
        w.host.set_num(alice, "hit", 10);
        script_damage(&mut w.host, alice, -5);
        assert_eq!(hit(&w, alice), 15);
        script_damage(&mut w.host, alice, -50);
        assert_eq!(hit(&w, alice), 20);
        assert!(w.sent_to(alice).is_empty());
        // Level 30 is not an immortal.
        w.host.set_num(alice, "level", 30);
        script_damage(&mut w.host, alice, 3);
        assert_eq!(hit(&w, alice), 17);
    }

    #[test]
    fn light_damage_says_nothing_and_heavy_damage_hurts() {
        let mut w = World::new();
        let alice = w.player;
        // max_hit 20: the thresholds are 20 >> 2 = 5.
        script_damage(&mut w.host, alice, 5);
        assert_eq!(hit(&w, alice), 15);
        assert!(w.sent_to(alice).is_empty());
        script_damage(&mut w.host, alice, 6);
        assert_eq!(hit(&w, alice), 9);
        assert_eq!(w.sent_to(alice), ["That really did HURT!\r\n"]);
    }

    #[test]
    fn hit_points_at_the_quarter_mark_do_not_bleed() {
        let mut w = World::new();
        let alice = w.player;
        // 20 - 15 = 5 is not below 20 >> 2 = 5.
        script_damage(&mut w.host, alice, 15);
        assert_eq!(hit(&w, alice), 5);
        assert_eq!(w.sent_to(alice), ["That really did HURT!\r\n"]);
    }

    #[test]
    fn low_hit_points_bleed_in_colour_when_colour_is_on() {
        let mut w = World::new();
        let alice = w.player;
        // dam 16 > 5 hurts; hit 4 < 5 bleeds.
        script_damage(&mut w.host, alice, 16);
        assert_eq!(
            w.sent_to(alice),
            [
                "That really did HURT!\r\n",
                "You wish that your wounds would stop BLEEDING so much!\r\n"
            ]
        );
        w.clear();
        w.host.set_num(alice, "hit", 20);
        // PRF_COLOR_1 alone is sparse colour, which is enough.
        w.set_bit(alice, "pref", 13, true);
        script_damage(&mut w.host, alice, 16);
        let coloured =
            "\x1B[0;31mYou wish that your wounds would stop BLEEDING so much!\x1B[0m\r\n";
        assert_eq!(w.sent_to(alice)[1], coloured);
        // So is PRF_COLOR_2 alone (normal colour); neither bit is none.
        for (low, high, on) in [(false, true, true), (false, false, false)] {
            w.clear();
            w.host.set_num(alice, "hit", 20);
            w.set_bit(alice, "pref", 13, low);
            w.set_bit(alice, "pref", 14, high);
            script_damage(&mut w.host, alice, 16);
            assert_eq!(w.sent_to(alice)[1] == coloured, on, "{low} {high}");
        }
        // An NPC never has colour, whatever its preference bits say.
        w.clear();
        let mob = w.mob;
        w.host.set_num(mob, "desc", 1);
        w.host.set_num(mob, "pref", 3 << 13);
        w.host.set_num(mob, "hit", 20);
        w.host.set_num(mob, "max_hit", 20);
        script_damage(&mut w.host, mob, 16);
        assert_eq!(
            w.sent_to(mob)[1],
            "You wish that your wounds would stop BLEEDING so much!\r\n"
        );
    }

    #[test]
    fn each_state_of_health_has_its_messages() {
        for (dam, pos, room_line, own_line) in [
            (
                20,
                3,
                "Alice is stunned, but will probably regain consciousness again.\r\n",
                "You're stunned, but will probably regain consciousness again.\r\n",
            ),
            (
                23,
                2,
                "Alice is incapacitated and will slowly die, if not aided.\r\n",
                "You are incapacitated and will slowly die, if not aided.\r\n",
            ),
            (
                26,
                1,
                "Alice is mortally wounded, and will die soon, if not aided.\r\n",
                "You are mortally wounded, and will die soon, if not aided.\r\n",
            ),
            (
                31,
                0,
                "Alice is dead!  R.I.P.\r\n",
                "You are dead!  Sorry...\r\n",
            ),
        ] {
            let mut w = World::new();
            let (alice, bob) = (w.player, w.player2);
            script_damage(&mut w.host, alice, dam);
            assert_eq!(w.host.num(alice, "pos"), pos, "dam {dam}");
            assert_eq!(w.sent_to(bob), [room_line], "dam {dam}");
            assert_eq!(w.sent_to(alice), [own_line], "dam {dam}");
            // The room is told before the victim.
            assert!(w.host.effects[0].starts_with(&format!("send {bob}:")));
        }
    }

    #[test]
    fn an_invisible_victim_is_not_announced_by_name_to_those_who_cannot_see() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        // hide_invisible is TRUE for the wounded states, FALSE for death.
        w.host
            .entity_mut(bob)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![alice]);
        script_damage(&mut w.host, alice, 20);
        assert!(w.sent_to(bob).is_empty());
        script_damage(&mut w.host, alice, 20);
        assert_eq!(w.host.num(alice, "pos"), 0);
        assert_eq!(w.sent_to(bob), ["Someone is dead!  R.I.P.\r\n"]);
    }

    #[test]
    fn a_dead_player_is_logged_and_dies_a_dead_mob_only_dies() {
        let mut w = World::new();
        let (alice, mob) = (w.player, w.mob);
        script_damage(&mut w.host, alice, 40);
        assert_eq!(
            w.effects("mudlog"),
            ["mudlog 1 31: Alice killed by script at The Temple"]
        );
        assert_eq!(w.effects("die"), [format!("die {alice}")]);
        // The log level is the larger of LVL_IMMORT and the invisibility level.
        w.clear();
        w.host.set_num(alice, "hit", 20);
        w.host.set_num(alice, "invis_level", 33);
        script_damage(&mut w.host, alice, 40);
        assert_eq!(
            w.effects("mudlog"),
            ["mudlog 1 33: Alice killed by script at The Temple"]
        );
        // In no room, the place is NOWHERE.
        w.clear();
        w.host.set_num(alice, "hit", 20);
        w.host.char_from_room(alice);
        script_damage(&mut w.host, alice, 40);
        assert_eq!(
            w.effects("mudlog"),
            ["mudlog 1 33: Alice killed by script at NOWHERE"]
        );
        w.clear();
        script_damage(&mut w.host, mob, 40);
        assert!(w.effects("mudlog").is_empty());
        assert_eq!(w.effects("die"), [format!("die {mob}")]);
    }

    #[test]
    fn hit_points_wrap_like_a_short() {
        let mut w = World::new();
        let alice = w.player;
        // 20 - (-32760) = 32780, which a sh_int holds as -32756.
        script_damage(&mut w.host, alice, -32760);
        assert_eq!(hit(&w, alice), -32756);
        assert_eq!(w.host.num(alice, "pos"), 0);
        // The subtraction itself cannot overflow.
        let mut w = World::new();
        let alice = w.player;
        script_damage(&mut w.host, alice, i32::MIN);
        script_damage(&mut w.host, alice, i32::MAX);
        assert!(hit(&w, alice) <= 20);
    }

    #[test]
    fn send_char_pos_default_branch_covers_the_other_positions() {
        let mut w = World::new();
        let alice = w.player;
        w.host.set_num(alice, "pos", 4);
        w.host.set_num(alice, "hit", 3);
        send_char_pos(&mut w.host, alice, 6);
        // Asleep is not awake: act() skips her, send_to_char does not.
        assert_eq!(
            w.sent_to(alice),
            ["You wish that your wounds would stop BLEEDING so much!\r\n"]
        );
    }
}
