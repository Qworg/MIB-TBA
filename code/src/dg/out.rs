//! Text delivery for scripts and script-driven characters.
//!
//! A script's output is ordinary room text. Only players hear it: an NPC has
//! no session, so a line addressed to one is dropped here (NPCs react to text
//! through `act` triggers, which the callers fire themselves).

use mib_core::prelude::*;
use mib_stdlib::prelude::{Location, Player, Session, SessionState};

/// The body of a line as the game sends it: without the `\r\n` the C appends
/// to every message.
fn body(text: &str) -> &str {
    text.trim_end_matches(['\r', '\n'])
}

/// Whether this entity is a player who is in the game.
pub fn is_playing(world: &World, entity: Entity) -> bool {
    world.get::<Player>(entity).is_some()
        && world
            .get::<Session>(entity)
            .is_some_and(|session| session.state == SessionState::Playing)
}

/// `send_to_char(ch, text)`: one line to one character. A line that is empty
/// once its line ending is removed is not sent.
pub fn send_to_char(world: &mut World, ch: Entity, text: &str) {
    let body = body(text);
    if body.is_empty() || !is_playing(world, ch) {
        return;
    }
    if let Some(mut output) = world.get_resource_mut::<Messages<OutputEvent>>() {
        output.write(OutputEvent {
            session: ch,
            payload: OutboundPayload::text(TextChannel::Room, body),
        });
    }
}

/// `send_to_room(room, text)`, less the listed characters: one line to every
/// player in the room.
pub fn send_to_room(world: &mut World, room: Entity, text: &str, except: &[Entity]) {
    let mut players = world.query_filtered::<(Entity, &Location), With<Player>>();
    let listeners: Vec<Entity> = players
        .iter(world)
        .filter(|(entity, location)| location.room == room && !except.contains(entity))
        .map(|(entity, _)| entity)
        .collect();
    for listener in listeners {
        send_to_char(world, listener, text);
    }
}
