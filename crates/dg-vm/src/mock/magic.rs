//! [`MagicHost`] for the mock.
//!
//! Keys beyond the shared table (spell names are the shared
//! `globals.texts["spell:<n>"]`, which the VM's `find_skill_num` matches over):
//!
//! | entity | key | meaning (C) |
//! |---|---|---|
//! | globals | `nums["spellinfo:<n>:targets"]`, `nums["spellinfo:<n>:routines"]` | `spell_info[n].targets` / `.routines` |
//! | char | `texts["dg_aff_bits"]` | comma-separated `AFF_*` numbers set by `dg_affect` (so `off` can clear them) |
//!
//! The caster mob an object or room casts through is made by the command
//! module's primitives (`read_mobile` needs the shared
//! `texts["mobproto:<vnum>"]`). `dg_affect` sets the shared
//! `nums["spell:<n>"]` and `nums["aff:<n>"]` keys.
//!
//! A spell cast through `call_magic` runs the victim's `cast` triggers
//! ([`crate::dispatch::cast_mtrigger`], which the host's `call_magic` does in
//! the C) when `globals.nums["cast_triggers"]` is non-zero.
//!
//! Effects recorded: `call_magic …`, `affect_to_char …`,
//! `affect_from_char <uid>`.

use crate::magic::MagicHost;
use crate::mock::MockHost;
use crate::types::Uid;

impl MockHost {
    /// Register a spell in `spell_info[]`: its name, targets and routines.
    pub fn add_spell(&mut self, name: &str, spellnum: i32, targets: u32, routines: u32) {
        self.globals
            .texts
            .insert(format!("spell:{spellnum}"), name.to_owned());
        let nums = &mut self.globals.nums;
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
    fn magic_spell_name(&self, spellnum: i32) -> Option<String> {
        self.globals
            .texts
            .get(&format!("spell:{spellnum}"))
            .cloned()
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
        if self.globals.nums.get("cast_triggers").copied().unwrap_or(0) != 0 {
            crate::dispatch::cast_mtrigger(self, caster, victim, spellnum);
        }
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
        modifier: i8,
        duration: i16,
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
