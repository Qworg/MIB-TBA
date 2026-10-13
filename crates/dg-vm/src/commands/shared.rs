//! What the three command files have in common: the log prefixes, the
//! commands whose C copies differ only in wording or lookup (`door`,
//! `zoneecho`, `recho`, `load`), and the room-finding functions.

use super::ExitField;
use super::comm::{send_to_range, send_to_zone};
use crate::cstr::{
    any_one_arg, atoi, is_abbrev, is_number, one_argument, search_block, skip_spaces, two_arguments,
};
use crate::dispatch::{enter_wtrigger, load_mtrigger, load_otrigger};
use crate::host::DgHost;
use crate::lookup::{
    FIND_INDEX_LAST, Where, can_wear_on_pos, find_eq_pos_script, get_char, get_char_by_obj,
    get_char_in_room, get_char_near_obj, get_char_room_vis, get_char_vis, get_number, get_obj,
    get_obj_by_obj, get_obj_in_room, get_obj_near_obj, get_obj_vis, get_room, obj_room,
};
use crate::types::{DIRS, LVL_GRGOD, UID_CHAR, Uid, uid_text};

/// `ROOM_PRIVATE`, `ROOM_GODROOM`, `ROOM_HOUSE` (`structs.h`).
const ROOM_PRIVATE: i32 = 9;
const ROOM_GODROOM: i32 = 10;
const ROOM_HOUSE: i32 = 11;

/// `ITEM_WEAR_TAKE`: bit 0 of the wear flags.
const ITEM_WEAR_TAKE: u32 = 1;

/// `door_field[]` of `do_mdoor`/`do_odoor`/`do_wdoor`.
pub(crate) const DOOR_FIELDS: [&str; 6] = ["purge", "description", "flags", "key", "name", "room"];

/// Who is running a command: the C passes `ch`, `obj` or `room` to the
/// per-type function; this carries the same three and selects the per-type
/// wording and lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    Mob(Uid),
    Obj(Uid),
    Wld(Uid),
}

impl Owner {
    /// The `m`/`o`/`w` of the command names.
    pub(crate) fn letter(self) -> char {
        match self {
            Owner::Mob(_) => 'm',
            Owner::Obj(_) => 'o',
            Owner::Wld(_) => 'w',
        }
    }

    /// `mob_log` / `obj_log` / `wld_log`: prefix the owner and send to
    /// `script_log`.
    ///
    /// Deviation: the C splices the owner's name into the format string, so a
    /// `%` in a name is read as a conversion; here the name is data.
    pub(crate) fn log<H: DgHost + ?Sized>(self, host: &mut H, message: &str) {
        let line = match self {
            Owner::Mob(ch) => {
                // `GET_SHORT` is NULL for a player, which `%s` prints as "(null)".
                let short = if host.is_npc(ch) {
                    host.name(ch)
                } else {
                    "(null)".to_owned()
                };
                format!("Mob ({short}, VNum {}):: {message}", host.vnum(ch))
            }
            Owner::Obj(obj) => {
                format!(
                    "Obj ({}, VNum {}):: {message}",
                    host.name(obj),
                    host.vnum(obj)
                )
            }
            Owner::Wld(room) => format!("Room {} :: {message}", host.vnum(room)),
        };
        host.script_log(&line);
    }
}

/// `isdigit(*s)`.
pub(crate) fn starts_with_digit(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
}

/// `obj_contains_obj(container, obj)` (`dg_objcmd.c`): `container` holds
/// `obj` at any depth.
pub(crate) fn obj_contains_obj<H: DgHost + ?Sized>(host: &H, container: Uid, obj: Uid) -> bool {
    let mut current = host.obj_in_obj(obj);
    // The bound only guards against a corrupt cycle.
    for _ in 0..1024 {
        match current {
            Some(i) if i == container => return true,
            Some(i) => current = host.obj_in_obj(i),
            None => return false,
        }
    }
    false
}

/// `char_from_room`, `char_to_room`, `enter_wtrigger(room, ch, -1)`: the
/// body of every script teleport.
pub(crate) fn teleport<H: DgHost + ?Sized>(host: &mut H, vict: Uid, target: Uid) {
    host.cmd_char_from_room(vict);
    host.cmd_char_to_room(vict, target);
    enter_wtrigger(host, target, vict, -1);
}

/// `mdoor` / `odoor` / `wdoor`.
pub(crate) fn door<H: DgHost + ?Sized>(host: &mut H, owner: Owner, argument: &str) {
    let letter = owner.letter();
    let (target, direction, rest) = two_arguments(argument);
    let (field, value) = one_argument(rest);
    let value = skip_spaces(value);

    if target.is_empty() || direction.is_empty() || field.is_empty() {
        owner.log(host, &format!("{letter}door called with too few args"));
        return;
    }

    let Some(room) = get_room(host, &target) else {
        owner.log(
            host,
            &format!("{letter}door: invalid target (arg == {target})"),
        );
        return;
    };

    let Some(dir) = search_block(&direction, &DIRS, false) else {
        owner.log(
            host,
            &format!(
                "{letter}door: invalid direction (arg == {direction}) not found in: [ {} ]",
                DIRS.join(" ")
            ),
        );
        return;
    };

    let Some(field_index) = search_block(&field, &DOOR_FIELDS, false) else {
        owner.log(
            host,
            &format!(
                "{letter}door: invalid field (arg == {field}) not found in: [ {} ]",
                DOOR_FIELDS.join(" ")
            ),
        );
        return;
    };

    match field_index {
        0 => host.cmd_exit_purge(room, dir),
        1 => host.cmd_exit_set(room, dir, ExitField::Description(format!("{value}\r\n"))),
        2 => {
            let flags = crate::types::trig::asciiflag_conv(value) as i16;
            host.cmd_exit_set(room, dir, ExitField::Flags(i32::from(flags)));
        }
        3 => host.cmd_exit_set(room, dir, ExitField::Key(atoi(value))),
        4 => host.cmd_exit_set(room, dir, ExitField::Keyword(value.to_owned())),
        _ => match host.real_room(atoi(value)) {
            Some(to) => host.cmd_exit_set(room, dir, ExitField::ToRoom(Some(to))),
            None => {
                host.cmd_exit_set(room, dir, ExitField::ToRoom(None));
                owner.log(
                    host,
                    &format!("{letter}door: invalid door target (arg == {value})"),
                );
            }
        },
    }
}

/// `mzoneecho` / `ozoneecho` / `wzoneecho`.
pub(crate) fn zoneecho<H: DgHost + ?Sized>(host: &mut H, owner: Owner, argument: &str) {
    let letter = owner.letter();
    let (room_number, msg) = any_one_arg(argument);
    let msg = skip_spaces(msg);

    if room_number.is_empty() || msg.is_empty() {
        owner.log(host, &format!("{letter}zoneecho called with too few args"));
    } else if let Some(zone) = host.cmd_zone_of_vnum(atoi(&room_number)) {
        // The C builds this in a MAX_INPUT_LENGTH buffer, which a message of
        // 510 bytes or more overruns; there is no limit here.
        send_to_zone(host, &format!("{msg}\r\n"), zone);
    } else {
        owner.log(
            host,
            &format!("{letter}zoneecho called for nonexistant zone"),
        );
    }
}

/// `mrecho` / `orecho` / `wrecho`.
pub(crate) fn recho<H: DgHost + ?Sized>(host: &mut H, owner: Owner, argument: &str) {
    let (start, finish, msg) = two_arguments(argument);
    let msg = skip_spaces(msg);

    if msg.is_empty()
        || start.is_empty()
        || finish.is_empty()
        || !is_number(&start)
        || !is_number(&finish)
    {
        let text = match owner {
            Owner::Mob(_) => "mrecho called with too few args",
            Owner::Obj(_) => "orecho: too few args",
            Owner::Wld(_) => "wrecho: too few args",
        };
        owner.log(host, text);
    } else {
        send_to_range(host, atoi(&start), atoi(&finish), msg);
    }
}

/// `add_var(&SCRIPT(owner)->global_vars, "lastloaded", …)`, when the owner
/// still has a script.
fn set_lastloaded<H: DgHost + ?Sized>(host: &mut H, owner: Owner, loaded: Uid) {
    let id = uid_text(loaded);
    let uid = match owner {
        Owner::Mob(uid) | Owner::Obj(uid) | Owner::Wld(uid) => uid,
    };
    if let Some(script) = host.script_mut(uid) {
        script.global_vars.add_var("lastloaded", &id, 0);
    }
}

/// `obj_to_room(obj, room)`, which the C leaves an object in limbo for
/// when `room` is `NOWHERE`; here that is logged and the object not placed.
fn obj_to_room_or_log<H: DgHost + ?Sized>(host: &mut H, obj: Uid, room: Option<Uid>) {
    match room {
        Some(room) => host.cmd_obj_to_room(obj, room),
        None => host.cmd_syslog("SYSERR: Illegal value(s) passed to obj_to_room"),
    }
}

/// `do_mload` / `do_dgoload` / `do_wload`, after the permission checks.
///
/// The per-type differences the C has are all kept: where "here" is, which
/// lookups find the target character and container, when `lastloaded` is
/// set (`wload` sets it only when no target is given), and the wording
/// (`wload` says `mload: bad mob vnum`).
pub(crate) fn load<H: DgHost + ?Sized>(host: &mut H, owner: Owner, argument: &str) {
    let letter = owner.letter();
    let (arg1, arg2, target) = two_arguments(argument);
    let number = atoi(&arg2);

    if arg1.is_empty() || arg2.is_empty() || !is_number(&arg2) || number < 0 {
        owner.log(host, &format!("{letter}load: bad syntax"));
        return;
    }

    let here = match owner {
        Owner::Mob(ch) => host.in_room(ch),
        Owner::Obj(obj) => match obj_room(host, obj) {
            Some(room) => Some(room),
            None => {
                owner.log(host, "oload: object in NOWHERE trying to load");
                return;
            }
        },
        Owner::Wld(room) => Some(room),
    };

    if is_abbrev(&arg1, "mob") {
        let rnum = if target.is_empty() {
            here
        } else {
            let found = if starts_with_digit(target) {
                host.real_room(atoi(target))
            } else {
                None
            };
            match found {
                Some(room) => Some(room),
                None => {
                    owner.log(
                        host,
                        &format!(
                            "{letter}load: room target vnum doesn't exist (loading mob vnum {number} to room {target})"
                        ),
                    );
                    return;
                }
            }
        };
        let Some(mob) = host.cmd_read_mobile(number) else {
            let text = match owner {
                Owner::Obj(_) => "oload: bad mob vnum",
                Owner::Mob(_) | Owner::Wld(_) => "mload: bad mob vnum",
            };
            owner.log(host, text);
            return;
        };
        match rnum {
            Some(room) => host.cmd_char_to_room(mob, room),
            None => host.cmd_syslog("SYSERR: Illegal value(s) passed to char_to_room"),
        }
        set_lastloaded(host, owner, mob);
        load_mtrigger(host, mob);
    } else if is_abbrev(&arg1, "obj") {
        let Some(object) = host.cmd_read_object(number) else {
            owner.log(host, &format!("{letter}load: bad object vnum"));
            return;
        };
        let is_wld = matches!(owner, Owner::Wld(_));
        if !is_wld {
            set_lastloaded(host, owner, object);
        }

        // Objects can be loaded onto a person, into a container, or worn.
        if target.is_empty() {
            match owner {
                Owner::Mob(ch) => {
                    if host.wear_flags(object) & ITEM_WEAR_TAKE != 0 {
                        host.cmd_obj_to_char(object, ch);
                    } else {
                        obj_to_room_or_log(host, object, here);
                    }
                }
                Owner::Obj(_) | Owner::Wld(_) => {
                    obj_to_room_or_log(host, object, here);
                    if is_wld {
                        set_lastloaded(host, owner, object);
                    }
                }
            }
            load_otrigger(host, object);
            return;
        }

        let (name, position, _) = two_arguments(target);
        let tch = match owner {
            Owner::Mob(ch) => {
                if name.starts_with(UID_CHAR) {
                    get_char(host, &name)
                } else {
                    get_char_room_vis(host, ch, &name)
                }
            }
            Owner::Obj(obj) => get_char_near_obj(host, obj, &name),
            Owner::Wld(room) => get_char_in_room(host, room, &name),
        };
        if let Some(tch) = tch {
            if !position.is_empty()
                && let Some(pos) = find_eq_pos_script(&position)
                && host.equipment(tch, pos).is_none()
                && can_wear_on_pos(host, object, pos)
            {
                host.cmd_equip_char(tch, object, pos);
                load_otrigger(host, object);
                return;
            }
            host.cmd_obj_to_char(object, tch);
            load_otrigger(host, object);
            return;
        }
        let cnt = match owner {
            Owner::Mob(ch) => {
                if name.starts_with(UID_CHAR) {
                    get_obj(host, &name)
                } else {
                    get_obj_vis(host, ch, &name)
                }
            }
            Owner::Obj(obj) => get_obj_near_obj(host, obj, &name),
            Owner::Wld(room) => get_obj_in_room(host, room, &name),
        };
        if let Some(cnt) = cnt
            && host.is_container(cnt)
        {
            host.cmd_obj_to_obj(object, cnt);
            load_otrigger(host, object);
            return;
        }
        // Neither a character nor a container: just drop it in the room.
        obj_to_room_or_log(host, object, here);
        load_otrigger(host, object);
    } else {
        owner.log(host, &format!("{letter}load: bad type"));
    }
}

/// `find_target_room(ch, rawroomstr)` (`act.wizard.c`), as the mob commands
/// use it: a room number, or the room of a visible character or object. The
/// refusals are shown to `ch` with `send_to_char`.
pub(crate) fn find_target_room<H: DgHost + ?Sized>(
    host: &mut H,
    ch: Uid,
    rawroomstr: &str,
) -> Option<Uid> {
    let (roomstr, _) = one_argument(rawroomstr);

    if roomstr.is_empty() {
        host.cmd_send(ch, "You must supply a room number or name.\r\n");
        return None;
    }

    let location;
    if starts_with_digit(&roomstr) && !roomstr.contains('.') {
        let Some(room) = host.real_room(atoi(&roomstr)) else {
            host.cmd_send(ch, "No room exists with that number.\r\n");
            return None;
        };
        location = room;
    } else {
        let (num, name) = get_number(&roomstr);
        let mut found = None;
        if let Some(target_mob) = get_char_vis(host, ch, &counted(num, name), Where::World) {
            match host.in_room(target_mob) {
                Some(room) => found = Some(room),
                None => {
                    host.cmd_send(ch, "That character is currently lost.\r\n");
                    return None;
                }
            }
        } else {
            // `get_char_vis` counted `num` down as it passed matches; the
            // object search carries on from what is left.
            let left = remaining_count(host, ch, num, name);
            if let Some(target_obj) = get_obj_vis(host, ch, &counted(left, name)) {
                found = host.in_room(target_obj).or_else(|| {
                    host.obj_carried_by(target_obj)
                        .and_then(|holder| host.in_room(holder))
                        .or_else(|| {
                            host.obj_worn_by(target_obj)
                                .and_then(|wearer| host.in_room(wearer))
                        })
                });
                if found.is_none() {
                    host.cmd_send(ch, "That object is currently not in a room.\r\n");
                    return None;
                }
            }
        }
        let Some(room) = found else {
            host.cmd_send(ch, "Nothing exists by that name.\r\n");
            return None;
        };
        location = room;
    }

    // A location has been found; from LVL_GRGOD up there are no restrictions.
    if host.level(ch) >= LVL_GRGOD {
        return Some(location);
    }

    if host.cmd_room_flagged(location, ROOM_GODROOM) {
        host.cmd_send(ch, "You are not godly enough to use that room!\r\n");
    } else if host.cmd_room_flagged(location, ROOM_PRIVATE) && host.room_people(location).len() > 1
    {
        host.cmd_send(
            ch,
            "There's a private conversation going on in that room.\r\n",
        );
    } else if host.cmd_room_flagged(location, ROOM_HOUSE)
        && !host.cmd_house_can_enter(ch, host.vnum(location))
    {
        host.cmd_send(ch, "That's private property -- no trespassing!\r\n");
    } else {
        return Some(location);
    }
    None
}

/// `N.name` with the count spelled the way `get_number` reads it.
fn counted(num: i32, name: &str) -> String {
    if num == FIND_INDEX_LAST {
        format!("last.{name}")
    } else {
        format!("{num}.{name}")
    }
}

/// How much of `num` is left after a failed `get_char_vis(…, FIND_CHAR_WORLD)`
/// has counted its matches off it: `num` less the visible characters `name`
/// names. A count that is zero or `last` is not counted down.
fn remaining_count<H: DgHost + ?Sized>(host: &H, ch: Uid, num: i32, name: &str) -> i32 {
    if num <= 0 {
        return num;
    }
    let mut matches = 0;
    while matches < num - 1
        && get_char_vis(host, ch, &counted(matches + 1, name), Where::World).is_some()
    {
        matches += 1;
    }
    num - matches
}

/// `find_obj_target_room(obj, rawroomstr)` (`dg_objcmd.c`): a room number or
/// the room of a character or object near `obj`, refusing god rooms, houses
/// and a private room that already has company.
pub(crate) fn find_obj_target_room<H: DgHost + ?Sized>(
    host: &H,
    obj: Uid,
    rawroomstr: &str,
) -> Option<Uid> {
    let (roomstr, _) = one_argument(rawroomstr);
    if roomstr.is_empty() {
        return None;
    }

    let location = if starts_with_digit(&roomstr) && !roomstr.contains('.') {
        host.real_room(atoi(&roomstr))?
    } else if let Some(target_mob) = get_char_by_obj(host, obj, &roomstr) {
        // A character in no room has no flags to read.
        host.in_room(target_mob)?
    } else if let Some(target_obj) = get_obj_by_obj(host, obj, &roomstr) {
        host.in_room(target_obj)?
    } else {
        return None;
    };

    if host.cmd_room_flagged(location, ROOM_GODROOM) || host.cmd_room_flagged(location, ROOM_HOUSE)
    {
        return None;
    }
    if host.cmd_room_flagged(location, ROOM_PRIVATE) && host.room_people(location).len() > 1 {
        return None;
    }
    Some(location)
}
