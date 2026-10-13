//! `dg_wldcmd.c`: the world (room) script commands, `wld_cmd_info[]` and
//! `wld_command_interpreter`.
//!
//! Matched like the object table: `strncmp(entry, word, strlen(word))` in
//! table order against entries that end in a space. `wlog`, the last entry,
//! has no trailing space, so only the full word `wlog` reaches it: `wl` and
//! `wlo` select `wload` (which comes first), and `wlogs` matches nothing.

use super::comm::{TO_CHAR, TO_ROOM, act_to_room, sub_write};
use super::misc::script_damage;
use super::shared::{
    Owner, dir_count, door, exit_to_room, load, recho, starts_with_digit, teleport, vnum16,
    zoneecho,
};
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

    for door in 0..dir_count(host) {
        if let Some(to) = exit_to_room(host, room, door)
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

    let nr = vnum16(atoi(&arg2));
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
        host.real_room(vnum16(atoi(&arg)))
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

    let nr = vnum16(atoi(&arg2));
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::commands::support::World;
    use crate::host::{BaseHost, WorldHost};
    use crate::types::{AttachType, uid_text};

    const PREFIX: &str = "Room 3001 :: ";

    fn run(w: &mut World, line: &str) {
        let room = w.room;
        w.run_wld(room, line);
    }

    fn log_is(w: &World, text: &str) {
        assert_eq!(w.log(), format!("{PREFIX}{text}"));
    }

    // ---- echoes ---------------------------------------------------------

    #[test]
    fn wecho_is_an_act_to_everyone_in_the_room() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        run(&mut w, "wecho  the ground trembles.");
        assert_eq!(w.host.effects.len(), 2);
        assert_eq!(
            w.host.effects[0],
            format!("send {alice}: The ground trembles.\r\n")
        );
        assert_eq!(
            w.host.effects[1],
            format!("send {bob}: The ground trembles.\r\n")
        );
        // $-codes are act's: $n is the room's first occupant (Bob).
        w.clear();
        run(&mut w, "wecho $n shivers; cost $$5 and $e");
        assert_eq!(w.sent_to(alice), ["Bob shivers; cost $5 and it\r\n"]);
        assert_eq!(w.sent_to(bob), ["Bob shivers; cost $5 and it\r\n"]);
        // ~ and | are not act's: they stay as written.
        w.clear();
        run(&mut w, "wecho ~alice");
        assert_eq!(w.sent_to(alice), ["~alice\r\n"]);
    }

    #[test]
    fn wecho_errors_and_the_unlucky_dollar() {
        let mut w = World::new();
        run(&mut w, "wecho");
        log_is(&w, "wecho called with no args");
        run(&mut w, "wecho    ");
        log_is(&w, "wecho called with no args");
        w.clear();
        run(&mut w, "wecho costs $5");
        assert_eq!(w.sent_to(w.player), ["Costs \r\n"]);
        assert_eq!(
            w.effects("syslog"),
            [
                "syslog: SYSERR: Illegal $-code to act(): 5",
                "syslog: SYSERR: 5"
            ]
            .repeat(2)
        );
        // An empty room is nobody to tell.
        let mut w = World::new();
        let room2 = w.room2;
        w.run_wld(room2, "wecho hello");
        assert!(w.host.effects.is_empty());
    }

    #[test]
    #[ignore = "needs the dispatch bodies: act_mtrigger must fire the act trigger"]
    fn wecho_offers_the_line_to_npcs_that_watch_for_it() {
        // The room's head is Bob. `act_to_room` tells the guard once (the
        // TO_ROOM pass); Bob's own TO_CHAR line is not offered to anyone.
        // A guard without a descriptor sees the line without a capital.
        let mut w = World::new();
        let mob = w.mob;
        let act_proto = w
            .host
            .add_proto(901, AttachType::Mob, "e", 0, "hush", "say heard %arg%");
        w.host.attach(mob, &act_proto);
        run(&mut w, "wecho hush");
        assert_eq!(w.host.mud_commands, [(mob, "say heard hush".to_owned())]);
    }

    #[test]
    fn wasound_goes_to_the_neighbours() {
        let mut w = World::new();
        let (alice, bob, room, room2) = (w.player, w.player2, w.room, w.room2);
        w.host.char_to_room(alice, room2);
        w.host.char_to_room(bob, room2);
        w.host.set_link(room, "exit:1", Some(room));
        run(&mut w, "wasound  a distant roar");
        // Room 2's head is Bob.
        assert_eq!(w.host.effects.len(), 2);
        assert_eq!(
            w.host.effects[0],
            format!("send {alice}: A distant roar\r\n")
        );
        assert_eq!(w.host.effects[1], format!("send {bob}: A distant roar\r\n"));
        run(&mut w, "wasound");
        log_is(&w, "wasound called with no argument");
        // Directions past DIR_COUNT are not heard.
        w.clear();
        w.host.set_link(room, "exit:7", Some(room2));
        run(&mut w, "wasound x");
        assert_eq!(w.host.effects.len(), 2);
    }

    #[test]
    fn wasound_skips_an_exit_back_into_the_same_room() {
        let mut w = World::new();
        let (alice, bob, room, room2) = (w.player, w.player2, w.room, w.room2);
        w.host.char_to_room(bob, room2);
        w.host.set_link(room, "exit:1", Some(room));
        w.host.set_link(room, "exit:2", Some(room2));
        run(&mut w, "wasound Hush");
        // The room echo is a TO_ROOM and a TO_CHAR for the head of the room.
        assert_eq!(w.sent_to(bob), ["Hush\r\n", "Hush\r\n"]);
        assert!(w.sent_to(alice).is_empty());
    }

    #[test]
    fn wsend_and_wechoaround() {
        let mut w = World::new();
        let (alice, bob, room2) = (w.player, w.player2, w.room2);
        run(&mut w, "wsend alice  Psst ~alice");
        assert_eq!(w.sent_to(alice), ["Psst you\n\r"]);
        assert!(w.sent_to(bob).is_empty());
        w.clear();
        run(&mut w, "wechoaround alice ~alice sneezes");
        assert_eq!(w.sent_to(bob), ["Alice sneezes\n\r"]);
        assert!(w.sent_to(alice).is_empty());
        // The target is looked for in the room first, then the world.
        w.clear();
        w.host.char_to_room(bob, room2);
        run(&mut w, "wsend bob far away");
        assert_eq!(w.sent_to(bob), ["far away\n\r"]);
        run(&mut w, "wsend");
        log_is(&w, "wsend called with no args");
        run(&mut w, "wsend alice");
        log_is(&w, "wsend called without a message");
        run(&mut w, "wsend zed hi");
        log_is(&w, "no target found for wsend");
        run(&mut w, "wechoaround zed hi");
        log_is(&w, "no target found for wsend");
    }

    #[test]
    fn wlog_wzoneecho_wrecho_wdoor_wording() {
        let mut w = World::new();
        let (alice, room) = (w.player, w.room);
        run(&mut w, "wlog");
        assert!(w.host.logs.is_empty());
        run(&mut w, "wlog   50% done");
        log_is(&w, "50% done");
        run(&mut w, "wrecho 3001 3001 Boom");
        assert_eq!(w.sent_to(alice), ["Boom\r\n"]);
        run(&mut w, "wrecho 3001");
        log_is(&w, "wrecho: too few args");
        w.host.globals.nums.insert("zone:3:bot".to_owned(), 3000);
        w.host.globals.nums.insert("zone:3:top".to_owned(), 3099);
        w.host.set_num(room, "zone", 3);
        w.clear();
        run(&mut w, "wzoneecho 3001 Tremor");
        assert_eq!(w.sent_to(alice), ["Tremor\r\n"]);
        run(&mut w, "wzoneecho 3001");
        log_is(&w, "wzoneecho called with too few args");
        run(&mut w, "wzoneecho 5000 x");
        log_is(&w, "wzoneecho called for nonexistant zone");
        run(&mut w, "wdoor 3001 north key 4");
        assert_eq!(w.host.num(room, "exit:0:key"), 4);
        run(&mut w, "wdoor 3001");
        log_is(&w, "wdoor called with too few args");
        run(&mut w, "wdoor 3001 north room 1");
        log_is(&w, "wdoor: invalid door target (arg == 1)");
    }

    // ---- people ---------------------------------------------------------

    #[test]
    fn wteleport_one_or_all() {
        let mut w = World::new();
        let (alice, bob, mob, room2) = (w.player, w.player2, w.mob, w.room2);
        run(&mut w, "wteleport alice 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));
        run(&mut w, "wteleport all 3002");
        assert_eq!(w.host.in_room(bob), Some(room2));
        assert_eq!(w.host.in_room(mob), Some(room2));
        // Anyone in the world can be named.
        run(&mut w, "wteleport alice 3001");
        assert_eq!(w.host.in_room(alice), Some(w.room));
        assert!(w.host.logs.is_empty());
        // The untargetable stay put.
        w.host.set_num(alice, "dg_invalid", 1);
        run(&mut w, "wteleport alice 3002");
        assert_eq!(w.host.in_room(alice), Some(w.room));
    }

    #[test]
    fn wforce_refuses_the_highest_gods_and_wteleport_does_not() {
        // A level 33 player is found by the lookup (it allows gods), refused
        // by `wforce` (`valid_dg_target(ch, 0)`) and accepted by
        // `wteleport` (`DG_ALLOW_GODS`), in the single and the `all` form.
        let mut w = World::new();
        let (alice, bob, mob, room2) = (w.player, w.player2, w.mob, w.room2);
        w.host.set_num(alice, "level", 33);
        run(&mut w, "wforce alice look");
        assert!(w.host.mud_commands.is_empty());
        run(&mut w, "wforce all look");
        assert_eq!(
            w.host.mud_commands,
            [(bob, " look".to_owned()), (mob, " look".to_owned())]
        );
        run(&mut w, "wteleport alice 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));

        let mut w = World::new();
        let (alice, bob, room2) = (w.player, w.player2, w.room2);
        w.host.set_num(alice, "level", 33);
        run(&mut w, "wteleport all 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));
        assert_eq!(w.host.in_room(bob), Some(room2));
    }

    #[test]
    fn wforce_and_wteleport_skip_a_god_with_nohassle() {
        let mut w = World::new();
        let (alice, bob, mob, room) = (w.player, w.player2, w.mob, w.room);
        w.host.set_num(alice, "level", 32);
        run(&mut w, "wforce alice look");
        assert_eq!(w.host.mud_commands, [(alice, " look".to_owned())]);
        w.host.mud_commands.clear();
        w.set_nohassle(alice, true);
        run(&mut w, "wforce alice look");
        run(&mut w, "wforce all look");
        assert_eq!(
            w.host.mud_commands,
            [(bob, " look".to_owned()), (mob, " look".to_owned())]
        );
        run(&mut w, "wteleport alice 3002");
        run(&mut w, "wteleport all 3002");
        assert_eq!(w.host.in_room(alice), Some(room));
        assert_eq!(w.host.in_room(bob), Some(w.room2));
    }

    #[test]
    fn wteleport_errors() {
        let mut w = World::new();
        run(&mut w, "wteleport");
        log_is(&w, "wteleport called with too few args");
        run(&mut w, "wteleport alice");
        log_is(&w, "wteleport called with too few args");
        run(&mut w, "wteleport alice 9999");
        log_is(&w, "wteleport target is an invalid room");
        run(&mut w, "wteleport alice bob");
        log_is(&w, "wteleport target is an invalid room");
        run(&mut w, "wteleport zed 3002");
        log_is(&w, "wteleport: no target found");
        run(&mut w, "wteleport all 3001");
        log_is(&w, "wteleport all target is itself");
        // Single targets are not checked for being already there.
        run(&mut w, "wteleport alice 3001");
        assert_eq!(w.host.in_room(w.player), Some(w.room));
    }

    #[test]
    fn wforce_one_or_all() {
        let mut w = World::new();
        let (alice, bob, mob) = (w.player, w.player2, w.mob);
        run(&mut w, "wforce alice say hi");
        assert_eq!(w.host.mud_commands, [(alice, " say hi".to_owned())]);
        w.host.mud_commands.clear();
        run(&mut w, "wforce all wave");
        assert_eq!(
            w.host.mud_commands,
            [
                (bob, " wave".to_owned()),
                (alice, " wave".to_owned()),
                (mob, " wave".to_owned())
            ]
        );
        w.host.mud_commands.clear();
        w.host.set_num(alice, "dg_invalid", 1);
        run(&mut w, "wforce alice wave");
        run(&mut w, "wforce all wave");
        assert_eq!(w.host.mud_commands.len(), 2);
        run(&mut w, "wforce");
        log_is(&w, "wforce called with too few args");
        run(&mut w, "wforce bob");
        log_is(&w, "wforce called with too few args");
        run(&mut w, "wforce zed wave");
        log_is(&w, "wforce: no target found");
    }

    #[test]
    fn wdamage_hurts() {
        let mut w = World::new();
        let alice = w.player;
        run(&mut w, "wdamage alice 4");
        assert_eq!(w.host.num(alice, "hit"), 16);
        w.host.char_to_room(alice, w.room2);
        run(&mut w, "wdamage alice 4");
        assert_eq!(w.host.num(alice, "hit"), 12);
        run(&mut w, "wdamage alice");
        log_is(&w, "wdamage: bad syntax");
        run(&mut w, "wdamage zed 4");
        log_is(&w, "wdamage: target not found");
    }

    // ---- purging, loading, moving ---------------------------------------

    #[test]
    fn wpurge_everything_or_one_thing() {
        let mut w = World::new();
        let (mob, alice, lamp, room) = (w.mob, w.player, w.obj, w.room);
        run(&mut w, "wpurge");
        assert!(w.host.is_dead(mob) && !w.host.is_dead(alice));
        assert!(!w.host.exists(lamp));
        let rock = w.host.add_obj(1, "a rock");
        w.host.set_text(rock, "alias", "rock");
        w.host.obj_to_room(rock, room);
        let rat = w.host.add_mob(3100, "a rat");
        w.host.char_to_room(rat, room);
        run(&mut w, "wpurge rock");
        assert!(!w.host.exists(rock));
        run(&mut w, "wpurge rat");
        assert!(w.host.is_dead(rat));
        run(&mut w, "wpurge alice");
        log_is(&w, "wpurge: purging a PC");
        run(&mut w, "wpurge zed");
        log_is(&w, "wpurge: bad argument");
        run(&mut w, "wpurge }999");
        log_is(&w, "wpurge: bad argument");
        // By id, from anywhere.
        let far = w.host.add_obj(2, "a gem");
        w.host.obj_to_room(far, w.room2);
        run(&mut w, &format!("wpurge {}", uid_text(far)));
        assert!(!w.host.exists(far));
        // A character in another room is not found by name.
        let other = w.host.add_mob(3100, "a bat");
        w.host.char_to_room(other, w.room2);
        w.host.set_text(other, "alias", "bat");
        run(&mut w, "wpurge bat");
        log_is(&w, "wpurge: bad argument");
        run(&mut w, &format!("wpurge {}", uid_text(other)));
        assert!(w.host.is_dead(other));
    }

    #[test]
    fn wload_mob_and_obj_into_the_room() {
        let mut w = World::new();
        let (room, room2) = (w.room, w.room2);
        run(&mut w, "wload mob 3100");
        let rat = w.host.list(room, "people")[0];
        assert_eq!(w.host.vnum(rat), 3100);
        assert_eq!(
            w.host
                .script(room)
                .unwrap()
                .global_vars
                .get("lastloaded")
                .unwrap()
                .value,
            uid_text(rat)
        );
        run(&mut w, "wload obj 7102");
        let rock = *w.host.list(room, "contents").last().unwrap();
        assert_eq!(w.host.vnum(rock), 7102);
        assert_eq!(
            w.host
                .script(room)
                .unwrap()
                .global_vars
                .get("lastloaded")
                .unwrap()
                .value,
            uid_text(rock)
        );
        // A target room is given as a number, but only the very first
        // character counts, which is a space.
        run(&mut w, "wload mob 3100 3002");
        log_is(
            &w,
            "wload: room target vnum doesn't exist (loading mob vnum 3100 to room  3002)",
        );
        assert!(w.host.list(room2, "people").is_empty());
    }

    #[test]
    fn wload_obj_onto_a_character_or_into_a_container_forgets_lastloaded() {
        let mut w = World::new();
        let (room, alice, room2) = (w.room, w.player, w.room2);
        w.host.script_mut(room).unwrap().global_vars.vars.clear();
        run(&mut w, "wload obj 7101 alice");
        assert_eq!(w.host.list(alice, "inventory").len(), 1);
        run(&mut w, "wload obj 7101 alice rfinger");
        assert!(w.host.equipment(alice, 1).is_some());
        let sack = w.host.add_obj(7100, "a sack");
        w.host.set_text(sack, "alias", "sack");
        w.host.set_num(sack, "type", 15);
        w.host.obj_to_room(sack, room);
        run(&mut w, "wload obj 7102 sack");
        assert_eq!(w.host.list(sack, "contents").len(), 1);
        // Only the no-target form sets the variable.
        assert!(
            w.host
                .script(room)
                .unwrap()
                .global_vars
                .get("lastloaded")
                .is_none()
        );
        // Characters and containers are looked for in this room only.
        let far = w.host.add_obj(7100, "a chest");
        w.host.set_text(far, "alias", "chest");
        w.host.set_num(far, "type", 15);
        w.host.obj_to_room(far, room2);
        let before = w.host.list(room, "contents").len();
        run(&mut w, "wload obj 7102 chest");
        assert!(w.host.list(far, "contents").is_empty());
        assert_eq!(w.host.list(room, "contents").len(), before + 1);
        // A script-less room still loads.
        w.host.remove_script(room);
        run(&mut w, "wload mob 3100");
        assert!(w.host.script(room).is_none());
    }

    #[test]
    fn wload_errors_with_the_c_wording() {
        let mut w = World::new();
        for line in ["wload", "wload mob", "wload mob x", "wload mob -1"] {
            run(&mut w, line);
            log_is(&w, "wload: bad syntax");
        }
        // (The C says "mload" here.)
        run(&mut w, "wload mob 9999");
        log_is(&w, "mload: bad mob vnum");
        run(&mut w, "wload obj 9999");
        log_is(&w, "wload: bad object vnum");
        run(&mut w, "wload thing 1");
        log_is(&w, "wload: bad type");
    }

    #[test]
    fn wat_runs_a_command_as_another_room() {
        let mut w = World::new();
        let bob = w.player2;
        w.host.char_to_room(bob, w.room2);
        run(&mut w, "wat 3002 wecho Over here");
        assert_eq!(w.sent_to(bob), ["Over here\r\n"]);
        run(&mut w, "wat 3002 wlog from there");
        assert_eq!(w.log(), "Room 3002 :: from there");
        run(&mut w, "wat bob wlog by name");
        assert_eq!(w.log(), "Room 3002 :: by name");
        // Nested.
        run(&mut w, "wat 3002 wat 3001 wlog back");
        log_is(&w, "back");
        run(&mut w, "wat");
        log_is(&w, "wat called with no args");
        run(&mut w, "wat 3002");
        log_is(&w, "wat called without a command");
        run(&mut w, "wat 9999 wlog x");
        log_is(&w, "wat: location not found (9999)");
        run(&mut w, "wat zed wlog x");
        log_is(&w, "wat: location not found (zed)");
        // An unknown command is the other room's complaint.
        run(&mut w, "wat 3002 frob");
        assert_eq!(w.log(), "Room 3002 :: Unknown world cmd: 'frob'");
    }

    #[test]
    fn wat_accepts_the_nesting_a_c_line_can_hold() {
        let mut w = World::new();
        w.host.add_room(1, "A closet");
        let line = format!("{}wlog deep", "wat 1 ".repeat(70));
        assert!(line.len() < 512);
        run(&mut w, &line);
        assert_eq!(w.host.logs.len(), 1);
        assert_eq!(w.log(), "Room 1 :: deep");
    }

    #[test]
    fn wat_nesting_is_bounded() {
        let mut w = World::new();
        let line = format!("{}wlog deep", "wat 3001 ".repeat(300));
        run(&mut w, &line);
        log_is(&w, "wat: commands nested too deeply");
        assert_eq!(w.host.logs.len(), 1);
    }

    #[test]
    fn wmove_moves_objects_between_rooms() {
        let mut w = World::new();
        let (lamp, room, room2) = (w.obj, w.room, w.room2);
        let rock = w.host.add_obj(1, "a rock");
        w.host.set_text(rock, "alias", "rock");
        w.host.obj_to_room(rock, room);
        run(&mut w, "wmove rock 3002");
        assert_eq!(w.host.in_room(rock), Some(room2));
        assert_eq!(w.host.in_room(lamp), Some(room));
        run(&mut w, "wmove all 3002");
        assert_eq!(w.host.in_room(lamp), Some(room2));
        assert!(w.host.list(room, "contents").is_empty());
        assert_eq!(w.host.list(room2, "contents"), [rock, lamp]);
    }

    #[test]
    fn wmove_errors() {
        let mut w = World::new();
        run(&mut w, "wmove");
        log_is(&w, "wmove called with too few args");
        run(&mut w, "wmove lamp");
        log_is(&w, "wmove called with too few args");
        run(&mut w, "wmove lamp 9999");
        log_is(&w, "wmove target is an invalid room");
        run(&mut w, "wmove lamp 3001");
        log_is(&w, "wmove target room is itself");
        run(&mut w, "wmove all 3001");
        log_is(&w, "wmove target room is itself");
        run(&mut w, "wmove zed 3002");
        log_is(&w, "wmove: no target found");
        // An object found elsewhere in the world that is not lying in a room
        // is left alone.
        let ring = w.host.add_obj(1, "a ring");
        w.host.set_text(ring, "alias", "ring");
        w.host.obj_to_char(ring, w.player);
        w.clear();
        run(&mut w, "wmove ring 3002");
        assert_eq!(w.host.obj_carried_by(ring), Some(w.player));
        assert_eq!(
            w.effects("syslog"),
            ["syslog: SYSERR: NULL object or obj not in a room passed to obj_from_room"]
        );
    }

    // ---- hostile input --------------------------------------------------

    #[test]
    fn hostile_arguments_do_not_panic() {
        let mut w = World::new();
        let nasty = [
            String::new(),
            " ".to_owned(),
            "\u{e9}\u{e9}\u{e9}".to_owned(),
            "}".to_owned(),
            "}}}}999999999999999999999999".to_owned(),
            "-".to_owned(),
            "\\".to_owned(),
            "$".to_owned(),
            "~~~|||^^^&&&***```".to_owned(),
            "\u{1f600} ~\u{1f600} `\u{1f600}".to_owned(),
            "x".repeat(20_000),
            "99999999999999999999 99999999999999999999".to_owned(),
            "-2147483648 -2147483648 x".to_owned(),
        ];
        for (command, _) in WLD_CMD_INFO {
            let command = command.trim_end();
            for arg in &nasty {
                run(&mut w, &format!("{command} {arg}"));
                run(&mut w, &format!("{command} {arg} {arg} {arg}"));
                run(&mut w, &format!("{command} 3001 {arg} {arg}"));
                run(&mut w, &format!("{command} {arg} 3001 {arg}"));
            }
        }
    }
}
