//! The triggers in front of the engine's own item commands.
//!
//! `get`, `drop`, `put` and the pack's `equip` / `unequip` do their work after
//! the router. The C asks the object's trigger before it commits, so each is
//! resolved here to the object the command will act on, the trigger runs, and a
//! refusal stops the command.

use dg_vm::dispatch::{drop_otrigger, drop_wtrigger, get_otrigger, remove_otrigger, wear_otrigger};
use mib_core::prelude::*;
use mib_stdlib::prelude::{CarriedBy, Container, Item};

use super::common::{EngineView, tell};
use crate::content::{ItemExtraFlag, WearSlot};
use crate::dg::host::{EcsHost, wear_position};
use crate::equipment::{TbaEquipment, choose_slot};
use crate::module::TbaContent;

/// One of the engine's item commands, as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gated {
    Get { item: String, from: Option<String> },
    Drop { item: String },
    Put { item: String, into: String },
    Equip { item: String },
    Unequip { slot: String },
}

/// Run the trigger the C runs before `command` commits. Returns whether the
/// command is refused. A command whose target the engine cannot find is not
/// refused: the engine answers it.
pub fn refuses(host: &mut EcsHost<'_>, actor: Entity, room: Entity, command: &Gated) -> bool {
    match command {
        Gated::Get { item, from } => get(host, actor, room, item, from.as_deref()),
        Gated::Drop { item } => drop_item(host, actor, item),
        Gated::Put { item, into } => put(host, actor, room, item, into),
        Gated::Equip { item } => equip(host, actor, item),
        Gated::Unequip { slot } => unequip(host, actor, slot),
    }
}

/// `perform_get_from_room` / `perform_get_from_container`: `get_otrigger`.
fn get(
    host: &mut EcsHost<'_>,
    actor: Entity,
    room: Entity,
    item: &str,
    from: Option<&str>,
) -> bool {
    let view = EngineView::new(host.world);
    let target = match from {
        Some(container) => {
            let Some(container) = view.container(actor, room, container) else {
                return false;
            };
            let contents = host
                .world
                .get::<Container>(container)
                .map(|container| container.contents.clone())
                .unwrap_or_default();
            view.in_contents(&contents, item)
        }
        None => {
            // A thing already carried is answered by the engine, not taken.
            if view.carried(actor, item).is_some() {
                return false;
            }
            view.ground(room, item)
        }
    };
    let Some(target) = target else {
        return false;
    };
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        return false;
    };
    get_otrigger(host, obj, ch) == 0
}

/// `perform_drop`: `drop_otrigger`, then `drop_wtrigger`, then the cursed
/// check. The pack has no junk or donate.
fn drop_item(host: &mut EcsHost<'_>, actor: Entity, item: &str) -> bool {
    let view = EngineView::new(host.world);
    let Some(target) = view.carried(actor, item) else {
        return false;
    };
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        return false;
    };
    if drop_otrigger(host, obj, ch) == 0 || host.entity(obj).is_none() {
        return true;
    }
    if drop_wtrigger(host, obj, ch) == 0 || host.entity(obj).is_none() {
        return true;
    }
    if has_extra_flag(host, target, ItemExtraFlag::NoDrop) {
        let name = item_name(host, target);
        tell(
            host.world,
            actor,
            &format!("You can't drop {name}, it must be CURSED!"),
        );
        return true;
    }
    false
}

/// `perform_put`: `drop_otrigger` on the object going into the container.
fn put(host: &mut EcsHost<'_>, actor: Entity, room: Entity, item: &str, into: &str) -> bool {
    let view = EngineView::new(host.world);
    let (Some(target), Some(container)) =
        (view.carried(actor, item), view.container(actor, room, into))
    else {
        return false;
    };
    if target == container {
        return false;
    }
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        return false;
    };
    drop_otrigger(host, obj, ch) == 0 || host.entity(obj).is_none()
}

/// `perform_wear` for the pack's `equip <item>`: `wear_otrigger` with the
/// position the item will take.
fn equip(host: &mut EcsHost<'_>, actor: Entity, item: &str) -> bool {
    let view = EngineView::new(host.world);
    let Some(target) = view.carried(actor, item) else {
        return false;
    };
    let worn = host
        .world
        .get::<TbaEquipment>(actor)
        .map(|equipment| equipment.slots.clone())
        .unwrap_or_default();
    if worn.values().any(|entity| *entity == target) {
        return false;
    }
    let Some(slot) = slot_for(host, target, &worn) else {
        return false;
    };
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        return false;
    };
    wear_otrigger(host, obj, ch, wear_position(slot)) == 0
        || host.world.get::<CarriedBy>(target).map(|carried| carried.0) != Some(actor)
}

/// The slot `equip` would put the item in.
fn slot_for(
    host: &EcsHost<'_>,
    item: Entity,
    worn: &std::collections::HashMap<WearSlot, Entity>,
) -> Option<WearSlot> {
    let def_id = host.world.get::<Item>(item)?.def_id.clone();
    let def = host
        .world
        .get_resource::<TbaContent>()?
        .items
        .get(&def_id)?;
    choose_slot(def, worn)
}

/// `perform_remove` for the pack's `unequip <slot>`: `remove_otrigger`.
fn unequip(host: &mut EcsHost<'_>, actor: Entity, slot: &str) -> bool {
    let Some(slot) = WearSlot::parse(slot) else {
        return false;
    };
    let Some(target) = host
        .world
        .get::<TbaEquipment>(actor)
        .and_then(|equipment| equipment.worn(slot))
    else {
        return false;
    };
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        return false;
    };
    remove_otrigger(host, obj, ch) == 0
}

/// Whether the prototype of an item carries an extra flag.
pub fn has_extra_flag(host: &EcsHost<'_>, item: Entity, flag: ItemExtraFlag) -> bool {
    let Some(def_id) = host.world.get::<Item>(item).map(|item| item.def_id.clone()) else {
        return false;
    };
    host.world
        .get_resource::<TbaContent>()
        .and_then(|content| content.items.get(&def_id))
        .is_some_and(|def| def.extra_flags.contains(&flag))
}

/// The short description of an item.
pub fn item_name(host: &EcsHost<'_>, item: Entity) -> String {
    host.world
        .get::<Item>(item)
        .map(|item| item.name.clone())
        .unwrap_or_default()
}
