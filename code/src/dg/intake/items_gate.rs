//! The engine's own item commands, carried out here with their triggers.
//!
//! `get`, `drop`, `put` and the pack's `equip` / `unequip` are done by the
//! engine and the equipment module after the router, which would commit a
//! different object than the one a trigger approved whenever a script changed
//! the world in between (an actor teleported by a `get` trigger no longer finds
//! the object in the room the router looks in), and would run them after the
//! intake's own verbs from the same frame. Each is therefore resolved here as
//! the engine resolves it (its name matching is mirrored by
//! [`EngineView`]), the C's triggers run, and the move is committed on the very
//! object that was resolved, with the engine's messages. The command is then
//! marked handled.

use bevy::prelude::{Fixed, Time};
use dg_vm::BaseHost;
use dg_vm::dispatch::{drop_otrigger, drop_wtrigger, get_otrigger, remove_otrigger, wear_otrigger};
use dg_vm::{Uid, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    CarriedBy, ContainedBy, Container, GroundItem, Item, Location, Player, QuestEvent,
};

use super::common::{EngineView, prompt, tell};
use crate::content::{ItemExtraFlag, WearSlot};
use crate::dg::host::{EcsHost, wear_position};
use crate::equipment::{TbaEquipment, choose_slot, clear_worn, forget_worn, set_worn};
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

/// Carry out `command` for `actor` in `room`, the C's triggers first.
pub fn perform(host: &mut EcsHost<'_>, actor: Entity, room: Entity, command: &Gated) {
    match command {
        Gated::Get { item, from } => get(host, actor, room, item, from.as_deref()),
        Gated::Drop { item } => drop_item(host, actor, item),
        Gated::Put { item, into } => put(host, actor, room, item, into),
        Gated::Equip { item } => equip(host, actor, item),
        Gated::Unequip { slot } => unequip(host, actor, slot),
    }
}

// -- the engine's replies ----------------------------------------------------------

/// A `NoSuchTarget` error and the prompt.
fn deny(world: &mut World, session: Entity, message: String) {
    if let Some(mut output) = world.get_resource_mut::<Messages<OutputEvent>>() {
        output.write(OutputEvent {
            session,
            payload: OutboundPayload::error(ErrorKind::NoSuchTarget, message),
        });
    }
    prompt(world, session);
}

/// A system-channel notice and the prompt.
fn notice(world: &mut World, session: Entity, message: String) {
    if let Some(mut output) = world.get_resource_mut::<Messages<OutputEvent>>() {
        output.write(OutputEvent {
            session,
            payload: OutboundPayload::text(TextChannel::System, message),
        });
    }
    prompt(world, session);
}

// -- moving an object ----------------------------------------------------------------

/// Take an object out of the container that holds it.
fn leave_container(world: &mut World, item: Entity) {
    let Some(holder) = world.get::<ContainedBy>(item).map(|held| held.0) else {
        return;
    };
    let Some(mut container) = world.get::<Container>(holder).cloned() else {
        return;
    };
    container.contents.retain(|entity| *entity != item);
    if let Ok(mut entity) = world.get_entity_mut(holder) {
        entity.insert(container);
    }
}

/// Take an object out of wherever it is: a wearer's equipment, a container,
/// the floor.
fn lift(world: &mut World, item: Entity) {
    forget_worn(world, item);
    leave_container(world, item);
    if let Ok(mut entity) = world.get_entity_mut(item) {
        entity.remove::<(Location, GroundItem, ContainedBy, CarriedBy)>();
    }
}

/// `obj_from_*` then `obj_to_char`.
fn take_into_hand(host: &mut EcsHost<'_>, item: Entity, actor: Entity) {
    lift(host.world, item);
    if let Ok(mut entity) = host.world.get_entity_mut(item) {
        entity.insert(CarriedBy(actor));
    }
    host.stamp_order(item);
}

/// The engine's `QuestEvent::Collected` for an item a player now carries.
fn collected(world: &mut World, player: Entity, item: Entity) {
    if world.get::<Player>(player).is_none() {
        return;
    }
    let Some(def_id) = world.get::<Item>(item).map(|item| item.def_id.clone()) else {
        return;
    };
    if let Some(mut events) = world.get_resource_mut::<Messages<QuestEvent>>() {
        events.write(QuestEvent::Collected {
            player,
            item: def_id,
        });
    }
}

/// The items the actor wears.
fn worn_items(world: &World, actor: Entity) -> Vec<Entity> {
    world
        .get::<TbaEquipment>(actor)
        .map(|equipment| equipment.slots.values().copied().collect())
        .unwrap_or_default()
}

/// Whether the object a trigger just ran for is still in the world.
fn alive(host: &EcsHost<'_>, obj: Uid) -> bool {
    host.entity(obj).is_some()
}

// -- get -----------------------------------------------------------------------------

/// `perform_get_from_room` / `perform_get_from_container`: `get_otrigger`
/// before the object moves.
fn get(host: &mut EcsHost<'_>, actor: Entity, room: Entity, item: &str, from: Option<&str>) {
    let view = EngineView::new(host.world);
    let (target, source) = match from {
        Some(container_name) => {
            let Some(container) = view.container(actor, room, container_name) else {
                deny(
                    host.world,
                    actor,
                    format!("You see no {container_name} here."),
                );
                return;
            };
            let Some(contents) = host
                .world
                .get::<Container>(container)
                .map(|container| container.contents.clone())
            else {
                prompt(host.world, actor);
                return;
            };
            let Some(target) = view.in_contents(&contents, item) else {
                deny(
                    host.world,
                    actor,
                    format!("You see no {item} in the {container_name}."),
                );
                return;
            };
            (target, Some(container_name))
        }
        None => {
            if let Some(carried) = view.carried(actor, item) {
                let name = item_name(host, carried);
                notice(
                    host.world,
                    actor,
                    format!("You are already carrying {name}."),
                );
                return;
            }
            let Some(target) = view.ground(room, item) else {
                deny(host.world, actor, format!("You don't see {item} here."));
                return;
            };
            (target, None)
        }
    };
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        prompt(host.world, actor);
        return;
    };
    if get_otrigger(host, obj, ch) == 0 || !alive(host, obj) {
        prompt(host.world, actor);
        return;
    }
    host.unsupported("carry_limit", "the pack has no carrying capacity or weight");
    take_into_hand(host, target, actor);
    collected(host.world, actor, target);
    let name = item_name(host, target);
    let message = match source {
        Some(container_name) => format!("You take {name} from the {container_name}."),
        None => format!("You pick up {name}."),
    };
    notice(host.world, actor, message);
}

// -- drop and put ------------------------------------------------------------------------

/// `perform_drop`: `drop_otrigger`, then `drop_wtrigger`, then the cursed
/// check. The pack has no junk or donate. Only what the actor carries and does
/// not wear can be dropped, as in the C.
fn drop_item(host: &mut EcsHost<'_>, actor: Entity, item: &str) {
    let worn = worn_items(host.world, actor);
    let view = EngineView::new(host.world);
    let Some(target) = view.carried_unworn(actor, item, &worn) else {
        deny(host.world, actor, "You aren't carrying that.".to_owned());
        return;
    };
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        prompt(host.world, actor);
        return;
    };
    if drop_otrigger(host, obj, ch) == 0 || !alive(host, obj) {
        prompt(host.world, actor);
        return;
    }
    if drop_wtrigger(host, obj, ch) == 0 || !alive(host, obj) {
        prompt(host.world, actor);
        return;
    }
    let name = item_name(host, target);
    if has_extra_flag(host, target, ItemExtraFlag::NoDrop) {
        tell(
            host.world,
            actor,
            &format!("You can't drop {name}, it must be CURSED!"),
        );
        prompt(host.world, actor);
        return;
    }
    let Some(room) = host
        .world
        .get::<Location>(actor)
        .map(|location| location.room)
    else {
        prompt(host.world, actor);
        return;
    };
    let spawned_at = host
        .world
        .get_resource::<Time<Fixed>>()
        .map_or(0.0, |time| time.elapsed_secs());
    lift(host.world, target);
    if let Ok(mut entity) = host.world.get_entity_mut(target) {
        entity.insert((Location { room }, GroundItem { spawned_at }));
    }
    host.stamp_order(target);
    notice(host.world, actor, format!("You drop {name}."));
}

/// `perform_put`: `drop_otrigger` on the object going into the container. An
/// object lying on the floor is picked up by the engine's `put`, so it is asked
/// to be taken first.
fn put(host: &mut EcsHost<'_>, actor: Entity, room: Entity, item: &str, into: &str) {
    let worn = worn_items(host.world, actor);
    let view = EngineView::new(host.world);
    let carried = view.carried_unworn(actor, item, &worn);
    let Some(target) = carried.or_else(|| view.ground(room, item)) else {
        deny(host.world, actor, format!("You don't have {item}."));
        return;
    };
    let Some(container) = view.container(actor, room, into) else {
        deny(host.world, actor, format!("You see no {into} here."));
        return;
    };
    if target == container {
        deny(
            host.world,
            actor,
            "You can't put something inside itself.".to_owned(),
        );
        return;
    }
    if host.world.get::<Container>(container).is_none() {
        prompt(host.world, actor);
        return;
    }
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        prompt(host.world, actor);
        return;
    };
    if carried.is_none() && (get_otrigger(host, obj, ch) == 0 || !alive(host, obj)) {
        prompt(host.world, actor);
        return;
    }
    if drop_otrigger(host, obj, ch) == 0 || !alive(host, obj) {
        prompt(host.world, actor);
        return;
    }
    if has_extra_flag(host, target, ItemExtraFlag::NoDrop) {
        if host.world.get::<Location>(container).is_some() {
            // A cursed object cannot be left in a container on the floor.
            let name = item_name(host, target);
            deny(
                host.world,
                actor,
                format!("You can't get {name} out of your hand."),
            );
            return;
        }
        // The C curses a carried container that takes it; the pack keeps no
        // per-instance flags.
        host.unsupported(
            "put_curse_container",
            "the pack keeps no per-instance ITEM_NODROP to set on the container",
        );
    }
    // The script may have emptied or destroyed the container.
    let Some(mut contents) = host.world.get::<Container>(container).cloned() else {
        prompt(host.world, actor);
        return;
    };
    if contents.contents.len() as u64 >= u64::from(contents.capacity) {
        let name = item_name(host, container);
        deny(host.world, actor, format!("{name} can't hold any more."));
        return;
    }
    lift(host.world, target);
    // `lift` may have rewritten this very container.
    if let Some(current) = host.world.get::<Container>(container).cloned() {
        contents = current;
    }
    contents.contents.retain(|entity| *entity != target);
    contents.contents.push(target);
    if let Ok(mut entity) = host.world.get_entity_mut(container) {
        entity.insert(contents);
    }
    if let Ok(mut entity) = host.world.get_entity_mut(target) {
        entity.insert(ContainedBy(container));
    }
    host.stamp_order(target);
    let (name, container_name) = (item_name(host, target), item_name(host, container));
    notice(
        host.world,
        actor,
        format!("You put {name} in {container_name}."),
    );
}

// -- equip and unequip ----------------------------------------------------------------------

/// `equip <item>` with `perform_wear`'s trigger: `wear_otrigger` with the
/// position the item will take.
fn equip(host: &mut EcsHost<'_>, actor: Entity, item: &str) {
    let view = EngineView::new(host.world);
    let Some(target) = view.carried(actor, item) else {
        deny(host.world, actor, "You aren't carrying that.".to_owned());
        return;
    };
    let name = item_name(host, target);
    let Some(def) = def_of(host, target) else {
        deny(host.world, actor, format!("You can't equip {name}."));
        return;
    };
    let worn = host
        .world
        .get::<TbaEquipment>(actor)
        .map(|equipment| equipment.slots.clone())
        .unwrap_or_default();
    if worn.values().any(|entity| *entity == target) {
        notice(
            host.world,
            actor,
            format!("You are already wearing {name}."),
        );
        return;
    }
    let Some(slot) = choose_slot(&def, &worn) else {
        if def.slots.is_empty() {
            deny(host.world, actor, format!("You can't equip {name}."));
        } else {
            let slots: Vec<String> = def
                .slots
                .iter()
                .map(|slot| slot.display_name().to_owned())
                .collect();
            deny(
                host.world,
                actor,
                format!("You have no free slot for {name} ({}).", slots.join(", ")),
            );
        }
        return;
    };
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        prompt(host.world, actor);
        return;
    };
    // `(obj->carried_by != ch)` after the trigger: a script that wore, moved
    // or destroyed the object ends the command, and so does one that filled
    // the position.
    if wear_otrigger(host, obj, ch, wear_position(slot)) == 0
        || host.obj_carried_by(obj) != Some(ch)
        || host
            .world
            .get::<TbaEquipment>(actor)
            .is_some_and(|equipment| equipment.worn(slot).is_some())
    {
        prompt(host.world, actor);
        return;
    }
    set_worn(host.world, actor, slot, target);
    host.stamp_order(target);
    notice(
        host.world,
        actor,
        format!("You equip {name} ({}).", slot.display_name()),
    );
}

/// `unequip <slot>` with `perform_remove`'s cursed check and `remove_otrigger`.
fn unequip(host: &mut EcsHost<'_>, actor: Entity, slot_word: &str) {
    let Some(slot) = WearSlot::parse(slot_word) else {
        deny(
            host.world,
            actor,
            format!("There's no such slot as {slot_word}."),
        );
        return;
    };
    let Some(target) = host
        .world
        .get::<TbaEquipment>(actor)
        .and_then(|equipment| equipment.worn(slot))
    else {
        deny(
            host.world,
            actor,
            format!(
                "You aren't wearing anything on your {}.",
                slot.display_name()
            ),
        );
        return;
    };
    let name = item_name(host, target);
    if has_extra_flag(host, target, ItemExtraFlag::NoDrop) {
        tell(
            host.world,
            actor,
            &format!("You can't remove {name}, it must be CURSED!"),
        );
        prompt(host.world, actor);
        return;
    }
    let (Some(obj), Some(ch)) = (host.uid(target), host.uid(actor)) else {
        prompt(host.world, actor);
        return;
    };
    if remove_otrigger(host, obj, ch) == 0
        || host
            .world
            .get::<TbaEquipment>(actor)
            .and_then(|equipment| equipment.worn(slot))
            != Some(target)
    {
        prompt(host.world, actor);
        return;
    }
    clear_worn(host.world, actor, slot);
    host.stamp_order(target);
    notice(host.world, actor, format!("You remove {name}."));
}

// -- the prototype --------------------------------------------------------------------------

/// The prototype definition of an item.
fn def_of(host: &EcsHost<'_>, item: Entity) -> Option<crate::content::TbaItemDef> {
    let def_id = host.world.get::<Item>(item)?.def_id.clone();
    host.world
        .get_resource::<TbaContent>()?
        .items
        .get(&def_id)
        .cloned()
}

/// Whether the prototype of an item carries an extra flag.
pub fn has_extra_flag(host: &EcsHost<'_>, item: Entity, flag: ItemExtraFlag) -> bool {
    def_of(host, item).is_some_and(|def| def.extra_flags.contains(&flag))
}

/// The short description of an item.
pub fn item_name(host: &EcsHost<'_>, item: Entity) -> String {
    host.world
        .get::<Item>(item)
        .map(|item| item.name.clone())
        .unwrap_or_default()
}
