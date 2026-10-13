//! [`MagicHost`] for the mock.
//!
//! Keys beyond the shared table:
//!
//! | entity | key | meaning (C) |
//! |---|---|---|
//! | globals | `nums["spellnum:<name>"]` | `find_skill_num(<name>)`, the name lowercased; exact match only |
//! | globals | `nums["spellinfo:<n>:targets"]`, `nums["spellinfo:<n>:routines"]` | `spell_info[n].targets` / `.routines` |
//! | globals | `nums["no_caster_proxy"]` | non-zero makes `read_mobile(DG_CASTER_PROXY)` fail |
//! | char | `nums["dg_affects"]` | how many `SPELL_DG_AFFECT` affects the character has |
//!
//! Effects recorded: `call_magic …`, `extract_char <uid>`,
//! `affect_to_char …`, `affect_from_char <uid>`.

use crate::magic::{DG_CASTER_PROXY, MagicHost};
use crate::mock::MockHost;
use crate::types::Uid;

impl MockHost {
    /// Register a spell for `find_skill_num` and `spell_info[]`.
    pub fn add_spell(&mut self, name: &str, spellnum: i32, targets: u32, routines: u32) {
        let nums = &mut self.globals.nums;
        nums.insert(
            format!("spellnum:{}", name.to_ascii_lowercase()),
            i64::from(spellnum),
        );
        nums.insert(format!("spellinfo:{spellnum}:targets"), i64::from(targets));
        nums.insert(
            format!("spellinfo:{spellnum}:routines"),
            i64::from(routines),
        );
    }
}

fn show(uid: Option<Uid>) -> String {
    uid.map_or_else(|| "-".to_owned(), |uid| uid.to_string())
}

impl MagicHost for MockHost {
    fn magic_find_skill_num(&self, name: &str) -> i32 {
        self.globals
            .nums
            .get(&format!("spellnum:{}", name.to_ascii_lowercase()))
            .and_then(|num| i32::try_from(*num).ok())
            .unwrap_or(-1)
    }

    fn magic_spell_targets(&self, spellnum: i32) -> u32 {
        self.globals
            .nums
            .get(&format!("spellinfo:{spellnum}:targets"))
            .and_then(|num| u32::try_from(*num).ok())
            .unwrap_or(0)
    }

    fn magic_spell_routines(&self, spellnum: i32) -> u32 {
        self.globals
            .nums
            .get(&format!("spellinfo:{spellnum}:routines"))
            .and_then(|num| u32::try_from(*num).ok())
            .unwrap_or(0)
    }

    fn magic_call_magic(
        &mut self,
        caster: Uid,
        victim: Option<Uid>,
        obj: Option<Uid>,
        spellnum: i32,
        level: i32,
    ) {
        let room = self
            .link(caster, "room")
            .map_or_else(String::new, |room| room.to_string());
        self.effect(format!(
            "call_magic spell={spellnum} level={level} caster={caster} victim={} obj={} room={room}",
            show(victim),
            show(obj)
        ));
    }

    fn magic_load_caster_proxy(&mut self, room: Uid, short_descr: Option<&str>) -> Option<Uid> {
        if self
            .globals
            .nums
            .get("no_caster_proxy")
            .copied()
            .unwrap_or(0)
            != 0
        {
            return None;
        }
        let proxy = self.add_mob(DG_CASTER_PROXY, "Puff");
        if let Some(name) = short_descr {
            self.set_text(proxy, "name", name);
        }
        self.char_to_room(proxy, room);
        Some(proxy)
    }

    fn magic_extract_caster_proxy(&mut self, proxy: Uid) {
        self.extract_char(proxy);
        self.effect(format!("extract_char {proxy}"));
    }

    fn magic_remove_dg_affects(&mut self, ch: Uid) {
        self.set_num(ch, "dg_affects", 0);
        self.effect(format!("affect_from_char {ch}"));
    }

    fn magic_add_dg_affect(
        &mut self,
        ch: Uid,
        location: i32,
        modifier: i32,
        duration: i32,
        bit: Option<u32>,
    ) {
        let count = self.num(ch, "dg_affects");
        self.set_num(ch, "dg_affects", count + 1);
        self.effect(format!(
            "affect_to_char {ch} location={location} modifier={modifier} duration={duration} bit={}",
            bit.map_or_else(|| "-".to_owned(), |bit| bit.to_string())
        ));
    }
}
