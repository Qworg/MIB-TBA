//! `script_driver` (`dg_scripts.c`): runs a trigger.
//!
//! Ports `script_driver`/`script_driver_default`, the control-flow scans they
//! use (`find_end`, `find_else_end`, `find_case`, `find_done`), and the
//! statements the driver executes itself (`wait`, `set`, `unset`, `global`,
//! `context`, `remote`, `rdelete`, `return`, `attach`, `detach`, `makeuid`,
//! `extract`, `dg_letter`, `halt`, `nop`), plus `remove_trigger` and
//! `extract_script` (`dg_handler.c`) which `detach` and the depth cap use, and
//! `trig_wait_event` as [`resume`].
//!
//! # Lines, not pointers
//!
//! The C walks a linked list of `cmdlist_element`; here a position is an index
//! into [`crate::script::Program::lines`], and `cl->next == NULL` is "the
//! index is the last line". A C function that can return `NULL` (only
//! `find_done`, on malformed scripts) returns `lines.len()` here, which ends
//! the script the way the C's `cl = cl->next` would if it did not crash.
//! `find_end` and `find_done` recurse in the C; here they keep an explicit
//! nesting counter, so a hostile script of a million nested `if`s cannot
//! overflow the stack. They still log what the C logs.
//!
//! # Deviations from the C
//!
//! - Per-line `loops` and `original` live in [`RunState`], per instance (the
//!   C shares them across every instance of a prototype).
//! - The C's `static int depth` is [`BaseHost::depth_mut`].
//! - Statements the C would run against a freed trigger or script (anything
//!   after a trigger detached itself, or `sc` being NULL) run against an
//!   empty variable list or are skipped. Waits are never queued for an
//!   instance that was detached (`check_in` fails).
//! - `dg_letter` takes the character that starts at byte `num - 1`; inside a
//!   multi-byte character it yields U+FFFD (the C copies a raw byte).
//! - `trig_wait_event`'s three syslog lines for an unknown owner ("Trigger
//!   restarted on unknown entity. Vnum: %d", "Type: %s trigger", "attached %d
//!   places") are not emitted: there is no syslog primitive and no instance
//!   count (`trig_index[]->number`, which `extract_trigger` also decrements).
//!   Only the `script_log` line the C sends to builders is written. Reported
//!   as a contract request.
//!
//! # Quirks kept
//!
//! - A `wait` whose argument cannot be parsed logs and leaves the trigger
//!   running-but-not-waiting: `GET_TRIG_DEPTH` stays non-zero, so the trigger
//!   never fires again. The script ends there.
//! - The C has no "version line" handling in `script_driver`; none is ported.
//! - The `default` label inside a `switch` is not a keyword of the main loop in
//!   the C (only `find_case` knows it), so a `default` line reached by falling
//!   through a case is dispatched as a command, exactly as there.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::cstr::{
    any_one_arg, atoi, atol, half_chop, is_abbrev, is_space, skip_spaces, starts_with_ci, str_eq,
    two_arguments,
};
use crate::expr::{eval_expr, eval_op, process_eval, process_if};
use crate::host::{BaseHost, Cx, DgHost};
use crate::lookup::{
    find_char, find_obj, find_room, get_char_in_room, get_char_near_obj, get_char_room_vis,
    get_number, get_obj_in_list_vis, get_obj_in_room, get_obj_near_obj, isname, obj_room,
};
use crate::script::Line;
use crate::subst::var_subst;
use crate::types::{
    AttachType, Kind, MAX_INPUT_LENGTH, MAX_SCRIPT_DEPTH, PASSES_PER_SEC, PULSES_PER_MUD_HOUR,
    SCRIPT_ERROR_CODE, SECS_PER_MUD_DAY, SECS_PER_MUD_HOUR, Script, TrigInstance, UID_CHAR, Uid,
    uid_text,
};

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
///
/// Deviation: in the C the per-line `loops`/`original` live on the command
/// list, which every instance of a prototype shares. Here they are per
/// instance, so two mobs running the same looping trigger do not disturb
/// each other's counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunState {
    /// `GET_TRIG_DEPTH`: `0` when idle; `1` plus the `if` nesting while
    /// running or waiting.
    pub depth: i32,
    /// `GET_TRIG_LOOPS`: `while` iterations this run (reset by `TRIG_NEW`).
    pub loops: i32,
    /// `curr_state`: the line index a `wait` resumes at (`lines.len()` when
    /// the `wait` was the last line, the C's NULL).
    pub curr_state: usize,
    /// `cl->loops` of each `while` line, by line index.
    pub line_loops: BTreeMap<usize, i32>,
    /// `cl->original` of each `done` line: the index of the `while` that
    /// last ran up to it.
    pub originals: BTreeMap<usize, usize>,
}

impl RunState {
    /// `GET_TRIG_DEPTH(t) != 0`: running or waiting.
    pub fn is_active(&self) -> bool {
        self.depth != 0
    }
}

/// How a run left the line loop.
enum Outcome {
    /// Fell off the end, or `halt`, or a `break` out of a runaway: the
    /// end-of-run cleanup applies.
    Finished,
    /// A `wait` statement: the pulses to queue, or `None` when its argument
    /// was bad and no event was created.
    Wait(Option<i64>),
    /// `dg_owner_purged` was set by the command just run.
    Purged,
}

/// What one line asks of the loop.
enum Step {
    /// Set `cl` to this line, then advance (the `for` increment).
    Goto(usize),
    /// The C's `break` out of the loop.
    Halt,
    /// Return from the driver.
    Yield(Outcome),
}

/// `script_driver(&go, trig, type, mode)`: run the owner's trigger instance
/// `trigger_id` and return its return value (default `1`).
///
/// The caller has already added the event's variables to the instance
/// (`host.script_mut(owner)` → `trigger_mut(id)` → `vars.add_var`).
///
/// The run type is the owner's kind (`AttachType::from`), not the
/// prototype's. The driver checks the instance out of the owner's script for
/// the run (see [`crate::host::Cx`]).
///
/// The C's `go_adress` out-parameter, which it NULLs when an object owner
/// purges itself, has no counterpart: after the call, a caller that the C
/// guards with `if (!obj)` tests `!host.exists(owner)`.
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
///
/// An owner that does not exist, or has no such instance (or has it already
/// running), yields `default_ret`: the C would dereference NULL.
pub fn script_driver_default<H: DgHost + ?Sized>(
    host: &mut H,
    owner: Uid,
    trigger_id: u64,
    mode: Mode,
    default_ret: i32,
) -> i32 {
    let Some(kind) = host.kind(owner) else {
        return default_ret;
    };
    let attach = AttachType::from(kind);

    if *host.depth_mut() > MAX_SCRIPT_DEPTH {
        let vnum = host
            .script(owner)
            .and_then(|script| script.trigger(trigger_id))
            .map_or(-1, |trigger| trigger.proto.vnum);
        host.script_log(&format!(
            "Trigger {vnum} recursed beyond maximum allowed depth."
        ));
        let line = format!(
            "It was attached to {} [{}]",
            host.name(owner),
            host.vnum(owner)
        );
        host.script_log(&line);
        extract_script(host, owner);
        return SCRIPT_ERROR_CODE;
    }

    let Some(mut trig) = host
        .script_mut(owner)
        .and_then(|script| script.check_out(trigger_id))
    else {
        return default_ret;
    };

    {
        let depth = host.depth_mut();
        *depth = depth.saturating_add(1);
    }
    if mode == Mode::New {
        trig.run.depth = 1;
        trig.run.loops = 0;
        if let Some(script) = host.script_mut(owner) {
            script.context = 0;
        }
    }
    *host.owner_purged_mut() = false;

    let start = if mode == Mode::New {
        0
    } else {
        trig.run.curr_state
    };
    let (outcome, ret_val) = {
        let mut cx = Cx {
            host: &mut *host,
            owner,
            attach,
            trig: &mut trig,
        };
        run_lines(&mut cx, start, default_ret)
    };

    end_run(host, owner, trig, outcome);
    ret_val
}

/// The half of `script_driver_default` after the line loop: the end-of-run
/// cleanup (a finished run frees its variables and goes idle; a purged or
/// waiting one does not), the depth decrement, returning the instance and
/// queueing the `wait`.
fn end_run<H: DgHost + ?Sized>(host: &mut H, owner: Uid, mut trig: TrigInstance, outcome: Outcome) {
    if matches!(outcome, Outcome::Finished) {
        trig.vars.clear();
        trig.run.depth = 0;
    }
    {
        let depth = host.depth_mut();
        *depth = depth.saturating_sub(1);
    }
    let id = trig.id;
    let kept = host
        .script_mut(owner)
        .is_some_and(|script| script.check_in(trig));
    if let Outcome::Wait(Some(pulses)) = outcome
        && kept
    {
        host.schedule_wait(owner, id, pulses);
    }
}

/// `trig_wait_event`: a scheduled `wait` came due. Resumes the trigger if the
/// owner and the instance still exist.
///
/// Always returns `0`, as the C does ("Do not reenqueue"): the script's own
/// return value is discarded, and a host that wires this to an event queue
/// must not read the result as a requeue delay.
pub fn resume<H: DgHost + ?Sized>(host: &mut H, owner: Uid, trigger_id: u64) -> i32 {
    if !host.exists(owner) {
        // The C's three syslog lines have no primitive (see the module docs).
        host.script_log("Trigger restart attempt on unknown entity.");
        return 0;
    }
    let present = host
        .script(owner)
        .is_some_and(|script| script.trigger(trigger_id).is_some());
    if present {
        script_driver(host, owner, trigger_id, Mode::Restart);
    }
    0
}

/// The `for (cl = …; cl && GET_TRIG_DEPTH(trig); cl = cl->next)` loop of
/// `script_driver_default`.
fn run_lines<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    start: usize,
    default_ret: i32,
) -> (Outcome, i32) {
    let program = Arc::clone(&cx.trig.proto.program);
    let lines: &[Line] = &program.lines;
    let mut ret_val = default_ret;
    let mut cl = start;
    while cl < lines.len() && cx.trig.run.depth != 0 {
        let stepped = step(cx, lines, cl, &mut ret_val);
        // Whatever the line ran (a command, `dg_cast`, `detach`) may have
        // freed this trigger through a nested one.
        if !matches!(stepped, Step::Yield(Outcome::Purged)) {
            clear_if_detached(cx);
        }
        match stepped {
            Step::Goto(next) => cl = next.saturating_add(1),
            Step::Halt => return (Outcome::Finished, ret_val),
            Step::Yield(outcome) => return (outcome, ret_val),
        }
    }
    (Outcome::Finished, ret_val)
}

/// `p + n`: the text past a keyword whose first `n` bytes are known ASCII.
fn after(text: &str, n: usize) -> &str {
    text.get(n..).unwrap_or("")
}

/// `script_log("Trigger VNum %d …")`.
fn vnum_log<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, what: &str) {
    let line = format!("Trigger VNum {} {}", cx.trig.proto.vnum, what);
    cx.host.script_log(&line);
}

/// One iteration of the main loop body for line `cl`.
fn step<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    lines: &[Line],
    cl: usize,
    ret_val: &mut i32,
) -> Step {
    let p = lines[cl].body();
    if p.starts_with('*') {
        return Step::Goto(cl);
    }
    if starts_with_ci(p, "if ") {
        if process_if(cx, after(p, 3)) {
            cx.trig.run.depth = cx.trig.run.depth.wrapping_add(1);
            return Step::Goto(cl);
        }
        return Step::Goto(find_else_end(cx, lines, cl));
    }
    if starts_with_ci(p, "elseif ") || starts_with_ci(p, "else") {
        // If not in an if-block, ignore the extra 'else[if]' and warn about it.
        if cx.trig.run.depth == 1 {
            vnum_log(cx, "has 'else' without 'if'.");
            return Step::Goto(cl);
        }
        let end = find_end(&mut *cx.host, cx.trig.proto.vnum, lines, cl);
        cx.trig.run.depth = cx.trig.run.depth.wrapping_sub(1);
        return Step::Goto(end);
    }
    if starts_with_ci(p, "while ") {
        let temp = find_done(lines, cl);
        if temp >= lines.len() {
            vnum_log(cx, "has 'while' without 'done'.");
            return Step::Halt;
        }
        if process_if(cx, after(p, 6)) {
            cx.trig.run.originals.insert(temp, cl);
            return Step::Goto(cl);
        }
        cx.trig.run.line_loops.remove(&cl);
        return Step::Goto(temp);
    }
    if starts_with_ci(p, "switch ") {
        return Step::Goto(find_case(cx, lines, cl, after(p, 7)));
    }
    if starts_with_ci(p, "end") {
        // If not in an if-block, ignore the extra 'end' and warn about it.
        if cx.trig.run.depth == 1 {
            vnum_log(cx, "has 'end' without 'if'.");
            return Step::Goto(cl);
        }
        cx.trig.run.depth = cx.trig.run.depth.wrapping_sub(1);
        return Step::Goto(cl);
    }
    if starts_with_ci(p, "done") {
        return done_line(cx, lines, cl);
    }
    if starts_with_ci(p, "break") {
        return Step::Goto(find_done(lines, cl));
    }
    if starts_with_ci(p, "case") {
        // Do nothing, this allows multiple cases to a single instance.
        return Step::Goto(cl);
    }
    command_line(cx, lines, cl, p, ret_val)
}

/// The `done` branch: if a `while` ran up to this line, re-test it and loop.
fn done_line<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, lines: &[Line], cl: usize) -> Step {
    let Some(&original) = cx.trig.run.originals.get(&cl) else {
        return Step::Goto(cl);
    };
    let orig_cmd = lines.get(original).map_or("", Line::body);
    if !process_if(cx, after(orig_cmd, 6)) {
        // Falling through a switch statement ends it.
        return Step::Goto(cl);
    }
    cx.trig.run.loops = cx.trig.run.loops.wrapping_add(1);
    let count = {
        let count = cx.trig.run.line_loops.entry(original).or_insert(0);
        *count = count.wrapping_add(1);
        *count
    };
    if count == 30 {
        cx.trig.run.line_loops.insert(original, 0);
        let pulses = process_wait(cx, "wait 1", lines, original);
        return Step::Yield(Outcome::Wait(pulses));
    }
    if cx.trig.run.loops >= 100 {
        vnum_log(cx, "has looped 100 times!!!");
        return Step::Halt;
    }
    Step::Goto(original)
}

/// The `else` branch of the main loop: substitute the line and dispatch on
/// its first word, in the C's order.
fn command_line<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    lines: &[Line],
    cl: usize,
    p: &str,
    ret_val: &mut i32,
) -> Step {
    let cmd = var_subst(cx, p);
    let cmd = cmd.as_str();
    if starts_with_ci(cmd, "eval ") {
        process_eval(cx, cmd);
    } else if starts_with_ci(cmd, "nop ") {
        // nop: do nothing
    } else if starts_with_ci(cmd, "extract ") {
        extract_value(cx, cmd);
    } else if starts_with_ci(cmd, "dg_letter ") {
        dg_letter_value(cx, cmd);
    } else if starts_with_ci(cmd, "makeuid ") {
        makeuid_var(cx, cmd);
    } else if starts_with_ci(cmd, "halt") {
        return Step::Halt;
    } else if starts_with_ci(cmd, "dg_cast ") {
        crate::magic::do_dg_cast(cx, cmd);
    } else if starts_with_ci(cmd, "dg_affect ") {
        crate::magic::do_dg_affect(cx, cmd);
    } else if starts_with_ci(cmd, "global ") {
        let context = cx.context();
        process_global(cx, cmd, context);
    } else if starts_with_ci(cmd, "context ") {
        process_context(cx, cmd);
    } else if starts_with_ci(cmd, "remote ") {
        process_remote(cx, cmd);
    } else if starts_with_ci(cmd, "rdelete ") {
        process_rdelete(cx, cmd);
    } else if starts_with_ci(cmd, "return ") {
        *ret_val = process_return(cx, cmd);
    } else if starts_with_ci(cmd, "set ") {
        process_set(cx, cmd);
    } else if starts_with_ci(cmd, "unset ") {
        process_unset(cx, cmd);
    } else if starts_with_ci(cmd, "wait ") {
        let pulses = process_wait(cx, cmd, lines, cl);
        return Step::Yield(Outcome::Wait(pulses));
    } else if starts_with_ci(cmd, "attach ") {
        process_attach(cx, cmd);
    } else if starts_with_ci(cmd, "detach ") {
        process_detach(cx, cmd);
    } else {
        let owner = cx.owner;
        match cx.attach {
            AttachType::Mob => {
                if !crate::commands::script_command_interpreter(cx, cmd) {
                    cx.host.mud_command(owner, cmd);
                }
            }
            AttachType::Obj => crate::commands::obj_command_interpreter(cx, cmd),
            AttachType::Wld => crate::commands::wld_command_interpreter(cx, cmd),
        }
        if *cx.host.owner_purged_mut() {
            return Step::Yield(Outcome::Purged);
        }
        clear_if_detached(cx);
    }
    Step::Goto(cl)
}

/// `find_end`: scans for the `end` matching the `if` at `cl`. Returns the
/// `end` line, or the last line of the trigger if not found.
///
/// Iterative form of the C's recursion: `level` is the number of C
/// invocations currently active. An invocation that returns hands its `c` to
/// its caller, which then runs the `if(!c->next)` check.
pub fn find_end<H: BaseHost + ?Sized>(host: &mut H, vnum: i32, lines: &[Line], cl: usize) -> usize {
    let Some(last) = lines.len().checked_sub(1) else {
        return cl;
    };
    if cl >= last {
        // rryan: if this is the last line, theres no end.
        host.script_log(&format!(
            "Trigger VNum {vnum} has 'if' without 'end'. (error 1)"
        ));
        return cl;
    }
    let mut level = 1usize;
    let mut c = cl + 1;
    loop {
        let p = lines[c].body();
        let mut check_last = true;
        if starts_with_ci(p, "if ") {
            if c < last {
                level += 1;
                c += 1;
                continue;
            }
            // The inner call is on the last line: error 1, return c.
            host.script_log(&format!(
                "Trigger VNum {vnum} has 'if' without 'end'. (error 1)"
            ));
        } else if starts_with_ci(p, "end") {
            if level == 1 {
                return c;
            }
            level -= 1;
        } else if c < last {
            check_last = false;
        }
        if check_last {
            // Each active invocation that sees the last line logs and returns.
            if c >= last {
                loop {
                    host.script_log(&format!(
                        "Trigger VNum {vnum} has 'if' without 'end'. (error 2)"
                    ));
                    if level == 1 {
                        return c;
                    }
                    level -= 1;
                }
            }
        }
        c += 1;
    }
}

/// `find_else_end`: scans for a valid `elseif`, `else` or `end` to continue
/// at. Returns that line, or the last line of the trigger.
fn find_else_end<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, lines: &[Line], cl: usize) -> usize {
    let Some(last) = lines.len().checked_sub(1) else {
        return cl;
    };
    if cl >= last {
        return cl;
    }
    let vnum = cx.trig.proto.vnum;
    let mut c = cl + 1;
    while c < last {
        let p = lines[c].body();
        if starts_with_ci(p, "if ") {
            c = find_end(&mut *cx.host, vnum, lines, c);
        } else if starts_with_ci(p, "elseif ") {
            if process_if(cx, after(p, 7)) {
                cx.trig.run.depth = cx.trig.run.depth.wrapping_add(1);
                return c;
            }
        } else if starts_with_ci(p, "else") {
            cx.trig.run.depth = cx.trig.run.depth.wrapping_add(1);
            return c;
        } else if starts_with_ci(p, "end") {
            return c;
        }
        // rryan: this is the last line, return.
        if c >= last {
            vnum_log(cx, "has 'if' without 'end'. (error 4)");
            return c;
        }
        c += 1;
    }
    // rryan: if we got here, it's the last line, if its not an end, log it.
    if !starts_with_ci(lines[c].body(), "end") {
        vnum_log(cx, "has 'if' without 'end'. (error 5)");
    }
    c
}

/// `find_case`: scans for the `case`/`default` instance to enter. Returns that
/// line, or the last line of the trigger if not found.
fn find_case<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    lines: &[Line],
    cl: usize,
    cond: &str,
) -> usize {
    let result = eval_expr(cx, cond);
    let Some(last) = lines.len().checked_sub(1) else {
        return cl;
    };
    if cl >= last {
        return cl;
    }
    let mut c = cl + 1;
    while c < last {
        let p = lines[c].body();
        if starts_with_ci(p, "while ") || starts_with_ci(p, "switch") {
            c = find_done(lines, c);
        } else if starts_with_ci(p, "case ") {
            let matched = eval_op("==", &result, after(p, 5));
            if !matched.is_empty() && !matched.starts_with('0') {
                return c;
            }
        } else if starts_with_ci(p, "default") || starts_with_ci(p, "don") {
            return c;
        }
        c += 1;
    }
    // `c` is the last line, or past it when a nested find_done ran off the end
    // (where the C would dereference NULL).
    c.min(lines.len())
}

/// `find_done`: scans for the end of a `while`/`switch` block. Returns the
/// `done` line, the last line of the trigger, or `lines.len()` (the C's NULL
/// on a malformed script).
///
/// The C matches `done` on its first three letters only (`strn_cmp("done", p,
/// 3)`), so any line starting `don` ends the block; that is kept.
pub fn find_done(lines: &[Line], cl: usize) -> usize {
    let len = lines.len();
    if cl.saturating_add(1) >= len {
        return cl;
    }
    let mut level = 1usize;
    let mut c = cl + 1;
    loop {
        let result;
        if c.saturating_add(1) < len {
            let p = lines[c].body();
            if starts_with_ci(p, "while ") || starts_with_ci(p, "switch ") {
                level += 1;
                c += 1;
                continue;
            } else if starts_with_ci(p, "don") {
                result = c;
            } else {
                c += 1;
                continue;
            }
        } else {
            // The loop ended: `c` is the last line, or NULL.
            result = c.min(len);
        }
        if level == 1 {
            return result;
        }
        if result >= len {
            // The caller's `c = c->next` would dereference NULL.
            return len;
        }
        level -= 1;
        c = result + 1;
    }
}

/// `fields = sscanf(text, "%ld")`: skip whitespace, an optional sign, digits;
/// out-of-range values saturate like `strtol`. Returns the value and the rest.
fn scan_long(text: &str) -> Option<(i64, &str)> {
    let text = text.trim_start_matches(is_space);
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let mut value: i64 = 0;
    for byte in digits[..end].bytes() {
        let digit = i64::from(byte - b'0');
        value = if negative {
            value.saturating_mul(10).saturating_sub(digit)
        } else {
            value.saturating_mul(10).saturating_add(digit)
        };
    }
    Some((value, &digits[end..]))
}

/// `process_wait`: processes a `wait` command. Returns the number of pulses
/// to queue (the C's `event_create(…, when)`), after recording where to
/// resume; `None` (after logging) when the argument is unusable, in which case
/// no event exists and the trigger stays "running" forever, as in the C.
fn process_wait<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    cmd: &str,
    lines: &[Line],
    cl: usize,
) -> Option<i64> {
    let raw = lines.get(cl).map_or("", |line| line.cmd.as_str());
    let (_wait, rest) = any_one_arg(cmd);
    let arg = skip_spaces(rest);
    if arg.is_empty() {
        cx.log(&format!("wait w/o an arg: '{raw}'"));
        return None;
    }

    let when = if starts_with_ci(arg, "until ") {
        // valid forms of time are 14:30 and 1430. sscanf's literal "until" is
        // case-sensitive, unlike the strn_cmp above.
        let (hr, min) = scan_until(arg);
        let minutes = match (hr, min) {
            (Some(hr), Some(min)) => min.wrapping_add(hr.wrapping_mul(60)),
            (Some(hr), None) => (hr % 100).wrapping_add((hr / 100).wrapping_mul(60)),
            _ => {
                cx.log(&format!("wait until w/o a time: '{raw}'"));
                return None;
            }
        };
        let pulses_per_hour = SECS_PER_MUD_HOUR * PASSES_PER_SEC;
        // calculate the pulse of the day of "until" time
        let ntime = minutes.wrapping_mul(pulses_per_hour) / 60;
        // calculate pulse of day of current time
        let now = (cx.host.pulse() % pulses_per_hour)
            .wrapping_add(i64::from(cx.host.mud_time().hours).wrapping_mul(pulses_per_hour));
        if now >= ntime {
            // adjust for next day
            (SECS_PER_MUD_DAY * PASSES_PER_SEC)
                .wrapping_sub(now)
                .wrapping_add(ntime)
        } else {
            ntime.wrapping_sub(now)
        }
    } else {
        // sscanf(arg, "%ld %c"): one field is 'wait 30'; two is 'wait 30 s'.
        let Some((mut when, tail)) = scan_long(arg) else {
            cx.log(&format!("wait w/o a number: '{raw}'"));
            return None;
        };
        match tail.trim_start_matches(is_space).chars().next() {
            Some('t') => when = when.wrapping_mul(PULSES_PER_MUD_HOUR),
            Some('s') => when = when.wrapping_mul(PASSES_PER_SEC),
            _ => {}
        }
        when
    };
    cx.trig.run.curr_state = cl.saturating_add(1);
    Some(when)
}

/// `sscanf(arg, "until %ld:%ld")` falling back to `"until %ld"`: the hour (or
/// the whole number) and, if a `:` and a second number follow, the minutes.
fn scan_until(arg: &str) -> (Option<i64>, Option<i64>) {
    let Some(rest) = arg.strip_prefix("until") else {
        return (None, None);
    };
    let Some((hr, rest)) = scan_long(rest) else {
        return (None, None);
    };
    let min = rest
        .strip_prefix(':')
        .and_then(scan_long)
        .map(|(min, _)| min);
    (Some(hr), min)
}

/// `process_set`: `set <name> <value>`.
fn process_set<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_set, name, value) = two_arguments(cmd);
    let value = skip_spaces(value);
    if name.is_empty() {
        cx.log(&format!("set w/o an arg: '{cmd}'"));
        return;
    }
    let context = cx.context();
    cx.trig.vars.add_var(&name, value, context);
}

/// `process_unset`: removes a variable from the globals of the script, or the
/// trigger's locals if not found there.
fn process_unset<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_unset, rest) = any_one_arg(cmd);
    let var = skip_spaces(rest);
    if var.is_empty() {
        cx.log(&format!("unset w/o an arg: '{cmd}'"));
        return;
    }
    let owner = cx.owner;
    let removed = cx
        .host
        .script_mut(owner)
        .is_some_and(|script| script.global_vars.remove_var(var));
    if !removed {
        cx.trig.vars.remove_var(var);
    }
}

/// `process_global`: makes a local variable into a global variable.
fn process_global<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str, id: i64) {
    let (_global, rest) = any_one_arg(cmd);
    let var = skip_spaces(rest);
    if var.is_empty() {
        cx.log(&format!("global w/o an arg: '{cmd}'"));
        return;
    }
    let Some(found) = cx.trig.vars.get(var).cloned() else {
        cx.log(&format!("local var '{var}' not found in global call"));
        return;
    };
    let owner = cx.owner;
    // The C dereferences `sc` here; a detached script has nowhere to put it.
    let Some(script) = cx.host.script_mut(owner) else {
        return;
    };
    script.global_vars.add_var(&found.name, &found.value, id);
    cx.trig.vars.remove_var(&found.name);
}

/// `process_context`: sets the current context for the script.
fn process_context<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_context, rest) = any_one_arg(cmd);
    let var = skip_spaces(rest);
    if var.is_empty() {
        cx.log(&format!("context w/o an arg: '{cmd}'"));
        return;
    }
    let owner = cx.owner;
    if let Some(script) = cx.host.script_mut(owner) {
        script.context = atol(var);
    }
}

/// The room, character or object a `remote`/`rdelete` uid names, in the C's
/// order of trying.
fn remote_target<H: DgHost + ?Sized>(cx: &Cx<'_, H>, uid: i64) -> Option<Uid> {
    let host = &*cx.host;
    find_room(host, uid)
        .or_else(|| find_char(host, uid))
        .or_else(|| find_obj(host, uid))
}

/// `process_remote`: copies a locally owned variable to the globals of
/// another script. `remote <variable_name> <uid>`.
fn process_remote<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_remote, line) = any_one_arg(cmd);
    let (var, uid_text, _) = two_arguments(line);
    if var.is_empty() || uid_text.is_empty() {
        cx.log(&format!("remote: invalid arguments '{cmd}'"));
        return;
    }

    // find the locally owned variable
    let mut found = cx.trig.vars.get(&var).cloned();
    if found.is_none() {
        found = cx
            .host
            .script(cx.owner)
            .and_then(|script| script.global_vars.get_in_context(&var, script.context))
            .cloned();
    }
    let Some(found) = found else {
        cx.log(&format!("local var '{var}' not found in remote call"));
        return;
    };

    // find the target script from the uid number
    let uid = i64::from(atoi(&uid_text));
    if uid <= 0 {
        cx.log(&format!("remote: illegal uid '{uid_text}'"));
        return;
    }
    let Some(target) = remote_target(cx, uid) else {
        cx.log(&format!("remote: uid '{uid}' invalid"));
        return;
    };
    // For all but PC's, context comes from the existing context. For PC's,
    // context is 0 (global).
    let mut context = found.context;
    if cx.host.kind(target) == Some(Kind::Char) && !cx.host.is_npc(target) {
        context = 0;
    }
    // no script to assign
    if let Some(remote) = cx.host.script_mut(target) {
        remote
            .global_vars
            .add_var(&found.name, &found.value, context);
    }
}

/// `process_rdelete`: deletes a variable from the globals of another script.
/// `rdelete <variable_name> <uid>`.
fn process_rdelete<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_rdelete, line) = any_one_arg(cmd);
    let (var, uid_text, _) = two_arguments(line);
    if var.is_empty() || uid_text.is_empty() {
        cx.log(&format!("rdelete: invalid arguments '{cmd}'"));
        return;
    }
    let uid = i64::from(atoi(&uid_text));
    if uid <= 0 {
        cx.log(&format!("rdelete: illegal uid '{uid_text}'"));
        return;
    }
    let Some(target) = remote_target(cx, uid) else {
        cx.log(&format!("remote: uid '{uid}' invalid"));
        return;
    };
    let context = cx.context();
    // no script, or no script globals
    let Some(remote) = cx.host.script_mut(target) else {
        return;
    };
    let found = remote
        .global_vars
        .vars
        .iter()
        .position(|vd| str_eq(&vd.name, &var) && (vd.context == 0 || vd.context == context));
    if let Some(index) = found {
        remote.global_vars.vars.remove(index);
    }
}

/// `process_return`: the new value for the script to return.
fn process_return<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) -> i32 {
    let (_return, value, _) = two_arguments(cmd);
    if value.is_empty() {
        cx.log(&format!("return w/o an arg: '{cmd}'"));
        return 1;
    }
    atoi(&value)
}

/// What `attach`/`detach` name: the trigger argument and the entity.
fn script_target<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    cmd: &str,
    verb: &str,
) -> Option<(String, Uid)> {
    let (_verb, trignum, id_p) = two_arguments(cmd);
    let id_p = skip_spaces(id_p);
    if trignum.is_empty() {
        cx.log(&format!("{verb} w/o an arg: '{cmd}'"));
        return None;
    }
    if id_p.is_empty() || atoi(id_p) == 0 {
        cx.log(&format!("{verb} invalid id arg: '{cmd}'"));
        return None;
    }
    // parse and locate the id specified
    let result = eval_expr(cx, id_p);
    let id = i64::from(atoi(&result));
    if id == 0 {
        cx.log(&format!("{verb} invalid id arg: '{cmd}'"));
        return None;
    }
    let host = &*cx.host;
    let target = find_char(host, id)
        .or_else(|| find_obj(host, id))
        .or_else(|| find_room(host, id));
    match target {
        Some(target) => Some((trignum, target)),
        None => {
            cx.log(&format!("{verb} invalid id arg: '{cmd}'"));
            None
        }
    }
}

/// `process_attach`: a script attaching a trigger to something.
fn process_attach<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let Some((trignum, target)) = script_target(cx, cmd, "attach") else {
        return;
    };
    // locate and load the trigger specified
    let Some(proto) = cx.host.trig_proto(atoi(&trignum)) else {
        cx.log(&format!("attach invalid trigger: '{trignum}'"));
        return;
    };
    if cx.host.kind(target) == Some(Kind::Char)
        && !cx.host.is_npc(target)
        && !cx.host.script_players()
    {
        let name = cx.host.name(target);
        cx.log(&format!("attach invalid target: '{name}'"));
        return;
    }
    if let Some(script) = cx.host.ensure_script(target) {
        script.add_trigger(proto, -1);
    }
}

/// `process_detach`: a script detaching a trigger from something.
fn process_detach<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let Some((trignum, target)) = script_target(cx, cmd, "detach") else {
        return;
    };
    if cx.host.script(target).is_none() {
        return;
    }
    if trignum == "all" {
        extract_script(&mut *cx.host, target);
    } else if remove_trigger(&mut *cx.host, target, &trignum).is_some() {
        let empty = cx
            .host
            .script(target)
            .is_some_and(|script| script.triggers.is_empty());
        if empty {
            extract_script(&mut *cx.host, target);
        }
    }
    clear_if_detached(cx);
}

/// A trigger that lost its slot while it ran (detached, or its script
/// extracted) carries on with its variables gone: the C freed the trigger.
fn clear_if_detached<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>) {
    let alive = cx
        .host
        .script(cx.owner)
        .and_then(|script| script.trigger(cx.trig.id))
        .is_some_and(|slot| slot.checked_out);
    if !alive {
        cx.trig.vars.clear();
    }
}

/// A trigger `remove_trigger` took off a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub id: u64,
    pub vnum: i32,
    pub name: String,
}

impl Removed {
    /// The C's `removed` text: `"%d (%s)"`.
    pub fn describe(&self) -> String {
        format!("{} ({})", self.vnum, self.name)
    }
}

/// `remove_trigger(sc, name, …)` (`dg_scripts.c`): removes the trigger `name`
/// names from the owner's script. `name` is a position (`2`), a vnum, a
/// keyword of the trigger's name (`guard`) or `<n>.<keyword>` for the nth
/// such. Cancels the trigger's wait. `None` when nothing matched or the owner
/// has no script.
///
/// A numeric `name` matches the nth trigger by position, and otherwise a
/// trigger by vnum, as in the C. It does **not** extract the script when the
/// list empties; the callers do.
pub fn remove_trigger<H: BaseHost + ?Sized>(
    host: &mut H,
    owner: Uid,
    name: &str,
) -> Option<Removed> {
    let script = host.script(owner)?;
    let (string, num, name) = match name.split_once('.') {
        Some((number, rest)) => (true, atoi(number), rest),
        None if !name.starts_with(|c: char| c.is_ascii_digit()) => (true, 0, name),
        None => (false, atoi(name), name),
    };
    let mut n: i32 = 0;
    let mut found = None;
    for (index, trigger) in script.triggers.iter().enumerate() {
        if string {
            if isname(name, &trigger.proto.name) {
                n = n.wrapping_add(1);
                if n >= num {
                    found = Some(index);
                    break;
                }
            }
        } else {
            // A numeric value matches its position or its vnum.
            n = n.wrapping_add(1);
            if n >= num || trigger.proto.vnum == num {
                found = Some(index);
                break;
            }
        }
    }
    let index = found?;
    let trigger = script.triggers.get(index)?;
    let removed = Removed {
        id: trigger.id,
        vnum: trigger.proto.vnum,
        name: trigger.proto.name.clone(),
    };
    host.cancel_wait(owner, removed.id);
    host.script_mut(owner)?.triggers.remove(index);
    Some(removed)
}

/// `extract_script(thing, type)` (`dg_handler.c`): remove an entity's script
/// entirely — its triggers (cancelling their waits), its variables and the
/// script itself. Does nothing when there is none.
pub fn extract_script<H: BaseHost + ?Sized>(host: &mut H, owner: Uid) {
    let ids = host
        .script(owner)
        .map(Script::trigger_ids)
        .unwrap_or_default();
    for id in ids {
        host.cancel_wait(owner, id);
    }
    host.remove_script(owner);
}

/// Truncate to what a `size`-byte C buffer holds (`size - 1` bytes), on a
/// character boundary.
fn truncate_c(text: &mut String, size: usize) {
    let mut end = text.len().min(size.saturating_sub(1));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

/// `makeuid_var`: `makeuid <var> <id>` or `makeuid <var> mob|obj|room <name>`.
fn makeuid_var<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    // The C chops `cmd` in place, so its log messages show what is left.
    let (_makeuid, rest) = half_chop(cmd);
    let (varname, rest) = half_chop(rest);
    let (arg, rest) = half_chop(rest);
    let (name, rest) = half_chop(rest);
    if varname.is_empty() {
        cx.log(&format!("makeuid w/o an arg: '{rest}'"));
        return;
    }
    if arg.is_empty() {
        cx.log(&format!("makeuid invalid id arg: '{rest}'"));
        return;
    }
    let uid = if atoi(&arg) != 0 {
        // easy, if you pass an id number
        let result = eval_expr(cx, &arg);
        let mut uid = format!("{UID_CHAR}{result}");
        truncate_c(&mut uid, MAX_INPUT_LENGTH + 1);
        uid
    } else {
        // a lot more work without it
        if name.is_empty() {
            cx.log(&format!("makeuid needs name: '{rest}'"));
            return;
        }
        let owner = cx.owner;
        let host = &*cx.host;
        let found = if is_abbrev(&arg, "mob") {
            match cx.attach {
                AttachType::Wld => get_char_in_room(host, owner, &name),
                AttachType::Obj => get_char_near_obj(host, owner, &name),
                AttachType::Mob => get_char_room_vis(host, owner, &name),
            }
        } else if is_abbrev(&arg, "obj") {
            match cx.attach {
                AttachType::Wld => get_obj_in_room(host, owner, &name),
                AttachType::Obj => get_obj_near_obj(host, owner, &name),
                AttachType::Mob => get_obj_in_list_vis(host, owner, &name, &host.carrying(owner))
                    .or_else(|| {
                        // The C's first lookup strips any `N.` prefix from the
                        // shared buffer, so the room search sees the bare name.
                        let bare = get_number(&name).1;
                        host.in_room(owner).and_then(|room| {
                            get_obj_in_list_vis(host, owner, bare, &host.room_contents(room))
                        })
                    }),
            }
        } else if is_abbrev(&arg, "room") {
            match cx.attach {
                AttachType::Wld => Some(owner),
                AttachType::Obj => obj_room(host, owner),
                AttachType::Mob => host.in_room(owner),
            }
        } else {
            cx.log(&format!("makeuid syntax error: '{rest}'"));
            return;
        };
        found.map(uid_text).unwrap_or_default()
    };
    if !uid.is_empty() {
        let context = cx.context();
        cx.trig.vars.add_var(&varname, &uid, context);
    }
}

/// `extract_value`: `extract <var> <n> <words…>` stores the nth word.
fn extract_value<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_extract, rest) = any_one_arg(cmd);
    let (to, buf) = half_chop(rest);
    let num = atoi(buf);
    if num < 1 {
        cx.host.script_log("extract number < 1!");
        return;
    }
    let (_number, mut words) = half_chop(buf);
    let mut word = String::new();
    for _ in 0..num {
        if words.is_empty() {
            // Every further half_chop yields an empty word.
            word.clear();
            break;
        }
        let (next, rest) = half_chop(words);
        if next.is_empty() && rest == words {
            // No progress (e.g. the text starts with a tab): every further
            // half_chop yields the same empty word.
            word.clear();
            break;
        }
        word = next;
        words = rest;
    }
    let context = cx.context();
    cx.trig.vars.add_var(&to, &word, context);
}

/// `dg_letter_value`: `dg_letter <var> <n> <string>` stores the nth letter.
fn dg_letter_value<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_dg_letter, rest) = half_chop(cmd);
    let (varname, rest) = half_chop(rest);
    let (num_s, string) = half_chop(rest);
    let num = atoi(&num_s);
    cx.host.script_log("The use of dg_letter is deprecated");
    cx.host
        .script_log("- Use 'set <new variable> %<text/var>.charat(index)%' instead.");
    let vnum = cx.trig.proto.vnum;
    if num < 1 {
        cx.host
            .script_log(&format!("Trigger #{vnum} : dg_letter number < 1!"));
        return;
    }
    let index = usize::try_from(num - 1).unwrap_or(0);
    if index >= string.len() {
        cx.host
            .script_log(&format!("Trigger #{vnum} : dg_letter number > strlen!"));
        return;
    }
    let letter = match string.get(index..).and_then(|tail| tail.chars().next()) {
        Some(c) => c.to_string(),
        // Inside a multi-byte character; the C copies one raw byte.
        None => "\u{fffd}".to_owned(),
    };
    let context = cx.context();
    cx.trig.vars.add_var(&varname, &letter, context);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::host::WorldHost;
    use crate::mock::MockHost;
    use crate::script::Program;
    use crate::types::{ROOM_ID_BASE, TrigInstance};

    /// A mob (vnum 100, "mob") in room 1 with trigger 5 attached.
    fn world(script: &str) -> (MockHost, Uid, u64) {
        let mut host = MockHost::new();
        let room = host.add_room(1, "Room");
        let mob = host.add_mob(100, "mob");
        host.char_to_room(mob, room);
        let proto = host.add_proto(5, AttachType::Mob, "g", 100, "", script);
        let id = host.attach(mob, &proto).unwrap();
        (host, mob, id)
    }

    fn run(host: &mut MockHost, owner: Uid, id: u64) -> i32 {
        script_driver(host, owner, id, Mode::New)
    }

    fn cmds(host: &MockHost) -> Vec<String> {
        host.mud_commands.iter().map(|(_, c)| c.clone()).collect()
    }

    fn count_logs(host: &MockHost, needle: &str) -> usize {
        host.logs.iter().filter(|l| l.contains(needle)).count()
    }

    fn run_script(script: &str) -> (MockHost, Uid, u64) {
        let (mut host, mob, id) = world(script);
        run(&mut host, mob, id);
        (host, mob, id)
    }

    fn state(host: &MockHost, owner: Uid, id: u64) -> RunState {
        host.script(owner).unwrap().trigger(id).unwrap().run.clone()
    }

    fn lines_of(script: &str) -> Vec<Line> {
        Program::parse(script).lines
    }

    #[test]
    fn plain_lines_are_substituted_and_dispatched_to_the_mob() {
        let (host, mob, id) = run_script("set x hello\nsay %x%\n* say no\n   * say no\nsay yes");
        assert_eq!(
            host.mud_commands,
            vec![(mob, "say hello".to_owned()), (mob, "say yes".to_owned())]
        );
        // End of run: variables freed, depth 0, instance returned.
        let trigger = host.script(mob).unwrap().trigger(id).unwrap();
        assert!(trigger.vars.is_empty());
        assert_eq!(trigger.run.depth, 0);
        assert_eq!(host.depth, 0);
    }

    #[test]
    fn empty_script_and_default_return() {
        let (mut host, mob, id) = world("");
        assert_eq!(run(&mut host, mob, id), 1);
        assert_eq!(script_driver_default(&mut host, mob, id, Mode::New, 7), 7);
        // No such owner or instance: the default, no panic.
        assert_eq!(script_driver_default(&mut host, 999, id, Mode::New, 3), 3);
        assert_eq!(script_driver_default(&mut host, mob, 99, Mode::New, 4), 4);
    }

    #[test]
    fn set_uses_two_arguments_so_fill_words_vanish() {
        // one_argument skips "from": the name is "hello", the value "world".
        let (host, _, _) = run_script("set from hello world\nsay %hello%");
        assert_eq!(cmds(&host), vec!["say world"]);
        // set with no name logs and sets nothing.
        let (host, _, _) = run_script("set \nsay [%x%]");
        assert_eq!(count_logs(&host, "set w/o an arg: 'set '"), 1);
        assert_eq!(cmds(&host), vec!["say []"]);
        // The name is lowercased, lookups ignore case.
        let (host, _, _) = run_script("set FOO bar\nsay %foo%");
        assert_eq!(cmds(&host), vec!["say bar"]);
    }

    #[test]
    fn if_elseif_else_pick_one_branch() {
        let body = |a: &str, b: &str| {
            format!(
                "set a {a}\nset b {b}\nif %a%\nsay one\nelseif %b%\nsay two\nelse\nsay three\nend\nsay after"
            )
        };
        assert_eq!(
            cmds(&run_script(&body("1", "1")).0),
            vec!["say one", "say after"]
        );
        assert_eq!(
            cmds(&run_script(&body("0", "1")).0),
            vec!["say two", "say after"]
        );
        assert_eq!(
            cmds(&run_script(&body("0", "0")).0),
            vec!["say three", "say after"]
        );
        // Empty and leading-zero conditions are false.
        assert_eq!(
            cmds(&run_script(&body("", "07")).0),
            vec!["say three", "say after"]
        );
    }

    #[test]
    fn nested_ifs_are_skipped_whole() {
        let (host, mob, id) =
            run_script("if 0\nif 1\nsay no\nend\nsay no2\nelse\nsay yes\nend\nsay after");
        assert_eq!(cmds(&host), vec!["say yes", "say after"]);
        assert!(host.logs.is_empty(), "{:?}", host.logs);
        assert_eq!(state(&host, mob, id).depth, 0);
        // A taken branch skips the else and its nested if.
        let (host, _, _) = run_script("if 1\nsay a\nelse\nif 1\nsay no\nend\nend\nsay z");
        assert_eq!(cmds(&host), vec!["say a", "say z"]);
        assert!(host.logs.is_empty(), "{:?}", host.logs);
    }

    #[test]
    fn stray_else_and_end_are_logged_and_ignored() {
        let (host, _, _) = run_script("else\nsay a\nend\nsay b\nelseif 1");
        assert_eq!(cmds(&host), vec!["say a", "say b"]);
        assert_eq!(
            count_logs(&host, "Trigger VNum 5 has 'else' without 'if'."),
            2
        );
        assert_eq!(
            count_logs(&host, "Trigger VNum 5 has 'end' without 'if'."),
            1
        );
    }

    #[test]
    fn keyword_prefixes_match_like_strn_cmp() {
        // "endgame" starts with "end"; "elsewhere" with "else": the C treats
        // both as keywords. "iffy" has no space after "if" and is a command.
        let (host, _, _) = run_script("iffy\nendgame\nelsewhere\nsay z");
        assert_eq!(cmds(&host), vec!["iffy", "say z"]);
        assert_eq!(count_logs(&host, "'end' without 'if'"), 1);
        assert_eq!(count_logs(&host, "'else' without 'if'"), 1);
        // IF is case-insensitive.
        let (host, _, _) = run_script("IF 1\nsay a\nEND");
        assert_eq!(cmds(&host), vec!["say a"]);
    }

    #[test]
    fn missing_end_is_logged_like_the_c() {
        // Failed if, no end: find_else_end reaches the last line (error 5).
        let (host, _, _) = run_script("if 0\nsay a");
        assert!(host.mud_commands.is_empty());
        assert_eq!(
            count_logs(&host, "Trigger VNum 5 has 'if' without 'end'. (error 5)"),
            1
        );
        // The if on the last line: find_else_end returns at once, no log.
        let (host, _, _) = run_script("say a\nif 0");
        assert!(host.logs.is_empty());
        // Last line is the end: no log.
        let (host, _, _) = run_script("if 0\nsay a\nend");
        assert!(host.logs.is_empty());
        // Failed if whose nested if swallows the last line: error 4.
        let (host, _, _) = run_script("if 0\nif 1\nend");
        assert_eq!(count_logs(&host, "(error 4)"), 1);
        // Passing if, else, then nothing: find_end hits the last line (error 2).
        let (host, _, _) = run_script("if 1\nsay a\nelse\nsay b");
        assert_eq!(cmds(&host), vec!["say a"]);
        assert_eq!(count_logs(&host, "(error 2)"), 1);
        // else on the last line: find_end(cl) with no next line (error 1).
        let (host, _, _) = run_script("if 1\nsay a\nelse");
        assert_eq!(count_logs(&host, "(error 1)"), 1);
    }

    #[test]
    fn find_end_logs_once_per_active_invocation() {
        // else -> find_end; the nested `if` inside it is unterminated: the
        // inner call logs error 2, and so does the outer call it returns to.
        let (host, _, _) = run_script("if 1\nsay a\nelse\nif 1\nsay b");
        assert_eq!(count_logs(&host, "(error 2)"), 2);
        // The nested if is the last line: inner error 1, outer error 2.
        let (host, _, _) = run_script("if 1\nsay a\nelse\nif 2");
        assert_eq!(count_logs(&host, "(error 1)"), 1);
        assert_eq!(count_logs(&host, "(error 2)"), 1);
        let one = host
            .logs
            .iter()
            .position(|l| l.contains("(error 1)"))
            .unwrap();
        let two = host
            .logs
            .iter()
            .position(|l| l.contains("(error 2)"))
            .unwrap();
        assert!(one < two);
    }

    #[test]
    fn find_end_matches_nested_pairs() {
        let lines = lines_of("if 1\nif 2\nend\nif 3\nend\nend\nsay z");
        let mut host = MockHost::new();
        assert_eq!(find_end(&mut host, 5, &lines, 0), 5);
        assert_eq!(find_end(&mut host, 5, &lines, 1), 2);
        assert_eq!(find_end(&mut host, 5, &lines, 3), 4);
        assert!(host.logs.is_empty());
        // An end followed by a last line that is not an end.
        assert_eq!(find_end(&mut host, 5, &lines, 6), 6);
        assert_eq!(host.logs.len(), 1);
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let depth = 100_000;
        let mut script = String::from("if 1\nsay a\nelse\n");
        for _ in 0..depth {
            script.push_str("if 1\n");
        }
        let (host, mob, id) = run_script(&script);
        assert_eq!(cmds(&host), vec!["say a"]);
        assert_eq!(count_logs(&host, "(error 2)"), depth);
        assert_eq!(state(&host, mob, id).depth, 0);

        let lines = lines_of(&"while 1\n".repeat(depth));
        // Every while is nested and none is closed: NULL, i.e. len.
        assert_eq!(find_done(&lines, 0), lines.len());
    }

    #[test]
    fn while_runs_until_the_condition_fails() {
        let (host, mob, id) = run_script("set go 1\nwhile %go%\nsay tick\nset go 0\ndone\nsay end");
        assert_eq!(cmds(&host), vec!["say tick", "say end"]);
        assert!(host.logs.is_empty());
        // The done line remembers the while it closes.
        assert_eq!(state(&host, mob, id).originals.get(&4), Some(&1));
    }

    #[test]
    fn while_false_from_the_start_skips_the_body() {
        let (host, mob, id) = run_script("while 0\nsay a\ndone\nsay b");
        assert_eq!(cmds(&host), vec!["say b"]);
        assert!(state(&host, mob, id).originals.is_empty());
    }

    #[test]
    fn endless_while_yields_every_thirty_loops_and_stops_at_a_hundred() {
        let (mut host, mob, id) = world("while 1\nsay tick\ndone\nsay never");
        run(&mut host, mob, id);
        assert_eq!(cmds(&host).len(), 30);
        assert_eq!(host.waits, vec![(mob, id, 1)]);
        assert_eq!(state(&host, mob, id).depth, 1);
        assert_eq!(state(&host, mob, id).curr_state, 1);

        for total in [60, 90] {
            host.waits.clear();
            assert_eq!(resume(&mut host, mob, id), 0);
            assert_eq!(cmds(&host).len(), total);
            assert_eq!(host.waits, vec![(mob, id, 1)]);
        }
        host.waits.clear();
        resume(&mut host, mob, id);
        // Loop 100: no wait, a log, and the run is over.
        assert_eq!(cmds(&host).len(), 100);
        assert!(host.waits.is_empty());
        assert_eq!(
            count_logs(&host, "Trigger VNum 5 has looped 100 times!!!"),
            1
        );
        assert!(!cmds(&host).contains(&"say never".to_owned()));
        assert_eq!(state(&host, mob, id).depth, 0);
        assert_eq!(host.depth, 0);
        // The instance can run again from the top. `cl->loops` of the while
        // line was left at 10 by the run that hit the cap (only a false
        // condition resets it), so the next wait comes after 20 more loops.
        run(&mut host, mob, id);
        assert_eq!(cmds(&host).len(), 120);
    }

    #[test]
    fn loop_counters_are_per_instance() {
        let mut host = MockHost::new();
        let room = host.add_room(1, "Room");
        let proto = host.add_proto(5, AttachType::Mob, "g", 100, "", "while 1\nsay tick\ndone");
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mob = host.add_mob(100, "mob");
            host.char_to_room(mob, room);
            ids.push((mob, host.attach(mob, &proto).unwrap()));
        }
        for (mob, id) in &ids {
            run(&mut host, *mob, *id);
        }
        // Each hit its own 30th loop; neither disturbed the other.
        assert_eq!(host.mud_commands.len(), 60);
        assert_eq!(host.waits.len(), 2);
    }

    #[test]
    fn while_without_done_is_logged_when_the_scan_runs_off_the_end() {
        let (host, _, _) = run_script("while 1\nwhile 1\nsay a");
        assert!(host.mud_commands.is_empty());
        assert_eq!(
            count_logs(&host, "Trigger VNum 5 has 'while' without 'done'."),
            1
        );
        // A single unclosed while finds the last line instead and runs.
        let (host, _, _) = run_script("while 1\nsay a");
        assert_eq!(cmds(&host), vec!["say a"]);
        // A while on the very last line is its own done.
        let (host, _, _) = run_script("say a\nwhile 1");
        assert_eq!(cmds(&host), vec!["say a"]);
    }

    #[test]
    fn find_done_matches_nested_blocks_and_three_letter_done() {
        let lines = lines_of("while 1\nswitch 2\ncase 1\ndone\nsay x\ndone\nsay z");
        assert_eq!(find_done(&lines, 0), 5);
        assert_eq!(find_done(&lines, 1), 3);
        // The last line is returned, never examined.
        assert_eq!(find_done(&lines, 5), 6);
        assert_eq!(find_done(&lines, 6), 6);
        // `don` is enough.
        let lines = lines_of("while 1\ndonkey\nsay z");
        assert_eq!(find_done(&lines, 0), 1);
        assert_eq!(find_done(&[], 0), 0);
    }

    #[test]
    fn switch_runs_the_matching_case_and_break_leaves() {
        let script = |x: &str| {
            format!(
                "set x {x}\nswitch %x%\ncase a\nsay A\nbreak\ncase b\nsay B\nbreak\ndefault\nsay D\ndone\nsay after"
            )
        };
        assert_eq!(
            cmds(&run_script(&script("b")).0),
            vec!["say B", "say after"]
        );
        assert_eq!(
            cmds(&run_script(&script("A")).0),
            vec!["say A", "say after"]
        );
        // find_case returns the `default` line and the loop's `cl = cl->next`
        // steps past it.
        assert_eq!(
            cmds(&run_script(&script("z")).0),
            vec!["say D", "say after"]
        );
        // Reached by falling through, `default` is not a keyword of the main
        // loop: it is dispatched as a command.
        assert_eq!(
            cmds(&run_script("switch a\ncase a\nsay A\ndefault\nsay D\ndone").0),
            vec!["say A", "default", "say D"]
        );
    }

    #[test]
    fn switch_falls_through_without_break() {
        let (host, _, _) =
            run_script("switch a\ncase a\nsay A\ncase b\nsay B\nbreak\ncase c\nsay C\ndone\nsay z");
        assert_eq!(cmds(&host), vec!["say A", "say B", "say z"]);
    }

    #[test]
    fn switch_skips_nested_loops_and_switches_when_searching() {
        let (host, _, _) = run_script(
            "switch b\ncase a\nwhile 1\ncase b\nsay no\ndone\nswitch q\ncase b\nsay no\ndone\ncase b\nsay yes\nbreak\ndone\nsay z",
        );
        assert_eq!(cmds(&host), vec!["say yes", "say z"]);
    }

    #[test]
    fn switch_with_no_match_ends_at_the_last_line() {
        // No case matches: find_case returns the last line, so the script
        // ends without running it.
        let (host, _, _) = run_script("switch x\ncase a\nsay A\nsay last");
        assert!(host.mud_commands.is_empty());
        // A `done` line stops the search and then executes harmlessly.
        let (host, _, _) = run_script("switch x\ncase a\nsay A\ndone\nsay z");
        assert_eq!(cmds(&host), vec!["say z"]);
        // A switch on the last line returns itself.
        let (host, _, _) = run_script("say a\nswitch x");
        assert_eq!(cmds(&host), vec!["say a"]);
        // A nested block that runs off the end ends the script (the C
        // dereferences NULL).
        let (host, _, _) = run_script("switch x\nwhile 1\nwhile 1\nsay a");
        assert!(host.mud_commands.is_empty());
    }

    #[test]
    fn break_outside_a_loop_runs_off_the_end_without_crashing() {
        let (host, _, _) = run_script("say a\nbreak\nsay b\nsay c");
        assert_eq!(cmds(&host), vec!["say a"]);
        let (host, _, _) = run_script("while 1\nwhile 1\nbreak");
        assert!(host.mud_commands.is_empty());
    }

    #[test]
    fn wait_forms_and_units() {
        let cases = [
            ("wait 5", Some(5)),
            ("wait   12  ", Some(12)),
            ("wait 2 s", Some(20)),
            ("wait 2s", Some(20)),
            ("wait 1 t", Some(750)),
            ("wait 3 x", Some(3)),
            ("wait -5", Some(-5)),
            ("wait +4 s", Some(40)),
            ("wait 7abc", Some(7)),
            ("WAIT 6", Some(6)),
            ("wait 0", Some(0)),
            ("wait 99999999999999999999", Some(i64::MAX)),
            ("wait -99999999999999999999", Some(i64::MIN)),
            (
                "wait 99999999999999999999 s",
                Some(i64::MAX.wrapping_mul(10)),
            ),
            ("wait abc", None),
            ("wait é", None),
            ("wait -", None),
            ("wait ", None),
        ];
        for (line, expected) in cases {
            let (host, mob, id) = run_script(&format!("{line}\nsay after"));
            match expected {
                Some(pulses) => assert_eq!(host.waits, vec![(mob, id, pulses)], "{line}"),
                None => assert!(host.waits.is_empty(), "{line}"),
            }
            assert!(host.mud_commands.is_empty(), "{line}");
            // Waiting or stuck: the trigger stays active either way.
            assert!(state(&host, mob, id).is_active(), "{line}");
            assert_eq!(host.depth, 0);
        }
    }

    #[test]
    fn bad_wait_logs_and_leaves_the_trigger_stuck_as_in_the_c() {
        let (mut host, mob, id) = world("wait abc");
        run(&mut host, mob, id);
        assert_eq!(
            count_logs(
                &host,
                "Trigger: trigger 5, VNum 5. wait w/o a number: 'wait abc'"
            ),
            1
        );
        assert_eq!(state(&host, mob, id).depth, 1);
        assert!(host.script(mob).unwrap().trigger(id).unwrap().is_busy());
        let (host, _, _) = run_script("wait ");
        assert_eq!(count_logs(&host, "wait w/o an arg: 'wait '"), 1);
    }

    #[test]
    fn wait_resumes_after_itself_with_variables_and_nesting_intact() {
        let (mut host, mob, id) = world("set x 1\nif 1\nwait 4\nsay in %x%\nend\nsay out");
        run(&mut host, mob, id);
        assert!(host.mud_commands.is_empty());
        assert_eq!(host.waits, vec![(mob, id, 4)]);
        assert_eq!(state(&host, mob, id).depth, 2);
        assert_eq!(state(&host, mob, id).curr_state, 3);
        assert_eq!(resume(&mut host, mob, id), 0);
        assert_eq!(cmds(&host), vec!["say in 1", "say out"]);
        assert_eq!(state(&host, mob, id).depth, 0);
        assert!(
            host.script(mob)
                .unwrap()
                .trigger(id)
                .unwrap()
                .vars
                .is_empty()
        );
    }

    #[test]
    fn wait_as_the_last_line_resumes_into_nothing() {
        let (mut host, mob, id) = world("set x 1\nwait 1");
        run(&mut host, mob, id);
        assert_eq!(state(&host, mob, id).curr_state, 2);
        resume(&mut host, mob, id);
        assert_eq!(state(&host, mob, id).depth, 0);
        assert!(
            host.script(mob)
                .unwrap()
                .trigger(id)
                .unwrap()
                .vars
                .is_empty()
        );
    }

    #[test]
    fn wait_until_computes_pulses_to_the_time_of_day() {
        // 14:30 is 870 minutes; a mud hour is 750 pulses, so 10875 pulses
        // into the day. It is hour 0, pulse 0 now.
        for line in ["wait until 14:30", "wait until 1430", "wait until  14:30"] {
            let (host, mob, id) = run_script(line);
            assert_eq!(host.waits, vec![(mob, id, 10_875)], "{line}");
        }
        // "14 : 30" scans only the 14: 14 % 100 minutes.
        let (host, mob, id) = run_script("wait until 14 : 30");
        assert_eq!(host.waits, vec![(mob, id, 175)]);
        // "until 14:" has one field; the second form reads just 14.
        let (host, mob, id) = run_script("wait until 14:");
        assert_eq!(host.waits, vec![(mob, id, 175)]);

        // Already past it: wait for tomorrow. Hour 15 and pulse 100 is
        // 100 % 750 + 15 * 750 = 11350 into the day: 18000 - 11350 + 10875.
        let (mut host, mob, id) = world("wait until 14:30");
        host.pulse = 100;
        host.time.hours = 15;
        run(&mut host, mob, id);
        assert_eq!(host.waits, vec![(mob, id, 17_525)]);
        // Exactly now counts as passed.
        let (mut host, mob, id) = world("wait until 0:00");
        run(&mut host, mob, id);
        assert_eq!(host.waits, vec![(mob, id, 18_000)]);
    }

    #[test]
    fn wait_until_rejects_what_sscanf_rejects() {
        // sscanf's literal is case-sensitive; strn_cmp's is not.
        for line in [
            "wait UNTIL 10:00",
            "wait until x",
            "wait until :30",
            "wait until ",
        ] {
            let (host, mob, id) = run_script(line);
            assert!(host.waits.is_empty(), "{line}");
            assert!(state(&host, mob, id).is_active(), "{line}");
        }
        // "wait until" with nothing after is "wait w/o a number" in the C: the
        // argument is not "until " so the plain form is tried.
        let (host, _, _) = run_script("wait until");
        assert_eq!(count_logs(&host, "wait w/o a number: 'wait until'"), 1);
        let (host, _, _) = run_script("wait until x");
        assert_eq!(
            count_logs(&host, "wait until w/o a time: 'wait until x'"),
            1
        );
        // Hostile magnitudes wrap instead of panicking.
        let (host, _, _) = run_script("wait until 99999999999999999999:99999999999999999999");
        assert_eq!(host.waits.len(), 1);
        let (host, _, _) = run_script("wait until -99999999999999999999");
        assert_eq!(host.waits.len(), 1);
    }

    #[test]
    fn return_sets_the_return_value() {
        let (mut host, mob, id) = world("return 0\nsay a");
        assert_eq!(run(&mut host, mob, id), 0);
        assert_eq!(cmds(&host), vec!["say a"]);
        let (mut host, mob, id) = world("say a");
        assert_eq!(script_driver_default(&mut host, mob, id, Mode::New, 9), 9);
        let (mut host, mob, id) = world("return 5\nreturn abc");
        assert_eq!(run(&mut host, mob, id), 0);
        // No argument: logged, and the value becomes 1 whatever it was.
        let (mut host, mob, id) = world("return 4\nreturn ");
        assert_eq!(run(&mut host, mob, id), 1);
        assert_eq!(count_logs(&host, "return w/o an arg: 'return '"), 1);
        // The value survives a wait.
        let (mut host, mob, id) = world("return 6\nwait 1\nsay a");
        assert_eq!(run(&mut host, mob, id), 6);
    }

    #[test]
    fn unset_prefers_globals_then_locals() {
        let (mut host, mob, id) = world("unset g\nsay [%g%]\nunset l\nsay [%l%]\nunset l");
        host.script_mut(mob)
            .unwrap()
            .global_vars
            .add_var("g", "1", 0);
        host.script_mut(mob)
            .unwrap()
            .global_vars
            .add_var("l", "G", 0);
        host.script_mut(mob)
            .unwrap()
            .trigger_mut(id)
            .unwrap()
            .vars
            .add_var("l", "L", 0);
        run(&mut host, mob, id);
        // g gone; the first `unset l` removes the global l, leaving the local.
        assert_eq!(cmds(&host), vec!["say []", "say [L]"]);
        assert!(host.script(mob).unwrap().global_vars.is_empty());
        let (host, _, _) = run_script("unset \nunset nothing here");
        assert_eq!(count_logs(&host, "unset w/o an arg: 'unset '"), 1);
    }

    #[test]
    fn global_moves_a_local_into_the_script_under_its_context() {
        let (host, mob, _) = run_script("set x 5\nglobal x\nsay [%x%]");
        // Found through the globals now; gone from the locals.
        assert_eq!(cmds(&host), vec!["say [5]"]);
        let global = host.script(mob).unwrap().global_vars.get("x").unwrap();
        assert_eq!((global.value.as_str(), global.context), ("5", 0));

        let (host, mob, _) = run_script("context 7\nset y 1\nglobal y\nglobal y\nglobal nope");
        let global = host.script(mob).unwrap().global_vars.get("y").unwrap();
        assert_eq!(global.context, 7);
        assert_eq!(host.script(mob).unwrap().context, 7);
        assert_eq!(
            count_logs(&host, "local var 'y' not found in global call"),
            1
        );
        assert_eq!(
            count_logs(&host, "local var 'nope' not found in global call"),
            1
        );
        let (host, _, _) = run_script("global \ncontext ");
        assert_eq!(count_logs(&host, "global w/o an arg: 'global '"), 1);
        assert_eq!(count_logs(&host, "context w/o an arg: 'context '"), 1);
    }

    #[test]
    fn context_is_reset_by_new_runs_but_not_by_restarts() {
        let (mut host, mob, id) = world("set a 1\nglobal a\nwait 1\nset b 1\nglobal b");
        host.script_mut(mob).unwrap().context = 9;
        run(&mut host, mob, id);
        assert_eq!(
            host.script(mob)
                .unwrap()
                .global_vars
                .get("a")
                .unwrap()
                .context,
            0
        );
        host.script_mut(mob).unwrap().context = 4;
        resume(&mut host, mob, id);
        assert_eq!(
            host.script(mob)
                .unwrap()
                .global_vars
                .get("b")
                .unwrap()
                .context,
            4
        );
    }

    /// Two mobs with scripts, a player and an object.
    fn remote_world(script: &str) -> (MockHost, Uid, u64, Uid, Uid, Uid, Uid) {
        let (mut host, mob, id) = world(script);
        let room = host.real_room(1).unwrap();
        let other = host.add_mob(101, "other");
        host.char_to_room(other, room);
        host.ensure_script(other);
        let player = host.add_player("Bob");
        host.ensure_script(player);
        let obj = host.add_obj(7, "sword");
        host.ensure_script(obj);
        host.ensure_script(room);
        (host, mob, id, other, player, obj, room)
    }

    #[test]
    fn remote_copies_a_local_to_another_script() {
        let (mut host, mob, id, other, player, obj, room) = remote_world("");
        let script = format!(
            "context 4\nset v 9\nremote v {other}\nremote V {player}\nremote v {obj}\nremote v {room}\nremote v {}",
            ROOM_ID_BASE + 99
        );
        host.protos.clear();
        let proto = host.add_proto(6, AttachType::Mob, "g", 100, "", &script);
        let id2 = host.attach(mob, &proto).unwrap();
        let _ = id;
        run(&mut host, mob, id2);
        let var = |uid| host.script(uid).unwrap().global_vars.get("v").cloned();
        assert_eq!(
            var(other).map(|v| (v.value, v.context)),
            Some(("9".into(), 4))
        );
        // Players get context 0.
        assert_eq!(
            var(player).map(|v| (v.value, v.context)),
            Some(("9".into(), 0))
        );
        assert_eq!(
            var(obj).map(|v| (v.value, v.context)),
            Some(("9".into(), 4))
        );
        assert_eq!(
            var(room).map(|v| (v.value, v.context)),
            Some(("9".into(), 4))
        );
        // The last target (no such room) logged.
        assert_eq!(
            count_logs(
                &host,
                &format!("remote: uid '{}' invalid", ROOM_ID_BASE + 99)
            ),
            1
        );
    }

    #[test]
    fn remote_finds_globals_and_reports_errors() {
        let (mut host, mob, _, other, _, _, _) = remote_world("");
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            &format!(
                "remote g {other}\nremote nope {other}\nremote v abc\nremote v -4\nremote v\nremote v 99\nremote \nremote v 0"
            ),
        );
        let id = host.attach(mob, &proto).unwrap();
        host.script_mut(mob)
            .unwrap()
            .global_vars
            .add_var("g", "gg", 0);
        host.script_mut(mob)
            .unwrap()
            .global_vars
            .add_var("v", "vv", 0);
        run(&mut host, mob, id);
        assert_eq!(
            host.script(other)
                .unwrap()
                .global_vars
                .get("g")
                .map(|v| v.value.clone()),
            Some("gg".to_owned())
        );
        assert_eq!(
            count_logs(&host, "local var 'nope' not found in remote call"),
            1
        );
        assert_eq!(count_logs(&host, "remote: illegal uid 'abc'"), 1);
        assert_eq!(count_logs(&host, "remote: illegal uid '-4'"), 1);
        assert_eq!(count_logs(&host, "remote: illegal uid '0'"), 1);
        assert_eq!(count_logs(&host, "remote: invalid arguments 'remote v'"), 1);
        assert_eq!(count_logs(&host, "remote: invalid arguments 'remote '"), 1);
        assert_eq!(count_logs(&host, "remote: uid '99' invalid"), 1);
    }

    #[test]
    fn remote_to_a_target_without_a_script_does_nothing() {
        let (mut host, mob, _) = world("");
        let bare = host.add_mob(102, "bare");
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            &format!("set v 1\nremote v {bare}"),
        );
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert!(host.script(bare).is_none());
        assert!(host.logs.is_empty());
    }

    #[test]
    fn rdelete_removes_the_first_visible_match() {
        let (mut host, mob, _, other, _, _, _) = remote_world("");
        {
            let globals = &mut host.script_mut(other).unwrap().global_vars;
            globals.add_var("w", "three", 3);
            globals.add_var("w", "five", 5);
            globals.add_var("v", "zero", 0);
        }
        let script = format!(
            "rdelete v {other}\nrdelete w {other}\ncontext 3\nrdelete w {other}\nrdelete w {other}\nrdelete x {other}\nrdelete w\nrdelete w zzz\nrdelete w 77"
        );
        let proto = host.add_proto(6, AttachType::Mob, "g", 100, "", &script);
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        let left: Vec<(String, i64)> = host
            .script(other)
            .unwrap()
            .global_vars
            .vars
            .iter()
            .map(|v| (v.name.clone(), v.context))
            .collect();
        // v (context 0) went first. Own context 0 cannot see w@3 or w@5.
        // Under context 3 the first visible match is w@3, the second entry,
        // not the head w@5; the second `rdelete w` finds nothing visible.
        assert_eq!(left, vec![("w".to_owned(), 5)]);
        assert_eq!(
            count_logs(&host, "rdelete: invalid arguments 'rdelete w'"),
            1
        );
        assert_eq!(count_logs(&host, "rdelete: illegal uid 'zzz'"), 1);
        assert_eq!(count_logs(&host, "remote: uid '77' invalid"), 1);
    }

    fn attach_world(script: &str) -> (MockHost, Uid, u64) {
        let (mut host, mob, id) = world(script);
        host.add_proto(10, AttachType::Mob, "g", 100, "", "say ten");
        host.add_proto(11, AttachType::Mob, "g", 100, "", "say eleven");
        (host, mob, id)
    }

    fn vnums(host: &MockHost, uid: Uid) -> Vec<i32> {
        host.script(uid)
            .map(|s| s.triggers.iter().map(|t| t.proto.vnum).collect())
            .unwrap_or_default()
    }

    #[test]
    fn attach_adds_a_trigger_to_a_mob_object_or_room() {
        let (mut host, mob, _) = attach_world("");
        let room = host.real_room(1).unwrap();
        let obj = host.add_obj(7, "sword");
        let script = format!("attach 10 {mob}\nattach 11 {obj}\nattach 10 {room}\nattach 11 {mob}");
        let proto = host.add_proto(6, AttachType::Mob, "g", 100, "", &script);
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        // Appended after the running trigger and its siblings.
        assert_eq!(vnums(&host, mob), vec![5, 6, 10, 11]);
        assert_eq!(vnums(&host, obj), vec![11]);
        assert_eq!(vnums(&host, room), vec![10]);
        assert!(host.logs.is_empty(), "{:?}", host.logs);
    }

    #[test]
    fn attach_errors_are_logged() {
        let (mut host, mob, _) = attach_world("");
        let player = host.add_player("Bob");
        let script = format!(
            "attach \nattach 10\nattach 10 abc\nattach 10 5\nattach 99 {mob}\nattach 10 {player}\nattach 10 \n"
        );
        let proto = host.add_proto(6, AttachType::Mob, "g", 100, "", &script);
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        for needle in [
            "attach w/o an arg: 'attach '",
            "attach invalid id arg: 'attach 10'",
            "attach invalid id arg: 'attach 10 abc'",
            "attach invalid id arg: 'attach 10 5'",
            "attach invalid trigger: '99'",
            "attach invalid target: 'Bob'",
            "attach invalid id arg: 'attach 10 '",
        ] {
            assert_eq!(count_logs(&host, needle), 1, "{needle}: {:?}", host.logs);
        }
        assert!(host.script(player).is_none());
        // With CONFIG_SCRIPT_PLAYERS the player may be targeted.
        host.globals.nums.insert("script_players".into(), 1);
        let proto = host.add_proto(
            7,
            AttachType::Mob,
            "g",
            100,
            "",
            &format!("attach 10 {player}"),
        );
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert_eq!(vnums(&host, player), vec![10]);
    }

    #[test]
    fn detach_by_vnum_position_keyword_and_all() {
        let (mut host, mob, _) = attach_world("");
        let a = host.protos[&10].clone();
        let b = host.protos[&11].clone();
        for _ in 0..2 {
            host.attach(mob, &a);
            host.attach(mob, &b);
        }
        // [5, 10, 11, 10, 11]
        let run_line = |host: &mut MockHost, line: &str| {
            let proto = host.add_proto(8, AttachType::Mob, "g", 100, "", line);
            let id = host.attach(mob, &proto).unwrap();
            // Detach the helper by hand afterwards so lists stay comparable.
            run(host, mob, id);
            if let Some(script) = host.script_mut(mob) {
                script.triggers.retain(|t| t.id != id);
            }
        };
        // A number below the trigger count is a position: n >= num breaks first.
        run_line(&mut host, &format!("detach 2 {mob}"));
        assert_eq!(vnums(&host, mob), vec![5, 11, 10, 11]);
        // A number larger than any position falls back to the vnum.
        run_line(&mut host, &format!("detach 10 {mob}"));
        assert_eq!(vnums(&host, mob), vec![5, 11, 11]);
        // Keyword match on the name ("trigger 11"), the 2nd one.
        run_line(&mut host, &format!("detach 2.trigger {mob}"));
        assert_eq!(vnums(&host, mob), vec![5, 11]);
        // Out-of-range position and unknown keyword: nothing.
        run_line(&mut host, &format!("detach 50 {mob}"));
        run_line(&mut host, &format!("detach zzz {mob}"));
        assert_eq!(vnums(&host, mob), vec![5, 11]);
        // `all` (any case) removes the script, globals and all.
        host.script_mut(mob)
            .unwrap()
            .global_vars
            .add_var("g", "1", 0);
        run_line(&mut host, &format!("detach ALL {mob}"));
        assert!(host.script(mob).is_none());
    }

    #[test]
    fn detach_extracts_the_script_when_the_last_trigger_goes() {
        let (mut host, mob, _) = attach_world("");
        let obj = host.add_obj(7, "sword");
        host.attach(obj, &host.protos[&10].clone());
        host.script_mut(obj)
            .unwrap()
            .global_vars
            .add_var("g", "1", 0);
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            &format!("detach 10 {obj}\ndetach 10 {mob}"),
        );
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert!(host.script(obj).is_none());
        // The mob's script had no trigger 10: still there.
        assert_eq!(vnums(&host, mob), vec![5, 6]);
        assert!(host.cancelled_waits.contains(&(obj, 0)));
    }

    #[test]
    fn detach_errors_are_logged() {
        let (host, _, _) = run_script("detach \ndetach 10\ndetach 10 abc\ndetach 10 5");
        for needle in [
            "detach w/o an arg: 'detach '",
            "detach invalid id arg: 'detach 10'",
            "detach invalid id arg: 'detach 10 abc'",
            "detach invalid id arg: 'detach 10 5'",
        ] {
            assert_eq!(count_logs(&host, needle), 1, "{needle}");
        }
        // A target with no script: silently nothing.
        let (mut host, mob, _) = world("");
        let bare = host.add_mob(102, "bare");
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            &format!("detach all {bare}"),
        );
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert!(host.logs.is_empty());
    }

    #[test]
    fn detaching_a_waiting_trigger_cancels_its_wait() {
        let (mut host, mob, waiter) = attach_world("wait 50");
        run(&mut host, mob, waiter);
        assert_eq!(host.waits, vec![(mob, waiter, 50)]);
        let proto = host.add_proto(6, AttachType::Mob, "g", 100, "", &format!("detach 5 {mob}"));
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert!(host.waits.is_empty());
        assert!(host.cancelled_waits.contains(&(mob, waiter)));
        assert_eq!(vnums(&host, mob), vec![6]);
        // A stale resume finds nothing.
        assert_eq!(resume(&mut host, mob, waiter), 0);
    }

    #[test]
    fn a_trigger_that_detaches_itself_loses_its_variables_and_its_wait() {
        let (mut host, mob, id) = world(&format!(
            "set x 1\ndetach trigger {}\nsay [%x%]\nwait 1\nsay never",
            10_000_001
        ));
        assert_eq!(mob, 10_000_001);
        run(&mut host, mob, id);
        // The run carried on with its variables gone ...
        assert_eq!(cmds(&host), vec!["say []"]);
        // ... and the last trigger going took the script with it, so nothing
        // was queued and nothing came back.
        assert!(host.script(mob).is_none());
        assert!(host.waits.is_empty());
        assert!(host.cancelled_waits.contains(&(mob, id)));
        assert_eq!(host.depth, 0);
    }

    #[test]
    fn a_self_detach_with_siblings_keeps_the_siblings() {
        let (mut host, mob, id) = attach_world(&format!(
            "set x 1\ndetach 1 {}\nsay [%x%]\nwait 1",
            10_000_001
        ));
        let sibling = host.attach(mob, &host.protos[&10].clone()).unwrap();
        run(&mut host, mob, id);
        assert_eq!(cmds(&host), vec!["say []"]);
        assert_eq!(vnums(&host, mob), vec![10]);
        assert!(host.waits.is_empty());
        assert_eq!(host.script(mob).unwrap().triggers[0].id, sibling);
    }

    #[test]
    fn detach_all_from_inside_is_survivable() {
        let (mut host, mob, id) = world(&format!(
            "set x 1\ndetach all {}\nsay [%x%]\nunset x\nglobal x\ncontext 3",
            10_000_001
        ));
        run(&mut host, mob, id);
        assert_eq!(cmds(&host), vec!["say []"]);
        assert!(
            host.logs
                .iter()
                .any(|l| l.contains("local var 'x' not found in global call"))
        );
    }

    #[test]
    fn remove_trigger_matches_the_c_rules() {
        let mut host = MockHost::new();
        let mob = host.add_mob(100, "mob");
        for vnum in [3, 7, 9] {
            let proto = host.add_proto(vnum, AttachType::Mob, "g", 100, "", "");
            host.attach(mob, &proto);
        }
        // Names are "trigger 3", "trigger 7", "trigger 9".
        assert!(remove_trigger(&mut host, 999, "1").is_none());
        // 0 is a position every trigger satisfies: the first goes.
        let removed = remove_trigger(&mut host, mob, "0").unwrap();
        assert_eq!(
            (removed.vnum, removed.describe()),
            (3, "3 (trigger 3)".to_owned())
        );
        // [7, 9]: "9" is past the 2 positions, so the vnum matches.
        assert_eq!(remove_trigger(&mut host, mob, "9").unwrap().vnum, 9);
        // A keyword with no dot matches the first trigger with that name word.
        assert_eq!(remove_trigger(&mut host, mob, "TRIGGER").unwrap().vnum, 7);
        assert!(remove_trigger(&mut host, mob, "trigger").is_none());
        // "1." has an empty name and matches nothing.
        let proto = host.add_proto(3, AttachType::Mob, "g", 100, "", "");
        host.attach(mob, &proto);
        assert!(remove_trigger(&mut host, mob, "1.").is_none());
        assert!(remove_trigger(&mut host, mob, "").is_none());
        // A non-numeric count atoi's to 0, which any match satisfies.
        assert_eq!(
            remove_trigger(&mut host, mob, "x.3").map(|r| r.vnum),
            Some(3)
        );
        let proto = host.add_proto(3, AttachType::Mob, "g", 100, "", "");
        host.attach(mob, &proto);
        assert_eq!(
            remove_trigger(&mut host, mob, "1.3").map(|r| r.vnum),
            Some(3)
        );
        // The list is empty but the script remains: callers extract it.
        assert!(host.script(mob).is_some());
        // Non-ASCII names never panic.
        assert!(remove_trigger(&mut host, mob, "é.é").is_none());
    }

    #[test]
    fn extract_script_cancels_waits_and_drops_the_script() {
        let (mut host, mob, id) = world("wait 9");
        run(&mut host, mob, id);
        assert_eq!(host.waits.len(), 1);
        extract_script(&mut host, mob);
        assert!(host.script(mob).is_none());
        assert!(host.waits.is_empty());
        extract_script(&mut host, mob);
        extract_script(&mut host, 12345);
    }

    #[test]
    fn makeuid_by_number_and_by_name() {
        let (mut host, mob, _) = world("");
        let room = host.real_room(1).unwrap();
        let guard = host.add_mob(110, "guard");
        host.char_to_room(guard, room);
        let sword = host.add_obj(8, "sword");
        host.obj_to_room(sword, room);
        let cloak = host.add_obj(9, "cloak");
        host.obj_to_char(cloak, mob);
        let script = "makeuid n 12345\n\
             makeuid m mob guard\n\
             makeuid o obj sword\n\
             makeuid c OB cloak\n\
             makeuid r room here\n\
             makeuid none mob nobody\n\
             makeuid bad mob\n\
             makeuid bad zzz x\n\
             makeuid bad\n\
             makeuid \n\
             makeuid UP 7 junk\n\
             say %n% %m% %o% %c% %r% [%none%] %up%";
        let proto = host.add_proto(6, AttachType::Mob, "g", 100, "", script);
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert_eq!(
            cmds(&host),
            vec![format!(
                "say }}12345 }}{guard} }}{sword} }}{cloak} }}{room} [] }}7"
            )]
        );
        // The C chops the command in place, so logs show what is left.
        assert_eq!(count_logs(&host, "makeuid needs name: ''"), 1);
        assert_eq!(count_logs(&host, "makeuid syntax error: ''"), 1);
        assert_eq!(count_logs(&host, "makeuid invalid id arg: ''"), 1);
        assert_eq!(count_logs(&host, "makeuid w/o an arg: ''"), 1);
    }

    #[test]
    fn makeuid_for_objects_and_rooms_resolves_near_the_owner() {
        let mut host = MockHost::new();
        let room = host.add_room(1, "Room");
        let guard = host.add_mob(110, "guard");
        host.char_to_room(guard, room);
        let sword = host.add_obj(8, "sword");
        host.obj_to_room(sword, room);
        let owner = host.add_obj(9, "orb");
        host.obj_to_room(owner, room);
        let script = "makeuid m mob guard\nmakeuid o obj sword\nmakeuid r room x\nglobal m\nglobal o\nglobal r";
        let proto = host.add_proto(6, AttachType::Obj, "b", 100, "", script);
        let id = host.attach(owner, &proto).unwrap();
        assert_eq!(script_driver(&mut host, owner, id, Mode::New), 1);
        let get = |host: &MockHost, uid: Uid, name: &str| {
            host.script(uid)
                .unwrap()
                .global_vars
                .get(name)
                .map(|v| v.value.clone())
        };
        assert_eq!(get(&host, owner, "m"), Some(format!("}}{guard}")));
        assert_eq!(get(&host, owner, "o"), Some(format!("}}{sword}")));
        assert_eq!(get(&host, owner, "r"), Some(format!("}}{room}")));

        let proto = host.add_proto(7, AttachType::Wld, "b", 100, "", script);
        let id = host.attach(room, &proto).unwrap();
        assert_eq!(script_driver(&mut host, room, id, Mode::New), 1);
        assert_eq!(get(&host, room, "m"), Some(format!("}}{guard}")));
        assert_eq!(get(&host, room, "o"), Some(format!("}}{sword}")));
        assert_eq!(get(&host, room, "r"), Some(format!("}}{room}")));
    }

    #[test]
    fn extract_takes_the_nth_word_lowercased() {
        let (host, _, _) = run_script(
            "extract w 2 alpha BETA gamma\nsay [%w%]\nextract w 1 HeLLo\nsay [%w%]\nextract w 5 a b\nsay [%w%]\nextract w 0 a\nextract w x a\nextract\nextract w 2000000000 a b\nsay [%w%]",
        );
        assert_eq!(
            cmds(&host),
            vec!["say [beta]", "say [hello]", "say []", "extract", "say []"]
        );
        // A bare `extract` has no trailing space, so it is a mud command.
        assert_eq!(count_logs(&host, "extract number < 1!"), 2);
    }

    #[test]
    fn dg_letter_takes_one_character_and_logs_deprecation() {
        let (host, _, _) = run_script(
            "dg_letter c 4 L337-String\nsay [%c%]\ndg_letter c 11 L337-String\nsay [%c%]\ndg_letter d 1 Abc\nsay [%d%]",
        );
        assert_eq!(cmds(&host), vec!["say [7]", "say [g]", "say [A]"]);
        assert_eq!(count_logs(&host, "The use of dg_letter is deprecated"), 3);
        assert_eq!(
            count_logs(
                &host,
                "- Use 'set <new variable> %<text/var>.charat(index)%' instead."
            ),
            3
        );
        let (host, _, _) = run_script(
            "dg_letter c 0 abc\ndg_letter c 12 L337-String\ndg_letter c 4 abc\ndg_letter c -1 abc",
        );
        assert_eq!(count_logs(&host, "Trigger #5 : dg_letter number < 1!"), 2);
        assert_eq!(
            count_logs(&host, "Trigger #5 : dg_letter number > strlen!"),
            2
        );
        // Non-ASCII: whole character at a boundary, U+FFFD inside one.
        let (host, _, _) = run_script(
            "dg_letter c 1 éa\nsay %c%\ndg_letter c 2 éa\nsay %c%\ndg_letter c 3 éa\nsay %c%",
        );
        assert_eq!(cmds(&host), vec!["say é", "say \u{fffd}", "say a"]);
    }

    #[test]
    fn halt_nop_and_dispatch_order() {
        let (host, _, _) = run_script("say a\nnop whatever\nnop\nhalting now\nsay b");
        assert_eq!(cmds(&host), vec!["say a", "nop"]);
        // Command words need their trailing space, so a bare `eval` or `set`
        // goes to the mob.
        let (host, _, _) = run_script("eval\nset\nwait\nreturn\nunset");
        assert_eq!(cmds(&host), vec!["eval", "set", "wait", "return", "unset"]);
        // Leading whitespace is skipped and the keyword test is case-blind.
        let (host, _, _) = run_script("  \tSET x 1\n\t SAY %x%");
        assert_eq!(cmds(&host), vec!["SAY 1"]);
        // A whitespace-only line reaches the interpreter empty.
        let (host, _, _) = run_script("   ");
        assert_eq!(cmds(&host), vec![""]);
    }

    #[test]
    fn eval_goes_to_the_expression_module() {
        let (host, _, _) = run_script("eval r 5\nsay %r%");
        assert_eq!(cmds(&host), vec!["say 5"]);
    }

    #[test]
    fn object_and_room_owners_use_their_own_interpreters_not_the_mob_fallback() {
        let mut host = MockHost::new();
        let room = host.add_room(1, "Room");
        let obj = host.add_obj(8, "orb");
        host.obj_to_room(obj, room);
        for (owner, attach) in [(obj, AttachType::Obj), (room, AttachType::Wld)] {
            let proto = host.add_proto(6, attach, "b", 100, "", "set x 1\nsay hi");
            let id = host.attach(owner, &proto).unwrap();
            assert_eq!(script_driver(&mut host, owner, id, Mode::New), 1);
        }
        assert!(host.mud_commands.is_empty());
    }

    #[test]
    fn the_run_type_comes_from_the_owner_not_the_prototype() {
        // A room-type prototype attached to a mob still runs as a mob script.
        let mut host = MockHost::new();
        let mob = host.add_mob(100, "mob");
        let proto = host.add_proto(6, AttachType::Wld, "b", 100, "", "say hi");
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert_eq!(cmds(&host), vec!["say hi"]);
    }

    #[test]
    fn depth_cap_logs_and_extracts_the_script() {
        let (mut host, mob, id) = world("say a");
        host.depth = 11;
        assert_eq!(run(&mut host, mob, id), SCRIPT_ERROR_CODE);
        assert_eq!(
            host.logs,
            vec![
                "Trigger 5 recursed beyond maximum allowed depth.".to_owned(),
                "It was attached to mob [100]".to_owned()
            ]
        );
        assert!(host.script(mob).is_none());
        assert!(host.mud_commands.is_empty());
        assert_eq!(host.depth, 11);

        // Ten deep is still allowed, and the counter is restored.
        let (mut host, mob, id) = world("say a");
        host.depth = 10;
        assert_eq!(run(&mut host, mob, id), 1);
        assert_eq!(cmds(&host), vec!["say a"]);
        assert_eq!(host.depth, 10);
    }

    #[test]
    fn depth_cap_cancels_waits_of_the_extracted_script() {
        let (mut host, mob, id) = world("wait 3");
        run(&mut host, mob, id);
        let proto = host.protos[&5].clone();
        let other = host.attach(mob, &proto).unwrap();
        host.depth = 11;
        assert_eq!(run(&mut host, mob, other), SCRIPT_ERROR_CODE);
        assert!(host.waits.is_empty());
        assert!(host.cancelled_waits.contains(&(mob, id)));
    }

    #[test]
    fn owner_purged_flag_is_zeroed_on_entry_and_stops_the_script_when_set() {
        let (mut host, mob, id) = world("say a\nsay b");
        host.owner_purged = true;
        run(&mut host, mob, id);
        assert_eq!(cmds(&host), vec!["say a", "say b"]);
        assert!(!host.owner_purged);

        // With the flag raised after a command, the run stops there: no
        // cleanup, the trigger keeps its variables and depth.
        let proto = host.protos[&5].clone();
        let mut trig = TrigInstance::new(0, proto);
        trig.run.depth = 1;
        trig.vars.add_var("keep", "1", 0);
        host.owner_purged = true;
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        let (outcome, ret) = run_lines(&mut cx, 0, 1);
        assert!(matches!(outcome, Outcome::Purged));
        assert_eq!(ret, 1);
        assert_eq!(trig.run.depth, 1);
        assert!(trig.vars.get("keep").is_some());
        assert_eq!(
            cmds(&host).len(),
            3,
            "only the first line ran: {:?}",
            cmds(&host)
        );
    }

    #[test]
    fn purge_is_tested_only_after_dispatched_commands() {
        let (mut host, mob, id) = world("set x 1\nsay a");
        let proto = host.protos[&5].clone();
        let mut trig = TrigInstance::new(id, proto);
        trig.run.depth = 1;
        host.owner_purged = true;
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        // `set` is not a dispatched command: the flag is not looked at.
        let (_, _) = run_lines(&mut cx, 0, 1);
        assert_eq!(host.mud_commands.len(), 1);
    }

    #[test]
    fn resume_ignores_missing_owners_and_instances() {
        let (mut host, mob, _) = world("say a");
        assert_eq!(resume(&mut host, 424_242, 0), 0);
        // Only the line the C sends to builders (`script_log`); the other
        // three go to the syslog, which has no primitive.
        assert_eq!(
            host.logs,
            vec!["Trigger restart attempt on unknown entity."]
        );
        assert_eq!(resume(&mut host, mob, 77), 0);
        assert!(host.mud_commands.is_empty());
    }

    #[test]
    fn resume_continues_the_script_and_never_requeues() {
        // trig_wait_event returns 0 whatever the script returned.
        let (mut host, mob, id) = world("say a\nwait 2\nsay b\nreturn 0");
        run(&mut host, mob, id);
        assert_eq!(host.waits, vec![(mob, id, 2)]);
        assert_eq!(resume(&mut host, mob, id), 0);
        assert_eq!(cmds(&host), vec!["say a", "say b"]);
        assert_eq!(state(&host, mob, id).depth, 0);
        // An idle instance (depth 0) resumed again runs no line at all.
        assert_eq!(resume(&mut host, mob, id), 0);
        assert_eq!(cmds(&host), vec!["say a", "say b"]);
        assert!(host.waits.len() == 1);
    }

    #[test]
    fn resume_runs_for_a_dead_but_still_listed_character() {
        // The C finds the mob in character_list until it is swept, and runs.
        let (mut host, mob, id) = world("wait 1\nsay late");
        run(&mut host, mob, id);
        host.extract_char(mob);
        assert!(host.exists(mob));
        assert_eq!(resume(&mut host, mob, id), 0);
        assert_eq!(cmds(&host), vec!["say late"]);
    }

    /// Run each script and check the commands it dispatched and the log
    /// lines it wrote, so a line that is never reached fails the test.
    fn expect(script: &str, want_cmds: &[&str], want_log: Option<&str>) {
        let (host, mob, id) = run_script(script);
        assert_eq!(cmds(&host), want_cmds, "script {script:?}");
        match want_log {
            Some(needle) => assert!(
                host.logs.iter().any(|l| l.contains(needle)),
                "script {script:?}: no log containing {needle:?} in {:?}",
                host.logs
            ),
            None => assert!(host.logs.is_empty(), "script {script:?}: {:?}", host.logs),
        }
        assert_eq!(host.depth, 0);
        assert!(host.script(mob).unwrap().trigger(id).is_some());
    }

    #[test]
    fn non_ascii_lines_reach_every_statement_without_panicking() {
        expect("é", &["é"], None);
        // NBSP is not isspace in the C locale: the line is a plain command.
        expect("\u{a0}if é\nsay ok", &["\u{a0}if é", "say ok"], None);
        expect("if é\nsay in\nend\nsay ok", &["say in", "say ok"], None);
        expect(
            "if é\nsay in\nelse\nsay out\nend\nsay ok",
            &["say in", "say ok"],
            None,
        );
        expect(
            "switch é\ncase é\nsay hit\nbreak\ndone\nsay ok",
            &["say hit", "say ok"],
            None,
        );
        expect("wait é\nsay ok", &[], Some("wait w/o a number"));
        expect("set é é\nsay %é%\nsay ok", &["say é", "say ok"], None);
        expect("extract é 1 é\nsay %é%", &["say é"], None);
        expect(
            "dg_letter x 2 é\nsay %x%",
            &["say \u{fffd}"],
            Some("deprecated"),
        );
        expect("makeuid é é é\nsay ok", &["say ok"], Some("makeuid"));
        expect("attach é é\nsay ok", &["say ok"], Some("attach"));
        expect("detach é é\nsay ok", &["say ok"], Some("detach"));
        expect(
            "remote é é\nsay ok",
            &["say ok"],
            Some("not found in remote call"),
        );
        expect("rdelete é é\nsay ok", &["say ok"], Some("illegal uid"));
        expect("return é\nsay ok", &["say ok"], None);
        expect("unset é\nsay ok", &["say ok"], None);
        expect(
            "global é\nsay ok",
            &["say ok"],
            Some("not found in global call"),
        );
        expect("context é\nsay ok", &["say ok"], None);
        expect(
            "if\nelse\nend\ndone\ncase\n*\n   ",
            &["if", ""],
            Some("without 'if'"),
        );
    }

    #[test]
    fn very_long_lines_do_not_panic() {
        // The real var_subst/eval_expr refuse lines over MAX_INPUT_LENGTH, so
        // only what holds either way is asserted: no panic, a clean state.
        let long = "x".repeat(100_000);
        for script in [
            format!("set a {long}\nsay %a%"),
            format!("if {long}\nsay hi\nend"),
            format!("wait {long}\nsay no"),
            format!("extract w 3 {long}\nsay [%w%]"),
            format!("while {long}\nsay hi\ndone"),
            format!("switch {long}\ncase {long}\nsay hi\ndone"),
            format!("makeuid m obj {long}"),
            format!("dg_letter l 5 {long}"),
        ] {
            let (host, _, _) = run_script(&script);
            assert_eq!(host.depth, 0);
        }
        // Driver-owned parsing handles long text directly.
        let (mut host, mob, id) = world("");
        let mut trig = host.script_mut(mob).unwrap().check_out(id).unwrap();
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        extract_value(&mut cx, &format!("extract w 1 {long}"));
        assert_eq!(cx.trig.vars.get("w").map(|v| v.value.len()), Some(100_000));
        dg_letter_value(&mut cx, &format!("dg_letter l 99999 {long}"));
        assert_eq!(cx.trig.vars.get("l").map(|v| v.value.as_str()), Some("x"));
    }

    #[test]
    fn extract_stops_at_a_fixpoint_instead_of_spinning() {
        // A remainder starting with a tab makes half_chop return the same
        // text forever (skip_spaces stops at a tab, any_one_arg at isspace).
        // The C yields an empty word after num iterations; so does this,
        // without running them.
        let started = std::time::Instant::now();
        let (host, _, _) = run_script("extract w 2000000000 \tfoo bar\nsay [%w%]");
        assert_eq!(cmds(&host), vec!["say []"]);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// Start trigger `id` of `owner` by hand the way the driver does, so a
    /// test can run another trigger "from inside" it.
    fn check_out_running(host: &mut MockHost, owner: Uid, id: u64) -> TrigInstance {
        let mut trig = host.script_mut(owner).unwrap().check_out(id).unwrap();
        trig.run.depth = 1;
        *host.depth_mut() += 1;
        trig
    }

    #[test]
    fn a_nested_trigger_detaching_the_running_one_clears_its_variables() {
        let (mut host, mob, a) = world("say a");
        host.add_proto(6, AttachType::Mob, "g", 100, "", &format!("detach 5 {mob}"));
        let sibling = host.attach(mob, &host.protos[&6].clone()).unwrap();
        let mut trig = check_out_running(&mut host, mob, a);
        trig.vars.add_var("x", "1", 0);
        // The nested run happens while A is checked out.
        assert_eq!(script_driver(&mut host, mob, sibling, Mode::New), 1);
        assert_eq!(host.depth, 1);
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        clear_if_detached(&mut cx);
        assert!(trig.vars.is_empty());
        // check_in fails: the slot is gone, so no wait would be queued.
        assert!(!host.script_mut(mob).unwrap().check_in(trig));
    }

    #[test]
    fn a_nested_depth_cap_extracting_the_script_clears_the_running_triggers_variables() {
        let (mut host, mob, a) = world("say a");
        let proto = host.protos[&5].clone();
        let b = host.attach(mob, &proto).unwrap();
        let mut trig = check_out_running(&mut host, mob, a);
        trig.vars.add_var("x", "1", 0);
        *host.depth_mut() = 11;
        assert_eq!(
            script_driver(&mut host, mob, b, Mode::New),
            SCRIPT_ERROR_CODE
        );
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        clear_if_detached(&mut cx);
        assert!(trig.vars.is_empty());
        assert!(host.script(mob).is_none());
    }

    #[test]
    fn a_live_running_trigger_keeps_its_variables() {
        let (mut host, mob, a) = world("say a");
        let mut trig = check_out_running(&mut host, mob, a);
        trig.vars.add_var("x", "1", 0);
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        clear_if_detached(&mut cx);
        assert!(trig.vars.get("x").is_some());
    }

    #[test]
    fn the_thirty_loop_yield_of_a_self_detached_trigger_queues_no_wait() {
        let (mut host, mob, id) = world(&format!(
            "detach 5 {mob}\nwhile 1\nsay tick\ndone",
            mob = 10_000_001
        ));
        assert_eq!(mob, 10_000_001);
        run(&mut host, mob, id);
        // The script went with its only trigger; the yield found no slot.
        assert!(host.script(mob).is_none());
        assert!(host.waits.is_empty());
        assert_eq!(host.depth, 0);
        assert_eq!(cmds(&host).len(), 30);
    }

    #[test]
    fn makeuid_for_a_mob_owner_prefers_inventory_to_the_room() {
        let (mut host, mob, _) = world("");
        let room = host.real_room(1).unwrap();
        let in_room = host.add_obj(8, "sword");
        host.obj_to_room(in_room, room);
        let carried = host.add_obj(9, "sword");
        host.obj_to_char(carried, mob);
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            "makeuid o obj sword\nsay %o%",
        );
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert_eq!(cmds(&host), vec![format!("say }}{carried}")]);
    }

    #[test]
    fn depth_cap_names_objects_and_rooms_by_their_own_text() {
        let mut host = MockHost::new();
        let room = host.add_room(1, "Hall");
        let obj = host.add_obj(8, "orb");
        host.obj_to_room(obj, room);
        for (owner, attach, want) in [
            (obj, AttachType::Obj, "It was attached to orb [8]"),
            (room, AttachType::Wld, "It was attached to Hall [1]"),
        ] {
            let proto = host.add_proto(6, attach, "b", 100, "", "set x 1");
            let id = host.attach(owner, &proto).unwrap();
            host.logs.clear();
            host.depth = 11;
            assert_eq!(
                script_driver(&mut host, owner, id, Mode::New),
                SCRIPT_ERROR_CODE
            );
            assert_eq!(
                host.logs,
                vec!["Trigger 6 recursed beyond maximum allowed depth.", want]
            );
            assert!(host.script(owner).is_none());
        }
    }

    #[test]
    fn makeuid_room_fallback_searches_the_bare_name() {
        // The C's inventory lookup strips `N.` from the shared buffer, so the
        // room lookup always takes the first visible match of the bare word.
        let (mut host, mob, _) = world("");
        let room = host.real_room(1).unwrap();
        let first = host.add_obj(8, "sword");
        host.obj_to_room(first, room);
        let second = host.add_obj(9, "sword");
        host.obj_to_room(second, room);
        let want = host.room_contents(room)[0];
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            "makeuid a obj 2.sword\nmakeuid b obj last.sword\nmakeuid c obj 0.sword\nmakeuid d obj x.sword\nsay %a% %b% %c% %d%",
        );
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert_eq!(
            cmds(&host),
            vec![format!("say }}{want} }}{want} }}{want} }}{want}")]
        );
    }

    #[test]
    fn a_while_false_at_its_own_line_resets_its_loop_counter() {
        let (mut host, mob, id) = world("while %go%\nsay tick\ndone");
        let set_go = |host: &mut MockHost, value: &str| {
            host.script_mut(mob)
                .unwrap()
                .trigger_mut(id)
                .unwrap()
                .vars
                .add_var("go", value, 0);
        };
        set_go(&mut host, "1");
        run(&mut host, mob, id);
        for _ in 0..3 {
            resume(&mut host, mob, id);
        }
        // 30, 60, 90 and then the cap at 100 leave cl->loops at 10.
        assert_eq!(cmds(&host).len(), 100);
        set_go(&mut host, "0");
        run(&mut host, mob, id);
        assert_eq!(cmds(&host).len(), 100);
        // `cl->loops = 0` ran: the next yield is a full thirty loops away.
        set_go(&mut host, "1");
        run(&mut host, mob, id);
        assert_eq!(cmds(&host).len(), 130);
    }

    #[test]
    fn find_case_nested_scan_uses_six_letter_switch_and_three_letter_done() {
        // `switchx` opens a nested block for find_case (strn_cmp(.., 6)).
        let (host, _, _) = run_script(
            "switch b\ncase a\nswitchx\nsay no\ndone\ncase b\nsay yes\nbreak\ndone\nsay z",
        );
        assert_eq!(cmds(&host), vec!["say yes", "say z"]);
        // `donkey` ends the search (strn_cmp("done", p, 3)) and execution
        // resumes on the line after it, falling into the lines before the
        // real case.
        let (host, _, _) =
            run_script("switch b\ncase a\ndonkey\nsay mid\ncase b\nsay yes\ndone\nsay z");
        assert_eq!(cmds(&host), vec!["say mid", "say yes", "say z"]);
    }

    #[test]
    fn remote_sees_only_globals_of_context_zero_or_the_current_one() {
        let (mut host, mob, _, other, _, _, _) = remote_world("");
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            &format!("remote five {other}\nremote zero {other}\ncontext 5\nremote five {other}"),
        );
        let id = host.attach(mob, &proto).unwrap();
        {
            let globals = &mut host.script_mut(mob).unwrap().global_vars;
            globals.add_var("five", "5", 5);
            globals.add_var("zero", "0", 0);
        }
        run(&mut host, mob, id);
        assert_eq!(count_logs(&host, "local var 'five' not found"), 1);
        let theirs = &host.script(other).unwrap().global_vars;
        assert!(theirs.get("zero").is_some());
        assert_eq!(theirs.get("five").map(|v| v.context), Some(5));
    }

    #[test]
    fn detach_all_is_the_whole_word() {
        let (mut host, mob, _) = attach_world("");
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            &format!("detach allx {mob}"),
        );
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        assert!(host.script(mob).is_some());
    }

    #[test]
    fn makeuid_room_of_an_object_owner_follows_its_carrier() {
        let mut host = MockHost::new();
        let room = host.add_room(1, "Room");
        let mob = host.add_mob(100, "mob");
        host.char_to_room(mob, room);
        let owner = host.add_obj(9, "orb");
        host.obj_to_char(owner, mob);
        // An object owner cannot `say`; read the variable back via remote.
        let proto = host.add_proto(
            7,
            AttachType::Obj,
            "b",
            100,
            "",
            &format!("makeuid r room x\nremote r {owner}"),
        );
        let id = host.attach(owner, &proto).unwrap();
        run(&mut host, owner, id);
        let got = host.script(owner).unwrap().global_vars.get("r").cloned();
        assert_eq!(got.map(|v| v.value), Some(format!("}}{room}")));
    }

    #[test]
    fn makeuid_extract_and_dg_letter_store_under_the_script_context() {
        let (mut host, mob, _, other, _, _, _) = remote_world("");
        let proto = host.add_proto(
            6,
            AttachType::Mob,
            "g",
            100,
            "",
            &format!(
                "context 4\nmakeuid m 12345\nextract e 1 foo\ndg_letter l 1 xyz\nremote m {other}\nremote e {other}\nremote l {other}"
            ),
        );
        let id = host.attach(mob, &proto).unwrap();
        run(&mut host, mob, id);
        let theirs = &host.script(other).unwrap().global_vars;
        for name in ["m", "e", "l"] {
            assert_eq!(theirs.get(name).map(|v| v.context), Some(4), "{name}");
        }
    }

    #[test]
    fn truncate_c_keeps_what_a_c_buffer_holds() {
        let mut text = "x".repeat(600);
        truncate_c(&mut text, MAX_INPUT_LENGTH + 1);
        assert_eq!(text.len(), MAX_INPUT_LENGTH);
        let mut text = format!("{}é", "x".repeat(MAX_INPUT_LENGTH - 1));
        truncate_c(&mut text, MAX_INPUT_LENGTH + 1);
        assert_eq!(text.len(), MAX_INPUT_LENGTH - 1);
    }

    /// A trigger checked out of `mob`'s script, with a variable and depth 1,
    /// as `script_driver_default` holds it when the line loop ends.
    fn held(host: &mut MockHost, mob: Uid, id: u64) -> TrigInstance {
        let mut trig = check_out_running(host, mob, id);
        trig.vars.add_var("keep", "1", 0);
        trig
    }

    #[test]
    fn end_run_cleans_up_only_a_finished_run() {
        let (mut host, mob, id) = world("");
        let trig = held(&mut host, mob, id);
        end_run(&mut host, mob, trig, Outcome::Finished);
        let slot = host.script(mob).unwrap().trigger(id).unwrap();
        assert!(slot.vars.is_empty());
        assert_eq!(slot.run.depth, 0);
        assert_eq!(host.depth, 0);

        // A purged run is returned as it was: variables kept, depth set, and
        // no wait queued.
        let trig = held(&mut host, mob, id);
        end_run(&mut host, mob, trig, Outcome::Purged);
        let slot = host.script(mob).unwrap().trigger(id).unwrap();
        assert!(slot.vars.get("keep").is_some());
        assert_eq!(slot.run.depth, 1);
        assert_eq!(host.depth, 0);
        assert!(host.waits.is_empty());
    }

    #[test]
    fn end_run_queues_a_wait_only_for_a_kept_instance() {
        let (mut host, mob, id) = world("");
        let trig = held(&mut host, mob, id);
        end_run(&mut host, mob, trig, Outcome::Wait(Some(7)));
        assert_eq!(host.waits, vec![(mob, id, 7)]);
        assert!(
            host.script(mob)
                .unwrap()
                .trigger(id)
                .unwrap()
                .vars
                .get("keep")
                .is_some()
        );

        host.waits.clear();
        let trig = held(&mut host, mob, id);
        end_run(&mut host, mob, trig, Outcome::Wait(None));
        assert!(host.waits.is_empty());

        // Detached while it ran: check_in fails and nothing is queued.
        let trig = held(&mut host, mob, id);
        extract_script(&mut host, mob);
        end_run(&mut host, mob, trig, Outcome::Wait(Some(7)));
        assert!(host.waits.is_empty());
        assert_eq!(host.depth, 0);
    }

    #[test]
    fn a_trigger_freed_mid_run_loses_its_variables_after_any_line() {
        // `set` is not a dispatched command; the clearing happens in the line
        // loop, after every line (so also after dg_cast / dg_affect).
        let (mut host, mob, id) = world("set x 1\nsay [%x%]");
        let mut trig = check_out_running(&mut host, mob, id);
        extract_script(&mut host, mob);
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        let (_, _) = run_lines(&mut cx, 0, 1);
        assert_eq!(cmds(&host), vec!["say []"]);
    }

    /// Where the `.trg` files live: `DG_TRG_DIR`, else `lib/world/trg` of a
    /// `tbamud` checkout next to the repository.
    fn corpus_dir() -> std::path::PathBuf {
        std::env::var_os("DG_TRG_DIR").map_or_else(
            || {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tbamud/lib/world/trg")
            },
            std::path::PathBuf::from,
        )
    }

    #[test]
    #[ignore = "needs the tbaMUD corpus; set DG_TRG_DIR and run with --ignored"]
    fn corpus_scripts_run_without_panicking_and_leave_a_sane_state() {
        let dir = corpus_dir();
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("corpus directory {} unreadable: {e}", dir.display()));
        let mut ran = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "trg") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let mut lines = text.lines();
            while let Some(line) = lines.next() {
                let Some(vnum) = line
                    .strip_prefix('#')
                    .and_then(|v| v.trim().parse::<i32>().ok())
                else {
                    continue;
                };
                let _name = lines.next();
                let attach = lines
                    .next()
                    .and_then(|h| h.split_whitespace().next()?.parse::<i32>().ok())
                    .and_then(AttachType::from_index)
                    .unwrap_or(AttachType::Mob);
                let _arg = lines.next();
                let mut body = String::new();
                for line in lines.by_ref() {
                    if line == "~" {
                        break;
                    }
                    body.push_str(line);
                    body.push('\n');
                }
                let mut host = MockHost::new();
                let room = host.add_room(1, "Room");
                let owner = match attach {
                    AttachType::Mob => host.add_mob(100, "mob"),
                    AttachType::Obj => host.add_obj(100, "orb"),
                    AttachType::Wld => room,
                };
                let proto = host.add_proto(vnum, attach, "a", 100, "", &body);
                let id = host.attach(owner, &proto).unwrap();
                script_driver(&mut host, owner, id, Mode::New);
                assert_eq!(host.depth, 0, "trigger {vnum}");
                // The trigger may have detached itself, or its owner may be
                // gone; otherwise it ended idle, waiting or with a log line.
                // A purged owner leaves the trigger as it was (the C returns
                // without cleanup), so only the other outcomes are checked.
                if !host.owner_purged
                    && let Some(trigger) = host.script(owner).and_then(|s| s.trigger(id))
                {
                    assert!(
                        trigger.run.depth == 0 || !host.waits.is_empty() || !host.logs.is_empty(),
                        "trigger {vnum} left running without a wait"
                    );
                }
                ran += 1;
            }
        }
        assert!(ran > 1000, "ran only {ran}");
    }
}
