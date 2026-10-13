//! The script command interpreters: `dg_mobcmd.c`, `dg_objcmd.c`,
//! `dg_wldcmd.c`, and the message formatter in `dg_comm.c`.
//!
//! Skeleton.

use crate::host::{Cx, DgHost};

/// The world effects script commands have.
pub trait CmdHost {}

/// `script_command_interpreter(ch, cmd)` (`interpreter.c`): run `cmd` if its
/// first word is one of the mob script commands (`mecho`, `mload`, …).
/// Returns `false` when it is not, and the driver then hands the line to
/// [`crate::host::BaseHost::mud_command`].
pub fn script_command_interpreter<H: DgHost + ?Sized>(_cx: &mut Cx<'_, H>, _cmd: &str) -> bool {
    false
}

/// `obj_command_interpreter(obj, argument)` (`dg_objcmd.c`).
pub fn obj_command_interpreter<H: DgHost + ?Sized>(_cx: &mut Cx<'_, H>, _cmd: &str) {}

/// `wld_command_interpreter(room, argument)` (`dg_wldcmd.c`).
pub fn wld_command_interpreter<H: DgHost + ?Sized>(_cx: &mut Cx<'_, H>, _cmd: &str) {}
