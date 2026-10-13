//! [`CharHost`] for the mock.
//!
//! Shared keys are used wherever the table in `mock/mod.rs` has one (`hit`,
//! `max_hit`, `mana`, `max_mana`, `move`, `max_move`, `gold`, `exp`, `align`,
//! `sex`, `class`, `pos`, `level`, `fighting`, `master`, `followers`,
//! `spell:<n>`). Keys added here, all on the character unless noted:
//!
//! | key | meaning (C) |
//! |---|---|
//! | `nums["damroll"]`, `nums["hitroll"]` | `GET_DAMROLL`, `GET_HITROLL` |
//! | `nums["weight"]` | `GET_WEIGHT` |
//! | `nums["ac"]` | the result of `compute_armor_class` |
//! | `nums["prac"]`, `nums["qp"]` | `GET_PRACTICES`, `GET_QUESTPOINTS` |
//! | `nums["wait"]` | `GET_WAIT_STATE` |
//! | `nums["cond:<0..2>"]` | `GET_COND` (drunk, hunger, thirst) |
//! | `nums["save:<0..4>"]` | `GET_SAVE`, indexed by `SAVING_PARA` 0, `ROD` 1, `PETRI` 2, `BREATH` 3, `SPELL` 4 |
//! | `nums["abil:<name>"]`, `nums["rabil:<name>"]` | `aff_abils` / `real_abils`; `name` is `str`, `stradd`, `int`, `wis`, `dex`, `con`, `cha`. `affect_total` copies real to aff (the mock has no affects that change abilities) |
//! | `nums["skill:<n>"]` | `GET_SKILL` |
//! | `nums["act"]`, `nums["pref"]` | the act / preference bitvectors (bit `n` = flag `n`); `IS_NPC` is the shared `nums["is_npc"]`, not act bit 3 (the VM reads that through `is_npc`) |
//! | `globals.nums["dummy:<key>"]` | the C's shared `dummy_mob` record: `prac`, `qp`, `cond:<n>` and `skill:<n>` of every NPC live here, not on the mob |
//! | `texts["title"]` | `GET_TITLE`; absent = NULL |
//! | `nums["quest"]` | `GET_QUEST`; absent = `NOTHING` (`-1`) |
//! | `nums["quest_done:<vnum>"]` | `is_complete(ch, vnum)` |
//! | `globals.nums["quest:<vnum>"]` | `real_quest(vnum) != NOTHING` |
//!
//! The act bit `DEAD(ch)` reads (`MOB_NOTDEADYET` 19 for an NPC,
//! `PLR_NOTDEADYET` 16 for a player) is the shared `nums["dead"]`, never
//! stored in `nums["act"]`: reading it answers `is_dead`, writing it is
//! ignored.
//!
//! `gain_exp` models only the parts of the C that do not need the level
//! tables: an NPC adds, a player below level 1 or at an immortal level is
//! unchanged, a zero gain changes nothing, gains add (capped at the default
//! `CONFIG_MAX_EXP_GAIN`, no level-up), losses are capped at the default
//! `CONFIG_MAX_EXP_LOSS` and floor the total at 0.

use crate::fields_char::{CharAbil, CharFact, CharHost, CharStat};
use crate::host::WorldHost;
use crate::mock::MockHost;
use crate::types::Uid;

/// `CONFIG_MAX_EXP_GAIN` and `CONFIG_MAX_EXP_LOSS` at their `config.c` defaults.
const MAX_EXP_GAIN: i32 = 100_000;
const MAX_EXP_LOSS: i32 = 500_000;
/// `MOB_NOTDEADYET` and `PLR_NOTDEADYET`: the act bit that is `DEAD(ch)`.
const MOB_NOTDEADYET: u32 = 19;
const PLR_NOTDEADYET: u32 = 16;
/// `MAX_GOLD`.
const MAX_GOLD: i32 = 2_140_000_000;

fn stat_key(stat: CharStat) -> &'static str {
    match stat {
        CharStat::Align => "align",
        CharStat::Damroll => "damroll",
        CharStat::Hitroll => "hitroll",
        CharStat::Hit => "hit",
        CharStat::MaxHit => "max_hit",
        CharStat::Mana => "mana",
        CharStat::MaxMana => "max_mana",
        CharStat::Move => "move",
        CharStat::MaxMove => "max_move",
        CharStat::Class => "class",
        CharStat::Pos => "pos",
        CharStat::Practices => "prac",
        CharStat::QuestPoints => "qp",
        CharStat::WaitState => "wait",
        CharStat::Drunk => "cond:0",
        CharStat::Hunger => "cond:1",
        CharStat::Thirst => "cond:2",
        CharStat::SavePara => "save:0",
        CharStat::SaveRod => "save:1",
        CharStat::SavePetri => "save:2",
        CharStat::SaveBreath => "save:3",
        CharStat::SaveSpell => "save:4",
    }
}

fn abil_name(abil: CharAbil) -> &'static str {
    match abil {
        CharAbil::Str => "str",
        CharAbil::StrAdd => "stradd",
        CharAbil::Int => "int",
        CharAbil::Wis => "wis",
        CharAbil::Dex => "dex",
        CharAbil::Con => "con",
        CharAbil::Cha => "cha",
    }
}

const ABILS: [CharAbil; 7] = [
    CharAbil::Str,
    CharAbil::StrAdd,
    CharAbil::Int,
    CharAbil::Wis,
    CharAbil::Dex,
    CharAbil::Con,
    CharAbil::Cha,
];

fn char_num_i32(host: &MockHost, ch: Uid, key: &str) -> i32 {
    i32::try_from(host.num(ch, key)).unwrap_or(0)
}

/// The entity key and owner for a player-only attribute: an NPC shares the
/// C's one `dummy_mob` record, held on the globals.
fn player_only(host: &MockHost, ch: Uid, key: &str) -> Option<String> {
    let player_key =
        matches!(key, "prac" | "qp") || key.starts_with("cond:") || key.starts_with("skill:");
    (player_key && host.num(ch, "is_npc") != 0).then(|| format!("dummy:{key}"))
}

/// Whether `bit` of this character's act array is `DEAD(ch)`.
fn is_dead_bit(host: &MockHost, ch: Uid, bit: u32) -> bool {
    bit == if host.is_npc(ch) {
        MOB_NOTDEADYET
    } else {
        PLR_NOTDEADYET
    }
}

fn char_exists(host: &MockHost, ch: Uid) -> bool {
    host.entity(ch).is_some()
}

impl MockHost {
    /// Read a character number, through the shared NPC record when the C would.
    fn shared_get(&self, ch: Uid, key: &str) -> i32 {
        match player_only(self, ch, key) {
            Some(shared) => {
                i32::try_from(self.globals.nums.get(&shared).copied().unwrap_or(0)).unwrap_or(0)
            }
            None => char_num_i32(self, ch, key),
        }
    }

    /// Write a character number, through the shared NPC record when the C would.
    fn shared_set(&mut self, ch: Uid, key: &str, value: i32) {
        match player_only(self, ch, key) {
            Some(shared) => {
                if char_exists(self, ch) {
                    self.globals.nums.insert(shared, i64::from(value));
                }
            }
            None => self.set_num(ch, key, i64::from(value)),
        }
    }
}

impl CharHost for MockHost {
    fn char_stat(&self, ch: Uid, stat: CharStat) -> i32 {
        self.shared_get(ch, stat_key(stat))
    }

    fn char_fact(&self, ch: Uid, fact: CharFact) -> i32 {
        char_num_i32(
            self,
            ch,
            match fact {
                CharFact::Gold => "gold",
                CharFact::Exp => "exp",
                CharFact::Weight => "weight",
                CharFact::Sex => "sex",
            },
        )
    }

    fn char_set_level(&mut self, ch: Uid, level: i32) {
        self.set_num(ch, "level", i64::from(level));
    }

    fn char_set_stat(&mut self, ch: Uid, stat: CharStat, value: i32) {
        self.shared_set(ch, stat_key(stat), value);
    }

    fn char_abil(&self, ch: Uid, abil: CharAbil) -> i32 {
        char_num_i32(self, ch, &format!("abil:{}", abil_name(abil)))
    }

    fn char_real_abil(&self, ch: Uid, abil: CharAbil) -> i32 {
        char_num_i32(self, ch, &format!("rabil:{}", abil_name(abil)))
    }

    fn char_set_real_abil(&mut self, ch: Uid, abil: CharAbil, value: i32) {
        self.set_num(ch, &format!("rabil:{}", abil_name(abil)), i64::from(value));
    }

    fn char_affect_total(&mut self, ch: Uid) {
        if !char_exists(self, ch) {
            return;
        }
        for abil in ABILS {
            let real = self.char_real_abil(ch, abil);
            self.set_num(ch, &format!("abil:{}", abil_name(abil)), i64::from(real));
        }
        self.effect(format!("affect_total {ch}"));
    }

    fn char_armor_class(&self, ch: Uid) -> i32 {
        char_num_i32(self, ch, "ac")
    }

    fn char_update_pos(&mut self, ch: Uid) {
        let hit = self.num(ch, "hit");
        let pos = self.num(ch, "pos");
        // POS_STUNNED 3, POS_STANDING 8, POS_DEAD 0, POS_MORTALLYW 1,
        // POS_INCAP 2.
        let new = if hit > 0 && pos > 3 {
            return;
        } else if hit > 0 {
            8
        } else if hit <= -11 {
            0
        } else if hit <= -6 {
            1
        } else if hit <= -3 {
            2
        } else {
            3
        };
        self.set_num(ch, "pos", new);
    }

    fn char_gain_exp(&mut self, ch: Uid, gain: i32) {
        if !char_exists(self, ch) {
            return;
        }
        self.effect(format!("gain_exp {ch}: {gain}"));
        let exp = self.char_fact(ch, CharFact::Exp);
        let level = self.num(ch, "level");
        let value = if self.num(ch, "is_npc") != 0 {
            exp.wrapping_add(gain)
        } else if !(1..31).contains(&level) {
            return;
        } else if gain > 0 {
            exp.wrapping_add(gain.min(MAX_EXP_GAIN))
        } else if gain < 0 {
            exp.wrapping_add(gain.max(-MAX_EXP_LOSS)).max(0)
        } else {
            return;
        };
        self.set_num(ch, "exp", i64::from(value));
    }

    fn char_increase_gold(&mut self, ch: Uid, amt: i32) {
        if !char_exists(self, ch) {
            return;
        }
        self.effect(format!("increase_gold {ch}: {amt}"));
        let curr = self.char_fact(ch, CharFact::Gold);
        let mut gold;
        if amt < 0 {
            gold = curr.wrapping_add(amt).max(0);
            if gold > curr {
                gold = 0;
            }
        } else {
            gold = curr.wrapping_add(amt).min(MAX_GOLD);
            if gold < curr {
                gold = MAX_GOLD;
            }
        }
        self.set_num(ch, "gold", i64::from(gold));
    }

    fn char_fighting(&self, ch: Uid) -> Option<Uid> {
        self.link(ch, "fighting")
    }

    fn char_master(&self, ch: Uid) -> Option<Uid> {
        self.link(ch, "master")
    }

    fn char_first_follower(&self, ch: Uid) -> Option<Uid> {
        self.list(ch, "followers").first().copied()
    }

    fn char_affected_by_spell(&self, ch: Uid, spell: i32) -> bool {
        self.num(ch, &format!("spell:{spell}")) != 0
    }

    fn char_skill(&self, ch: Uid, skill: i32) -> i32 {
        self.shared_get(ch, &format!("skill:{skill}"))
    }

    fn char_set_skill(&mut self, ch: Uid, skill: i32, value: i32) {
        self.shared_set(ch, &format!("skill:{skill}"), value);
    }

    fn char_act_flagged(&self, ch: Uid, bit: u32) -> bool {
        if is_dead_bit(self, ch, bit) {
            return self.is_dead(ch);
        }
        (self.num(ch, "act") >> bit.min(62)) & 1 != 0
    }

    fn char_set_act_flag(&mut self, ch: Uid, bit: u32, on: bool) {
        if is_dead_bit(self, ch, bit) {
            return;
        }
        let mask = 1i64 << bit.min(62);
        let act = self.num(ch, "act");
        self.set_num(ch, "act", if on { act | mask } else { act & !mask });
    }

    fn char_pref_flagged(&self, ch: Uid, bit: u32) -> bool {
        (self.num(ch, "pref") >> bit.min(62)) & 1 != 0
    }

    fn char_set_short_descr(&mut self, ch: Uid, text: &str) {
        self.set_text(ch, "name", text);
    }

    fn char_title(&self, ch: Uid) -> Option<String> {
        self.entity(ch)?.texts.get("title").cloned()
    }

    fn char_set_title(&mut self, ch: Uid, title: &str) {
        self.set_text(ch, "title", title);
    }

    fn char_quest(&self, ch: Uid) -> i32 {
        self.entity(ch)
            .and_then(|entity| entity.nums.get("quest"))
            .and_then(|quest| i32::try_from(*quest).ok())
            .unwrap_or(-1)
    }

    fn char_quest_exists(&self, vnum: i32) -> bool {
        self.globals
            .nums
            .get(&format!("quest:{vnum}"))
            .is_some_and(|flag| *flag != 0)
    }

    fn char_quest_completed(&self, ch: Uid, vnum: i32) -> bool {
        self.num(ch, &format!("quest_done:{vnum}")) != 0
    }
}
