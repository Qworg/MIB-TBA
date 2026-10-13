//! `dg_mobcmd.c`: the mob script commands, and the `mob_script_commands`
//! table of `interpreter.c`.
//!
//! Every command begins with the same gate: `MOB_OR_IMPL` (an NPC with no
//! descriptor or one an implementor drives, or anything with triggers
//! attached) else the `CONFIG_HUH` reply; most then refuse a charmed
//! character. `mzoneecho` and `mrecho` skip the charm test, as in the C. The
//! commands that make the world change (`mload`, `mpurge`, `mforce`,
//! `mhunt`, `mremember`, `mforget`) also refuse a switched player below
//! `LVL_IMPL`.

use super::comm::{TO_CHAR, TO_ROOM, sub_write, sub_write_in};
use super::misc::script_damage;
use super::shared::{
    Owner, door, find_target_room, load, recho, starts_with_digit, teleport, zoneecho,
};
use crate::cstr::{atoi, one_argument, skip_spaces, str_eq, two_arguments};
use crate::host::DgHost;
use crate::lookup::{
    NUM_WEARS, Where, get_char, get_char_room_vis, get_char_vis, get_obj, get_obj_in_list_vis,
    get_obj_pos_in_equip_vis, get_obj_vis, isname,
};
use crate::types::{LVL_IMPL, ScriptMemory, UID_CHAR, Uid};

/// One mob script command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobCommand {
    Masound,
    Mkill,
    Mjunk,
    Mdamage,
    Mdoor,
    Mecho,
    Mrecho,
    Mechoaround,
    Msend,
    Mload,
    Mpurge,
    Mgoto,
    Mat,
    Mteleport,
    Mforce,
    Mhunt,
    Mremember,
    Mforget,
    Mtransform,
    Mzoneecho,
    Mfollow,
    Mlog,
}

/// `mob_script_commands[]` (`interpreter.c`), in table order.
pub const MOB_SCRIPT_COMMANDS: &[(&str, MobCommand)] = &[
    ("masound", MobCommand::Masound),
    ("mkill", MobCommand::Mkill),
    ("mjunk", MobCommand::Mjunk),
    ("mdamage", MobCommand::Mdamage),
    ("mdoor", MobCommand::Mdoor),
    ("mecho", MobCommand::Mecho),
    ("mrecho", MobCommand::Mrecho),
    ("mechoaround", MobCommand::Mechoaround),
    ("msend", MobCommand::Msend),
    ("mload", MobCommand::Mload),
    ("mpurge", MobCommand::Mpurge),
    ("mgoto", MobCommand::Mgoto),
    ("mat", MobCommand::Mat),
    ("mteleport", MobCommand::Mteleport),
    ("mforce", MobCommand::Mforce),
    ("mhunt", MobCommand::Mhunt),
    ("mremember", MobCommand::Mremember),
    ("mforget", MobCommand::Mforget),
    ("mtransform", MobCommand::Mtransform),
    ("mzoneecho", MobCommand::Mzoneecho),
    ("mfollow", MobCommand::Mfollow),
    ("mlog", MobCommand::Mlog),
];

/// Run `command` for `ch` with `argument` (the rest of the line after the
/// command word, leading space and all).
pub fn run<H: DgHost + ?Sized>(host: &mut H, ch: Uid, command: MobCommand, argument: &str) {
    match command {
        MobCommand::Masound => do_masound(host, ch, argument),
        MobCommand::Mkill => do_mkill(host, ch, argument),
        MobCommand::Mjunk => do_mjunk(host, ch, argument),
        MobCommand::Mdamage => do_mdamage(host, ch, argument),
        MobCommand::Mdoor => do_mdoor(host, ch, argument),
        MobCommand::Mecho => do_mecho(host, ch, argument),
        MobCommand::Mrecho => do_mrecho(host, ch, argument),
        MobCommand::Mechoaround => do_msend(host, ch, argument, true),
        MobCommand::Msend => do_msend(host, ch, argument, false),
        MobCommand::Mload => do_mload(host, ch, argument),
        MobCommand::Mpurge => do_mpurge(host, ch, argument),
        MobCommand::Mgoto => do_mgoto(host, ch, argument),
        MobCommand::Mat => do_mat(host, ch, argument),
        MobCommand::Mteleport => do_mteleport(host, ch, argument),
        MobCommand::Mforce => do_mforce(host, ch, argument),
        MobCommand::Mhunt => do_mhunt(host, ch, argument),
        MobCommand::Mremember => do_mremember(host, ch, argument),
        MobCommand::Mforget => do_mforget(host, ch, argument),
        MobCommand::Mtransform => do_mtransform(host, ch, argument),
        MobCommand::Mzoneecho => do_mzoneecho(host, ch, argument),
        MobCommand::Mfollow => do_mfollow(host, ch, argument),
        MobCommand::Mlog => do_mlog(host, ch, argument),
    }
}

/// `ch->desc && GET_LEVEL(ch->desc->original) < LVL_IMPL`.
fn desc_below_impl<H: DgHost + ?Sized>(host: &H, ch: Uid) -> bool {
    host.cmd_desc_original_level(ch)
        .is_some_and(|level| level < LVL_IMPL)
}

/// `MOB_OR_IMPL(ch)`.
fn mob_or_impl<H: DgHost + ?Sized>(host: &H, ch: Uid) -> bool {
    let npc_ok = host.is_npc(ch) && !desc_below_impl(host, ch);
    let has_triggers = host
        .script(ch)
        .is_some_and(|script| !script.triggers.is_empty());
    npc_ok || has_triggers
}

/// The start of every command: `MOB_OR_IMPL` or the "Huh?" reply, then
/// (when `charm_check`) the refusal of a charmed character. `true` when the
/// command may go on.
fn gate<H: DgHost + ?Sized>(host: &mut H, ch: Uid, charm_check: bool) -> bool {
    if !mob_or_impl(host, ch) {
        let huh = host.cmd_huh();
        host.cmd_send(ch, &huh);
        return false;
    }
    !(charm_check && host.cmd_is_charmed(ch))
}

/// `if (*arg == UID_CHAR) get_char(arg) else get_char_room_vis(ch, arg)`.
fn victim_in_room<H: DgHost + ?Sized>(host: &H, ch: Uid, arg: &str) -> Option<Uid> {
    if arg.starts_with(UID_CHAR) {
        get_char(host, arg)
    } else {
        get_char_room_vis(host, ch, arg)
    }
}

/// `if (*arg == UID_CHAR) get_char(arg) else get_char_vis(ch, arg, NULL, FIND_CHAR_WORLD)`.
fn victim_in_world<H: DgHost + ?Sized>(host: &H, ch: Uid, arg: &str) -> Option<Uid> {
    if arg.starts_with(UID_CHAR) {
        get_char(host, arg)
    } else {
        get_char_vis(host, ch, arg, Where::World)
    }
}

/// Prints the argument to all the rooms around the mobile.
fn do_masound<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if argument.is_empty() {
        Owner::Mob(ch).log(host, "masound called with no argument");
        return;
    }
    let argument = skip_spaces(argument);

    // A mob in no room has no exits (the C indexes `world[NOWHERE]`).
    let Some(was_in_room) = host.in_room(ch) else {
        return;
    };
    for door in 0..host.cmd_dir_count() {
        if let Some(to) = host.cmd_exit_to_room(was_in_room, door)
            && to != was_in_room
        {
            sub_write_in(host, argument, ch, Some(to), true, TO_ROOM);
        }
    }
}

/// Lets the mobile kill any player or mobile.
fn do_mkill<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    let (arg, _) = one_argument(argument);
    let log = Owner::Mob(ch);

    if arg.is_empty() {
        log.log(host, "mkill called with no argument");
        return;
    }
    let Some(victim) = victim_in_room(host, ch, &arg) else {
        log.log(host, &format!("mkill: victim ({arg}) not found"));
        return;
    };
    if victim == ch {
        log.log(host, "mkill: victim is self");
        return;
    }
    if !host.is_npc(victim) && host.cmd_nohassle(victim) {
        log.log(host, "mkill: target has nohassle on");
        return;
    }
    if host.cmd_fighting(ch).is_some() {
        log.log(host, "mkill: already fighting");
        return;
    }
    host.cmd_hit_victim(ch, victim);
}

/// `find_all_dots(arg)` as `do_mjunk` sees it, including its damage to `arg`.
///
/// The C rewrites `arg` in place for `all.name` (the buffer then holds
/// `name\0` followed by the tail of the old text), and `do_mjunk` goes on to
/// read `arg[3]` and `arg+4` of that buffer. This returns the word as the
/// functions after it see it, `arg[3] == '\0'`, and the string at `arg+4`.
///
/// Reads past the end of the buffer's text (a word shorter than four bytes)
/// are indeterminate in the C; here they read as a non-NUL byte and an empty
/// string, so a short plain name junks nothing.
struct JunkArg {
    /// `arg` after `find_all_dots`.
    word: String,
    /// `find_all_dots(arg) != FIND_INDIV`.
    dotted: bool,
    /// `arg[3] == '\0'`.
    third_is_nul: bool,
    /// The text at `arg + 4`.
    tail: String,
}

fn junk_arg(arg: &str) -> JunkArg {
    let bytes = arg.as_bytes();
    let dotted = arg == "all" || arg.starts_with("all.");
    // The buffer: the text and its NUL, then (after `all.`) rewritten.
    let mut buf: Vec<u8> = bytes.to_vec();
    buf.push(0);
    if arg.starts_with("all.") {
        let rest = buf[4..].to_vec();
        for (i, b) in rest.iter().enumerate() {
            buf[i] = *b;
        }
    }
    let text_end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let word = String::from_utf8_lossy(&buf[..text_end]).into_owned();
    let third_is_nul = buf.get(3).is_some_and(|b| *b == 0);
    let tail = match buf.get(4..) {
        Some(rest) => {
            let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
            String::from_utf8_lossy(&rest[..end]).into_owned()
        }
        None => String::new(),
    };
    JunkArg {
        word,
        dotted,
        third_is_nul,
        tail,
    }
}

/// Lets the mobile destroy an object in its inventory; it can also destroy
/// a worn object, and items using `all.xxxxx` or just plain `all`.
fn do_mjunk<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        Owner::Mob(ch).log(host, "mjunk called with no argument");
        return;
    }

    let junk_all = str_eq(&arg, "all");
    let junk = junk_arg(&arg);

    if junk.dotted && !junk_all {
        if let Some(pos) = get_obj_pos_in_equip_vis(host, ch, ch, &junk.word)
            && let Some(obj) = host.cmd_unequip_char(ch, pos)
        {
            host.cmd_extract_obj(obj);
            return;
        }
        if let Some(obj) = get_obj_in_list_vis(host, ch, &junk.word, &host.carrying(ch)) {
            host.cmd_extract_obj(obj);
        }
        return;
    }

    for obj in host.carrying(ch) {
        if junk.third_is_nul || isname(&junk.tail, &host.keywords(obj)) {
            host.cmd_extract_obj(obj);
        }
    }
    // Each pass takes one worn object off, so NUM_WEARS passes are enough.
    for _ in 0..NUM_WEARS {
        let Some(pos) = get_obj_pos_in_equip_vis(host, ch, ch, &junk.word) else {
            break;
        };
        match host.cmd_unequip_char(ch, pos) {
            Some(obj) => host.cmd_extract_obj(obj),
            None => break,
        }
    }
}

/// Prints the message to everyone in the room other than the mob and victim
/// (`mechoaround`), or sends it to the victim only (`msend`).
fn do_msend<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str, around: bool) {
    if !gate(host, ch, true) {
        return;
    }
    let (arg, rest) = one_argument(argument);
    let p = skip_spaces(rest);
    let name = if around { "mechoaround" } else { "msend" };

    if arg.is_empty() {
        Owner::Mob(ch).log(host, &format!("{name} called with no argument"));
        return;
    }
    let Some(victim) = victim_in_room(host, ch, &arg) else {
        Owner::Mob(ch).log(host, &format!("{name}: victim ({arg}) does not exist"));
        return;
    };
    sub_write(
        host,
        p,
        victim,
        true,
        if around { TO_ROOM } else { TO_CHAR },
    );
}

/// Prints the message to the room at large.
fn do_mecho<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if argument.is_empty() {
        Owner::Mob(ch).log(host, "mecho called with no arguments");
        return;
    }
    let p = skip_spaces(argument);
    sub_write(host, p, ch, true, TO_ROOM);
    sub_write(host, p, ch, true, TO_CHAR);
}

fn do_mlog<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if argument.is_empty() {
        return;
    }
    let p = skip_spaces(argument);
    Owner::Mob(ch).log(host, p);
}

fn do_mzoneecho<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, false) {
        return;
    }
    zoneecho(host, Owner::Mob(ch), argument);
}

/// Prints the message to everyone in the range of numbers.
fn do_mrecho<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, false) {
        return;
    }
    recho(host, Owner::Mob(ch), argument);
}

/// Lets the mobile load an item or mobile. All items are loaded into
/// inventory, unless it is NO-TAKE.
fn do_mload<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if desc_below_impl(host, ch) {
        return;
    }
    load(host, Owner::Mob(ch), argument);
}

/// Lets the mobile purge all objects and other NPCs in the room, or purge a
/// specified object or mob in the room. It can purge itself, but this will
/// be the last command it does.
fn do_mpurge<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if desc_below_impl(host, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        // `world[NOWHERE]` for a mob in no room: nothing to purge.
        let Some(room) = host.in_room(ch) else {
            return;
        };
        for victim in host.room_people(room) {
            if host.is_npc(victim) && victim != ch {
                host.cmd_extract_char(victim);
            }
        }
        for obj in host.room_contents(room) {
            if host.exists(obj) {
                host.cmd_extract_obj(obj);
            }
        }
        return;
    }

    let victim = if arg.starts_with(UID_CHAR) {
        get_char(host, &arg)
    } else {
        get_char_room_vis(host, ch, &arg)
    };
    let Some(victim) = victim else {
        let obj = if arg.starts_with(UID_CHAR) {
            get_obj(host, &arg)
        } else {
            get_obj_vis(host, ch, &arg)
        };
        match obj {
            Some(obj) => host.cmd_extract_obj(obj),
            None => Owner::Mob(ch).log(host, "mpurge: bad argument"),
        }
        return;
    };

    if !host.is_npc(victim) {
        Owner::Mob(ch).log(host, "mpurge: purging a PC");
        return;
    }
    if victim == ch {
        *host.owner_purged_mut() = true;
    }
    host.cmd_extract_char(victim);
}

/// Lets the mobile go to any location it wishes that is not private.
fn do_mgoto<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        Owner::Mob(ch).log(host, "mgoto called with no argument");
        return;
    }
    let Some(location) = find_target_room(host, ch, &arg) else {
        Owner::Mob(ch).log(host, "mgoto: invalid location");
        return;
    };

    if host.cmd_fighting(ch).is_some() {
        host.cmd_stop_fighting(ch);
    }
    host.cmd_char_from_room(ch);
    host.cmd_char_to_room(ch, location);
    crate::dispatch::enter_wtrigger(host, location, ch, -1);
}

/// Lets the mobile do a command at another location.
fn do_mat<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    let (arg, command) = one_argument(argument);

    if arg.is_empty() || command.is_empty() {
        Owner::Mob(ch).log(host, "mat: bad argument");
        return;
    }
    let Some(location) = find_target_room(host, ch, &arg) else {
        Owner::Mob(ch).log(host, "mat: invalid location");
        return;
    };

    let original = host.in_room(ch);
    host.cmd_char_from_room(ch);
    host.cmd_char_to_room(ch, location);
    host.mud_command(ch, command);

    // See if `ch` is still there before moving it back: `at XXXX quit`.
    if host.in_room(ch) == Some(location) {
        host.cmd_char_from_room(ch);
        match original {
            Some(room) => host.cmd_char_to_room(ch, room),
            None => host.cmd_syslog("SYSERR: Illegal value(s) passed to char_to_room"),
        }
    }
}

/// Lets the mobile transfer people. The `all` argument transfers everyone in
/// the current room to the specified location.
fn do_mteleport<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    let (arg1, arg2, _) = two_arguments(argument);
    let log = Owner::Mob(ch);

    if arg1.is_empty() || arg2.is_empty() {
        log.log(host, "mteleport: bad syntax");
        return;
    }
    let Some(target) = find_target_room(host, ch, &arg2) else {
        log.log(host, "mteleport target is an invalid room");
        return;
    };

    if str_eq(&arg1, "all") {
        let Some(here) = host.in_room(ch) else {
            return;
        };
        if target == here {
            log.log(host, "mteleport all target is itself");
            return;
        }
        for vict in host.room_people(here) {
            if host.exists(vict) && host.valid_dg_target(vict, true) {
                teleport(host, vict, target);
            }
        }
    } else {
        let Some(vict) = victim_in_world(host, ch, &arg1) else {
            log.log(host, &format!("mteleport: victim ({arg1}) does not exist"));
            return;
        };
        if host.valid_dg_target(vict, true) {
            teleport(host, vict, target);
        }
    }
}

fn do_mdamage<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    let (name, amount, _) = two_arguments(argument);

    // Whether the amount is a number is not checked; if not it is 0.
    if name.is_empty() || amount.is_empty() {
        Owner::Mob(ch).log(host, "mdamage: bad syntax");
        return;
    }
    let dam = atoi(&amount);
    let Some(vict) = victim_in_room(host, ch, &name) else {
        Owner::Mob(ch).log(host, &format!("mdamage: victim ({name}) does not exist"));
        return;
    };
    script_damage(host, vict, dam);
}

/// Lets the mobile force someone to do something. Must be mortal level, and
/// the `all` argument only affects those in the room with the mobile.
fn do_mforce<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if desc_below_impl(host, ch) {
        return;
    }
    let (arg, line) = one_argument(argument);
    let log = Owner::Mob(ch);

    if arg.is_empty() || line.is_empty() {
        log.log(host, "mforce: bad syntax");
        return;
    }

    if str_eq(&arg, "all") {
        for vch in host.cmd_playing_characters() {
            if vch != ch
                && host.in_room(vch) == host.in_room(ch)
                && host.level(vch) < host.level(ch)
                && host.can_see(ch, vch)
                && host.valid_dg_target(vch, false)
            {
                host.mud_command(vch, line);
            }
        }
    } else {
        let victim = if arg.starts_with(UID_CHAR) {
            let found = get_char(host, &arg);
            if found.is_none() {
                log.log(host, &format!("mforce: victim ({arg}) does not exist"));
                return;
            }
            found
        } else {
            let found = get_char_room_vis(host, ch, &arg);
            if found.is_none() {
                log.log(host, "mforce: no such victim");
                return;
            }
            found
        };
        let Some(victim) = victim else {
            return;
        };
        if victim == ch {
            log.log(host, "mforce: forcing self");
            return;
        }
        if host.valid_dg_target(victim, false) {
            host.mud_command(victim, line);
        }
    }
}

/// Hunt for someone.
fn do_mhunt<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if desc_below_impl(host, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        Owner::Mob(ch).log(host, "mhunt called with no argument");
        return;
    }
    if host.cmd_fighting(ch).is_some() {
        return;
    }
    let Some(victim) = victim_in_world(host, ch, &arg) else {
        Owner::Mob(ch).log(host, &format!("mhunt: victim ({arg}) does not exist"));
        return;
    };
    host.cmd_set_hunting(ch, victim);
}

/// Place someone into the mob's memory list.
fn do_mremember<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if desc_below_impl(host, ch) {
        return;
    }
    let (arg, argument) = one_argument(argument);

    if arg.is_empty() {
        Owner::Mob(ch).log(host, "mremember: bad syntax");
        return;
    }
    let Some(victim) = victim_in_world(host, ch, &arg) else {
        Owner::Mob(ch).log(host, &format!("mremember: victim ({arg}) does not exist"));
        return;
    };

    // The new entry goes on the end of the list. The command is kept exactly
    // as left after the name, leading space included.
    let mut memory = host.script_mem(ch);
    memory.push(ScriptMemory {
        id: victim,
        cmd: (!argument.is_empty()).then(|| argument.to_owned()),
    });
    host.set_script_mem(ch, memory);
}

/// Remove someone from the list.
fn do_mforget<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if desc_below_impl(host, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        Owner::Mob(ch).log(host, "mforget: bad syntax");
        return;
    }
    let Some(victim) = victim_in_world(host, ch, &arg) else {
        Owner::Mob(ch).log(host, &format!("mforget: victim ({arg}) does not exist"));
        return;
    };

    let mut memory = host.script_mem(ch);
    memory.retain(|entry| entry.id != victim);
    host.set_script_mem(ch, memory);
}

/// Transform into a different mobile.
fn do_mtransform<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    if host.cmd_has_desc(ch) {
        host.cmd_send(
            ch,
            "You've got no VNUM to return to, dummy! try 'switch'\r\n",
        );
        return;
    }
    let (arg, _) = one_argument(argument);
    let log = Owner::Mob(ch);

    if arg.is_empty() {
        log.log(host, "mtransform: missing argument");
    } else if !starts_with_digit(&arg) && !arg.starts_with('-') {
        log.log(host, "mtransform: bad argument");
    } else {
        // A leading `-` keeps the old hit points out of the new mob.
        let (vnum, keep_hp) = if starts_with_digit(&arg) {
            (atoi(&arg), true)
        } else {
            (atoi(&arg[1..]), false)
        };
        if !host.cmd_transform_mob(ch, vnum, keep_hp) {
            log.log(host, "mtransform: bad mobile vnum");
        }
    }
}

fn do_mdoor<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    door(host, Owner::Mob(ch), argument);
}

fn do_mfollow<H: DgHost + ?Sized>(host: &mut H, ch: Uid, argument: &str) {
    if !gate(host, ch, true) {
        return;
    }
    let (buf, _) = one_argument(argument);
    let log = Owner::Mob(ch);

    if buf.is_empty() {
        log.log(host, "mfollow: bad syntax");
        return;
    }
    let leader = if buf.starts_with(UID_CHAR) {
        let found = get_char(host, &buf);
        if found.is_none() {
            log.log(host, &format!("mfollow: victim ({buf}) does not exist"));
            return;
        }
        found
    } else {
        let found = get_char_vis(host, ch, &buf, Where::Room);
        if found.is_none() {
            log.log(host, &format!("mfollow: victim ({buf}) not found"));
            return;
        }
        found
    };
    let Some(leader) = leader else {
        return;
    };

    if host.cmd_master(ch) == Some(leader) {
        // Already following.
        return;
    }
    // (The C then refuses to override a charm; a charmed `ch` has already
    // left at the gate.)

    // Stop following someone else first.
    if host.cmd_master(ch).is_some() {
        host.cmd_unfollow(ch);
    }
    if ch == leader {
        return;
    }
    if circle_follow(host, ch, leader) {
        log.log(host, "mfollow: Following in circles.");
        return;
    }
    host.cmd_follow(ch, leader);
}

/// `circle_follow(ch, victim)`: `ch` is somewhere up `victim`'s chain of
/// leaders.
fn circle_follow<H: DgHost + ?Sized>(host: &H, ch: Uid, victim: Uid) -> bool {
    let mut k = Some(victim);
    // The bound only guards against a corrupt cycle in the chain.
    for _ in 0..host.character_list().len().saturating_add(1) {
        match k {
            Some(i) if i == ch => return true,
            Some(i) => k = host.cmd_master(i),
            None => return false,
        }
    }
    false
}
