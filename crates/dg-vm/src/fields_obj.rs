//! The object field table of `find_replacement` (`dg_variables.c`).
//!
//! Skeleton.

use crate::host::{Cx, DgHost};
use crate::types::Uid;

/// The object attributes the field table reads and writes.
pub trait ObjHost {}

/// The `if (o) { … }` branch of `find_replacement`: the value of
/// `field(subfield)` on this object.
///
/// Returns `None` when the table leaves the result untouched (the C's
/// `*str == '\x1'` sentinel); the caller then applies the C's fallback (the
/// entity's own script variables, then an "unknown field" log). That is the
/// case for a field the table does not name, and also for the few it does
/// name that write nothing on some path.
pub fn obj_field<H: DgHost + ?Sized>(
    _cx: &mut Cx<'_, H>,
    _obj: Uid,
    _field: &str,
    _subfield: &str,
) -> Option<String> {
    None
}
