//! `script_driver` (`dg_scripts.c`): runs a trigger.
//!
//! Skeleton. The contract below is fixed; the body is the port.

use crate::host::DgHost;
use crate::types::Uid;

/// `TRIG_NEW` / `TRIG_RESTART`: start from the top, or continue after a
/// `wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    New,
    Restart,
}

/// The interpreter's per-instance position and nesting state: the C's
/// `curr_state`, `GET_TRIG_DEPTH`, `GET_TRIG_LOOPS`, and the per-line
/// `loops`/`original` fields of `cmdlist_element`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunState {
    /// `GET_TRIG_DEPTH`: `0` when idle; `1` plus the `if` nesting while
    /// running or waiting.
    pub depth: i32,
}

impl RunState {
    /// `GET_TRIG_DEPTH(t) != 0`: running or waiting.
    pub fn is_active(&self) -> bool {
        self.depth != 0
    }
}

/// `script_driver(&go, trig, type, mode)`: run the owner's trigger instance
/// `trigger_id` and return its return value (default `1`).
///
/// The caller has already added the event's variables to the instance
/// (`host.script_mut(owner)` → `trigger_mut(id)` → `vars.add_var`).
pub fn script_driver<H: DgHost + ?Sized>(
    host: &mut H,
    owner: Uid,
    trigger_id: u64,
    mode: Mode,
) -> i32 {
    script_driver_default(host, owner, trigger_id, mode, 1)
}

/// `script_driver_default`: as [`script_driver`] with an explicit default
/// return value.
pub fn script_driver_default<H: DgHost + ?Sized>(
    _host: &mut H,
    _owner: Uid,
    _trigger_id: u64,
    _mode: Mode,
    default_ret: i32,
) -> i32 {
    default_ret
}

/// `trig_wait_event`: a scheduled `wait` came due. Resumes the trigger if the
/// owner and the instance still exist.
pub fn resume<H: DgHost + ?Sized>(host: &mut H, owner: Uid, trigger_id: u64) -> i32 {
    script_driver(host, owner, trigger_id, Mode::Restart)
}
