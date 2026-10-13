//! `DispatchHost`: what `dg_triggers.c` needs beyond the shared primitives.

use dg_vm::Uid;
use dg_vm::dispatch::DispatchHost;
use mib_stdlib::prelude::{Item, Npc, NpcKey};

use super::EcsHost;
use crate::dg::script::DgProtos;

impl DispatchHost for EcsHost<'_> {
    /// Empty the attachment list of the mob's or object's prototype in
    /// [`DgProtos`] (`npc:<id>`, `item:<id>`), so nothing created from it
    /// afterwards carries triggers. Entities that exist keep theirs, as the C
    /// frees only the prototype's list. A player, a room or a missing entity
    /// has no prototype and is left alone.
    fn disp_free_proto_script(&mut self, uid: Uid) {
        let Some(entity) = self.entity(uid) else {
            return;
        };
        let owner = if self.world.get::<Npc>(entity).is_some() {
            self.world
                .get::<NpcKey>(entity)
                .map(|key| format!("npc:{}", key.0))
        } else {
            self.world
                .get::<Item>(entity)
                .map(|item| format!("item:{}", item.def_id))
        };
        let Some(owner) = owner else {
            return;
        };
        if let Some(mut protos) = self.world.get_resource_mut::<DgProtos>() {
            protos.set_attachments(&owner, Vec::new());
        }
    }
}
