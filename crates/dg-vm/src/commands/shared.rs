//! What the three command files have in common: the log prefixes, the
//! commands whose C copies differ only in wording or lookup (`door`,
//! `zoneecho`, `recho`, `load`), and the room-finding functions.

use super::ExitField;
use super::comm::{send_to_range, send_to_zone};
use crate::cstr::{
    any_one_arg, atoi, is_abbrev, is_number, one_argument, search_block, skip_spaces, str_eq,
    two_arguments,
};
use crate::dispatch::{enter_wtrigger, load_mtrigger, load_otrigger};
use crate::fields_char::CharStat;
use crate::host::DgHost;
use crate::lookup::{
    FIND_INDEX_LAST, Where, c_vnum, can_wear_on_pos, find_eq_pos_script, get_char, get_char_by_obj,
    get_char_in_room, get_char_near_obj, get_char_room_vis, get_char_world_vis, get_number,
    get_obj, get_obj_by_obj, get_obj_in_room, get_obj_near_obj, get_obj_vis, get_room, idx_vnum,
    obj_room,
};
use crate::types::{DIRS, ITEM_CONTAINER, LVL_GRGOD, UID_CHAR, Uid, uid_text};

/// `ROOM_PRIVATE`, `ROOM_GODROOM`, `ROOM_HOUSE` (`structs.h`).
const ROOM_PRIVATE: usize = 9;
const ROOM_GODROOM: usize = 10;
const ROOM_HOUSE: usize = 11;

/// `PLR_WRITING` (a bit of the player's act flags).
pub(crate) const PLR_WRITING: u32 = 4;
/// `PRF_NOHASSLE`, `PRF_COLOR_1` and `PRF_COLOR_2` (bits of the preference
/// flags).
pub(crate) const PRF_NOHASSLE: u32 = 8;
const PRF_COLOR_1: u32 = 13;
const PRF_COLOR_2: u32 = 14;

/// `POS_SLEEPING`; `AWAKE(ch)` is `GET_POS(ch) > POS_SLEEPING`.
pub(crate) const POS_SLEEPING: i32 = 4;

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
                format!("Mob ({short}, VNum {}):: {message}", c_vnum(host.vnum(ch)))
            }
            Owner::Obj(obj) => {
                format!(
                    "Obj ({}, VNum {}):: {message}",
                    host.name(obj),
                    c_vnum(host.vnum(obj))
                )
            }
            Owner::Wld(room) => format!("Room {} :: {message}", host.vnum(room)),
        };
        host.script_log(&line);
    }
}

/// `GET_POS(ch)`.
pub(crate) fn pos<H: DgHost + ?Sized>(host: &H, ch: Uid) -> i32 {
    host.char_stat(ch, CharStat::Pos)
}

/// `AWAKE(ch)`.
pub(crate) fn awake<H: DgHost + ?Sized>(host: &H, ch: Uid) -> bool {
    pos(host, ch) > POS_SLEEPING
}

/// `ch->desc != NULL`.
pub(crate) fn has_desc<H: DgHost + ?Sized>(host: &H, ch: Uid) -> bool {
    host.cmd_desc(ch).is_some()
}

/// `COLOR_LEV(ch)` (`_clrlevel`, `screen.h`): 0 for an NPC, else the
/// `PRF_COLOR_1` and `PRF_COLOR_2` bits as a number.
pub(crate) fn color_level<H: DgHost + ?Sized>(host: &H, ch: Uid) -> i32 {
    if host.is_npc(ch) {
        return 0;
    }
    i32::from(host.char_pref_flagged(ch, PRF_COLOR_1))
        + 2 * i32::from(host.char_pref_flagged(ch, PRF_COLOR_2))
}

/// `DIR_COUNT`: 6, or 10 with `CONFIG_DIAGONAL_DIRS`.
pub(crate) fn dir_count<H: DgHost + ?Sized>(host: &H) -> usize {
    if host.subst_diagonal_dirs() { 10 } else { 6 }
}

/// `world[room].dir_option[dir]->to_room`, `None` when there is no such exit
/// or it leads to `NOWHERE`.
pub(crate) fn exit_to_room<H: DgHost + ?Sized>(host: &H, room: Uid, dir: usize) -> Option<Uid> {
    host.room_exit(room, dir).and_then(|exit| exit.to_room)
}

/// What the buffer holds after a failed `get_char_room_vis` / `get_char_vis`
/// on a name without a UID. A `0.` count sends the lookup to
/// `get_player_vis(ch, name, NULL, ...)`, which runs `get_number` on the same
/// buffer again, and a world search that fails does so once more.
pub(crate) fn shown_after_char(arg: &str, scope: Where) -> String {
    if arg.starts_with(UID_CHAR) {
        return arg.to_owned();
    }
    let (count, bare) = get_number(arg);
    if count != 0 {
        return bare.to_owned();
    }
    let once = get_number(bare).1;
    match scope {
        Where::Room => once.to_owned(),
        Where::World => get_number(once).1.to_owned(),
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

    // `get_room` hands `real_room` the unwrapped `atoi`; `real_room` takes a
    // `room_vnum`, so a plain number is narrowed first. UIDs go through
    // `get_room` as they are.
    let room = if crate::types::parse_uid(&target).is_some() {
        get_room(host, &target)
    } else {
        host.real_room(idx_vnum(atoi(&target)))
    };
    let Some(room) = room else {
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
        3 => host.cmd_exit_set(room, dir, ExitField::Key(idx_vnum(atoi(value)))),
        4 => host.cmd_exit_set(room, dir, ExitField::Keyword(value.to_owned())),
        _ => match host.real_room(idx_vnum(atoi(value))) {
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
    } else if let Some(zone) = host.cmd_zone_of_vnum(idx_vnum(atoi(&room_number))) {
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
    let raw_number = atoi(&arg2);

    if arg1.is_empty() || arg2.is_empty() || !is_number(&arg2) || raw_number < 0 {
        owner.log(host, &format!("{letter}load: bad syntax"));
        return;
    }
    // `read_mobile` / `read_object` take an `mob_vnum` / `obj_vnum`.
    let number = idx_vnum(raw_number);

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
                host.real_room(idx_vnum(atoi(target)))
            } else {
                None
            };
            match found {
                Some(room) => Some(room),
                None => {
                    owner.log(
                        host,
                        &format!(
                            "{letter}load: room target vnum doesn't exist (loading mob vnum {raw_number} to room {target})"
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
                    // `get_char_room_vis` has already stripped any `N.`.
                    get_obj_vis(host, ch, &shown_after_char(&name, Where::Room))
                }
            }
            Owner::Obj(obj) => get_obj_near_obj(host, obj, &name),
            Owner::Wld(room) => get_obj_in_room(host, room, &name),
        };
        if let Some(cnt) = cnt
            && host.obj_type(cnt) == ITEM_CONTAINER
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

    let location = if starts_with_digit(&roomstr) && !roomstr.contains('.') {
        match host.real_room(idx_vnum(atoi(&roomstr))) {
            Some(room) => room,
            None => {
                host.cmd_send(ch, "No room exists with that number.\r\n");
                return None;
            }
        }
    } else {
        match room_of_name(host, ch, &roomstr) {
            Ok(room) => room,
            Err(refusal) => {
                host.cmd_send(ch, refusal);
                return None;
            }
        }
    };

    // A location has been found; from LVL_GRGOD up there are no restrictions.
    if host.level(ch) >= LVL_GRGOD {
        return Some(location);
    }

    if host.room_flagged(location, ROOM_GODROOM) {
        host.cmd_send(ch, "You are not godly enough to use that room!\r\n");
    } else if host.room_flagged(location, ROOM_PRIVATE) && host.room_people(location).len() > 1 {
        host.cmd_send(
            ch,
            "There's a private conversation going on in that room.\r\n",
        );
    } else if host.room_flagged(location, ROOM_HOUSE)
        && !host.cmd_house_can_enter(ch, host.vnum(location))
    {
        host.cmd_send(ch, "That's private property -- no trespassing!\r\n");
    } else {
        return Some(location);
    }
    None
}

/// The room of the visible character or object `roomstr` names, or what
/// `find_target_room` says when there is none.
///
/// The C reads the count once (`num = get_number(&mobobjstr)`) and hands
/// `&num` to both lookups, so the second carries on from what the first left.
fn room_of_name<H: DgHost + ?Sized>(host: &H, ch: Uid, roomstr: &str) -> Result<Uid, &'static str> {
    let (num, name) = get_number(roomstr);
    // A negative count other than `last.` is counted further from zero by
    // every match and so never finds anything; only `self` / `me`, which the
    // character lookup tests before it looks at the count, still answer.
    let never_matches = num < 0 && num != FIND_INDEX_LAST;
    let target_mob = if never_matches {
        (str_eq(name, "self") || str_eq(name, "me")).then_some(ch)
    } else {
        get_char_world_vis(host, ch, &counted(num, name))
    };
    if let Some(target_mob) = target_mob {
        return host
            .in_room(target_mob)
            .ok_or("That character is currently lost.\r\n");
    }
    // `get_char_vis` counted `num` down as it passed matches; the object
    // search carries on from what is left.
    let left = remaining_count(host, ch, num, name);
    let target_obj = if never_matches {
        None
    } else {
        get_obj_vis(host, ch, &counted(left, name))
    };
    let Some(target_obj) = target_obj else {
        return Err("Nothing exists by that name.\r\n");
    };
    host.in_room(target_obj)
        .or_else(|| {
            host.obj_carried_by(target_obj)
                .and_then(|holder| host.in_room(holder))
        })
        .or_else(|| {
            host.obj_worn_by(target_obj)
                .and_then(|wearer| host.in_room(wearer))
        })
        .ok_or("That object is currently not in a room.\r\n")
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
    while matches < num - 1 && get_char_world_vis(host, ch, &counted(matches + 1, name)).is_some() {
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
        host.real_room(idx_vnum(atoi(&roomstr)))?
    } else if let Some(target_mob) = get_char_by_obj(host, obj, &roomstr) {
        // A character in no room has no flags to read.
        host.in_room(target_mob)?
    } else {
        host.in_room(get_obj_by_obj(host, obj, &roomstr)?)?
    };

    if host.room_flagged(location, ROOM_GODROOM) || host.room_flagged(location, ROOM_HOUSE) {
        return None;
    }
    if host.room_flagged(location, ROOM_PRIVATE) && host.room_people(location).len() > 1 {
        return None;
    }
    Some(location)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::commands::support::World;

    fn find(w: &mut World, raw: &str) -> Option<Uid> {
        let alice = w.player;
        w.clear();
        find_target_room(&mut w.host, alice, raw)
    }

    fn said(w: &World) -> Vec<String> {
        w.sent_to(w.player)
    }

    #[test]
    fn load_log_prints_the_unwrapped_number_and_door_narrows_like_the_c() {
        let mut w = World::new();
        let room = w.room;
        // C: `int number` is printed as written, narrowed only at read_mobile.
        load(&mut w.host, Owner::Mob(w.mob), "mob 70000 x");
        assert!(
            w.host
                .logs
                .iter()
                .any(|l| l.contains("loading mob vnum 70000 to room  x"))
        );
        // obj_vnum key is ush_int: -1 stores 65535, 70000 stores 4464.
        door(&mut w.host, Owner::Mob(w.mob), "3001 north key -1");
        assert_eq!(w.host.num(room, "exit:0:key"), 65535);
        door(&mut w.host, Owner::Mob(w.mob), "3001 north key 70000");
        assert_eq!(w.host.num(room, "exit:0:key"), 4464);
        // room_vnum 68537 == 3001.
        door(&mut w.host, Owner::Mob(w.mob), "68537 north purge");
        assert_eq!(w.host.link(room, "exit:0"), None);
    }

    #[test]
    fn a_room_number_names_a_room() {
        let mut w = World::new();
        let room2 = w.room2;
        assert_eq!(find(&mut w, "3002"), Some(room2));
        assert!(said(&w).is_empty());
        assert_eq!(find(&mut w, "9999"), None);
        assert_eq!(said(&w), ["No room exists with that number.\r\n"]);
        assert_eq!(find(&mut w, ""), None);
        assert_eq!(said(&w), ["You must supply a room number or name.\r\n"]);
        assert_eq!(find(&mut w, "   "), None);
        // Only the first word is read, and a number with a dot is a name.
        assert_eq!(find(&mut w, "3002 3001"), Some(room2));
        assert_eq!(find(&mut w, "3.002"), None);
        assert_eq!(said(&w), ["Nothing exists by that name.\r\n"]);
        // atoi stops at the first non-digit.
        assert_eq!(find(&mut w, "3002abc"), Some(room2));
    }

    #[test]
    fn a_character_or_object_names_its_room() {
        let mut w = World::new();
        let (room, room2, bob, lamp) = (w.room, w.room2, w.player2, w.obj);
        assert_eq!(find(&mut w, "bob"), Some(room));
        w.host.char_to_room(bob, room2);
        assert_eq!(find(&mut w, "bob"), Some(room2));
        assert_eq!(find(&mut w, "lamp"), Some(room));
        // Carried or worn: where the holder is.
        w.host.obj_to_char(lamp, bob);
        assert_eq!(find(&mut w, "lamp"), Some(room2));
        w.host.equip_char(bob, lamp, 3);
        assert_eq!(find(&mut w, "lamp"), Some(room2));
        // In a container, the object is in no room.
        let sack = w.host.add_obj(7100, "a sack");
        w.host.obj_to_obj(lamp, sack);
        w.host.obj_to_room(sack, room2);
        assert_eq!(find(&mut w, "lamp"), None);
        assert_eq!(said(&w), ["That object is currently not in a room.\r\n"]);
        // A character in no room is lost.
        w.host.char_from_room(bob);
        assert_eq!(find(&mut w, "bob"), None);
        assert_eq!(said(&w), ["That character is currently lost.\r\n"]);
        assert_eq!(find(&mut w, "zed"), None);
        assert_eq!(said(&w), ["Nothing exists by that name.\r\n"]);
    }

    #[test]
    fn the_object_search_carries_on_counting_from_the_character_search() {
        // `get_char_vis` counts the number off as it passes matches, and the
        // object search starts from what is left.
        let mut w = World::new();
        let (room, room2) = (w.room, w.room2);
        let statue = w.host.add_obj(1, "a guard statue");
        w.host.set_text(statue, "alias", "guard statue");
        w.host.obj_to_room(statue, room2);
        // One guard (the mob) is passed over, then the first "guard" object.
        assert_eq!(find(&mut w, "guard"), Some(room));
        assert_eq!(find(&mut w, "1.guard"), Some(room));
        assert_eq!(find(&mut w, "2.guard"), Some(room2));
        assert_eq!(find(&mut w, "3.guard"), None);
        assert_eq!(said(&w), ["Nothing exists by that name.\r\n"]);
        // Zero and non-numeric counts are not counted down: a player of that
        // name, or nothing; `last.` takes the last match.
        assert_eq!(find(&mut w, "0.guard"), None);
        assert_eq!(find(&mut w, "x.guard"), None);
        assert_eq!(find(&mut w, "last.guard"), Some(room));
        assert_eq!(find(&mut w, "0.bob"), Some(room));
        // With a second guard the count is used up on characters.
        let guard2 = w.host.add_mob(3000, "the other guard");
        w.host.set_text(guard2, "alias", "guard");
        w.host.char_to_room(guard2, room2);
        assert_eq!(find(&mut w, "2.guard"), Some(room2));
        assert_eq!(find(&mut w, "3.guard"), Some(room2));
        assert_eq!(find(&mut w, "4.guard"), None);
    }

    #[test]
    fn some_rooms_are_closed_to_all_but_the_highest_gods() {
        let mut w = World::new();
        let (room2, alice, bob) = (w.room2, w.player, w.player2);
        w.host.set_num(room2, "flag:10", 1);
        assert_eq!(find(&mut w, "3002"), None);
        assert_eq!(said(&w), ["You are not godly enough to use that room!\r\n"]);
        w.host.set_num(alice, "level", 32);
        assert_eq!(find(&mut w, "3002"), None);
        w.host.set_num(alice, "level", 33);
        assert_eq!(find(&mut w, "3002"), Some(room2));
        assert!(said(&w).is_empty());
        w.host.set_num(alice, "level", 0);
        w.host.set_num(room2, "flag:10", 0);

        // Private rooms with company.
        w.host.set_num(room2, "flag:9", 1);
        w.host.char_to_room(bob, room2);
        assert_eq!(find(&mut w, "3002"), Some(room2));
        w.host.char_to_room(alice, room2);
        assert_eq!(find(&mut w, "3002"), None);
        assert_eq!(
            said(&w),
            ["There's a private conversation going on in that room.\r\n"]
        );
        w.host.set_num(room2, "flag:9", 0);

        // Houses, by permission.
        w.host.set_num(room2, "flag:11", 1);
        assert_eq!(find(&mut w, "3002"), None);
        assert_eq!(said(&w), ["That's private property -- no trespassing!\r\n"]);
        w.host.globals.nums.insert("house_enter:3002".to_owned(), 1);
        assert_eq!(find(&mut w, "3002"), Some(room2));
    }

    #[test]
    fn the_god_room_test_comes_before_the_others() {
        let mut w = World::new();
        let room2 = w.room2;
        for flag in [9, 10, 11] {
            w.host.set_num(room2, &format!("flag:{flag}"), 1);
        }
        assert_eq!(find(&mut w, "3002"), None);
        assert_eq!(said(&w), ["You are not godly enough to use that room!\r\n"]);
    }

    #[test]
    fn contains_obj_walks_the_containers() {
        let mut w = World::new();
        let (a, b, c) = (
            w.host.add_obj(1, "a"),
            w.host.add_obj(2, "b"),
            w.host.add_obj(3, "c"),
        );
        w.host.obj_to_obj(c, b);
        w.host.obj_to_obj(b, a);
        assert!(obj_contains_obj(&w.host, a, c));
        assert!(obj_contains_obj(&w.host, b, c));
        assert!(!obj_contains_obj(&w.host, c, a));
        assert!(!obj_contains_obj(&w.host, c, c));
        assert!(!obj_contains_obj(&w.host, w.obj, a));
    }

    #[test]
    fn owner_log_prefixes() {
        let mut w = World::new();
        Owner::Mob(w.mob).log(&mut w.host, "m");
        Owner::Obj(w.obj).log(&mut w.host, "o");
        Owner::Wld(w.room).log(&mut w.host, "w");
        assert_eq!(
            w.host.logs,
            [
                "Mob (the guard, VNum 3000):: m",
                "Obj (a lamp, VNum 7000):: o",
                "Room 3001 :: w"
            ]
        );
        assert_eq!(Owner::Mob(1).letter(), 'm');
        assert_eq!(Owner::Obj(1).letter(), 'o');
        assert_eq!(Owner::Wld(1).letter(), 'w');
    }
}
