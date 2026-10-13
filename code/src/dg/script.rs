//! Per-entity script state and the trigger prototypes built from `tba.triggers`.
//!
//! [`DgProtos`] is built once from the pack content: one [`TrigProto`] per
//! trigger record, and for each owner (`npc:<id>`, `item:<id>`, `room:<key>`)
//! the vnums of the triggers it carries, in attachment order. An entity that
//! has not yet been through [`attach_proto_scripts`] gets its triggers then
//! (the C's `copy_proto_script` + `assign_triggers`) and the [`DgScriptInit`]
//! marker, so it is attached exactly once.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use dg_vm::Uid;
use dg_vm::script::Program;
use dg_vm::types::{AttachType, ScriptMemory, TrigProto, trig::asciiflag_conv};
use mib_core::prelude::*;
use mib_stdlib::prelude::{Item, Npc, NpcKey, Room};

use super::ids::{DgId, ensure_ids};
use crate::triggers::content::{TriggerDef, TriggerId};

/// An entity's script: its attached triggers and its global variables.
#[derive(Component, Debug, Clone, Default)]
pub struct DgScript(pub dg_vm::Script);

/// The characters a mob remembers (`SCRIPT_MEM`), head first.
#[derive(Component, Debug, Clone, Default)]
pub struct DgMemory(pub Vec<ScriptMemory>);

/// A character a script extracted. The C defers the extraction to the end of
/// the pulse (`NOTDEADYET`); the entity stays in its room's lists, answering
/// `is_dead`, until the outermost `with_host` call finishes and sweeps it.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct DgDead;

/// The entity has been through the prototype attach pass.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct DgScriptInit;

/// The trigger prototypes and the triggers each owner carries.
#[derive(Resource, Debug, Default, Clone)]
pub struct DgProtos {
    protos: HashMap<i32, Arc<TrigProto>>,
    owners: HashMap<String, Vec<i32>>,
    /// Trigger records that could not be loaded.
    pub skipped_records: u32,
    /// Attachment entries that named a trigger that was not loaded.
    pub skipped_attachments: u32,
}

impl DgProtos {
    /// Build the prototypes from the pack's trigger records and attachments.
    ///
    /// A record with a missing or unknown `attach_type`, or no type flags, is
    /// skipped and counted; so is an attachment entry naming a trigger that is
    /// not loaded. Neither stops the rest from loading.
    pub fn build(
        triggers: &BTreeMap<TriggerId, TriggerDef>,
        attachments: &BTreeMap<String, Vec<TriggerId>>,
    ) -> Self {
        let mut protos = Self::default();
        let mut by_id: HashMap<&TriggerId, i32> = HashMap::new();
        for (id, def) in triggers {
            if protos.add(def) {
                by_id.insert(id, def.vnum);
            }
        }
        for (owner, list) in attachments {
            let mut vnums = Vec::with_capacity(list.len());
            for id in list {
                match by_id.get(id) {
                    Some(vnum) => vnums.push(*vnum),
                    None => protos.skipped_attachments += 1,
                }
            }
            protos.owners.insert(owner.clone(), vnums);
        }
        if protos.skipped_records > 0 || protos.skipped_attachments > 0 {
            tracing::warn!(
                records = protos.skipped_records,
                attachments = protos.skipped_attachments,
                "DG trigger content skipped"
            );
        }
        protos
    }

    /// Load one trigger record. `false` (and counted) when it has no usable
    /// `attach_type` or no type flags.
    pub fn add(&mut self, def: &TriggerDef) -> bool {
        let attach_type = match def.attach_type.as_str() {
            "mob" => Some(AttachType::Mob),
            "obj" => Some(AttachType::Obj),
            "wld" => Some(AttachType::Wld),
            _ => None,
        };
        let Some(attach_type) = attach_type.filter(|_| !def.flags.trim().is_empty()) else {
            self.skipped_records += 1;
            return false;
        };
        let proto = TrigProto {
            vnum: def.vnum,
            name: def.name.clone(),
            attach_type,
            trigger_type: asciiflag_conv(&def.flags),
            narg: def.narg,
            arglist: def.arglist.clone(),
            program: Arc::new(Program::parse(&def.script)),
        };
        self.protos.insert(def.vnum, Arc::new(proto));
        true
    }

    /// Set the triggers an owner (`npc:<id>`, `item:<id>`, `room:<key>`)
    /// carries, in attachment order.
    pub fn set_attachments(&mut self, owner: &str, vnums: Vec<i32>) {
        self.owners.insert(owner.to_owned(), vnums);
    }

    /// The prototype with this vnum.
    pub fn get(&self, vnum: i32) -> Option<Arc<TrigProto>> {
        self.protos.get(&vnum).cloned()
    }

    /// The vnums of the triggers an owner carries, in attachment order.
    pub fn owner(&self, owner: &str) -> Option<&[i32]> {
        self.owners.get(owner).map(Vec::as_slice)
    }

    /// How many prototypes are loaded.
    pub fn len(&self) -> usize {
        self.protos.len()
    }

    /// Whether no prototype is loaded.
    pub fn is_empty(&self) -> bool {
        self.protos.is_empty()
    }

    /// How many owners have an attachment list.
    pub fn owner_count(&self) -> usize {
        self.owners.len()
    }
}

/// The attachment key of an entity: `npc:<id>`, `item:<id>` or `room:<key>`.
fn owner_key(world: &World, entity: Entity) -> Option<String> {
    if let Some(room) = world.get::<Room>(entity) {
        return Some(format!("room:{}", room.key));
    }
    if world.get::<Npc>(entity).is_some() {
        return world
            .get::<NpcKey>(entity)
            .map(|key| format!("npc:{}", key.0));
    }
    world
        .get::<Item>(entity)
        .map(|item| format!("item:{}", item.def_id))
}

/// Attach an entity's prototype triggers (in attachment order) if it has not
/// been initialised, and mark it. Returns whether it was initialised now.
pub fn attach_proto_scripts_to(world: &mut World, entity: Entity) -> bool {
    if world.get_entity(entity).is_err() || world.get::<DgScriptInit>(entity).is_some() {
        return false;
    }
    let protos: Vec<Arc<TrigProto>> = {
        let Some(table) = world.get_resource::<DgProtos>() else {
            world.entity_mut(entity).insert(DgScriptInit);
            return true;
        };
        owner_key(world, entity)
            .and_then(|key| table.owner(&key))
            .map(|vnums| vnums.iter().filter_map(|vnum| table.get(*vnum)).collect())
            .unwrap_or_default()
    };
    let mut entity_mut = world.entity_mut(entity);
    entity_mut.insert(DgScriptInit);
    if !protos.is_empty() {
        // An entity that already has a script (a trigger attached to it before
        // its pass) keeps it: the prototype triggers are appended
        // (`assign_triggers`).
        let mut script = entity_mut
            .take::<DgScript>()
            .map(|script| script.0)
            .unwrap_or_default();
        for proto in protos {
            script.add_trigger(proto, -1);
        }
        entity_mut.insert(DgScript(script));
    }
    true
}

/// Give every room, NPC and item that has not been initialised its prototype
/// triggers. Returns the mobs and objects initialised now, newest last, so the
/// caller can fire their `load` triggers (the C calls `load_mtrigger` /
/// `load_otrigger` when the zone reset or a load command creates them).
pub fn attach_proto_scripts(world: &mut World) -> Vec<Entity> {
    let mut rooms = world.query_filtered::<Entity, (With<Room>, Without<DgScriptInit>)>();
    let fresh_rooms: Vec<Entity> = rooms.iter(world).collect();
    for entity in fresh_rooms {
        attach_proto_scripts_to(world, entity);
    }

    let mut loaded = Vec::new();
    let mut npcs = world.query_filtered::<Entity, (With<Npc>, Without<DgScriptInit>)>();
    let fresh_npcs: Vec<Entity> = npcs.iter(world).collect();
    let mut items = world.query_filtered::<Entity, (With<Item>, Without<DgScriptInit>)>();
    let fresh_items: Vec<Entity> = items.iter(world).collect();
    for entity in fresh_npcs.into_iter().chain(fresh_items) {
        if attach_proto_scripts_to(world, entity) {
            loaded.push(entity);
        }
    }
    loaded
}

/// Give a freshly created entity its id, list-order stamps and prototype
/// scripts without firing any trigger, and return its id. The script `load`
/// commands call this for the mob or object they create; the caller fires the
/// load trigger itself when the C does.
pub fn init_entity(world: &mut World, entity: Entity) -> Option<Uid> {
    ensure_ids(world);
    attach_proto_scripts_to(world, entity);
    world.get::<DgId>(entity).map(|id| id.0)
}
