//! `EcsHost`: the [`dg_vm::DgHost`] implementation over the Bevy world.
//!
//! An `EcsHost` borrows the `World` for the length of one dispatch. The nine
//! traits `DgHost` unions are implemented one per file beside this one;
//! [`BaseHost`] and [`WorldHost`] live here. Every method is unmodelled for
//! now and answers the neutral value `HOST.md` lists for it.

mod chars;
mod commands;
mod dispatch;
mod magic;
mod objs;
mod rooms;
mod subst;

use std::sync::Arc;

use dg_vm::types::VmState;
use dg_vm::types::{MudTime, ScriptMemory};
use dg_vm::{BaseHost, Kind, Script, TrigProto, Uid, WorldHost};
use mib_core::prelude::*;

use crate::dg::DgVm;
use crate::dg::mobcmd;

/// The host the VM runs against: the ECS world, borrowed for one dispatch.
pub struct EcsHost<'w> {
    pub world: &'w mut World,
}

/// `EcsHost` implements every host trait, which is what `DgHost` requires.
const _: fn() = || {
    fn assert_dg_host<T: dg_vm::DgHost>() {}
    assert_dg_host::<EcsHost<'static>>();
};

impl BaseHost for EcsHost<'_> {
    fn exists(&self, _uid: Uid) -> bool {
        false
    }

    fn script(&self, _uid: Uid) -> Option<&Script> {
        None
    }

    fn script_mut(&mut self, _uid: Uid) -> Option<&mut Script> {
        None
    }

    fn ensure_script(&mut self, _uid: Uid) -> Option<&mut Script> {
        None
    }

    fn remove_script(&mut self, _uid: Uid) -> Option<Script> {
        None
    }

    fn vm_state(&mut self) -> &mut VmState {
        &mut self
            .world
            .get_resource_or_insert_with(DgVm::default)
            .into_inner()
            .state
    }

    fn script_players(&self) -> bool {
        false
    }

    fn pulse(&self) -> i64 {
        0
    }

    fn rand_number(&mut self, from: i64, to: i64) -> i64 {
        from.min(to)
    }

    fn mud_time(&self) -> MudTime {
        MudTime::default()
    }

    fn trig_proto(&self, _vnum: i32) -> Option<Arc<TrigProto>> {
        None
    }

    fn schedule_wait(&mut self, _owner: Uid, _trigger_id: u64, _pulses: i64) {}

    fn cancel_wait(&mut self, _owner: Uid, _trigger_id: u64) {}

    fn mud_command(&mut self, ch: Uid, line: &str) {
        mobcmd::mud_command(self, ch, line);
    }

    fn mud_command_name(&self, prefix: &str) -> Option<String> {
        mobcmd::mud_command_name(prefix)
    }

    fn script_log(&mut self, _message: &str) {}

    fn unsupported(&mut self, _what: &str, _detail: &str) {}
}

impl WorldHost for EcsHost<'_> {
    fn kind(&self, _uid: Uid) -> Option<Kind> {
        None
    }

    fn is_npc(&self, _ch: Uid) -> bool {
        false
    }

    fn valid_dg_target(&self, _ch: Uid, _allow_gods: bool) -> bool {
        true
    }

    fn name(&self, _uid: Uid) -> String {
        String::new()
    }

    fn keywords(&self, _uid: Uid) -> String {
        String::new()
    }

    fn vnum(&self, _uid: Uid) -> i32 {
        -1
    }

    fn in_room(&self, _uid: Uid) -> Option<Uid> {
        None
    }

    fn room_people(&self, _room: Uid) -> Vec<Uid> {
        Vec::new()
    }

    fn room_contents(&self, _room: Uid) -> Vec<Uid> {
        Vec::new()
    }

    fn carrying(&self, _ch: Uid) -> Vec<Uid> {
        Vec::new()
    }

    fn equipment(&self, _ch: Uid, _pos: i32) -> Option<Uid> {
        None
    }

    fn obj_contains(&self, _obj: Uid) -> Vec<Uid> {
        Vec::new()
    }

    fn obj_carried_by(&self, _obj: Uid) -> Option<Uid> {
        None
    }

    fn obj_worn_by(&self, _obj: Uid) -> Option<Uid> {
        None
    }

    fn obj_in_obj(&self, _obj: Uid) -> Option<Uid> {
        None
    }

    fn character_list(&self) -> Vec<Uid> {
        Vec::new()
    }

    fn object_list(&self) -> Vec<Uid> {
        Vec::new()
    }

    fn real_room(&self, _vnum: i32) -> Option<Uid> {
        None
    }

    fn can_see(&self, _ch: Uid, _target: Uid) -> bool {
        true
    }

    fn level(&self, _ch: Uid) -> i32 {
        0
    }

    fn is_dead(&self, _ch: Uid) -> bool {
        false
    }

    fn obj_type(&self, _obj: Uid) -> i32 {
        0
    }

    fn wear_flags(&self, _obj: Uid) -> u32 {
        0
    }

    fn script_mem(&self, _ch: Uid) -> Vec<ScriptMemory> {
        Vec::new()
    }

    fn set_script_mem(&mut self, _ch: Uid, _memory: Vec<ScriptMemory>) {}
}
