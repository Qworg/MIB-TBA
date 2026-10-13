//! `dg_cast` and `dg_affect` (`dg_misc.c`).
//!
//! Skeleton.

use crate::host::{Cx, DgHost};

/// The spell and affect primitives.
pub trait MagicHost {}

/// `do_dg_cast(go, sc, trig, type, cmd)`: `cmd` is the whole substituted
/// `dg_cast '<spell>' [target]` line.
pub fn do_dg_cast<H: DgHost + ?Sized>(_cx: &mut Cx<'_, H>, _cmd: &str) {}

/// `do_dg_affect(go, sc, trig, type, cmd)`: `cmd` is the whole substituted
/// `dg_affect <target> <property> <value> <duration>` line.
pub fn do_dg_affect<H: DgHost + ?Sized>(_cx: &mut Cx<'_, H>, _cmd: &str) {}
