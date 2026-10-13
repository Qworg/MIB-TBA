//! `dg_wldcmd.c`: the world (room) script commands, `wld_cmd_info[]` and
//! `wld_command_interpreter`.
//!
//! Matched like the object table: `strncmp(entry, word, strlen(word))` in
//! table order against entries that end in a space. `wlog`, the last entry,
//! has no trailing space, so only the full word `wlog` reaches it: `wl` and
//! `wlo` select `wload` (which comes first), and `wlogs` matches nothing.

use super::comm::{TO_CHAR, TO_ROOM, act_to_room, sub_write};
use super::misc::script_damage;
use super::shared::{Owner, door, load, recho, starts_with_digit, teleport, zoneecho};
use super::strncmp_lookup;
use crate::cstr::{any_one_arg, atoi, one_argument, skip_spaces, str_eq, two_arguments};
use crate::host::DgHost;
use crate::lookup::{
    get_char, get_char_by_room, get_char_in_room, get_obj, get_obj_by_room, get_obj_in_room,
};
use crate::types::{UID_CHAR, Uid};

/// One world script command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WldCommand {
    Wasound,
    Wdoor,
    Wecho,
    Wechoaround,
    Wforce,
    Wload,
    Wpurge,
    Wrecho,
    Wsend,
    Wteleport,
    Wzoneecho,
    Wdamage,
    Wat,
    Wmove,
    Wlog,
}

/// `wld_cmd_info[]`.
pub const WLD_CMD_INFO: &[(&str, Option<WldCommand>)] = &[
    ("RESERVED", None),
    ("wasound ", Some(WldCommand::Wasound)),
    ("wdoor ", Some(WldCommand::Wdoor)),
    ("wecho ", Some(WldCommand::Wecho)),
    ("wechoaround ", Some(WldCommand::Wechoaround)),
    ("wforce ", Some(WldCommand::Wforce)),
    ("wload ", Some(WldCommand::Wload)),
    ("wpurge ", Some(WldCommand::Wpurge)),
    ("wrecho ", Some(WldCommand::Wrecho)),
    ("wsend ", Some(WldCommand::Wsend)),
    ("wteleport ", Some(WldCommand::Wteleport)),
    ("wzoneecho ", Some(WldCommand::Wzoneecho)),
    ("wdamage ", Some(WldCommand::Wdamage)),
    ("wat ", Some(WldCommand::Wat)),
    ("wmove ", Some(WldCommand::Wmove)),
    ("wlog", Some(WldCommand::Wlog)),
];

/// How many `wat` levels deep a command may nest (the C is bounded only by
/// the length of the line).
const MAX_NEST: u32 = super::obj::MAX_NEST;

/// `wld_command_interpreter(room, argument)`.
pub fn interpret<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    interpret_nested(host, room, argument, 0);
}

fn interpret_nested<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str, nest: u32) {
    let argument = skip_spaces(argument);

    // Just drop to the next line for hitting CR.
    if argument.is_empty() {
        return;
    }
    let (arg, line) = any_one_arg(argument);

    let names: Vec<&str> = WLD_CMD_INFO.iter().map(|(name, _)| *name).collect();
    // An empty word selects `RESERVED`, whose function pointer is NULL in the
    // C; it is reported as unknown here.
    let command = strncmp_lookup(&names, &arg)
        .and_then(|index| WLD_CMD_INFO.get(index))
        .and_then(|(_, command)| *command);

    match command {
        None => Owner::Wld(room).log(host, &format!("Unknown world cmd: '{argument}'")),
        Some(command) => run(host, room, command, line, nest),
    }
}

fn run<H: DgHost + ?Sized>(
    host: &mut H,
    room: Uid,
    command: WldCommand,
    argument: &str,
    nest: u32,
) {
    match command {
        WldCommand::Wasound => do_wasound(host, room, argument),
        WldCommand::Wdoor => door(host, Owner::Wld(room), argument),
        WldCommand::Wecho => do_wecho(host, room, argument),
        WldCommand::Wechoaround => do_wsend(host, room, argument, true),
        WldCommand::Wforce => do_wforce(host, room, argument),
        WldCommand::Wload => load(host, Owner::Wld(room), argument),
        WldCommand::Wpurge => do_wpurge(host, room, argument),
        WldCommand::Wrecho => recho(host, Owner::Wld(room), argument),
        WldCommand::Wsend => do_wsend(host, room, argument, false),
        WldCommand::Wteleport => do_wteleport(host, room, argument),
        WldCommand::Wzoneecho => zoneecho(host, Owner::Wld(room), argument),
        WldCommand::Wdamage => do_wdamage(host, room, argument),
        WldCommand::Wat => do_wat(host, room, argument, nest),
        WldCommand::Wmove => do_wmove(host, room, argument),
        WldCommand::Wlog => do_wlog(host, room, argument),
    }
}

/// Prints the argument to all the rooms around the room.
fn do_wasound<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    let argument = skip_spaces(argument);

    if argument.is_empty() {
        Owner::Wld(room).log(host, "wasound called with no argument");
        return;
    }

    for door in 0..host.cmd_dir_count() {
        if let Some(to) = host.cmd_exit_to_room(room, door)
            && to != room
        {
            act_to_room(host, argument, to);
        }
    }
}

fn do_wecho<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    let argument = skip_spaces(argument);

    if argument.is_empty() {
        Owner::Wld(room).log(host, "wecho called with no args");
    } else {
        act_to_room(host, argument, room);
    }
}

fn do_wlog<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    let argument = skip_spaces(argument);
    if !argument.is_empty() {
        Owner::Wld(room).log(host, argument);
    }
}

/// `wsend` (`around` false) and `wechoaround`.
fn do_wsend<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str, around: bool) {
    let (buf, msg) = any_one_arg(argument);
    let log = Owner::Wld(room);

    if buf.is_empty() {
        log.log(host, "wsend called with no args");
        return;
    }
    let msg = skip_spaces(msg);
    if msg.is_empty() {
        log.log(host, "wsend called without a message");
        return;
    }

    if let Some(ch) = get_char_by_room(host, room, &buf) {
        sub_write(host, msg, ch, true, if around { TO_ROOM } else { TO_CHAR });
    } else {
        log.log(host, "no target found for wsend");
    }
}

fn do_wteleport<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    let (arg1, arg2, _) = two_arguments(argument);
    let log = Owner::Wld(room);

    if arg1.is_empty() || arg2.is_empty() {
        log.log(host, "wteleport called with too few args");
        return;
    }

    let nr = atoi(&arg2);
    let Some(target) = host.real_room(nr) else {
        log.log(host, "wteleport target is an invalid room");
        return;
    };

    if str_eq(&arg1, "all") {
        if nr == host.vnum(room) {
            log.log(host, "wteleport all target is itself");
            return;
        }
        for ch in host.room_people(room) {
            if host.exists(ch) && host.valid_dg_target(ch, true) {
                teleport(host, ch, target);
            }
        }
    } else if let Some(ch) = get_char_by_room(host, room, &arg1) {
        if host.valid_dg_target(ch, true) {
            teleport(host, ch, target);
        }
    } else {
        log.log(host, "wteleport: no target found");
    }
}

fn do_wforce<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    let (arg1, line) = one_argument(argument);
    let log = Owner::Wld(room);

    if arg1.is_empty() || line.is_empty() {
        log.log(host, "wforce called with too few args");
        return;
    }

    if str_eq(&arg1, "all") {
        for ch in host.room_people(room) {
            if host.exists(ch) && host.valid_dg_target(ch, false) {
                host.mud_command(ch, line);
            }
        }
    } else if let Some(ch) = get_char_by_room(host, room, &arg1) {
        if host.valid_dg_target(ch, false) {
            host.mud_command(ch, line);
        }
    } else {
        log.log(host, "wforce: no target found");
    }
}

/// Purge all objects and NPCs in the room, or the specified object or mob.
fn do_wpurge<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        for ch in host.room_people(room) {
            if host.is_npc(ch) {
                host.cmd_extract_char(ch);
            }
        }
        for obj in host.room_contents(room) {
            if host.exists(obj) {
                host.cmd_extract_obj(obj);
            }
        }
        return;
    }

    let ch = if arg.starts_with(UID_CHAR) {
        get_char(host, &arg)
    } else {
        get_char_in_room(host, room, &arg)
    };
    let Some(ch) = ch else {
        let obj = if arg.starts_with(UID_CHAR) {
            get_obj(host, &arg)
        } else {
            get_obj_in_room(host, room, &arg)
        };
        match obj {
            Some(obj) => host.cmd_extract_obj(obj),
            None => Owner::Wld(room).log(host, "wpurge: bad argument"),
        }
        return;
    };

    if !host.is_npc(ch) {
        Owner::Wld(room).log(host, "wpurge: purging a PC");
        return;
    }
    host.cmd_extract_char(ch);
}

fn do_wdamage<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    let (name, amount, _) = two_arguments(argument);

    // Whether the amount is a number is not checked; if not it is 0.
    if name.is_empty() || amount.is_empty() {
        Owner::Wld(room).log(host, "wdamage: bad syntax");
        return;
    }
    let dam = atoi(&amount);
    let Some(ch) = get_char_by_room(host, room, &name) else {
        Owner::Wld(room).log(host, "wdamage: target not found");
        return;
    };
    script_damage(host, ch, dam);
}

/// Run a world command as if it were another room's.
fn do_wat<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str, nest: u32) {
    let (arg, command) = any_one_arg(argument);
    let log = Owner::Wld(room);

    if arg.is_empty() {
        log.log(host, "wat called with no args");
        return;
    }
    let command = skip_spaces(command);
    if command.is_empty() {
        log.log(host, "wat called without a command");
        return;
    }

    let loc = if starts_with_digit(&arg) {
        host.real_room(atoi(&arg))
    } else {
        get_char_by_room(host, room, &arg).and_then(|ch| host.in_room(ch))
    };
    let Some(loc) = loc else {
        log.log(host, &format!("wat: location not found ({arg})"));
        return;
    };
    if nest >= MAX_NEST {
        log.log(host, "wat: commands nested too deeply");
        return;
    }
    interpret_nested(host, loc, command, nest + 1);
}

fn do_wmove<H: DgHost + ?Sized>(host: &mut H, room: Uid, argument: &str) {
    let (arg1, arg2, _) = two_arguments(argument);
    let log = Owner::Wld(room);

    if arg1.is_empty() || arg2.is_empty() {
        log.log(host, "wmove called with too few args");
        return;
    }

    let nr = atoi(&arg2);
    let Some(target) = host.real_room(nr) else {
        log.log(host, "wmove target is an invalid room");
        return;
    };
    if nr == host.vnum(room) {
        log.log(host, "wmove target room is itself");
    } else if str_eq(&arg1, "all") {
        for obj in host.room_contents(room) {
            if host.exists(obj) {
                host.cmd_obj_detach(obj);
                host.cmd_obj_to_room(obj, target);
            }
        }
    } else if let Some(obj) = get_obj_by_room(host, room, &arg1) {
        // `get_obj_by_room` can find an object anywhere in the world. The C
        // then calls `obj_from_room` on one that is not lying in a room,
        // which only logs, and `obj_to_room` anyway, leaving it in two
        // places; here such an object is left where it is.
        if host.in_room(obj).is_some() {
            host.cmd_obj_detach(obj);
            host.cmd_obj_to_room(obj, target);
        } else {
            host.cmd_syslog("SYSERR: NULL object or obj not in a room passed to obj_from_room");
        }
    } else {
        log.log(host, "wmove: no target found");
    }
}
