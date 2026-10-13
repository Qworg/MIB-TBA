//! `%…%` substitution (`dg_variables.c`): `var_subst` and `find_replacement`.
//!
//! [`var_subst`] is the scanner that finds each `%var.field(subfield)%` in a
//! script line; [`find_replacement`] turns one such reference into text. The
//! three entity field tables (`char_field`, `obj_field`, `room_field`) and
//! `text_processed` live in their own modules; this module decides *which*
//! of them a reference reaches, and what happens when they decline.
//!
//! # Deviations from the C
//!
//! - **The scanner works on a byte buffer.** `var_subst` edits its copy of
//!   the line in place (it writes NULs over `.`, `(` and `)`, and
//!   `strcpy(var, "tmpvr")` overwrites the text after the variable name), and
//!   scripts can observe that: with a variable name shorter than five
//!   characters, `%a.b.c%` loses the first character of `c`. The port keeps a
//!   `[u8]` buffer and the same indices, so those effects are reproduced.
//!   A read past the end of the buffer yields NUL and a write past it is
//!   dropped; the C reads uninitialised stack there (an unterminated `%var`
//!   moves `p` one past the terminator), which the port treats as NUL, so the
//!   scan simply ends.
//! - `subfield_p` and `paren_count` are never reset between references on a
//!   line, exactly as in the C: a reference with no `(…)` after one that had
//!   one sees the earlier subfield text again, and an unbalanced `)` leaves
//!   the count negative for the rest of the line.
//! - Text is cut to the C's buffer sizes at a character boundary rather than
//!   mid-character: a replacement to 491 bytes (`repl_str`), the output to
//!   511 (`left`). Bytes the scanner leaves that are not valid UTF-8 (a
//!   multi-byte character cut by the `tmpvr` overwrite) become U+FFFD in a
//!   variable, field or subfield name, and are dropped at the end of the
//!   output.
//! - The C recurses without bound: a subfield is substituted by a recursive
//!   `var_subst`, and a chained reference runs `eval`, which substitutes
//!   again. A variable whose value reproduces its own reference makes that
//!   loop forever. [`MAX_NESTING`] bounds the nesting and [`MAX_WORK`] the
//!   total number of nested substitutions under one top-level call; when
//!   either trips, one log line is written and the whole top-level line
//!   substitutes to nothing. Once a limit trips no further reference on the
//!   line is evaluated (no `find_replacement`, no `eval`).
//! - `%random.dir%` retries a random direction until it finds an exit; the
//!   retry is capped at [`DIR_RETRIES`] and then takes the first exit, so a
//!   host whose exits change underneath it cannot hang the VM. `DIR_COUNT` follows
//!   `CONFIG_DIAGONAL_DIRS` ([`SubstHost::subst_diagonal_dirs`]): 6, or 10
//!   with diagonals on, for both the exit count and the roll.
//! - The vnum arguments of `%people%`, `%findmob%` and the room of
//!   `%findobj%` are narrowed to `IDXTYPE` (`u16`) as the C does, so `65535`
//!   is `NOBODY` and `68537` is `3001`. So is the object vnum of `%findobj%`
//!   (`fields_obj::item_in_list_c`).
//! - `%global.name%` reads the script of `world[0]`; the port takes that to
//!   be room vnum 0 (`real_room(0)`), the void the rest of the C names by
//!   `r->number == 0`.
//! - The fallback for a field no table names reads the entity's own globals
//!   by name only, ignoring contexts, because that is what the C does at
//!   that point (the lookup before it, for the variable itself, does honour
//!   the context).
//! - A `%self%` in the nested `%<npc>.global(<var>)%` call is the id of the
//!   running trigger's owner whatever its kind; the C casts `go` to a
//!   character.

use crate::cstr::{atoi, str_eq};
use crate::expr::process_eval;
use crate::host::{BaseHost, Cx, DgHost};
use crate::lookup::{
    get_char, get_char_by_obj, get_char_by_room, get_char_in_room, get_obj, get_obj_by_obj,
    get_obj_by_room, get_obj_in_list, get_room, obj_room, trgvar_in_room,
};
use crate::text::text_processed;
use crate::types::{
    AttachType, DIRS, MAX_INPUT_LENGTH, NUM_OF_DIRS, ROOM_ID_BASE, Uid, VmState, uid_text,
};
use crate::{fields_char, fields_obj, fields_room};

/// `happy_data` (`act.h`): the happy-hour state `%happyhour%` reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HappyHour {
    /// `HAPPY_QP` (`happy_data.qp_rate`).
    pub qp_rate: i32,
    /// `HAPPY_EXP` (`happy_data.exp_rate`).
    pub exp_rate: i32,
    /// `HAPPY_GOLD` (`happy_data.gold_rate`).
    pub gold_rate: i32,
    /// `HAPPY_TIME` (`happy_data.ticks_left`).
    pub ticks_left: i32,
}

impl HappyHour {
    /// `IS_HAPPYHOUR`: any rate is positive and time is left.
    pub fn is_active(&self) -> bool {
        (self.exp_rate > 0 || self.gold_rate > 0 || self.qp_rate > 0) && self.ticks_left > 0
    }
}

/// Name lookup and the special variables' world primitives.
pub trait SubstHost {
    /// `happy_data`: the happy-hour rates and the ticks left. All zero when
    /// there is no happy hour.
    fn subst_happy_hour(&self) -> HappyHour;

    /// `CONFIG_DIAGONAL_DIRS`: whether diagonal directions exist
    /// (`DIR_COUNT` is 10 when set, 6 otherwise). `false` by default.
    fn subst_diagonal_dirs(&self) -> bool;
}

/// `sizeof(repl_str)` in `var_subst`: `MAX_INPUT_LENGTH - 20`, which leaves
/// room to prefix `"eval tmpvr "`. A replacement holds one byte less.
const REPL_MAX: usize = MAX_INPUT_LENGTH - 20 - 1;

/// How deeply `var_subst` may nest (a subfield, or a chained reference's
/// `eval`) before it gives up. Real scripts nest two or three levels.
pub const MAX_NESTING: u32 = 10;

/// How many `var_subst` entries one top-level call may make in all, nested or
/// not, before it gives up. Depth alone does not bound the work: a value that
/// reproduces its chained reference twice branches at every level.
pub const MAX_WORK: u32 = 300;

/// How many random directions `%random.dir%` tries before taking the first
/// exit.
pub const DIR_RETRIES: u32 = 1000;

/// Why [`nest_enter`] refused.
enum Refusal {
    /// A limit tripped on this call: log it.
    Tripped,
    /// An earlier call of this top-level substitution already gave up.
    Aborted,
}

/// Count a `var_subst` call into the nesting and work totals in
/// [`VmState`]; every `Ok` is paired with a [`nest_leave`].
fn nest_enter(vm: &mut VmState) -> Result<(), Refusal> {
    if vm.subst_aborted {
        return Err(Refusal::Aborted);
    }
    if vm.subst_nesting >= MAX_NESTING || vm.subst_work >= MAX_WORK {
        vm.subst_aborted = true;
        return Err(Refusal::Tripped);
    }
    vm.subst_nesting += 1;
    vm.subst_work += 1;
    Ok(())
}

/// End a `var_subst` call; the outermost one resets the work count and the
/// abort flag.
fn nest_leave(vm: &mut VmState) {
    vm.subst_nesting = vm.subst_nesting.saturating_sub(1);
    if vm.subst_nesting == 0 {
        vm.subst_work = 0;
        vm.subst_aborted = false;
    }
}

/// The byte at `i`, NUL past the end of the buffer.
fn at(buf: &[u8], i: usize) -> u8 {
    buf.get(i).copied().unwrap_or(0)
}

/// `buf[i] = byte`, dropped past the end of the buffer.
fn put(buf: &mut [u8], i: usize, byte: u8) {
    if let Some(slot) = buf.get_mut(i) {
        *slot = byte;
    }
}

/// The C string starting at `start`: the bytes up to the first NUL.
fn c_string(buf: &[u8], start: usize) -> String {
    let tail = buf.get(start..).unwrap_or(&[]);
    let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(tail.get(..end).unwrap_or(tail)).into_owned()
}

/// Cut `text` to at most `max` bytes, backing off to a character boundary.
fn cap(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The output buffer as text: a character cut at the end is dropped, any
/// other invalid byte becomes U+FFFD.
fn bytes_to_string(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let incomplete_tail = error.utf8_error().error_len().is_none();
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            if incomplete_tail {
                bytes.truncate(valid);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }
}

/// `var_subst(go, sc, trig, type, line, buf)`: replace every `%…%` in `line`.
pub fn var_subst<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, line: &str) -> String {
    // A C string ends at its first NUL.
    let line = line.split('\0').next().unwrap_or("");

    // A line this long does not fit the C's buffers: refuse it, run nothing.
    if line.len() >= MAX_INPUT_LENGTH {
        let shown = cap(line.to_owned(), 60);
        let message = format!(
            "Trigger: {}, VNum {}, type: {}. Line is {} characters, over the {} limit: '{}...'",
            cx.trig.proto.name,
            cx.trig.proto.vnum,
            cx.attach.index(),
            line.len(),
            MAX_INPUT_LENGTH - 1,
            shown
        );
        cx.host.script_log(&message);
        return String::new();
    }

    // skip out if no %'s
    if !line.contains('%') {
        return line.to_owned();
    }

    if let Err(refusal) = nest_enter(cx.host.vm_state()) {
        if matches!(refusal, Refusal::Tripped) {
            cx.log("variable substitution nested too deeply");
        }
        return String::new();
    }
    let result = subst_line(cx, line);
    nest_leave(cx.host.vm_state());
    result
}

/// The body of [`var_subst`] once the nesting counters have admitted it.
fn subst_line<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, line: &str) -> String {
    let mut tmp = vec![0u8; MAX_INPUT_LENGTH];
    if let Some(head) = tmp.get_mut(..line.len()) {
        head.copy_from_slice(line.as_bytes());
    }
    let mut subfield = vec![0u8; MAX_INPUT_LENGTH];
    let mut subfield_p: usize = 0;
    let mut out: Vec<u8> = Vec::with_capacity(line.len());
    let mut p: usize = 0;
    let mut left: usize = MAX_INPUT_LENGTH - 1;
    let mut paren_count: i32 = 0;

    while at(&tmp, p) != 0 && left > 0 && !cx.host.vm_state().subst_aborted {
        // copy until we find the first %
        while at(&tmp, p) != 0 && at(&tmp, p) != b'%' && left > 0 {
            out.push(at(&tmp, p));
            p += 1;
            left -= 1;
        }

        // double %
        if at(&tmp, p) != 0 {
            p += 1;
            if at(&tmp, p) == b'%' && left > 0 {
                out.push(b'%');
                p += 1;
                left -= 1;
                continue;
            }
        }

        // so it wasn't double %'s
        if at(&tmp, p) != 0 && left > 0 {
            // search until end of var or beginning of field
            let var = p;
            while at(&tmp, p) != 0 && at(&tmp, p) != b'%' && at(&tmp, p) != b'.' {
                p += 1;
            }

            let mut field = p;
            if at(&tmp, p) == b'.' {
                put(&mut tmp, p, 0);
                p += 1;
                let mut dots: i32 = 0;
                field = p;
                while at(&tmp, p) != 0
                    && (at(&tmp, p) != b'%' || paren_count > 0 || dots > 0)
                    && !cx.host.vm_state().subst_aborted
                {
                    if dots > 0 {
                        put(&mut subfield, subfield_p, 0);
                        let replacement = find_replacement(
                            cx,
                            &c_string(&tmp, var),
                            &c_string(&tmp, field),
                            &c_string(&subfield, 0),
                        );
                        if !replacement.is_empty() {
                            let command =
                                cap(format!("eval tmpvr {replacement}"), MAX_INPUT_LENGTH - 1);
                            process_eval(cx, &command);
                            for (offset, byte) in b"tmpvr\0".iter().enumerate() {
                                put(&mut tmp, var + offset, *byte);
                            }
                            field = p;
                            dots = 0;
                            p += 1;
                            continue;
                        }
                        dots = 0;
                    } else if at(&tmp, p) == b'(' {
                        put(&mut tmp, p, 0);
                        paren_count += 1;
                    } else if at(&tmp, p) == b')' {
                        put(&mut tmp, p, 0);
                        paren_count -= 1;
                    } else if paren_count > 0 {
                        put(&mut subfield, subfield_p, at(&tmp, p));
                        subfield_p += 1;
                    } else if at(&tmp, p) == b'.' {
                        put(&mut tmp, p, 0);
                        dots += 1;
                    }
                    p += 1;
                }
            }

            put(&mut tmp, p, 0);
            p += 1;
            put(&mut subfield, subfield_p, 0);

            if at(&subfield, 0) != 0 {
                let expanded = var_subst(cx, &c_string(&subfield, 0));
                // strcpy(subfield, tmp2)
                for (offset, byte) in expanded.bytes().take_while(|b| *b != 0).enumerate() {
                    put(&mut subfield, offset, byte);
                }
                put(
                    &mut subfield,
                    expanded.bytes().take_while(|b| *b != 0).count(),
                    0,
                );
            }

            // a limit tripped: run nothing further on this line
            if cx.host.vm_state().subst_aborted {
                break;
            }
            let replacement = find_replacement(
                cx,
                &c_string(&tmp, var),
                &c_string(&tmp, field),
                &c_string(&subfield, 0),
            );

            // strncat(buf, repl_str, left)
            let take = replacement.len().min(left);
            out.extend(replacement.bytes().take(take));
            left -= take;
        }
    }
    // A limit tripped somewhere below: the whole line substitutes to nothing,
    // as the over-length refusal does, rather than running half-expanded.
    if cx.host.vm_state().subst_aborted {
        return String::new();
    }
    cap(bytes_to_string(out), MAX_INPUT_LENGTH - 1)
}

/// `find_replacement(go, sc, trig, type, var, field, subfield, str, slen)`:
/// the value of one `%var.field(subfield)%` reference.
pub fn find_replacement<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    var: &str,
    field: &str,
    subfield: &str,
) -> String {
    let (owner, attach) = (cx.owner, cx.attach);
    cap(
        replace(cx, owner, true, attach, var, field, subfield),
        REPL_MAX,
    )
}

/// `find_replacement(go, SCRIPT(script_owner), NULL, MOB_TRIGGER, var, "", NULL, …)`:
/// the `%<char>.global(<var>)%` path, the one place the C calls
/// `find_replacement` with another entity's script and no trigger. Looks
/// `var` up in `script_owner`'s globals under that script's own context, then
/// falls back to the no-field specials with the mob spellings.
///
/// [`find_replacement`] intercepts the `global` field of an NPC that has a
/// script and calls this; `char_field` never sees that case.
pub fn find_global_replacement<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    script_owner: Uid,
    var: &str,
) -> String {
    cap(
        replace(cx, script_owner, false, AttachType::Mob, var, "", ""),
        REPL_MAX,
    )
}

/// What a reference resolved to: the C's `c`, `o` and `r`, of which at most
/// one is set.
#[derive(Clone, Copy)]
enum Ent {
    Char(Uid),
    Obj(Uid),
    Room(Uid),
    Nothing,
}

/// The body of `find_replacement`. `sc_owner` is the entity whose script is
/// `sc`; `has_trig` is whether `trig` is non-NULL (trigger-local variables
/// are searched only then); `attach` is `type`. `go` is `cx.owner`.
fn replace<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    sc_owner: Uid,
    has_trig: bool,
    attach: AttachType,
    var: &str,
    field: &str,
    subfield: &str,
) -> String {
    // X.global() will have a NULL trig
    let mut vd: Option<String> = None;
    if has_trig {
        vd = cx.trig.vars.get(var).map(|found| found.value.clone());
    }
    if vd.is_none() {
        vd = cx.host.script(sc_owner).and_then(|sc| {
            sc.global_vars
                .get_in_context(var, sc.context)
                .map(|found| found.value.clone())
        });
    }

    if field.is_empty() {
        return match vd {
            Some(value) => value,
            None => no_field_value(cx, attach, var),
        };
    }

    if let Some(result) = vd
        .as_ref()
        .and_then(|value| text_processed(&*cx.host, field, subfield, value))
    {
        return result;
    }

    let entity = match &vd {
        Some(name) => resolve_name(&*cx.host, attach, cx.owner, name),
        None => match special_var(cx, attach, var, field, subfield) {
            Special::Done(text) => return text,
            Special::Entity(entity) => entity,
        },
    };

    match entity {
        Ent::Char(c) => {
            // get global of something else
            if str_eq(field, "global") && cx.host.is_npc(c) && cx.host.script(c).is_some() {
                return replace(cx, c, false, AttachType::Mob, subfield, "", "");
            }
            match fields_char::char_field(cx, c, field, subfield) {
                Some(text) => text,
                None => match own_global(&*cx.host, c, field) {
                    Some(value) => value,
                    None => {
                        cx.log(&format!("unknown char field: '{field}'"));
                        String::new()
                    }
                },
            }
        }
        Ent::Obj(o) => match fields_obj::obj_field(cx, o, field, subfield) {
            Some(text) => text,
            None => match own_global(&*cx.host, o, field) {
                Some(value) => value,
                None => {
                    unknown_field(cx, attach, "object", field);
                    String::new()
                }
            },
        },
        Ent::Room(r) => {
            // The void (room 0) stores the 'full global' variables; room_field
            // answers for it.
            match fields_room::room_field(cx, r, field, subfield) {
                Some(text) => text,
                None => match own_global(&*cx.host, r, field) {
                    Some(value) => value,
                    None => {
                        unknown_field(cx, attach, "room", field);
                        String::new()
                    }
                },
            }
        }
        Ent::Nothing => String::new(),
    }
}

/// The entity's own script global named `field`, by name alone: the C's
/// `for (vd = SCRIPT(x)->global_vars; vd; vd = vd->next) if (!str_cmp(vd->name, field))`.
fn own_global<H: BaseHost + ?Sized>(host: &H, entity: Uid, field: &str) -> Option<String> {
    host.script(entity)
        .and_then(|sc| sc.global_vars.get(field))
        .map(|found| found.value.clone())
}

/// The log line for a field no table names on an object or room.
fn unknown_field<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    attach: AttachType,
    what: &str,
    field: &str,
) {
    let message = format!(
        "Trigger: {}, VNum {}, type: {}. unknown {} field: '{}'",
        cx.trig.proto.name,
        cx.trig.proto.vnum,
        attach.index(),
        what,
        field
    );
    cx.host.script_log(&message);
}

/// The `!*field` branch of `find_replacement` for a name that is not a
/// variable: `%self%`, `%global%` and the per-type command words.
fn no_field_value<H: DgHost + ?Sized>(cx: &Cx<'_, H>, attach: AttachType, var: &str) -> String {
    let index = attach.index();
    let words: Option<[&str; 3]> = if str_eq(var, "door") {
        Some(["mdoor ", "odoor ", "wdoor "])
    } else if str_eq(var, "force") {
        Some(["mforce ", "oforce ", "wforce "])
    } else if str_eq(var, "load") {
        Some(["mload ", "oload ", "wload "])
    } else if str_eq(var, "purge") {
        Some(["mpurge ", "opurge ", "wpurge "])
    } else if str_eq(var, "teleport") {
        Some(["mteleport ", "oteleport ", "wteleport "])
    } else if str_eq(var, "damage") {
        Some(["mdamage ", "odamage ", "wdamage "])
    } else if str_eq(var, "send") {
        Some(["msend ", "osend ", "wsend "])
    } else if str_eq(var, "echo") {
        Some(["mecho ", "oecho ", "wecho "])
    } else if str_eq(var, "echoaround") {
        Some(["mechoaround ", "oechoaround ", "wechoaround "])
    } else if str_eq(var, "zoneecho") {
        Some(["mzoneecho ", "ozoneecho ", "wzoneecho "])
    } else if str_eq(var, "asound") {
        Some(["masound ", "oasound ", "wasound "])
    } else if str_eq(var, "at") {
        Some(["mat ", "oat ", "wat "])
    } else if str_eq(var, "transform") {
        // there is no such thing as wtransform, thus the wecho
        Some(["mtransform ", "otransform ", "wecho "])
    } else if str_eq(var, "recho") {
        Some(["mrecho ", "orecho ", "wrecho "])
    } else if str_eq(var, "move") {
        // there is no such thing as mmove, thus the mecho
        Some(["mecho ", "omove ", "wmove "])
    } else if str_eq(var, "log") {
        Some(["mlog ", "olog ", "wlog "])
    } else {
        None
    };
    if let Some(words) = words {
        return words.get(index).copied().unwrap_or("").to_owned();
    }
    if str_eq(var, "self") {
        return uid_text(cx.owner);
    }
    if str_eq(var, "global") {
        // so "remote varname %global%" will work
        return ROOM_ID_BASE.to_string();
    }
    String::new()
}

/// What a special variable (no variable of that name) produced.
enum Special {
    /// The C wrote the result and returned (or fell through with nothing
    /// resolved): this is the final text.
    Done(String),
    /// `%self%`: continue with this entity.
    Entity(Ent),
}

/// `find_replacement`'s `else` branch for a name that is not a variable and
/// has a field: `self`, `global`, `people`, `happyhour`, `time`, `findmob`,
/// `findobj`, `random`.
fn special_var<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    attach: AttachType,
    var: &str,
    field: &str,
    subfield: &str,
) -> Special {
    let go = cx.owner;
    if str_eq(var, "self") {
        return Special::Entity(match attach {
            AttachType::Mob => Ent::Char(go),
            AttachType::Obj => Ent::Obj(go),
            AttachType::Wld => Ent::Room(go),
        });
    }
    if str_eq(var, "global") {
        let Some(void) = cx.host.real_room(0) else {
            cx.host
                .script_log("Attempt to find global var. Apparently the void has no script.");
            return Special::Done(String::new());
        };
        let Some(sc) = cx.host.script(void) else {
            cx.host
                .script_log("Attempt to find global var. Apparently the void has no script.");
            return Special::Done(String::new());
        };
        return Special::Done(
            sc.global_vars
                .get(field)
                .map(|found| found.value.clone())
                .unwrap_or_default(),
        );
    }
    if str_eq(var, "people") {
        let num = atoi(field);
        let count = if num > 0 {
            match trgvar_in_room(&*cx.host, idx(num)) {
                Some(count) => count,
                None => {
                    cx.host
                        .script_log("people.vnum: world[rnum] does not exist");
                    -1
                }
            }
        } else {
            0
        };
        return Special::Done(count.to_string());
    }
    if str_eq(var, "happyhour") {
        let happy = cx.host.subst_happy_hour();
        let value = if str_eq(field, "qp") && happy.is_active() {
            happy.qp_rate
        } else if str_eq(field, "exp") && happy.is_active() {
            happy.exp_rate
        } else if str_eq(field, "gold") && happy.is_active() {
            happy.gold_rate
        } else {
            happy.ticks_left
        };
        return Special::Done(value.to_string());
    }
    if str_eq(var, "time") {
        let time = cx.host.mud_time();
        return Special::Done(if str_eq(field, "hour") {
            time.hours.to_string()
        } else if str_eq(field, "day") {
            time.day.wrapping_add(1).to_string()
        } else if str_eq(field, "month") {
            time.month.wrapping_add(1).to_string()
        } else if str_eq(field, "year") {
            time.year.to_string()
        } else {
            String::new()
        });
    }
    // %findmob.<room vnum X>(<mob vnum Y>)% and %findobj.<room vnum X>(<object
    // vnum/id/name>)%: count the mobs / objects in room X.
    if str_eq(var, "findmob") {
        if field.is_empty() || subfield.is_empty() {
            cx.host.script_log("findmob.vnum(mvnum) - illegal syntax");
            return Special::Done("0".to_owned());
        }
        let Some(room) = cx.host.real_room(idx(atoi(field))) else {
            let message = format!("findmob.vnum(ovnum): No room with vnum {}", atoi(field));
            cx.host.script_log(&message);
            return Special::Done("0".to_owned());
        };
        let mvnum = idx(atoi(subfield));
        let count = cx
            .host
            .room_people(room)
            .into_iter()
            .filter(|ch| idx(cx.host.vnum(*ch)) == mvnum)
            .count();
        return Special::Done(count.to_string());
    }
    if str_eq(var, "findobj") {
        if field.is_empty() || subfield.is_empty() {
            cx.host.script_log("findobj.vnum(ovnum) - illegal syntax");
            return Special::Done("0".to_owned());
        }
        let Some(room) = cx.host.real_room(idx(atoi(field))) else {
            let message = format!("findobj.vnum(ovnum): No room with vnum {}", atoi(field));
            cx.host.script_log(&message);
            return Special::Done("0".to_owned());
        };
        // item_in_list looks within containers as well.
        let list = cx.host.room_contents(room);
        return Special::Done(fields_obj::item_in_list_c(&*cx.host, subfield, &list).to_string());
    }
    if str_eq(var, "random") {
        return Special::Done(random_var(cx, attach, field));
    }
    Special::Entity(Ent::Nothing)
}

/// An `int` stored into an `IDXTYPE` (`ush_int`): the low 16 bits.
fn idx(value: i32) -> i32 {
    i32::from(value as u16)
}

/// `%random.char%`, `%random.dir%` and `%random.<number>%`.
fn random_var<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, attach: AttachType, field: &str) -> String {
    let go = cx.owner;
    if str_eq(field, "char") {
        let candidates: Vec<Uid> = {
            let host = &*cx.host;
            match attach {
                AttachType::Mob => host
                    .in_room(go)
                    .map(|room| host.room_people(room))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| *c != go && host.valid_dg_target(*c, true) && host.can_see(go, *c))
                    .collect(),
                AttachType::Obj => obj_room(host, go)
                    .map(|room| host.room_people(room))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| host.valid_dg_target(*c, true))
                    .collect(),
                AttachType::Wld => host
                    .room_people(go)
                    .into_iter()
                    .filter(|c| host.valid_dg_target(*c, true))
                    .collect(),
            }
        };
        let mut chosen = None;
        for (count, c) in (0_i64..).zip(candidates) {
            if cx.host.rand_number(0, count) == 0 {
                chosen = Some(c);
            }
        }
        return chosen.map(uid_text).unwrap_or_default();
    }

    if str_eq(field, "dir") {
        let in_room = match attach {
            AttachType::Wld => {
                let number = cx.host.vnum(go);
                cx.host.real_room(number)
            }
            AttachType::Obj => obj_room(&*cx.host, go),
            AttachType::Mob => cx.host.in_room(go),
        };
        let Some(room) = in_room else {
            return String::new();
        };
        let dir_count = if cx.host.subst_diagonal_dirs() {
            NUM_OF_DIRS
        } else {
            6
        };
        let exits: Vec<usize> = (0..dir_count)
            .filter(|dir| cx.host.room_exit(room, *dir).is_some())
            .collect();
        let Some(first) = exits.first().copied() else {
            return String::new();
        };
        let mut chosen = first;
        for _ in 0..DIR_RETRIES {
            let roll = cx.host.rand_number(0, dir_count as i64 - 1);
            if let Some(dir) = usize::try_from(roll).ok().filter(|d| exits.contains(d)) {
                chosen = dir;
                break;
            }
        }
        return DIRS.get(chosen).copied().unwrap_or("").to_owned();
    }

    let num = atoi(field);
    if num > 0 {
        cx.host.rand_number(1, i64::from(num)).to_string()
    } else {
        "0".to_owned()
    }
}

/// Resolve a variable's value to a character, object or room by the owner's
/// type, in the C's order.
fn resolve_name<H: DgHost + ?Sized>(host: &H, attach: AttachType, go: Uid, name: &str) -> Ent {
    match attach {
        AttachType::Mob => {
            if let Some(o) = fields_obj::get_object_in_equip_c(host, go, name) {
                return Ent::Obj(o);
            }
            if let Some(o) = get_obj_in_list(host, name, &host.carrying(go)) {
                return Ent::Obj(o);
            }
            // The C indexes world[IN_ROOM(ch)] for the contents even when the
            // character is nowhere; here that is an empty list.
            let room = host.in_room(go);
            if let Some(c) = room.and_then(|room| get_char_in_room(host, room, name)) {
                return Ent::Char(c);
            }
            if let Some(o) =
                room.and_then(|room| get_obj_in_list(host, name, &host.room_contents(room)))
            {
                return Ent::Obj(o);
            }
            if let Some(c) = get_char(host, name) {
                return Ent::Char(c);
            }
            if let Some(o) = get_obj(host, name) {
                return Ent::Obj(o);
            }
        }
        AttachType::Obj => {
            if let Some(c) = get_char_by_obj(host, go, name) {
                return Ent::Char(c);
            }
            if let Some(o) = get_obj_by_obj(host, go, name) {
                return Ent::Obj(o);
            }
        }
        AttachType::Wld => {
            if let Some(c) = get_char_by_room(host, go, name) {
                return Ent::Char(c);
            }
            if let Some(o) = get_obj_by_room(host, go, name) {
                return Ent::Obj(o);
            }
        }
    }
    match get_room(host, name) {
        Some(r) => Ent::Room(r),
        None => Ent::Nothing,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::mock::MockHost;
    use crate::types::{TrigInstance, uid_text};

    /// A mob `guard` (vnum 3000) in room 3001 with a second mob `thug`
    /// (vnum 3001) and an object `sword` (vnum 3020) lying there.
    struct World {
        host: MockHost,
        room: Uid,
        guard: Uid,
        thug: Uid,
        sword: Uid,
        trig: TrigInstance,
    }

    fn world() -> World {
        let mut host = MockHost::new();
        let room = host.add_room(3001, "Temple");
        let guard = host.add_mob(3000, "guard");
        let thug = host.add_mob(3001, "thug");
        let sword = host.add_obj(3020, "sword");
        host.char_to_room(guard, room);
        host.char_to_room(thug, room);
        host.obj_to_room(sword, room);
        let proto = host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        let trig = TrigInstance::new(0, proto);
        World {
            host,
            room,
            guard,
            thug,
            sword,
            trig,
        }
    }

    /// Run `f` with a context owned by `owner` of type `attach`.
    fn run<T>(
        w: &mut World,
        owner: Uid,
        attach: AttachType,
        f: impl FnOnce(&mut Cx<'_, MockHost>) -> T,
    ) -> T {
        let mut cx = Cx {
            host: &mut w.host,
            owner,
            attach,
            trig: &mut w.trig,
        };
        f(&mut cx)
    }

    fn sub(w: &mut World, line: &str) -> String {
        let guard = w.guard;
        run(w, guard, AttachType::Mob, |cx| var_subst(cx, line))
    }

    fn set_global(w: &mut World, owner: Uid, name: &str, value: &str, context: i64) {
        w.host
            .ensure_script(owner)
            .unwrap()
            .global_vars
            .add_var(name, value, context);
    }

    #[test]
    fn no_percent_is_copied_verbatim() {
        let mut w = world();
        assert_eq!(sub(&mut w, ""), "");
        assert_eq!(sub(&mut w, "say hello there"), "say hello there");
        assert_eq!(sub(&mut w, "naïve ☃"), "naïve ☃");
    }

    #[test]
    fn double_percent_is_a_literal_percent() {
        let mut w = world();
        assert_eq!(sub(&mut w, "100%% sure"), "100% sure");
        assert_eq!(sub(&mut w, "%%"), "%");
        assert_eq!(sub(&mut w, "%%%%"), "%%");
        // %%% is a literal percent and then a lone, unterminated one.
        assert_eq!(sub(&mut w, "a%%%"), "a%");
    }

    #[test]
    fn a_trailing_lone_percent_is_dropped() {
        let mut w = world();
        // The ++p after the first % reaches the NUL: nothing else happens.
        assert_eq!(sub(&mut w, "50%"), "50");
    }

    #[test]
    fn plain_variables_come_from_the_trigger_then_the_script() {
        let mut w = world();
        w.trig.vars.add_var("who", "world", 0);
        assert_eq!(sub(&mut w, "hello %who%!"), "hello world!");
        // names are case-insensitive
        assert_eq!(sub(&mut w, "%WHO%%who%"), "worldworld");
        // an undefined variable is empty
        assert_eq!(sub(&mut w, "[%nope%]"), "[]");
        // a global is found when the trigger has no such local
        let guard = w.guard;
        set_global(&mut w, guard, "g", "global value", 0);
        assert_eq!(sub(&mut w, "%g%"), "global value");
        // the trigger's local wins over a global of the same name
        w.trig.vars.add_var("g", "local", 0);
        assert_eq!(sub(&mut w, "%g%"), "local");
    }

    #[test]
    fn globals_obey_the_scripts_context() {
        let mut w = world();
        let guard = w.guard;
        set_global(&mut w, guard, "ctx", "five", 5);
        assert_eq!(sub(&mut w, "[%ctx%]"), "[]");
        w.host.script_mut(guard).unwrap().context = 5;
        assert_eq!(sub(&mut w, "[%ctx%]"), "[five]");
        w.host.script_mut(guard).unwrap().context = 6;
        assert_eq!(sub(&mut w, "[%ctx%]"), "[]");
    }

    #[test]
    fn an_unterminated_reference_still_substitutes() {
        let mut w = world();
        w.trig.vars.add_var("v", "W", 0);
        // The scan stops at the NUL, the variable is replaced, the loop ends.
        assert_eq!(sub(&mut w, "hi %v"), "hi W");
        assert_eq!(sub(&mut w, "%v"), "W");
        assert_eq!(sub(&mut w, "hi %nope"), "hi ");
    }

    #[test]
    fn a_line_over_the_limit_is_refused_and_logged() {
        let mut w = world();
        let long = "a".repeat(MAX_INPUT_LENGTH);
        assert_eq!(sub(&mut w, &long), "");
        assert_eq!(w.host.logs.len(), 1);
        let log = &w.host.logs[0];
        assert!(log.starts_with("Trigger: trigger 7, VNum 7, type: 0. Line is 512 characters"));
        assert!(log.contains("over the 511 limit: '"));
        assert!(log.ends_with(&format!("{}...'", "a".repeat(60))));

        // 511 characters is accepted, with or without a percent.
        let ok = "a".repeat(MAX_INPUT_LENGTH - 1);
        assert_eq!(sub(&mut w, &ok), ok);
        let with_pct = format!("{}%v%", "a".repeat(MAX_INPUT_LENGTH - 5));
        w.trig.vars.add_var("v", "xyz", 0);
        // 507 copied, left is 4, "xyz" fits.
        assert_eq!(sub(&mut w, &with_pct).len(), MAX_INPUT_LENGTH - 5 + 3);
    }

    #[test]
    fn the_refusal_log_does_not_split_a_character() {
        let mut w = world();
        let long = "é".repeat(MAX_INPUT_LENGTH);
        assert_eq!(sub(&mut w, &long), "");
        assert!(w.host.logs[0].ends_with("...'"));
    }

    #[test]
    fn the_output_is_truncated_to_the_left_budget() {
        let mut w = world();
        w.trig.vars.add_var("big", &"b".repeat(300), 0);
        let line = format!("{}%big%tail", "a".repeat(300));
        let out = sub(&mut w, &line);
        // left starts at 511: 300 copied, 211 of the replacement, then stop.
        assert_eq!(out.len(), 511);
        assert!(out.ends_with(&"b".repeat(211)));
        assert!(!out.contains("tail"));
    }

    #[test]
    fn an_exhausted_budget_drops_what_follows() {
        let mut w = world();
        w.trig.vars.add_var("big", &"b".repeat(300), 0);
        let line = format!("{}%big%%%x%big%", "a".repeat(300));
        let out = sub(&mut w, &line);
        // 300 + 211 fills left; the %% and everything after are not reached.
        assert_eq!(out.len(), 511);
        assert!(!out.contains('%') && !out.contains('x'));
        // The text copy loop stops when left runs out: 200 + 300 + 11.
        let line = format!("{}%big%{}", "a".repeat(200), "c".repeat(100));
        let out = sub(&mut w, &line);
        assert_eq!(out.len(), 511);
        assert!(out.ends_with(&format!("{}{}", "b".repeat(300), "c".repeat(11))));
        let line = format!("{}%big%{}", "a".repeat(250), "c".repeat(100));
        let out = sub(&mut w, &line);
        assert_eq!(out.len(), 511);
        assert!(out.ends_with(&"b".repeat(261)));
    }

    #[test]
    fn a_replacement_is_cut_to_the_repl_buffer() {
        let mut w = world();
        w.trig.vars.add_var("v", &"c".repeat(600), 0);
        // repl_str holds MAX_INPUT_LENGTH - 20 bytes including the NUL.
        assert_eq!(sub(&mut w, "%v%").len(), 491);
    }

    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        let mut w = world();
        w.trig.vars.add_var("v", &"é".repeat(300), 0);
        // 491 bytes would end inside an é; the cut backs off to 490.
        let out = sub(&mut w, "%v%");
        assert_eq!(out.len(), 490);
        assert!(out.chars().all(|c| c == 'é'));
        // And the output budget: 3 bytes are left, which hold one é and half
        // of the next, which is dropped.
        let line = format!("{}%v%", "a".repeat(508));
        let out = sub(&mut w, &line);
        assert_eq!(out, format!("{}é", "a".repeat(508)));
    }

    #[test]
    fn non_ascii_text_around_references_survives() {
        let mut w = world();
        w.trig.vars.add_var("v", "ü", 0);
        assert_eq!(sub(&mut w, "é%v%☃%%"), "éü☃%");
        assert_eq!(sub(&mut w, "%nöpe%"), "");
        assert_eq!(sub(&mut w, "%v.zzé(ü)%"), "");
    }

    #[test]
    fn a_nul_ends_the_line() {
        let mut w = world();
        w.trig.vars.add_var("v", "x", 0);
        assert_eq!(sub(&mut w, "a%v%\0b%v%"), "ax");
    }

    #[test]
    fn command_words_follow_the_owner_type() {
        let mut w = world();
        let (guard, sword, room) = (w.guard, w.sword, w.room);
        let words = [
            ("send", ["msend ", "osend ", "wsend "]),
            ("echo", ["mecho ", "oecho ", "wecho "]),
            (
                "echoaround",
                ["mechoaround ", "oechoaround ", "wechoaround "],
            ),
            ("door", ["mdoor ", "odoor ", "wdoor "]),
            ("force", ["mforce ", "oforce ", "wforce "]),
            ("load", ["mload ", "oload ", "wload "]),
            ("purge", ["mpurge ", "opurge ", "wpurge "]),
            ("teleport", ["mteleport ", "oteleport ", "wteleport "]),
            ("damage", ["mdamage ", "odamage ", "wdamage "]),
            ("zoneecho", ["mzoneecho ", "ozoneecho ", "wzoneecho "]),
            ("asound", ["masound ", "oasound ", "wasound "]),
            ("at", ["mat ", "oat ", "wat "]),
            ("transform", ["mtransform ", "otransform ", "wecho "]),
            ("recho", ["mrecho ", "orecho ", "wrecho "]),
            ("move", ["mecho ", "omove ", "wmove "]),
            ("log", ["mlog ", "olog ", "wlog "]),
        ];
        for (var, spelled) in words {
            let line = format!("%{var}%");
            assert_eq!(
                run(&mut w, guard, AttachType::Mob, |cx| var_subst(cx, &line)),
                spelled[0],
                "{var} mob"
            );
            assert_eq!(
                run(&mut w, sword, AttachType::Obj, |cx| var_subst(cx, &line)),
                spelled[1],
                "{var} obj"
            );
            assert_eq!(
                run(&mut w, room, AttachType::Wld, |cx| var_subst(cx, &line)),
                spelled[2],
                "{var} wld"
            );
        }
        // Case-insensitive, and a trigger variable of that name wins.
        assert_eq!(sub(&mut w, "%SEND%"), "msend ");
        w.trig.vars.add_var("send", "mine", 0);
        assert_eq!(sub(&mut w, "%send%"), "mine");
    }

    #[test]
    fn self_and_global_without_a_field() {
        let mut w = world();
        let (guard, sword, room) = (w.guard, w.sword, w.room);
        assert_eq!(sub(&mut w, "%self%"), uid_text(guard));
        assert_eq!(
            run(&mut w, sword, AttachType::Obj, |cx| var_subst(cx, "%self%")),
            uid_text(sword)
        );
        assert_eq!(
            run(&mut w, room, AttachType::Wld, |cx| var_subst(cx, "%self%")),
            uid_text(room)
        );
        assert_eq!(sub(&mut w, "%global%"), "20000000");
        assert_eq!(sub(&mut w, "%GLOBAL%"), "20000000");
    }

    #[test]
    fn time_fields() {
        let mut w = world();
        w.host.time = crate::types::MudTime {
            hours: 13,
            day: 4,
            month: 11,
            year: 650,
        };
        assert_eq!(sub(&mut w, "%time.hour%"), "13");
        assert_eq!(sub(&mut w, "%time.day%"), "5");
        assert_eq!(sub(&mut w, "%time.month%"), "12");
        assert_eq!(sub(&mut w, "%time.year%"), "650");
        assert_eq!(sub(&mut w, "[%time.week%]"), "[]");
        w.host.time.day = i32::MAX;
        assert_eq!(sub(&mut w, "%time.day%"), i32::MIN.to_string());
    }

    #[test]
    fn people_counts_a_rooms_occupants() {
        let mut w = world();
        assert_eq!(sub(&mut w, "%people.3001%"), "2");
        assert_eq!(sub(&mut w, "%people.9999%"), "-1");
        assert_eq!(
            w.host.logs,
            vec!["people.vnum: world[rnum] does not exist".to_owned()]
        );
        // a non-positive vnum is 0 without a lookup
        assert_eq!(sub(&mut w, "%people.0%"), "0");
        assert_eq!(sub(&mut w, "%people.-4%"), "0");
        assert_eq!(sub(&mut w, "%people.abc%"), "0");
        assert_eq!(w.host.logs.len(), 1);
    }

    #[test]
    fn happyhour_reports_rates_only_while_active() {
        let mut w = world();
        let set = |w: &mut World, qp: i64, exp: i64, gold: i64, ticks: i64| {
            for (key, value) in [("qp", qp), ("exp", exp), ("gold", gold), ("ticks", ticks)] {
                w.host.globals.nums.insert(format!("happy:{key}"), value);
            }
        };
        set(&mut w, 25, 50, 75, 100);
        assert_eq!(sub(&mut w, "%happyhour.qp%"), "25");
        assert_eq!(sub(&mut w, "%happyhour.exp%"), "50");
        assert_eq!(sub(&mut w, "%happyhour.gold%"), "75");
        assert_eq!(sub(&mut w, "%happyhour.time%"), "100");
        assert_eq!(sub(&mut w, "%happyhour.whatever%"), "100");
        // no ticks left: not a happy hour, every field is the time
        set(&mut w, 25, 50, 75, 0);
        assert_eq!(sub(&mut w, "%happyhour.qp%"), "0");
        // ticks but every rate zero: not a happy hour either
        set(&mut w, 0, 0, 0, 9);
        assert_eq!(sub(&mut w, "%happyhour.exp%"), "9");
        // one positive rate is enough
        set(&mut w, 0, 0, 3, 9);
        assert_eq!(sub(&mut w, "%happyhour.exp%"), "0");
        assert_eq!(sub(&mut w, "%happyhour.gold%"), "3");
    }

    #[test]
    fn findmob_counts_by_vnum_and_players_are_minus_one() {
        let mut w = world();
        let player = w.host.add_player("Bob");
        let room = w.room;
        w.host.char_to_room(player, room);
        let extra = w.host.add_mob(3000, "guard");
        w.host.char_to_room(extra, room);
        assert_eq!(sub(&mut w, "%findmob.3001(3000)%"), "2");
        assert_eq!(sub(&mut w, "%findmob.3001(3001)%"), "1");
        assert_eq!(sub(&mut w, "%findmob.3001(-1)%"), "1");
        assert_eq!(sub(&mut w, "%findmob.3001(77)%"), "0");
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn findmob_and_findobj_report_bad_syntax_and_bad_rooms() {
        let mut w = world();
        assert_eq!(sub(&mut w, "%findmob.3001%"), "0");
        assert_eq!(sub(&mut w, "%findmob.3001()%"), "0");
        assert_eq!(sub(&mut w, "%findmob.9999(1)%"), "0");
        assert_eq!(sub(&mut w, "%findobj.3001%"), "0");
        assert_eq!(sub(&mut w, "%findobj.9999(5)%"), "0");
        assert_eq!(
            w.host.logs,
            vec![
                "findmob.vnum(mvnum) - illegal syntax".to_owned(),
                "findmob.vnum(mvnum) - illegal syntax".to_owned(),
                "findmob.vnum(ovnum): No room with vnum 9999".to_owned(),
                "findobj.vnum(ovnum) - illegal syntax".to_owned(),
                "findobj.vnum(ovnum): No room with vnum 9999".to_owned(),
            ]
        );
    }

    #[test]
    fn findobj_counts_by_vnum_id_or_name_through_containers() {
        let mut w = world();
        let (room, sword) = (w.room, w.sword);
        let bag = w.host.add_obj(3030, "bag");
        w.host.set_num(bag, "type", 15);
        w.host.obj_to_room(bag, room);
        let coin = w.host.add_obj(3020, "coin");
        w.host.obj_to_obj(coin, bag);
        assert_eq!(sub(&mut w, "%findobj.3001(3020)%"), "2");
        assert_eq!(sub(&mut w, "%findobj.3001(bag)%"), "1");
        assert_eq!(
            sub(&mut w, &format!("%findobj.3001({})%", uid_text(sword))),
            "1"
        );
        assert_eq!(sub(&mut w, "%findobj.3001(9)%"), "0");
        // The object vnum is an IDXTYPE too: 68556 is 3020 (item_in_list).
        assert_eq!(sub(&mut w, "%findobj.3001(68556)%"), "2");
    }

    #[test]
    fn random_number_rolls_in_range() {
        let mut w = world();
        w.host.rolls.extend([7, 100, 0]);
        assert_eq!(sub(&mut w, "%random.10%"), "7");
        // clamped by the mock into 1..=20
        assert_eq!(sub(&mut w, "%random.20%"), "20");
        assert_eq!(sub(&mut w, "%random.20%"), "1");
        // non-positive or non-numeric: 0, no roll
        w.host.rolls.push_back(5);
        assert_eq!(sub(&mut w, "%random.0%"), "0");
        assert_eq!(sub(&mut w, "%random.-3%"), "0");
        assert_eq!(sub(&mut w, "%random.x%"), "0");
        assert_eq!(w.host.rolls.len(), 1);
    }

    #[test]
    fn random_char_picks_among_visible_targets_for_a_mob() {
        let mut w = world();
        let (guard, thug, room) = (w.guard, w.thug, w.room);
        let player = w.host.add_player("Bob");
        let ghost = w.host.add_player("Ghost");
        let god = w.host.add_player("Hidden");
        w.host.char_to_room(player, room);
        w.host.char_to_room(ghost, room);
        w.host.char_to_room(god, room);
        w.host.set_num(ghost, "dg_invalid", 1);
        w.host
            .entity_mut(guard)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![god]);
        // people list, head first: god, ghost, player, thug, guard.
        // Candidates: player, thug (guard is self, ghost is not a target,
        // god cannot be seen). rand_number(0, count) == 0 replaces the pick:
        // roll 0 at count 0 picks player; at count 1 a roll of 1 keeps it.
        w.host.rolls.extend([0, 1]);
        assert_eq!(sub(&mut w, "%random.char%"), uid_text(player));
        // A 0 on the second candidate takes it.
        w.host.rolls.extend([0, 0]);
        assert_eq!(sub(&mut w, "%random.char%"), uid_text(thug));
    }

    #[test]
    fn random_char_for_an_object_or_room_allows_gods() {
        // find_replacement: `valid_dg_target(c, DG_ALLOW_GODS)` for the
        // object and world cases, so a level 34 god without nohassle is a
        // target; with nohassle it is not.
        let mut w = world();
        let sword = w.sword;
        let hall = w.host.add_room(1, "Hall");
        let god = w.host.add_player("Zeus");
        w.host.set_num(god, "level", 34);
        w.host.char_to_room(god, hall);
        w.host.obj_to_room(sword, hall);
        let expect = uid_text(god);
        assert_eq!(
            run(&mut w, sword, AttachType::Obj, |cx| var_subst(
                cx,
                "%random.char%"
            )),
            expect
        );
        assert_eq!(
            run(&mut w, hall, AttachType::Wld, |cx| var_subst(
                cx,
                "%random.char%"
            )),
            expect
        );
        w.host.set_nohassle(god, true);
        assert_eq!(
            run(&mut w, hall, AttachType::Wld, |cx| var_subst(
                cx,
                "[%random.char%]"
            )),
            "[]"
        );
    }

    #[test]
    fn random_dir_names_an_exit_that_leads_nowhere() {
        // `for (i = 0; i < DIR_COUNT; i++) if (R_EXIT(room, i)) doors++;`:
        // an exit to NOWHERE is still an exit, so the only one, south, is
        // the answer.
        let mut w = world();
        let (guard, room) = (w.guard, w.room);
        w.host.set_num(room, "exit:2:nowhere", 1);
        assert_eq!(sub(&mut w, "%random.dir%"), "south");
        w.host.rolls.push_back(5);
        w.host.rolls.push_back(2);
        assert_eq!(
            run(&mut w, guard, AttachType::Mob, |cx| var_subst(
                cx,
                "%random.dir%"
            )),
            "south"
        );
    }

    #[test]
    fn a_mob_names_the_character_in_its_own_room_before_any_elsewhere() {
        // find_replacement (mob): get_char_in_room(world[IN_ROOM(ch)], name)
        // comes before get_char / get_obj over the whole world.
        let mut w = world();
        let hall = w.host.add_room(1, "Hall");
        let far = w.host.add_mob(4000, "rat");
        w.host.char_to_room(far, hall);
        let near = w.host.add_mob(4001, "rat");
        let room = w.room;
        w.host.char_to_room(near, room);
        // One more elsewhere, created after, so either end of the world's
        // character list is a rat that is not in this room.
        let far_after = w.host.add_mob(4002, "rat");
        w.host.char_to_room(far_after, hall);
        w.trig.vars.add_var("who", "rat", 0);
        assert_eq!(sub(&mut w, "%who.vnum%"), "4001");
    }

    #[test]
    fn random_char_for_objects_and_rooms_and_nobody() {
        let mut w = world();
        let (sword, room, guard) = (w.sword, w.room, w.guard);
        // An object lying in the room sees everyone there, itself excluded
        // by nature. People: thug, guard (head first); nothing rolled = 0,
        // so each candidate replaces the last.
        assert_eq!(
            run(&mut w, sword, AttachType::Obj, |cx| var_subst(
                cx,
                "%random.char%"
            )),
            uid_text(guard)
        );
        assert_eq!(
            run(&mut w, room, AttachType::Wld, |cx| var_subst(
                cx,
                "%random.char%"
            )),
            uid_text(guard)
        );
        // A carried object is in its carrier's room.
        w.host.obj_to_char(sword, guard);
        assert_eq!(
            run(&mut w, sword, AttachType::Obj, |cx| var_subst(
                cx,
                "%random.char%"
            )),
            uid_text(guard)
        );
        // An empty room yields nothing.
        let empty = w.host.add_room(1, "Empty");
        assert_eq!(
            run(&mut w, empty, AttachType::Wld, |cx| var_subst(
                cx,
                "[%random.char%]"
            )),
            "[]"
        );
        // A mob that is nowhere sees nobody.
        let lost = w.host.add_mob(5, "lost");
        assert_eq!(
            run(&mut w, lost, AttachType::Mob, |cx| var_subst(
                cx,
                "[%random.char%]"
            )),
            "[]"
        );
    }

    #[test]
    fn random_dir_picks_an_existing_exit() {
        let mut w = world();
        let (room, guard, sword) = (w.room, w.guard, w.sword);
        // no exits at all
        assert_eq!(sub(&mut w, "[%random.dir%]"), "[]");
        let other = w.host.add_room(3002, "Altar");
        w.host.set_link(room, "exit:1", Some(other));
        w.host.set_link(room, "exit:5", Some(other));
        // rolls: 0 (no exit), 3 (no exit), 5 (down)
        w.host.rolls.extend([0, 3, 5]);
        assert_eq!(sub(&mut w, "%random.dir%"), "down");
        w.host.rolls.extend([1]);
        assert_eq!(sub(&mut w, "%random.dir%"), "east");
        // the same room seen from an object lying in it and from the room
        w.host.rolls.extend([1]);
        assert_eq!(
            run(&mut w, sword, AttachType::Obj, |cx| var_subst(
                cx,
                "%random.dir%"
            )),
            "east"
        );
        w.host.rolls.extend([5]);
        assert_eq!(
            run(&mut w, room, AttachType::Wld, |cx| var_subst(
                cx,
                "%random.dir%"
            )),
            "down"
        );
        // a mob that is nowhere has no room
        let lost = w.host.add_mob(5, "lost");
        assert_eq!(
            run(&mut w, lost, AttachType::Mob, |cx| var_subst(
                cx,
                "[%random.dir%]"
            )),
            "[]"
        );
        let _ = guard;
    }

    #[test]
    fn random_dir_is_bounded_when_the_rolls_never_hit() {
        let mut w = world();
        let room = w.room;
        let other = w.host.add_room(3002, "Altar");
        w.host.set_link(room, "exit:2", Some(other));
        // The mock returns the low bound (0, north) forever: no exit there.
        assert_eq!(sub(&mut w, "%random.dir%"), "south");
    }

    #[test]
    fn global_reads_the_voids_script() {
        let mut w = world();
        assert_eq!(sub(&mut w, "[%global.cnt%]"), "[]");
        assert_eq!(
            w.host.logs,
            vec!["Attempt to find global var. Apparently the void has no script.".to_owned()]
        );
        let void = w.host.add_room(0, "The Void");
        assert_eq!(sub(&mut w, "[%global.cnt%]"), "[]");
        assert_eq!(w.host.logs.len(), 2);
        set_global(&mut w, void, "cnt", "42", 0);
        assert_eq!(sub(&mut w, "%global.cnt%"), "42");
        assert_eq!(sub(&mut w, "%GLOBAL.CNT%"), "42");
        // name only: a contextual global of the void is found whatever the
        // context
        set_global(&mut w, void, "ctx", "c", 9);
        assert_eq!(sub(&mut w, "%global.ctx%"), "c");
        assert_eq!(sub(&mut w, "[%global.none%]"), "[]");
    }

    #[test]
    fn a_variable_naming_the_void_reads_its_globals() {
        let mut w = world();
        w.trig.vars.add_var("v", "0", 0);
        // get_room("0") is the void, which has no script yet.
        let void = w.host.add_room(0, "The Void");
        assert_eq!(sub(&mut w, "[%v.cnt%]"), "[]");
        assert_eq!(
            w.host.logs,
            vec!["Trigger: trigger 7, Vnum 7, type 0. Trying to access Global var list of void. Apparently this has not been set up!".to_owned()]
        );
        set_global(&mut w, void, "cnt", "8", 0);
        assert_eq!(sub(&mut w, "%v.cnt%"), "8");
        // a missing variable on the void is silent
        assert_eq!(sub(&mut w, "[%v.zz%]"), "[]");
        assert_eq!(w.host.logs.len(), 1);
    }

    #[test]
    fn a_character_field_not_in_the_table_falls_back_to_its_globals() {
        let mut w = world();
        let thug = w.thug;
        w.trig.vars.add_var("v", "thug", 0);
        set_global(&mut w, thug, "zzmood", "angry", 0);
        assert_eq!(sub(&mut w, "%v.zzmood%"), "angry");
        // the fallback ignores contexts
        set_global(&mut w, thug, "zzctx", "any", 3);
        assert_eq!(sub(&mut w, "%v.zzctx%"), "any");
        // first match by name, head first
        set_global(&mut w, thug, "zzmood", "calm", 0);
        assert_eq!(sub(&mut w, "%v.zzmood%"), "calm");
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn an_unknown_character_field_is_logged() {
        let mut w = world();
        w.trig.vars.add_var("v", "thug", 0);
        // thug has no script
        assert_eq!(sub(&mut w, "[%v.zzfield%]"), "[]");
        // thug has a script without that variable
        let thug = w.thug;
        set_global(&mut w, thug, "other", "x", 0);
        assert_eq!(sub(&mut w, "[%v.zzfield%]"), "[]");
        assert_eq!(
            w.host.logs,
            vec![
                "Trigger: trigger 7, VNum 7. unknown char field: 'zzfield'".to_owned(),
                "Trigger: trigger 7, VNum 7. unknown char field: 'zzfield'".to_owned(),
            ]
        );
    }

    #[test]
    fn self_with_a_field_routes_by_owner_type() {
        let mut w = world();
        let (guard, sword, room) = (w.guard, w.sword, w.room);
        set_global(&mut w, guard, "zzg", "mob", 0);
        set_global(&mut w, sword, "zzg", "obj", 0);
        set_global(&mut w, room, "zzg", "room", 0);
        assert_eq!(sub(&mut w, "%self.zzg%"), "mob");
        assert_eq!(
            run(&mut w, sword, AttachType::Obj, |cx| var_subst(
                cx,
                "%self.zzg%"
            )),
            "obj"
        );
        assert_eq!(
            run(&mut w, room, AttachType::Wld, |cx| var_subst(
                cx,
                "%self.zzg%"
            )),
            "room"
        );
    }

    #[test]
    fn unknown_object_and_room_fields_log_with_the_owner_type() {
        let mut w = world();
        let (sword, room) = (w.sword, w.room);
        w.trig.vars.add_var("o", "sword", 0);
        w.trig.vars.add_var("r", "3001", 0);
        // From a mob: "sword" resolves to the object lying in the room.
        assert_eq!(sub(&mut w, "[%o.zzf%]"), "[]");
        // "3001" names no object or character, so it is the room.
        assert_eq!(sub(&mut w, "[%r.zzf%]"), "[]");
        assert_eq!(
            w.host.logs,
            vec![
                "Trigger: trigger 7, VNum 7, type: 0. unknown object field: 'zzf'".to_owned(),
                "Trigger: trigger 7, VNum 7, type: 0. unknown room field: 'zzf'".to_owned(),
            ]
        );
        // Their own globals are found first.
        set_global(&mut w, sword, "zzf", "o-value", 0);
        set_global(&mut w, room, "zzf", "r-value", 0);
        assert_eq!(sub(&mut w, "%o.zzf%"), "o-value");
        assert_eq!(sub(&mut w, "%r.zzf%"), "r-value");
        assert_eq!(w.host.logs.len(), 2);
    }

    #[test]
    fn the_type_in_the_log_is_the_owners() {
        let mut w = world();
        let sword = w.sword;
        w.trig.vars.add_var("o", "sword", 0);
        run(&mut w, sword, AttachType::Obj, |cx| {
            // sword resolves via get_obj_by_obj (itself or the world)
            assert_eq!(var_subst(cx, "%o.zzf%"), "");
        });
        assert_eq!(
            w.host.logs,
            vec!["Trigger: trigger 7, VNum 7, type: 1. unknown object field: 'zzf'".to_owned()]
        );
    }

    #[test]
    fn resolution_order_for_a_mob_owner() {
        let mut w = world();
        let (guard, thug, sword, room) = (w.guard, w.thug, w.sword, w.room);
        // A carried item shadows a character and an object in the room.
        let blade = w.host.add_obj(5, "thug blade");
        w.host.obj_to_char(blade, guard);
        set_global(&mut w, blade, "zzwho", "carried", 0);
        set_global(&mut w, thug, "zzwho", "thug", 0);
        w.trig.vars.add_var("v", "thug", 0);
        assert_eq!(sub(&mut w, "%v.zzwho%"), "carried");
        // Worn equipment comes before the inventory.
        let helm = w.host.add_obj(6, "thug helm");
        w.host.equip_char(guard, helm, 6);
        set_global(&mut w, helm, "zzwho", "worn", 0);
        assert_eq!(sub(&mut w, "%v.zzwho%"), "worn");
        // Without items, a character in the room comes before an object there.
        w.host.extract_obj(blade);
        w.host.extract_obj(helm);
        let _ = w.host.add_obj(7, "thug");
        assert_eq!(sub(&mut w, "%v.zzwho%"), "thug");
        // An object in the room before the world at large.
        let far = w.host.add_room(9, "Far");
        let stranger = w.host.add_mob(8, "wizard");
        w.host.char_to_room(stranger, far);
        let stick = w.host.add_obj(9, "wizard stick");
        w.host.obj_to_room(stick, room);
        set_global(&mut w, stranger, "zzwho", "wizard", 0);
        set_global(&mut w, stick, "zzwho", "stick", 0);
        w.trig.vars.add_var("v", "wizard", 0);
        assert_eq!(sub(&mut w, "%v.zzwho%"), "stick");
        // A character elsewhere in the world is found by get_char.
        w.host.extract_obj(stick);
        assert_eq!(sub(&mut w, "%v.zzwho%"), "wizard");
        // An id resolves directly.
        w.trig.vars.add_var("v", &uid_text(sword), 0);
        set_global(&mut w, sword, "zzwho", "sword", 0);
        assert_eq!(sub(&mut w, "%v.zzwho%"), "sword");
        // Nothing matches: empty and silent.
        w.trig.vars.add_var("v", "nonesuch", 0);
        assert_eq!(sub(&mut w, "[%v.zzwho%]"), "[]");
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn a_value_naming_a_room_by_vnum_resolves_a_room() {
        let mut w = world();
        let other = w.host.add_room(3002, "Altar");
        set_global(&mut w, other, "zzr", "altar", 0);
        w.trig.vars.add_var("v", "3002", 0);
        assert_eq!(sub(&mut w, "%v.zzr%"), "altar");
        w.trig.vars.add_var("v", &uid_text(other), 0);
        assert_eq!(sub(&mut w, "%v.zzr%"), "altar");
    }

    #[test]
    fn object_owners_resolve_through_their_holder() {
        let mut w = world();
        let (sword, guard) = (w.sword, w.guard);
        w.host.obj_to_char(sword, guard);
        set_global(&mut w, guard, "zzh", "holder", 0);
        w.trig.vars.add_var("v", "guard", 0);
        assert_eq!(
            run(&mut w, sword, AttachType::Obj, |cx| var_subst(
                cx, "%v.zzh%"
            )),
            "holder"
        );
    }

    #[test]
    fn room_owners_resolve_people_then_objects() {
        let mut w = world();
        let (room, thug, sword) = (w.room, w.thug, w.sword);
        set_global(&mut w, thug, "zzh", "thug", 0);
        set_global(&mut w, sword, "zzh", "sword", 0);
        w.trig.vars.add_var("v", "thug", 0);
        assert_eq!(
            run(&mut w, room, AttachType::Wld, |cx| var_subst(cx, "%v.zzh%")),
            "thug"
        );
        w.trig.vars.add_var("v", "sword", 0);
        assert_eq!(
            run(&mut w, room, AttachType::Wld, |cx| var_subst(cx, "%v.zzh%")),
            "sword"
        );
    }

    #[test]
    fn npc_global_reads_another_scripts_variable() {
        let mut w = world();
        let thug = w.thug;
        set_global(&mut w, thug, "zzmood", "angry", 0);
        w.trig.vars.add_var("v", "thug", 0);
        assert_eq!(sub(&mut w, "%v.global(zzmood)%"), "angry");
        assert_eq!(sub(&mut w, "%v.global(ZZMOOD)%"), "angry");
        // a variable the script lacks is empty and silent (no unknown-field log)
        assert_eq!(sub(&mut w, "[%v.global(none)%]"), "[]");
        assert!(w.host.logs.is_empty());
        // the other script's own context applies
        set_global(&mut w, thug, "zzctx", "c", 4);
        assert_eq!(sub(&mut w, "[%v.global(zzctx)%]"), "[]");
        w.host.script_mut(thug).unwrap().context = 4;
        assert_eq!(sub(&mut w, "%v.global(zzctx)%"), "c");
        // the trigger's own variables are not searched (NULL trig) ...
        w.trig.vars.add_var("mine", "local", 0);
        assert_eq!(sub(&mut w, "[%v.global(mine)%]"), "[]");
        // ... but the no-field specials are, with the mob spellings
        assert_eq!(sub(&mut w, "%v.global(send)%"), "msend ");
        assert_eq!(sub(&mut w, "%v.global(global)%"), "20000000");
        assert_eq!(sub(&mut w, "%v.global(self)%"), uid_text(w.guard));
    }

    #[test]
    fn global_on_an_npc_without_a_script_goes_to_the_table() {
        let mut w = world();
        w.trig.vars.add_var("v", "thug", 0);
        // thug has no script: not intercepted, so char_field is asked and
        // (with nothing there) the unknown-field fallback runs.
        assert_eq!(sub(&mut w, "[%v.global(x)%]"), "[]");
        assert_eq!(
            w.host.logs,
            vec!["Trigger: trigger 7, VNum 7. unknown char field: 'global'".to_owned()]
        );
    }

    #[test]
    fn find_global_replacement_looks_in_the_other_script() {
        let mut w = world();
        let (guard, thug) = (w.guard, w.thug);
        set_global(&mut w, thug, "zzv", "from thug", 0);
        set_global(&mut w, thug, "zzp", "private", 8);
        set_global(&mut w, guard, "zzg", "from guard", 0);
        assert_eq!(
            run(
                &mut w,
                guard,
                AttachType::Obj,
                |cx| find_global_replacement(cx, thug, "zzv")
            ),
            "from thug"
        );
        // not the owner's own script, not a context the script is not in
        assert_eq!(
            run(
                &mut w,
                guard,
                AttachType::Mob,
                |cx| find_global_replacement(cx, thug, "zzg")
            ),
            ""
        );
        assert_eq!(
            run(
                &mut w,
                guard,
                AttachType::Mob,
                |cx| find_global_replacement(cx, thug, "zzp")
            ),
            ""
        );
        // always the mob spellings, whatever the running trigger is attached to
        assert_eq!(
            run(
                &mut w,
                guard,
                AttachType::Wld,
                |cx| find_global_replacement(cx, thug, "log")
            ),
            "mlog "
        );
        assert_eq!(
            run(
                &mut w,
                guard,
                AttachType::Mob,
                |cx| find_global_replacement(cx, thug, "nonsense")
            ),
            ""
        );
        // a script owner with no script has nothing but the specials
        let sword = w.sword;
        assert_eq!(
            run(
                &mut w,
                guard,
                AttachType::Mob,
                |cx| find_global_replacement(cx, sword, "zzv")
            ),
            ""
        );
    }

    #[test]
    fn a_field_on_a_variable_that_is_text_is_left_to_text_processed() {
        // text_processed is a stub here that declines; the value then names
        // nothing, so the result is empty and nothing is logged.
        let mut w = world();
        w.trig.vars.add_var("v", "no such entity", 0);
        assert_eq!(sub(&mut w, "[%v.zzfield%]"), "[]");
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn a_non_variable_with_a_field_that_is_not_special_is_empty() {
        let mut w = world();
        assert_eq!(sub(&mut w, "[%nothing.field%]"), "[]");
        assert_eq!(sub(&mut w, "[%send.field%]"), "[]");
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn subfields_are_substituted_recursively() {
        let mut w = world();
        w.trig.vars.add_var("mv", "3000", 0);
        assert_eq!(sub(&mut w, "%findmob.3001(%mv%)%"), "1");
        // nested parentheses inside the subfield are counted but not kept
        assert_eq!(sub(&mut w, "%findmob.3001((3000))%"), "1");
        // dots inside parentheses stay in the subfield
        w.trig.vars.add_var("dotted", "3000", 0);
        assert_eq!(sub(&mut w, "%findmob.3001(%dotted%)%"), "1");
        // a % inside parentheses does not end the reference
        assert_eq!(sub(&mut w, "%findmob.3001(%mv%)% x"), "1 x");
    }

    #[test]
    fn find_replacement_receives_var_field_and_subfield() {
        let mut w = world();
        let guard = w.guard;
        // random.<n> shows the field reaching find_replacement; findmob the
        // subfield. A direct call checks the raw argument handling.
        w.host.rolls.push_back(4);
        assert_eq!(
            run(&mut w, guard, AttachType::Mob, |cx| find_replacement(
                cx, "random", "9", ""
            )),
            "4"
        );
        assert_eq!(
            run(&mut w, guard, AttachType::Mob, |cx| find_replacement(
                cx, "findmob", "3001", "3000"
            )),
            "1"
        );
    }

    #[test]
    fn a_stale_subfield_leaks_into_the_next_reference() {
        let mut w = world();
        // The first reference captures "3000" as its subfield. subfield_p is
        // never reset, so the second reference (which has no parentheses)
        // still sees the same subfield text.
        assert_eq!(sub(&mut w, "%findmob.3001(3000)% %findmob.3001%"), "1 1");
        // The next capture appends after the old text: "3000" + "7" is the
        // subfield "30007" the second reference sees.
        assert_eq!(sub(&mut w, "%findmob.3001(3000)% %findmob.3001(7)%"), "1 0");
    }

    #[test]
    fn unbalanced_parentheses_follow_the_c() {
        let mut w = world();
        // An unclosed ( swallows the rest of the line, even the closing %.
        assert_eq!(sub(&mut w, "a %findmob.3001(3000% b"), "a 1");
        // A stray ) leaves paren_count negative for later references on the
        // line, so their (…) is not captured: findmob sees an empty subfield.
        assert_eq!(sub(&mut w, "%findmob.3001)% %findmob.3001(3000)%"), "0 0");
    }

    #[test]
    fn dots_chain_through_a_temporary_variable() {
        let mut w = world();
        let (guard, thug) = (w.guard, w.thug);
        // The reference is %victim.zzfoe.zzmood%: victim is the thug, whose
        // zzfoe global holds the guard's id, whose zzmood global is "tense".
        set_global(&mut w, thug, "zzfoe", &uid_text(guard), 0);
        set_global(&mut w, guard, "zzmood", "tense", 0);
        w.trig.vars.add_var("victim", "thug", 0);
        assert_eq!(sub(&mut w, "<%victim.zzfoe.zzmood%>"), "<tense>");
        // The temporary lives on in the trigger's variables.
        assert_eq!(
            w.trig.vars.get("tmpvr").map(|v| v.value.clone()),
            Some(uid_text(guard))
        );
        // Three links.
        set_global(&mut w, guard, "zzfoe", &uid_text(thug), 0);
        set_global(&mut w, thug, "zzmood", "furious", 0);
        assert_eq!(sub(&mut w, "%victim.zzfoe.zzfoe.zzmood%"), "furious");
    }

    #[test]
    fn a_chain_link_that_is_empty_does_not_replace_the_variable() {
        let mut w = world();
        let thug = w.thug;
        set_global(&mut w, thug, "zzmood", "calm", 0);
        w.trig.vars.add_var("victim", "thug", 0);
        // victim.zznone is empty (and logs nothing: thug has a script), so
        // there is no eval; field stays "zznone" and the final call asks for
        // it again.
        assert_eq!(sub(&mut w, "[%victim.zznone.zzmood%]"), "[]");
        assert!(w.trig.vars.get("tmpvr").is_none());
    }

    #[test]
    fn a_short_variable_name_is_clobbered_by_the_tmpvr_overwrite() {
        let mut w = world();
        let (guard, thug) = (w.guard, w.thug);
        set_global(&mut w, thug, "a", &uid_text(guard), 0);
        set_global(&mut w, guard, "zzmood", "tense", 0);
        w.trig.vars.add_var("v", "thug", 0);
        // tmp = "%v.a.zzmood%". "tmpvr\0" is six bytes written at the
        // one-byte name, over "v.a.zz" -> the field (which starts at the
        // 'z' after the second dot) is now "r", cut by the new NUL.
        // The scan ends at that NUL, but the outer loop goes on one byte past
        // it: "mood" is plain text and "%]" is an unterminated reference to
        // a variable named "]".
        assert_eq!(sub(&mut w, "[%v.a.zzmood%]"), "[mood");
        assert_eq!(
            w.host.logs,
            vec!["Trigger: trigger 7, VNum 7. unknown char field: 'r'".to_owned()]
        );
        // With a long enough name the field is untouched (see
        // dots_chain_through_a_temporary_variable).
    }

    #[test]
    fn a_trailing_dot_chain_reads_past_the_percent() {
        let mut w = world();
        let thug = w.thug;
        set_global(&mut w, thug, "zzfoe", "}1", 0);
        w.trig.vars.add_var("victim", "thug", 0);
        // dots stays set at the closing %, so the loop consumes it and goes
        // on: the C swallows text after it. The line ends at the buffer's NUL.
        let out = sub(&mut w, "%victim.zzfoe.% tail");
        assert_eq!(out, "");
    }

    #[test]
    fn hostile_lines_do_not_panic() {
        let mut w = world();
        let lines = [
            "%",
            "%.",
            "%..",
            "%...%",
            "%.%",
            "%.a.b.c.d%",
            "%(%",
            "%)%",
            "%a(%",
            "%a.(%",
            "%a.b(c(d)e)f%",
            "%a.b.%",
            "%%%.%%",
            "%a.b(%c.d(%e%)%)%",
            "%é.é(é).é%",
            "((((((%",
            "))))%a.b)%",
            "%v.%",
            "%v..%",
            "%v..zz%",
            "%v.zz..%",
        ];
        w.trig.vars.add_var("v", "thug", 0);
        for line in lines {
            assert!(sub(&mut w, line).len() < MAX_INPUT_LENGTH);
            let long = line.repeat(100);
            assert!(sub(&mut w, &long).len() < MAX_INPUT_LENGTH);
        }
        let all: String = (0..511).map(|i| ['%', '.', '(', ')', 'v'][i % 5]).collect();
        assert!(sub(&mut w, &all).len() < MAX_INPUT_LENGTH);
        // the counters unwound: an ordinary line still works
        assert_eq!(sub(&mut w, "%v%"), "thug");
    }

    #[test]
    fn self_referential_values_are_cut_off_by_the_nesting_limit() {
        let mut w = world();
        let thug = w.thug;
        // victim.zzloop is "%victim.zzloop.x%": the chained reference evals
        // it, which substitutes it again, forever, were it not bounded.
        set_global(&mut w, thug, "zzloop", "%victim.zzloop.zzx%", 0);
        w.trig.vars.add_var("victim", "thug", 0);
        // The limit trips: the whole line substitutes to nothing, logged once.
        let out = sub(&mut w, "[%victim.zzloop.zzx%]");
        assert_eq!(out, "");
        let trips = w
            .host
            .logs
            .iter()
            .filter(|l| l.contains("nested too deeply"));
        assert_eq!(trips.count(), 1);
        // the counter unwinds: an ordinary line works afterwards
        w.host.logs.clear();
        w.trig.vars.add_var("who", "w", 0);
        assert_eq!(sub(&mut w, "%who%"), "w");
    }

    #[test]
    fn corpus_lines_substitute() {
        // Script-shaped lines in the style of tbamud/lib/world/trg.
        let mut w = world();
        let thug = w.thug;
        w.trig.vars.add_var("actor", &uid_text(thug), 0);
        set_global(&mut w, thug, "zzname", "Thug", 0);
        assert_eq!(
            sub(&mut w, "%echo% %actor.zzname% says hi, 100%% sure"),
            "mecho  Thug says hi, 100% sure"
        );
        assert_eq!(sub(&mut w, "if %actor.zzname% == Thug"), "if Thug == Thug");
        assert_eq!(sub(&mut w, "wait 1 sec"), "wait 1 sec");
        assert_eq!(
            sub(&mut w, "%force% %actor% north"),
            format!("mforce  {} north", uid_text(thug))
        );
    }

    #[test]
    fn a_self_reproducing_value_that_branches_is_bounded_in_work() {
        let mut w = world();
        let thug = w.thug;
        // Each level evals two copies of its own chained reference: 2^depth
        // substitutions unless the work is capped.
        set_global(
            &mut w,
            thug,
            "zzloop",
            "%victim.zzloop.zzx%%victim.zzloop.zzx%",
            0,
        );
        w.trig.vars.add_var("victim", "thug", 0);
        let started = std::time::Instant::now();
        assert_eq!(sub(&mut w, "[%victim.zzloop.zzx%]"), "");
        assert!(started.elapsed().as_secs() < 5);
        assert_eq!(
            w.host
                .logs
                .iter()
                .filter(|l| l.contains("nested too deeply"))
                .count(),
            1
        );
        // the counters reset for the next top-level line
        w.trig.vars.add_var("who", "w", 0);
        assert_eq!(sub(&mut w, "%who%"), "w");
    }

    #[test]
    fn output_stays_within_the_left_budget_after_an_invalid_byte() {
        let mut w = world();
        let thug = w.thug;
        // The chain cuts the 2-byte "é" with the tmpvr overwrite, leaving a
        // lone byte in the output; the lossy conversion must not grow the
        // result past 511 bytes.
        w.trig.vars.add_var("a", "thug", 0);
        set_global(&mut w, thug, "b", &uid_text(thug), 0);
        w.trig.vars.add_var("big", &"b".repeat(300), 0);
        let out = sub(&mut w, "%a.b.cé%big%%big%");
        assert!(out.len() < MAX_INPUT_LENGTH, "{}", out.len());
    }

    #[test]
    fn random_dir_counts_six_directions_without_diagonals() {
        let mut w = world();
        let room = w.room;
        let other = w.host.add_room(3002, "Altar");
        // DIR_COUNT is 6 by default: a lone northwest exit is not counted,
        // and no roll is drawn.
        w.host.set_link(room, "exit:6", Some(other));
        w.host.rolls.extend([6]);
        assert_eq!(sub(&mut w, "[%random.dir%]"), "[]");
        // with north too, only north can come back, whatever is rolled
        w.host.set_link(room, "exit:0", Some(other));
        w.host.rolls.clear();
        w.host.rolls.extend([0]);
        assert_eq!(sub(&mut w, "%random.dir%"), "north");
        // diagonals on: DIR_COUNT is 10 and northwest is a candidate
        w.host.globals.nums.insert("config:diagonal_dirs".into(), 1);
        w.host.rolls.clear();
        w.host.rolls.extend([6]);
        assert_eq!(sub(&mut w, "%random.dir%"), "northwest");
    }

    #[test]
    fn an_exit_with_only_flags_is_not_an_exit() {
        let mut w = world();
        let room = w.room;
        w.host.set_num(room, "exit:2:flags", 1);
        assert_eq!(sub(&mut w, "[%random.dir%]"), "[]");
    }

    #[test]
    fn findobj_narrows_the_room_vnum_and_a_variable_names_worn_objects_by_vnum() {
        let mut w = world();
        // dg_variables.c findobj: real_room(atoi(field)) with a 16-bit vnum.
        assert_eq!(
            sub(&mut w, "%findobj.68537(3020)%"),
            sub(&mut w, "%findobj.3001(3020)%")
        );
        assert_eq!(sub(&mut w, "%findobj.3001(3020)%"), "1");
        // find_replacement (mob): get_object_in_equip(ch, name) with
        // `obj_vnum ovnum = atoi(name)`, compared with an IDXTYPE.
        let guard = w.guard;
        let corpse = w.host.add_obj(-1, "corpse");
        w.host.set_text(corpse, "name", "the corpse of a rat");
        w.host.equip_char(guard, corpse, 3);
        set_global(&mut w, guard, "x", "65535", 0);
        assert_eq!(sub(&mut w, "%x.shortdesc%"), "the corpse of a rat");
        assert_eq!(sub(&mut w, "%self.has_item(65535)%"), "1");
        set_global(&mut w, guard, "x", "131071", 0);
        assert_eq!(sub(&mut w, "%x.shortdesc%"), "the corpse of a rat");
    }

    #[test]
    fn room_and_mob_vnums_narrow_to_sixteen_bits() {
        let mut w = world();
        let room = w.room;
        let pc = w.host.add_player("Alice");
        w.host.char_to_room(pc, room);
        // 68537 == 3001 + 65536
        assert_eq!(sub(&mut w, "%people.68537%"), sub(&mut w, "%people.3001%"));
        assert_eq!(sub(&mut w, "%people.3001%"), "3");
        assert_eq!(sub(&mut w, "%findmob.68537(3000)%"), "1");
        // a player's vnum is NOBODY, 65535
        assert_eq!(sub(&mut w, "%findmob.3001(65535)%"), "1");
        assert_eq!(sub(&mut w, "%findmob.3001(-1)%"), "1");
    }

    #[test]
    fn a_chain_link_with_a_subfield_leaves_it_for_the_last_link() {
        let mut w = world();
        let (guard, thug) = (w.guard, w.thug);
        // %actor.zzeq(wield).zzname%: the link zzeq(wield) is resolved with
        // subfield "wield" (an entity global ignores it), then tmpvr.zzname.
        set_global(&mut w, thug, "zzeq", &uid_text(guard), 0);
        set_global(&mut w, guard, "zzname", "Guard", 0);
        w.trig.vars.add_var("actor", "thug", 0);
        assert_eq!(sub(&mut w, "%actor.zzeq(wield).zzname%"), "Guard");
        w.trig.vars.add_var("pos", "wield", 0);
        assert_eq!(sub(&mut w, "%actor.zzeq(%pos%).zzname%"), "Guard");
    }

    #[test]
    fn an_intermediate_link_sees_its_subfield_unexpanded() {
        // C: the subfield is passed to find_replacement as captured; only the
        // final call is preceded by `var_subst(subfield)`. So findmob sees
        // atoi("%mv%") == 0 and counts mobs of vnum 0.
        let mut w = world();
        w.trig.vars.add_var("mv", "3000", 0);
        sub(&mut w, "%findmob.3001(%mv%).zz%");
        assert_eq!(
            w.trig.vars.get("tmpvr").map(|v| v.value.clone()),
            Some("0".to_owned())
        );
    }

    #[test]
    fn self_global_reads_the_owners_own_global() {
        let mut w = world();
        let guard = w.guard;
        set_global(&mut w, guard, "zzx", "mine", 0);
        assert_eq!(sub(&mut w, "%self.zzx%"), "mine");
        // C: `%self.global(x)%` on an NPC with a script reads its global x
        assert_eq!(sub(&mut w, "%self.global(zzx)%"), "mine");
    }

    #[test]
    fn global_on_a_player_with_a_script_is_not_intercepted() {
        // C: the `global` intercept needs IS_NPC; a PC goes to the char
        // field table (None in this tree), then to its own globals by name.
        let mut w = world();
        let pc = w.host.add_player("Alice");
        w.trig.vars.add_var("who", &uid_text(pc), 0);
        set_global(&mut w, pc, "zzx", "val", 0);
        assert_eq!(sub(&mut w, "%who.global(zzx)%"), "");
        assert!(
            w.host
                .logs
                .iter()
                .any(|l| l.contains("unknown char field: 'global'"))
        );
        set_global(&mut w, pc, "global", "pcglobal", 0);
        assert_eq!(sub(&mut w, "%who.global(zzx)%"), "pcglobal");
    }

    #[test]
    fn a_tripped_limit_runs_nothing_later_on_the_line() {
        let mut w = world();
        let (thug, guard) = (w.thug, w.guard);
        set_global(&mut w, thug, "zzloop", "%victim.zzloop.zzx%", 0);
        set_global(&mut w, thug, "zzfoe", &uid_text(guard), 0);
        set_global(&mut w, guard, "zzmood", "tense", 0);
        w.trig.vars.add_var("victim", "thug", 0);
        w.host.rolls.push_back(4);
        let out = sub(
            &mut w,
            "%victim.zzloop.zzx% %victim.zzfoe.zzmood% %random.9%",
        );
        assert_eq!(out, "");
        assert_eq!(w.host.rolls.len(), 1);
        assert_ne!(
            w.trig.vars.get("tmpvr").map(|v| v.value.clone()),
            Some(uid_text(guard))
        );
    }

    #[test]
    fn corpus_double_percent_around_a_variable() {
        // 2.trg: `eval r %%random.%n%%%`
        let mut w = world();
        w.trig.vars.add_var("n", "5", 0);
        assert_eq!(sub(&mut w, "eval r %%random.%n%%%"), "eval r %random.5%");
    }
}

/// `var_subst` through `find_replacement` into the real field tables, text
/// functions, expression evaluator and spell code, on the mock world. Lines
/// marked with a `.trg` file and line number are taken from
/// `tbamud/lib/world/trg`; every expected value is derived from the C
/// (`dg_variables.c`, `dg_scripts.c`, `dg_misc.c`), not from the Rust.
#[cfg(test)]
mod integration {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::driver::{Mode, script_driver};
    use crate::expr::{eval_expr, process_eval, process_if};
    use crate::host::WorldHost;
    use crate::lookup::{WEAR_LEGS, WEAR_WIELD};
    use crate::magic::{
        MAG_GROUPS, SPELL_DG_AFFECT, TAR_CHAR_ROOM, TAR_IGNORE, TAR_OBJ_INV, do_dg_affect,
        do_dg_cast,
    };
    use crate::mock::MockHost;
    use crate::types::{ROOM_ID_BASE, TrigInstance, uid_text};

    const ARMOR: i32 = 1;
    const MAGIC_MISSILE: i32 = 32;
    const IDENTIFY: i32 = 20;
    const HEAL: i32 = 28;
    const GROUP_HEAL: i32 = 64;

    /// The Temple (3001, zone 30) holds the guard (owner of every trigger
    /// here, level 17), the thug (3001, with a script) and Bob (a player of
    /// level 5, 100 gold, 30 of 50 hit points, wielding a sword). The room's
    /// floor has a brass lamp and a bag with two more; the guard carries a
    /// brass key. North of the Temple is the Altar.
    struct Game {
        host: MockHost,
        void: Uid,
        temple: Uid,
        altar: Uid,
        guard: Uid,
        thug: Uid,
        bob: Uid,
        sword: Uid,
        key: Uid,
        lamp: Uid,
        bag: Uid,
        trig: TrigInstance,
    }

    fn game() -> Game {
        let mut host = MockHost::new();
        let void = host.add_room(0, "The Void");
        let temple = host.add_room(3001, "Temple");
        let altar = host.add_room(3002, "Altar");
        host.set_num(temple, "zone", 30);
        host.globals
            .texts
            .insert("zone:30:name".to_owned(), "Midgaard".to_owned());
        // `R_EXIT(temple, NORTH)` to the Altar, no flags, no key.
        host.set_link(temple, "exit:0", Some(altar));

        let guard = host.add_mob(3000, "guard");
        host.set_num(guard, "level", 17);
        let thug = host.add_mob(3001, "thug");
        let bob = host.add_player("Bob");
        host.set_num(bob, "level", 5);
        host.set_num(bob, "gold", 100);
        host.set_num(bob, "hit", 30);
        host.set_num(bob, "max_hit", 50);
        host.set_num(bob, "pos", 8);
        host.set_num(bob, "sex", 1);
        host.char_to_room(guard, temple);
        host.char_to_room(thug, temple);
        host.char_to_room(bob, temple);

        let sword = host.add_obj(3020, "sword sharp");
        host.set_text(sword, "name", "a sharp sword");
        host.equip_char(bob, sword, WEAR_WIELD);
        let key = host.add_obj(3022, "key brass");
        host.set_text(key, "name", "a brass key");
        host.obj_to_char(key, guard);
        let bag = host.add_obj(3030, "bag");
        host.set_text(bag, "name", "a leather bag");
        host.set_num(bag, "type", 15);
        let lamp = host.add_obj(3050, "lamp");
        host.set_text(lamp, "name", "a brass lamp");
        host.obj_to_room(bag, temple);
        host.obj_to_room(lamp, temple);
        for _ in 0..2 {
            let inner = host.add_obj(3050, "lamp");
            host.obj_to_obj(inner, bag);
        }

        host.add_spell("armor", ARMOR, TAR_CHAR_ROOM, 0);
        host.add_spell("identify", IDENTIFY, TAR_CHAR_ROOM | TAR_OBJ_INV, 0);
        host.add_spell("heal", HEAL, TAR_CHAR_ROOM, 0);
        host.add_spell("magic missile", MAGIC_MISSILE, TAR_CHAR_ROOM, 0);
        host.add_spell("group heal", GROUP_HEAL, TAR_IGNORE, MAG_GROUPS);
        host.add_spell("Script-inflicted", SPELL_DG_AFFECT, 0, 0);
        host.globals
            .texts
            .insert("mobproto:1".to_owned(), "caster".to_owned());

        let proto = host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        let mut trig = TrigInstance::new(0, proto);
        trig.vars.add_var("actor", &uid_text(bob), 0);
        Game {
            host,
            void,
            temple,
            altar,
            guard,
            thug,
            bob,
            sword,
            key,
            lamp,
            bag,
            trig,
        }
    }

    impl Game {
        fn script_var(&mut self, owner: Uid, name: &str, value: &str) {
            self.host
                .ensure_script(owner)
                .unwrap()
                .global_vars
                .add_var(name, value, 0);
        }

        fn run<T>(
            &mut self,
            owner: Uid,
            attach: AttachType,
            f: impl FnOnce(&mut Cx<'_, MockHost>) -> T,
        ) -> T {
            let mut cx = Cx {
                host: &mut self.host,
                owner,
                attach,
                trig: &mut self.trig,
            };
            f(&mut cx)
        }
    }

    /// `var_subst` as the guard's mob trigger.
    fn sub(g: &mut Game, line: &str) -> String {
        g.run(g.guard, AttachType::Mob, |cx| var_subst(cx, line))
    }

    /// `eval_expr` as the guard's mob trigger.
    fn ev(g: &mut Game, line: &str) -> String {
        g.run(g.guard, AttachType::Mob, |cx| eval_expr(cx, line))
    }

    /// `process_if` as the guard's mob trigger.
    fn truth(g: &mut Game, line: &str) -> bool {
        g.run(g.guard, AttachType::Mob, |cx| process_if(cx, line))
    }

    /// Substitute, then run `dg_cast`, as the driver does for the line.
    fn cast(g: &mut Game, owner: Uid, attach: AttachType, line: &str) {
        g.run(owner, attach, |cx| {
            let line = var_subst(cx, line);
            do_dg_cast(cx, &line);
        });
    }

    fn affect(g: &mut Game, line: &str) {
        g.run(g.guard, AttachType::Mob, |cx| {
            let line = var_subst(cx, line);
            do_dg_affect(cx, &line);
        });
    }

    #[test]
    fn character_fields_read_through_the_variable() {
        let mut g = game();
        assert_eq!(sub(&mut g, "%actor.name%"), "Bob");
        assert_eq!(
            sub(&mut g, "%actor.level% %actor.gold% %actor.hitp%"),
            "5 100 30"
        );
        assert_eq!(
            sub(&mut g, "%actor.sex% %actor.heshe% %actor.hisher%"),
            "male he his"
        );
        assert_eq!(sub(&mut g, "%actor.id%"), "1");
        assert_eq!(sub(&mut g, "%actor.is_pc% %actor.vnum%"), "1 -1");
        assert_eq!(sub(&mut g, "%actor.room%"), uid_text(g.temple));
        // Case does not matter for the variable or the field.
        assert_eq!(sub(&mut g, "%ACTOR.Name%"), "Bob");
        // 117.trg:133, with an object owner: `%send%` is osend there.
        let leggings = g.host.add_obj(11722, "boots large");
        g.host.set_text(leggings, "name", "The large boots");
        let greaves = g.host.add_obj(11700, "greaves iron");
        g.host.set_text(greaves, "name", "the iron greaves");
        g.host.equip_char(g.bob, greaves, WEAR_LEGS);
        g.host.obj_to_room(leggings, g.temple);
        let line = "%send% %actor% %self.shortdesc% cover your entire legs. You must remove  \
                    %actor.eq(legs).shortdesc% to wear them.";
        let out = g.run(leggings, AttachType::Obj, |cx| var_subst(cx, line));
        assert_eq!(
            out,
            format!(
                "osend  {} The large boots cover your entire legs. You must remove  \
                 the iron greaves to wear them.",
                uid_text(g.bob)
            )
        );
        // The same line is the world's own `wsend` for a room owner.
        let out = g.run(g.temple, AttachType::Wld, |cx| {
            var_subst(cx, "%send% %actor% in %self.name%")
        });
        assert_eq!(out, format!("wsend  {} in Temple", uid_text(g.bob)));
    }

    #[test]
    fn dead_is_read_from_the_shared_is_dead_fact() {
        // DEAD(ch) is MOB_NOTDEADYET for an NPC: `npcflag(DEAD)` prints it
        // from WorldHost::is_dead once the mob is extracted, and the act
        // array never holds it.
        let mut g = game();
        g.trig.vars.add_var("mob", &uid_text(g.thug), 0);
        assert_eq!(sub(&mut g, "%mob.npcflag(DEAD)%"), "0");
        g.host.extract_char(g.thug);
        assert_eq!(sub(&mut g, "%mob.npcflag(DEAD)%"), "1");
        assert_eq!(g.host.num(g.thug, "act"), 0);
        g.host.set_num(g.thug, "dead", 0);
        assert_eq!(sub(&mut g, "%mob.npcflag(DEAD)%"), "0");
    }

    #[test]
    fn dotted_chains_go_through_the_temporary_variable() {
        let mut g = game();
        assert_eq!(sub(&mut g, "%actor.room.vnum%"), "3001");
        assert_eq!(
            sub(&mut g, "%actor.room.name% in %actor.room.zonename%"),
            "Temple in Midgaard"
        );
        assert_eq!(sub(&mut g, "%actor.room.zonenumber%"), "30");
        // 11.trg:114 `eval origin %actor.room.vnum%` is the same chain.
        assert_eq!(
            sub(&mut g, "eval origin %actor.room.vnum%"),
            "eval origin 3001"
        );
        // `self` is only four characters, but the overwritten bytes are the
        // dead ones before the second field.
        assert_eq!(sub(&mut g, "%self.room.vnum%"), "3001");
        // A link with a subfield, then a last link: the exit's destination.
        assert_eq!(sub(&mut g, "%actor.room.north(vnum)%"), "3002");
        assert_eq!(sub(&mut g, "%actor.room.north(room).name%"), "Altar");
        // The subfield buffer is never cleared, so the last link still sees
        // "room": `vnum(room)` asks whether the room's vnum is atoi("room").
        assert_eq!(sub(&mut g, "%actor.room.north(room).vnum%"), "0");
        // A room's first object and first occupant, then a field of each.
        assert_eq!(
            sub(&mut g, "%actor.room.contents(3050).shortdesc%"),
            "a brass lamp"
        );
        assert_eq!(sub(&mut g, "%actor.room.people.name%"), "Bob");
        // The chain leaves the temporary variable behind.
        // The last link evaluated was `people`: Bob, the head of the list.
        assert_eq!(
            g.trig.vars.get("tmpvr").map(|v| v.value.clone()),
            Some(uid_text(g.bob))
        );
        // A link that yields text becomes a text variable: %actor.name.strlen%.
        assert_eq!(
            sub(&mut g, "%actor.name.strlen% %actor.name.toupper%"),
            "3 Bob"
        );
        // 130.trg:514 `eval length %name.strlen%` on a plain variable.
        g.trig.vars.add_var("name", "  Gandalf the Grey ", 0);
        assert_eq!(
            sub(&mut g, "%name.strlen% [%name.trim%]"),
            "19 [Gandalf the Grey]"
        );
    }

    #[test]
    fn a_chain_value_is_evaluated_as_an_expression() {
        // `eval tmpvr <value>` runs the value through eval_expr, so a value
        // with an operator in it is computed: "a long-sword" is
        // atoi("a long") - atoi("sword") = 0.
        let mut g = game();
        g.host.set_text(g.sword, "name", "a long-sword");
        assert_eq!(sub(&mut g, "%actor.eq(wield).shortdesc%"), "a long-sword");
        assert_eq!(sub(&mut g, "%actor.eq(wield).shortdesc.strlen%"), "1");
        // A plain name is its own value.
        g.host.set_text(g.sword, "name", "a sharp sword");
        assert_eq!(sub(&mut g, "%actor.eq(wield).shortdesc.strlen%"), "13");
    }

    #[test]
    fn inventory_equipment_and_object_fields() {
        let mut g = game();
        let (key, sword) = (uid_text(g.key), uid_text(g.sword));
        assert_eq!(sub(&mut g, "%self.inventory(3022)%"), key);
        assert_eq!(sub(&mut g, "%self.inventory%"), key);
        assert_eq!(sub(&mut g, "[%self.inventory(9999)%]"), "[]");
        assert_eq!(
            sub(&mut g, "%self.inventory(3022).shortdesc%"),
            "a brass key"
        );
        // 117.trg:62 `%purge% %actor.inventory(11719)%`: nothing carried.
        assert_eq!(
            sub(&mut g, "%purge% [%actor.inventory(11719)%]"),
            "mpurge  []"
        );
        assert_eq!(sub(&mut g, "%self.has_item(3022)%"), "1");
        assert_eq!(sub(&mut g, "%self.has_item(key)%"), "1");
        assert_eq!(sub(&mut g, "%self.has_item(zz)%"), "0");
        assert_eq!(sub(&mut g, "%actor.has_item(3020)%"), "1");
        assert_eq!(sub(&mut g, "%actor.eq(wield)%"), sword);
        assert_eq!(sub(&mut g, "%actor.eq(wield).shortdesc%"), "a sharp sword");
        // The last link still sees the first link's subfield "wield" (the
        // buffer is never cleared), so `vnum` is the boolean form
        // `vnum(wield)`, false; a second subfield is appended to the first.
        assert_eq!(sub(&mut g, "%actor.eq(wield).vnum%"), "0");
        assert_eq!(sub(&mut g, "%actor.eq(wield).vnum(3020)%"), "0");
        // Through a variable the subfield is the caller's own.
        g.trig.vars.add_var("blade", &sword, 0);
        assert_eq!(sub(&mut g, "%blade.vnum% %blade.vnum%"), "3020 3020");
        assert_eq!(sub(&mut g, "%blade.vnum(3020)%"), "1");
        assert_eq!(sub(&mut g, "[%actor.eq(head)%]"), "[]");
        assert_eq!(sub(&mut g, "%actor.eq(*)% [%self.eq(*)%]"), "1 []");
        // 118.trg:1203 `with %weapon.shortdesc%`.
        g.trig.vars.add_var("weapon", &sword, 0);
        assert_eq!(
            sub(&mut g, "with %weapon.shortdesc%."),
            "with a sharp sword."
        );
        // The object's own links and a container.
        let (bag, lamp, temple) = (uid_text(g.bag), uid_text(g.lamp), uid_text(g.temple));
        g.trig.vars.add_var("bag", &bag, 0);
        assert_eq!(
            sub(&mut g, "%bag.type% %bag.is_inroom% %bag.next_in_list%"),
            format!("CONTAINER {temple} {lamp}")
        );
        assert_eq!(sub(&mut g, "%bag.count(lamp)%"), "2");
        assert_eq!(sub(&mut g, "%bag.has_in(lamp)%"), "1");
        assert_eq!(sub(&mut g, "%bag.has_in(key)%"), "0");
        assert_eq!(
            sub(&mut g, "%weapon.worn_by% %weapon.carried_by%."),
            format!("{} .", uid_text(g.bob))
        );
    }

    #[test]
    fn a_corpse_has_the_nothing_vnum() {
        // 30.trg:208 `if %item.vnum(65535)%`: GET_OBJ_VNUM of an object with
        // no prototype is NOTHING, 65535 in this tree.
        let mut g = game();
        let corpse = g.host.add_obj(-1, "corpse");
        g.trig.vars.add_var("item", &uid_text(corpse), 0);
        assert!(truth(&mut g, "%item.vnum(65535)%"));
        assert_eq!(sub(&mut g, "%item.vnum%"), "65535");
        assert!(truth(&mut g, "%item.vnum% == 65535"));
        assert!(!truth(&mut g, "%item.vnum(3020)%"));
        // A mob is not a corpse either way.
        g.trig.vars.add_var("item", &uid_text(g.sword), 0);
        assert!(!truth(&mut g, "%item.vnum(65535)%"));
    }

    #[test]
    fn setters_change_the_character_and_return_the_new_value() {
        let mut g = game();
        // 104.trg:180 `nop %actor.gold(150)%`: the line is still substituted.
        assert_eq!(sub(&mut g, "nop %actor.gold(150)%"), "nop 250");
        assert_eq!(g.host.num(g.bob, "gold"), 250);
        // increase_gold clamps at 0.
        assert_eq!(sub(&mut g, "%actor.gold(-1000)%"), "0");
        assert_eq!(g.host.num(g.bob, "gold"), 0);
        // A subfield with a reference is substituted first.
        g.trig.vars.add_var("amount", "7", 0);
        assert_eq!(sub(&mut g, "%actor.gold(%amount%)%"), "7");
        // A later reference without parentheses sees the earlier subfield
        // text again (subfield_p is never reset): the second reference
        // re-applies "3" to 10, giving 13.
        assert_eq!(sub(&mut g, "%actor.gold(3)% %actor.gold%"), "10 13");
        // Alignment is clamped to -1000..1000.
        assert_eq!(sub(&mut g, "%actor.align(5000)%"), "1000");
        assert_eq!(sub(&mut g, "%actor.align(-5000)%"), "-1000");
        // Hit points add, and update_pos follows: 30 - 40 = -10 is
        // mortally wounded.
        assert_eq!(sub(&mut g, "%actor.hitp(-40)%"), "-10");
        assert_eq!(sub(&mut g, "%actor.pos%"), "Mortally wounded");
        // A position is set by name (Sleeping..Standing only).
        assert_eq!(sub(&mut g, "%actor.pos(sit)%"), "Sitting");
        // The pools are shorts: 32767 + 1 wraps.
        g.host.set_num(g.bob, "mana", 32767);
        assert_eq!(sub(&mut g, "%actor.mana(1)%"), "-32768");
        // `level(<n>)` stores the level but, like the C, prints nothing and
        // reaches the unknown-field log.
        g.host.logs.clear();
        assert_eq!(sub(&mut g, "[%actor.level(3)%]"), "[]");
        assert_eq!(g.host.level(g.bob), 3);
        assert_eq!(
            g.host.logs,
            vec!["Trigger: trigger 7, VNum 7. unknown char field: 'level'"]
        );
        assert_eq!(sub(&mut g, "%actor.level%"), "3");
        // skillset prints nothing; skill reads the value back through the
        // spell table.
        assert_eq!(sub(&mut g, "[%actor.skillset(armor 55)%]"), "[]");
        assert_eq!(sub(&mut g, "%actor.skill(armor)%"), "55");
        assert_eq!(sub(&mut g, "%actor.skill(nothing)%"), "unknown skill");
    }

    #[test]
    fn an_unknown_field_falls_back_to_the_entitys_globals_then_the_log() {
        let mut g = game();
        g.script_var(g.bob, "zzcustom", "hello");
        g.host.logs.clear();
        assert_eq!(sub(&mut g, "%actor.zzcustom%"), "hello");
        assert!(g.host.logs.is_empty());
        // 11.trg:550 `%actor.roomstocheck.cdr%`: a global field then a text
        // function on the chain's result.
        g.script_var(g.bob, "roomstocheck", "3001 3002 3003");
        assert_eq!(sub(&mut g, "%actor.roomstocheck.cdr%"), "3002 3003");
        assert_eq!(sub(&mut g, "%actor.roomstocheck.car%"), "3001");
        // The fallback compares names with str_cmp.
        assert_eq!(sub(&mut g, "%actor.ZZCUSTOM%"), "hello");
        // No such global: empty and logged, naming the trigger.
        assert_eq!(sub(&mut g, "[%actor.zznone%]"), "[]");
        assert_eq!(
            g.host.logs,
            vec!["Trigger: trigger 7, VNum 7. unknown char field: 'zznone'"]
        );
        // A character with no script at all is logged the same way.
        g.host.logs.clear();
        g.trig.vars.add_var("mob", &uid_text(g.guard), 0);
        assert_eq!(sub(&mut g, "[%mob.zznone%]"), "[]");
        assert_eq!(g.host.logs.len(), 1);
        // Objects and rooms log with the owner's type.
        g.host.logs.clear();
        g.trig.vars.add_var("weapon", &uid_text(g.sword), 0);
        assert_eq!(sub(&mut g, "[%weapon.zzx%]"), "[]");
        assert_eq!(sub(&mut g, "[%actor.room.zzy%]"), "[]");
        assert_eq!(
            g.host.logs,
            vec![
                "Trigger: trigger 7, VNum 7, type: 0. unknown object field: 'zzx'",
                "Trigger: trigger 7, VNum 7, type: 0. unknown room field: 'zzy'",
            ]
        );
        // An object's global is found the same way.
        g.script_var(g.sword, "zzx", "forged");
        assert_eq!(sub(&mut g, "%weapon.zzx%"), "forged");
    }

    #[test]
    fn varexists_and_global_read_another_entitys_variables() {
        let mut g = game();
        g.script_var(g.bob, "solved", "1");
        g.script_var(g.thug, "v", "thug value");
        // 14.trg:127 and 11.trg:342.
        assert_eq!(sub(&mut g, "%actor.varexists(solved)%"), "1");
        assert_eq!(sub(&mut g, "%actor.varexists(SOLVED)%"), "1");
        assert_eq!(sub(&mut g, "%actor.varexists(other)%"), "0");
        assert!(truth(&mut g, "%actor.varexists(solved)%"));
        assert!(!truth(&mut g, "!%actor.varexists(solved)%"));
        assert!(truth(&mut g, "!(%actor.varexists(roomstocheck)%)"));
        // A character with no script has no variables.
        g.trig.vars.add_var("mob", &uid_text(g.guard), 0);
        assert_eq!(sub(&mut g, "%mob.varexists(solved)%"), "0");
        // %npc.global(v)%: the variable is looked up in the NPC's script.
        g.trig.vars.add_var("npc", &uid_text(g.thug), 0);
        assert_eq!(sub(&mut g, "%npc.global(v)%"), "thug value");
        assert_eq!(sub(&mut g, "[%npc.global(nope)%]"), "[]");
        // A player is not intercepted: "global" is an unknown field for it.
        g.host.logs.clear();
        g.trig.vars.add_var("pc", &uid_text(g.bob), 0);
        assert_eq!(sub(&mut g, "[%pc.global(v)%]"), "[]");
        assert_eq!(g.host.logs.len(), 1);
        assert!(g.host.logs[0].ends_with("unknown char field: 'global'"));
        // The void's globals are read by %global.name%.
        g.script_var(g.void, "worldvar", "everywhere");
        assert_eq!(sub(&mut g, "%global.worldvar%"), "everywhere");
        assert_eq!(sub(&mut g, "%global%"), ROOM_ID_BASE.to_string());
    }

    #[test]
    fn text_functions_take_precedence_over_entity_fields() {
        let mut g = game();
        // `actor` holds "}1": its text functions answer before the entity.
        assert_eq!(
            sub(&mut g, "%actor.strlen% %actor.car% %actor.charat(2)%"),
            "2 }1 1"
        );
        g.trig.vars.add_var("speech", "heal me please", 0);
        // 0.trg:1645 and 211.trg:299.
        assert_eq!(sub(&mut g, "%speech.car%|%speech.cdr%"), "heal|me please");
        assert_eq!(sub(&mut g, "%speech.contains(ME)%"), "1");
        assert_eq!(sub(&mut g, "%speech.contains(you)%"), "0");
        assert_eq!(sub(&mut g, "%speech.charat(1)%"), "h");
        assert_eq!(sub(&mut g, "[%speech.charat(99)%]"), "[]");
        assert_eq!(sub(&mut g, "%speech.toupper%"), "Heal me please");
        // 118.trg:935 `%speech.contains(dg_cast)%`.
        g.trig.vars.add_var("speech", "do dg_cast now", 0);
        assert!(truth(
            &mut g,
            "%speech.contains(mdamage)% || %speech.contains(dg_cast)%"
        ));
        // The non-backtracking str_str is shared by contains and /=.
        g.trig.vars.add_var("txt", "aaab", 0);
        assert_eq!(sub(&mut g, "%txt.contains(aab)%"), "0");
        assert_eq!(sub(&mut g, "%txt.contains(aaa)%"), "1");
        assert_eq!(sub(&mut g, "%txt.contains(ab)%"), "1");
        // Several subfielded references on one line share the subfield
        // buffer, which is never cleared: the second sees "aab" + "aaa".
        assert_eq!(
            sub(&mut g, "%txt.contains(aab)% %txt.contains(aaa)%"),
            "0 0"
        );
        assert!(!truth(&mut g, "%txt% /= aab"));
        assert!(truth(&mut g, "%txt% /= aaa"));
        // A word that is no text function falls to the entities. get_room
        // takes atoi("zzword") == 0 for a room vnum, which is the void, so
        // the field is looked up in the void's globals (dg_variables.c,
        // "special handling of the void").
        g.host.logs.clear();
        g.trig.vars.add_var("word", "zzword", 0);
        assert_eq!(sub(&mut g, "[%word.name%]"), "[]");
        assert_eq!(
            g.host.logs,
            vec![
                "Trigger: trigger 7, Vnum 7, type 0. Trying to access Global var list of \
                 void. Apparently this has not been set up!"
            ]
        );
        g.script_var(g.void, "zzfoo", "from the void");
        assert_eq!(sub(&mut g, "%word.zzfoo%"), "from the void");
    }

    #[test]
    fn mudcommand_names_the_command_a_word_abbreviates() {
        // 14.trg:219 `if %cmd.mudcommand% == wake && ...`.
        let mut g = game();
        g.host
            .globals
            .texts
            .insert("commands".to_owned(), "north wake say".to_owned());
        g.trig.vars.add_var("cmd", "wa", 0);
        assert_eq!(sub(&mut g, "%cmd.mudcommand%"), "wake");
        assert!(truth(
            &mut g,
            "%cmd.mudcommand% == wake && %actor.level% == 5"
        ));
        g.trig.vars.add_var("cmd", "no", 0);
        assert!(!truth(
            &mut g,
            "%cmd.mudcommand% == wake && %actor.level% == 5"
        ));
    }

    #[test]
    fn the_special_variables() {
        let mut g = game();
        // %people.<vnum>% counts the occupants.
        assert_eq!(sub(&mut g, "%people.3001%"), "3");
        assert_eq!(sub(&mut g, "%people.3002%"), "0");
        assert_eq!(sub(&mut g, "%people.0%"), "0");
        g.host.logs.clear();
        assert_eq!(sub(&mut g, "%people.4444%"), "-1");
        assert_eq!(g.host.logs, vec!["people.vnum: world[rnum] does not exist"]);
        // 130.trg:927 `eval total %%people.%thisRoom%%%`.
        g.trig.vars.add_var("thisroom", "3001", 0);
        assert_eq!(
            sub(&mut g, "eval total %%people.%thisRoom%%%"),
            "eval total %people.3001%"
        );
        let line = sub(&mut g, "eval total %%people.%thisRoom%%%");
        g.run(g.guard, AttachType::Mob, |cx| process_eval(cx, &line));
        assert_eq!(
            g.trig.vars.get("total").map(|v| v.value.clone()),
            Some("3".to_owned())
        );
        // %findobj% counts through containers, by vnum, id or name.
        assert_eq!(sub(&mut g, "%findobj.3001(3050)%"), "3");
        assert_eq!(sub(&mut g, "%findobj.3001(lamp)%"), "3");
        assert_eq!(sub(&mut g, "%findobj.3001(bag)%"), "1");
        assert_eq!(sub(&mut g, "%findobj.3001(%lampid%)%"), "0");
        g.trig.vars.add_var("lampid", &uid_text(g.lamp), 0);
        assert_eq!(sub(&mut g, "%findobj.3001(%lampid%)%"), "1");
        g.host.logs.clear();
        assert_eq!(sub(&mut g, "%findobj.9999(1)%"), "0");
        assert_eq!(sub(&mut g, "%findobj.3001()%"), "0");
        assert_eq!(g.host.logs.len(), 2);
        // %findmob%: mobs by vnum, and players are vnum -1.
        assert_eq!(sub(&mut g, "%findmob.3001(3001)%"), "1");
        assert_eq!(sub(&mut g, "%findmob.3001(3000)%"), "1");
        assert_eq!(sub(&mut g, "%findmob.3001(-1)%"), "1");
        assert_eq!(sub(&mut g, "%findmob.3002(3001)%"), "0");
        // %random.<n>% rolls 1..n; <= 0 gives 0.
        g.host.rolls.push_back(4);
        assert_eq!(sub(&mut g, "%random.6% %random.0%"), "4 0");
        // %random.dir% names an exit: the Temple's only one is north.
        assert_eq!(sub(&mut g, "%random.dir%"), "north");
        // 11.trg `if %here.north%`: an exit with no flags prints
        // NOBITS, which is true; no exit is empty.
        g.trig.vars.add_var("here", "3001", 0);
        assert!(truth(&mut g, "%here.north%"));
        assert!(!truth(&mut g, "%here.east%"));
        assert_eq!(sub(&mut g, "%here.north(vnum)%"), "3002");
        // A world-file exit without a key has NOTHING (65535); an exit the
        // mock has no key for is the calloc'd 0.
        assert_eq!(sub(&mut g, "%here.north(key)%"), "0");
        g.host.set_num(g.temple, "exit:0:key", 65535);
        assert_eq!(sub(&mut g, "%here.north(key)%"), "65535");
    }

    #[test]
    fn random_char_picks_one_of_the_others_by_the_c_reservoir_rule() {
        let mut g = game();
        let (bob, thug) = (uid_text(g.bob), uid_text(g.thug));
        // The room's list is bob, thug, guard; the guard is excluded, so the
        // candidates are bob (count 0) then thug (count 1). rand_number(0, n)
        // == 0 takes the candidate.
        g.host.rolls.clear();
        g.host.rolls.extend([0, 1]);
        assert_eq!(sub(&mut g, "%random.char%"), bob);
        g.host.rolls.clear();
        g.host.rolls.extend([0, 0]);
        assert_eq!(sub(&mut g, "%random.char%"), thug);
        // 14.trg:206: `if %random.char%` is true while anyone else is here.
        g.host.rolls.clear();
        g.host.rolls.extend([0, 1]);
        assert!(truth(&mut g, "%random.char%"));
        // CAN_SEE(guard, bob) fails: bob is not a candidate.
        if let Some(entity) = g.host.entity_mut(g.guard) {
            entity.lists.insert("cannot_see".to_owned(), vec![g.bob]);
        }
        g.host.rolls.clear();
        g.host.rolls.extend([0, 1]);
        assert_eq!(sub(&mut g, "%random.char%"), thug);
        // Nobody to pick: empty.
        if let Some(entity) = g.host.entity_mut(g.guard) {
            entity
                .lists
                .insert("cannot_see".to_owned(), vec![g.bob, g.thug]);
        }
        assert_eq!(sub(&mut g, "[%random.char%]"), "[]");
        // An object or a room picks among everyone, itself excluded by nothing.
        let guard = uid_text(g.guard);
        g.host.rolls.clear();
        g.host.rolls.extend([0, 1, 1]);
        let out = g.run(g.sword, AttachType::Obj, |cx| {
            var_subst(cx, "%random.char%")
        });
        // The sword is worn by bob in the Temple: candidates bob, thug,
        // guard; only the first roll (0) takes.
        assert_eq!(out, bob);
        g.host.rolls.clear();
        g.host.rolls.extend([0, 0, 0]);
        let out = g.run(g.temple, AttachType::Wld, |cx| {
            var_subst(cx, "%random.char%")
        });
        assert_eq!(out, guard);
    }

    #[test]
    fn an_expression_over_fields_follows_the_c_precedence() {
        let mut g = game();
        // Right-leaning splits: the first '-' in the line is the operator.
        assert_eq!(ev(&mut g, "%actor.gold% - 40 - 10"), "70");
        assert_eq!(ev(&mut g, "%actor.gold% / 10 / 5"), "50");
        assert_eq!(ev(&mut g, "%actor.gold% + 5 * 2"), "110");
        assert_eq!(ev(&mut g, "2 * 3 + 4"), "10");
        // || binds loosest, && next, comparisons tighter than both.
        assert_eq!(
            ev(&mut g, "%actor.level% > 3 || %actor.gold% < 0 && 0"),
            "1"
        );
        assert_eq!(
            ev(&mut g, "%actor.level% > 9 || %actor.gold% < 0 && 1"),
            "0"
        );
        // 14.trg:157.
        g.trig.vars.add_var("object", &uid_text(g.sword), 0);
        g.host.set_num(g.sword, "vnum", 47);
        assert!(truth(
            &mut g,
            "(%object.vnum% == 47) && !%actor.varexists(solved_example_quest_zone_0)%"
        ));
        g.script_var(g.bob, "solved_example_quest_zone_0", "1");
        assert!(!truth(
            &mut g,
            "(%object.vnum% == 47) && !%actor.varexists(solved_example_quest_zone_0)%"
        ));
        // != yields the str_cmp value for text, so "sword" != "axe" is not 1.
        g.host.set_num(g.sword, "vnum", 3020);
        assert_eq!(ev(&mut g, "%actor.name% != Bob"), "0");
        assert_eq!(ev(&mut g, "%actor.name% != Zed"), "-24");
        assert_eq!(ev(&mut g, "%actor.name% == bob"), "1");
        assert_eq!(ev(&mut g, "%actor.name% /= ob"), "1");
        // A number against a non-number compares as text: "100" < "5x".
        assert_eq!(ev(&mut g, "%actor.gold% < 5"), "0");
        assert_eq!(ev(&mut g, "%actor.gold% < 5x"), "1");
        assert_eq!(ev(&mut g, "%actor.gold% == 100"), "1");
        assert_eq!(ev(&mut g, "%actor.gold% == 100.0"), "0");
        // The weapon under /=: the leaf is substituted after the split.
        assert!(truth(&mut g, "%actor.eq(wield).shortdesc% /= sharp"));
        assert!(!truth(&mut g, "%actor.eq(wield).shortdesc% /= dull"));
        // 30.trg:229 (trigger 3011): a string and a numeric test joined by &&.
        g.trig.vars.add_var("item", &uid_text(g.lamp), 0);
        g.host.set_num(g.lamp, "cost", 10);
        assert!(truth(
            &mut g,
            "%item.type% != FOUNTAIN && %item.cost% <= 15"
        ));
        g.host.set_num(g.lamp, "cost", 16);
        assert!(!truth(
            &mut g,
            "%item.type% != FOUNTAIN && %item.cost% <= 15"
        ));
        g.host.set_num(g.lamp, "cost", 10);
        g.host.set_num(g.lamp, "type", 23);
        assert!(!truth(
            &mut g,
            "%item.type% != FOUNTAIN && %item.cost% <= 15"
        ));
        // An unknown variable is empty, which is false.
        assert!(!truth(&mut g, "%nothing%"));
        assert!(!truth(&mut g, "%nothing.name%"));
    }

    #[test]
    fn dg_cast_resolves_its_target_from_the_actor() {
        let mut g = game();
        let (guard, bob) = (g.guard, g.bob);
        // 0.trg:1788 `dg_cast 'magic missile' %actor%`.
        cast(
            &mut g,
            guard,
            AttachType::Mob,
            "dg_cast 'magic missile' %actor%",
        );
        assert_eq!(
            g.host.effects,
            vec![format!(
                "call_magic spell=32 level=17 caster={guard} victim={bob} obj=- room={}",
                g.temple
            )]
        );
        // 274.trg:8 `dg_cast 'heal' %actor.name%`: the target by name.
        g.host.effects.clear();
        cast(
            &mut g,
            guard,
            AttachType::Mob,
            "dg_cast 'heal' %actor.name%",
        );
        assert_eq!(g.host.effects.len(), 1);
        assert!(g.host.effects[0].starts_with("call_magic spell=28 level=17"));
        assert!(g.host.effects[0].contains(&format!("victim={bob} ")));
        // A target reached through a chain: the thug's room-mate.
        g.host.effects.clear();
        cast(
            &mut g,
            guard,
            AttachType::Mob,
            "dg_cast 'armor' %actor.room.people%",
        );
        assert!(g.host.effects[0].contains(&format!("victim={bob} ")));
        // An object target from the caster's inventory (TAR_OBJ_INV).
        g.host.effects.clear();
        cast(
            &mut g,
            guard,
            AttachType::Mob,
            "dg_cast 'identify' %self.inventory(3022)%",
        );
        assert!(
            g.host.effects[0].contains(&format!("obj={}", g.key)),
            "{:?}",
            g.host.effects
        );
        // A group spell is refused whatever the target.
        g.host.effects.clear();
        g.host.logs.clear();
        cast(
            &mut g,
            guard,
            AttachType::Mob,
            "dg_cast 'group heal' %actor%",
        );
        assert!(g.host.effects.is_empty());
        assert!(g.host.logs[0].contains("dg_cast: group spells not permitted"));
        // 118.trg:1201 `dg_cast '%spell%' %people%` with an unknown spell and
        // an empty target.
        g.host.logs.clear();
        g.trig.vars.add_var("spell", "zz", 0);
        cast(&mut g, guard, AttachType::Mob, "dg_cast '%spell%' %people%");
        assert!(g.host.logs[0].contains("dg_cast: invalid spell name"));
    }

    #[test]
    fn an_object_or_room_casts_through_a_proxy_caster() {
        let mut g = game();
        let bob = g.bob;
        // A room-owned trigger in the Altar (with only Bob in it) targets
        // %random.char%.
        g.host.char_to_room(bob, g.altar);
        g.host.rolls.push_back(0);
        let altar = g.altar;
        cast(
            &mut g,
            altar,
            AttachType::Wld,
            "dg_cast 'magic missile' %random.char%",
        );
        assert_eq!(g.host.effects.len(), 2, "{:?}", g.host.effects);
        let call = g
            .host
            .effects
            .iter()
            .find(|e| e.starts_with("call_magic"))
            .unwrap();
        assert!(call.contains("level=25"), "{call}");
        assert!(call.contains(&format!("victim={bob} ")), "{call}");
        assert!(call.ends_with(&format!("room={altar}")), "{call}");
        // The proxy is gone again.
        assert!(g.host.effects.iter().any(|e| e.starts_with("extract_char")));
    }

    #[test]
    fn dg_affect_marks_the_character_the_field_table_can_see() {
        let mut g = game();
        // `%actor.affect(...)%` asks affected_by_spell for the spell the
        // name matches; dg_affect files its affects under SPELL_DG_AFFECT.
        assert_eq!(sub(&mut g, "%actor.affect(script-inflicted)%"), "0");
        affect(&mut g, "dg_affect %actor% SANCT 1 10");
        assert_eq!(g.host.num(g.bob, &format!("spell:{SPELL_DG_AFFECT}")), 1);
        assert_eq!(g.host.num(g.bob, "aff:8"), 1);
        assert_eq!(sub(&mut g, "%actor.affect(script-inflicted)%"), "1");
        assert_eq!(sub(&mut g, "%actor.affect%"), "0");
        assert!(
            g.host
                .effects
                .iter()
                .any(|e| e.contains("duration=9 bit=8"))
        );
        // `off` removes everything the script inflicted.
        affect(&mut g, "dg_affect %actor% SANCT off 1");
        assert_eq!(g.host.num(g.bob, "aff:8"), 0);
        assert_eq!(sub(&mut g, "%actor.affect(script-inflicted)%"), "0");
        // An apply adds to the attribute: the modifier is the value.
        g.host.effects.clear();
        affect(&mut g, "dg_affect %actor.name% STR 3 5");
        assert_eq!(g.host.effects.len(), 1);
        assert!(g.host.effects[0].contains("location=1 modifier=3 duration=4"));
        // A bad target substitutes to nothing and is logged.
        g.host.logs.clear();
        affect(&mut g, "dg_affect %nobody% STR 3 5");
        assert!(
            g.host.logs[0].contains("dg_affect usage"),
            "{:?}",
            g.host.logs
        );
    }

    /// Give the trigger on `owner` the variables `vars`, run it and return
    /// the commands it had `owner` perform.
    fn run_trigger(
        g: &mut Game,
        owner: Uid,
        attach: AttachType,
        types: &str,
        vars: &[(&str, String)],
        body: &str,
    ) -> Vec<String> {
        let proto = g.host.add_proto(900, attach, types, 100, "", body);
        let id = g.host.attach(owner, &proto).unwrap();
        let slot = g
            .host
            .script_mut(owner)
            .and_then(|script| script.trigger_mut(id))
            .unwrap();
        for (name, value) in vars {
            slot.vars.add_var(name, value, 0);
        }
        g.host.mud_commands.clear();
        script_driver(&mut g.host, owner, id, Mode::New);
        g.host
            .mud_commands
            .iter()
            .filter(|(ch, _)| *ch == owner)
            .map(|(_, line)| line.clone())
            .collect()
    }

    #[test]
    fn the_stock_fido_eats_the_corpse() {
        // 30.trg #3010, verbatim.
        let body = "set inroom %self.room%\n\
set item %inroom.contents%\n\
while %item%\n\
  * Target the next item in room. In case it is devoured.\n\
  set next_item %item.next_in_list%\n\
  * Check for a corpse. Corpse on TBA is vnum 65535. Stock is -1.\n\
  if %item.vnum(65535)%\n\
    emote savagely devours a corpse.\n\
    %purge% %item%\n\
    halt\n\
  end\n\
  set item %next_item%\n\
  * Loop back\n\
done";
        let mut g = game();
        let corpse = g.host.add_obj(-1, "corpse");
        g.host.obj_to_room(corpse, g.temple);
        let tail = g.host.add_obj(3051, "rag");
        g.host.obj_to_room(tail, g.temple);
        let guard = g.guard;
        let said = run_trigger(&mut g, guard, AttachType::Mob, "b", &[], body);
        assert_eq!(said, vec!["emote savagely devours a corpse."]);
        assert!(!g.host.exists(corpse));
        assert!(g.host.exists(tail) && g.host.exists(g.lamp));
        // Without a corpse it eats nothing.
        let said = run_trigger(&mut g, guard, AttachType::Mob, "b", &[], body);
        assert!(said.is_empty());
        assert!(g.host.exists(tail));
    }

    #[test]
    fn the_stock_janitor_takes_cheap_things_but_not_fountains() {
        // 30.trg #3011, verbatim (the TODO comment line included).
        let body = "eval inroom %self.room%\n\
eval item %inroom.contents%\n\
while %item%\n\
  * Target the next item in room. In case it is picked up.\n\
  set next_item %item.next_in_list%\n\
* TODO: if %item.wearflag(take)%\n\
  * Check for fountains and expensive items.\n\
  if %item.type% != FOUNTAIN && %item.cost% <= 15\n\
    take %item.name%\n\
  end\n\
  set item %next_item%\n\
  * Loop back\n\
done";
        let mut g = game();
        // The floor: bag (cost 100), lamp (cost 10), fountain (cost 0),
        // rag (cost 15), gem (cost 16).
        g.host.set_num(g.bag, "cost", 100);
        g.host.set_num(g.lamp, "cost", 10);
        let fountain = g.host.add_obj(3052, "fountain marble");
        g.host.set_num(fountain, "type", 23);
        let rag = g.host.add_obj(3051, "rag");
        g.host.set_num(rag, "cost", 15);
        let gem = g.host.add_obj(3053, "gem");
        g.host.set_num(gem, "cost", 16);
        for obj in [fountain, rag, gem] {
            g.host.obj_to_room(obj, g.temple);
        }
        let guard = g.guard;
        let said = run_trigger(&mut g, guard, AttachType::Mob, "b", &[], body);
        assert_eq!(said, vec!["take lamp", "take rag"]);
    }

    #[test]
    fn an_exit_made_by_wdoor_has_the_callocd_key() {
        // do_wdoor: `CREATE(newexit, struct room_direction_data, 1)` is
        // calloc, so a fresh exit has key 0 (not NOTHING).
        let mut g = game();
        let temple = g.temple;
        run_trigger(
            &mut g,
            temple,
            AttachType::Wld,
            "e",
            &[],
            "wdoor 3001 south flags a",
        );
        g.trig.vars.add_var("here", "3001", 0);
        assert_eq!(sub(&mut g, "%here.south%"), "DOOR ");
        assert_eq!(sub(&mut g, "%here.south(key)%"), "0");
        assert_eq!(sub(&mut g, "%here.south(vnum)%"), "0");
    }

    #[test]
    fn the_speech_variable_checker_reads_a_variable_named_by_the_speaker() {
        // 13.trg #1344 verbatim, except that `%echo%` is `say` and the echo's
        // continuation line is joined (the harness records `say` lines).
        let body = "eval name %actor.car%\n\
eval test %%name.varexists(%speech.cdr%)%%\n\
if %test%\n\
  eval var %%name.%speech.cdr%%%\n\
  say %name.name% has remote variable %speech.cdr% which has the value of '%var%'.\n\
else\n\
  say %name.name% doesnt have the variable %speech.cdr%.\n\
end";
        let mut g = game();
        g.script_var(g.bob, "zzcustom", "hello");
        let bob = uid_text(g.bob);
        let guard = g.guard;
        let said = run_trigger(
            &mut g,
            guard,
            AttachType::Mob,
            "d",
            &[
                ("actor", bob.clone()),
                ("speech", "Bob zzcustom".to_owned()),
            ],
            // %actor.car% is text_processed on the UID text: the UID itself.
            body,
        );
        assert_eq!(
            said,
            vec!["say Bob has remote variable zzcustom which has the value of 'hello'."]
        );
        let said = run_trigger(
            &mut g,
            guard,
            AttachType::Mob,
            "d",
            &[("actor", bob), ("speech", "Bob nosuch".to_owned())],
            body,
        );
        assert_eq!(said, vec!["say Bob doesnt have the variable nosuch."]);
    }

    #[test]
    fn object_and_room_owners_read_their_own_fields() {
        let mut g = game();
        // `%self%` is the sword for an object trigger: the C's `o` branch.
        let out = g.run(g.sword, AttachType::Obj, |cx| {
            var_subst(
                cx,
                "%self.worn_by.name% %self.vnum% %self.room.vnum% %self.shortdesc%",
            )
        });
        assert_eq!(out, "Bob 3020 3001 a sharp sword");
        // And the Temple for a world trigger.
        let out = g.run(g.temple, AttachType::Wld, |cx| {
            var_subst(
                cx,
                "%self.contents.shortdesc% %self.name% %self.vnum% %self.people.name% %self.north(vnum)%",
            )
        });
        assert_eq!(out, "a leather bag Temple 3001 Bob 3002");
        // `%actor%` resolves for them too, through get_char_by_obj/_by_room.
        let out = g.run(g.sword, AttachType::Obj, |cx| {
            var_subst(cx, "%actor.name% %actor.room.name%")
        });
        assert_eq!(out, "Bob Temple");
        let out = g.run(g.temple, AttachType::Wld, |cx| {
            var_subst(cx, "%actor.name% %actor.eq(wield).shortdesc%")
        });
        assert_eq!(out, "Bob a sharp sword");
    }

    #[test]
    fn a_zone_echo_line_and_the_dynamic_path_trick_substitute() {
        let mut g = game();
        // 30.trg:250 (trigger 3013, the Newbie Tour Guide Loader).
        g.trig.vars.add_var("inroom", &uid_text(g.temple), 0);
        assert_eq!(
            sub(
                &mut g,
                "%zoneecho% %inroom.vnum% %self.name% shouts, 'Welcome, %actor.name%!'"
            ),
            "mzoneecho  3001 guard shouts, 'Welcome, Bob!'"
        );
        // 11.trg:355: `%%actor.%here%%%` builds the reference
        // `%actor.<room vnum>%`, which names a global of the actor.
        g.script_var(g.bob, "3001", "  n 3002 ");
        g.trig.vars.add_var("here", "3001", 0);
        let line = sub(&mut g, "eval path %%actor.%here%%%");
        assert_eq!(line, "eval path %actor.3001%");
        g.run(g.guard, AttachType::Mob, |cx| process_eval(cx, &line));
        // eval_expr trims the expression before substituting, so the value
        // keeps its own spaces; the script trims it with `.trim`.
        assert_eq!(
            g.trig.vars.get("path").map(|v| v.value.clone()),
            Some("  n 3002 ".to_owned())
        );
        assert_eq!(sub(&mut g, "[%path.trim%]"), "[n 3002]");
        assert!(!truth(&mut g, "%path.trim% == here"));
    }
}
