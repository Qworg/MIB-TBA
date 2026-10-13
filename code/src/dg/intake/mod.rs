//! Intake: the systems that observe gameplay and call `dg-vm`'s dispatch
//! functions. One module per family of events, so each can be filled in
//! independently.

use mib_core::prelude::*;

pub mod combat;
pub mod commands;
pub mod items;
pub mod movement;
pub mod world;

/// Register every intake module.
pub fn register(app: &mut App) {
    commands::register(app);
    movement::register(app);
    items::register(app);
    combat::register(app);
    world::register(app);
}
