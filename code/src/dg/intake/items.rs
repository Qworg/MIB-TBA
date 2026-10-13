//! Intake for get, drop, give, receive, bribe, wear, remove and consume triggers.
//!
//! One exclusive system in [`DgIntake::PreCommit`] reads the frame's commands
//! and, for each item action, runs the trigger the C runs where the action
//! happens, with the C's consequence when it refuses:
//!
//! - `get`, `drop`, `put`, `equip`, `unequip` are done by the engine and the
//!   equipment module after the router. The object the command will act on is
//!   resolved here as the engine resolves it ([`gate`]), the trigger runs, and a
//!   refusal marks the command handled.
//! - `give` (an object or coins, to a player or an NPC), `wear`, `wield`,
//!   `hold`, `remove`, `eat`, `taste`, `drink`, `sip` and `quaff` do not exist in
//!   the engine. They are carried out here in full ([`give`], [`wear`],
//!   [`consume`]) and the command is marked handled, so the router never
//!   answers them.

#[path = "items_common.rs"]
pub mod common;
#[path = "../../consume.rs"]
pub mod consume;
#[path = "items_gate.rs"]
pub mod gate;
#[path = "items_give.rs"]
pub mod give;
#[path = "../../wear.rs"]
pub mod wear;

use bevy::ecs::message::MessageCursor;
use mib_core::prelude::*;
use mib_stdlib::prelude::{Location, Player, Session, SessionState};

use super::DgIntake;
use crate::dg::host::EcsHost;
use crate::dg::with_host;
use crate::equipment::EquipmentVetoes;
use gate::Gated;

/// What a command asks of the item intake.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// An action the engine or the equipment module carries out after the
    /// router; only its trigger runs here.
    Gated(Gated),
    /// `give` with the C's grammar.
    Give(String),
    /// The engine's structured `give`.
    GiveTyped {
        item: String,
        to: String,
    },
    Wear(String),
    Wield(String),
    Hold(String),
    Remove(String),
    Eat(String),
    Taste(String),
    Drink(String),
    Sip(String),
    Quaff(String),
}

/// Read what a command asks of the item intake, if anything.
fn parse(input: &PlayerInput) -> Option<Action> {
    let line = match input {
        PlayerInput::RawLine(line) => line.clone(),
        PlayerInput::Command(Command::Custom { verb, args }) => {
            format!("{verb} {}", args.join(" "))
        }
        PlayerInput::Command(command) => return from_command(command.clone()),
    };
    let line = line.trim();
    let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim().to_owned();
    match verb.to_lowercase().as_str() {
        "give" => Some(Action::Give(rest)),
        "wear" => Some(Action::Wear(rest)),
        "wield" => Some(Action::Wield(rest)),
        "hold" | "grab" => Some(Action::Hold(rest)),
        "remove" => Some(Action::Remove(rest)),
        "eat" => Some(Action::Eat(rest)),
        "taste" => Some(Action::Taste(rest)),
        "drink" => Some(Action::Drink(rest)),
        "sip" => Some(Action::Sip(rest)),
        "quaff" => Some(Action::Quaff(rest)),
        _ => from_command(parse_line(line).ok()?),
    }
}

/// The engine's typed commands the intake cares about.
fn from_command(command: Command) -> Option<Action> {
    Some(match command {
        Command::Get { item, from } => Action::Gated(Gated::Get { item, from }),
        Command::Drop { item } => Action::Gated(Gated::Drop { item }),
        Command::Put { item, into } => Action::Gated(Gated::Put { item, into }),
        Command::Equip { item } => Action::Gated(Gated::Equip { item }),
        Command::Unequip { slot } => Action::Gated(Gated::Unequip { slot }),
        Command::Give { item, to } => Action::GiveTyped { item, to },
        _ => return None,
    })
}

/// The room of a player who is in the game.
fn playing_room(world: &World, session: Entity) -> Option<Entity> {
    world.get::<Player>(session)?;
    let state = &world.get::<Session>(session)?.state;
    if *state != SessionState::Playing {
        return None;
    }
    world.get::<Location>(session).map(|location| location.room)
}

/// Whether an earlier stage already took the command over.
fn is_handled(world: &World, id: CommandId) -> bool {
    world
        .get_resource::<HandledCommands>()
        .is_some_and(|handled| handled.is_handled(id))
}

fn mark_handled(world: &mut World, id: CommandId) {
    if let Some(mut handled) = world.get_resource_mut::<HandledCommands>() {
        handled.mark_handled(id);
    }
}

/// Carry out an action that needs nothing from the engine.
fn run_own(host: &mut EcsHost<'_>, ch: Entity, action: &Action) {
    match action {
        Action::Give(args) => give::do_give(host, ch, args),
        Action::GiveTyped { item, to } => give::do_give_typed(host, ch, item, to),
        Action::Wear(args) => wear::do_wear(host, ch, args),
        Action::Wield(args) => wear::do_wield(host, ch, args),
        Action::Hold(args) => wear::do_hold(host, ch, args),
        Action::Remove(args) => wear::do_remove(host, ch, args),
        Action::Eat(args) => consume::do_eat(host, ch, args, false),
        Action::Taste(args) => consume::do_eat(host, ch, args, true),
        Action::Drink(args) => consume::do_drink(host, ch, args, false),
        Action::Sip(args) => consume::do_drink(host, ch, args, true),
        Action::Quaff(args) => consume::do_quaff(host, ch, args),
        Action::Gated(_) => {}
    }
}

/// `DgIntake::PreCommit`: the item triggers of this frame's commands.
pub fn items_intake(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    let messages: Vec<CommandReceived> = match world.get_resource::<Messages<CommandReceived>>() {
        Some(messages) => cursor.read(messages).cloned().collect(),
        None => return,
    };
    if let Some(mut vetoes) = world.get_resource_mut::<EquipmentVetoes>() {
        vetoes.clear();
    }
    for msg in messages {
        let Some(room) = playing_room(world, msg.session) else {
            continue;
        };
        let Some(action) = parse(&msg.input) else {
            continue;
        };
        if is_handled(world, msg.id) {
            // The command gate took it over; the equipment module must not
            // see it either.
            if let Action::Gated(Gated::Equip { .. } | Gated::Unequip { .. }) = action
                && let Some(mut vetoes) = world.get_resource_mut::<EquipmentVetoes>()
            {
                vetoes.veto(&msg);
            }
            continue;
        }
        let session = msg.session;
        let refused = with_host(world, |host| match &action {
            Action::Gated(command) => {
                let refused = gate::refuses(host, session, room, command);
                if refused {
                    common::prompt(host.world, session);
                }
                refused
            }
            other => {
                run_own(host, session, other);
                common::prompt(host.world, session);
                true
            }
        });
        if refused {
            mark_handled(world, msg.id);
            if let Action::Gated(Gated::Equip { .. } | Gated::Unequip { .. }) = action
                && let Some(mut vetoes) = world.get_resource_mut::<EquipmentVetoes>()
            {
                vetoes.veto(&msg);
            }
        }
    }
}

/// Register this module's systems.
pub fn register(app: &mut App) {
    app.init_resource::<EquipmentVetoes>();
    app.add_systems(Update, items_intake.in_set(DgIntake::PreCommit));
}
