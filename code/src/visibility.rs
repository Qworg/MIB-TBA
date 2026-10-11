//! Stealth visibility: a `CommandSet::PreRoute` interceptor that keeps
//! `TbaHidden`/`TbaSneaking` players out of another actor's `look`/`who`,
//! without reimplementing the stdlib renderers in the common case.
//!
//! The stdlib router owns the fixed `look`/`who` commands: `route.rs` handles
//! `Command::Look`/`Command::Who` before the pack-verb registry, so a pack verb
//! cannot shadow them. MIB's command-interceptor pipeline is the engine seam
//! that can. [`intercept_visibility_commands`] runs in
//! [`CommandSet::PreRoute`](mib_stdlib::prelude::CommandSet), before
//! `route_commands`, and marks a command handled
//! ([`HandledCommands::mark_handled`]) **only** when stealth changes the
//! answer:
//!
//! - `who` is always intercepted (its list is the only thing stealth changes),
//!   and reproduces the stdlib line with the stealth filter applied.
//! - bare `look` is intercepted **only** when a stealthed player shares the
//!   actor's room; otherwise it passes through untouched, so the stdlib scene
//!   is byte-for-byte the normal one.
//! - targeted `look <t>` is intercepted **only** when [`find_look_target`]
//!   resolves the selector to a player who is stealthed for the asker.
//!
//! Everything else falls through to the stdlib router. When the interceptor
//! renders a scene it mirrors [`mib_stdlib::world::emit_scene`] (Scene, Media,
//! and GMCP), which is engine code the pack cannot filter; the copy below is
//! annotated so it can be re-synced if the engine renderer changes.
//!
//! **Residual (documented in `docs/runbook.md` §8):** the *arrival* scene a
//! mover receives is emitted inside `execute_move` → `emit_scene`, so a hidden
//! occupant still appears when someone walks into the room. Covering it would
//! require duplicating `execute_move`'s location/online/quest bookkeeping, which
//! this pack deliberately does not do.

use std::collections::BTreeSet;

use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{
    Discovered, Exits, ItemQuery, Location, LookTarget, Npc, Player, Room, RoomId, Session,
    SessionState, build_room_view, canonical_exits, check_room_visibility, find_look_target,
    visible_exits,
};

use crate::combat::TbaHidden;
use crate::combat::TbaSneaking;

/// The stealth affects that hide a player: Circle's `AFF_HIDE`/`AFF_SNEAK`.
type Stealthed<'w, 's> = Query<'w, 's, (), Or<(With<TbaHidden>, With<TbaSneaking>)>>;

/// `CommandSet::PreRoute`: hide stealthed players from `look`/`who` by marking
/// only the commands whose answer stealth changes.
///
/// The decision table (normalized input — a typed [`Command`] or a raw line
/// parsed with [`parse_line`]):
///
/// | input | condition | result |
/// |---|---|---|
/// | `who` | always (playing session) | marked handled; stdlib-format line, stealthed players dropped except the asker |
/// | `look` (no target) | a stealthed player is in the actor's room | marked handled; filtered scene (dark path reused from `check_room_visibility`) |
/// | `look` (no target) | no stealthed player in the room | **pass-through** (stdlib scene) |
/// | `look <t>` | `find_look_target` resolves to a stealthed player other than the asker | marked handled; "You don't see them here." |
/// | `look <t>` | otherwise (npc/item/extra-desc/direction/visible player/dark) | **pass-through** (stdlib) |
/// | anything else | — | **pass-through** |
#[allow(clippy::too_many_arguments)]
pub fn intercept_visibility_commands(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    players: Query<(Entity, &Player, &Location)>,
    view_players: Query<(&Player, &Location)>,
    stealthed: Stealthed<'_, '_>,
    rooms: Query<(&Room, &Exits)>,
    npcs: Query<(Entity, &Npc, &Location)>,
    items: ItemQuery<'_, '_>,
    connections: Query<&Connection>,
    discovered: Query<&Discovered>,
    mut handled: ResMut<HandledCommands>,
    mut output: MessageWriter<OutputEvent>,
) {
    for msg in reader.read() {
        // Login/registration input must reach the login flow untouched.
        let playing = sessions
            .get(msg.session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }

        let command = match &msg.input {
            PlayerInput::Command(command) => command.clone(),
            PlayerInput::RawLine(line) => match parse_line(line) {
                Ok(command) => command,
                Err(_) => continue,
            },
        };

        let session = msg.session;
        match command {
            Command::Who => {
                let names = who_names(session, &players, &stealthed);
                output.write(OutputEvent {
                    session,
                    payload: OutboundPayload::text(
                        TextChannel::System,
                        format!("Players online: {} ({})", names.join(", "), names.len()),
                    ),
                });
                write_prompt(&mut output, session);
                handled.mark_handled(msg.id);
            }
            Command::Look { target: None } => {
                let Ok((_, _, location)) = players.get(session) else {
                    continue;
                };
                let room = location.room;
                if !has_stealthed_occupant(room, session, &players, &stealthed) {
                    // Nothing to hide: let the stdlib renderer do its job.
                    continue;
                }
                let discovered_now = discovered_for(session, room, &rooms, &discovered);
                emit_filtered_scene(
                    &mut output,
                    &connections,
                    session,
                    room,
                    &rooms,
                    &players,
                    &view_players,
                    &stealthed,
                    &npcs,
                    &items,
                    &discovered_now,
                );
                write_prompt(&mut output, session);
                handled.mark_handled(msg.id);
            }
            Command::Look {
                target: Some(target),
            } => {
                let Ok((_, _, location)) = players.get(session) else {
                    continue;
                };
                let room = location.room;
                // A pitch-black room is the stdlib's to report; it reveals no
                // occupant either way.
                if !check_room_visibility(session, room, &rooms, &items) {
                    continue;
                }
                let selector = TargetSelector::parse(&target);
                if matches!(
                    find_look_target(
                        session,
                        room,
                        &selector,
                        &rooms,
                        &view_players,
                        &npcs,
                        &items
                    ),
                    Some(LookTarget::Player(_))
                ) && stealthed_match(session, room, &selector, &players, &stealthed).is_some()
                {
                    output.write(OutputEvent {
                        session,
                        payload: OutboundPayload::error(
                            ErrorKind::NoSuchTarget,
                            "You don't see them here.",
                        ),
                    });
                    write_prompt(&mut output, session);
                    handled.mark_handled(msg.id);
                }
            }
            _ => {}
        }
    }
}

/// The stdlib `who` line's name list: every player except stealthed players
/// other than the asker, sorted for determinism (mirrors `handle_who`).
fn who_names(
    viewer: Entity,
    players: &Query<(Entity, &Player, &Location)>,
    stealthed: &Stealthed<'_, '_>,
) -> Vec<String> {
    let mut names: Vec<String> = players
        .iter()
        .filter(|(entity, _, _)| *entity == viewer || !stealthed.contains(*entity))
        .map(|(_, player, _)| player.name.clone())
        .collect();
    names.sort();
    names
}

/// Is there a stealthed player in `room` other than `viewer`?
fn has_stealthed_occupant(
    room: Entity,
    viewer: Entity,
    players: &Query<(Entity, &Player, &Location)>,
    stealthed: &Stealthed<'_, '_>,
) -> bool {
    players.iter().any(|(entity, _, location)| {
        location.room == room && entity != viewer && stealthed.contains(entity)
    })
}

/// The stealthed player the selector resolves to, if any. Mirrors
/// `find_look_target`'s per-category ordinal counting so the entity is the same
/// one that function matched (callers must already know it resolved to a
/// `Player`, i.e. no extra-description/npc/item won the ordinal).
fn stealthed_match(
    viewer: Entity,
    room: Entity,
    selector: &TargetSelector,
    players: &Query<(Entity, &Player, &Location)>,
    stealthed: &Stealthed<'_, '_>,
) -> Option<Entity> {
    let mut count = 0usize;
    for (entity, player, location) in players.iter() {
        if location.room == room && selector.matches(&player.name) {
            count += 1;
            if count == selector.ordinal {
                return (entity != viewer && stealthed.contains(entity)).then_some(entity);
            }
        }
    }
    None
}

/// The exits `session` has discovered in `room` (empty when the session has no
/// [`Discovered`] component), the same set `handle_look` builds before calling
/// `emit_scene`.
fn discovered_for(
    session: Entity,
    room: Entity,
    rooms: &Query<(&Room, &Exits)>,
    discovered: &Query<&Discovered>,
) -> BTreeSet<ExitKey> {
    rooms
        .get(room)
        .ok()
        .map(|(room, _)| RoomId(room.key.clone()))
        .and_then(|room_key| {
            discovered
                .get(session)
                .ok()
                .map(|discovered| discovered.exits_in(&room_key))
        })
        .unwrap_or_default()
}

/// Render the room to one session with stealthed players filtered out.
///
/// The Scene/Media/GMCP emission **mirrors
/// [`mib_stdlib::world::emit_scene`]** (engine code the pack cannot filter);
/// only the occupant and GMCP player lists differ, dropping every stealthed
/// player except the viewer. The pitch-black branch is the same view the engine
/// builds. Re-sync this block if the engine renderer changes.
#[allow(clippy::too_many_arguments)]
fn emit_filtered_scene(
    output: &mut MessageWriter<OutputEvent>,
    connections: &Query<&Connection>,
    session: Entity,
    room_entity: Entity,
    rooms: &Query<(&Room, &Exits)>,
    players: &Query<(Entity, &Player, &Location)>,
    view_players: &Query<(&Player, &Location)>,
    stealthed: &Stealthed<'_, '_>,
    npcs: &Query<(Entity, &Npc, &Location)>,
    items: &ItemQuery<'_, '_>,
    discovered: &BTreeSet<ExitKey>,
) {
    let Ok((room, exits)) = rooms.get(room_entity) else {
        tracing::error!(room = ?room_entity, "emit_filtered_scene for missing room");
        return;
    };

    if !check_room_visibility(session, room_entity, rooms, items) {
        // Mirrors `emit_scene`'s dark view: no occupants, exits, or items.
        output.write(OutputEvent {
            session,
            payload: OutboundPayload::Scene {
                room: RoomView {
                    key: room.key.to_string(),
                    name: "A Dark Room".to_string(),
                    description: FormattedText::plain("It is pitch black! You can't see a thing."),
                    exits: Vec::new(),
                    occupants: Vec::new(),
                    items: Vec::new(),
                },
            },
        });
        return;
    }

    let visible = visible_exits(exits, discovered);
    let mut view = build_room_view(room_entity, room, &visible, view_players, npcs, items);

    // Override the occupant list with the stealth-filtered one (players, then
    // NPCs, sorted) — `build_room_view` lists every player unfiltered.
    let mut occupants: Vec<String> = players
        .iter()
        .filter(|(entity, _, location)| {
            location.room == room_entity && (*entity == session || !stealthed.contains(*entity))
        })
        .map(|(_, player, _)| player.name.clone())
        .collect();
    occupants.extend(
        npcs.iter()
            .filter(|(_, _, location)| location.room == room_entity)
            .map(|(_, npc, _)| npc.name.clone()),
    );
    occupants.sort();
    view.occupants = occupants;

    let gmcp = connections
        .get(session)
        .map(|c| c.capabilities.gmcp_like)
        .unwrap_or(false);
    let exit_map: serde_json::Map<String, serde_json::Value> = canonical_exits(&visible)
        .iter()
        .filter_map(|dir| {
            let target = exits.map.get(dir)?;
            let destination = rooms.get(*target).ok()?.0;
            Some((
                dir.as_str().to_owned(),
                serde_json::Value::String(destination.name.clone()),
            ))
        })
        .collect();
    let player_names: Vec<&str> = players
        .iter()
        .filter(|(entity, _, location)| {
            location.room == room_entity && (*entity == session || !stealthed.contains(*entity))
        })
        .map(|(_, player, _)| player.name.as_str())
        .collect();
    let npc_names: Vec<&str> = npcs
        .iter()
        .filter(|(_, _, location)| location.room == room_entity)
        .map(|(_, npc, _)| npc.name.as_str())
        .collect();

    output.write(OutputEvent {
        session,
        payload: OutboundPayload::Scene { room: view.clone() },
    });
    if !room.media.is_empty() {
        output.write(OutputEvent {
            session,
            payload: OutboundPayload::Media {
                media: room.media.clone(),
            },
        });
    }
    if gmcp {
        output.write(OutputEvent {
            session,
            payload: OutboundPayload::GmcpLike {
                namespace: "Room.Info".to_owned(),
                body: serde_json::json!({
                    "key": room.key.as_str(),
                    "num": room.num,
                    "name": room.name,
                    "area": room.area.as_str(),
                    "desc": room.description.render_plain(),
                    "exits": view.exits.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
                    "exit_map": exit_map,
                    "coord": { "x": room.coord.0, "y": room.coord.1, "z": room.coord.2 },
                    "npcs": npc_names,
                    "items": view.items,
                }),
            },
        });
        output.write(OutputEvent {
            session,
            payload: OutboundPayload::GmcpLike {
                namespace: "Room.Players".to_owned(),
                body: serde_json::Value::Array(
                    player_names
                        .iter()
                        .map(|name| serde_json::json!({ "name": name }))
                        .collect(),
                ),
            },
        });
    }
}
