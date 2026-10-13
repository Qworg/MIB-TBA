//! `MagicHost`: spells and affects.
//!
//! Every method is unmodelled for now and answers the neutral value
//! `HOST.md` lists for it.

use dg_vm::Uid;
use dg_vm::magic::MagicHost;

use super::EcsHost;

impl MagicHost for EcsHost<'_> {
    fn magic_spell_name(&self, _spellnum: i32) -> Option<String> {
        None
    }

    fn magic_spell_targets(&self, _spellnum: i32) -> u32 {
        0
    }

    fn magic_spell_routines(&self, _spellnum: i32) -> u32 {
        0
    }

    fn magic_call_magic(
        &mut self,
        _caster: Uid,
        _victim: Option<Uid>,
        _obj: Option<Uid>,
        _spellnum: i32,
        _level: i32,
    ) {
    }

    fn magic_remove_dg_affects(&mut self, _ch: Uid, _spell: i32) {}

    fn magic_add_dg_affect(
        &mut self,
        _ch: Uid,
        _spell: i32,
        _location: i32,
        _modifier: i8,
        _duration: i16,
        _bit: Option<u32>,
    ) {
    }
}
