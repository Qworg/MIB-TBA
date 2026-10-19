//! Spell transport: Circle's `spell_recall`, `spell_teleport` and
//! `spell_summon` (`tbamud/src/spells.c:57-249`).
//!
//! A transport is not a blow or an affect: it moves a character between rooms
//! and must leave every derived bit of state consistent. [`warp`] is that one
//! path — it stops a fight, moves `Location`, re-anchors the mover's followers
//! so the follow system does not read the jump as a walk, updates the engine's
//! online registry, and shows a playing player the new room.

use bevy::prelude::*;
use mib_stdlib::prelude::{Location, Npc, OnlinePlayers, Player, Room, RoomTag};

use crate::combat::TbaCombatant;
use crate::content::NpcBehaviour;
use crate::content::Position;
use crate::dg::intake::movement::describe_room;
use crate::dg::out::{is_playing, send_to_char};
use crate::module::TbaContent;
use crate::party::TbaFollowing;

/// The world's recall point: the room authored with [`RoomTag::StartRoom`].
///
/// Circle keeps a `CONFIG_MORTAL_START` vnum; the pack tags the room instead,
/// so `--start-room` at conversion time decides where a recall lands.
pub fn recall_room(world: &mut World) -> Option<Entity> {
    let mut query = world.query_filtered::<(Entity, &Room), With<Room>>();
    let mut best: Option<(u32, Entity)> = None;
    for (entity, room) in query.iter(world) {
        if room.tags.contains(&RoomTag::StartRoom) && best.is_none_or(|(num, _)| room.num < num) {
            best = Some((room.num, entity));
        }
    }
    best.map(|(_, entity)| entity)
}

/// Every room a `teleport` may land in, in a stable order.
pub fn all_rooms(world: &mut World) -> Vec<Entity> {
    let mut query = world.query_filtered::<(Entity, &Room), With<Room>>();
    let mut rooms: Vec<(u32, Entity)> = query
        .iter(world)
        .map(|(entity, room)| (room.num, entity))
        .collect();
    rooms.sort_unstable();
    rooms.into_iter().map(|(_, entity)| entity).collect()
}

/// Whether `victim` carries the `MOB_*` flag that refuses `flag`.
fn has_mob_flag(world: &World, victim: Entity, flag: NpcBehaviour) -> bool {
    let Some(key) = world.get::<mib_stdlib::prelude::NpcKey>(victim) else {
        return false;
    };
    world
        .get_resource::<TbaContent>()
        .and_then(|content| content.npc_stats.get(&key.0))
        .is_some_and(|stats| stats.behaviour.contains(&flag))
}

/// Whether `victim` refuses `summon` (`MOB_NOSUMMON`, `spell_summon`).
pub fn refuses_summon(world: &World, victim: Entity) -> bool {
    has_mob_flag(world, victim, NpcBehaviour::NoSummon)
}

/// Whether `victim` refuses `charm` (`MOB_NOCHARM`, `spell_charm`).
pub fn refuses_charm(world: &World, victim: Entity) -> bool {
    has_mob_flag(world, victim, NpcBehaviour::NoCharm)
}

/// `char_to_room`: move `ch` into `room`, keeping every derived record in step.
///
/// Returns whether the move happened. A character already in `room` is left
/// alone (no duplicate arrival scene).
pub fn warp(world: &mut World, ch: Entity, room: Entity) -> bool {
    if world.get_entity(room).is_err() || world.get::<Room>(room).is_none() {
        return false;
    }
    if world
        .get::<Location>(ch)
        .is_some_and(|location| location.room == room)
    {
        return false;
    }
    // Leaving a fight is `stop_fighting`: no target, and back on its feet.
    if let Some(mut combatant) = world.get_mut::<TbaCombatant>(ch) {
        combatant.target = None;
        if combatant.position == Position::Fighting {
            combatant.position = Position::Standing;
        }
    }
    if world.get_mut::<Location>(ch).is_none() {
        // Nothing to move, or a character with no location at all.
        if world.get::<Player>(ch).is_none() && world.get::<Npc>(ch).is_none() {
            return false;
        }
    }
    world.entity_mut(ch).insert(Location { room });
    // Followers anchor to the new room so their next step is not a walk.
    {
        let mut followers = world.query::<&mut TbaFollowing>();
        for mut following in followers.iter_mut(world) {
            if following.leader == ch {
                following.last_room = room;
            }
        }
    }
    if let Some(mut online) = world.get_resource_mut::<OnlinePlayers>() {
        for record in online.map.values_mut().filter(|record| record.entity == ch) {
            record.room = room;
        }
    }
    if is_playing(world, ch) {
        let text = describe_room(world, room);
        send_to_char(world, ch, &text);
    }
    true
}
