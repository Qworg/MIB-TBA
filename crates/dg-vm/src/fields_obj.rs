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
/// Returns `None` for a field the table does not name; the caller then
/// applies the C's fallback (the entity's own script variables, then an
/// "unknown field" log).
pub fn obj_field<H: DgHost + ?Sized>(
    _cx: &mut Cx<'_, H>,
    _obj: Uid,
    _field: &str,
    _subfield: &str,
) -> Option<String> {
    None
}
