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
//!   is `NOBODY` and `68537` is `3001`. The object vnum of `%findobj%` goes
//!   through `lookup::item_in_list`, which compares the un-narrowed value
//!   (reported as a contract request).
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

use std::cell::Cell;

use crate::cstr::{atoi, str_eq};
use crate::expr::process_eval;
use crate::host::{BaseHost, Cx, DgHost};
use crate::lookup::{
    get_char, get_char_by_obj, get_char_by_room, get_char_in_room, get_obj, get_obj_by_obj,
    get_obj_by_room, get_obj_in_list, get_object_in_equip, get_room, item_in_list, obj_room,
    trgvar_in_room,
};
use crate::text::text_processed;
use crate::types::{AttachType, DIRS, MAX_INPUT_LENGTH, NUM_OF_DIRS, ROOM_ID_BASE, Uid, uid_text};
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

thread_local! {
    static NESTING: Cell<u32> = const { Cell::new(0) };
    static WORK: Cell<u32> = const { Cell::new(0) };
    static ABORTED: Cell<bool> = const { Cell::new(false) };
}

/// Decrements [`NESTING`] when a `var_subst` call ends; the outermost call
/// also resets the work count and the abort flag.
struct NestGuard;

/// Why [`NestGuard::enter`] refused.
enum Refusal {
    /// A limit tripped on this call: log it.
    Tripped,
    /// An earlier call of this top-level substitution already gave up.
    Aborted,
}

impl NestGuard {
    fn enter() -> Result<Self, Refusal> {
        if ABORTED.with(Cell::get) {
            return Err(Refusal::Aborted);
        }
        let depth = NESTING.with(Cell::get);
        let work = WORK.with(Cell::get);
        if depth >= MAX_NESTING || work >= MAX_WORK {
            ABORTED.with(|flag| flag.set(true));
            return Err(Refusal::Tripped);
        }
        NESTING.with(|d| d.set(depth + 1));
        WORK.with(|w| w.set(work + 1));
        Ok(NestGuard)
    }
}

impl Drop for NestGuard {
    fn drop(&mut self) {
        let depth = NESTING.with(|d| {
            let depth = d.get().saturating_sub(1);
            d.set(depth);
            depth
        });
        if depth == 0 {
            WORK.with(|w| w.set(0));
            ABORTED.with(|flag| flag.set(false));
        }
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

    let _guard = match NestGuard::enter() {
        Ok(guard) => guard,
        Err(refusal) => {
            if matches!(refusal, Refusal::Tripped) {
                cx.log("variable substitution nested too deeply");
            }
            return String::new();
        }
    };

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

    while at(&tmp, p) != 0 && left > 0 && !ABORTED.with(Cell::get) {
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
                    && !ABORTED.with(Cell::get)
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
            if ABORTED.with(Cell::get) {
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
    if ABORTED.with(Cell::get) {
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
            // special handling of the void, as it stores all 'full global'
            // variables
            if cx.host.vnum(r) == 0 {
                if cx.host.script(r).is_none() {
                    let message = format!(
                        "Trigger: {}, Vnum {}, type {}. Trying to access Global var list of void. Apparently this has not been set up!",
                        cx.trig.proto.name,
                        cx.trig.proto.vnum,
                        attach.index()
                    );
                    cx.host.script_log(&message);
                    return String::new();
                }
                return own_global(&*cx.host, r, field).unwrap_or_default();
            }
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
        return Special::Done(item_in_list(&*cx.host, subfield, &list).to_string());
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
            if let Some(o) = get_object_in_equip(host, go, name) {
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
