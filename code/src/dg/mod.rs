//! The DG script runtime: `dg-vm` hosted on the ECS world.
//!
//! `dg-vm` is an engine-agnostic port of tbaMUD's DG Scripts interpreter. It
//! reaches the game only through the [`dg_vm::DgHost`] trait and runs a script
//! when the game calls one of its dispatch functions (`dg_vm::dispatch`). This
//! module is the game's side of that contract:
//!
//! - [`host`] — [`host::EcsHost`], which borrows the `World` for the length of
//!   one dispatch and implements the nine host traits against it, one trait
//!   per file.
//! - [`mobcmd`] — the game's command interpreter as the VM's
//!   `mud_command`/`mudcommand` escape hatch.
//! - [`ids`] — the mapping between ECS entities and the VM's [`dg_vm::Uid`].
//! - [`script`] — per-entity script state and the trigger prototypes built
//!   from `tba.triggers`.
//! - [`waits`] — the pulse clock and the queue of scripts suspended in `wait`.
//! - [`vars`] — [`vars::TbaTriggerVars`], the persisted quest variables.
//! - [`intake`] — the systems that observe gameplay and call the dispatch
//!   functions, grouped by the events they own: commands, movement, items,
//!   combat and the world.
//!
//! The VM's run state lives in the [`DgVm`] resource. [`register`] is called
//! by [`TbaPlugin`](crate::module::TbaPlugin).

pub mod host;
pub mod ids;
pub mod intake;
pub mod mobcmd;
pub mod script;
pub mod vars;
pub mod waits;

use dg_vm::types::VmState;
use mib_core::prelude::*;

/// The interpreter state the C keeps in globals and statics, shared across
/// nested script runs for the life of the world (`BaseHost::vm_state`).
#[derive(Resource, Debug, Default)]
pub struct DgVm {
    pub state: VmState,
}

/// Install the runtime's resources and intake systems.
pub fn register(app: &mut App) {
    app.init_resource::<DgVm>();
    intake::register(app);
}
