//! The zone reset engine (TBA-E07).
//!
//! A tba zone is an **imperative reset program**, not a spawn table: `G`/`E`/`P`
//! act on the mob placed by the preceding `M`, and are skipped when that
//! placement did not happen (doc §3.1). [`zone_reset_system`] is that
//! interpreter:
//!
//! 1. age every zone by the fixed-tick time;
//! 2. when a zone's `lifespan_mins` elapses, check occupancy for `when_desert`;
//! 3. execute the commands in order, threading `last_mob`;
//! 4. reset the age.
//!
//! It is an **exclusive** system because the stdlib spawn helpers take
//! `&mut World`. It runs on `FixedUpdate` (world simulation), not the combat
//! [`TbaRound`](crate::combat::TbaRound) clock.

use mib_core::prelude::*;
use mib_protocol::command::ExitKey;
use mib_stdlib::prelude::{
    DoorState, Item, ItemId, Location, NpcId, NpcKey, Player, Room, RoomIndex, RoomOccupants,
    RoomState, find_item_def, spawn_item_carried_by_entity, spawn_item_in_container,
    spawn_item_in_room, spawn_npc_from_content,
};

use crate::content::{DoorReset, ResetMode, ZoneCommand, ZoneDef};

/// Per-zone reset age, keyed in the same order as [`TbaContent::zones`].
///
/// [`TbaContent::zones`]: crate::module::TbaContent::zones
#[derive(Resource, Debug, Default)]
pub struct ZoneTracker {
    pub zones: Vec<ZoneState>,
}

impl ZoneTracker {
    /// Build the tracker from the loaded zone definitions (stable `u32` order).
    pub fn from_zones(zones: &std::collections::BTreeMap<u32, ZoneDef>) -> Self {
        Self {
            zones: zones
                .values()
                .cloned()
                .map(|def| ZoneState { def, age_mins: 0.0 })
                .collect(),
        }
    }
}

/// One zone's runtime age.
#[derive(Debug, Clone)]
pub struct ZoneState {
    pub def: ZoneDef,
    pub age_mins: f32,
}

/// `FixedUpdate`: age zones and run the resets that are due.
pub fn zone_reset_system(world: &mut World) {
    let delta_secs = world
        .get_resource::<Time<Fixed>>()
        .map(Time::delta_secs)
        .unwrap_or(0.0);
    let delta_mins = delta_secs / 60.0;

    // Take the tracker out so the body can hold `&mut World` freely; it is
    // always put back, so a reset never loses the resource.
    let mut tracker = world.remove_resource::<ZoneTracker>().unwrap_or_default();

    for zone in &mut tracker.zones {
        zone.age_mins += delta_mins;
        let lifespan = zone.def.lifespan_mins as f32;
        if lifespan > 0.0 && zone.age_mins < lifespan {
            continue;
        }
        if zone.def.reset_mode == ResetMode::Never {
            // A never-resetting zone still keeps its age pinned at zero so a
            // later content edit to `when_desert`/`always` does not fire a
            // backlog of resets at once.
            zone.age_mins = 0.0;
            continue;
        }
        if zone.def.reset_mode == ResetMode::WhenDesert && zone_occupied(world, &zone.def) {
            continue;
        }
        execute_zone(world, &zone.def);
        zone.age_mins = 0.0;
    }

    world.insert_resource(tracker);
}

/// Is any player inside the zone's `[bot, top]` room-number bounds?
fn zone_occupied(world: &mut World, def: &ZoneDef) -> bool {
    let rooms: Vec<Entity> = {
        let mut query = world.query::<(Entity, &Room)>();
        query
            .iter(world)
            .filter(|(_, room)| room.num >= def.bot && room.num <= def.top)
            .map(|(entity, _)| entity)
            .collect()
    };
    let Some(occupants) = world.get_resource::<RoomOccupants>() else {
        return false;
    };
    rooms.iter().any(|&room| {
        occupants
            .occupants(room)
            .iter()
            .any(|&occupant| world.get::<Player>(occupant).is_some())
    })
}

/// Run one zone's commands in order, threading the last placed mob.
fn execute_zone(world: &mut World, def: &ZoneDef) {
    let mut last_mob: Option<Entity> = None;
    for command in &def.commands {
        match command {
            ZoneCommand::PlaceMob {
                mob,
                room,
                max_in_world,
            } => {
                last_mob = if under_limit(*max_in_world, || live_npc_count(world, mob)) {
                    spawn_npc_from_content(world, room, mob)
                } else {
                    None
                };
            }
            ZoneCommand::DropItem {
                item,
                room,
                max_in_world,
            } => {
                if under_limit(*max_in_world, || live_item_count(world, item))
                    && let Some(def) = find_item_def(world, item)
                {
                    spawn_item_in_room(world, room, item, &def);
                }
            }
            ZoneCommand::GiveItem { item, max_in_world } => {
                let Some(mob_entity) = last_mob else { continue };
                if under_limit(*max_in_world, || live_item_count(world, item))
                    && let Some(def) = find_item_def(world, item)
                {
                    spawn_item_carried_by_entity(world, mob_entity, item, &def);
                }
            }
            ZoneCommand::EquipItem {
                item, max_in_world, ..
            } => {
                // The 18-slot equipment model (TBA-E28) lands in the next
                // phase; until it does the item is carried so the reset's
                // referential effect is still visible.
                let Some(mob_entity) = last_mob else { continue };
                if under_limit(*max_in_world, || live_item_count(world, item))
                    && let Some(def) = find_item_def(world, item)
                {
                    spawn_item_carried_by_entity(world, mob_entity, item, &def);
                }
            }
            ZoneCommand::PutItem {
                item,
                container,
                max_in_world,
            } => {
                if !under_limit(*max_in_world, || live_item_count(world, item)) {
                    continue;
                }
                let Some(container_entity) = find_container_entity(world, container) else {
                    continue;
                };
                if let Some(def) = find_item_def(world, item) {
                    spawn_item_in_container(world, container_entity, item, &def);
                }
            }
            ZoneCommand::DoorState { room, exit, state } => {
                set_door_state(world, room, exit, *state);
            }
            ZoneCommand::RemoveItem { room, item } => {
                remove_item_from_room(world, room, item);
            }
        }
    }
}

/// `0` means unlimited (the Circle `max` convention); otherwise the current
/// count must be below the limit.
fn under_limit(max_in_world: u32, count: impl FnOnce() -> usize) -> bool {
    max_in_world == 0 || count() < max_in_world as usize
}

/// Live NPC instances of `mob` (a despawned NPC is no longer an entity).
fn live_npc_count(world: &mut World, mob: &NpcId) -> usize {
    let mut query = world.query::<&NpcKey>();
    query.iter(world).filter(|key| &key.0 == mob).count()
}

/// Live item instances of `item`, wherever they are placed.
fn live_item_count(world: &mut World, item: &ItemId) -> usize {
    let mut query = world.query::<&Item>();
    query
        .iter(world)
        .filter(|held| &held.def_id == item)
        .count()
}

/// The first live container instance of the prototype `container`.
fn find_container_entity(world: &mut World, container: &ItemId) -> Option<Entity> {
    let mut query = world.query::<(Entity, &Item)>();
    query
        .iter(world)
        .find(|(_, held)| &held.def_id == container)
        .map(|(entity, _)| entity)
}

/// Apply a `D` reset to a room's runtime [`RoomState`].
fn set_door_state(
    world: &mut World,
    room: &mib_stdlib::prelude::RoomId,
    exit: &str,
    state: DoorReset,
) {
    let Some(room_entity) = world
        .get_resource::<RoomIndex>()
        .and_then(|index| index.map.get(room).copied())
    else {
        return;
    };
    let Some(direction) = ExitKey::from_word(exit) else {
        return;
    };
    let (closed, locked) = match state {
        DoorReset::Open => (false, false),
        DoorReset::Closed => (true, false),
        DoorReset::Locked => (true, true),
    };
    if let Some(mut room_state) = world.get_mut::<RoomState>(room_entity) {
        if let Some(door) = room_state.doors.get_mut(&direction) {
            door.closed = closed;
            door.locked = locked;
        } else {
            room_state.doors.insert(
                direction,
                DoorState {
                    closed,
                    locked,
                    to: None,
                },
            );
        }
        return;
    }
    // A room with no seeded runtime state still honours the reset.
    let mut room_state = RoomState::default();
    room_state.doors.insert(
        direction,
        DoorState {
            closed,
            locked,
            to: None,
        },
    );
    world.entity_mut(room_entity).insert(room_state);
}

/// `R`: remove the first instance of `item` on the floor of `room`.
fn remove_item_from_room(world: &mut World, room: &mib_stdlib::prelude::RoomId, item: &ItemId) {
    let Some(room_entity) = world
        .get_resource::<RoomIndex>()
        .and_then(|index| index.map.get(room).copied())
    else {
        return;
    };
    let mut found: Option<Entity> = None;
    let mut query = world.query::<(Entity, &Item, &Location)>();
    for (entity, held, location) in query.iter(world) {
        if &held.def_id == item && location.room == room_entity {
            found = Some(entity);
            break;
        }
    }
    if let Some(entity) = found {
        let _ = world.despawn(entity);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn tracker_follows_zone_order() {
        let mut zones = BTreeMap::new();
        zones.insert(
            30,
            ZoneDef {
                name: "Midgaard".into(),
                bot: 3000,
                top: 3099,
                lifespan_mins: 15,
                reset_mode: ResetMode::Always,
                commands: Vec::new(),
            },
        );
        zones.insert(
            5,
            ZoneDef {
                name: "Earlier".into(),
                bot: 1,
                top: 99,
                lifespan_mins: 10,
                reset_mode: ResetMode::Never,
                commands: Vec::new(),
            },
        );
        let tracker = ZoneTracker::from_zones(&zones);
        assert_eq!(tracker.zones.len(), 2);
        assert_eq!(
            tracker.zones[0].def.name, "Earlier",
            "byte order, not insert"
        );
        assert_eq!(tracker.zones[1].def.name, "Midgaard");
    }
}
