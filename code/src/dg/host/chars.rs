//! `CharHost`: character attributes.
//!
//! Every method is unmodelled for now and answers the neutral value
//! `HOST.md` lists for it.

use dg_vm::Uid;
use dg_vm::fields_char::{CharAbil, CharFact, CharHost, CharStat};

use super::EcsHost;

impl CharHost for EcsHost<'_> {
    fn char_stat(&self, _ch: Uid, _stat: CharStat) -> i32 {
        0
    }

    fn char_fact(&self, _ch: Uid, _fact: CharFact) -> i32 {
        0
    }

    fn char_set_stat(&mut self, _ch: Uid, _stat: CharStat, _value: i32) {}

    fn char_set_level(&mut self, _ch: Uid, _level: i32) {}

    fn char_abil(&self, _ch: Uid, _abil: CharAbil) -> i32 {
        0
    }

    fn char_real_abil(&self, _ch: Uid, _abil: CharAbil) -> i32 {
        0
    }

    fn char_set_real_abil(&mut self, _ch: Uid, _abil: CharAbil, _value: i32) {}

    fn char_affect_total(&mut self, _ch: Uid) {}

    fn char_armor_class(&self, _ch: Uid) -> i32 {
        0
    }

    fn char_update_pos(&mut self, _ch: Uid) {}

    fn char_gain_exp(&mut self, _ch: Uid, _gain: i32) {}

    fn char_increase_gold(&mut self, _ch: Uid, _amt: i32) {}

    fn char_fighting(&self, _ch: Uid) -> Option<Uid> {
        None
    }

    fn char_master(&self, _ch: Uid) -> Option<Uid> {
        None
    }

    fn char_first_follower(&self, _ch: Uid) -> Option<Uid> {
        None
    }

    fn char_affected_by_spell(&self, _ch: Uid, _spell: i32) -> bool {
        false
    }

    fn char_skill(&self, _ch: Uid, _skill: i32) -> i32 {
        0
    }

    fn char_set_skill(&mut self, _ch: Uid, _skill: i32, _value: i32) {}

    fn char_act_flagged(&self, _ch: Uid, _bit: u32) -> bool {
        false
    }

    fn char_set_act_flag(&mut self, _ch: Uid, _bit: u32, _on: bool) {}

    fn char_pref_flagged(&self, _ch: Uid, _bit: u32) -> bool {
        false
    }

    fn char_set_short_descr(&mut self, _ch: Uid, _text: &str) {}

    fn char_title(&self, _ch: Uid) -> Option<String> {
        None
    }

    fn char_set_title(&mut self, _ch: Uid, _title: &str) {}

    fn char_quest(&self, _ch: Uid) -> i32 {
        -1
    }

    fn char_quest_exists(&self, _vnum: i32) -> bool {
        false
    }

    fn char_quest_completed(&self, _ch: Uid, _vnum: i32) -> bool {
        false
    }
}
