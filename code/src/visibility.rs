//! The pack's `who` filter — the one visibility surface the engine does not
//! own.
//!
//! MIB's observer-aware projection (`build_room_view`/`emit_scene`/
//! `find_look_target`, driven by the engine `Concealment`/`Perception` carriers)
//! already hides a concealed player from another actor's `look`, `Scene`, and
//! GMCP frames. `handle_who` is deliberately global and lists every player, so
//! it is the only remaining surface the pack filters.
//!
//! [`intercept_who`] runs in
//! [`CommandSet::PreRoute`](mib_stdlib::prelude::CommandSet), before
//! `route_commands`, marks a `who` handled
//! ([`HandledCommands::mark_handled`](mib_stdlib::prelude::HandledCommands)),
//! and reproduces the stdlib line with the engine [`perceive`] predicate
//! applied. Every other command passes through untouched.

use crate::dg::intake::commands::{DgClaimed, is_claimed};
use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{
    Concealment, Location, Perceiver, Perception, Player, Session, SessionState, perceive,
};

/// `CommandSet::PreRoute`: hide players the asker cannot perceive from `who`.
///
/// Uses the same engine predicate (`Concealment`/`Perception`/`perceive`) as
/// the scene projection, so `who` and `look` agree on who is concealed. `who`
/// is global, so the darkness arm is neutralised (lit room); only concealment
/// is considered.
#[allow(clippy::too_many_arguments)]
pub fn intercept_who(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    players: Query<(Entity, &Player, &Location)>,
    concealment: Query<&Concealment>,
    perception: Query<&Perception>,
    mut handled: ResMut<HandledCommands>,
    claimed: Option<Res<DgClaimed>>,
    mut output: MessageWriter<OutputEvent>,
) {
    for msg in reader.read() {
        // A `command` trigger took the line over; `who` is not answered.
        if is_claimed(claimed.as_deref(), msg.id) {
            continue;
        }
        // Login/registration input must reach the login flow untouched.
        let playing = sessions
            .get(msg.session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }

        let is_who = match &msg.input {
            PlayerInput::Command(Command::Who) => true,
            PlayerInput::RawLine(line) => matches!(parse_line(line), Ok(Command::Who)),
            _ => false,
        };
        if !is_who {
            continue;
        }

        let names = who_names(msg.session, &players, &concealment, &perception);
        output.write(OutputEvent {
            session: msg.session,
            payload: OutboundPayload::text(
                TextChannel::System,
                format!("Players online: {} ({})", names.join(", "), names.len()),
            ),
        });
        write_prompt(&mut output, msg.session);
        handled.mark_handled(msg.id);
    }
}

/// The stdlib `who` line's name list: every player the asker perceives, sorted
/// for determinism. The asker always sees themselves. Mirrors `handle_who`.
fn who_names(
    viewer: Entity,
    players: &Query<(Entity, &Player, &Location)>,
    concealment: &Query<&Concealment>,
    perception: &Query<&Perception>,
) -> Vec<String> {
    let observer = Perceiver::from_perception(perception.get(viewer).ok(), true);
    let mut names: Vec<String> = players
        .iter()
        .filter(|(entity, _, _)| {
            *entity == viewer
                || perceive(
                    &observer,
                    concealment.get(*entity).unwrap_or(&Concealment::default()),
                    false,
                )
        })
        .map(|(_, player, _)| player.name.clone())
        .collect();
    names.sort();
    names
}
