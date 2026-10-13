//! The expression evaluator (`dg_scripts.c`): `eval_expr`, `eval_lhs_op_rhs`,
//! `eval_op`, `process_if`, `process_eval`.
//!
//! Skeleton. The signatures are fixed; the bodies here are the minimum that
//! lets the other modules' tests run (no operators) and are replaced by the
//! port.

use crate::host::{Cx, DgHost};
use crate::subst::var_subst;

/// `eval_expr(line, result, go, sc, trig, type)`.
pub fn eval_expr<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, line: &str) -> String {
    var_subst(cx, line.trim_start())
}

/// `eval_op(op, lhs, rhs, result, …)`: apply one operator to two already
/// evaluated operands. `switch` uses it directly to compare a `case`.
pub fn eval_op(op: &str, lhs: &str, rhs: &str) -> String {
    match op {
        "==" => u8::from(lhs.trim().eq_ignore_ascii_case(rhs.trim())).to_string(),
        _ => String::new(),
    }
}

/// `process_if(cond, go, sc, trig, type)`: true unless the result is empty or
/// starts with `0`.
pub fn process_if<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cond: &str) -> bool {
    let result = eval_expr(cx, cond);
    let result = crate::cstr::skip_spaces(&result);
    !(result.is_empty() || result.starts_with('0'))
}

/// `process_eval(go, sc, trig, type, cmd)`: `cmd` is the whole
/// `eval <name> <expression>` line; the result is stored as a trigger-local
/// variable under the script's current context (`sc ? sc->context : 0`), as
/// every local the interpreter creates is.
pub fn process_eval<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_eval, rest) = crate::cstr::half_chop(cmd);
    let (name, expr) = crate::cstr::half_chop(rest);
    if name.is_empty() || expr.is_empty() {
        return;
    }
    let value = eval_expr(cx, expr);
    let context = cx.context();
    cx.trig.vars.add_var(&name, &value, context);
}
