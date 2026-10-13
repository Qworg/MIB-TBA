//! The object field table of `find_replacement` (`dg_variables.c`), the
//! `else if (o) { … }` block, with `handle_oset` and the `oset_*` handlers
//! (`genobj.c`) it calls.
//!
//! The C switches on the first letter of the field and then compares the whole
//! name with `str_cmp`. Every name is unique and the compare is
//! case-insensitive, so a flat chain of exact comparisons selects the same
//! branch. The one fall-through in the switch (`affects` into the `c` cases)
//! matches nothing and changes nothing.
//!
//! # Differences from the C
//!
//! - `snprintf(str, slen, …)` truncates to the caller's buffer
//!   (`MAX_INPUT_LENGTH - 20` bytes in `var_subst`, one of them for the NUL).
//!   `fit` does the same, backing up to a character boundary instead of
//!   splitting a UTF-8 sequence.
//! - `check_flags_by_name_ar` walks `NUM_*_FLAGS` entries of a name table; the
//!   tables here hold exactly that many names, so the walk cannot run past the
//!   end as it could if a table were short.
//! - `wearflag` passes `find_eq_pos_script`'s `-1` (no such position) to
//!   `can_wear_on_pos`, which answers false for it.
//! - `next_in_list` (`o->next_content`) is derived from the list that holds
//!   the object (room contents, carried items, container contents). A worn
//!   object has no `next_content` in the C either: `obj_from_char` clears it
//!   before `equip_char`.
//! - `weight`, `cost` and `cost_per_day` writes use wrapping `int` arithmetic
//!   as the C does; the object's carried weight is not adjusted, exactly as in
//!   the C, which assigns the field directly.
//! - An `oset apply` modifier is a signed byte in the C, so the sum wraps to
//!   `i8`. The location is a `byte` there too; [`ObjHost::obj_apply`] hands
//!   it over as `u8`, which holds every apply number (0..24) the same.
//! - `GET_OBJ_VNUM` of an object with no prototype is `NOTHING`, 65535 in
//!   this tree (`CIRCLE_UNSIGNED_INDEX`). A host may report any negative
//!   number for such an object (the mock reports -1); `obj_vnum` maps it to
//!   65535, so `%corpse.vnum(65535)%` is 1 as stock scripts (30.trg:208)
//!   expect.
//!
//! Tables ported from `constants.c`: [`ITEM_TYPES`], [`EXTRA_BITS`],
//! [`AFFECTED_BITS`], [`APPLY_TYPES`]. (`wear_bits` is not used by this
//! table: `wearflag` goes through `can_wear_on_pos`.)

use crate::cstr::{atoi, is_abbrev, one_argument, skip_spaces, sprinttype, str_eq};
use crate::host::{Cx, DgHost};
use crate::lookup::{c_vnum, can_wear_on_pos, find_eq_pos_script, item_in_list, obj_room};
use crate::types::{ITEM_CONTAINER, MAX_INPUT_LENGTH, Uid, uid_text};

/// `GET_OBJ_VNUM(obj)`: the host's vnum through [`c_vnum`].
pub(crate) fn obj_vnum<H: DgHost + ?Sized>(host: &H, obj: Uid) -> i32 {
    c_vnum(host.vnum(obj))
}

/// `item_types[]` (`constants.c`), without the `"\n"` terminator.
pub const ITEM_TYPES: [&str; 24] = [
    "UNDEFINED",
    "LIGHT",
    "SCROLL",
    "WAND",
    "STAFF",
    "WEAPON",
    "FURNITURE",
    "FREE",
    "TREASURE",
    "ARMOR",
    "POTION",
    "WORN",
    "OTHER",
    "TRASH",
    "FREE2",
    "CONTAINER",
    "NOTE",
    "LIQ CONTAINER",
    "KEY",
    "FOOD",
    "MONEY",
    "PEN",
    "BOAT",
    "FOUNTAIN",
];

/// `extra_bits[]` (`constants.c`), without the terminator. `NUM_ITEM_FLAGS`
/// is its length.
pub const EXTRA_BITS: [&str; 18] = [
    "GLOW",
    "HUM",
    "NO_RENT",
    "NO_DONATE",
    "NO_INVIS",
    "INVISIBLE",
    "MAGIC",
    "NO_DROP",
    "BLESS",
    "ANTI_GOOD",
    "ANTI_EVIL",
    "ANTI_NEUTRAL",
    "ANTI_MAGE",
    "ANTI_CLERIC",
    "ANTI_THIEF",
    "ANTI_WARRIOR",
    "NO_SELL",
    "QUEST_ITEM",
];

/// `affected_bits[]` (`constants.c`), without the terminator. Entry 0 is the
/// empty "DO NOT REMOVE" placeholder. `NUM_AFF_FLAGS` is its length.
pub const AFFECTED_BITS: [&str; 23] = [
    "",
    "BLIND",
    "INVIS",
    "DET-ALIGN",
    "DET-INVIS",
    "DET-MAGIC",
    "SENSE-LIFE",
    "WATWALK",
    "SANCT",
    "GROUP",
    "CURSE",
    "INFRA",
    "POISON",
    "PROT-EVIL",
    "PROT-GOOD",
    "SLEEP",
    "NO_TRACK",
    "FLY",
    "SCUBA",
    "SNEAK",
    "HIDE",
    "UNUSED",
    "CHARM",
];

/// `apply_types[]` (`constants.c`), without the terminator.
pub const APPLY_TYPES: [&str; 25] = [
    "NONE",
    "STR",
    "DEX",
    "INT",
    "WIS",
    "CON",
    "CHA",
    "CLASS",
    "LEVEL",
    "AGE",
    "CHAR_WEIGHT",
    "CHAR_HEIGHT",
    "MAXMANA",
    "MAXHIT",
    "MAXMOVE",
    "GOLD",
    "EXP",
    "ARMOR",
    "HITROLL",
    "DAMROLL",
    "SAVING_PARA",
    "SAVING_ROD",
    "SAVING_PETRI",
    "SAVING_BREATH",
    "SAVING_SPELL",
];

/// `MAX_OBJ_AFFECT`: the object's apply slots.
pub const MAX_OBJ_AFFECT: usize = 6;
/// `EF_ARRAY_MAX`: 32-bit words in the extra-flag bit array.
pub const EF_ARRAY_MAX: usize = 4;
/// The `oset alias` / `oset shortdesc` length cap (`max_len`).
const OSET_NAME_MAX: usize = 64;
/// `oset_long_description`'s length cap.
const OSET_LONG_MAX: usize = 128;

/// The longest result `var_subst` lets `find_replacement` write
/// (`repl_str[MAX_INPUT_LENGTH - 20]`, less the NUL).
pub(crate) const REPL_MAX: usize = MAX_INPUT_LENGTH - 20 - 1;

/// What `snprintf(str, slen, …)` leaves in the buffer: the text cut to
/// [`REPL_MAX`] bytes, at a character boundary.
pub(crate) fn fit(mut text: String) -> String {
    if text.len() > REPL_MAX {
        let mut end = REPL_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The object attributes the field table reads and writes.
///
/// A host that does not model a fact returns the neutral value (`0`, empty,
/// `false`) and ignores the write.
pub trait ObjHost {
    /// `GET_OBJ_VAL(obj, idx)`, `idx` in `0..4`.
    fn obj_val(&self, obj: Uid, idx: usize) -> i32;

    /// `GET_OBJ_VAL(obj, idx) = value`, `idx` in `0..4`.
    fn obj_set_val(&mut self, obj: Uid, idx: usize, value: i32);

    /// `GET_OBJ_TIMER(obj)`.
    fn obj_timer(&self, obj: Uid) -> i32;

    /// `GET_OBJ_TIMER(obj) = value`.
    fn obj_set_timer(&mut self, obj: Uid, value: i32);

    /// `GET_OBJ_COST(obj)`.
    fn obj_cost(&self, obj: Uid) -> i32;

    /// `GET_OBJ_COST(obj) = value`.
    fn obj_set_cost(&mut self, obj: Uid, value: i32);

    /// `GET_OBJ_RENT(obj)` (`cost_per_day`).
    fn obj_rent(&self, obj: Uid) -> i32;

    /// `GET_OBJ_RENT(obj) = value`.
    fn obj_set_rent(&mut self, obj: Uid, value: i32);

    /// `GET_OBJ_WEIGHT(obj)`.
    fn obj_weight(&self, obj: Uid) -> i32;

    /// `GET_OBJ_WEIGHT(obj) = value` (the field only: no carried-weight
    /// bookkeeping, as in the C).
    fn obj_set_weight(&mut self, obj: Uid, value: i32);

    /// `IS_SET_AR(GET_OBJ_EXTRA(obj), bit)`.
    fn obj_extra_flagged(&self, obj: Uid, bit: usize) -> bool;

    /// `IS_SET_AR(GET_OBJ_AFFECT(obj), bit)`.
    fn obj_affect_flagged(&self, obj: Uid, bit: usize) -> bool;

    /// `(obj->affected[slot].location, obj->affected[slot].modifier)`, `slot`
    /// in `0..MAX_OBJ_AFFECT`.
    fn obj_apply(&self, obj: Uid, slot: usize) -> (u8, i8);

    /// Assign `obj->affected[slot].location` and `.modifier`.
    fn obj_set_apply(&mut self, obj: Uid, slot: usize, location: u8, modifier: i8);

    /// `obj->name = strdup(name)` (`oset_alias`, after its length check).
    fn obj_set_alias(&mut self, obj: Uid, name: &str);

    /// `obj->short_description = strdup(text)` (`oset_short_description`).
    fn obj_set_short_desc(&mut self, obj: Uid, text: &str);

    /// `obj->description = strdup(text)` (`oset_long_description`).
    fn obj_set_long_desc(&mut self, obj: Uid, text: &str);
}

/// `check_flags_by_name_ar`'s name search: the index of the first table entry
/// equal (case-sensitively, `strcmp`) to `search`.
fn flag_index(names: &[&str], search: &str) -> Option<usize> {
    names.iter().position(|name| *name == search)
}

/// `sprintbitarray(GET_OBJ_EXTRA(obj), extra_bits, EF_ARRAY_MAX, str)`:
/// the set bits' names, each followed by a space, `"NOBITS "` if none. The
/// walk stops after the table's `"\n"` entry, whose own bit prints as
/// `UNDEFINED`; empty names print nothing.
fn extra_bits_text<H: DgHost + ?Sized>(host: &H, obj: Uid) -> String {
    let mut out = String::new();
    for bit in 0..EF_ARRAY_MAX * 32 {
        if host.obj_extra_flagged(obj, bit) {
            match EXTRA_BITS.get(bit) {
                Some(name) if !name.is_empty() => {
                    out.push_str(name);
                    out.push(' ');
                }
                Some(_) => {}
                None => out.push_str("UNDEFINED "),
            }
        }
        if bit >= EXTRA_BITS.len() {
            break;
        }
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
    out
}

/// `oset_alias` / `oset_short_description` / `oset_long_description`: skip
/// leading spaces, refuse text over `max` bytes, otherwise store it.
fn oset_text<H: DgHost + ?Sized>(
    host: &mut H,
    obj: Uid,
    argument: &str,
    max: usize,
    store: fn(&mut H, Uid, &str),
) {
    let text = skip_spaces(argument);
    if text.len() > max {
        return;
    }
    store(host, obj, text);
}

/// `oset_apply` (`genobj.c`): add `value` to the modifier of the object's
/// apply slot for the named apply type (a free slot if it has none).
///
/// Quirk kept: `is_abbrev(apply_types[i], arg)` has its operands this way
/// round, so the *table* name must be a prefix of what the script typed
/// (`strength` selects `STR`, `hit` selects nothing).
fn oset_apply<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (arg, rest) = one_argument(argument);
    let value = atoi(skip_spaces(rest));
    if value == 0 {
        return;
    }
    let Some(apply) = APPLY_TYPES.iter().position(|name| is_abbrev(name, &arg)) else {
        return;
    };
    let mut location = None;
    let mut modifier = 0i32;
    let mut empty = None;
    for slot in 0..MAX_OBJ_AFFECT {
        let (loc, modi) = host.obj_apply(obj, slot);
        if usize::from(loc) == apply {
            location = Some(slot);
            modifier = i32::from(modi);
            break;
        } else if loc == 0 && empty.is_none() {
            empty = Some(slot);
        }
    }
    let Some(slot) = location.or(empty) else {
        return;
    };
    // The modifier is a signed byte in the C: the sum wraps.
    let stored = modifier.wrapping_add(value) as i8;
    let loc = if stored != 0 {
        u8::try_from(apply).unwrap_or(0)
    } else {
        0
    };
    host.obj_set_apply(obj, slot, loc, stored);
}

/// `handle_oset(obj, argument)`: run the named `oset` handler. `true` when a
/// handler name matched, whatever the handler then made of its argument.
fn handle_oset<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) -> bool {
    if argument.is_empty() {
        return false;
    }
    let (value, rest) = one_argument(argument);
    const HANDLERS: [&str; 4] = ["alias", "apply", "longdesc", "shortdesc"];
    let Some(which) = HANDLERS.iter().position(|name| is_abbrev(&value, name)) else {
        return false;
    };
    match which {
        0 => oset_text(host, obj, rest, OSET_NAME_MAX, H::obj_set_alias),
        1 => oset_apply(host, obj, rest),
        2 => oset_text(host, obj, rest, OSET_LONG_MAX, H::obj_set_long_desc),
        _ => oset_text(host, obj, rest, OSET_NAME_MAX, H::obj_set_short_desc),
    }
    true
}

/// `o->next_content`: the object after this one in the list that holds it.
fn next_content<H: DgHost + ?Sized>(host: &H, obj: Uid) -> Option<Uid> {
    let list = if let Some(room) = host.in_room(obj) {
        host.room_contents(room)
    } else if let Some(holder) = host.obj_carried_by(obj) {
        host.carrying(holder)
    } else {
        host.obj_contains(host.obj_in_obj(obj)?)
    };
    let at = list.iter().position(|entry| *entry == obj)?;
    list.get(at + 1).copied()
}

/// `MAX(1, addition + current)` with `int` wraparound.
fn bump(current: i32, subfield: &str) -> i32 {
    atoi(subfield).wrapping_add(current).max(1)
}

/// The `if (o) { … }` branch of `find_replacement`: the value of
/// `field(subfield)` on this object.
///
/// Returns `None` when the table leaves the result untouched (the C's
/// `*str == '\x1'` sentinel); the caller then applies the C's fallback (the
/// entity's own script variables, then an "unknown field" log). That is the
/// case for a field the table does not name, and also for the few it does
/// name that write nothing on some path (`oset` with no subfield).
pub fn obj_field<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    obj: Uid,
    field: &str,
    subfield: &str,
) -> Option<String> {
    let host = &mut *cx.host;
    let has_sub = !subfield.is_empty();
    let is = |name: &str| str_eq(field, name);

    let out = if is("affects") {
        if has_sub {
            let hit = flag_index(&AFFECTED_BITS, subfield)
                .is_some_and(|bit| host.obj_affect_flagged(obj, bit));
            u8::from(hit).to_string()
        } else {
            "0".to_owned()
        }
    } else if is("cost") {
        if has_sub {
            let value = bump(host.obj_cost(obj), subfield);
            host.obj_set_cost(obj, value);
        }
        host.obj_cost(obj).to_string()
    } else if is("cost_per_day") {
        if has_sub {
            let value = bump(host.obj_rent(obj), subfield);
            host.obj_set_rent(obj, value);
        }
        host.obj_rent(obj).to_string()
    } else if is("carried_by") {
        host.obj_carried_by(obj).map(uid_text).unwrap_or_default()
    } else if is("contents") {
        host.obj_contains(obj)
            .first()
            .copied()
            .map(uid_text)
            .unwrap_or_default()
    } else if is("count") {
        if host.obj_type(obj) == ITEM_CONTAINER {
            item_in_list(&*host, subfield, &host.obj_contains(obj)).to_string()
        } else {
            "0".to_owned()
        }
    } else if is("extra") {
        if has_sub {
            let hit = flag_index(&EXTRA_BITS, subfield)
                .is_some_and(|bit| host.obj_extra_flagged(obj, bit));
            u8::from(hit).to_string()
        } else {
            extra_bits_text(&*host, obj)
        }
    } else if is("has_in") {
        if host.obj_type(obj) == ITEM_CONTAINER {
            let found = item_in_list(&*host, subfield, &host.obj_contains(obj)) != 0;
            u8::from(found).to_string()
        } else {
            "0".to_owned()
        }
    } else if is("hasattached") {
        if has_sub {
            let attached = host
                .script(obj)
                .is_some_and(|script| script.is_attached(atoi(subfield)));
            u8::from(attached).to_string()
        } else {
            String::new()
        }
    } else if is("id") {
        obj.to_string()
    } else if is("is_inroom") {
        host.in_room(obj).map(uid_text).unwrap_or_default()
    } else if is("is_pc") {
        "-1".to_owned()
    } else if is("name") {
        host.keywords(obj)
    } else if is("next_in_list") {
        next_content(&*host, obj).map(uid_text).unwrap_or_default()
    } else if is("oset") {
        if has_sub {
            u8::from(handle_oset(host, obj, subfield)).to_string()
        } else {
            return None;
        }
    } else if is("room") {
        obj_room(&*host, obj).map(uid_text).unwrap_or_default()
    } else if is("shortdesc") {
        host.name(obj)
    } else if is("type") {
        sprinttype(host.obj_type(obj), &ITEM_TYPES)
    } else if is("timer") {
        host.obj_timer(obj).to_string()
    } else if is("vnum") {
        if has_sub {
            u8::from(obj_vnum(&*host, obj) == atoi(subfield)).to_string()
        } else {
            obj_vnum(&*host, obj).to_string()
        }
    } else if is("val0") {
        host.obj_val(obj, 0).to_string()
    } else if is("val1") {
        host.obj_val(obj, 1).to_string()
    } else if is("val2") {
        host.obj_val(obj, 2).to_string()
    } else if is("val3") {
        host.obj_val(obj, 3).to_string()
    } else if is("wearflag") {
        if has_sub {
            let pos = find_eq_pos_script(subfield).unwrap_or(-1);
            u8::from(can_wear_on_pos(&*host, obj, pos)).to_string()
        } else {
            "0".to_owned()
        }
    } else if is("weight") {
        if has_sub {
            let value = bump(host.obj_weight(obj), subfield);
            host.obj_set_weight(obj, value);
        }
        host.obj_weight(obj).to_string()
    } else if is("worn_by") {
        host.obj_worn_by(obj).map(uid_text).unwrap_or_default()
    } else {
        return None;
    };
    Some(fit(out))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::host::BaseHost;
    use crate::mock::MockHost;
    use crate::types::{AttachType, ROOM_ID_BASE, TrigInstance};

    /// Run `obj_field` on a mock object, as an object-attached trigger would.
    fn field(host: &mut MockHost, obj: Uid, name: &str, sub: &str) -> Option<String> {
        let proto = host.add_proto(1, AttachType::Obj, "g", 100, "", "nop");
        let mut trig = TrigInstance::new(1, proto);
        let mut cx = Cx {
            host,
            owner: obj,
            attach: AttachType::Obj,
            trig: &mut trig,
        };
        obj_field(&mut cx, obj, name, sub)
    }

    fn f(host: &mut MockHost, obj: Uid, name: &str, sub: &str) -> String {
        field(host, obj, name, sub).unwrap()
    }

    fn sword(host: &mut MockHost) -> Uid {
        host.add_obj(3020, "sword steel")
    }

    #[test]
    fn simple_reads() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        h.set_text(o, "name", "a steel sword");
        h.set_num(o, "timer", 7);
        h.set_num(o, "val0", 1);
        h.set_num(o, "val1", -2);
        h.set_num(o, "val2", 3);
        h.set_num(o, "val3", 4);
        // %wield.id% is the bare id, with no UID_CHAR.
        assert_eq!(f(&mut h, o, "id", ""), o.to_string());
        assert_eq!(f(&mut h, o, "is_pc", ""), "-1");
        assert_eq!(f(&mut h, o, "name", ""), "sword steel");
        assert_eq!(f(&mut h, o, "shortdesc", ""), "a steel sword");
        assert_eq!(f(&mut h, o, "timer", ""), "7");
        assert_eq!(f(&mut h, o, "val0", ""), "1");
        assert_eq!(f(&mut h, o, "val1", ""), "-2");
        assert_eq!(f(&mut h, o, "val2", ""), "3");
        assert_eq!(f(&mut h, o, "val3", ""), "4");
        // Field names are case-insensitive.
        assert_eq!(f(&mut h, o, "TIMER", ""), "7");
        assert_eq!(f(&mut h, o, "Val1", ""), "-2");
    }

    #[test]
    fn unknown_and_empty_fields_are_untouched() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(field(&mut h, o, "bogus", ""), None);
        assert_eq!(field(&mut h, o, "", ""), None);
        assert_eq!(field(&mut h, o, "val4", ""), None);
        assert_eq!(field(&mut h, o, "vnum2", ""), None);
        assert_eq!(field(&mut h, o, "cost_per", ""), None);
        // oset with no subfield writes nothing.
        assert_eq!(field(&mut h, o, "oset", ""), None);
        assert_eq!(field(&mut h, o, "\u{e9}cost", ""), None);
    }

    #[test]
    fn vnum_with_and_without_a_subfield() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(f(&mut h, o, "vnum", ""), "3020");
        assert_eq!(f(&mut h, o, "vnum", "3020"), "1");
        assert_eq!(f(&mut h, o, "vnum", "3021"), "0");
        // atoi of non-numeric text is 0, which is not this vnum.
        assert_eq!(f(&mut h, o, "vnum", "abc"), "0");
        assert_eq!(f(&mut h, o, "vnum", "3020x"), "1");
    }

    #[test]
    fn prototype_less_object_vnum_is_nothing() {
        // 30.trg:208 `if %item.vnum(65535)%`, 201.trg:1145
        // `%object.vnum% == 65535`: GET_OBJ_VNUM is NOTHING (65535).
        let mut h = MockHost::new();
        let o = h.add_obj(-1, "corpse");
        assert_eq!(f(&mut h, o, "vnum", ""), "65535");
        assert_eq!(f(&mut h, o, "vnum", "65535"), "1");
        assert_eq!(f(&mut h, o, "vnum", "-1"), "0");
    }

    #[test]
    fn cost_weight_and_rent_adjust_with_a_floor_of_one() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        h.set_num(o, "cost", 100);
        h.set_num(o, "weight", 10);
        h.set_num(o, "rent", 5);
        assert_eq!(f(&mut h, o, "cost", ""), "100");
        assert_eq!(f(&mut h, o, "cost", "25"), "125");
        assert_eq!(h.num(o, "cost"), 125);
        assert_eq!(f(&mut h, o, "cost", "-500"), "1");
        assert_eq!(h.num(o, "cost"), 1);
        // A subfield that is not a number adds 0, but still clamps to 1.
        h.set_num(o, "cost", 0);
        assert_eq!(f(&mut h, o, "cost", "x"), "1");
        assert_eq!(f(&mut h, o, "weight", "5"), "15");
        assert_eq!(f(&mut h, o, "weight", "-15"), "1");
        assert_eq!(f(&mut h, o, "cost_per_day", ""), "5");
        assert_eq!(f(&mut h, o, "cost_per_day", "-9"), "1");
        assert_eq!(h.num(o, "rent"), 1);
        // int wraparound: INT_MAX + 1 wraps negative, then the floor applies.
        h.set_num(o, "cost", i64::from(i32::MAX));
        assert_eq!(f(&mut h, o, "cost", "1"), "1");
        // Case-insensitive field name still writes.
        assert_eq!(f(&mut h, o, "COST", "2"), "3");
    }

    #[test]
    fn type_names_come_from_item_types() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        for (n, name) in [
            (0, "UNDEFINED"),
            (5, "WEAPON"),
            (15, "CONTAINER"),
            (17, "LIQ CONTAINER"),
            (23, "FOUNTAIN"),
            (24, "UNDEFINED"),
            (-1, "UNDEFINED"),
        ] {
            h.set_num(o, "type", n);
            assert_eq!(f(&mut h, o, "type", ""), name, "type {n}");
        }
    }

    #[test]
    fn locations_and_links() {
        let mut h = MockHost::new();
        let room = h.add_room(3001, "Temple");
        let mob = h.add_mob(3000, "guard");
        let bag = h.add_obj(1, "bag");
        let a = h.add_obj(2, "apple");
        let b = h.add_obj(3, "bread");
        h.obj_to_room(bag, room);
        h.obj_to_room(a, room);
        h.obj_to_room(b, room);
        assert_eq!(f(&mut h, bag, "is_inroom", ""), format!("}}{}", room));
        assert_eq!(
            f(&mut h, bag, "room", ""),
            format!("}}{}", ROOM_ID_BASE + 3001)
        );
        // next_content follows the room's list (oldest first for rooms).
        assert_eq!(f(&mut h, bag, "next_in_list", ""), format!("}}{a}"));
        assert_eq!(f(&mut h, a, "next_in_list", ""), format!("}}{b}"));
        assert_eq!(f(&mut h, b, "next_in_list", ""), "");

        // Carried: carried_by, no room of its own but room via the carrier.
        h.char_to_room(mob, room);
        h.obj_to_char(a, mob);
        h.obj_to_char(b, mob);
        assert_eq!(f(&mut h, a, "carried_by", ""), format!("}}{mob}"));
        assert_eq!(f(&mut h, a, "is_inroom", ""), "");
        assert_eq!(f(&mut h, a, "room", ""), format!("}}{room}"));
        // Inventory is head first: b then a.
        assert_eq!(f(&mut h, b, "next_in_list", ""), format!("}}{a}"));
        assert_eq!(f(&mut h, a, "next_in_list", ""), "");
        assert_eq!(f(&mut h, a, "worn_by", ""), "");

        // Worn: worn_by, no next, room via the wearer.
        h.equip_char(mob, a, 16);
        assert_eq!(f(&mut h, a, "worn_by", ""), format!("}}{mob}"));
        assert_eq!(f(&mut h, a, "carried_by", ""), "");
        assert_eq!(f(&mut h, a, "next_in_list", ""), "");
        assert_eq!(f(&mut h, a, "room", ""), format!("}}{room}"));

        // In a container: next follows the container's contents.
        h.obj_to_obj(a, bag);
        h.obj_to_obj(b, bag);
        assert_eq!(f(&mut h, bag, "contents", ""), format!("}}{b}"));
        assert_eq!(f(&mut h, b, "next_in_list", ""), format!("}}{a}"));
        assert_eq!(f(&mut h, a, "room", ""), format!("}}{room}"));
        // A loose object nowhere.
        let lost = h.add_obj(9, "lost");
        assert_eq!(f(&mut h, lost, "room", ""), "");
        assert_eq!(f(&mut h, lost, "is_inroom", ""), "");
        assert_eq!(f(&mut h, lost, "next_in_list", ""), "");
        assert_eq!(f(&mut h, lost, "contents", ""), "");
        // contents ignores its subfield in this tree.
        assert_eq!(f(&mut h, bag, "contents", "2"), format!("}}{b}"));
    }

    #[test]
    fn count_and_has_in_search_containers() {
        let mut h = MockHost::new();
        let bag = h.add_obj(1, "bag");
        h.set_num(bag, "type", 15);
        let a1 = h.add_obj(50, "apple red");
        let a2 = h.add_obj(50, "apple green");
        let sack = h.add_obj(2, "sack");
        h.set_num(sack, "type", 15);
        let a3 = h.add_obj(50, "apple yellow");
        h.obj_to_obj(a1, bag);
        h.obj_to_obj(a2, bag);
        h.obj_to_obj(sack, bag);
        h.obj_to_obj(a3, sack);
        assert_eq!(f(&mut h, bag, "count", "50"), "3");
        assert_eq!(f(&mut h, bag, "count", "apple"), "3");
        assert_eq!(f(&mut h, bag, "count", &uid_text(a3)), "1");
        assert_eq!(f(&mut h, bag, "count", "pear"), "0");
        assert_eq!(f(&mut h, bag, "count", ""), "0");
        assert_eq!(f(&mut h, bag, "has_in", "50"), "1");
        assert_eq!(f(&mut h, bag, "has_in", "pear"), "0");
        assert_eq!(f(&mut h, bag, "has_in", ""), "0");
        // A non-container answers 0 whatever it holds.
        h.set_num(bag, "type", 5);
        assert_eq!(f(&mut h, bag, "count", "50"), "0");
        assert_eq!(f(&mut h, bag, "has_in", "50"), "0");
    }

    #[test]
    fn extra_flags_by_name_and_as_text() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(f(&mut h, o, "extra", ""), "NOBITS ");
        h.set_num(o, "extra:0", 1);
        h.set_num(o, "extra:7", 1);
        assert_eq!(f(&mut h, o, "extra", ""), "GLOW NO_DROP ");
        assert_eq!(f(&mut h, o, "extra", "GLOW"), "1");
        assert_eq!(f(&mut h, o, "extra", "NO_DROP"), "1");
        assert_eq!(f(&mut h, o, "extra", "HUM"), "0");
        // The name compare is strcmp: case-sensitive.
        assert_eq!(f(&mut h, o, "extra", "glow"), "0");
        assert_eq!(f(&mut h, o, "extra", "NOSUCH"), "0");
        // Bit 18 is the table's "\n" entry: UNDEFINED. Bits beyond are never
        // looked at.
        h.set_num(o, "extra:18", 1);
        h.set_num(o, "extra:40", 1);
        assert_eq!(f(&mut h, o, "extra", ""), "GLOW NO_DROP UNDEFINED ");
        h.set_num(o, "extra:17", 1);
        assert_eq!(
            f(&mut h, o, "extra", ""),
            "GLOW NO_DROP QUEST_ITEM UNDEFINED "
        );
        // Only a bit past the terminator: nothing printed.
        let p = sword(&mut h);
        h.set_num(p, "extra:19", 1);
        assert_eq!(f(&mut h, p, "extra", ""), "NOBITS ");
    }

    #[test]
    fn affects_by_name() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(f(&mut h, o, "affects", ""), "0");
        assert_eq!(f(&mut h, o, "affects", "BLIND"), "0");
        h.set_num(o, "affect:1", 1);
        h.set_num(o, "affect:22", 1);
        assert_eq!(f(&mut h, o, "affects", "BLIND"), "1");
        assert_eq!(f(&mut h, o, "affects", "CHARM"), "1");
        assert_eq!(f(&mut h, o, "affects", "SANCT"), "0");
        assert_eq!(f(&mut h, o, "affects", "blind"), "0");
        // No subfield is "0" even with bits set.
        assert_eq!(f(&mut h, o, "affects", ""), "0");
    }

    #[test]
    fn wearflag_maps_the_position_name_to_a_bit() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        // ITEM_WEAR_TAKE (0) and ITEM_WEAR_WIELD (13), HEAD (4).
        h.set_num(o, "wear", (1 << 0) | (1 << 13) | (1 << 4));
        assert_eq!(f(&mut h, o, "wearflag", ""), "0");
        assert_eq!(f(&mut h, o, "wearflag", "wield"), "1");
        assert_eq!(f(&mut h, o, "wearflag", "head"), "1");
        assert_eq!(f(&mut h, o, "wearflag", "body"), "0");
        assert_eq!(f(&mut h, o, "wearflag", "16"), "1");
        assert_eq!(f(&mut h, o, "wearflag", "6"), "1");
        assert_eq!(f(&mut h, o, "wearflag", "5"), "0");
        // Unknown position: find_eq_pos_script gives -1, which no wear flag
        // answers for.
        assert_eq!(f(&mut h, o, "wearflag", "nowhere"), "0");
        assert_eq!(f(&mut h, o, "wearflag", "99"), "0");
    }

    #[test]
    fn hasattached_checks_the_objects_script() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(f(&mut h, o, "hasattached", ""), "");
        assert_eq!(f(&mut h, o, "hasattached", "7"), "0");
        let proto = h.add_proto(7, AttachType::Obj, "g", 100, "", "nop");
        h.attach(o, &proto).unwrap();
        assert_eq!(f(&mut h, o, "hasattached", "7"), "1");
        assert_eq!(f(&mut h, o, "hasattached", "8"), "0");
        assert_eq!(f(&mut h, o, "hasattached", "7abc"), "1");
        assert_eq!(f(&mut h, o, "hasattached", "x"), "0");
    }

    #[test]
    fn oset_text_keeps_a_leading_tab() {
        // skip_spaces stops at '\t', so the stored text starts with it.
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(f(&mut h, o, "oset", "alias  \tfoo"), "1");
        assert_eq!(h.text(o, "alias"), "\tfoo");
    }

    #[test]
    fn oset_shortdesc_and_longdesc_keep_a_leading_tab_too() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(f(&mut h, o, "oset", "shortdesc \tfoo bar"), "1");
        assert_eq!(h.text(o, "name"), "\tfoo bar");
        assert_eq!(f(&mut h, o, "oset", "longdesc   \tlies here"), "1");
        assert_eq!(h.text(o, "longdesc"), "\tlies here");
        // Spaces before the tab are skipped, and only those.
        assert_eq!(f(&mut h, o, "oset", "alias  \t foo"), "1");
        assert_eq!(h.text(o, "alias"), "\t foo");
    }

    #[test]
    fn hasattached_counts_the_running_trigger() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        let proto = h.add_proto(7, AttachType::Obj, "g", 100, "", "nop");
        let id = h.attach(o, &proto).unwrap();
        let _running = h.script_mut(o).unwrap().check_out(id).unwrap();
        assert_eq!(f(&mut h, o, "hasattached", "7"), "1");
    }

    #[test]
    fn oset_alias_shortdesc_longdesc() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(f(&mut h, o, "oset", "alias big axe"), "1");
        assert_eq!(h.text(o, "alias"), "big axe");
        // Abbreviations of the handler name work; leading spaces are skipped.
        assert_eq!(f(&mut h, o, "oset", "sh   a gleaming axe"), "1");
        assert_eq!(h.text(o, "name"), "a gleaming axe");
        assert_eq!(f(&mut h, o, "oset", "l An axe lies here."), "1");
        assert_eq!(h.text(o, "longdesc"), "An axe lies here.");
        assert_eq!(f(&mut h, o, "oset", "SHORTDESC Upper"), "1");
        assert_eq!(h.text(o, "name"), "Upper");
        // Unknown handler: 0, nothing changed.
        assert_eq!(f(&mut h, o, "oset", "bogus x"), "0");
        assert_eq!(f(&mut h, o, "oset", "a x"), "1"); // "a" abbreviates alias
        assert_eq!(h.text(o, "alias"), "x");
        // A fill word alone leaves an empty handler name.
        assert_eq!(f(&mut h, o, "oset", "the"), "0");
        assert_eq!(f(&mut h, o, "oset", "   "), "0");
        // Handler name found but the text too long: still reports 1, and the
        // old value stays.
        let long65 = "x".repeat(65);
        assert_eq!(f(&mut h, o, "oset", &format!("alias {long65}")), "1");
        assert_eq!(h.text(o, "alias"), "x");
        let long64 = "y".repeat(64);
        assert_eq!(f(&mut h, o, "oset", &format!("alias {long64}")), "1");
        assert_eq!(h.text(o, "alias"), long64);
        // Long description allows 128.
        let l128 = "z".repeat(128);
        assert_eq!(f(&mut h, o, "oset", &format!("longdesc {l128}")), "1");
        assert_eq!(h.text(o, "longdesc"), l128);
        let l129 = "w".repeat(129);
        assert_eq!(f(&mut h, o, "oset", &format!("longdesc {l129}")), "1");
        assert_eq!(h.text(o, "longdesc"), l128);
        // Empty text after the handler name is stored (strdup("")).
        assert_eq!(f(&mut h, o, "oset", "alias"), "1");
        assert_eq!(h.text(o, "alias"), "");
        // Non-ASCII text is kept whole; the limit counts bytes.
        assert_eq!(f(&mut h, o, "oset", "alias caf\u{e9}"), "1");
        assert_eq!(h.text(o, "alias"), "caf\u{e9}");
        let wide = "\u{e9}".repeat(33); // 66 bytes
        assert_eq!(f(&mut h, o, "oset", &format!("alias {wide}")), "1");
        assert_eq!(h.text(o, "alias"), "caf\u{e9}");
    }

    #[test]
    fn oset_apply_table_prefix_and_fill_words() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        // apply_types[] order: NONE STR DEX INT WIS CON CHA ...; the first
        // table name that is a prefix of "char_weight" is CHA (6), not
        // CHAR_WEIGHT.
        assert_eq!(f(&mut h, o, "oset", "apply char_weight 3"), "1");
        assert_eq!(h.num(o, "apply:0:loc"), 6);
        assert_eq!(h.num(o, "apply:0:mod"), 3);
        // one_argument skips a leading fill word ("the").
        assert_eq!(f(&mut h, o, "oset", "the alias   big axe"), "1");
        // skip_spaces stops at a tab, so the first word is empty and no
        // handler matches.
        assert_eq!(f(&mut h, o, "oset", "\tapply str 1"), "0");
    }

    #[test]
    fn oset_apply_adds_to_a_slot() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        assert_eq!(f(&mut h, o, "oset", "apply str 2"), "1");
        assert_eq!(h.num(o, "apply:0:loc"), 1);
        assert_eq!(h.num(o, "apply:0:mod"), 2);
        // Same type again reuses the slot.
        assert_eq!(f(&mut h, o, "oset", "apply str 3"), "1");
        assert_eq!(h.num(o, "apply:0:mod"), 5);
        // The type name only needs to be a prefix OF what was typed.
        assert_eq!(f(&mut h, o, "oset", "apply strength 1"), "1");
        assert_eq!(h.num(o, "apply:0:mod"), 6);
        // A different type takes the first empty slot.
        assert_eq!(f(&mut h, o, "oset", "apply dex -4"), "1");
        assert_eq!(h.num(o, "apply:1:loc"), 2);
        assert_eq!(h.num(o, "apply:1:mod"), -4);
        // Cancelling to 0 frees the slot's location.
        assert_eq!(f(&mut h, o, "oset", "apply dex 4"), "1");
        assert_eq!(h.num(o, "apply:1:loc"), 0);
        assert_eq!(h.num(o, "apply:1:mod"), 0);
        // A zero or unparsable amount does nothing, but the handler matched.
        assert_eq!(f(&mut h, o, "oset", "apply str 0"), "1");
        assert_eq!(f(&mut h, o, "oset", "apply str lots"), "1");
        assert_eq!(h.num(o, "apply:0:mod"), 6);
        // Unknown type or a type typed as a mere prefix of the table name.
        assert_eq!(f(&mut h, o, "oset", "apply hit 3"), "1");
        assert_eq!(f(&mut h, o, "oset", "apply 3"), "1");
        assert_eq!(h.num(o, "apply:1:loc"), 0);
        assert_eq!(h.num(o, "apply:2:loc"), 0);
        // The modifier is a signed byte: 6 + 127 wraps to -123.
        assert_eq!(f(&mut h, o, "oset", "apply str 127"), "1");
        assert_eq!(h.num(o, "apply:0:mod"), -123);
        // Fill the six slots; a seventh type finds none free.
        for (i, ty) in ["int", "wis", "con", "cha", "class"].iter().enumerate() {
            assert_eq!(f(&mut h, o, "oset", &format!("apply {ty} 1")), "1");
            assert_eq!(h.num(o, &format!("apply:{}:mod", i + 1)), 1);
        }
        assert_eq!(f(&mut h, o, "oset", "apply level 1"), "1");
        assert_eq!(h.num(o, "apply:0:loc"), 1);
        for slot in 0..6 {
            assert_ne!(h.num(o, &format!("apply:{slot}:loc")), 8);
        }
    }

    #[test]
    fn oset_apply_none_matches_an_empty_slot_first() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        // "none" is apply type 0, so slot 0 (location 0) matches directly.
        assert_eq!(f(&mut h, o, "oset", "apply none 5"), "1");
        assert_eq!(h.num(o, "apply:0:mod"), 5);
        // The stored modifier is nonzero so the location is set to NONE (0).
        assert_eq!(h.num(o, "apply:0:loc"), 0);
    }

    #[test]
    fn results_are_cut_like_snprintf() {
        let mut h = MockHost::new();
        let o = sword(&mut h);
        h.set_text(o, "name", &"a".repeat(600));
        assert_eq!(f(&mut h, o, "shortdesc", "").len(), REPL_MAX);
        // A multibyte character straddling the limit is dropped whole.
        h.set_text(o, "name", &format!("{}\u{e9}", "a".repeat(REPL_MAX - 1)));
        assert_eq!(f(&mut h, o, "shortdesc", ""), "a".repeat(REPL_MAX - 1));
    }

    #[test]
    fn fields_from_the_corpus() {
        // From lib/world/trg: `%self.count%`, `%self.has_in%`, `%object.cost%`
        // (an `eval value ((%object.cost% * 4) / 10)` trigger) and
        // `%wield.cost_per_day%`.
        let mut h = MockHost::new();
        let o = sword(&mut h);
        h.set_num(o, "cost", 250);
        h.set_num(o, "rent", 25);
        assert_eq!(f(&mut h, o, "cost", ""), "250");
        assert_eq!(f(&mut h, o, "cost_per_day", ""), "25");
        assert_eq!(f(&mut h, o, "count", ""), "0");
        assert_eq!(f(&mut h, o, "has_in", ""), "0");
    }
}
