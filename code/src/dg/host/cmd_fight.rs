//! Fighting, dying, hunting and following.

use mib_core::prelude::*;
use mib_stdlib::prelude::{
    Location, Npc, NpcId, NpcKey, Player, PlayerDefeated, Room, RoomState, RoomTag,
};

use crate::combat::{NpcDefeated, TbaAttackerMemory, TbaCombatant, alignment_for, reward_for};
use crate::content::{NpcBehaviour, Position};
use crate::dg::script::DgDead;
use crate::module::TbaContent;
use crate::party::TbaFollowing;

/// What a mob set to hunt a character is after (`HUNTING(ch)`). The pack has
/// no hunting behaviour (its pursuit follows a combat target only), so this
/// only records the intent.
#[allow(dead_code)] // written here; no pack system reads it yet.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DgHunting(pub Entity);

/// `stop_fighting(ch)`: the character has no target and stands up from the
/// fighting position.
pub(super) fn stop_fighting(world: &mut World, ch: Entity) {
    if let Some(mut combatant) = world.get_mut::<TbaCombatant>(ch)
        && (combatant.target.is_some() || combatant.position == Position::Fighting)
    {
        combatant.target = None;
        if combatant.position == Position::Fighting {
            combatant.position = Position::Standing;
        }
    }
}

/// Everyone who was fighting `ch` stops (`extract_char` and `die` take the
/// character out of every fight).
pub(super) fn drop_targets(world: &mut World, ch: Entity) {
    let mut fighters = world.query::<(Entity, &TbaCombatant)>();
    let against: Vec<Entity> = fighters
        .iter(world)
        .filter(|(_, combatant)| combatant.target == Some(ch))
        .map(|(entity, _)| entity)
        .collect();
    for entity in against {
        stop_fighting(world, entity);
    }
}

/// `set_fighting(who, target)`: begin a fight if `who` is able and not already
/// in one.
fn engage(world: &mut World, who: Entity, target: Entity) {
    if let Some(mut combatant) = world.get_mut::<TbaCombatant>(who)
        && combatant.is_alive()
        && combatant.target.is_none()
        && combatant.position > Position::Stunned
    {
        combatant.target = Some(target);
        combatant.position = Position::Fighting;
    }
}

/// Whether a character has been extracted and awaits the runtime's sweep.
fn is_dead(world: &World, ch: Entity) -> bool {
    world.get::<DgDead>(ch).is_some()
}

/// `ROOM_PEACEFUL` on the room the character is in.
fn in_peaceful_room(world: &World, ch: Entity) -> bool {
    world
        .get::<Location>(ch)
        .and_then(|location| {
            let room = world.get::<Room>(location.room)?;
            let state = world.get::<RoomState>(location.room)?;
            Some(room.effective_flags(state).contains(&RoomTag::Peaceful))
        })
        .unwrap_or(false)
}

/// `MOB_NOKILL` on the victim's prototype.
fn is_nokill(world: &World, victim: Entity) -> bool {
    let (Some(key), Some(content)) = (
        world.get::<NpcKey>(victim),
        world.get_resource::<TbaContent>(),
    ) else {
        return false;
    };
    content
        .npc_stats
        .get(&key.0)
        .is_some_and(|stats| stats.behaviour.contains(&NpcBehaviour::NoKill))
}

/// `hit(ch, victim, TYPE_UNDEFINED)`: both sides take up the fight, as
/// `damage` does with `set_fighting` on each. The swing itself is resolved by
/// the pack's combat round, not here. As in the C nothing happens (and a
/// fight between the two is broken off) when they are not in the same room,
/// when the attacker is in a peaceful room, or when the victim is `NOKILL`.
pub(super) fn hit_victim(world: &mut World, ch: Entity, victim: Entity) {
    if ch == victim || is_dead(world, ch) || is_dead(world, victim) {
        return;
    }
    let here = world.get::<Location>(ch).map(|location| location.room);
    if here.is_none() || here != world.get::<Location>(victim).map(|location| location.room) {
        if world
            .get::<TbaCombatant>(ch)
            .is_some_and(|combatant| combatant.target == Some(victim))
        {
            stop_fighting(world, ch);
        }
        return;
    }
    if in_peaceful_room(world, ch) || is_nokill(world, victim) {
        return;
    }
    super::place::init_npc_combat(world, ch);
    super::place::init_npc_combat(world, victim);
    let alive = |world: &World, who: Entity| {
        world
            .get::<TbaCombatant>(who)
            .is_some_and(TbaCombatant::is_alive)
    };
    if !alive(world, ch) || !alive(world, victim) {
        return;
    }
    engage(world, ch, victim);
    engage(world, victim, ch);
    // A remembering mob remembers who attacked it.
    if world.get::<Player>(ch).is_some()
        && let Some(mut memory) = world.get_mut::<TbaAttackerMemory>(victim)
    {
        memory.0.insert(ch);
    }
}

/// `die(ch, NULL)`: a script kill, with no killer.
///
/// A player goes through the pack's player-death handling
/// (`PlayerDefeated`: corpse, death penalty, respawn); an NPC is announced
/// with `NpcDefeated` (the victim stands in as its own killer, so nobody is
/// paid) and extracted. Either way the character is marked dead until the
/// runtime sweeps it.
pub(super) fn die(world: &mut World, ch: Entity) {
    let is_player = world.get::<Player>(ch).is_some();
    let is_npc = world.get::<Npc>(ch).is_some();
    if !is_player && !is_npc {
        return;
    }
    if is_dead(world, ch) {
        return;
    }
    if let Some(mut combatant) = world.get_mut::<TbaCombatant>(ch) {
        combatant.hp = combatant.hp.max(0);
        combatant.position = Position::Dead;
        combatant.target = None;
    }
    if is_player {
        // A player stays in the world at the dead position until the pack's
        // death handling runs, so a second kill must not announce a second
        // death while the first is still pending.
        if !defeat_pending(world, ch)
            && let Some(mut events) = world.get_resource_mut::<Messages<PlayerDefeated>>()
        {
            events.write(PlayerDefeated { player: ch });
        }
    } else {
        let (npc, experience, gold, alignment) = match world.get::<NpcKey>(ch) {
            Some(key) => {
                let content = world.get_resource::<TbaContent>();
                let (experience, gold) = content.map_or((0, 0), |content| reward_for(content, key));
                let alignment = content.map_or(0, |content| alignment_for(content, key));
                (key.0.clone(), experience, gold, alignment)
            }
            None => (NpcId::from(""), 0, 0, 0),
        };
        if let Some(mut events) = world.get_resource_mut::<Messages<NpcDefeated>>() {
            events.write(NpcDefeated {
                victim: ch,
                killer: ch,
                npc,
                experience,
                gold,
                alignment,
            });
        }
    }
    super::place::extract_char(world, ch);
}

/// Whether a `PlayerDefeated` for `player` is still waiting in the message
/// buffers.
fn defeat_pending(world: &World, player: Entity) -> bool {
    world
        .get_resource::<Messages<PlayerDefeated>>()
        .is_some_and(|events| {
            events
                .get_cursor()
                .read(events)
                .any(|event| event.player == player)
        })
}

/// `HUNTING(ch) = victim`. Returns whether the hunt was recorded.
pub(super) fn set_hunting(world: &mut World, ch: Entity, victim: Entity) -> bool {
    if world.get::<Npc>(ch).is_some()
        && super::place::is_character(world, victim)
        && !is_dead(world, ch)
        && !is_dead(world, victim)
    {
        world.entity_mut(ch).insert(DgHunting(victim));
        return true;
    }
    false
}

/// `die_follower`: everyone who followed `ch` stops.
pub(super) fn drop_followers(world: &mut World, ch: Entity) {
    let mut followers = world.query::<(Entity, &TbaFollowing)>();
    let behind: Vec<Entity> = followers
        .iter(world)
        .filter(|(_, following)| following.leader == ch)
        .map(|(entity, _)| entity)
        .collect();
    for entity in behind {
        unfollow(world, entity);
    }
}

/// The "stop following someone else first" block of `do_mfollow`.
pub(super) fn unfollow(world: &mut World, ch: Entity) {
    if let Ok(mut entity) = world.get_entity_mut(ch) {
        entity.remove::<TbaFollowing>();
    }
}

/// The tail of `do_mfollow`: `ch` follows `leader`. The pack drags a
/// follower only while it stands where its leader just was, so the anchor is
/// the leader's current room.
pub(super) fn follow(world: &mut World, ch: Entity, leader: Entity) {
    if ch == leader
        || !super::place::is_character(world, ch)
        || is_dead(world, ch)
        || is_dead(world, leader)
    {
        return;
    }
    let room = world
        .get::<Location>(leader)
        .or_else(|| world.get::<Location>(ch))
        .map(|location| location.room);
    if let Some(last_room) = room {
        world
            .entity_mut(ch)
            .insert(TbaFollowing { leader, last_room });
    }
}
