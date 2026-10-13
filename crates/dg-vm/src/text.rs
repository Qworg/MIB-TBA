//! `text_processed` (`dg_variables.c`): string functions applied to a
//! variable's value (`strlen`, `trim`, `contains`, `car`, `cdr`, `charat`,
//! `mudcommand`, `toupper`).
//!
//! Skeleton.

/// `text_processed(field, subfield, vd, str, slen)`.
///
/// `value` is the variable's value. Returns `Some(result)` when `field` is a
/// text function, `None` when it is not (the caller goes on to treat the
/// value as an entity reference).
pub fn text_processed(_field: &str, _subfield: &str, _value: &str) -> Option<String> {
    None
}
