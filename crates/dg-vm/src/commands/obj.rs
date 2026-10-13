//! `dg_objcmd.c`: the object script commands, `obj_cmd_info[]` and
//! `obj_command_interpreter`.
//!
//! The table entries end in a space (`"oecho "`) and the word is matched with
//! `strncmp(entry, word, strlen(word))`, in table order, so an abbreviation
//! selects the first entry it begins. The full name still reaches its own
//! entry (`oechoaround` is not a prefix of `"oecho "`), and a word longer than
//! an entry never matches it.

use super::comm::{TO_CHAR, TO_ROOM, for_each_occupant, sub_write};
use super::misc::script_damage;
use super::shared::{
    Owner, dir_count, door, exit_to_room, find_obj_target_room, load, obj_contains_obj, recho,
    starts_with_digit, teleport, zoneecho,
};
use super::strncmp_lookup;
use crate::cstr::{any_one_arg, atoi, is_number, one_argument, skip_spaces, str_eq, two_arguments};
use crate::host::DgHost;
use crate::lookup::{NUM_WEARS, get_char_by_obj, get_obj_by_obj, idx_vnum, obj_room};
use crate::types::Uid;

/// How many `oat` levels deep a command may nest (the C is bounded only by
/// the length of the line; 256 covers a
/// `MAX_INPUT_LENGTH` line of `wat 1 ` prefixes).
pub(crate) const MAX_NEST: u32 = 256;

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
                for_each_occupant(host, room, |host, ch| {
                    if host.valid_dg_target(ch, false) {
                        host.mud_command(ch, line);
                    }
                });
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
        host.obj_set_timer(obj, atoi(&arg));
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
    } else if let Some(new_obj) = host.cmd_read_object(idx_vnum(atoi(&arg))) {
        // A worn object is taken off and put back on at the same position.
        // `worn_on` is the position in the wearer's equipment holding `obj`.
        let worn = host.obj_worn_by(obj).and_then(|wearer| {
            (0..NUM_WEARS)
                .find(|pos| host.equipment(wearer, *pos) == Some(obj))
                .map(|pos| (wearer, pos))
        });
        if let Some((wearer, pos)) = worn {
            host.cmd_unequip_char(wearer, pos);
        }
        host.cmd_overwrite_obj(obj, new_obj);
        if let Some((wearer, pos)) = worn {
            host.cmd_equip_char(wearer, obj, pos);
        }
        host.cmd_extract_obj(new_obj);
    } else {
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
                    host.vm_state().owner_purged = true;
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
        for_each_occupant(host, rm, |host, ch| {
            if host.valid_dg_target(ch, true) {
                teleport(host, ch, target);
            }
        });
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

    for door in 0..dir_count(host) {
        if let Some(to) = exit_to_room(host, room, door)
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
        host.obj_set_val(obj, position as usize, new_value);
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
        host.real_room(idx_vnum(atoi(&arg)))
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
    let saved_purged = host.vm_state().owner_purged;
    host.vm_state().owner_purged = false;

    host.cmd_obj_to_room(object, loc);
    interpret_nested(host, object, command, nest + 1);

    // Wherever the command left it, the duplicate was made to carry one
    // command and nothing else. (Ids are never reused, so existence is
    // enough to tell it is still the same object.)
    if host.exists(object) {
        host.cmd_extract_obj(object);
    }

    host.vm_state().owner_purged = if host.exists(obj) { saved_purged } else { true };
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::commands::support::World;
    use crate::host::{BaseHost, WorldHost};
    use crate::types::uid_text;

    const PREFIX: &str = "Obj (a lamp, VNum 7000):: ";

    fn run(w: &mut World, line: &str) {
        let obj = w.obj;
        w.run_obj(obj, line);
    }

    fn log_is(w: &World, text: &str) {
        assert_eq!(w.log(), format!("{PREFIX}{text}"));
    }

    /// The lamp with nowhere to be.
    fn lost_lamp() -> World {
        let mut w = World::new();
        w.host.obj_from_anywhere(w.obj);
        w
    }

    // ---- echoes ---------------------------------------------------------

    #[test]
    fn oecho_reaches_the_room_of_the_object_wherever_it_is() {
        let mut w = World::new();
        let (alice, bob, lamp, room2) = (w.player, w.player2, w.obj, w.room2);
        run(&mut w, "oecho  Hello ~alice");
        // The room's head (Bob) is the speaker: TO_ROOM first, then TO_CHAR.
        assert_eq!(w.host.effects.len(), 2);
        assert_eq!(w.host.effects[0], format!("send {alice}: Hello you\n\r"));
        assert_eq!(w.host.effects[1], format!("send {bob}: Hello Alice\n\r"));
        // Carried: the carrier's room. Contained: the container's room.
        w.clear();
        w.host.obj_to_char(lamp, alice);
        run(&mut w, "oecho carried");
        assert_eq!(w.sent_to(bob), ["carried\n\r"]);
        w.clear();
        let sack = w.host.add_obj(7100, "a sack");
        w.host.obj_to_obj(lamp, sack);
        w.host.obj_to_room(sack, room2);
        w.host.char_to_room(bob, room2);
        run(&mut w, "oecho inside");
        assert_eq!(w.sent_to(bob), ["inside\n\r"]);
        assert!(w.sent_to(alice).is_empty());
    }

    #[test]
    fn oecho_errors() {
        let mut w = World::new();
        run(&mut w, "oecho");
        log_is(&w, "oecho called with no args");
        run(&mut w, "oecho    ");
        log_is(&w, "oecho called with no args");
        let mut w = lost_lamp();
        run(&mut w, "oecho hi");
        log_is(&w, "oecho called by object in NOWHERE");
        // An empty room is not an error: nobody to hear it.
        let mut w = World::new();
        let (room2, lamp) = (w.room2, w.obj);
        w.host.obj_to_room(lamp, room2);
        w.clear();
        run(&mut w, "oecho hi");
        assert!(w.host.effects.is_empty() && w.host.logs.is_empty());
    }

    #[test]
    fn osend_and_oechoaround() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        run(&mut w, "osend alice  Psst ~alice");
        assert_eq!(w.sent_to(alice), ["Psst you\n\r"]);
        assert!(w.sent_to(bob).is_empty());
        w.clear();
        run(&mut w, "oechoaround alice ~alice sneezes");
        assert_eq!(w.sent_to(bob), ["Alice sneezes\n\r"]);
        assert!(w.sent_to(alice).is_empty());
        // By id, and the target may be anywhere.
        w.clear();
        w.host.char_to_room(bob, w.room2);
        run(&mut w, &format!("osend {} hey", uid_text(bob)));
        assert_eq!(w.sent_to(bob), ["hey\n\r"]);
        run(&mut w, "osend");
        log_is(&w, "osend called with no args");
        run(&mut w, "osend alice");
        log_is(&w, "osend called without a message");
        run(&mut w, "osend alice   ");
        log_is(&w, "osend called without a message");
        run(&mut w, "osend zed hi");
        log_is(&w, "no target found for osend");
        run(&mut w, "oechoaround zed hi");
        log_is(&w, "no target found for osend");
    }

    #[test]
    fn oasound_speaks_into_the_neighbouring_rooms() {
        let mut w = World::new();
        let (alice, bob, room, room2) = (w.player, w.player2, w.room, w.room2);
        w.host.char_to_room(alice, room2);
        w.host.char_to_room(bob, room2);
        w.host.set_link(room, "exit:1", Some(room));
        w.host.set_link(room, "exit:2", None);
        run(&mut w, " oasound The wind ~alice");
        // Room 2's head is Bob: Alice (TO_ROOM) then Bob (TO_CHAR).
        assert_eq!(w.host.effects.len(), 2);
        assert_eq!(w.host.effects[0], format!("send {alice}: The wind you\n\r"));
        assert_eq!(w.host.effects[1], format!("send {bob}: The wind Alice\n\r"));
        run(&mut w, "oasound");
        log_is(&w, "oasound called with no args");
        let mut w = lost_lamp();
        run(&mut w, "oasound x");
        log_is(&w, "oasound called by object in NOWHERE");
        // A neighbour with nobody in it is skipped.
        let mut w = World::new();
        run(&mut w, "oasound x");
        assert!(w.host.effects.is_empty() && w.host.logs.is_empty());
    }

    #[test]
    fn oasound_skips_an_exit_back_into_the_objects_own_room() {
        let mut w = World::new();
        let (alice, bob, room, room2) = (w.player, w.player2, w.room, w.room2);
        w.host.char_to_room(bob, room2);
        w.host.set_link(room, "exit:1", Some(room));
        w.host.set_link(room, "exit:2", Some(room2));
        run(&mut w, "oasound Hush");
        // The room echo is a TO_ROOM and a TO_CHAR for the head of the room.
        assert_eq!(w.sent_to(bob), ["Hush\n\r", "Hush\n\r"]);
        assert!(w.sent_to(alice).is_empty());
    }

    #[test]
    fn olog_and_orecho_and_ozoneecho() {
        let mut w = World::new();
        let (alice, bob, room) = (w.player, w.player2, w.room);
        run(&mut w, "olog");
        run(&mut w, "olog   ");
        assert!(w.host.logs.is_empty());
        run(&mut w, "olog  50% done");
        log_is(&w, "50% done");

        run(&mut w, "orecho 3001 3002 Listen");
        assert_eq!(w.sent_to(alice), ["Listen\r\n"]);
        assert_eq!(w.sent_to(bob), ["Listen\r\n"]);
        run(&mut w, "orecho 3001 3002");
        log_is(&w, "orecho: too few args");
        run(&mut w, "orecho x 3002 hi");
        log_is(&w, "orecho: too few args");

        w.host.globals.nums.insert("zone:3:bot".to_owned(), 3000);
        w.host.globals.nums.insert("zone:3:top".to_owned(), 3099);
        w.host.set_num(room, "zone", 3);
        w.clear();
        run(&mut w, "ozoneecho 3001 Tremor");
        assert_eq!(w.sent_to(alice), ["Tremor\r\n"]);
        run(&mut w, "ozoneecho 3001");
        log_is(&w, "ozoneecho called with too few args");
        run(&mut w, "ozoneecho 5000 x");
        log_is(&w, "ozoneecho called for nonexistant zone");
    }

    // ---- forcing and violence -------------------------------------------

    #[test]
    fn oforce_one_or_all() {
        let mut w = World::new();
        let (alice, bob, mob) = (w.player, w.player2, w.mob);
        run(&mut w, "oforce alice say hi");
        assert_eq!(w.host.mud_commands, [(alice, " say hi".to_owned())]);
        w.host.mud_commands.clear();
        run(&mut w, "oforce all wave");
        assert_eq!(
            w.host.mud_commands,
            [
                (bob, " wave".to_owned()),
                (alice, " wave".to_owned()),
                (mob, " wave".to_owned())
            ]
        );
        // Untargetable characters are skipped.
        w.host.mud_commands.clear();
        w.host.set_num(alice, "dg_invalid", 1);
        run(&mut w, "oforce alice wave");
        run(&mut w, "oforce all wave");
        assert_eq!(w.host.mud_commands.len(), 2);
    }

    #[test]
    fn oforce_errors() {
        let mut w = World::new();
        run(&mut w, "oforce");
        log_is(&w, "oforce called with too few args");
        run(&mut w, "oforce alice");
        log_is(&w, "oforce called with too few args");
        run(&mut w, "oforce zed wave");
        log_is(&w, "oforce: no target found");
        let mut w = lost_lamp();
        run(&mut w, "oforce all wave");
        log_is(&w, "oforce called by object in NOWHERE");
        assert!(w.host.mud_commands.is_empty());
    }

    #[test]
    fn odamage_hurts() {
        let mut w = World::new();
        let alice = w.player;
        run(&mut w, "odamage alice 4");
        assert_eq!(w.host.num(alice, "hit"), 16);
        run(&mut w, "odamage alice zz");
        assert_eq!(w.host.num(alice, "hit"), 16);
        run(&mut w, "odamage alice");
        log_is(&w, "odamage: bad syntax");
        run(&mut w, "odamage zed 4");
        log_is(&w, "odamage: target not found");
    }

    // ---- values ---------------------------------------------------------

    #[test]
    fn otimer_sets_the_timer() {
        let mut w = World::new();
        let lamp = w.obj;
        run(&mut w, "otimer 15");
        assert_eq!(w.host.num(lamp, "timer"), 15);
        run(&mut w, "otimer 7x");
        assert_eq!(w.host.num(lamp, "timer"), 7);
        run(&mut w, "otimer");
        log_is(&w, "otimer: missing argument");
        run(&mut w, "otimer -3");
        log_is(&w, "otimer: bad argument");
        run(&mut w, "otimer soon");
        log_is(&w, "otimer: bad argument");
        assert_eq!(w.host.num(lamp, "timer"), 7);
    }

    #[test]
    fn osetval_sets_a_value_and_re_equips_a_worn_object() {
        let mut w = World::new();
        let (lamp, alice) = (w.obj, w.player);
        run(&mut w, "osetval 0 5");
        run(&mut w, "osetval 3 -7");
        assert_eq!(w.host.num(lamp, "val0"), 5);
        assert_eq!(w.host.num(lamp, "val3"), -7);
        // Worn: still worn in the same place afterwards.
        w.host.equip_char(alice, lamp, 3);
        run(&mut w, "osetval 1 9");
        assert_eq!(w.host.num(lamp, "val1"), 9);
        assert_eq!(w.host.equipment(alice, 3), Some(lamp));
        for line in [
            "osetval",
            "osetval 1",
            "osetval a 1",
            "osetval 1 b",
            "osetval 1.5 2",
        ] {
            run(&mut w, line);
            log_is(&w, "osetval: bad syntax");
        }
        run(&mut w, "osetval 4 1");
        log_is(&w, "osetval: position out of bounds!");
        run(&mut w, "osetval -1 1");
        log_is(&w, "osetval: position out of bounds!");
    }

    #[test]
    fn otransform() {
        let mut w = World::new();
        let lamp = w.obj;
        run(&mut w, "otransform 7102");
        assert_eq!(
            w.effects("transform_obj"),
            [format!("transform_obj {lamp} 7102")]
        );
        // The lamp has become a rock, and logs as one.
        let rock = "Obj (a rock, VNum 7102):: ";
        run(&mut w, "otransform");
        assert_eq!(w.log(), format!("{rock}otransform: missing argument"));
        run(&mut w, "otransform -7102");
        assert_eq!(w.log(), format!("{rock}otransform: bad argument"));
        run(&mut w, "otransform x");
        assert_eq!(w.log(), format!("{rock}otransform: bad argument"));
        run(&mut w, "otransform 9999");
        assert_eq!(w.log(), format!("{rock}otransform: bad object vnum"));
    }

    // ---- purging --------------------------------------------------------

    #[test]
    fn opurge_with_no_argument_spares_the_object_and_what_holds_it() {
        let mut w = World::new();
        let (lamp, room, mob, alice) = (w.obj, w.room, w.mob, w.player);
        let rock = w.host.add_obj(1, "a rock");
        w.host.obj_to_room(rock, room);
        let sack = w.host.add_obj(7100, "a sack");
        w.host.obj_to_room(sack, room);
        w.host.obj_to_obj(lamp, sack);
        run(&mut w, "opurge");
        assert!(!w.host.exists(rock));
        assert!(w.host.exists(sack) && w.host.exists(lamp));
        assert!(w.host.is_dead(mob) && !w.host.is_dead(alice));
        assert!(!w.host.vm.owner_purged);
        // The same with the object on the floor: it keeps itself.
        let mut w = World::new();
        let (lamp, room) = (w.obj, w.room);
        let rock = w.host.add_obj(1, "a rock");
        w.host.obj_to_room(rock, room);
        run(&mut w, "opurge");
        assert!(w.host.exists(lamp) && !w.host.exists(rock));
        // In nowhere there is nothing to purge.
        let mut w = lost_lamp();
        run(&mut w, "opurge");
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn opurge_named_things() {
        let mut w = World::new();
        let (lamp, room, mob, alice) = (w.obj, w.room, w.mob, w.player);
        run(&mut w, "opurge guard");
        assert!(w.host.is_dead(mob));
        assert!(!w.host.vm.owner_purged);
        run(&mut w, "opurge alice");
        log_is(&w, "opurge: purging a PC");
        assert!(!w.host.is_dead(alice));
        run(&mut w, "opurge zed");
        log_is(&w, "opurge: bad argument");
        // Another object: no flag.
        let rock = w.host.add_obj(1, "a rock");
        w.host.obj_to_room(rock, room);
        run(&mut w, "opurge rock");
        assert!(!w.host.exists(rock) && !w.host.vm.owner_purged);
        // Itself: the flag.
        run(&mut w, "opurge lamp");
        assert!(!w.host.exists(lamp) && w.host.vm.owner_purged);
    }

    #[test]
    fn opurge_of_a_container_holding_the_object_stops_the_script() {
        let mut w = World::new();
        let (lamp, room) = (w.obj, w.room);
        let sack = w.host.add_obj(7100, "a sack");
        w.host.set_text(sack, "alias", "sack");
        w.host.obj_to_room(sack, room);
        w.host.obj_to_obj(lamp, sack);
        run(&mut w, "opurge sack");
        assert!(!w.host.exists(sack) && !w.host.exists(lamp));
        assert!(w.host.vm.owner_purged);
    }

    // ---- teleporting ----------------------------------------------------

    #[test]
    fn oteleport_one_or_all() {
        let mut w = World::new();
        let (alice, bob, mob, room, room2) = (w.player, w.player2, w.mob, w.room, w.room2);
        run(&mut w, "oteleport alice 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));
        run(&mut w, "oteleport all 3002");
        assert_eq!(w.host.in_room(bob), Some(room2));
        assert_eq!(w.host.in_room(mob), Some(room2));
        // A person or object may name the room.
        run(&mut w, "oteleport alice bob");
        assert_eq!(w.host.in_room(alice), Some(room2));
        w.host.char_to_room(alice, room);
        run(&mut w, "oteleport bob alice");
        assert_eq!(w.host.in_room(bob), Some(room));
        run(&mut w, "oteleport bob lamp");
        assert_eq!(w.host.in_room(bob), Some(room));
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn oforce_refuses_the_highest_gods_and_oteleport_does_not() {
        // A level 33 player is found by the lookup (it allows gods), refused
        // by `oforce` (`valid_dg_target(ch, 0)`) and accepted by
        // `oteleport` (`DG_ALLOW_GODS`), in the single and the `all` form.
        let mut w = World::new();
        let (alice, bob, mob, room2) = (w.player, w.player2, w.mob, w.room2);
        w.host.set_num(alice, "level", 33);
        run(&mut w, "oforce alice look");
        assert!(w.host.mud_commands.is_empty());
        run(&mut w, "oforce all look");
        assert_eq!(
            w.host.mud_commands,
            [(bob, " look".to_owned()), (mob, " look".to_owned())]
        );
        run(&mut w, "oteleport alice 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));

        let mut w = World::new();
        let (alice, bob, room2) = (w.player, w.player2, w.room2);
        w.host.set_num(alice, "level", 33);
        run(&mut w, "oteleport all 3002");
        assert_eq!(w.host.in_room(alice), Some(room2));
        assert_eq!(w.host.in_room(bob), Some(room2));
    }

    #[test]
    fn oforce_and_oteleport_skip_a_god_with_nohassle() {
        let mut w = World::new();
        let (alice, bob, mob, room) = (w.player, w.player2, w.mob, w.room);
        w.host.set_num(alice, "level", 32);
        run(&mut w, "oforce alice look");
        assert_eq!(w.host.mud_commands, [(alice, " look".to_owned())]);
        w.host.mud_commands.clear();
        w.set_nohassle(alice, true);
        run(&mut w, "oforce alice look");
        run(&mut w, "oforce all look");
        assert_eq!(
            w.host.mud_commands,
            [(bob, " look".to_owned()), (mob, " look".to_owned())]
        );
        run(&mut w, "oteleport alice 3002");
        run(&mut w, "oteleport all 3002");
        assert_eq!(w.host.in_room(alice), Some(room));
        assert_eq!(w.host.in_room(bob), Some(w.room2));
    }

    #[test]
    fn oteleport_errors() {
        let mut w = World::new();
        run(&mut w, "oteleport");
        log_is(&w, "oteleport called with too few args");
        run(&mut w, "oteleport alice");
        log_is(&w, "oteleport called with too few args");
        run(&mut w, "oteleport alice 9999");
        log_is(&w, "oteleport target is an invalid room");
        run(&mut w, "oteleport zed 3002");
        log_is(&w, "oteleport: no target found");
        run(&mut w, "oteleport all 3001");
        log_is(&w, "oteleport target is itself");
        // Refused rooms count as invalid.
        w.host.set_num(w.room2, "flag:10", 1);
        run(&mut w, "oteleport alice 3002");
        log_is(&w, "oteleport target is an invalid room");
        let mut w = lost_lamp();
        run(&mut w, "oteleport all 3002");
        log_is(&w, "oteleport called by an object that is nowhere");
        run(&mut w, "oteleport alice 3002");
        assert_eq!(w.host.in_room(w.player), Some(w.room2));
    }

    #[test]
    fn target_rooms_are_refused_for_gods_houses_and_company() {
        let mut w = World::new();
        let (lamp, room2, alice, bob) = (w.obj, w.room2, w.player, w.player2);
        for flag in [10, 11] {
            w.host.set_num(room2, &format!("flag:{flag}"), 1);
            assert_eq!(find_obj_target_room(&w.host, lamp, "3002"), None);
            w.host.set_num(room2, &format!("flag:{flag}"), 0);
        }
        assert_eq!(find_obj_target_room(&w.host, lamp, "3002"), Some(room2));
        // Private: one occupant is fine, two are not.
        w.host.set_num(room2, "flag:9", 1);
        w.host.char_to_room(alice, room2);
        assert_eq!(find_obj_target_room(&w.host, lamp, "3002"), Some(room2));
        w.host.char_to_room(bob, room2);
        assert_eq!(find_obj_target_room(&w.host, lamp, "3002"), None);
        // Named by a character, by an object lying about, never by one that is
        // carried; empty and unknown names find nothing.
        assert_eq!(find_obj_target_room(&w.host, lamp, "alice"), None);
        w.host.set_num(room2, "flag:9", 0);
        assert_eq!(find_obj_target_room(&w.host, lamp, "alice"), Some(room2));
        assert_eq!(find_obj_target_room(&w.host, lamp, ""), None);
        assert_eq!(find_obj_target_room(&w.host, lamp, "zed"), None);
        assert_eq!(find_obj_target_room(&w.host, lamp, "3.002"), None);
        assert_eq!(find_obj_target_room(&w.host, lamp, "9999"), None);
        let held = w.host.add_obj(1, "a ring");
        w.host.obj_to_char(held, alice);
        assert_eq!(find_obj_target_room(&w.host, lamp, "ring"), None);
        // A character in no room has no room to name.
        w.host.char_from_room(alice);
        assert_eq!(find_obj_target_room(&w.host, lamp, "alice"), None);
    }

    #[test]
    fn omove_moves_the_object() {
        let mut w = World::new();
        let (lamp, room2, alice) = (w.obj, w.room2, w.player);
        run(&mut w, "omove 3002");
        assert_eq!(w.host.in_room(lamp), Some(room2));
        assert!(w.host.list(w.room, "contents").is_empty());
        // Carried, then contained.
        w.host.obj_to_char(lamp, alice);
        run(&mut w, "omove 3002");
        assert_eq!(w.host.in_room(lamp), Some(room2));
        assert!(w.host.list(alice, "inventory").is_empty());
        let sack = w.host.add_obj(7100, "a sack");
        w.host.obj_to_obj(lamp, sack);
        run(&mut w, "omove 3001");
        assert_eq!(w.host.in_room(lamp), Some(w.room));
        assert!(w.host.list(sack, "contents").is_empty());
        // To where a character is.
        run(&mut w, "omove alice");
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn omove_errors() {
        let mut w = World::new();
        let (lamp, alice) = (w.obj, w.player);
        run(&mut w, "omove");
        log_is(&w, "omove called with too few args");
        run(&mut w, "omove 9999");
        log_is(&w, "omove target is an invalid room");
        // Worn is none of the three: refused.
        w.host.equip_char(alice, lamp, 3);
        run(&mut w, "omove 3002");
        log_is(
            &w,
            "omove: target object is not in a room, held or in a container!",
        );
        assert_eq!(w.host.equipment(alice, 3), Some(lamp));
        let mut w = lost_lamp();
        run(&mut w, "omove 3002");
        log_is(
            &w,
            "omove: target object is not in a room, held or in a container!",
        );
    }

    // ---- loading --------------------------------------------------------

    #[test]
    fn oload_puts_things_in_the_room_of_the_object() {
        let mut w = World::new();
        let (lamp, room) = (w.obj, w.room);
        run(&mut w, "oload mob 3100");
        let rat = w.host.list(room, "people")[0];
        assert_eq!(w.host.vnum(rat), 3100);
        assert_eq!(
            w.host
                .script(lamp)
                .unwrap()
                .global_vars
                .get("lastloaded")
                .unwrap()
                .value,
            uid_text(rat)
        );
        run(&mut w, "oload obj 7101");
        let ring = *w.host.list(room, "contents").last().unwrap();
        assert_eq!(w.host.vnum(ring), 7101);
        assert_eq!(
            w.host
                .script(lamp)
                .unwrap()
                .global_vars
                .get("lastloaded")
                .unwrap()
                .value,
            uid_text(ring)
        );
        // Carried by someone, the object loads where they are.
        let (alice, room2) = (w.player, w.room2);
        w.host.char_to_room(alice, room2);
        w.host.obj_to_char(lamp, alice);
        run(&mut w, "oload mob 3100");
        assert_eq!(w.host.list(room2, "people").len(), 2);
    }

    #[test]
    fn oload_onto_characters_and_into_containers() {
        let mut w = World::new();
        let (room, alice) = (w.room, w.player);
        run(&mut w, "oload obj 7101 alice");
        assert_eq!(w.host.list(alice, "inventory").len(), 1);
        run(&mut w, "oload obj 7101 alice lfinger");
        assert!(w.host.equipment(alice, 2).is_some());
        let sack = w.host.add_obj(7100, "a sack");
        w.host.set_text(sack, "alias", "sack");
        w.host.set_num(sack, "type", 15);
        w.host.obj_to_room(sack, room);
        run(&mut w, "oload obj 7102 sack");
        assert_eq!(w.host.list(sack, "contents").len(), 1);
        // Neither: the room.
        let before = w.host.list(room, "contents").len();
        run(&mut w, "oload obj 7102 nothing");
        assert_eq!(w.host.list(room, "contents").len(), before + 1);
    }

    #[test]
    fn oload_errors() {
        let mut w = World::new();
        for line in ["oload", "oload mob", "oload mob x", "oload mob -2"] {
            run(&mut w, line);
            log_is(&w, "oload: bad syntax");
        }
        run(&mut w, "oload mob 9999");
        log_is(&w, "oload: bad mob vnum");
        run(&mut w, "oload obj 9999");
        log_is(&w, "oload: bad object vnum");
        run(&mut w, "oload thing 1");
        log_is(&w, "oload: bad type");
        run(&mut w, "oload mob 3100 3002");
        log_is(
            &w,
            "oload: room target vnum doesn't exist (loading mob vnum 3100 to room  3002)",
        );
        // Syntax is checked before the object's room.
        let mut w = lost_lamp();
        run(&mut w, "oload mob");
        log_is(&w, "oload: bad syntax");
        run(&mut w, "oload mob 3100");
        log_is(&w, "oload: object in NOWHERE trying to load");
    }

    // ---- doors ----------------------------------------------------------

    #[test]
    fn odoor_uses_the_object_wording() {
        let mut w = World::new();
        let room = w.room;
        run(&mut w, "odoor 3001 north key 9");
        assert_eq!(w.host.num(room, "exit:0:key"), 9);
        run(&mut w, "odoor 3001");
        log_is(&w, "odoor called with too few args");
        run(&mut w, "odoor 1 n purge");
        log_is(&w, "odoor: invalid target (arg == 1)");
        run(&mut w, "odoor 3001 q purge");
        assert!(w.log().starts_with(&format!(
            "{PREFIX}odoor: invalid direction (arg == q) not found in: [ north"
        )));
        run(&mut w, "odoor 3001 n q");
        log_is(
            &w,
            "odoor: invalid field (arg == q) not found in: [ purge description flags key name room ]",
        );
        run(&mut w, "odoor 3001 n room 1");
        log_is(&w, "odoor: invalid door target (arg == 1)");
    }

    // ---- at -------------------------------------------------------------

    #[test]
    fn oat_runs_a_command_from_a_copy_in_another_room() {
        let mut w = World::new();
        let (alice, bob, lamp, room2) = (w.player, w.player2, w.obj, w.room2);
        w.host.char_to_room(bob, room2);
        let objects_before = w.host.object_list.len();
        run(&mut w, "oat 3002 oecho Over here");
        // Bob is the only one in room 2: oecho goes TO_ROOM (nobody) then
        // TO_CHAR (him).
        assert_eq!(w.sent_to(bob), ["Over here\n\r"]);
        assert!(w.sent_to(alice).is_empty());
        // The copy was removed again.
        assert_eq!(w.host.object_list.len(), objects_before);
        assert!(w.host.exists(lamp));
        assert!(w.host.logs.is_empty());
        // By the room of a character.
        w.clear();
        run(&mut w, "oat bob oecho By name");
        assert_eq!(w.sent_to(bob), ["By name\n\r"]);
    }

    #[test]
    fn oat_errors() {
        let mut w = World::new();
        run(&mut w, "oat");
        log_is(&w, "oat called with no args");
        run(&mut w, "oat 3002");
        log_is(&w, "oat called without a command");
        run(&mut w, "oat 3002    ");
        log_is(&w, "oat called without a command");
        run(&mut w, "oat 9999 oecho x");
        log_is(&w, "oat: location not found (9999)");
        run(&mut w, "oat zed oecho x");
        log_is(&w, "oat: location not found (zed)");
        // Without a prototype there is nothing to copy.
        w.host.globals.texts.remove("objproto:7000");
        let n = w.host.logs.len();
        run(&mut w, "oat 3002 oecho x");
        assert_eq!(w.host.logs.len(), n);
    }

    #[test]
    fn oat_keeps_the_callers_purge_flag_unless_the_owner_is_gone() {
        let mut w = World::new();
        let lamp = w.obj;
        // The copy purges itself: the caller's flag is as it was.
        run(&mut w, "oat 3002 opurge lamp");
        assert!(!w.host.vm.owner_purged);
        w.host.vm.owner_purged = true;
        run(&mut w, "oat 3002 opurge lamp");
        assert!(w.host.vm.owner_purged);
        w.host.vm.owner_purged = false;
        // The command purges the real object: the script must stop.
        run(&mut w, &format!("oat 3002 opurge {}", uid_text(lamp)));
        assert!(!w.host.exists(lamp));
        assert!(w.host.vm.owner_purged);
    }

    #[test]
    fn oat_accepts_the_nesting_a_c_line_can_hold() {
        let mut w = World::new();
        w.host.add_room(1, "A closet");
        let line = format!("{}olog deep", "oat 1 ".repeat(70));
        assert!(line.len() < 512);
        run(&mut w, &line);
        log_is(&w, "deep");
        assert_eq!(w.host.logs.len(), 1);
        assert_eq!(w.host.object_list.len(), 1);
    }

    #[test]
    fn oat_nesting_is_bounded() {
        let mut w = World::new();
        let line = format!("{}olog deep", "oat 3001 ".repeat(300));
        run(&mut w, &line);
        log_is(&w, "oat: commands nested too deeply");
        assert_eq!(w.host.logs.len(), 1);
        // And the copies were all removed.
        assert_eq!(w.host.object_list.len(), 1);
        // A shallow chain works.
        w.clear();
        run(&mut w, "oat 3001 oat 3002 olog here");
        log_is(&w, "here");
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
            "~~~|||^^^&&&***```".to_owned(),
            "\u{1f600} ~\u{1f600} `\u{1f600}".to_owned(),
            "x".repeat(20_000),
            "99999999999999999999 99999999999999999999".to_owned(),
            "-2147483648 -2147483648 x".to_owned(),
            "2147483648.bob".to_owned(),
            "2147483648.lamp".to_owned(),
        ];
        for (command, _) in OBJ_CMD_INFO {
            let command = command.trim_end();
            for arg in &nasty {
                run(&mut w, &format!("{command} {arg}"));
                run(&mut w, &format!("{command} {arg} {arg} {arg}"));
                run(&mut w, &format!("{command} 3001 {arg} {arg}"));
                run(&mut w, &format!("{command} {arg} 3001 {arg}"));
            }
        }
    }

    #[test]
    fn otransform_re_equips_a_worn_object() {
        let mut w = World::new();
        let (lamp, alice) = (w.obj, w.player);
        w.host.equip_char(alice, lamp, 16);
        run(&mut w, "otransform 7102");
        assert_eq!(w.host.equipment(alice, 16), Some(lamp));
        assert_eq!(w.host.vnum(lamp), 7102);
        assert_eq!(w.effects("extract_obj").len(), 1);
    }

    // ---- branches the earlier tests left unpinned --------------------------

    #[test]
    fn osend_reaches_a_sleeping_victim() {
        let mut w = World::new();
        let bob = w.player2;
        w.host.set_num(bob, "pos", 4);
        run(&mut w, "osend bob hi");
        assert_eq!(w.sent_to(bob), ["hi\n\r"]);
    }

    #[test]
    fn oteleport_reads_a_dotted_room_word_as_a_name() {
        // `isdigit(*roomstr) && !strchr(roomstr, '.')` is false for `3.002`,
        // so it is looked up as the 3rd thing named `002`, which is nothing,
        // and not as room 3.
        let mut w = World::new();
        let (alice, room) = (w.player, w.room);
        let three = w.host.add_room(3, "Nowhere Much");
        run(&mut w, "oteleport alice 3.002");
        log_is(&w, "oteleport target is an invalid room");
        assert_eq!(w.host.in_room(alice), Some(room));
        run(&mut w, "oteleport alice 3");
        assert_eq!(w.host.in_room(alice), Some(three));
    }

    #[test]
    fn otransform_takes_a_worn_object_off_before_the_copy() {
        let mut w = World::new();
        let (lamp, alice) = (w.obj, w.player);
        w.host.equip_char(alice, lamp, 16);
        run(&mut w, "otransform 7102");
        assert!(w.effects("transform_obj_while_worn").is_empty());
        assert_eq!(w.host.equipment(alice, 16), Some(lamp));
    }
}
