//! [`MagicHost`] for the mock.
//!
//! Keys beyond the shared table:
//!
//! | entity | key | meaning (C) |
//! |---|---|---|
//! | globals | `nums["spellnum:<name>"]` | `find_skill_num(<name>)`, the name lowercased; exact match only |
//! | globals | `nums["spellinfo:<n>:targets"]`, `nums["spellinfo:<n>:routines"]` | `spell_info[n].targets` / `.routines` |
//! | char | `texts["dg_aff_bits"]` | comma-separated `AFF_*` numbers set by `dg_affect` (so `off` can clear them) |
//!
//! `read_mobile(vnum)` succeeds when `texts["mobproto:<vnum>"]` exists (the
//! shared key); the mob is named from it. `dg_affect` sets the shared
//! `nums["spell:<n>"]` and `nums["aff:<n>"]` keys.
//!
//! Effects recorded: `call_magic …`, `extract_char <uid>`,
//! `affect_to_char …`, `affect_from_char <uid>`.

use crate::magic::MagicHost;
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

    fn magic_load_caster_proxy(&mut self, vnum: i32, room: Uid, short_descr: &str) -> Option<Uid> {
        // Absent prototype: read_mobile fails.
        let _proto = self.globals.texts.get(&format!("mobproto:{vnum}"))?;
        let proxy = self.add_mob(vnum, short_descr);
        self.char_to_room(proxy, room);
        Some(proxy)
    }

    fn magic_extract_caster_proxy(&mut self, proxy: Uid) {
        self.extract_char(proxy);
        self.effect(format!("extract_char {proxy}"));
    }

    fn magic_remove_dg_affects(&mut self, ch: Uid, spell: i32) {
        let bits = self.text(ch, "dg_aff_bits");
        for bit in bits.split(',').filter(|bit| !bit.is_empty()) {
            self.set_num(ch, &format!("aff:{bit}"), 0);
        }
        self.set_text(ch, "dg_aff_bits", "");
        self.set_num(ch, &format!("spell:{spell}"), 0);
        self.effect(format!("affect_from_char {ch}"));
    }

    fn magic_add_dg_affect(
        &mut self,
        ch: Uid,
        spell: i32,
        location: i32,
        modifier: i32,
        duration: i32,
        bit: Option<u32>,
    ) {
        self.set_num(ch, &format!("spell:{spell}"), 1);
        if let Some(bit) = bit {
            self.set_num(ch, &format!("aff:{bit}"), 1);
            let bits = self.text(ch, "dg_aff_bits");
            self.set_text(ch, "dg_aff_bits", &format!("{bits}{bit},"));
        }
        self.effect(format!(
            "affect_to_char {ch} location={location} modifier={modifier} duration={duration} bit={}",
            bit.map_or_else(|| "-".to_owned(), |bit| bit.to_string())
        ));
    }
}
