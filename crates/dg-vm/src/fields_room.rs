//! The room field table of `find_replacement` (`dg_variables.c`), the
//! `else if (r) { … }` block.
//!
//! Unlike the object block, this one starts from an empty result (the
//! function's own `*str = '\0'`), not the `\x01` sentinel: every named field
//! produces a string, possibly empty. [`room_field`] returns `None` only for
//! the closing `else` (a field the table does not name), where the C looks in
//! the room's own script variables and logs an unknown field; the caller
//! does that.
//!
//! Room `0` (the void) is handled here, ahead of the table: it holds the
//! "full global" variables, so every field name on it is a lookup in its
//! script's globals (first name match, any context), and a missing script
//! logs the C's message.
//!
//! Only the six directions `north east south west up down` are fields in this
//! tree of the C, although the game has ten exits; the diagonals fall through
//! to the unknown-field path, as they do there. An exit subfield other than
//! `vnum key bits room` leaves the result empty. `GET_ROOM_VNUM(to_room)` of
//! an exit with no destination is `NOWHERE`, 65535 in this tree
//! (`CIRCLE_UNSIGNED_INDEX`), and that is what `(vnum)` prints. A
//! prototype-less object counts as 65535 in `contents(N)` too.
//!
//! # Differences from the C
//!
//! - `snprintf` truncation to the caller's buffer is applied (the
//!   crate-private `fit` of `fields_obj`).
//! - `id` and `roomflag` call `real_room(r->number)`; for a room that call
//!   cannot find, `id` is empty and `roomflag` is `0` (the C indexes
//!   `world[NOWHERE]`).
//! - `weather` indexes `sky_look[]` with `weather_info.sky`; a sky value
//!   outside `0..4` (which the C would read out of bounds) gives an empty
//!   result.
//!
//! Tables ported from `constants.c`: [`ROOM_BITS`], [`EXIT_BITS`],
//! [`SECTOR_TYPES`], and `sky_look` ([`SKY_LOOK`]).

use crate::cstr::{atoi, sprintbit, sprinttype, str_eq};
use crate::fields_obj::fit;
use crate::host::{Cx, DgHost};
use crate::types::{DIRS, Uid, uid_text};

/// `room_bits[]` (`constants.c`), without the terminator. `NUM_ROOM_FLAGS` is
/// its length.
pub const ROOM_BITS: [&str; 17] = [
    "DARK",
    "DEATH",
    "NO_MOB",
    "INDOORS",
    "PEACEFUL",
    "SOUNDPROOF",
    "NO_TRACK",
    "NO_MAGIC",
    "TUNNEL",
    "PRIVATE",
    "GODROOM",
    "HOUSE",
    "HCRSH",
    "ATRIUM",
    "OLC",
    "*",
    "WORLDMAP",
];

/// `exit_bits[]` (`constants.c`), without the terminator.
pub const EXIT_BITS: [&str; 4] = ["DOOR", "CLOSED", "LOCKED", "PICKPROOF"];

/// `sector_types[]` (`constants.c`), without the terminator.
pub const SECTOR_TYPES: [&str; 10] = [
    "Inside",
    "City",
    "Field",
    "Forest",
    "Hills",
    "Mountains",
    "Water (Swim)",
    "Water (No Swim)",
    "In Flight",
    "Underwater",
];

/// `sky_look[]`, the `weather` field's names for `weather_info.sky`.
pub const SKY_LOOK: [&str; 4] = ["sunny", "cloudy", "rainy", "lightning"];

/// `ROOM_INDOORS`: the room-flag number that suppresses weather.
pub const ROOM_INDOORS: usize = 3;

/// The directions this table names as fields, in direction-number order.
const FIELD_DIRS: usize = 6;

/// A room exit: the parts of `struct room_direction_data` the table reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomExit {
    /// `to_room`: `None` for `NOWHERE`.
    pub to_room: Option<Uid>,
    /// `key`, the C value: `obj_vnum` is `ush_int` here, so "no key"
    /// (`NOTHING`) is 65535, not -1; the VM prints it unchanged.
    pub key: i32,
    /// `exit_info`.
    pub info: u32,
}

/// The room attributes the field table reads.
///
/// A host that does not model a fact returns the neutral value (`0`, empty,
/// `None`, `false`).
pub trait RoomHost {
    /// `R_EXIT(room, dir)`, `dir` in `0..NUM_OF_DIRS` (the field table only
    /// asks for `0..6`, `NORTH`..`DOWN`): `None` when the room has no exit
    /// that way.
    fn room_exit(&self, room: Uid, dir: usize) -> Option<RoomExit>;

    /// `r->sector_type`.
    fn room_sector(&self, room: Uid) -> i32;

    /// `IS_SET_AR(ROOM_FLAGS(room), flag)`.
    fn room_flagged(&self, room: Uid, flag: usize) -> bool;

    /// `zone_table[r->zone].number`.
    fn room_zone_number(&self, room: Uid) -> i32;

    /// `zone_table[r->zone].name`.
    fn room_zone_name(&self, room: Uid) -> String;

    /// `weather_info.sky` (`SKY_CLOUDLESS`..`SKY_LIGHTNING`).
    fn room_sky(&self) -> i32;
}

/// The `if (r) { … }` branch of `find_replacement`: the value of
/// `field(subfield)` on this room.
///
/// Returns `None` when the table leaves the result untouched (the C's
/// `*str == '\x1'` sentinel); the caller then applies the C's fallback (the
/// entity's own script variables, then an "unknown field" log). For a room
/// that is the closing `else`: a field the table does not name.
pub fn room_field<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    room: Uid,
    field: &str,
    subfield: &str,
) -> Option<String> {
    let has_sub = !subfield.is_empty();
    let number = cx.host.vnum(room);

    // The void stores the "full global" variables.
    if number == 0 {
        let out = match cx.host.script(room) {
            None => {
                let line = format!(
                    "Trigger: {}, Vnum {}, type {}. Trying to access Global var list of void. \
                     Apparently this has not been set up!",
                    cx.trig.proto.name,
                    cx.trig.proto.vnum,
                    cx.attach.index()
                );
                cx.host.script_log(&line);
                String::new()
            }
            Some(script) => script
                .global_vars
                .get(field)
                .map(|var| var.value.clone())
                .unwrap_or_default(),
        };
        return Some(fit(out));
    }

    let host = &mut *cx.host;
    let is = |name: &str| str_eq(field, name);

    let out = if is("name") {
        host.name(room)
    } else if is("sector") {
        sprinttype(host.room_sector(room), &SECTOR_TYPES)
    } else if is("vnum") {
        if has_sub {
            u8::from(number == atoi(subfield)).to_string()
        } else {
            number.to_string()
        }
    } else if is("contents") {
        let contents = host.room_contents(room);
        if has_sub {
            // The first object whose vnum is the subfield's number; none
            // found is empty.
            contents
                .iter()
                .find(|obj| crate::fields_obj::obj_vnum(&*host, **obj) == atoi(subfield))
                .map(|obj| uid_text(*obj))
                .unwrap_or_default()
        } else {
            contents
                .first()
                .map(|obj| uid_text(*obj))
                .unwrap_or_default()
        }
    } else if is("people") {
        host.room_people(room)
            .first()
            .map(|ch| uid_text(*ch))
            .unwrap_or_default()
    } else if is("id") {
        host.real_room(number)
            .map(|rnum| rnum.to_string())
            .unwrap_or_default()
    } else if is("weather") {
        if host.room_flagged(room, ROOM_INDOORS) {
            String::new()
        } else {
            usize::try_from(host.room_sky())
                .ok()
                .and_then(|sky| SKY_LOOK.get(sky))
                .map(|look| (*look).to_owned())
                .unwrap_or_default()
        }
    } else if is("hasattached") {
        if has_sub {
            let attached = host
                .script(room)
                .is_some_and(|script| script.is_attached(atoi(subfield)));
            u8::from(attached).to_string()
        } else {
            String::new()
        }
    } else if is("zonenumber") {
        host.room_zone_number(room).to_string()
    } else if is("zonename") {
        host.room_zone_name(room)
    } else if is("roomflag") {
        if has_sub {
            let hit = host.real_room(number).is_some_and(|this| {
                ROOM_BITS
                    .iter()
                    .position(|name| *name == subfield)
                    .is_some_and(|bit| host.room_flagged(this, bit))
            });
            u8::from(hit).to_string()
        } else {
            "0".to_owned()
        }
    } else {
        let dir = DIRS[..FIELD_DIRS].iter().position(|name| is(name))?;
        exit_field(&*host, room, dir, subfield)
    };
    Some(fit(out))
}

/// One of the six exit fields: `north`, `east`, … with their subfields.
/// No exit is empty; no subfield is the `exit_info` bits.
fn exit_field<H: DgHost + ?Sized>(host: &H, room: Uid, dir: usize, subfield: &str) -> String {
    let Some(exit) = host.room_exit(room, dir) else {
        return String::new();
    };
    let bits = || sprintbit(u64::from(exit.info), &EXIT_BITS);
    if subfield.is_empty() {
        bits()
    } else if str_eq(subfield, "vnum") {
        // GET_ROOM_VNUM(to_room): NOWHERE (65535, ush_int) with no destination.
        exit.to_room.map_or(65535, |to| host.vnum(to)).to_string()
    } else if str_eq(subfield, "key") {
        exit.key.to_string()
    } else if str_eq(subfield, "bits") {
        bits()
    } else if str_eq(subfield, "room") {
        exit.to_room.map(uid_text).unwrap_or_default()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::host::{BaseHost, WorldHost};
    use crate::mock::MockHost;
    use crate::types::{AttachType, ROOM_ID_BASE, TrigInstance};

    fn field(host: &mut MockHost, room: Uid, name: &str, sub: &str) -> Option<String> {
        let proto = host.add_proto(5, AttachType::Wld, "a", 100, "", "nop");
        let mut trig = TrigInstance::new(1, proto);
        let mut cx = Cx {
            host,
            owner: room,
            attach: AttachType::Wld,
            trig: &mut trig,
        };
        room_field(&mut cx, room, name, sub)
    }

    fn f(host: &mut MockHost, room: Uid, name: &str, sub: &str) -> String {
        field(host, room, name, sub).unwrap()
    }

    #[test]
    fn name_vnum_id_and_sector() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "The Temple");
        assert_eq!(f(&mut h, r, "name", ""), "The Temple");
        assert_eq!(f(&mut h, r, "vnum", ""), "3001");
        assert_eq!(f(&mut h, r, "vnum", "3001"), "1");
        assert_eq!(f(&mut h, r, "vnum", "3002"), "0");
        assert_eq!(f(&mut h, r, "vnum", "x"), "0");
        // `id` is the bare id, with no UID_CHAR.
        assert_eq!(f(&mut h, r, "id", ""), (ROOM_ID_BASE + 3001).to_string());
        for (n, name) in [
            (0, "Inside"),
            (1, "City"),
            (6, "Water (Swim)"),
            (9, "Underwater"),
            (10, "UNDEFINED"),
            (-3, "UNDEFINED"),
        ] {
            h.set_num(r, "sector", n);
            assert_eq!(f(&mut h, r, "sector", ""), name, "sector {n}");
        }
        assert_eq!(f(&mut h, r, "NAME", ""), "The Temple");
        assert_eq!(f(&mut h, r, "Sector", ""), "UNDEFINED");
    }

    #[test]
    fn unknown_fields_and_diagonals_are_untouched() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        assert_eq!(field(&mut h, r, "bogus", ""), None);
        assert_eq!(field(&mut h, r, "", ""), None);
        assert_eq!(field(&mut h, r, "northeast", ""), None);
        assert_eq!(field(&mut h, r, "southwest", "vnum"), None);
        assert_eq!(field(&mut h, r, "nam", ""), None);
        assert_eq!(field(&mut h, r, "\u{e9}", ""), None);
    }

    #[test]
    fn contents_by_vnum_or_first() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        assert_eq!(f(&mut h, r, "contents", ""), "");
        assert_eq!(f(&mut h, r, "contents", "7"), "");
        let a = h.add_obj(7, "apple");
        let b = h.add_obj(8, "bread");
        let c = h.add_obj(7, "another apple");
        h.obj_to_room(a, r);
        h.obj_to_room(b, r);
        h.obj_to_room(c, r);
        // Room contents are oldest first.
        assert_eq!(f(&mut h, r, "contents", ""), format!("}}{a}"));
        assert_eq!(f(&mut h, r, "contents", "8"), format!("}}{b}"));
        assert_eq!(f(&mut h, r, "contents", "7"), format!("}}{a}"));
        assert_eq!(f(&mut h, r, "contents", "9"), "");
        // A prototype-less object is NOTHING (65535): 30.trg:208.
        let corpse = h.add_obj(-1, "corpse");
        h.obj_to_room(corpse, r);
        assert_eq!(f(&mut h, r, "contents", "65535"), format!("}}{corpse}"));
        assert_eq!(f(&mut h, r, "contents", "-1"), "");
        // atoi stops at the junk: "7junk" is vnum 7.
        assert_eq!(f(&mut h, r, "contents", "7junk"), format!("}}{a}"));
        // A non-numeric subfield is vnum 0, which nothing here has.
        assert_eq!(f(&mut h, r, "contents", "apple"), "");
        let zero = h.add_obj(0, "zero");
        h.obj_to_room(zero, r);
        assert_eq!(f(&mut h, r, "contents", "apple"), format!("}}{zero}"));
    }

    #[test]
    fn people_is_the_head_of_the_list() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        assert_eq!(f(&mut h, r, "people", ""), "");
        let a = h.add_mob(1, "a");
        let b = h.add_mob(2, "b");
        h.char_to_room(a, r);
        h.char_to_room(b, r);
        assert_eq!(f(&mut h, r, "people", ""), format!("}}{b}"));
    }

    #[test]
    fn weather_reads_the_sky_unless_indoors() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        for (sky, name) in [(0, "sunny"), (1, "cloudy"), (2, "rainy"), (3, "lightning")] {
            h.globals.nums.insert("sky".to_owned(), sky);
            assert_eq!(f(&mut h, r, "weather", ""), name);
        }
        h.globals.nums.insert("sky".to_owned(), 4);
        assert_eq!(f(&mut h, r, "weather", ""), "");
        h.globals.nums.insert("sky".to_owned(), -1);
        assert_eq!(f(&mut h, r, "weather", ""), "");
        h.globals.nums.insert("sky".to_owned(), 2);
        h.set_num(r, "flag:3", 1);
        assert_eq!(f(&mut h, r, "weather", ""), "");
    }

    #[test]
    fn zone_fields() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        h.set_num(r, "zone", 30);
        h.globals
            .texts
            .insert("zone:30:name".to_owned(), "Midgaard".to_owned());
        assert_eq!(f(&mut h, r, "zonenumber", ""), "30");
        assert_eq!(f(&mut h, r, "zonename", ""), "Midgaard");
        let lost = h.add_room(9999, "Elsewhere");
        h.set_num(lost, "zone", 31);
        assert_eq!(f(&mut h, lost, "zonenumber", ""), "31");
        assert_eq!(f(&mut h, lost, "zonename", ""), "");
    }

    #[test]
    fn roomflag_by_name() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        assert_eq!(f(&mut h, r, "roomflag", ""), "0");
        assert_eq!(f(&mut h, r, "roomflag", "DARK"), "0");
        h.set_num(r, "flag:0", 1);
        h.set_num(r, "flag:16", 1);
        assert_eq!(f(&mut h, r, "roomflag", "DARK"), "1");
        assert_eq!(f(&mut h, r, "roomflag", "WORLDMAP"), "1");
        assert_eq!(f(&mut h, r, "roomflag", "INDOORS"), "0");
        // strcmp: case-sensitive.
        assert_eq!(f(&mut h, r, "roomflag", "dark"), "0");
        assert_eq!(f(&mut h, r, "roomflag", "NOSUCH"), "0");
        // The bfs mark is named "*".
        h.set_num(r, "flag:15", 1);
        assert_eq!(f(&mut h, r, "roomflag", "*"), "1");
    }

    #[test]
    fn id_and_roomflag_need_a_real_room() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        h.set_num(r, "flag:0", 1);
        assert_eq!(f(&mut h, r, "roomflag", "DARK"), "1");
        // A room whose number real_room cannot find: `id` is empty, and
        // roomflag answers 0 where the C reads ROOM_FLAGS(NOWHERE).
        h.set_num(r, "vnum", 9999);
        assert_eq!(h.real_room(9999), None);
        assert_eq!(f(&mut h, r, "id", ""), "");
        assert_eq!(f(&mut h, r, "roomflag", "DARK"), "0");
        assert_eq!(f(&mut h, r, "roomflag", ""), "0");
        assert_eq!(f(&mut h, r, "vnum", ""), "9999");
    }

    #[test]
    fn weather_outside_the_sky_table_is_empty() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        for (sky, want) in [(0, "sunny"), (3, "lightning"), (4, ""), (-1, ""), (99, "")] {
            h.globals.nums.insert("sky".to_owned(), sky);
            assert_eq!(f(&mut h, r, "weather", ""), want, "sky {sky}");
        }
    }

    #[test]
    fn hasattached_checks_the_rooms_script() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        assert_eq!(f(&mut h, r, "hasattached", ""), "");
        assert_eq!(f(&mut h, r, "hasattached", "5"), "0");
        let proto = h.add_proto(5, AttachType::Wld, "a", 100, "", "nop");
        h.attach(r, &proto).unwrap();
        assert_eq!(f(&mut h, r, "hasattached", "5"), "1");
        assert_eq!(f(&mut h, r, "hasattached", "6"), "0");
        // The running instance is checked out of the list; its slot stays.
        let id = h.script(r).unwrap().trigger_ids()[0];
        let _running = h.script_mut(r).unwrap().check_out(id).unwrap();
        assert_eq!(f(&mut h, r, "hasattached", "5"), "1");
    }

    #[test]
    fn exits() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        let n = h.add_room(3002, "North");
        // No exit at all.
        for dir in ["north", "east", "south", "west", "up", "down"] {
            assert_eq!(f(&mut h, r, dir, ""), "", "{dir}");
            assert_eq!(f(&mut h, r, dir, "vnum"), "", "{dir}");
        }
        h.set_link(r, "exit:0", Some(n));
        h.set_num(r, "exit:0:flags", 0b0111);
        h.set_num(r, "exit:0:key", 3010);
        assert_eq!(f(&mut h, r, "north", ""), "DOOR CLOSED LOCKED ");
        assert_eq!(f(&mut h, r, "north", "bits"), "DOOR CLOSED LOCKED ");
        assert_eq!(f(&mut h, r, "north", "vnum"), "3002");
        assert_eq!(f(&mut h, r, "north", "key"), "3010");
        assert_eq!(f(&mut h, r, "north", "room"), format!("}}{n}"));
        assert_eq!(f(&mut h, r, "NORTH", "VNUM"), "3002");
        // An unrecognised subfield leaves the result empty.
        assert_eq!(f(&mut h, r, "north", "dest"), "");
        // Other directions are independent.
        assert_eq!(f(&mut h, r, "south", "vnum"), "");

        // An exit that leads nowhere (the shared `nowhere` key): vnum is
        // NOWHERE (65535 in this tree), room is empty, and a key of NOTHING
        // (a world-file exit without one) is 65535.
        h.set_num(r, "exit:5:nowhere", 1);
        h.set_num(r, "exit:5:key", 65535);
        assert_eq!(f(&mut h, r, "down", ""), "NOBITS ");
        assert_eq!(f(&mut h, r, "down", "vnum"), "65535");
        assert_eq!(f(&mut h, r, "down", "room"), "");
        assert_eq!(f(&mut h, r, "down", "key"), "65535");
        // Every direction maps to its own number.
        for (i, dir) in ["north", "east", "south", "west", "up", "down"]
            .iter()
            .enumerate()
        {
            if h.link(r, &format!("exit:{i}")).is_none() {
                h.set_num(r, &format!("exit:{i}:nowhere"), 1);
            }
            h.set_num(r, &format!("exit:{i}:key"), 100 + i as i64);
            assert_eq!(f(&mut h, r, dir, "key"), (100 + i).to_string());
        }
        // Bits past the table print UNDEFINED, one per set bit.
        h.set_num(r, "exit:1:flags", 1 << 5);
        assert_eq!(f(&mut h, r, "east", "bits"), "UNDEFINED ");
        // All four defined bits, then two undefined ones.
        h.set_num(r, "exit:1:flags", 0b1111 | (1 << 4) | (1 << 6));
        assert_eq!(
            f(&mut h, r, "east", "bits"),
            "DOOR CLOSED LOCKED PICKPROOF UNDEFINED UNDEFINED "
        );
        assert_eq!(f(&mut h, r, "east", ""), f(&mut h, r, "east", "bits"));
    }

    #[test]
    fn an_exit_needs_a_link_or_the_nowhere_key() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "Temple");
        // Flags and a key alone do not make an exit.
        h.set_num(r, "exit:2:flags", 1);
        h.set_num(r, "exit:2:key", 5);
        assert_eq!(f(&mut h, r, "south", ""), "");
        assert_eq!(f(&mut h, r, "south", "key"), "");
        assert_eq!(h.room_exit(r, 2), None);
        h.set_num(r, "exit:2:nowhere", 1);
        assert_eq!(f(&mut h, r, "south", ""), "DOOR ");
        assert_eq!(f(&mut h, r, "south", "key"), "5");
        h.set_num(r, "exit:2:nowhere", 0);
        assert_eq!(h.room_exit(r, 2), None);
        // A link to an entity that no longer exists is no exit, as the
        // commands mock reads it.
        h.set_link(r, "exit:2", Some(-77));
        assert_eq!(h.room_exit(r, 2), None);
        assert_eq!(f(&mut h, r, "south", "vnum"), "");
    }

    #[test]
    fn the_void_holds_full_globals() {
        let mut h = MockHost::new();
        let void = h.add_room(0, "The Void");
        // No script: empty, and the C's message.
        assert_eq!(f(&mut h, void, "anything", ""), "");
        assert_eq!(
            h.logs,
            vec![
                "Trigger: trigger 5, Vnum 5, type 2. Trying to access Global var list of void. \
                 Apparently this has not been set up!"
                    .to_owned()
            ]
        );
        // With a script: a global by name, any context, case-insensitive;
        // missing is empty; no log.
        h.ensure_script(void)
            .unwrap()
            .global_vars
            .add_var("Gold", "42", 77);
        h.logs.clear();
        assert_eq!(f(&mut h, void, "gold", ""), "42");
        assert_eq!(f(&mut h, void, "nothing", ""), "");
        // The void answers nothing from the table: `name` is a global lookup.
        assert_eq!(f(&mut h, void, "name", ""), "");
        assert_eq!(f(&mut h, void, "vnum", ""), "");
        assert!(h.logs.is_empty());
    }

    #[test]
    fn results_are_cut_like_snprintf() {
        let mut h = MockHost::new();
        let r = h.add_room(3001, "x");
        h.set_text(r, "name", &"n".repeat(1000));
        assert_eq!(f(&mut h, r, "name", "").len(), crate::fields_obj::REPL_MAX);
    }

    #[test]
    fn fields_from_the_corpus() {
        // lib/world/trg: `eval today %here.weather%`, `if %here.north%`,
        // `eval dest %here.north(vnum)%`, `%here.east(bits)%`,
        // `eval temp %23.sector%`.
        let mut h = MockHost::new();
        let here = h.add_room(100, "Here");
        let dest = h.add_room(101, "There");
        h.set_num(dest, "sector", 2);
        h.globals.nums.insert("sky".to_owned(), 2);
        assert_eq!(f(&mut h, here, "weather", ""), "rainy");
        assert_eq!(f(&mut h, here, "north", ""), "");
        h.set_link(here, "exit:0", Some(dest));
        h.set_link(here, "exit:1", Some(dest));
        h.set_num(here, "exit:1:flags", 1);
        assert_eq!(f(&mut h, here, "north", "vnum"), "101");
        assert_eq!(f(&mut h, here, "east", "bits"), "DOOR ");
        assert_eq!(f(&mut h, dest, "sector", ""), "Field");
    }
}
