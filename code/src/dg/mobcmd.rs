//! The game's command interpreter as seen from a script: a mob command line
//! that is not a script command, and the `mudcommand` text function.

use dg_vm::Uid;

use super::host::EcsHost;

/// `command_interpreter(ch, line)`: have `ch` perform an ordinary game command.
pub fn mud_command(_host: &mut EcsHost<'_>, _ch: Uid, _line: &str) {}

/// The full name of the first game command that `prefix` abbreviates.
pub fn mud_command_name(_prefix: &str) -> Option<String> {
    None
}
