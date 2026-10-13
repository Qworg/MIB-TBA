//! `dg_objcmd.c`: the object script commands, `obj_cmd_info[]` and
//! `obj_command_interpreter`.
//!
//! The table entries end in a space (`"oecho "`) and the word is matched with
//! `strncmp(entry, word, strlen(word))`, in table order, so an abbreviation
//! selects the first entry it begins. The full name still reaches its own
//! entry (`oechoaround` is not a prefix of `"oecho "`), and a word longer than
//! an entry never matches it.

use super::comm::{TO_CHAR, TO_ROOM, sub_write};
use super::misc::script_damage;
use super::shared::{
    Owner, door, find_obj_target_room, load, obj_contains_obj, recho, starts_with_digit, teleport,
    zoneecho,
};
use super::strncmp_lookup;
use crate::cstr::{any_one_arg, atoi, is_number, one_argument, skip_spaces, str_eq, two_arguments};
use crate::host::DgHost;
use crate::lookup::{NUM_WEARS, get_char_by_obj, get_obj_by_obj, obj_room};
use crate::types::Uid;

/// How many `oat` levels deep a command may nest (the C is bounded only by
/// the length of the line).
pub(crate) const MAX_NEST: u32 = 64;

/// `NUM_OBJ_VAL_POSITIONS`.
const NUM_OBJ_VAL_POSITIONS: i32 = 4;

/// One object script command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjCommand {
    Oasound,
    Oat,
    Odoor,
    Odamage,
    Oecho,
    Oechoaround,
    Oforce,
    Oload,
    Opurge,
    Orecho,
    Osend,
    Osetval,
    Oteleport,
    Otimer,
    Otransform,
    Ozoneecho,
    Omove,
    Olog,
}

/// `obj_cmd_info[]`. The first entry, `RESERVED`, has no function (it is
/// there for special procedures) and is matched like any other.
pub const OBJ_CMD_INFO: &[(&str, Option<ObjCommand>)] = &[
    ("RESERVED", None),
    ("oasound ", Some(ObjCommand::Oasound)),
    ("oat ", Some(ObjCommand::Oat)),
    ("odoor ", Some(ObjCommand::Odoor)),
    ("odamage ", Some(ObjCommand::Odamage)),
    ("oecho ", Some(ObjCommand::Oecho)),
    ("oechoaround ", Some(ObjCommand::Oechoaround)),
    ("oforce ", Some(ObjCommand::Oforce)),
    ("oload ", Some(ObjCommand::Oload)),
    ("opurge ", Some(ObjCommand::Opurge)),
    ("orecho ", Some(ObjCommand::Orecho)),
    ("osend ", Some(ObjCommand::Osend)),
    ("osetval ", Some(ObjCommand::Osetval)),
    ("oteleport ", Some(ObjCommand::Oteleport)),
    ("otimer ", Some(ObjCommand::Otimer)),
    ("otransform ", Some(ObjCommand::Otransform)),
    ("ozoneecho ", Some(ObjCommand::Ozoneecho)),
    ("omove ", Some(ObjCommand::Omove)),
    ("olog ", Some(ObjCommand::Olog)),
];

/// `obj_command_interpreter(obj, argument)`.
pub fn interpret<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    interpret_nested(host, obj, argument, 0);
}

fn interpret_nested<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str, nest: u32) {
    let argument = skip_spaces(argument);

    // Just drop to the next line for hitting CR.
    if argument.is_empty() {
        return;
    }
    let (arg, line) = any_one_arg(argument);

    let names: Vec<&str> = OBJ_CMD_INFO.iter().map(|(name, _)| *name).collect();
    // An empty word (a line led by a tab) selects `RESERVED`, whose function
    // pointer is NULL in the C; it is reported as unknown here.
    let command = strncmp_lookup(&names, &arg)
        .and_then(|index| OBJ_CMD_INFO.get(index))
        .and_then(|(_, command)| *command);

    match command {
        None => Owner::Obj(obj).log(host, &format!("Unknown object cmd: '{argument}'")),
        Some(command) => run(host, obj, command, line, nest),
    }
}

fn run<H: DgHost + ?Sized>(host: &mut H, obj: Uid, command: ObjCommand, argument: &str, nest: u32) {
    match command {
        ObjCommand::Oasound => do_oasound(host, obj, argument),
        ObjCommand::Oat => do_oat(host, obj, argument, nest),
        ObjCommand::Odoor => door(host, Owner::Obj(obj), argument),
        ObjCommand::Odamage => do_odamage(host, obj, argument),
        ObjCommand::Oecho => do_oecho(host, obj, argument),
        ObjCommand::Oechoaround => do_osend(host, obj, argument, true),
        ObjCommand::Oforce => do_oforce(host, obj, argument),
        ObjCommand::Oload => load(host, Owner::Obj(obj), argument),
        ObjCommand::Opurge => do_opurge(host, obj, argument),
        ObjCommand::Orecho => recho(host, Owner::Obj(obj), argument),
        ObjCommand::Osend => do_osend(host, obj, argument, false),
        ObjCommand::Osetval => do_osetval(host, obj, argument),
        ObjCommand::Oteleport => do_oteleport(host, obj, argument),
        ObjCommand::Otimer => do_otimer(host, obj, argument),
        ObjCommand::Otransform => do_otransform(host, obj, argument),
        ObjCommand::Ozoneecho => zoneecho(host, Owner::Obj(obj), argument),
        ObjCommand::Omove => do_omove(host, obj, argument),
        ObjCommand::Olog => do_olog(host, obj, argument),
    }
}

/// `sub_write` to a room's first occupant, `TO_ROOM` then `TO_CHAR`, which
/// is how an object addresses a whole room.
fn echo_room<H: DgHost + ?Sized>(host: &mut H, text: &str, room: Uid) {
    if let Some(first) = host.room_people(room).first().copied() {
        sub_write(host, text, first, true, TO_ROOM);
        sub_write(host, text, first, true, TO_CHAR);
    }
}

fn do_oecho<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let argument = skip_spaces(argument);
    let log = Owner::Obj(obj);

    if argument.is_empty() {
        log.log(host, "oecho called with no args");
    } else if let Some(room) = obj_room(host, obj) {
        echo_room(host, argument, room);
    } else {
        log.log(host, "oecho called by object in NOWHERE");
    }
}

fn do_olog<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let argument = skip_spaces(argument);
    if !argument.is_empty() {
        Owner::Obj(obj).log(host, argument);
    }
}

fn do_oforce<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (arg1, line) = one_argument(argument);
    let log = Owner::Obj(obj);

    if arg1.is_empty() || line.is_empty() {
        log.log(host, "oforce called with too few args");
        return;
    }

    if str_eq(&arg1, "all") {
        match obj_room(host, obj) {
            None => log.log(host, "oforce called by object in NOWHERE"),
            Some(room) => {
                for ch in host.room_people(room) {
                    if host.exists(ch) && host.valid_dg_target(ch, false) {
                        host.mud_command(ch, line);
                    }
                }
            }
        }
    } else if let Some(ch) = get_char_by_obj(host, obj, &arg1) {
        if host.valid_dg_target(ch, false) {
            host.mud_command(ch, line);
        }
    } else {
        log.log(host, "oforce: no target found");
    }
}

/// `osend` (`around` false) and `oechoaround`.
fn do_osend<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str, around: bool) {
    let (buf, msg) = any_one_arg(argument);
    let log = Owner::Obj(obj);

    if buf.is_empty() {
        log.log(host, "osend called with no args");
        return;
    }
    let msg = skip_spaces(msg);
    if msg.is_empty() {
        log.log(host, "osend called without a message");
        return;
    }

    if let Some(ch) = get_char_by_obj(host, obj, &buf) {
        sub_write(host, msg, ch, true, if around { TO_ROOM } else { TO_CHAR });
    } else {
        log.log(host, "no target found for osend");
    }
}

/// Set the object's timer value.
fn do_otimer<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    let log = Owner::Obj(obj);

    if arg.is_empty() {
        log.log(host, "otimer: missing argument");
    } else if !starts_with_digit(&arg) {
        log.log(host, "otimer: bad argument");
    } else {
        host.cmd_set_obj_timer(obj, atoi(&arg));
    }
}

/// Transform into a different object.
fn do_otransform<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    let log = Owner::Obj(obj);

    if arg.is_empty() {
        log.log(host, "otransform: missing argument");
    } else if !starts_with_digit(&arg) {
        log.log(host, "otransform: bad argument");
    } else if !host.cmd_transform_obj(obj, atoi(&arg)) {
        log.log(host, "otransform: bad object vnum");
    }
}

/// Purge all objects and NPCs in the room, or the specified object or mob.
fn do_opurge<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        // Purge all.
        if let Some(rm) = obj_room(host, obj) {
            for ch in host.room_people(rm) {
                if host.is_npc(ch) {
                    host.cmd_extract_char(ch);
                }
            }
            for o in host.room_contents(rm) {
                // `extract_obj` takes an object's contents with it, so the
                // running object must be spared along with whatever holds it.
                if host.exists(o) && o != obj && !obj_contains_obj(host, o, obj) {
                    host.cmd_extract_obj(o);
                }
            }
        }
        return;
    }

    let Some(ch) = get_char_by_obj(host, obj, &arg) else {
        match get_obj_by_obj(host, obj, &arg) {
            Some(o) => {
                // Purging a container purges what is inside it, so the
                // running object is gone either way: named directly, or
                // held by the thing that was named.
                if o == obj || obj_contains_obj(host, o, obj) {
                    *host.owner_purged_mut() = true;
                }
                host.cmd_extract_obj(o);
            }
            None => Owner::Obj(obj).log(host, "opurge: bad argument"),
        }
        return;
    };

    if !host.is_npc(ch) {
        Owner::Obj(obj).log(host, "opurge: purging a PC");
        return;
    }
    host.cmd_extract_char(ch);
}

fn do_oteleport<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (arg1, arg2, _) = two_arguments(argument);
    let log = Owner::Obj(obj);

    if arg1.is_empty() || arg2.is_empty() {
        log.log(host, "oteleport called with too few args");
        return;
    }

    let Some(target) = find_obj_target_room(host, obj, &arg2) else {
        log.log(host, "oteleport target is an invalid room");
        return;
    };

    if str_eq(&arg1, "all") {
        let Some(rm) = obj_room(host, obj) else {
            log.log(host, "oteleport called by an object that is nowhere");
            return;
        };
        if target == rm {
            log.log(host, "oteleport target is itself");
            return;
        }
        for ch in host.room_people(rm) {
            if host.exists(ch) && host.valid_dg_target(ch, true) {
                teleport(host, ch, target);
            }
        }
    } else if let Some(ch) = get_char_by_obj(host, obj, &arg1) {
        if host.valid_dg_target(ch, true) {
            teleport(host, ch, target);
        }
    } else {
        log.log(host, "oteleport: no target found");
    }
}

fn do_odamage<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (name, amount, _) = two_arguments(argument);

    // Whether the amount is a number is not checked; if not it is 0.
    if name.is_empty() || amount.is_empty() {
        Owner::Obj(obj).log(host, "odamage: bad syntax");
        return;
    }
    let dam = atoi(&amount);
    let Some(ch) = get_char_by_obj(host, obj, &name) else {
        Owner::Obj(obj).log(host, "odamage: target not found");
        return;
    };
    script_damage(host, ch, dam);
}

fn do_oasound<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let argument = skip_spaces(argument);
    let log = Owner::Obj(obj);

    if argument.is_empty() {
        log.log(host, "oasound called with no args");
        return;
    }
    let Some(room) = obj_room(host, obj) else {
        log.log(host, "oasound called by object in NOWHERE");
        return;
    };

    for door in 0..host.cmd_dir_count() {
        if let Some(to) = host.cmd_exit_to_room(room, door)
            && to != room
        {
            echo_room(host, argument, to);
        }
    }
}

fn do_osetval<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (arg1, arg2, _) = two_arguments(argument);
    let log = Owner::Obj(obj);

    if arg1.is_empty() || arg2.is_empty() || !is_number(&arg1) || !is_number(&arg2) {
        log.log(host, "osetval: bad syntax");
        return;
    }
    let position = atoi(&arg1);
    let new_value = atoi(&arg2);

    if (0..NUM_OBJ_VAL_POSITIONS).contains(&position) {
        // A worn object's affects are taken off and put back around the
        // change.
        let worn = host.obj_worn_by(obj).and_then(|wearer| {
            (0..NUM_WEARS)
                .find(|pos| host.equipment(wearer, *pos) == Some(obj))
                .map(|pos| (wearer, pos))
        });
        if let Some((wearer, pos)) = worn {
            host.cmd_unequip_char(wearer, pos);
        }
        host.cmd_set_obj_val(obj, position as usize, new_value);
        if let Some((wearer, pos)) = worn {
            host.cmd_equip_char(wearer, obj, pos);
        }
    } else {
        log.log(host, "osetval: position out of bounds!");
    }
}

/// Run a command as if the object were in another room (submitted by
/// PurpleOnyx). The command runs on a duplicate of the object put in that
/// room, and the duplicate is removed afterwards.
fn do_oat<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str, nest: u32) {
    let (arg, command) = any_one_arg(argument);
    let log = Owner::Obj(obj);

    if arg.is_empty() {
        log.log(host, "oat called with no args");
        return;
    }
    let command = skip_spaces(command);
    if command.is_empty() {
        log.log(host, "oat called without a command");
        return;
    }

    let loc = if starts_with_digit(&arg) {
        host.real_room(atoi(&arg))
    } else {
        get_char_by_obj(host, obj, &arg).and_then(|ch| host.in_room(ch))
    };
    let Some(loc) = loc else {
        log.log(host, &format!("oat: location not found ({arg})"));
        return;
    };

    if nest >= MAX_NEST {
        log.log(host, "oat: commands nested too deeply");
        return;
    }

    let Some(object) = host.cmd_read_object(host.vnum(obj)) else {
        return;
    };

    // The command may purge the duplicate, or the object this script runs
    // on. The caller's own purge flag is put back afterwards, unless the
    // owner is gone, in which case its script must stop.
    let saved_purged = *host.owner_purged_mut();
    *host.owner_purged_mut() = false;

    host.cmd_obj_to_room(object, loc);
    interpret_nested(host, object, command, nest + 1);

    // Wherever the command left it, the duplicate was made to carry one
    // command and nothing else. (Ids are never reused, so existence is
    // enough to tell it is still the same object.)
    if host.exists(object) {
        host.cmd_extract_obj(object);
    }

    *host.owner_purged_mut() = if host.exists(obj) { saved_purged } else { true };
}

fn do_omove<H: DgHost + ?Sized>(host: &mut H, obj: Uid, argument: &str) {
    let (arg1, _) = one_argument(argument);
    let log = Owner::Obj(obj);

    if arg1.is_empty() {
        log.log(host, "omove called with too few args");
        return;
    }
    let Some(target) = find_obj_target_room(host, obj, &arg1) else {
        log.log(host, "omove target is an invalid room");
        return;
    };

    // Remove the object from its current location. A worn object is none of
    // these and is refused.
    if host.obj_carried_by(obj).is_some()
        || host.in_room(obj).is_some()
        || host.obj_in_obj(obj).is_some()
    {
        host.cmd_obj_detach(obj);
    } else {
        log.log(
            host,
            "omove: target object is not in a room, held or in a container!",
        );
        return;
    }
    host.cmd_obj_to_room(obj, target);
}
