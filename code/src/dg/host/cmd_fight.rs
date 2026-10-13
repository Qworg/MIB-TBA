//! Fighting, dying, hunting and following.

use mib_core::prelude::*;
use mib_stdlib::prelude::{Location, Npc, NpcId, NpcKey, Player, PlayerDefeated};

use crate::combat::{NpcDefeated, TbaAttackerMemory, TbaCombatant, alignment_for, reward_for};
use crate::content::Position;
use crate::module::TbaContent;
use crate::party::TbaFollowing;

/// What a mob set to hunt a character is after (`HUNTING(ch)`). The pack has
/// no hunting behaviour (its pursuit follows a combat target only), so this
/// only records the intent.
#[allow(dead_code)] // written here; nothing in the pack reads it yet.
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

/// `hit(ch, victim, TYPE_UNDEFINED)`: both sides take up the fight, as
/// `damage` does with `set_fighting` on each. The swing itself is resolved by
/// the pack's combat round, not here.
pub(super) fn hit_victim(world: &mut World, ch: Entity, victim: Entity) {
    if ch == victim {
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
    if world.get::<crate::dg::script::DgDead>(ch).is_some() {
        return;
    }
    if let Some(mut combatant) = world.get_mut::<TbaCombatant>(ch) {
        combatant.hp = combatant.hp.max(0);
        combatant.position = Position::Dead;
        combatant.target = None;
    }
    if is_player {
        if let Some(mut events) = world.get_resource_mut::<Messages<PlayerDefeated>>() {
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

/// `HUNTING(ch) = victim`.
pub(super) fn set_hunting(world: &mut World, ch: Entity, victim: Entity) {
    if world.get::<Npc>(ch).is_some() && super::place::is_character(world, victim) {
        world.entity_mut(ch).insert(DgHunting(victim));
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
    if ch == leader || !super::place::is_character(world, ch) {
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
