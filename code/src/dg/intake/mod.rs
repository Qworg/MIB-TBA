//! Intake: the systems that observe gameplay and call `dg-vm`'s dispatch
//! functions. One module per family of events, so each can be filled in
//! independently.
//!
//! # Order within a frame
//!
//! The C runs a typed command through a fixed sequence, and several triggers
//! in it can stop the command. [`DgIntake`] reproduces that sequence around
//! the engine's router:
//!
//! 1. [`DgIntake::CommandGate`] — `command` triggers (world, then mob, then
//!    object). One that returns non-zero took the command over: it is marked
//!    handled and nothing below sees it.
//! 2. [`DgIntake::PreCommit`] — the triggers that can refuse a specific
//!    action before it happens (`leave`, `door`, `get`, `drop`, `give`,
//!    `receive`, `bribe`, `wear`, `remove`, `consume`, `cast`). A refusal
//!    marks the command handled.
//! 3. The engine routes the command, and the pack's own handlers run. Every
//!    pack handler tests [`commands::is_claimed`](commands::is_claimed) to
//!    skip commands taken over by a trigger or carried out by pre-commit intake.
//! 4. [`DgIntake::PostRoute`] — the triggers that react to what happened
//!    (`speech`, `act`, `enter`, `greet`, `entry`, `memory`).
//!
//! "Claimed" is recorded in [`commands::DgClaimed`](commands::DgClaimed):
//! a system in step 1 or 2 calls `claim(world, msg)` or records the refusal,
//! marking the command handled in the engine's `HandledCommands` for the router
//! and recording it in `DgClaimed` for the pack's handlers.

use mib_core::prelude::*;
use mib_stdlib::prelude::{CommandSet, route_commands};

/// Where an intake system sits relative to the engine's command router.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DgIntake {
    /// Before routing: `command` triggers.
    CommandGate,
    /// Before routing, after the gate: triggers that can refuse an action.
    PreCommit,
    /// After routing: triggers that react to the action.
    PostRoute,
}

pub mod combat;
pub mod commands;
pub mod items;
pub mod movement;
pub mod world;

/// Register every intake module.
pub fn register(app: &mut App) {
    app.configure_sets(
        Update,
        (DgIntake::CommandGate, DgIntake::PreCommit)
            .chain()
            .in_set(CommandSet::PreRoute),
    );
    app.configure_sets(
        Update,
        DgIntake::PostRoute
            .in_set(BridgeSet::Game)
            .after(route_commands),
    );
    commands::register(app);
    movement::register(app);
    items::register(app);
    combat::register(app);
    world::register(app);
}
