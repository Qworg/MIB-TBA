//! `%…%` substitution (`dg_variables.c`): `var_subst` and `find_replacement`.
//!
//! Skeleton. The signatures are fixed; the bodies here are the minimum that
//! lets the other modules' tests run (plain `%name%` variables and `%%`) and
//! are replaced by the port.

use crate::host::{Cx, DgHost};

/// Name lookup and the special variables' world primitives.
pub trait SubstHost {}

/// `var_subst(go, sc, trig, type, line, buf)`: replace every `%…%` in `line`.
pub fn var_subst<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, line: &str) -> String {
    if !line.contains('%') {
        return line.to_owned();
    }
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(stripped) = after.strip_prefix('%') {
            out.push('%');
            rest = stripped;
            continue;
        }
        let Some(end) = after.find('%') else {
            rest = "";
            break;
        };
        let token = &after[..end];
        let (var, field) = token.split_once('.').unwrap_or((token, ""));
        out.push_str(&find_replacement(cx, var, field, ""));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// `find_replacement(go, sc, trig, type, var, field, subfield, str, slen)`:
/// the value of one `%var.field(subfield)%` reference.
pub fn find_replacement<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    var: &str,
    field: &str,
    _subfield: &str,
) -> String {
    if !field.is_empty() {
        return String::new();
    }
    if let Some(found) = cx.trig.vars.get(var) {
        return found.value.clone();
    }
    let context = cx.context();
    cx.host
        .script(cx.owner)
        .and_then(|script| script.global_vars.get_in_context(var, context))
        .map(|found| found.value.clone())
        .unwrap_or_default()
}
