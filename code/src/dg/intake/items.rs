//! Intake for get, drop, give, receive, bribe, wear, remove and consume triggers.
//!
//! One exclusive system in [`DgIntake::PreCommit`] reads the frame's commands
//! and, for each item action, runs the trigger the C runs where the action
//! happens, with the C's consequence when it refuses:
//!
//! - `get`, `drop`, `put`, `equip` and `unequip` exist in the engine and the
//!   equipment module. They are carried out here ([`gate`]) on the object the
//!   engine's name matching resolves, with the engine's messages, so the object
//!   a trigger approved is the one that moves.
//! - `give` (an object or coins, to a player or an NPC), `wear`, `wield`,
//!   `hold`, `remove`, `eat`, `taste`, `drink`, `sip` and `quaff` do not exist
//!   in the engine. They are carried out here in full ([`give`], [`wear`],
//!   [`consume`]).
//!
//! Every command carried out is marked handled, so the router never answers it
//! again, and the commands of one frame run in the order they were typed. A
//! character who is not at least resting is refused as the interpreter does.

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
use crate::combat::TbaCombatant;
use crate::content::Position;
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

/// The typed command behind a line, if the engine's parser reads one.
fn typed_command(input: &PlayerInput) -> Option<Command> {
    match input {
        PlayerInput::Command(command) => Some(command.clone()),
        PlayerInput::RawLine(line) => parse_line(line).ok(),
    }
}

/// Whether the equipment module would act on this command.
fn is_equipment_command(input: &PlayerInput) -> bool {
    matches!(
        typed_command(input),
        Some(Command::Equip { .. } | Command::Unequip { .. } | Command::Equipment)
    )
}

/// The interpreter's refusal for a character below `POS_RESTING`, which every
/// verb here requires (`command_interpreter`).
fn position_refusal(world: &World, session: Entity) -> Option<&'static str> {
    let position = world.get::<TbaCombatant>(session)?.position;
    if position >= Position::Resting {
        return None;
    }
    Some(match position {
        Position::Dead => "Lie still; you are DEAD!!! :-(",
        Position::MortallyWounded | Position::Incapacitated => {
            "You are in a pretty bad shape, unable to do anything!"
        }
        Position::Stunned => "All you can do right now is think about the stars!",
        _ => "In your dreams, or what?",
    })
}

/// How many times the stage looks again for commands written while it ran
/// (by a script's `force`), so a script that forces forever cannot hold the
/// frame.
const MAX_ROUNDS: usize = 32;

/// `DgIntake::PreCommit`: the item actions of this frame's commands, in the
/// order they were typed. Commands a script forces while the stage runs are
/// taken in the same stage, so they pass the same triggers.
pub fn items_intake(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    if let Some(mut vetoes) = world.get_resource_mut::<EquipmentVetoes>() {
        vetoes.clear();
    }
    for _ in 0..MAX_ROUNDS {
        let messages: Vec<CommandReceived> = match world.get_resource::<Messages<CommandReceived>>()
        {
            Some(messages) => cursor.read(messages).cloned().collect(),
            None => return,
        };
        if messages.is_empty() {
            return;
        }
        for msg in messages {
            intake_command(world, &msg);
        }
    }
}

/// One command of the frame.
fn intake_command(world: &mut World, msg: &CommandReceived) {
    let Some(room) = playing_room(world, msg.session) else {
        return;
    };
    if is_handled(world, msg.id) {
        // The command gate took it over; the equipment module must not see it
        // either.
        if is_equipment_command(&msg.input)
            && let Some(mut vetoes) = world.get_resource_mut::<EquipmentVetoes>()
        {
            vetoes.veto(msg);
        }
        return;
    }
    let Some(action) = parse(&msg.input) else {
        return;
    };
    let session = msg.session;
    if let Some(refusal) = position_refusal(world, session) {
        common::tell(world, session, refusal);
        common::prompt(world, session);
    } else {
        with_host(world, |host| match &action {
            Action::Gated(command) => gate::perform(host, session, room, command),
            other => {
                run_own(host, session, other);
                common::prompt(host.world, session);
            }
        });
    }
    // Everything here was carried out, so neither the router nor the equipment
    // module may answer it again.
    mark_handled(world, msg.id);
    if matches!(
        action,
        Action::Gated(Gated::Equip { .. } | Gated::Unequip { .. })
    ) && let Some(mut vetoes) = world.get_resource_mut::<EquipmentVetoes>()
    {
        vetoes.veto(msg);
    }
}

/// Register this module's systems.
pub fn register(app: &mut App) {
    app.init_resource::<EquipmentVetoes>();
    app.add_systems(Update, items_intake.in_set(DgIntake::PreCommit));
}
