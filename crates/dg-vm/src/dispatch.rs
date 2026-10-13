//! `dg_triggers.c`: which triggers fire for which event.
//!
//! Skeleton.

/// Enumerating the entities that can react to an event.
pub trait DispatchHost {}

use crate::host::DgHost;
use crate::types::Uid;

/// `load_mtrigger(ch)`: a mob was just loaded. Script `load` commands call
/// this for the mob they create.
pub fn load_mtrigger<H: DgHost + ?Sized>(_host: &mut H, _ch: Uid) {}

/// `load_otrigger(obj)`: an object was just loaded. Script `load` commands
/// call this for the object they create.
pub fn load_otrigger<H: DgHost + ?Sized>(_host: &mut H, _obj: Uid) {}

/// `enter_wtrigger(room, actor, dir)`: `actor` is entering `room` from
/// direction `dir` (`-1` when moved by a script). Returns `0` when a trigger
/// refuses the entry. Script `teleport` and `goto` commands call this.
pub fn enter_wtrigger<H: DgHost + ?Sized>(
    _host: &mut H,
    _room: Uid,
    _actor: Uid,
    _dir: i32,
) -> i32 {
    1
}
