//! The player combat command seam (TBA-E31).
//!
//! [`handle_combat_commands`] runs in [`BridgeSet::Game`](mib_core::prelude::BridgeSet)
//! after the stdlib `route_commands` router (which no-ops the genre commands):
//!
//! - `attack <target>` resolves a room-mate by name, points the actor's
//!   [`TbaCombatant::target`] at it and puts them in the `Fighting` position.
//!   Attacking an NPC records the actor in that NPC's
//!   [`TbaAttackerMemory`], so a `MEMORY` mob re-engages on sight (TBA-E08).
//! - `flee` clears the target and moves the actor through a random available
//!   exit, or reports that there is nowhere to go.
//!
//! Both emit room/actor text through [`OutputEvent`]; the round itself is
//! resolved later on the combat [`TbaRound`](super::round::TbaRound) clock.

use bevy::ecs::system::SystemParam;
use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{
    Concealment, Exits, Location, Npc, OnlinePlayers, Perception, Player, Session, SessionState,
    VisibilityPolicy, visible_to,
};

use crate::content::Position;
use crate::dg::intake::commands::{DgClaimed, is_claimed};

use super::ai::TbaAttackerMemory;
use super::model::{TbaCombatant, TbaHidden};
use super::progression::TbaRng;

/// The engine perception view a target resolution consults: the observer's
/// [`Perception`], every candidate's [`Concealment`], and the registered
/// [`VisibilityPolicy`].
///
/// Bundled as one [`SystemParam`] so `find_target_in_room` and its callers
/// (combat, the offensive skills, spells) thread the whole seam as one value
/// instead of three parameters. The field is named `hidden` on the callers for
/// continuity with the pre-seam pack state it replaces.
#[derive(SystemParam)]
pub struct StealthView<'w, 's> {
    pub concealment: Query<'w, 's, &'static Concealment>,
    pub perception: Query<'w, 's, &'static Perception>,
    pub policy: Res<'w, VisibilityPolicy>,
}

/// One line of room/actor text.
pub(crate) fn text(session: Entity, body: impl Into<String>) -> OutputEvent {
    OutputEvent {
        session,
        payload: OutboundPayload::text(TextChannel::Room, body),
    }
}

/// A command error for the acting session, followed by the prompt.
pub(crate) fn deny(
    output: &mut MessageWriter<OutputEvent>,
    session: Entity,
    message: impl Into<String>,
) {
    output.write(OutputEvent {
        session,
        payload: OutboundPayload::error(ErrorKind::NoSuchTarget, message),
    });
    write_prompt(output, session);
}

/// Find a room-mate by a case-insensitive display-name fragment (NPCs first,
/// the item core's convention). The actor is never returned. A concealed
/// candidate is skipped unless the actor's engine perception admits it
/// (Circle's `CAN_SEE`; you cannot target someone you cannot see).
#[allow(clippy::too_many_arguments)]
pub(crate) fn find_target_in_room(
    actor: Entity,
    needle: &str,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
    hidden: &StealthView<'_, '_>,
) -> Option<Entity> {
    let room = players
        .iter()
        .find(|(entity, _, _)| *entity == actor)
        .map(|(_, _, location)| location.room)?;
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    npcs.iter()
        .find(|(entity, npc, location)| {
            *entity != actor
                && location.room == room
                && npc.name.to_lowercase().contains(&needle)
                && visible_to(
                    &hidden.policy,
                    actor,
                    room,
                    *entity,
                    &npc.name,
                    false,
                    &hidden.concealment,
                    &hidden.perception,
                    false,
                    true,
                )
        })
        .map(|(entity, _, _)| entity)
        .or_else(|| {
            players
                .iter()
                .find(|(entity, player, location)| {
                    *entity != actor
                        && location.room == room
                        && player.name.to_lowercase().contains(&needle)
                        && visible_to(
                            &hidden.policy,
                            actor,
                            room,
                            *entity,
                            &player.name,
                            true,
                            &hidden.concealment,
                            &hidden.perception,
                            false,
                            true,
                        )
                })
                .map(|(entity, _, _)| entity)
        })
}

/// A target's display name, for the combat messages.
pub(crate) fn entity_name(
    entity: Entity,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
) -> String {
    if let Some((_, player, _)) = players.iter().find(|(e, _, _)| *e == entity) {
        return player.name.clone();
    }
    if let Some((_, npc, _)) = npcs.iter().find(|(e, _, _)| *e == entity) {
        return npc.name.clone();
    }
    "someone".to_owned()
}

/// Send one line to every player in `room` except `except`.
pub(crate) fn broadcast(
    players: &Query<(Entity, &Player, &Location)>,
    room: Entity,
    except: Entity,
    output: &mut MessageWriter<OutputEvent>,
    body: &str,
) {
    for (entity, _, location) in players.iter() {
        if entity != except && location.room == room {
            output.write(text(entity, body));
        }
    }
}

/// `BridgeSet::Game`: translate `attack`/`flee` into TBA combat state.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_combat_commands(
    mut reader: MessageReader<CommandReceived>,
    claimed: Option<Res<DgClaimed>>,
    sessions: Query<&Session>,
    players: Query<(Entity, &Player, &Location)>,
    npcs: Query<(Entity, &Npc, &Location)>,
    hidden: StealthView<'_, '_>,
    mut combatants: Query<&mut TbaCombatant>,
    mut memories: Query<&mut TbaAttackerMemory>,
    exits: Query<&Exits>,
    mut online: Option<ResMut<OnlinePlayers>>,
    mut rng: ResMut<TbaRng>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    for msg in reader.read() {
        let session = msg.session;
        let playing = sessions
            .get(session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        if is_claimed(claimed.as_deref(), msg.id) {
            continue;
        }
        let command = match &msg.input {
            PlayerInput::Command(command) => command.clone(),
            PlayerInput::RawLine(line) => match parse_line(line) {
                Ok(command) => command,
                Err(_) => continue,
            },
        };

        match command {
            Command::Attack { target } => {
                let Some(target_entity) =
                    find_target_in_room(session, &target, &players, &npcs, &hidden)
                else {
                    deny(
                        &mut output,
                        session,
                        format!("You don't see {target} here."),
                    );
                    continue;
                };
                if target_entity == session {
                    deny(&mut output, session, "You can't attack yourself.");
                    continue;
                }
                {
                    let Ok(mut combatant) = combatants.get_mut(session) else {
                        deny(&mut output, session, "You have no combat state.");
                        continue;
                    };
                    combatant.target = Some(target_entity);
                    combatant.position = Position::Fighting;
                }
                // Attacking breaks stealth (Circle's `appear()`,
                // `tbamud/src/fight.c:70-84`).
                commands.entity(session).remove::<TbaHidden>();
                // A MEMORY mob records who hit it (doc §5).
                if let Ok(mut memory) = memories.get_mut(target_entity) {
                    memory.0.insert(session);
                }
                let actor_name = entity_name(session, &players, &npcs);
                let target_name = entity_name(target_entity, &players, &npcs);
                output.write(text(session, format!("You attack {target_name}!")));
                if let Ok((_, _, location)) = players.get(session) {
                    broadcast(
                        &players,
                        location.room,
                        session,
                        &mut output,
                        &format!("{actor_name} attacks {target_name}!"),
                    );
                }
                write_prompt(&mut output, session);
            }
            Command::Flee => {
                let Ok((_, _, location)) = players.get(session) else {
                    continue;
                };
                let room = location.room;
                {
                    let Ok(mut combatant) = combatants.get_mut(session) else {
                        deny(&mut output, session, "You aren't fighting anyone.");
                        continue;
                    };
                    if combatant.target.is_none() && combatant.position != Position::Fighting {
                        deny(&mut output, session, "You aren't fighting anyone.");
                        continue;
                    }
                    combatant.target = None;
                    combatant.position = Position::Standing;
                }

                let mut destinations: Vec<Entity> = exits
                    .get(room)
                    .map(|exits| exits.map.values().copied().collect())
                    .unwrap_or_default();
                destinations.sort_unstable();
                if destinations.is_empty() {
                    output.write(text(session, "There's nowhere to flee to!"));
                    write_prompt(&mut output, session);
                    continue;
                }
                let index = rng.range(0, destinations.len() as i32 - 1) as usize;
                let Some(destination) = destinations.get(index).copied() else {
                    continue;
                };
                commands
                    .entity(session)
                    .insert(Location { room: destination });
                if let Some(online) = online.as_deref_mut()
                    && let Some(record) = online.map.get_mut(&msg.session_id)
                {
                    record.room = destination;
                }
                output.write(text(session, "You flee!"));
                broadcast(&players, room, session, &mut output, "Your opponent flees!");
                write_prompt(&mut output, session);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn text_uses_the_room_channel() {
        let mut world = World::new();
        let session = world.spawn_empty().id();
        let event = text(session, "hello");
        assert_eq!(event.session, session);
        assert!(matches!(
            event.payload,
            OutboundPayload::Text {
                channel: TextChannel::Room,
                ..
            }
        ));
    }

    #[derive(Resource, Default)]
    struct TargetHit(Option<Entity>);

    /// Probe `find_target_in_room` for the room's `Bram`, recording whether it
    /// resolves. Used to exercise the engine seam without an integration app.
    fn probe_bram(
        players: Query<(Entity, &Player, &Location)>,
        npcs: Query<(Entity, &Npc, &Location)>,
        hidden: super::StealthView<'_, '_>,
        mut hit: ResMut<TargetHit>,
    ) {
        let actor = players
            .iter()
            .find(|(_, player, _)| player.name == "Aria")
            .map(|(entity, _, _)| entity)
            .expect("Aria is spawned");
        hit.0 = find_target_in_room(actor, "Bram", &players, &npcs, &hidden);
    }

    #[test]
    fn concealment_filters_targeting_through_the_engine_seam() {
        // The consumer uses the engine `Concealment`/`Perception` carriers, not
        // the pack's `TbaHidden`: a concealed entity is unreachable unless the
        // observer carries a matching `Perception`.
        let mut app = App::new();
        app.init_resource::<VisibilityPolicy>();
        app.init_resource::<TargetHit>();
        app.add_systems(Update, probe_bram);
        let room = app.world_mut().spawn_empty().id();
        let actor = app
            .world_mut()
            .spawn((
                Player {
                    account_id: 1,
                    name: "Aria".to_owned(),
                },
                Location { room },
            ))
            .id();
        let target = app
            .world_mut()
            .spawn((
                Player {
                    account_id: 2,
                    name: "Bram".to_owned(),
                },
                Location { room },
            ))
            .id();
        app.world_mut().entity_mut(target).insert(Concealment {
            hidden: true,
            invisible: false,
            stealth: 5,
        });

        // No observer Perception: the concealed target is not resolvable.
        app.world_mut().run_schedule(Update);
        assert_eq!(app.world().resource::<TargetHit>().0, None);

        // A matching detect reveals it.
        app.world_mut().entity_mut(actor).insert(Perception {
            sight: 0,
            detect: 5,
        });
        app.world_mut().run_schedule(Update);
        assert_eq!(app.world().resource::<TargetHit>().0, Some(target));
    }
}
