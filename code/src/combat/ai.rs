//! Diku NPC behaviour: auto-engage aggro, memory, helper, and light pursuit
//! (TBA-E05 / TBA-E08).
//!
//! The aggro rules are the doc §4.2/§5 set, read from
//! [`TbaNpcStats::behaviour`](crate::content::TbaNpcStats::behaviour):
//!
//! - `Aggressive` attacks any visible player;
//! - `AggrEvil` / `AggrGood` / `AggrNeutral` gate on the **player's**
//!   [`TbaAlignment`] (Circle's `IS_EVIL`/`IS_GOOD` thresholds);
//! - `Memory` re-engages anyone recorded in its [`TbaAttackerMemory`];
//! - `Helper` joins an ally NPC already fighting in the room;
//! - `Sentinel` never leaves the room it spawned in and `StayZone` never
//!   leaves its zone, both enforced by [`pursuit_system`].
//!
//! All four systems share `RoomOccupants` and run on `FixedUpdate`, after the
//! stdlib spatial index is maintained. Player commands (the explicit
//! `attack`/`flee` seam) live in [`super::commands`].

use std::collections::HashSet;

use mib_core::prelude::*;
use mib_protocol::prelude::{OutboundPayload, TextChannel};
use mib_stdlib::prelude::{Exits, Location, Npc, NpcKey, Player, Room, RoomOccupants};

use crate::content::{NpcBehaviour, Position};
use crate::module::TbaContent;

use super::model::{EVIL_ALIGNMENT, GOOD_ALIGNMENT, TbaAlignment, TbaCombatant};

/// A Diku `MEMORY` mob's list of remembered attackers (doc §5).
///
/// Runtime-only: an `Entity` is not portable across a restart, so this is
/// never persisted. [`super::commands::handle_combat_commands`] records a
/// player here when they attack the mob; [`auto_engage_system`] then
/// re-engages any remembered player on sight.
#[derive(Debug, Default, Component)]
pub struct TbaAttackerMemory(pub HashSet<Entity>);

/// Where an NPC spawned, for `Sentinel`/`StayZone` pursuit limits.
///
/// `zone` is the `[bot, top]` room-number range the home room falls in, if the
/// loaded `tba.zones` content declares one. It is a cheap, static snapshot:
/// the later zone-reset pass never relocates a mob's spawn point.
#[derive(Debug, Clone, Copy, Component)]
pub struct TbaSpawnHome {
    /// The room entity the NPC was initialised in.
    pub room: Entity,
    /// The `[bot, top]` zone bounds of `room`, when one matches.
    pub zone: Option<(u32, u32)>,
}

/// Does `behaviour` say this mob hates a player with `player_alignment`?
///
/// A mob with more than one aggro flag attacks if any of them matches (Circle
/// ORs the flags). A missing player alignment is neutral.
pub fn engages(behaviour: &[NpcBehaviour], player_alignment: Option<i32>) -> bool {
    let alignment = player_alignment.unwrap_or(0);
    behaviour.iter().any(|flag| match flag {
        NpcBehaviour::Aggressive => true,
        NpcBehaviour::AggrEvil => alignment < EVIL_ALIGNMENT,
        NpcBehaviour::AggrGood => alignment > GOOD_ALIGNMENT,
        NpcBehaviour::AggrNeutral => (EVIL_ALIGNMENT..=GOOD_ALIGNMENT).contains(&alignment),
        _ => false,
    })
}

/// Is `flag` present in a mob's behaviour list?
fn has(behaviour: &[NpcBehaviour], flag: NpcBehaviour) -> bool {
    behaviour.contains(&flag)
}

/// `FixedUpdate`: hostile mobs in a room with a player engage it (doc §4.2).
///
/// For every room containing a live player, each peaceful (`Standing`, no
/// target) NPC there engages the first player it either hates by alignment or
/// remembers. One NPC never targets two players in one pass: its `target`
/// becomes `Some` and the later rooms skip it.
#[allow(clippy::type_complexity)]
pub fn auto_engage_system(
    occupants: Res<RoomOccupants>,
    mut queries: ParamSet<(
        Query<
            (
                Entity,
                &Player,
                &Location,
                Option<&TbaCombatant>,
                Option<&TbaAlignment>,
            ),
            With<Player>,
        >,
        Query<
            (
                Entity,
                &mut TbaCombatant,
                Option<&Npc>,
                Option<&TbaAttackerMemory>,
                Option<&NpcKey>,
            ),
            Without<Player>,
        >,
    )>,
    content: Option<Res<TbaContent>>,
    mut output: MessageWriter<OutputEvent>,
) {
    // Snapshot the players first: the mob query needs a conflicting `&mut
    // TbaCombatant` view, and the room broadcast below needs the names.
    let players: Vec<(Entity, String, Entity, Option<i32>, bool)> = queries
        .p0()
        .iter()
        .map(|(entity, player, location, combatant, alignment)| {
            (
                entity,
                player.name.clone(),
                location.room,
                alignment.map(|a| a.0),
                combatant.is_none_or(TbaCombatant::is_alive),
            )
        })
        .collect();

    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    // Bind the mutable mob view once: a `queries.p1()` temporary cannot
    // outlive the `get_mut` borrow it guards.
    let mut mobs = queries.p1();
    let mut engaged: Vec<(Entity, String, String, Entity)> = Vec::new();
    for (player, player_name, room, alignment, alive) in &players {
        if !*alive {
            continue;
        }
        for &occupant in occupants.occupants(*room) {
            if occupant == *player {
                continue;
            }
            let Ok((_mob, mut combatant, npc, memory, key)) = mobs.get_mut(occupant) else {
                continue;
            };
            if combatant.target.is_some()
                || combatant.position != Position::Standing
                || !combatant.is_alive()
            {
                continue;
            }
            let behaviour = key
                .and_then(|key| content.npc_stats.get(&key.0))
                .map(|stats| stats.behaviour.as_slice())
                .unwrap_or(&[]);
            let remembers = memory.is_some_and(|memory| memory.0.contains(player))
                && has(behaviour, NpcBehaviour::Memory);
            if remembers || engages(behaviour, *alignment) {
                combatant.target = Some(*player);
                combatant.position = Position::Fighting;
                engaged.push((
                    *player,
                    npc.map(|npc| npc.name.clone())
                        .unwrap_or_else(|| "Someone".to_owned()),
                    player_name.clone(),
                    *room,
                ));
            }
        }
    }

    for (player, mob_name, player_name, room) in engaged {
        output.write(OutputEvent {
            session: player,
            payload: OutboundPayload::text(TextChannel::Room, format!("{mob_name} attacks you!")),
        });
        for (other, _, other_room, _, _) in &players {
            if *other_room == room && *other != player {
                output.write(OutputEvent {
                    session: *other,
                    payload: OutboundPayload::text(
                        TextChannel::Room,
                        format!("{mob_name} attacks {player_name}!"),
                    ),
                });
            }
        }
    }
}

/// `FixedUpdate`: a `Helper` mob joins an ally already fighting in its room.
///
/// All NPCs are on the same side, so a peaceful helper copies the target of
/// any combatant NPC in the room. The two-phase `ParamSet` avoids aliasing the
/// read and write views of `TbaCombatant`.
#[allow(clippy::type_complexity)]
pub fn helper_system(
    occupants: Res<RoomOccupants>,
    mobs: Query<(Entity, &Location, Option<&NpcKey>), Without<Player>>,
    mut combatants: ParamSet<(Query<&TbaCombatant>, Query<&mut TbaCombatant>)>,
    content: Option<Res<TbaContent>>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    let mut intents: Vec<(Entity, Entity)> = Vec::new();
    {
        let read = combatants.p0();
        for (entity, location, key) in &mobs {
            let behaviour = key
                .and_then(|key| content.npc_stats.get(&key.0))
                .map(|stats| stats.behaviour.as_slice())
                .unwrap_or(&[]);
            if !has(behaviour, NpcBehaviour::Helper) {
                continue;
            }
            let Ok(own) = read.get(entity) else { continue };
            if own.target.is_some() || own.position != Position::Standing || !own.is_alive() {
                continue;
            }
            let ally_target = occupants
                .occupants(location.room)
                .iter()
                .find_map(|&other| {
                    if other == entity || !mobs.contains(other) {
                        return None;
                    }
                    let fighter = read.get(other).ok()?;
                    (fighter.is_alive() && fighter.target.is_some()).then_some(fighter.target)
                });
            if let Some(Some(target)) = ally_target {
                intents.push((entity, target));
            }
        }
    }

    let mut write = combatants.p1();
    for (helper, target) in intents {
        if let Ok(mut combatant) = write.get_mut(helper) {
            combatant.target = Some(target);
            combatant.position = Position::Fighting;
        }
    }
}

/// `FixedUpdate`: a mob whose target left the room either follows one room or
/// gives up.
///
/// There is deliberately **no pathfinder**: if the target's room is one of the
/// mob's exits, the mob moves there; otherwise it drops the target. `Sentinel`
/// never leaves its spawn room and `StayZone` never leaves its zone, so both
/// drop the target instead of pursuing. Multi-room pathing and door-blocked
/// pursuit are deferred to a later phase.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn pursuit_system(
    mut npcs: Query<
        (
            Entity,
            &mut TbaCombatant,
            &Location,
            Option<&Npc>,
            Option<&NpcKey>,
            Option<&TbaSpawnHome>,
        ),
        Without<Player>,
    >,
    locations: Query<&Location>,
    exits: Query<&Exits>,
    rooms: Query<&Room>,
    content: Option<Res<TbaContent>>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    for (entity, mut combatant, location, npc, key, home) in &mut npcs {
        let Some(target) = combatant.target else {
            continue;
        };
        let Ok(target_location) = locations.get(target) else {
            // The target entity is gone; stop fighting a ghost.
            combatant.target = None;
            continue;
        };
        if target_location.room == location.room {
            continue;
        }

        let destination = exits.get(location.room).ok().and_then(|exits| {
            exits
                .map
                .values()
                .copied()
                .find(|&room| room == target_location.room)
        });
        let behaviour = key
            .and_then(|key| content.npc_stats.get(&key.0))
            .map(|stats| stats.behaviour.as_slice())
            .unwrap_or(&[]);
        let sentinel = has(behaviour, NpcBehaviour::Sentinel);
        let stay_zone = has(behaviour, NpcBehaviour::StayZone);
        let allowed = !sentinel
            && destination.is_some_and(|destination| {
                if !stay_zone {
                    return true;
                }
                match (home.and_then(|home| home.zone), rooms.get(destination)) {
                    (Some((bot, top)), Ok(room)) => room.num >= bot && room.num <= top,
                    // No zone on record: conservatively never leave home.
                    (None, _) => home.is_some_and(|home| home.room == destination),
                    _ => false,
                }
            });

        if allowed {
            if let Some(destination) = destination {
                let mob_name = npc
                    .map(|npc| npc.name.clone())
                    .unwrap_or_else(|| "Someone".to_owned());
                commands
                    .entity(entity)
                    .insert(Location { room: destination });
                // Tell the prey (and its room) that it is being followed.
                output.write(OutputEvent {
                    session: target,
                    payload: OutboundPayload::text(
                        TextChannel::Room,
                        format!("{mob_name} follows you!"),
                    ),
                });
            }
        } else {
            combatant.target = None;
            combatant.position = Position::Standing;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn alignment_gates_aggro_flags() {
        assert!(engages(&[NpcBehaviour::Aggressive], Some(0)));
        assert!(!engages(&[NpcBehaviour::Sentinel], Some(0)));
        assert!(engages(&[NpcBehaviour::AggrEvil], Some(-400)));
        assert!(!engages(&[NpcBehaviour::AggrEvil], Some(400)));
        assert!(engages(&[NpcBehaviour::AggrGood], Some(400)));
        assert!(!engages(&[NpcBehaviour::AggrGood], Some(-400)));
        assert!(engages(&[NpcBehaviour::AggrNeutral], Some(0)));
        assert!(!engages(&[NpcBehaviour::AggrNeutral], Some(400)));
        assert!(engages(
            &[NpcBehaviour::AggrGood, NpcBehaviour::AggrEvil],
            Some(-400)
        ));
    }
}
