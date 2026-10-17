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

use super::comm::{TO_CHAR, TO_ROOM, for_each_occupant, sub_write, sub_write_in};
use super::misc::script_damage;
use super::shared::{
    Owner, PRF_NOHASSLE, dir_count, door, exit_to_room, find_target_room, has_desc, load, recho,
    shown_after_char, starts_with_digit, teleport, zoneecho,
};
use crate::cstr::{atoi, one_argument, skip_spaces, str_eq, two_arguments};
use crate::host::DgHost;
use crate::lookup::{
    NUM_WEARS, Where, get_char, get_char_room_vis, get_char_vis, get_char_world_vis, get_number,
    get_obj, get_obj_in_list_vis, get_obj_pos_in_equip_vis, get_obj_vis, idx_vnum, isname,
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
    // A descriptor that is not switched has no `original` (the C
    // dereferences NULL): the character's own level stands in, so an
    // ordinary player is refused and an implementor is not.
    host.cmd_desc(ch)
        .is_some_and(|original| host.level(original.unwrap_or(ch)) < LVL_IMPL)
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
        get_char_world_vis(host, ch, arg)
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
    for door in 0..dir_count(host) {
        if let Some(to) = exit_to_room(host, was_in_room, door)
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
        log.log(
            host,
            &format!(
                "mkill: victim ({}) not found",
                shown_after_char(&arg, Where::Room)
            ),
        );
        return;
    };
    if victim == ch {
        log.log(host, "mkill: victim is self");
        return;
    }
    if !host.is_npc(victim) && host.char_pref_flagged(victim, PRF_NOHASSLE) {
        log.log(host, "mkill: target has nohassle on");
        return;
    }
    if host.char_fighting(ch).is_some() {
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
        // The equipment lookup has stripped any `N.` from the buffer, whatever
        // its first byte (`do_mjunk` has no UID branch).
        let bare = get_number(&junk.word).1.to_owned();
        if let Some(obj) = get_obj_in_list_vis(host, ch, &bare, &host.carrying(ch)) {
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
    // Each lookup strips one `N.` from the buffer the next one sees.
    let mut word = junk.word.clone();
    for _ in 0..NUM_WEARS {
        let found = get_obj_pos_in_equip_vis(host, ch, ch, &word);
        word = get_number(&word).1.to_owned();
        let Some(pos) = found else {
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
        Owner::Mob(ch).log(
            host,
            &format!(
                "{name}: victim ({}) does not exist",
                shown_after_char(&arg, Where::Room)
            ),
        );
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
            // `get_char_room_vis` has already stripped any `N.` from `arg`.
            get_obj_vis(host, ch, &shown_after_char(&arg, Where::Room))
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
        host.vm_state().owner_purged = true;
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

    if host.char_fighting(ch).is_some() {
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
        for_each_occupant(host, here, |host, vict| {
            if host.valid_dg_target(vict, true) {
                teleport(host, vict, target);
            }
        });
    } else {
        let Some(vict) = victim_in_world(host, ch, &arg1) else {
            log.log(
                host,
                &format!(
                    "mteleport: victim ({}) does not exist",
                    shown_after_char(&arg1, Where::World)
                ),
            );
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
        Owner::Mob(ch).log(
            host,
            &format!(
                "mdamage: victim ({}) does not exist",
                shown_after_char(&name, Where::Room)
            ),
        );
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
    if host.char_fighting(ch).is_some() {
        return;
    }
    let Some(victim) = victim_in_world(host, ch, &arg) else {
        Owner::Mob(ch).log(
            host,
            &format!(
                "mhunt: victim ({}) does not exist",
                shown_after_char(&arg, Where::World)
            ),
        );
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
        Owner::Mob(ch).log(
            host,
            &format!(
                "mremember: victim ({}) does not exist",
                shown_after_char(&arg, Where::World)
            ),
        );
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
        Owner::Mob(ch).log(
            host,
            &format!(
                "mforget: victim ({}) does not exist",
                shown_after_char(&arg, Where::World)
            ),
        );
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
    if has_desc(host, ch) {
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
        let (vnum, keep_hp) = match arg.strip_prefix('-') {
            Some(digits) => (idx_vnum(atoi(digits)), false),
            None => (idx_vnum(atoi(&arg)), true),
        };
        let Some(new_mob) = host.cmd_read_mobile(vnum) else {
            log.log(host, "mtransform: bad mobile vnum");
            return;
        };
        // Take the equipment off, put the new mob in the same room so the
        // extract works, copy it over `ch`, then dress `ch` again.
        let mut worn = Vec::new();
        for pos in 0..NUM_WEARS {
            if host.equipment(ch, pos).is_some()
                && let Some(obj) = host.cmd_unequip_char(ch, pos)
            {
                worn.push((pos, obj));
            }
        }
        if let Some(room) = host.in_room(ch) {
            host.cmd_char_to_room(new_mob, room);
        }
        host.cmd_overwrite_mob(ch, new_mob, keep_hp);
        for (pos, obj) in worn {
            host.cmd_equip_char(ch, obj, pos);
        }
        host.cmd_extract_char(new_mob);
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
            log.log(
                host,
                &format!(
                    "mfollow: victim ({}) not found",
                    shown_after_char(&buf, Where::Room)
                ),
            );
            return;
        }
        found
    };
    let Some(leader) = leader else {
        return;
    };

    if host.char_master(ch) == Some(leader) {
        // Already following.
        return;
    }
    // (The C then refuses to override a charm; a charmed `ch` has already
    // left at the gate.)

    // Stop following someone else first.
    if host.char_master(ch).is_some() {
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
            Some(i) => k = host.char_master(i),
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::commands::CmdHost;
    use crate::commands::support::World;
    use crate::host::{BaseHost, WorldHost};
    use crate::mock::MockAction;
    use crate::types::{AttachType, uid_text};

    const PREFIX: &str = "Mob (the guard, VNum 3000):: ";

    fn run(w: &mut World, line: &str) {
        let mob = w.mob;
        assert!(w.run_mob(mob, line));
    }

    fn log_is(w: &World, text: &str) {
        assert_eq!(w.log(), format!("{PREFIX}{text}"));
    }

    /// A mob with no script, as `MOB_OR_IMPL` sees an ordinary NPC.
    fn bare_mob(w: &mut World) -> Uid {
        let mob = w.host.add_mob(3001, "a rat");
        w.host.char_to_room(mob, w.room);
        mob
    }

    // ---- the gate -------------------------------------------------------

    #[test]
    fn a_player_without_triggers_gets_huh() {
        let mut w = World::new();
        let alice = w.player;
        assert!(w.run_mob(alice, "mecho hi"));
        assert_eq!(w.sent_to(alice), ["Huh?!?\r\n"]);
        assert_eq!(w.host.effects.len(), 1);
        // The reply is CONFIG_HUH as the host has it.
        w.clear();
        w.host
            .globals
            .texts
            .insert("huh".to_owned(), "What?\r\n".to_owned());
        assert!(w.run_mob(alice, "mkill bob"));
        assert_eq!(w.sent_to(alice), ["What?\r\n"]);
    }

    #[test]
    fn a_player_with_triggers_passes_the_gate_and_logs_as_null() {
        let mut w = World::new();
        let (alice, proto) = (w.player, w.proto.clone());
        w.host.attach(alice, &proto);
        assert!(w.run_mob(alice, "mlog hi"));
        assert_eq!(w.log(), "Mob ((null), VNum 65535):: hi");
    }

    #[test]
    fn an_unswitched_descriptor_is_judged_by_the_characters_own_level() {
        // `ch->desc->original` is NULL in the C; the character's own level
        // stands in.
        let mut w = World::new();
        let mob = bare_mob(&mut w);
        w.host.set_num(mob, "desc", 1);
        w.host.set_num(mob, "level", 33);
        assert!(w.run_mob(mob, "mecho hi"));
        assert_eq!(w.sent_to(mob), ["Huh?!?\r\n"]);
        w.clear();
        w.host.set_num(mob, "level", 34);
        assert!(w.run_mob(mob, "mecho hi"));
        assert_eq!(w.sent_to(w.player), ["hi\n\r"]);
    }

    #[test]
    fn a_switched_npc_needs_an_implementor_behind_it() {
        let mut w = World::new();
        let mob = bare_mob(&mut w);
        w.host.set_num(mob, "desc", 1);
        w.switch_behind(mob, 33);
        assert!(w.run_mob(mob, "mecho hi"));
        assert_eq!(w.sent_to(mob), ["Huh?!?\r\n"]);
        w.clear();
        w.switch_behind(mob, 34);
        assert!(w.run_mob(mob, "mecho hi"));
        assert_eq!(w.sent_to(w.player), ["hi\n\r"]);
        // With no descriptor at all an NPC is fine.
        let plain = bare_mob(&mut w);
        w.clear();
        assert!(w.run_mob(plain, "mecho yo"));
        assert_eq!(w.sent_to(w.player), ["yo\n\r"]);
    }

    #[test]
    fn a_charmed_mob_does_nothing_except_zoneecho_and_recho() {
        let mut w = World::new();
        let mob = w.mob;
        w.host.set_num(mob, "aff:22", 1);
        w.host.globals.nums.insert("zone:0:bot".to_owned(), 3000);
        w.host.globals.nums.insert("zone:0:top".to_owned(), 3099);
        for line in [
            "masound x",
            "mkill alice",
            "mjunk all",
            "mdamage alice 5",
            "mdoor 3001 north purge",
            "mecho x",
            "mechoaround alice x",
            "msend alice x",
            "mload mob 3100",
            "mpurge",
            "mgoto 3002",
            "mat 3002 look",
            "mteleport alice 3002",
            "mforce alice look",
            "mhunt alice",
            "mremember alice",
            "mforget alice",
            "mtransform 3100",
            "mfollow alice",
            "mlog x",
        ] {
            run(&mut w, line);
        }
        assert!(w.host.effects.is_empty(), "{:?}", w.host.effects);
        assert!(w.host.logs.is_empty());
        assert_eq!(w.host.num(w.player, "hit"), 20);
        // Zone and range echoes skip the charm test.
        run(&mut w, "mzoneecho 3001 loud");
        run(&mut w, "mrecho 3001 3001 louder");
        assert_eq!(w.sent_to(w.player), ["loud\r\n", "louder\r\n"]);
    }

    // ---- echoes ---------------------------------------------------------

    #[test]
    fn mecho_goes_to_the_whole_room() {
        let mut w = World::new();
        run(&mut w, "mecho   Hello ~alice");
        assert_eq!(w.sent_to(w.player), ["Hello you\n\r"]);
        assert_eq!(w.sent_to(w.player2), ["Hello Alice\n\r"]);
        run(&mut w, "mecho");
        log_is(&w, "mecho called with no arguments");
        // A line of nothing but spaces is not empty: it sends a blank line.
        w.clear();
        run(&mut w, "mecho    ");
        assert_eq!(w.sent_to(w.player), ["\n\r"]);
    }

    #[test]
    fn mechoaround_skips_the_victim_and_msend_reaches_only_the_victim() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        run(&mut w, "mechoaround alice  ~alice sneezes");
        assert_eq!(w.sent_to(bob), ["Alice sneezes\n\r"]);
        assert!(w.sent_to(alice).is_empty());
        w.clear();
        run(&mut w, "msend alice  Psst ~alice");
        assert_eq!(w.sent_to(alice), ["Psst you\n\r"]);
        assert!(w.sent_to(bob).is_empty());
        // By id.
        w.clear();
        run(&mut w, &format!("msend {} by id", uid_text(bob)));
        assert_eq!(w.sent_to(bob), ["by id\n\r"]);
    }

    #[test]
    fn mechoaround_and_msend_report_their_errors() {
        let mut w = World::new();
        run(&mut w, "mechoaround");
        log_is(&w, "mechoaround called with no argument");
        run(&mut w, "mechoaround zed hi");
        log_is(&w, "mechoaround: victim (zed) does not exist");
        run(&mut w, "msend");
        log_is(&w, "msend called with no argument");
        run(&mut w, "msend ZED hi");
        log_is(&w, "msend: victim (zed) does not exist");
        run(&mut w, "msend }999 hi");
        log_is(&w, "msend: victim (}999) does not exist");
        // No message is allowed: a blank line is sent.
        w.clear();
        run(&mut w, "msend alice");
        assert_eq!(w.sent_to(w.player), ["\n\r"]);
    }

    #[test]
    fn mlog_logs_only_when_there_is_something_to_say() {
        let mut w = World::new();
        run(&mut w, "mlog");
        assert!(w.host.logs.is_empty());
        run(&mut w, "mlog    spaced   out ");
        log_is(&w, "spaced   out ");
        // A % in a name or message is data.
        run(&mut w, "mlog 100%s %d");
        log_is(&w, "100%s %d");
        w.host.set_text(w.mob, "name", "100% guard");
        run(&mut w, "mlog x");
        assert_eq!(w.log(), "Mob (100% guard, VNum 3000):: x");
    }

    #[test]
    fn masound_speaks_into_the_neighbouring_rooms() {
        let mut w = World::new();
        let (alice, bob, room, room2) = (w.player, w.player2, w.room, w.room2);
        w.host.char_to_room(bob, room2);
        run(&mut w, "masound The wind howls ~bob");
        // Bob is in the next room and the names resolve there.
        assert_eq!(w.sent_to(bob), ["The wind howls you\n\r"]);
        assert!(w.sent_to(alice).is_empty());
        // An exit back into the same room is skipped, and so is one with no
        // destination; directions past DIR_COUNT are not looked at.
        w.clear();
        w.host.set_link(room, "exit:1", Some(room));
        w.host.set_link(room, "exit:6", Some(room2));
        run(&mut w, "masound again");
        assert_eq!(w.sent_to(bob), ["again\n\r"]);
        // The self-loop exit does not speak into the mob's own room.
        assert!(w.sent_to(alice).is_empty());
        w.clear();
        w.host
            .globals
            .nums
            .insert("config:diagonal_dirs".to_owned(), 1);
        run(&mut w, "masound twice");
        assert_eq!(w.sent_to(bob), ["twice\n\r", "twice\n\r"]);
        run(&mut w, "masound");
        log_is(&w, "masound called with no argument");
        // Spaces only: a blank line goes out.
        w.clear();
        run(&mut w, "masound   ");
        assert_eq!(w.sent_to(bob), ["\n\r", "\n\r"]);
        // A mob in no room has nowhere to speak into.
        w.clear();
        w.host.char_from_room(w.mob);
        run(&mut w, "masound hush");
        assert!(w.host.effects.is_empty());
    }

    #[test]
    fn mzoneecho_finds_zones_numbered_above_63() {
        let mut w = World::new();
        let (alice, room) = (w.player, w.room);
        w.host.set_num(room, "zone", 120);
        w.host.globals.nums.insert("zone:120:bot".to_owned(), 3000);
        w.host.globals.nums.insert("zone:120:top".to_owned(), 3099);
        run(&mut w, "mzoneecho 3001 hello");
        assert_eq!(w.sent_to(alice), ["hello\r\n"]);
    }

    #[test]
    fn mzoneecho_and_mrecho() {
        let mut w = World::new();
        let (alice, bob, room, room2) = (w.player, w.player2, w.room, w.room2);
        w.host.set_num(room, "zone", 5);
        w.host.set_num(room2, "zone", 6);
        w.host.char_to_room(bob, room2);
        w.host.globals.nums.insert("zone:5:bot".to_owned(), 3000);
        w.host.globals.nums.insert("zone:5:top".to_owned(), 3099);
        // The vnum 3050 names zone 5 whatever room it is, and a room in zone 5
        // is where Alice is.
        run(&mut w, "mzoneecho 3050   Earthquake!");
        assert_eq!(w.sent_to(alice), ["Earthquake!\r\n"]);
        assert!(w.sent_to(bob).is_empty());
        run(&mut w, "mzoneecho 3050");
        log_is(&w, "mzoneecho called with too few args");
        run(&mut w, "mzoneecho");
        log_is(&w, "mzoneecho called with too few args");
        run(&mut w, "mzoneecho 9999 hello");
        log_is(&w, "mzoneecho called for nonexistant zone");
        // atoi: junk reads as 0, which is in no zone here.
        run(&mut w, "mzoneecho abc hello");
        log_is(&w, "mzoneecho called for nonexistant zone");

        w.clear();
        run(&mut w, "mrecho 3001 3002 Hear ye");
        assert_eq!(w.sent_to(alice), ["Hear ye\r\n"]);
        assert_eq!(w.sent_to(bob), ["Hear ye\r\n"]);
        for line in [
            "mrecho 3001 3002",
            "mrecho 3001 Hear",
            "mrecho a 3002 Hear",
            "mrecho 3001 b Hear",
            "mrecho",
        ] {
            run(&mut w, line);
            log_is(&w, "mrecho called with too few args");
        }
        // Negative numbers are numbers, but `send_to_range` takes `room_vnum`
        // (`ush_int`): -5 is 65531, so the range is inverted and refused.
        w.clear();
        run(&mut w, "mrecho -5 3001 neg");
        assert!(w.sent_to(alice).is_empty());
        assert_eq!(
            w.effects("syslog"),
            ["syslog: send_to_range passed start room value greater then finish."]
        );
        w.clear();
        // An inverted range is refused by send_to_range.
        run(&mut w, "mrecho 3002 3001 inverted");
        assert_eq!(
            w.effects("syslog"),
            ["syslog: send_to_range passed start room value greater then finish."]
        );
    }

    // ---- violence -------------------------------------------------------

    #[test]
    fn mkill_starts_a_fight() {
        let mut w = World::new();
        let (alice, mob) = (w.player, w.mob);
        run(&mut w, "mkill alice");
        assert_eq!(w.effects("hit"), [format!("hit {mob} {alice}")]);
        // Already fighting.
        run(&mut w, "mkill bob");
        log_is(&w, "mkill: already fighting");
        assert_eq!(w.effects("hit").len(), 1);
    }

    #[test]
    fn mkill_refusals() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        run(&mut w, "mkill");
        log_is(&w, "mkill called with no argument");
        run(&mut w, "mkill ZED");
        log_is(&w, "mkill: victim (zed) not found");
        run(&mut w, "mkill }999");
        log_is(&w, "mkill: victim (}999) not found");
        run(&mut w, "mkill me");
        log_is(&w, "mkill: victim is self");
        run(&mut w, "mkill guard");
        log_is(&w, "mkill: victim is self");
        w.set_nohassle(alice, true);
        run(&mut w, "mkill alice");
        log_is(&w, "mkill: target has nohassle on");
        assert!(w.effects("hit").is_empty());
        // By id, and nohassle only protects players.
        run(&mut w, &format!("mkill {}", uid_text(bob)));
        assert_eq!(w.effects("hit").len(), 1);
    }

    #[test]
    fn mkill_never_hits_a_nohassle_npc_check() {
        // PRF_NOHASSLE is read for players only; an NPC with the bit is hit.
        let mut w = World::new();
        let rat = bare_mob(&mut w);
        w.host.set_text(rat, "alias", "rat");
        w.set_nohassle(rat, true);
        run(&mut w, "mkill rat");
        assert_eq!(w.effects("hit").len(), 1);
    }

    #[test]
    fn mdamage_hurts_the_named_victim() {
        let mut w = World::new();
        let alice = w.player;
        run(&mut w, "mdamage alice 5");
        assert_eq!(w.host.num(alice, "hit"), 15);
        // The amount is atoi'd: junk is 0, a prefix is its number.
        run(&mut w, "mdamage alice abc");
        assert_eq!(w.host.num(alice, "hit"), 15);
        run(&mut w, "mdamage alice 2x");
        assert_eq!(w.host.num(alice, "hit"), 13);
        run(&mut w, "mdamage alice -3");
        assert_eq!(w.host.num(alice, "hit"), 16);
        run(&mut w, &format!("mdamage {} 1", uid_text(alice)));
        assert_eq!(w.host.num(alice, "hit"), 15);
        run(&mut w, "mdamage alice");
        log_is(&w, "mdamage: bad syntax");
        run(&mut w, "mdamage");
        log_is(&w, "mdamage: bad syntax");
        run(&mut w, "mdamage zed 3");
        log_is(&w, "mdamage: victim (zed) does not exist");
    }

    #[test]
    fn mhunt_sets_the_quarry_unless_fighting() {
        let mut w = World::new();
        let (alice, bob, room2, mob) = (w.player, w.player2, w.room2, w.mob);
        w.host.char_to_room(bob, room2);
        run(&mut w, "mhunt bob");
        assert_eq!(w.host.link(mob, "hunting"), Some(bob));
        run(&mut w, "mhunt");
        log_is(&w, "mhunt called with no argument");
        run(&mut w, "mhunt zed");
        log_is(&w, "mhunt: victim (zed) does not exist");
        run(&mut w, "mhunt }999");
        log_is(&w, "mhunt: victim (}999) does not exist");
        // A fighting mob ignores the command without even checking the name.
        w.clear();
        w.host.set_link(mob, "fighting", Some(alice));
        run(&mut w, "mhunt zed");
        run(&mut w, "mhunt alice");
        assert!(w.host.logs.is_empty());
        assert_eq!(w.effects("hunt").len(), 0);
    }

    // ---- inventory ------------------------------------------------------

    fn junk_world() -> (World, Uid, Uid, Uid) {
        let mut w = World::new();
        let mob = w.mob;
        let sword = w.host.add_obj(1, "a sword");
        w.host.set_text(sword, "alias", "sword");
        let dagger = w.host.add_obj(2, "a dagger");
        w.host.set_text(dagger, "alias", "dagger");
        let helmet = w.host.add_obj(3, "a helmet");
        w.host.set_text(helmet, "alias", "helmet");
        w.host.obj_to_char(sword, mob);
        w.host.obj_to_char(dagger, mob);
        w.host.equip_char(mob, helmet, 6);
        (w, sword, dagger, helmet)
    }

    #[test]
    fn mjunk_with_a_dotted_name_removes_one_item() {
        let (mut w, sword, dagger, helmet) = junk_world();
        run(&mut w, "mjunk");
        log_is(&w, "mjunk called with no argument");
        // Carried first.
        run(&mut w, "mjunk all.dagger");
        assert!(!w.host.exists(dagger));
        assert!(w.host.exists(sword));
        // Then worn.
        run(&mut w, "mjunk all.helmet");
        assert!(!w.host.exists(helmet));
        assert!(w.host.exists(sword));
        // Nothing by that name: nothing happens, and no log either.
        w.clear();
        run(&mut w, "mjunk all.zzz");
        run(&mut w, "mjunk all.");
        assert!(w.host.logs.is_empty());
        assert!(w.host.exists(sword));
    }

    #[test]
    fn mjunk_worn_item_is_preferred_to_a_carried_one() {
        let (mut w, sword, _, helmet) = junk_world();
        let mob = w.mob;
        let spare = w.host.add_obj(4, "a spare helmet");
        w.host.set_text(spare, "alias", "helmet");
        w.host.obj_to_char(spare, mob);
        run(&mut w, "mjunk all.helmet");
        assert!(!w.host.exists(helmet));
        assert!(w.host.exists(spare) && w.host.exists(sword));
    }

    #[test]
    fn mjunk_with_a_plain_name_reads_the_clobbered_buffer() {
        // The C tests `arg[3] == '\0' || isname(arg + 4, obj->name)` on a
        // plain name: a three-letter name junks everything carried, any
        // other name is compared from its fifth letter on.
        let (mut w, sword, dagger, helmet) = junk_world();
        // "sword": arg+4 is "d", which names the dagger and not the sword.
        run(&mut w, "mjunk sword");
        assert!(w.host.exists(sword));
        assert!(!w.host.exists(dagger));
        // Worn items are matched by the whole word: "sword" names none.
        assert!(w.host.exists(helmet));

        // "cup": the fourth byte is the terminator.
        let (mut w, sword, dagger, helmet) = junk_world();
        run(&mut w, "mjunk cup");
        assert!(!w.host.exists(sword) && !w.host.exists(dagger));
        assert!(w.host.exists(helmet));

        // "helm": nothing carried is named "" (its fifth byte onward), but the
        // worn helmet answers to "helm".
        let (mut w, sword, dagger, helmet) = junk_world();
        run(&mut w, "mjunk helm");
        assert!(w.host.exists(sword) && w.host.exists(dagger));
        assert!(!w.host.exists(helmet));

        // Shorter than three bytes the C reads past the terminator; nothing
        // is junked here.
        let (mut w, sword, dagger, helmet) = junk_world();
        run(&mut w, "mjunk ab");
        run(&mut w, "mjunk a");
        assert!(w.host.exists(sword) && w.host.exists(dagger) && w.host.exists(helmet));
    }

    #[test]
    fn mjunk_all_empties_the_inventory_but_not_the_worn_items() {
        let (mut w, sword, dagger, helmet) = junk_world();
        run(&mut w, "mjunk all");
        assert!(!w.host.exists(sword) && !w.host.exists(dagger));
        // The worn items are matched against the word "all" itself.
        assert!(w.host.exists(helmet));
        let all_hat = w.host.add_obj(5, "an all-weather hat");
        w.host.set_text(all_hat, "alias", "allweather hat");
        w.host.equip_char(w.mob, all_hat, 7);
        run(&mut w, "mjunk all");
        assert!(!w.host.exists(all_hat));
        assert!(w.host.exists(helmet));
    }

    #[test]
    fn mjunk_does_not_see_invisible_things() {
        let (mut w, _, dagger, _) = junk_world();
        let mob = w.mob;
        w.host
            .entity_mut(mob)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![dagger]);
        run(&mut w, "mjunk all.dagger");
        assert!(w.host.exists(dagger));
    }

    // ---- loading and purging --------------------------------------------

    #[test]
    fn mload_mob_puts_it_in_the_room_and_remembers_it() {
        let mut w = World::new();
        let (mob, room) = (w.mob, w.room);
        run(&mut w, "mload mob 3100");
        let rat = w
            .host
            .list(room, "people")
            .into_iter()
            .find(|c| w.host.vnum(*c) == 3100)
            .unwrap();
        let last = w
            .host
            .script(mob)
            .unwrap()
            .global_vars
            .get("lastloaded")
            .unwrap();
        assert_eq!(last.value, uid_text(rat));
        assert_eq!(last.context, 0);
        // Any abbreviation of "mob" will do.
        run(&mut w, "mload m 3100");
        run(&mut w, "mload MO 3100");
        assert_eq!(
            w.host
                .list(room, "people")
                .iter()
                .filter(|c| w.host.vnum(**c) == 3100)
                .count(),
            3
        );
    }

    #[test]
    fn mload_reports_bad_syntax_vnums_and_types() {
        let mut w = World::new();
        for line in [
            "mload",
            "mload mob",
            "mload mob abc",
            "mload mob -1",
            "mload mob 12abc",
            "mload mob 1.5",
        ] {
            run(&mut w, line);
            log_is(&w, "mload: bad syntax");
        }
        run(&mut w, "mload mob 9999");
        log_is(&w, "mload: bad mob vnum");
        run(&mut w, "mload obj 9999");
        log_is(&w, "mload: bad object vnum");
        run(&mut w, "mload xyz 1");
        log_is(&w, "mload: bad type");
        // "mob" abbreviates only to a prefix: "mobs" is not one.
        run(&mut w, "mload mobs 3100");
        log_is(&w, "mload: bad type");
        // Nothing was created by any of this.
        assert_eq!(w.host.list(w.room, "people").len(), 3);
    }

    #[test]
    fn mload_room_target_quirk_the_target_keeps_its_leading_space() {
        // two_arguments leaves the space before the third word in place, and
        // the C then asks isdigit() of that space.
        let mut w = World::new();
        run(&mut w, "mload mob 3100 3002");
        log_is(
            &w,
            "mload: room target vnum doesn't exist (loading mob vnum 3100 to room  3002)",
        );
        run(&mut w, "mload mob 3100 ");
        log_is(
            &w,
            "mload: room target vnum doesn't exist (loading mob vnum 3100 to room  )",
        );
        assert_eq!(w.host.list(w.room2, "people").len(), 0);
        assert!(
            w.host
                .script(w.mob)
                .unwrap()
                .global_vars
                .get("lastloaded")
                .is_none()
        );
    }

    #[test]
    fn mload_obj_goes_to_the_inventory_unless_it_cannot_be_taken() {
        let mut w = World::new();
        let (mob, room) = (w.mob, w.room);
        run(&mut w, "mload obj 7101");
        let ring = w.host.list(mob, "inventory")[0];
        assert_eq!(w.host.vnum(ring), 7101);
        assert_eq!(
            w.host
                .script(mob)
                .unwrap()
                .global_vars
                .get("lastloaded")
                .unwrap()
                .value,
            uid_text(ring)
        );
        run(&mut w, "mload obj 7102");
        let rock = *w.host.list(room, "contents").last().unwrap();
        assert_eq!(w.host.vnum(rock), 7102);
        assert_eq!(
            w.host
                .script(mob)
                .unwrap()
                .global_vars
                .get("lastloaded")
                .unwrap()
                .value,
            uid_text(rock)
        );
        // In no room a no-take object has nowhere to go.
        w.clear();
        w.host.char_from_room(mob);
        run(&mut w, "mload obj 7102");
        assert_eq!(
            w.effects("syslog"),
            ["syslog: SYSERR: Illegal value(s) passed to obj_to_room"]
        );
        // Likewise a mob.
        w.clear();
        run(&mut w, "mload mob 3100");
        assert_eq!(
            w.effects("syslog"),
            ["syslog: SYSERR: Illegal value(s) passed to char_to_room"]
        );
    }

    #[test]
    fn mload_obj_onto_a_character_worn_or_carried() {
        let mut w = World::new();
        let alice = w.player;
        run(&mut w, "mload obj 7101 alice");
        let carried = w.host.list(alice, "inventory");
        assert_eq!(carried.len(), 1);
        // Worn on a named place; the wear flags allow a finger (bit 1).
        run(&mut w, "mload obj 7101 alice rfinger");
        assert!(w.host.equipment(alice, 1).is_some());
        // That place is taken now, so the second ring is carried.
        run(&mut w, "mload obj 7101 alice rfinger");
        assert_eq!(w.host.list(alice, "inventory").len(), 2);
        // A place the object cannot be worn on, or one that is not a place.
        run(&mut w, "mload obj 7101 alice head");
        run(&mut w, "mload obj 7101 alice nowhere");
        assert_eq!(w.host.list(alice, "inventory").len(), 4);
        assert!(w.host.equipment(alice, 6).is_none());
        // Numeric positions work too, and an id names the character.
        run(&mut w, &format!("mload obj 7101 {} 2", uid_text(alice)));
        assert!(w.host.equipment(alice, 2).is_some());
        // A character that is not here is not a target: the room gets it.
        w.clear();
        run(&mut w, "mload obj 7101 zed");
        assert_eq!(w.host.list(w.room, "contents").len(), 2);
    }

    #[test]
    fn mload_obj_into_a_container() {
        let mut w = World::new();
        let (room, lamp) = (w.room, w.obj);
        let sack = w.host.add_obj(7100, "a sack");
        w.host.set_text(sack, "alias", "sack");
        w.host.set_num(sack, "type", 15);
        w.host.obj_to_room(sack, room);
        run(&mut w, "mload obj 7102 sack");
        let rock = w.host.list(sack, "contents")[0];
        assert_eq!(w.host.vnum(rock), 7102);
        // The lamp is not a container: the object lands in the room.
        run(&mut w, "mload obj 7102 lamp");
        assert!(
            w.host
                .list(room, "contents")
                .iter()
                .any(|o| w.host.vnum(*o) == 7102)
        );
        assert!(w.host.list(lamp, "contents").is_empty());
        // By id.
        run(&mut w, &format!("mload obj 7102 {}", uid_text(sack)));
        assert_eq!(w.host.list(sack, "contents").len(), 2);
    }

    #[test]
    fn mload_needs_an_implementor_behind_a_switched_mob() {
        let mut w = World::new();
        let mob = w.mob;
        w.host.set_num(mob, "desc", 1);
        w.switch_behind(mob, 33);
        run(&mut w, "mload mob 3100");
        run(&mut w, "mpurge");
        run(&mut w, "mforce alice look");
        run(&mut w, "mhunt alice");
        run(&mut w, "mremember alice");
        run(&mut w, "mforget alice");
        assert!(w.host.logs.is_empty());
        assert_eq!(w.host.list(w.room, "people").len(), 3);
        assert!(w.host.mud_commands.is_empty());
        assert!(w.host.script_mem(mob).is_empty());
        w.switch_behind(mob, 34);
        run(&mut w, "mload mob 3100");
        assert_eq!(w.host.list(w.room, "people").len(), 4);
    }

    #[test]
    fn mpurge_with_no_argument_clears_the_room_of_npcs_and_objects() {
        let mut w = World::new();
        let (mob, alice, lamp) = (w.mob, w.player, w.obj);
        let rat = bare_mob(&mut w);
        run(&mut w, "mpurge");
        assert!(w.host.is_dead(rat));
        assert!(!w.host.is_dead(mob) && !w.host.is_dead(alice));
        assert!(!w.host.exists(lamp));
        assert!(!w.host.vm.owner_purged);
    }

    #[test]
    fn mpurge_named_targets() {
        let mut w = World::new();
        let (mob, alice, lamp) = (w.mob, w.player, w.obj);
        let rat = bare_mob(&mut w);
        w.host.set_text(rat, "alias", "rat");
        run(&mut w, "mpurge rat");
        assert!(w.host.is_dead(rat));
        assert!(!w.host.vm.owner_purged);
        run(&mut w, "mpurge alice");
        log_is(&w, "mpurge: purging a PC");
        assert!(!w.host.is_dead(alice));
        run(&mut w, "mpurge zed");
        log_is(&w, "mpurge: bad argument");
        run(&mut w, &format!("mpurge {}", uid_text(lamp)));
        assert!(!w.host.exists(lamp));
        run(&mut w, "mpurge }999");
        log_is(&w, "mpurge: bad argument");
        // Purging oneself raises the flag the driver watches.
        run(&mut w, "mpurge guard");
        assert!(w.host.is_dead(mob));
        assert!(w.host.vm.owner_purged);
    }

    #[test]
    fn mpurge_an_object_by_name() {
        let mut w = World::new();
        let lamp = w.obj;
        run(&mut w, "mpurge lamp");
        assert!(!w.host.exists(lamp));
        assert!(!w.host.vm.owner_purged);
        // In no room there is nothing to sweep.
        w.host.char_from_room(w.mob);
        run(&mut w, "mpurge");
    }

    // ---- movement -------------------------------------------------------

    #[test]
    fn mgoto_moves_the_mob() {
        let mut w = World::new();
        let (mob, room2, alice) = (w.mob, w.room2, w.player);
        w.host.set_link(mob, "fighting", Some(alice));
        run(&mut w, "mgoto 3002");
        assert_eq!(w.host.in_room(mob), Some(room2));
        assert_eq!(w.host.link(mob, "fighting"), None);
        run(&mut w, "mgoto");
        log_is(&w, "mgoto called with no argument");
        run(&mut w, "mgoto 9999");
        log_is(&w, "mgoto: invalid location");
        run(&mut w, "mgoto zed");
        log_is(&w, "mgoto: invalid location");
        // To a character's room, and to an object's.
        run(&mut w, "mgoto alice");
        assert_eq!(w.host.in_room(mob), Some(w.room));
        run(&mut w, "mgoto 3002");
        run(&mut w, "mgoto lamp");
        assert_eq!(w.host.in_room(mob), Some(w.room));
        // A room number with a dot is a name.
        run(&mut w, "mgoto 3.002");
        log_is(&w, "mgoto: invalid location");
    }

    #[test]
    fn mgoto_with_a_negative_count_finds_nothing() {
        // atoi keeps the low 32 bits of 4294967294, which is -2: every match counts it further from
        // zero, so neither the character nor the object search returns.
        let mut w = World::new();
        let (mob, room2) = (w.mob, w.room2);
        w.host.char_to_room(mob, room2);
        run(&mut w, "mgoto 4294967294.alice");
        log_is(&w, "mgoto: invalid location");
        assert_eq!(w.host.in_room(mob), Some(room2));
        w.clear();
        run(&mut w, "mgoto 4294967294.lamp");
        log_is(&w, "mgoto: invalid location");
        assert_eq!(w.host.in_room(mob), Some(room2));
        // The same name with a count that is still positive does move it.
        w.clear();
        run(&mut w, "mgoto 1.alice");
        assert_eq!(w.host.in_room(mob), Some(w.room));
        // `self` and `me` are answered before the count is looked at: the
        // mob "goes" to its own room and nothing is logged.
        w.host.char_to_room(mob, room2);
        w.clear();
        run(&mut w, "mgoto 4294967294.self");
        run(&mut w, "mgoto 4294967294.me");
        assert!(w.host.logs.is_empty());
        assert_eq!(w.host.in_room(mob), Some(room2));
    }

    #[test]
    fn mgoto_to_a_private_room_with_company_is_refused() {
        let mut w = World::new();
        let (mob, room2, bob, alice) = (w.mob, w.room2, w.player2, w.player);
        w.host.set_num(room2, "flag:9", 1);
        run(&mut w, "mgoto 3002");
        assert_eq!(w.host.in_room(mob), Some(room2));
        w.host.char_to_room(mob, w.room);
        w.host.char_to_room(bob, room2);
        w.host.char_to_room(alice, room2);
        run(&mut w, "mgoto 3002");
        log_is(&w, "mgoto: invalid location");
        assert_eq!(w.host.in_room(mob), Some(w.room));
    }

    #[test]
    fn mat_leaves_the_mob_where_the_command_put_it() {
        // `if (IN_ROOM(ch) == location)`: when the command took the mob
        // somewhere else (`at XXXX quit`), it is not put back. A mock hook
        // stands in for the command and moves the mob on to a third room.
        let mut w = World::new();
        let mob = w.mob;
        let room3 = w.host.add_room(3003, "The Cellar");
        w.host.hook(
            Some(mob),
            "look",
            MockAction::MoveChar {
                ch: mob,
                room: room3,
            },
        );
        run(&mut w, "mat 3002 look");
        assert_eq!(w.host.mud_commands, [(mob, " look".to_owned())]);
        assert_eq!(w.host.in_room(mob), Some(room3));
    }

    #[test]
    fn mat_runs_a_command_elsewhere_and_comes_back() {
        let mut w = World::new();
        let mob = w.mob;
        run(&mut w, "mat 3002 say hi there");
        assert_eq!(w.host.mud_commands, [(mob, " say hi there".to_owned())]);
        assert_eq!(w.host.in_room(mob), Some(w.room));
        run(&mut w, "mat 3002");
        log_is(&w, "mat: bad argument");
        run(&mut w, "mat");
        log_is(&w, "mat: bad argument");
        run(&mut w, "mat 9999 look");
        log_is(&w, "mat: invalid location");
        // The test is on the raw rest of the line: a trailing space is a
        // command (a blank one).
        run(&mut w, "mat 3002 ");
        assert_eq!(w.host.mud_commands[1], (mob, " ".to_owned()));
        // In no room to begin with, the mob is left where the command put it
        // back out.
        w.host.char_from_room(mob);
        run(&mut w, "mat 3002 look");
        assert_eq!(w.host.in_room(mob), None);
    }

    #[test]
    fn mteleport_moves_people() {
        let mut w = World::new();
        let (alice, bob, mob, room, room2) = (w.player, w.player2, w.mob, w.room, w.room2);
        run(&mut w, "mteleport alice 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));
        run(&mut w, "mteleport all 3002");
        assert_eq!(w.host.in_room(bob), Some(room2));
        assert_eq!(w.host.in_room(mob), Some(room2));
        // The victim may be anywhere; the target may be a character.
        run(&mut w, "mteleport alice bob");
        assert_eq!(w.host.in_room(alice), Some(room2));
        w.host.char_to_room(alice, room);
        run(&mut w, "mteleport bob alice");
        assert_eq!(w.host.in_room(bob), Some(room));
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn mteleport_refusals() {
        let mut w = World::new();
        let (alice, mob) = (w.player, w.mob);
        run(&mut w, "mteleport alice");
        log_is(&w, "mteleport: bad syntax");
        run(&mut w, "mteleport");
        log_is(&w, "mteleport: bad syntax");
        run(&mut w, "mteleport alice 9999");
        log_is(&w, "mteleport target is an invalid room");
        run(&mut w, "mteleport zed 3002");
        log_is(&w, "mteleport: victim (zed) does not exist");
        run(&mut w, "mteleport }999 3002");
        log_is(&w, "mteleport: victim (}999) does not exist");
        run(&mut w, "mteleport all 3001");
        log_is(&w, "mteleport all target is itself");
        w.host.set_num(alice, "dg_invalid", 1);
        run(&mut w, "mteleport alice 3002");
        assert_eq!(w.host.in_room(alice), Some(w.room));
        run(&mut w, "mteleport all 3002");
        assert_eq!(w.host.in_room(alice), Some(w.room));
        assert_eq!(w.host.in_room(mob), Some(w.room2));
    }

    #[test]
    fn mteleport_all_from_no_room_does_nothing() {
        let mut w = World::new();
        w.host.char_from_room(w.mob);
        run(&mut w, "mteleport all 3002");
        assert!(w.host.logs.is_empty());
        assert_eq!(w.host.list(w.room2, "people").len(), 0);
    }

    // ---- forcing --------------------------------------------------------

    #[test]
    fn mforce_runs_a_command_as_someone_else() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        run(&mut w, "mforce alice say hello");
        assert_eq!(w.host.mud_commands, [(alice, " say hello".to_owned())]);
        run(&mut w, &format!("mforce {} jump", uid_text(bob)));
        assert_eq!(w.host.mud_commands[1], (bob, " jump".to_owned()));
        // A target that scripts may not touch is skipped quietly.
        w.host.set_num(alice, "dg_invalid", 1);
        run(&mut w, "mforce alice look");
        assert_eq!(w.host.mud_commands.len(), 2);
    }

    #[test]
    fn mforce_refusals() {
        let mut w = World::new();
        run(&mut w, "mforce");
        log_is(&w, "mforce: bad syntax");
        run(&mut w, "mforce alice");
        log_is(&w, "mforce: bad syntax");
        run(&mut w, "mforce zed look");
        log_is(&w, "mforce: no such victim");
        run(&mut w, "mforce }999 look");
        log_is(&w, "mforce: victim (}999) does not exist");
        run(&mut w, "mforce guard look");
        log_is(&w, "mforce: forcing self");
        run(&mut w, "mforce me look");
        log_is(&w, "mforce: forcing self");
        assert!(w.host.mud_commands.is_empty());
    }

    #[test]
    fn mforce_all_forces_playing_lower_level_visible_people_in_the_room() {
        let mut w = World::new();
        let (alice, bob, mob, room2) = (w.player, w.player2, w.mob, w.room2);
        // Equal levels are not lower.
        run(&mut w, "mforce all smile");
        assert!(w.host.mud_commands.is_empty());
        w.host.set_num(mob, "level", 10);
        run(&mut w, "mforce all smile");
        // Descriptor order: the newest first.
        assert_eq!(
            w.host.mud_commands,
            [(bob, " smile".to_owned()), (alice, " smile".to_owned())]
        );
        w.host.mud_commands.clear();
        // Not unseen ones, not other rooms, not the untargetable.
        w.host
            .entity_mut(mob)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![alice]);
        w.host.char_to_room(bob, room2);
        run(&mut w, "mforce all smile");
        assert!(w.host.mud_commands.is_empty());
        w.host.char_to_room(bob, w.room);
        w.host.set_num(bob, "dg_invalid", 1);
        run(&mut w, "mforce all smile");
        assert!(w.host.mud_commands.is_empty());
    }

    // ---- memory ---------------------------------------------------------

    #[test]
    fn mremember_and_mforget_keep_a_list_in_order() {
        let mut w = World::new();
        let (alice, bob, mob) = (w.player, w.player2, w.mob);
        run(&mut w, "mremember alice say hello");
        run(&mut w, "mremember bob");
        run(&mut w, "mremember alice ");
        let memory = w.host.script_mem(mob);
        assert_eq!(memory.len(), 3);
        assert_eq!(
            (memory[0].id, memory[0].cmd.as_deref()),
            (alice, Some(" say hello"))
        );
        assert_eq!((memory[1].id, memory[1].cmd.as_deref()), (bob, None));
        assert_eq!((memory[2].id, memory[2].cmd.as_deref()), (alice, Some(" ")));
        run(&mut w, "mforget alice");
        let memory = w.host.script_mem(mob);
        assert_eq!(memory.len(), 1);
        assert_eq!(memory[0].id, bob);
        // Forgetting someone not remembered is quietly nothing.
        run(&mut w, "mforget alice");
        assert_eq!(w.host.script_mem(mob).len(), 1);
        run(&mut w, &format!("mforget {}", uid_text(bob)));
        assert!(w.host.script_mem(mob).is_empty());
    }

    #[test]
    fn mremember_and_mforget_refusals() {
        let mut w = World::new();
        run(&mut w, "mremember");
        log_is(&w, "mremember: bad syntax");
        run(&mut w, "mremember zed");
        log_is(&w, "mremember: victim (zed) does not exist");
        run(&mut w, "mremember }999 x");
        log_is(&w, "mremember: victim (}999) does not exist");
        run(&mut w, "mforget");
        log_is(&w, "mforget: bad syntax");
        run(&mut w, "mforget zed");
        log_is(&w, "mforget: victim (zed) does not exist");
        assert!(w.host.script_mem(w.mob).is_empty());
    }

    #[test]
    fn mremember_finds_people_in_other_rooms() {
        let mut w = World::new();
        let (bob, room2) = (w.player2, w.room2);
        w.host.char_to_room(bob, room2);
        run(&mut w, "mremember bob");
        assert_eq!(w.host.script_mem(w.mob)[0].id, bob);
    }

    // ---- transforming ---------------------------------------------------

    #[test]
    fn mtransform_keeps_hit_points_only_for_a_plain_vnum() {
        let mut w = World::new();
        let mob = w.mob;
        run(&mut w, "mtransform 3100");
        assert_eq!(
            w.effects("transform_mob"),
            [format!("transform_mob {mob} 3100 true")]
        );
        run(&mut w, "mtransform -3100");
        assert_eq!(
            w.effects("transform_mob")[1],
            format!("transform_mob {mob} 3100 false")
        );
        // The mob is a rat now, and logs as one.
        // `ch->nr` is restored, so the vnum stays 3000.
        let rat = "Mob (a rat, VNum 3000):: ";
        run(&mut w, "mtransform");
        assert_eq!(w.log(), format!("{rat}mtransform: missing argument"));
        run(&mut w, "mtransform abc");
        assert_eq!(w.log(), format!("{rat}mtransform: bad argument"));
        run(&mut w, "mtransform +3100");
        assert_eq!(w.log(), format!("{rat}mtransform: bad argument"));
        run(&mut w, "mtransform 9999");
        assert_eq!(w.log(), format!("{rat}mtransform: bad mobile vnum"));
        run(&mut w, "mtransform -9999");
        assert_eq!(w.log(), format!("{rat}mtransform: bad mobile vnum"));
        // "-" alone reads vnum 0, which does not exist.
        run(&mut w, "mtransform -");
        assert_eq!(w.log(), format!("{rat}mtransform: bad mobile vnum"));
        assert_eq!(w.effects("transform_mob").len(), 2);
    }

    #[test]
    fn mtransform_refuses_a_mob_with_a_descriptor() {
        let mut w = World::new();
        let mob = w.mob;
        w.host.set_num(mob, "desc", 1);
        w.switch_behind(mob, 34);
        run(&mut w, "mtransform 3100");
        assert_eq!(
            w.sent_to(mob),
            ["You've got no VNUM to return to, dummy! try 'switch'\r\n"]
        );
        assert!(w.effects("transform_mob").is_empty());
    }

    // ---- doors ----------------------------------------------------------

    #[test]
    fn mdoor_sets_the_fields_of_an_exit() {
        let mut w = World::new();
        let (room, room2) = (w.room, w.room2);
        run(&mut w, "mdoor 3001 north description A long hall");
        assert_eq!(w.host.text(room, "exit:0:desc"), "A long hall\r\n");
        run(&mut w, "mdoor 3001 n flags ab");
        assert_eq!(w.host.num(room, "exit:0:flags"), 3);
        run(&mut w, "mdoor 3001 n flags 70000");
        // (sh_int)70000
        assert_eq!(w.host.num(room, "exit:0:flags"), 4464);
        run(&mut w, "mdoor 3001 n flags -1");
        assert_eq!(w.host.num(room, "exit:0:flags"), -1);
        run(&mut w, "mdoor 3001 north key 3005 junk");
        assert_eq!(w.host.num(room, "exit:0:key"), 3005);
        run(&mut w, "mdoor 3001 north name door gate");
        assert_eq!(w.host.text(room, "exit:0:keyword"), "door gate");
        run(&mut w, "mdoor 3001 east room 3002");
        assert_eq!(w.host.link(room, "exit:1"), Some(room2));
        // A new exit is made when none was there; the room it points at
        // before its destination is set is the first room.
        run(&mut w, "mdoor 3002 up key 7");
        assert_eq!(w.host.link(room2, "exit:4"), Some(room));
        assert_eq!(w.host.num(room2, "exit:4:key"), 7);
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn mdoor_purge_and_room_errors() {
        let mut w = World::new();
        let room = w.room;
        run(&mut w, "mdoor 3001 north purge");
        assert_eq!(w.host.link(room, "exit:0"), None);
        // Purging a missing exit is nothing.
        run(&mut w, "mdoor 3001 north purge");
        assert!(w.host.logs.is_empty());
        run(&mut w, "mdoor 3001 east room 9999");
        log_is(&w, "mdoor: invalid door target (arg == 9999)");
        assert_eq!(w.host.link(room, "exit:1"), None);
        assert_eq!(w.host.num(room, "exit:1:nowhere"), 1);
        // The exit that leads nowhere is still an exit: a later field set does
        // not make a new one pointing at room number 0.
        run(&mut w, "mdoor 3001 east flags a");
        assert_eq!(w.host.link(room, "exit:1"), None);
        assert_eq!(w.host.num(room, "exit:1:nowhere"), 1);
        assert_eq!(w.host.num(room, "exit:1:flags"), 1);
    }

    #[test]
    fn mdoor_argument_errors() {
        let mut w = World::new();
        for line in [
            "mdoor",
            "mdoor 3001",
            "mdoor 3001 north",
            "mdoor  3001  north ",
        ] {
            run(&mut w, line);
            log_is(&w, "mdoor called with too few args");
        }
        run(&mut w, "mdoor 9999 north purge");
        log_is(&w, "mdoor: invalid target (arg == 9999)");
        run(&mut w, "mdoor 3001 sideways purge");
        log_is(
            &w,
            "mdoor: invalid direction (arg == sideways) not found in: [ north east south west up down northwest northeast southeast southwest ]",
        );
        run(&mut w, "mdoor 3001 north colour red");
        log_is(
            &w,
            "mdoor: invalid field (arg == colour) not found in: [ purge description flags key name room ]",
        );
        // A room named by id.
        let id = uid_text(w.room);
        run(&mut w, &format!("mdoor {id} north purge"));
        assert_eq!(w.host.link(w.room, "exit:0"), None);
    }

    // ---- following ------------------------------------------------------

    #[test]
    fn mfollow_follows_and_changes_leaders() {
        let mut w = World::new();
        let (alice, bob, mob) = (w.player, w.player2, w.mob);
        run(&mut w, "mfollow alice");
        assert_eq!(w.host.link(mob, "master"), Some(alice));
        assert_eq!(w.host.list(alice, "followers"), [mob]);
        // Already following: nothing changes.
        run(&mut w, "mfollow alice");
        assert_eq!(w.host.list(alice, "followers"), [mob]);
        // Another leader: the old one is dropped first.
        run(&mut w, "mfollow bob");
        assert_eq!(w.host.link(mob, "master"), Some(bob));
        assert!(w.host.list(alice, "followers").is_empty());
        assert_eq!(w.host.list(bob, "followers"), [mob]);
        // Following oneself only stops following.
        run(&mut w, "mfollow me");
        assert_eq!(w.host.link(mob, "master"), None);
        assert!(w.host.list(bob, "followers").is_empty());
        run(&mut w, &format!("mfollow {}", uid_text(alice)));
        assert_eq!(w.host.link(mob, "master"), Some(alice));
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn mfollow_refuses_circles_and_unknowns() {
        let mut w = World::new();
        let (alice, mob) = (w.player, w.mob);
        run(&mut w, "mfollow");
        log_is(&w, "mfollow: bad syntax");
        run(&mut w, "mfollow zed");
        log_is(&w, "mfollow: victim (zed) not found");
        run(&mut w, "mfollow }999");
        log_is(&w, "mfollow: victim (}999) does not exist");
        // Alice follows the mob; the mob cannot follow Alice.
        w.host.set_link(alice, "master", Some(mob));
        run(&mut w, "mfollow alice");
        log_is(&w, "mfollow: Following in circles.");
        assert_eq!(w.host.link(mob, "master"), None);
        // The circle is found at any depth.
        let bob = w.player2;
        w.host.set_link(bob, "master", Some(alice));
        run(&mut w, "mfollow bob");
        log_is(&w, "mfollow: Following in circles.");
        // A leader is looked for in the room only.
        w.host.char_to_room(bob, w.room2);
        run(&mut w, "mfollow bob");
        log_is(&w, "mfollow: victim (bob) not found");
    }

    #[test]
    fn mfollow_leaves_the_old_leader_even_when_the_new_one_is_a_circle() {
        let mut w = World::new();
        let (alice, bob, mob) = (w.player, w.player2, w.mob);
        run(&mut w, "mfollow alice");
        w.host.set_link(bob, "master", Some(mob));
        run(&mut w, "mfollow bob");
        // The C drops the old master before it tests for the circle.
        log_is(&w, "mfollow: Following in circles.");
        assert_eq!(w.host.link(mob, "master"), None);
        assert!(w.host.list(alice, "followers").is_empty());
    }

    // ---- hostile input --------------------------------------------------

    #[test]
    fn hostile_arguments_do_not_panic() {
        let mut w = World::new();
        let long = "x".repeat(100_000);
        let nasty = [
            String::new(),
            " ".to_owned(),
            "\u{e9}\u{e9}\u{e9}".to_owned(),
            "}".to_owned(),
            "}}}}999999999999999999999999".to_owned(),
            "-".to_owned(),
            "\\".to_owned(),
            "~~~|||^^^&&&***```".to_owned(),
            "\u{1f600} ~\u{1f600} `\u{1f600}".to_owned(),
            long,
            "99999999999999999999 99999999999999999999".to_owned(),
            "-2147483648 -2147483648 x".to_owned(),
            // `atoi` wraps to `i32::MIN`; the count decrement must not
            // overflow when a target of that name is present.
            "2147483648.bob".to_owned(),
            "2147483648.lamp".to_owned(),
            "4294967294.alice".to_owned(),
        ];
        for (command, _) in MOB_SCRIPT_COMMANDS {
            for arg in &nasty {
                let mut line = format!("{command} {arg}");
                // Also with a second and third word.
                run(&mut w, &line);
                line = format!("{command} {arg} {arg} {arg}");
                run(&mut w, &line);
                line = format!("{command} 3001 {arg} {arg}");
                run(&mut w, &line);
            }
        }
        let _ = AttachType::Mob;
    }

    // ---- the get_number side effect ---------------------------------------

    fn two_lamps(w: &mut World) -> (Uid, Uid) {
        let second = w.host.add_obj(7000, "a lamp");
        w.host.set_text(second, "alias", "lamp");
        w.host.obj_to_room(second, w.room);
        let list = w.host.room_contents(w.room);
        (list[0], list[1])
    }

    #[test]
    fn mpurge_strips_the_count_before_the_object_lookup() {
        // `get_char_room_vis` strips `2.` from the buffer, so the object
        // lookup counts 1 and takes the first lamp.
        let mut w = World::new();
        let (first, second) = two_lamps(&mut w);
        run(&mut w, "mpurge 2.lamp");
        assert!(!w.host.exists(first));
        assert!(w.host.exists(second));
    }

    #[test]
    fn mload_container_lookup_counts_from_the_bare_name() {
        let mut w = World::new();
        w.host
            .globals
            .nums
            .insert("objproto:7100:type".to_owned(), 15);
        let mut sacks = Vec::new();
        for _ in 0..2 {
            let sack = w.host.add_obj(7100, "a sack");
            w.host.set_text(sack, "alias", "sack");
            w.host.set_num(sack, "type", 15);
            w.host.obj_to_room(sack, w.room);
            sacks.push(sack);
        }
        let order: Vec<Uid> = w
            .host
            .room_contents(w.room)
            .into_iter()
            .filter(|o| sacks.contains(o))
            .collect();
        run(&mut w, "mload obj 7102 2.sack");
        let rock = w
            .host
            .object_list()
            .into_iter()
            .find(|o| w.host.vnum(*o) == 7102)
            .expect("rock loaded");
        assert_eq!(w.host.obj_in_obj(rock), Some(order[0]));
    }

    #[test]
    fn mjunk_counts_only_on_the_first_equipment_pass() {
        // `mjunk 2.sword` is not an `all.` form, so the C takes the loop
        // branch: pass one counts 2, the buffer is then `sword`, so the
        // remaining passes count 1 and all three swords go.
        let mut w = World::new();
        let mob = w.mob;
        let mut swords = Vec::new();
        for pos in [5, 6, 7] {
            let sword = w.host.add_obj(7000, "a sword");
            w.host.set_text(sword, "alias", "sword");
            w.host.equip_char(mob, sword, pos);
            swords.push(sword);
        }
        run(&mut w, "mjunk 2.sword");
        assert!(swords.iter().all(|s| !w.host.exists(*s)));
    }

    #[test]
    fn mjunk_all_dot_count_takes_the_first_carried() {
        // `all.2.sword` leaves `2.sword`; the equipment lookup strips the
        // count, so the carried lookup takes the first sword.
        let mut w = World::new();
        let mob = w.mob;
        let mut swords = Vec::new();
        for _ in 0..2 {
            let sword = w.host.add_obj(7000, "a sword");
            w.host.set_text(sword, "alias", "sword");
            w.host.obj_to_char(sword, mob);
            swords.push(sword);
        }
        let first = w.host.carrying(mob)[0];
        run(&mut w, "mjunk all.2.sword");
        assert!(!w.host.exists(first));
        assert_eq!(w.host.carrying(mob).len(), 1);
    }

    #[test]
    fn failed_lookups_log_the_name_without_its_count() {
        let mut w = World::new();
        run(&mut w, "mkill 2.zed");
        log_is(&w, "mkill: victim (zed) not found");
        run(&mut w, "msend 2.zed hi");
        log_is(&w, "msend: victim (zed) does not exist");
        run(&mut w, "mechoaround last.zed hi");
        log_is(&w, "mechoaround: victim (zed) does not exist");
        run(&mut w, "mdamage 2.zed 5");
        log_is(&w, "mdamage: victim (zed) does not exist");
        run(&mut w, "mteleport 2.zed 3002");
        log_is(&w, "mteleport: victim (zed) does not exist");
        run(&mut w, "mhunt 2.zed");
        log_is(&w, "mhunt: victim (zed) does not exist");
        run(&mut w, "mremember 2.zed");
        log_is(&w, "mremember: victim (zed) does not exist");
        run(&mut w, "mforget 2.zed");
        log_is(&w, "mforget: victim (zed) does not exist");
        run(&mut w, "mfollow 2.zed");
        log_is(&w, "mfollow: victim (zed) not found");
        // A UID argument never reaches get_number.
        run(&mut w, "mkill }9.9");
        log_is(&w, "mkill: victim (}9.9) not found");
    }

    #[test]
    fn room_arguments_wrap_to_sixteen_bits() {
        // `real_room((room_vnum)68538)` is room 3002.
        let mut w = World::new();
        let (alice, room2) = (w.player, w.room2);
        run(&mut w, "mteleport alice 68538");
        assert_eq!(w.host.in_room(alice), Some(room2));
    }

    #[test]
    fn mtransform_puts_the_equipment_back_and_extracts_the_temporary_mob() {
        let mut w = World::new();
        let (mob, lamp) = (w.mob, w.obj);
        w.host.equip_char(mob, lamp, 3);
        run(&mut w, "mtransform 3100");
        assert_eq!(w.host.equipment(mob, 3), Some(lamp));
        assert_eq!(w.host.vnum(mob), 3000);
        assert_eq!(w.effects("extract_char").len(), 1);
    }

    // ---- the buffer after a `0.` count ------------------------------------

    #[test]
    fn a_zero_count_world_search_looks_for_the_bare_name() {
        // `0.2.bob`: the room pass strips `2.` from the buffer and finds no
        // such player in the room; the world pass then asks for the player
        // `bob`, count 1 (handler.c `get_char_world_vis`).
        let mut w = World::new();
        let (mob, bob, room2) = (w.mob, w.player2, w.room2);
        w.host.char_to_room(bob, room2);
        run(&mut w, "mhunt 0.2.bob");
        assert!(w.host.logs.is_empty());
        assert_eq!(w.effects("hunt "), [format!("hunt {mob} {bob}")]);
        run(&mut w, "mgoto 0.2.bob");
        assert!(w.host.logs.is_empty());
        assert_eq!(w.host.in_room(mob), Some(room2));
        // With Bob in the mob's room the room pass (count 2) still finds no
        // second Bob, and the world pass finds the first.
        let mut w = World::new();
        let (mob, bob) = (w.mob, w.player2);
        run(&mut w, "mhunt 0.2.bob");
        assert!(w.host.logs.is_empty());
        assert_eq!(w.effects("hunt "), [format!("hunt {mob} {bob}")]);
    }

    #[test]
    fn a_zero_count_strips_the_name_a_second_time_in_the_buffer() {
        // `get_char_room_vis` sends `0.<name>` to `get_player_vis`, which runs
        // `get_number` on the same buffer again; a failed world search does so
        // once more. The log lines and the follow-up lookups see what is left.
        let mut w = World::new();
        for (line, text) in [
            ("mkill 0.2.zed", "mkill: victim (zed) not found"),
            ("msend 0.2.zed hi", "msend: victim (zed) does not exist"),
            (
                "mechoaround 0.2.zed hi",
                "mechoaround: victim (zed) does not exist",
            ),
            ("mdamage 0.2.zed 5", "mdamage: victim (zed) does not exist"),
            ("mfollow 0.2.zed", "mfollow: victim (zed) not found"),
            (
                "mteleport 0.2.zed 3002",
                "mteleport: victim (zed) does not exist",
            ),
            ("mhunt 0.2.zed", "mhunt: victim (zed) does not exist"),
            (
                "mremember 0.2.zed",
                "mremember: victim (zed) does not exist",
            ),
            ("mforget 0.2.zed", "mforget: victim (zed) does not exist"),
            // The world search strips a third time.
            ("mhunt 0.1.2.zed", "mhunt: victim (zed) does not exist"),
            (
                "mteleport 0.1.2.zed 3002",
                "mteleport: victim (zed) does not exist",
            ),
            // `last.` is a count of its own: it strips once and is never zero.
            ("mkill last.2.zed", "mkill: victim (2.zed) not found"),
            // Not a number: the count is 0 as well.
            ("mkill x.2.zed", "mkill: victim (zed) not found"),
            // A real count strips once.
            ("mkill 2.1.zed", "mkill: victim (1.zed) not found"),
        ] {
            w.clear();
            run(&mut w, line);
            log_is(&w, text);
        }
    }

    #[test]
    fn a_zero_count_leaves_the_bare_name_for_the_object_lookup() {
        let mut w = World::new();
        let mob = w.mob;
        let mut swords = Vec::new();
        for _ in 0..2 {
            let sword = w.host.add_obj(7000, "a sword");
            w.host.set_text(sword, "alias", "sword");
            w.host.obj_to_char(sword, mob);
            swords.push(sword);
        }
        // Carried head first: the first in the list is the one added last.
        let (first, second) = (w.host.carrying(mob)[0], w.host.carrying(mob)[1]);
        run(&mut w, "mpurge 0.2.sword");
        assert!(!w.host.exists(first));
        assert!(w.host.exists(second));
        assert_eq!(w.log(), "");
    }

    #[test]
    fn a_zero_count_leaves_the_bare_name_for_the_container_lookup() {
        let mut w = World::new();
        let mut sacks = Vec::new();
        for _ in 0..2 {
            let sack = w.host.add_obj(7100, "a sack");
            w.host.set_text(sack, "alias", "sack");
            w.host.set_num(sack, "type", 15);
            w.host.obj_to_room(sack, w.room);
            sacks.push(sack);
        }
        run(&mut w, "mload obj 7102 0.2.sack");
        let rock = w.host.object_list()[0];
        assert_eq!(w.host.vnum(rock), 7102);
        assert_eq!(w.host.obj_in_obj(rock), Some(sacks[0]));
    }

    // ---- commands a switched mob below LVL_IMPL may not give --------------

    #[test]
    fn a_switched_mob_below_impl_is_refused_by_each_world_changing_command() {
        let mut w = World::new();
        let (mob, alice, bob, lamp) = (w.mob, w.player, w.player2, w.obj);
        let rat = bare_mob(&mut w);
        w.host.set_text(rat, "alias", "rat");
        w.host.set_num(mob, "desc", 1);
        w.switch_behind(mob, 33);
        let remembered = ScriptMemory {
            id: alice,
            cmd: None,
        };
        w.host.set_script_mem(mob, vec![remembered.clone()]);
        let lines = [
            "mpurge",
            "mpurge rat",
            "mpurge lamp",
            "mhunt alice",
            "mremember bob",
            "mforget alice",
            "mforce alice look",
            "mload mob 3100",
        ];
        for line in lines {
            run(&mut w, line);
        }
        assert!(w.host.logs.is_empty());
        assert!(!w.host.is_dead(rat));
        assert!(w.host.exists(lamp));
        assert!(w.effects("hunt").is_empty());
        assert_eq!(w.host.script_mem(mob), [remembered]);
        assert!(w.host.mud_commands.is_empty());
        assert_eq!(w.host.list(w.room, "people").len(), 4);

        // The same lines work for an implementor.
        w.switch_behind(mob, 34);
        for line in [
            "mpurge rat",
            "mhunt alice",
            "mremember bob",
            "mforget alice",
            "mforce alice look",
            "mload mob 3100",
        ] {
            run(&mut w, line);
        }
        assert!(w.host.is_dead(rat));
        assert_eq!(w.effects("hunt").len(), 1);
        assert_eq!(
            w.host.script_mem(mob),
            [ScriptMemory { id: bob, cmd: None }]
        );
        assert_eq!(w.host.mud_commands, [(alice, " look".to_owned())]);
        assert_eq!(w.host.list(w.room, "people").len(), 5);
        run(&mut w, "mpurge");
        assert!(!w.host.exists(lamp));
    }

    #[test]
    fn a_player_with_triggers_is_judged_by_their_own_level() {
        // A descriptor with no `original` is answered with the character's
        // own level, which is below LVL_IMPL for an ordinary player.
        let mut w = World::new();
        let (alice, proto) = (w.player, w.proto.clone());
        w.host.attach(alice, &proto);
        run_as(&mut w, alice, "mload mob 3100");
        assert_eq!(w.host.list(w.room, "people").len(), 3);
        w.host.set_num(alice, "level", 34);
        run_as(&mut w, alice, "mload mob 3100");
        assert_eq!(w.host.list(w.room, "people").len(), 4);
    }

    fn run_as(w: &mut World, ch: Uid, line: &str) {
        assert!(w.run_mob(ch, line));
    }

    // ---- DG_ALLOW_GODS ----------------------------------------------------

    #[test]
    fn force_refuses_the_highest_gods_and_teleport_does_not() {
        let mut w = World::new();
        let (alice, room2) = (w.player, w.room2);
        w.host.set_num(alice, "level", 33);
        // `mforce` passes 0, `mteleport` DG_ALLOW_GODS.
        run(&mut w, "mforce alice look");
        assert!(w.host.mud_commands.is_empty());
        run(&mut w, "mteleport alice 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));
        // One level lower is forced like a mortal unless nohassle is on.
        w.host.set_num(alice, "level", 32);
        run(&mut w, "mteleport alice 3001");
        assert_eq!(w.host.in_room(alice), Some(w.room));
        run(&mut w, "mforce alice look");
        assert_eq!(w.host.mud_commands.len(), 1);
        w.set_nohassle(alice, true);
        run(&mut w, "mforce alice look");
        run(&mut w, "mteleport alice 3002");
        assert_eq!(w.host.mud_commands.len(), 1);
        assert_eq!(w.host.in_room(alice), Some(w.room));
    }

    #[test]
    fn force_all_and_teleport_all_apply_the_same_rule() {
        let mut w = World::new();
        let (alice, bob, mob, room2) = (w.player, w.player2, w.mob, w.room2);
        w.host.set_num(alice, "level", 33);
        w.host.set_num(mob, "level", 40);
        run(&mut w, "mforce all look");
        // Bob only: Alice is a high god and the rest are not lower level.
        assert_eq!(w.host.mud_commands, [(bob, " look".to_owned())]);
        run(&mut w, "mteleport all 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));
        assert_eq!(w.host.in_room(bob), Some(room2));
    }

    // ---- sendok, damage and following boundaries --------------------------

    #[test]
    fn writing_blocks_players_but_not_mobs() {
        let mut w = World::new();
        let (alice, mob) = (w.player, w.mob);
        w.host.set_num(mob, "desc", 1);
        w.switch_behind(mob, 34);
        // Bit 4 of an NPC's act flags is MOB_AWARE, not PLR_WRITING.
        w.set_writing(mob, true);
        w.set_writing(alice, true);
        run(&mut w, "mecho hi");
        assert_eq!(w.sent_to(mob), ["hi\n\r"]);
        assert!(w.sent_to(alice).is_empty());
    }

    #[test]
    fn already_following_the_leader_changes_nothing() {
        let mut w = World::new();
        let (alice, mob) = (w.player, w.mob);
        let rat = bare_mob(&mut w);
        w.host.set_text(rat, "alias", "rat");
        w.host.cmd_follow(mob, alice);
        w.host.cmd_follow(rat, alice);
        assert_eq!(w.host.list(alice, "followers"), [rat, mob]);
        run(&mut w, "mfollow alice");
        // Leaving and rejoining would put the mob at the head of the list.
        assert_eq!(w.host.list(alice, "followers"), [rat, mob]);
        assert!(w.host.logs.is_empty());
    }

    // ---- branches the earlier tests left unpinned --------------------------

    #[test]
    fn msend_reaches_a_sleeping_victim() {
        // `sub_write` has `to_sleeping = 1`.
        let mut w = World::new();
        let bob = w.player2;
        w.host.set_num(bob, "pos", 4);
        run(&mut w, "msend bob hi");
        assert_eq!(w.sent_to(bob), ["hi\n\r"]);
    }

    #[test]
    fn mzoneecho_and_mdoor_narrow_their_room_numbers_to_sixteen_bits() {
        let mut w = World::new();
        let (alice, room, room2) = (w.player, w.room, w.room2);
        w.host.globals.nums.insert("zone:3:bot".to_owned(), 3000);
        w.host.globals.nums.insert("zone:3:top".to_owned(), 3099);
        w.host.set_num(room, "zone", 3);
        // 68537 is 65536 + 3001.
        run(&mut w, "mzoneecho 68537 hello");
        assert_eq!(w.sent_to(alice), ["hello\r\n"]);
        assert!(w.host.logs.is_empty());
        // The room a door leads to is narrowed too (68538 is 65536 + 3002).
        run(&mut w, "mdoor 3001 east room 68538");
        assert_eq!(w.host.link(room, "exit:1"), Some(room2));
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn mtransform_puts_the_temporary_mob_in_the_room_of_the_mob() {
        // `char_to_room(m, IN_ROOM(ch))` before the struct copy.
        let mut w = World::new();
        let (mob, room) = (w.mob, w.room);
        run(&mut w, "mtransform 3100");
        let extracted = w.effects("extract_char");
        assert_eq!(extracted.len(), 1);
        let temp: Uid = extracted[0]
            .strip_prefix("extract_char ")
            .and_then(|uid| uid.parse().ok())
            .unwrap();
        assert_ne!(temp, mob);
        assert_eq!(w.host.in_room(temp), Some(room));
    }

    #[test]
    fn mjunk_strips_the_count_from_a_word_that_starts_with_the_uid_character() {
        // `do_mjunk` has no UID branch: `get_number` strips the buffer
        // whatever its first byte, so the lookups see `sword`.
        let mut w = World::new();
        let (mob, lamp) = (w.mob, w.obj);
        let sword = w.host.add_obj(7200, "a sword");
        w.host.set_text(sword, "alias", "sword");
        w.host.obj_to_char(sword, mob);
        w.host.equip_char(mob, lamp, 16);
        run(&mut w, "mjunk all.}1.sword");
        assert_eq!(w.effects("extract_obj"), [format!("extract_obj {sword}")]);
        assert!(!w.host.exists(sword));
        assert!(w.host.exists(lamp));
    }
}
