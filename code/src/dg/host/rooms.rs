//! `RoomHost`: room attributes and exits.
//!
//! A room's zone is the `tba.zones` entry whose vnum range holds the room's
//! number. Its flags are the pack's effective [`RoomTag`]s mapped to the
//! `ROOM_*` bit numbers of `structs.h`. Its exits are the engine's [`Exits`]
//! (the destination), [`Doors`] (the authored door: key and pickability) and
//! [`RoomState`] (whether the door is closed and locked now), seen through the
//! ten DG directions ([`exit_key`], [`dir_number`]).
//!
//! The weather is not modelled: the pack has no sky. The room reads take
//! `&self`, so the unmodelled ones (the weather, the `ROOM_*` bits with no tag)
//! answer their neutral value without a `BaseHost::unsupported` count; only a
//! `&mut self` method can report.

use dg_vm::Uid;
use dg_vm::fields_room::{RoomExit, RoomHost};
use mib_core::prelude::Entity;
use mib_protocol::command::ExitKey;
use mib_stdlib::content::{RoomTag, Sector};
use mib_stdlib::prelude::{Doors, Exits, Room, RoomState};

use super::EcsHost;
use crate::dg::ids::vnum_from_id;

/// `EX_ISDOOR`, `EX_CLOSED`, `EX_LOCKED`, `EX_PICKPROOF` and `EX_HIDDEN` of
/// `exit_info`.
const EX_ISDOOR: u32 = 1 << 0;
const EX_CLOSED: u32 = 1 << 1;
const EX_LOCKED: u32 = 1 << 2;
const EX_PICKPROOF: u32 = 1 << 3;
const EX_HIDDEN: u32 = 1 << 4;

/// `key` of an exit that needs none (`NOTHING` as an `ush_int`).
const NO_KEY: i32 = 65_535;

/// The engine direction at each DG direction number (`types::DIRS`).
const DIR_KEYS: [ExitKey; 10] = [
    ExitKey::North,
    ExitKey::East,
    ExitKey::South,
    ExitKey::West,
    ExitKey::Up,
    ExitKey::Down,
    ExitKey::Northwest,
    ExitKey::Northeast,
    ExitKey::Southeast,
    ExitKey::Southwest,
];

/// The engine direction of a DG direction number, or `None` outside
/// `0..NUM_OF_DIRS`.
pub fn exit_key(dir: usize) -> Option<ExitKey> {
    DIR_KEYS.get(dir).copied()
}

/// The DG direction number of an engine direction. `In` and `Out` have none.
/// The inverse of [`exit_key`], for the host traits that take an engine exit.
#[allow(dead_code)]
pub fn dir_number(key: ExitKey) -> Option<usize> {
    DIR_KEYS.iter().position(|candidate| *candidate == key)
}

/// The `SECT_*` number of a sector.
pub fn sector_number(sector: Sector) -> i32 {
    match sector {
        Sector::Inside => 0,
        Sector::City => 1,
        Sector::Field => 2,
        Sector::Forest => 3,
        Sector::Hills => 4,
        Sector::Mountain => 5,
        Sector::WaterSwim => 6,
        Sector::WaterNoSwim => 7,
        Sector::Flying => 8,
        Sector::Underwater => 9,
    }
}

/// The `ROOM_*` bit a tag stands for. `Outdoor` is the absence of
/// `ROOM_INDOORS` and is handled apart; `Safe`, `NoRecall` and `StartRoom`
/// have no bit.
fn room_bit(tag: RoomTag) -> Option<usize> {
    Some(match tag {
        RoomTag::Dark => 0,
        RoomTag::NoMob => 2,
        RoomTag::Peaceful => 4,
        RoomTag::Soundproof => 5,
        RoomTag::Tunnel => 8,
        RoomTag::Private => 9,
        RoomTag::Godroom => 10,
        RoomTag::Safe | RoomTag::NoRecall | RoomTag::Outdoor | RoomTag::StartRoom => return None,
    })
}

/// `ROOM_INDOORS`.
const ROOM_INDOORS: usize = 3;

impl EcsHost<'_> {
    /// The room entity a script id names.
    fn room_entity(&self, room: Uid) -> Option<(Entity, &Room)> {
        let entity = self.entity(room)?;
        Some((entity, self.world.get::<Room>(entity)?))
    }

    /// The `tba.zones` entry whose range holds a room: its number and name.
    fn room_zone(&self, room: Uid) -> Option<(i32, &str)> {
        let (_, room) = self.room_entity(room)?;
        let zones = &self.content()?.zones;
        let (number, zone) = zones
            .iter()
            .find(|(_, zone)| (zone.bot..=zone.top).contains(&room.num))?;
        Some((i32::try_from(*number).ok()?, zone.name.as_str()))
    }
}

impl RoomHost for EcsHost<'_> {
    /// `to_room` is `None` when the exit's destination is gone. A door is an
    /// authored [`Doors`] entry, as the engine's movement gate treats it (a
    /// [`RoomState`] door entry alone is not one); its closed and locked state
    /// is the runtime one, falling back to the authored default. The key is the
    /// vnum of the door's key item. `EX_HIDDEN` is a direction in
    /// [`Exits::hidden`].
    fn room_exit(&self, room: Uid, dir: usize) -> Option<RoomExit> {
        let (entity, _) = self.room_entity(room)?;
        let key = exit_key(dir)?;
        let destination = *self.world.get::<Exits>(entity)?.map.get(&key)?;
        let to_room = self
            .world
            .get::<Room>(destination)
            .and_then(|_| self.uid(destination));
        let door = self
            .world
            .get::<Doors>(entity)
            .and_then(|doors| doors.by_dir.get(&key));
        let state = self
            .world
            .get::<RoomState>(entity)
            .and_then(|state| state.doors.get(&key));
        let mut info = 0;
        if door.is_some() {
            info |= EX_ISDOOR;
        }
        if self
            .world
            .get::<Exits>(entity)
            .is_some_and(|exits| exits.hidden.contains(&key))
        {
            info |= EX_HIDDEN;
        }
        let closed = door.is_some()
            && state.map_or_else(
                || door.is_some_and(|door| door.default_closed),
                |s| s.closed,
            );
        let locked = door.is_some()
            && state.map_or_else(
                || door.is_some_and(|door| door.default_locked),
                |s| s.locked,
            );
        if closed {
            info |= EX_CLOSED;
        }
        if locked {
            info |= EX_LOCKED;
        }
        if door.is_some_and(|door| !door.pickable) {
            info |= EX_PICKPROOF;
        }
        let key_vnum = door
            .and_then(|door| door.key.as_ref())
            .and_then(|key| vnum_from_id(key.as_ref()))
            .unwrap_or(NO_KEY);
        Some(RoomExit {
            to_room,
            key: key_vnum,
            info,
        })
    }

    fn room_sector(&self, room: Uid) -> i32 {
        self.room_entity(room)
            .map_or(0, |(_, room)| sector_number(room.sector))
    }

    /// `ROOM_INDOORS` is a room that is not tagged outdoor (the converter maps
    /// it as the tag's absence). The bits the pack has no tag for (`DEATH`,
    /// `NOTRACK`, `NOMAGIC`, `HOUSE`, `HCRSH`, `ATRIUM`, `OLC`, `WORLDMAP`)
    /// are never set.
    fn room_flagged(&self, room: Uid, flag: usize) -> bool {
        let Some((entity, room)) = self.room_entity(room) else {
            return false;
        };
        let tags = match self.world.get::<RoomState>(entity) {
            Some(state) => room.effective_flags(state),
            None => room.tags.clone(),
        };
        if flag == ROOM_INDOORS {
            return !tags.contains(&RoomTag::Outdoor);
        }
        tags.iter().any(|tag| room_bit(*tag) == Some(flag))
    }

    fn room_zone_number(&self, room: Uid) -> i32 {
        self.room_zone(room).map_or(0, |(number, _)| number)
    }

    fn room_zone_name(&self, room: Uid) -> String {
        self.room_zone(room)
            .map(|(_, name)| name.to_owned())
            .unwrap_or_default()
    }

    /// The pack has no weather: always `SKY_CLOUDLESS`.
    fn room_sky(&self) -> i32 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dg_directions_and_engine_directions_round_trip() {
        for dir in 0..10 {
            let key = exit_key(dir);
            assert!(key.is_some(), "direction {dir}");
            assert_eq!(key.and_then(dir_number), Some(dir));
        }
        assert_eq!(exit_key(10), None);
        assert_eq!(exit_key(usize::MAX), None);
        assert_eq!(dir_number(ExitKey::In), None);
        assert_eq!(dir_number(ExitKey::Out), None);
        assert_eq!(dir_number(ExitKey::North), Some(0));
        assert_eq!(dir_number(ExitKey::Southwest), Some(9));
    }
}
