//! The expression evaluator (`dg_scripts.c`): `eval_expr`, `eval_lhs_op_rhs`,
//! `eval_op`, `process_if`, `process_eval`.
//!
//! An expression is evaluated by scanning for the lowest-priority operator
//! (`||` first, `*` and `!` last), splitting there, and recursing on both
//! halves; a line with no operator is a parenthesised sub-expression or a
//! leaf handed to [`var_subst`]. All values are strings, and the operators
//! decide per call whether to treat them as numbers ([`is_num`]) or text.
//!
//! # Deviations from the C
//!
//! - An unbalanced `(` whose last byte is part of a multi-byte character
//!   drops the whole character where the C drops one byte of it (a `String`
//!   cannot hold half a character).
//! - A line that is just `(` evaluates to the empty string; the C reads the
//!   buffer past the terminator there.
//! - `INT_MIN / -1` wraps instead of trapping, and `*`, `+`, `-` wrap as C
//!   `int` arithmetic does in practice.
//! - [`eval_op`] with an operator not in the table yields the empty string;
//!   the C leaves its result buffer untouched. [`eval_expr`] only ever passes
//!   it operators from the table.

use crate::cstr::{atoi, is_num, is_space, matching_paren, matching_quote, one_argument};
use crate::cstr::{skip_spaces, str_cmp, str_str};
use crate::host::{Cx, DgHost};
use crate::subst::var_subst;

/// `MAX_INPUT_LENGTH` (`structs.h`): the buffer size every expression buffer
/// shares.
pub const MAX_INPUT_LENGTH: usize = 512;

/// The operators in priority order (`eval_lhs_op_rhs`'s `ops[]`). The order
/// matters: `!=` must precede `!`, and `/=` must precede `/`.
const OPS: &[&str] = &[
    "||", "&&", "==", "!=", "<=", ">=", "<", ">", "/=", "-", "+", "/", "*", "!",
];

/// `eval_expr(line, result, go, sc, trig, type)`: evaluate `line`.
///
/// An expression of `MAX_INPUT_LENGTH` bytes or more is refused (logged) and
/// evaluates to the empty string, which reads as false.
pub fn eval_expr<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, line: &str) -> String {
    let line = line.trim_start_matches(is_space);

    if line.len() >= MAX_INPUT_LENGTH {
        let mut shown = line.len().min(60);
        while !line.is_char_boundary(shown) {
            shown -= 1;
        }
        let message = format!(
            "Trigger: {}, VNum {}, type: {}. Expression is {} characters, over the {} limit: '{}...'",
            cx.trig.proto.name,
            cx.trig.proto.vnum,
            cx.attach.index(),
            line.len(),
            MAX_INPUT_LENGTH - 1,
            &line[..shown]
        );
        cx.host.script_log(&message);
        return String::new();
    }

    if let Some(result) = eval_lhs_op_rhs(cx, line) {
        return result;
    }

    if line.starts_with('(') {
        let close = matching_paren(line, 0);
        if close < 1 {
            return eval_expr(cx, "");
        }
        let mut end = close.min(line.len());
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        return eval_expr(cx, &line[1..end]);
    }

    var_subst(cx, line)
}

/// `eval_lhs_op_rhs(expr, result, …)`: if `expr` has the form `lhs op rhs`,
/// the answer. The first operator in [`OPS`] order that occurs at the start
/// of a token wins, leftmost occurrence first.
///
/// Tokens start at the beginning of the line, after a parenthesised group or
/// a quoted string, after any other single character, and after a run of
/// letters and digits (which swallows spaces too, so an operator never
/// starts inside a word).
fn eval_lhs_op_rhs<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, expr: &str) -> Option<String> {
    let bytes = expr.as_bytes();
    let mut tokens: Vec<usize> = Vec::new();
    let mut p = 0;
    while p < bytes.len() {
        tokens.push(p);
        if bytes[p] == b'(' {
            p = matching_paren(expr, p) + 1;
        } else if bytes[p] == b'"' {
            p = matching_quote(expr, p) + 1;
        } else if bytes[p].is_ascii_alphanumeric() {
            p += 1;
            while p < bytes.len()
                && (bytes[p].is_ascii_alphanumeric() || is_space(char::from(bytes[p])))
            {
                p += 1;
            }
        } else {
            p += 1;
        }
    }

    for op in OPS {
        for &start in &tokens {
            if bytes[start..].starts_with(op.as_bytes()) {
                // `start` and `start + op.len()` follow ASCII bytes of the
                // operator, so both are character boundaries.
                let lhs = eval_expr(cx, &expr[..start]);
                let rhs = eval_expr(cx, &expr[start + op.len()..]);
                return Some(eval_op(op, &lhs, &rhs));
            }
        }
    }
    None
}

/// `eval_op(op, lhs, rhs, result, …)`: apply one operator to two already
/// evaluated operands. `switch` uses it directly to compare a `case`.
///
/// Both operands lose their surrounding whitespace first. The comparison
/// operators compare as integers when both sides are [`is_num`], otherwise as
/// case-insensitive strings; `!=` on strings yields the raw [`str_cmp`]
/// difference, not `1`.
pub fn eval_op(op: &str, lhs: &str, rhs: &str) -> String {
    let lhs = lhs.trim_matches(is_space);
    let rhs = rhs.trim_matches(is_space);
    let both_num = is_num(lhs) && is_num(rhs);
    let flag = |value: bool| i32::from(value).to_string();
    let falsy = |text: &str| text.is_empty() || text.starts_with('0');

    match op {
        "||" => flag(!(falsy(lhs) && falsy(rhs))),
        "&&" => flag(!(falsy(lhs) || falsy(rhs))),
        "==" if both_num => flag(atoi(lhs) == atoi(rhs)),
        "==" => flag(str_cmp(lhs, rhs) == 0),
        "!=" if both_num => flag(atoi(lhs) != atoi(rhs)),
        "!=" => str_cmp(lhs, rhs).to_string(),
        "<=" if both_num => flag(atoi(lhs) <= atoi(rhs)),
        "<=" => flag(str_cmp(lhs, rhs) <= 0),
        ">=" if both_num => flag(atoi(lhs) >= atoi(rhs)),
        ">=" => flag(str_cmp(lhs, rhs) >= 0),
        "<" if both_num => flag(atoi(lhs) < atoi(rhs)),
        "<" => flag(str_cmp(lhs, rhs) < 0),
        ">" if both_num => flag(atoi(lhs) > atoi(rhs)),
        ">" => flag(str_cmp(lhs, rhs) > 0),
        "/=" => flag(str_str(lhs, rhs).is_some()),
        "*" => atoi(lhs).wrapping_mul(atoi(rhs)).to_string(),
        "/" => {
            let divisor = atoi(rhs);
            if divisor == 0 {
                "0".to_owned()
            } else {
                atoi(lhs).wrapping_div(divisor).to_string()
            }
        }
        "+" => atoi(lhs).wrapping_add(atoi(rhs)).to_string(),
        "-" => atoi(lhs).wrapping_sub(atoi(rhs)).to_string(),
        "!" if is_num(rhs) => flag(atoi(rhs) == 0),
        "!" => flag(rhs.is_empty()),
        _ => String::new(),
    }
}

/// `process_if(cond, go, sc, trig, type)`: true unless the result is empty or
/// starts with `0`.
pub fn process_if<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cond: &str) -> bool {
    let result = eval_expr(cx, cond);
    let result = skip_spaces(&result);
    !(result.is_empty() || result.starts_with('0'))
}

/// `process_eval(go, sc, trig, type, cmd)`: `cmd` is the whole
/// `eval <name> <expression>` line; the result is stored as a trigger-local
/// variable under the script's current context (`sc ? sc->context : 0`), as
/// every local the interpreter creates is.
///
/// Both words go through `one_argument`, so the name is lowercased and a fill
/// word (`in`, `to`, `at`, …) in front of it is skipped.
pub fn process_eval<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_eval, rest) = one_argument(cmd);
    let (name, rest) = one_argument(rest);
    let expr = skip_spaces(rest);

    if name.is_empty() {
        cx.log(&format!("eval w/o an arg: '{cmd}'"));
        return;
    }

    let value = eval_expr(cx, expr);
    let context = cx.context();
    cx.trig.vars.add_var(&name, &value, context);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::host::BaseHost;
    use crate::mock::MockHost;
    use crate::types::{AttachType, TrigInstance};

    /// Run `f` with a context for a mob-attached trigger that has the local
    /// variables `vars`.
    fn with_cx<R>(vars: &[(&str, &str)], f: impl FnOnce(&mut Cx<'_, MockHost>) -> R) -> R {
        let mut host = MockHost::new();
        let mob = host.add_mob(1, "a mob");
        let proto = host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        let mut trig = TrigInstance::new(1, proto);
        for (name, value) in vars {
            trig.vars.add_var(name, value, 0);
        }
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        f(&mut cx)
    }

    fn eval(line: &str) -> String {
        with_cx(&[], |cx| eval_expr(cx, line))
    }

    fn truth(line: &str) -> bool {
        with_cx(&[], |cx| process_if(cx, line))
    }

    #[test]
    fn leaves_are_substituted_text() {
        assert_eq!(eval("hello world"), "hello world");
        assert_eq!(eval("   padded"), "padded");
        assert_eq!(eval(""), "");
        assert_eq!(with_cx(&[("x", "42")], |cx| eval_expr(cx, "%x% + 1")), "43");
    }

    #[test]
    fn numeric_operators() {
        assert_eq!(eval("2 + 3"), "5");
        assert_eq!(eval("2 - 3"), "-1");
        assert_eq!(eval("6 * 7"), "42");
        assert_eq!(eval("7 / 2"), "3");
        assert_eq!(eval("-7 / 2"), "-3");
        // Division by zero yields 0 rather than a trap.
        assert_eq!(eval("7 / 0"), "0");
        assert_eq!(eval("7 / abc"), "0");
        // Unary minus is `0 - rhs`: atoi("") == 0.
        assert_eq!(eval("-5"), "-5");
        assert_eq!(eval("3 - -2"), "5");
    }

    #[test]
    fn arithmetic_wraps_like_c_int() {
        assert_eq!(eval("2147483647 + 1"), "-2147483648");
        assert_eq!(eval("2147483647 * 2"), "-2");
        assert_eq!(eval_op("-", "-2147483647", "2"), "2147483647");
        assert_eq!(eval_op("/", "-2147483648", "-1"), "-2147483648");
    }

    #[test]
    fn operator_priority_is_list_order_not_arithmetic_order() {
        // '-' is split before '+', and left to right: 1 - (2 + 3)... the
        // leftmost '-' is the only one, so lhs = "10", rhs = "2 + 3".
        assert_eq!(eval("10 - 2 + 3"), "5");
        // '+' before '*': 2 + (3 * 4).
        assert_eq!(eval("2 + 3 * 4"), "14");
        // '/' before '*': (8) / (2 * 2) = 2.
        assert_eq!(eval("8 / 2 * 2"), "2");
        // '-' splits at the first one: 10 - (3 - 2) = 9.
        assert_eq!(eval("10 - 3 - 2"), "9");
        // '||' binds loosest.
        assert!(truth("0 && 0 || 1"));
        assert!(!truth("1 && 0 || 0"));
    }

    #[test]
    fn comparisons_numeric_and_string() {
        assert_eq!(eval("5 == 5"), "1");
        assert_eq!(eval("5 == 6"), "0");
        assert_eq!(eval("05 == 5"), "1");
        assert_eq!(eval("abc == ABC"), "1");
        assert_eq!(eval("abc == abd"), "0");
        // One side non-numeric: string comparison.
        assert_eq!(eval("5 == 5x"), "0");
        assert_eq!(eval("5 != 6"), "1");
        assert_eq!(eval("5 != 5"), "0");
        // String `!=` is the str_cmp difference: 'b' - 'a'.
        assert_eq!(eval("b != a"), "1");
        assert_eq!(eval("a != c"), "-2");
        assert_eq!(eval("a != A"), "0");
        assert_eq!(eval("9 < 10"), "1");
        assert_eq!(eval("9 <= 9"), "1");
        assert_eq!(eval("9 > 10"), "0");
        assert_eq!(eval("9 >= 10"), "0");
        // String ordering: "9x" vs "10x" compares '9' with '1'.
        assert_eq!(eval("9x < 10x"), "0");
        assert_eq!(eval("apple < banana"), "1");
        assert_eq!(eval("apple <= apple"), "1");
        assert_eq!(eval("apple >= banana"), "0");
        assert_eq!(eval("pear > banana"), "1");
    }

    #[test]
    fn contains_operator_uses_str_str() {
        assert_eq!(eval("hello world /= WORLD"), "1");
        assert_eq!(eval("hello /= xyz"), "0");
        assert_eq!(eval_op("/=", "abc", ""), "0");
        // str_str does not back up after a partial match.
        assert_eq!(eval_op("/=", "aaab", "aab"), "0");
    }

    #[test]
    fn logical_operators_and_not() {
        assert_eq!(eval_op("||", "", "0"), "0");
        assert_eq!(eval_op("||", "0", "x"), "1");
        assert_eq!(eval_op("||", "x", ""), "1");
        assert_eq!(eval_op("&&", "x", "y"), "1");
        assert_eq!(eval_op("&&", "x", "0abc"), "0");
        assert_eq!(eval_op("&&", "", "y"), "0");
        assert_eq!(eval("!0"), "1");
        assert_eq!(eval("!5"), "0");
        assert_eq!(eval("!-1"), "0");
        // Non-numeric operand: true when empty.
        assert_eq!(eval_op("!", "", "abc"), "0");
        assert_eq!(eval_op("!", "", ""), "1");
        assert_eq!(eval("!!1"), "1");
        // A lone sign is a number: atoi("-") == 0.
        assert_eq!(eval_op("!", "", "-"), "1");
    }

    #[test]
    fn eval_op_trims_operands() {
        assert_eq!(eval_op("==", "  5  ", "\t5\n"), "1");
        assert_eq!(eval_op("==", " a b ", "a b"), "1");
        assert_eq!(eval_op("+", " 1 ", " 2 "), "3");
        assert_eq!(eval_op("||", "  ", " "), "0");
        assert_eq!(eval_op("nonsense", "1", "2"), "");
    }

    #[test]
    fn parentheses_group_and_strip() {
        assert_eq!(eval("(2 + 3) * 4"), "20");
        assert_eq!(eval("((7))"), "7");
        assert_eq!(eval("(1 + 2) == 3"), "1");
        assert_eq!(eval("2 * (3 + 4)"), "14");
        assert_eq!(eval("(a)"), "a");
        // An operator inside parentheses is not a split point.
        assert_eq!(eval("(1 || 0) && 1"), "1");
        // Parentheses inside quotes do not count.
        assert_eq!(eval("(\"a)\")"), "\"a)\"");
    }

    #[test]
    fn unbalanced_parentheses_drop_the_last_byte() {
        // matching_paren returns the last byte; the C overwrites it with NUL.
        assert_eq!(eval("(abc"), "ab");
        assert_eq!(eval("(abc d"), "abc ");
        assert_eq!(eval("("), "");
        assert_eq!(eval("()"), "");
        // Non-ASCII last byte: the whole character goes, never a panic.
        assert_eq!(eval("(ab\u{e9}"), "ab");
    }

    #[test]
    fn operators_inside_words_and_quotes_are_not_split() {
        // A letter run swallows following spaces, so "a b" is one token and
        // the '=' operators are found at their own token starts.
        assert_eq!(eval("a b == a b"), "1");
        // Quoted strings are skipped over as a unit.
        assert_eq!(eval("\"a + b\" == \"a + b\""), "1");
        assert_eq!(eval("\"a == b"), "\"a == b");
        // An unterminated quote swallows to the end; no operator found.
        assert_eq!(eval("\"abc + 1"), "\"abc + 1");
    }

    #[test]
    fn operator_after_digit_run_is_found() {
        // Token starts: "12 " is one run, then '+', then "3".
        assert_eq!(eval("12 +3"), "15");
        assert_eq!(eval("12+3"), "15");
    }

    #[test]
    fn over_long_expression_is_refused_and_logged() {
        let long = format!("1 + {}", "9".repeat(MAX_INPUT_LENGTH));
        let mut host = MockHost::new();
        let mob = host.add_mob(1, "a mob");
        let proto = host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        let mut trig = TrigInstance::new(1, proto);
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        assert_eq!(eval_expr(&mut cx, &long), "");
        assert!(!process_if(&mut cx, &long));
        let exact = "9".repeat(MAX_INPUT_LENGTH - 1);
        assert_eq!(eval_expr(&mut cx, &exact), exact);
        assert_eq!(host.logs.len(), 2);
        assert!(host.logs[0].starts_with("Trigger: trigger 7, VNum 7, type: 0. Expression is 516 characters, over the 511 limit: '1 + 9999"));
        assert!(host.logs[0].ends_with("...'"));
    }

    #[test]
    fn over_long_non_ascii_expression_logs_without_splitting_a_character() {
        // 60 bytes land in the middle of a two-byte character.
        let long = format!("{}{}", "a".repeat(59), "\u{e9}".repeat(300));
        let mut host = MockHost::new();
        let mob = host.add_mob(1, "a mob");
        let proto = host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        let mut trig = TrigInstance::new(1, proto);
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        assert_eq!(eval_expr(&mut cx, &long), "");
        assert_eq!(host.logs.len(), 1);
    }

    #[test]
    fn hostile_inputs_do_not_panic() {
        for text in [
            "(",
            ")",
            "\"",
            "\\",
            "((((",
            "))))",
            "!",
            "!!!!!!!!",
            "&&",
            "||",
            "==",
            "/",
            "-",
            "+",
            "\u{e9}",
            "a\u{e9}+\u{e9}",
            "(\u{e9}",
            "\"\u{e9}",
            "1 + ",
            " + 1",
            "\t",
            "((\"))",
        ] {
            let _ = eval(text);
            let _ = truth(text);
        }
        let deep = format!("{}1{}", "(".repeat(250), ")".repeat(250));
        assert_eq!(eval(&deep), "1");
        let bangs = format!("{}1", "!".repeat(400));
        assert_eq!(eval(&bangs), "1");
    }

    #[test]
    fn process_if_truth() {
        assert!(truth("1"));
        assert!(truth("  yes"));
        assert!(truth("-1"));
        assert!(!truth(""));
        assert!(!truth("0"));
        assert!(!truth("  0"));
        assert!(!truth("0abc"));
        assert!(truth("abc"));
        assert!(truth("3 > 2"));
        assert!(!truth("3 < 2"));
        assert!(truth("!0"));
    }

    #[test]
    fn corpus_conditions() {
        // Shapes taken from lib/world/trg: `if %actor.level% < 10`,
        // `if (%arg% /= foo) && !%done%`.
        with_cx(
            &[("level", "5"), ("arg", "say foo bar"), ("done", "0")],
            |cx| {
                assert!(process_if(cx, "%level% < 10"));
                assert!(!process_if(cx, "%level% >= 10"));
                assert!(process_if(cx, "(%arg% /= foo) && !%done%"));
                assert!(process_if(cx, "%level% == 5 && %arg% /= bar"));
                assert!(!process_if(cx, "%level% == 6 || %arg% /= baz"));
            },
        );
    }

    #[test]
    fn process_eval_stores_a_local_variable() {
        with_cx(&[], |cx| {
            process_eval(cx, "eval total 2 + 3 * 4");
            assert_eq!(cx.trig.vars.get("total").unwrap().value, "14");
            process_eval(cx, "eval Total (1 + 1) * 5");
            // Names are lowercased by one_argument, so this replaces `total`.
            assert_eq!(cx.trig.vars.get("total").unwrap().value, "10");
            process_eval(cx, "eval empty");
            assert_eq!(cx.trig.vars.get("empty").unwrap().value, "");
            // A fill word before the name is skipped.
            process_eval(cx, "eval to dest hello");
            assert_eq!(cx.trig.vars.get("dest").unwrap().value, "hello");
            assert!(cx.trig.vars.get("to").is_none());
        });
    }

    #[test]
    fn process_eval_without_a_name_logs() {
        let mut host = MockHost::new();
        let mob = host.add_mob(1, "a mob");
        let proto = host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        let mut trig = TrigInstance::new(1, proto);
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        process_eval(&mut cx, "eval");
        process_eval(&mut cx, "eval   ");
        assert!(cx.trig.vars.is_empty());
        assert_eq!(host.logs.len(), 2);
        assert_eq!(
            host.logs[0],
            "Trigger: trigger 7, VNum 7. eval w/o an arg: 'eval'"
        );
    }

    #[test]
    fn process_eval_uses_the_script_context() {
        let mut host = MockHost::new();
        let mob = host.add_mob(1, "a mob");
        let proto = host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        host.attach(mob, &proto).unwrap();
        host.script_mut(mob).unwrap().context = 77;
        let mut trig = TrigInstance::new(1, proto);
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        process_eval(&mut cx, "eval n 1 + 1");
        assert_eq!(cx.trig.vars.get("n").unwrap().context, 77);
    }
}
