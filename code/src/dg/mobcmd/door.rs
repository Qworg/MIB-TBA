//! Doors: `open`, `close`, `unlock`, `lock` and `pick` (`do_gen_door` of
//! `act.movement.c`), over the room's [`Doors`] prototypes and the runtime
//! [`RoomState`] the movement gate reads.
//!
//! A door in the pack lives on each side of an exit separately, as the C
//! stores it, so a change is written to both sides when the far side leads
//! back. Containers are not doors here: the pack keeps no per-instance
//! open/locked state, so a closeable container reports the gap instead of
//! pretending.

use dg_vm::cstr::{is_abbrev, search_block, two_arguments};
use dg_vm::dispatch::{door_mtrigger, door_wtrigger};
use dg_vm::lookup::{WEAR_HOLD, get_obj_in_list_vis, isname};
use dg_vm::types::ITEM_CONTAINER;
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_core::prelude::Entity;
use mib_stdlib::prelude::{Door, DoorState, Doors, Exits, Room, RoomId, RoomState};

use super::act::{ActObj, ActVict, Flags, To, act_full, an, fname};
use super::table::DoorCmd;
use super::world::{DIR_COUNT, DIRS, exit_key, rev_dir, room_of, to_char};
use crate::content::TbaItemKind;
use crate::dg::host::EcsHost;
use crate::dg::ids::vnum_from_id;

/// `autoexits[]`: the short direction names.
const AUTOEXITS: [&str; 10] = ["n", "e", "s", "w", "u", "d", "nw", "ne", "se", "sw"];

/// `LVL_IMMORT`.
const LVL_IMMORT: i32 = 31;

/// `LVL_GOD`.
const LVL_GOD: i32 = 32;

/// `CONT_CLOSEABLE`, the bit of a container's second value.
const CONT_CLOSEABLE: i32 = 1;

/// One exit of a room as the door commands see it.
#[derive(Debug, Clone)]
pub struct ExitView {
    /// The room it leads to.
    pub to: Entity,
    /// The door on it, if it has one.
    pub door: Option<DoorView>,
}

/// A door's prototype data and current state.
#[derive(Debug, Clone)]
pub struct DoorView {
    pub proto: Door,
    pub closed: bool,
    pub locked: bool,
}

/// `EXIT(ch, dir)`: the exit of `room` in direction `dir` (the C's number).
pub fn exit_of(host: &EcsHost<'_>, room: Entity, dir: usize) -> Option<ExitView> {
    let key = exit_key(dir)?;
    let to = *host.world.get::<Exits>(room)?.map.get(&key)?;
    host.world.get::<Room>(to)?;
    let door = host
        .world
        .get::<Doors>(room)
        .and_then(|doors| doors.by_dir.get(&key))
        .map(|proto| {
            let state = host
                .world
                .get::<RoomState>(room)
                .and_then(|state| state.doors.get(&key));
            DoorView {
                proto: proto.clone(),
                closed: state.map_or(proto.default_closed, |state| state.closed),
                locked: state.map_or(proto.default_locked, |state| state.locked),
            }
        });
    Some(ExitView { to, door })
}

/// The exit on the far side of `dir` that leads back to `room`
/// (`world[to].dir_option[rev_dir[dir]]` when its `to_room` is `room`).
fn back_exit(host: &EcsHost<'_>, room: Entity, dir: usize, exit: &ExitView) -> Option<ExitView> {
    let back = exit_of(host, exit.to, rev_dir(dir))?;
    (back.to == room).then_some(back)
}

/// Write the state of the door at `dir` of `room`, creating the runtime entry
/// from the prototype when the room has none.
fn set_door_state(
    host: &mut EcsHost<'_>,
    room: Entity,
    dir: usize,
    change: impl FnOnce(&mut DoorState),
) {
    let Some(key) = exit_key(dir) else {
        return;
    };
    let Some(view) = exit_of(host, room, dir) else {
        return;
    };
    let Some(door) = view.door else {
        return;
    };
    let target = host
        .world
        .get::<Room>(view.to)
        .map(|room| RoomId(room.key.clone()));
    if host.world.get::<RoomState>(room).is_none() {
        host.world.entity_mut(room).insert(RoomState::default());
    }
    if let Some(mut state) = host.world.get_mut::<RoomState>(room) {
        let entry = state.doors.entry(key).or_insert(DoorState {
            closed: door.proto.default_closed,
            locked: door.proto.default_locked,
            to: target,
        });
        change(entry);
    }
}

/// `has_key(ch, key)`: the key is carried or held.
fn has_key(host: &EcsHost<'_>, ch: Uid, key: Option<&Door>) -> bool {
    let Some(vnum) = key
        .and_then(|door| door.key.as_ref())
        .and_then(|key| vnum_from_id(key.as_ref()))
    else {
        return false;
    };
    host.carrying(ch)
        .into_iter()
        .chain(host.equipment(ch, WEAR_HOLD))
        .any(|obj| host.vnum(obj) == vnum)
}

/// `find_door`: the direction a door command means, or `None` after telling
/// the character why not.
fn find_door(
    host: &mut EcsHost<'_>,
    ch: Uid,
    room: Entity,
    kind: &str,
    dir: &str,
    cmdname: &str,
) -> Option<usize> {
    if !dir.is_empty() {
        let door = search_block(dir, &DIRS, false).or_else(|| search_block(dir, &AUTOEXITS, false));
        let Some(door) = door else {
            to_char(host, ch, "That's not a direction.\r\n");
            return None;
        };
        let Some(exit) = exit_of(host, room, door) else {
            to_char(
                host,
                ch,
                &format!("I really don't see how you can {cmdname} anything there.\r\n"),
            );
            return None;
        };
        return match exit.door.and_then(|door| door.proto.name) {
            Some(keyword) if !isname(kind, &keyword) => {
                to_char(host, ch, &format!("I see no {kind} there.\r\n"));
                None
            }
            _ => Some(door),
        };
    }
    if kind.is_empty() {
        to_char(host, ch, &format!("What is it you want to {cmdname}?\r\n"));
        return None;
    }
    let npc = host.is_npc(ch);
    for door in 0..DIR_COUNT {
        let Some(view) = exit_of(host, room, door).and_then(|exit| exit.door) else {
            continue;
        };
        let Some(keyword) = view.proto.name.as_deref() else {
            continue;
        };
        if !isname(kind, keyword) {
            continue;
        }
        // Players without autodoor get the door whatever its state; the pack
        // has no autodoor preference, so only the NPC branch applies.
        let wanted = if is_abbrev(cmdname, "open") {
            view.closed || view.locked
        } else if is_abbrev(cmdname, "close") {
            !view.closed
        } else if is_abbrev(cmdname, "lock") {
            !view.locked
        } else if is_abbrev(cmdname, "unlock") || is_abbrev(cmdname, "pick") {
            view.locked
        } else {
            false
        };
        if !npc || wanted {
            return Some(door);
        }
    }
    let an = an(kind);
    let tail = if is_abbrev(cmdname, "open") {
        "opened"
    } else if is_abbrev(cmdname, "close") {
        "closed"
    } else if is_abbrev(cmdname, "lock") {
        "locked"
    } else if is_abbrev(cmdname, "unlock") {
        "unlocked"
    } else {
        "picked"
    };
    to_char(
        host,
        ch,
        &format!("There doesn't seem to be {an} {kind} that can be {tail}.\r\n"),
    );
    None
}

/// The door commands' names, in `cmd_door[]` order.
fn cmd_name(cmd: DoorCmd) -> &'static str {
    match cmd {
        DoorCmd::Open => "open",
        DoorCmd::Close => "close",
        DoorCmd::Unlock => "unlock",
        DoorCmd::Lock => "lock",
        DoorCmd::Pick => "pick",
    }
}

/// `SCMD_OPEN`..`SCMD_PICK`, as the door triggers number them.
fn subcmd(cmd: DoorCmd) -> i32 {
    match cmd {
        DoorCmd::Open => 0,
        DoorCmd::Close => 1,
        DoorCmd::Unlock => 2,
        DoorCmd::Lock => 3,
        DoorCmd::Pick => 4,
    }
}

/// `do_doorcmd` for an exit door.
fn do_doorcmd(host: &mut EcsHost<'_>, ch: Uid, room: Entity, door: usize, cmd: DoorCmd) {
    if door_mtrigger(host, ch, subcmd(cmd), door as i32) == 0 {
        return;
    }
    if door_wtrigger(host, ch, subcmd(cmd), door as i32) == 0 {
        return;
    }
    let Some(exit) = exit_of(host, room, door) else {
        return;
    };
    let back = back_exit(host, room, door, &exit);
    let sides = [
        Some((room, door)),
        back.as_ref().map(|_| (exit.to, rev_dir(door))),
    ];
    for (side_room, side_dir) in sides.into_iter().flatten() {
        set_door_state(host, side_room, side_dir, |state| match cmd {
            DoorCmd::Open => state.closed = false,
            DoorCmd::Close => state.closed = true,
            DoorCmd::Lock => state.locked = true,
            DoorCmd::Unlock => state.locked = false,
            DoorCmd::Pick => state.locked = !state.locked,
        });
    }
    // Only the acting character hears the confirmation, and an NPC has no
    // session, so the "Okay." / "*Click*" lines are not produced.
    let keyword = exit.door.as_ref().and_then(|door| door.proto.name.clone());
    let head = if cmd == DoorCmd::Pick {
        "$n skillfully picks the lock on ".to_owned()
    } else {
        format!("$n {}s ", cmd_name(cmd))
    };
    let what = if keyword.is_some() { "$F" } else { "door" };
    let line = format!("{head}the {what}.");
    act_full(
        host,
        &line,
        false,
        Some(ch),
        ActObj::None,
        match keyword.as_deref() {
            Some(keyword) => ActVict::Text(keyword),
            None => ActVict::None,
        },
        To::Room,
        Flags::default(),
    );
    if back.is_some() && matches!(cmd, DoorCmd::Open | DoorCmd::Close) {
        let name = back
            .as_ref()
            .and_then(|back| back.door.as_ref())
            .and_then(|door| door.proto.name.as_deref())
            .map_or("door", fname)
            .to_owned();
        let suffix = if cmd == DoorCmd::Close { "d" } else { "ed" };
        let text = format!(
            "The {name} is {}{suffix} from the other side.",
            cmd_name(cmd)
        );
        crate::dg::out::send_to_room(host.world, exit.to, &text, &[]);
    }
}

/// `ok_pick`: the pick-lock skill check. An NPC has no skills, so the roll
/// can only fail (`GET_SKILL` is 0 and a dexterity of 11 adds nothing).
fn ok_pick(host: &mut EcsHost<'_>, ch: Uid, door: &DoorView, cmd: DoorCmd) -> bool {
    if cmd != DoorCmd::Pick {
        return true;
    }
    let percent = host.rand_number(1, 101);
    if door.proto.key.is_none() {
        to_char(host, ch, "Odd - you can't seem to find a keyhole.\r\n");
    } else if !door.proto.pickable {
        to_char(host, ch, "It resists your attempts to pick it.\r\n");
    } else if percent > 0 {
        to_char(host, ch, "You failed to pick the lock.\r\n");
    } else {
        return true;
    }
    false
}

/// `do_gen_door`.
pub fn do_gen_door(host: &mut EcsHost<'_>, ch: Uid, argument: &str, cmd: DoorCmd) {
    let name = cmd_name(cmd);
    let argument = dg_vm::cstr::skip_spaces(argument);
    if argument.is_empty() {
        let mut title = name.to_owned();
        if let Some(first) = title.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        to_char(host, ch, &format!("{title} what?\r\n"));
        return;
    }
    let (kind, dir, _) = two_arguments(argument);
    let Some(room) = room_of(host, ch) else {
        return;
    };
    // `generic_find(type, FIND_OBJ_INV | FIND_OBJ_ROOM)`: an object by that
    // name decides unless it is not a container.
    let mut found = get_obj_in_list_vis(host, ch, &kind, &host.carrying(ch));
    if found.is_none()
        && let Some(room_uid) = host.in_room(ch)
    {
        found = get_obj_in_list_vis(host, ch, &kind, &host.room_contents(room_uid));
    }
    if let Some(obj) = found
        && host.obj_type(obj) == ITEM_CONTAINER
    {
        door_on_object(host, ch, obj, name);
        return;
    }
    let Some(door) = find_door(host, ch, room, &kind, &dir, name) else {
        return;
    };
    let Some(view) = exit_of(host, room, door).and_then(|exit| exit.door) else {
        to_char(host, ch, &format!("You can't {name} that!\r\n"));
        return;
    };
    let level = host.level(ch);
    let (need_open, need_closed, need_unlocked, need_locked) = match cmd {
        DoorCmd::Open => (false, true, true, false),
        DoorCmd::Close => (true, false, false, false),
        DoorCmd::Unlock => (false, true, false, true),
        DoorCmd::Lock => (false, true, true, false),
        DoorCmd::Pick => (false, true, false, true),
    };
    if view.closed && need_open {
        to_char(host, ch, "But it's already closed!\r\n");
    } else if !view.closed && need_closed {
        to_char(host, ch, "But it's currently open!\r\n");
    } else if !view.locked && need_locked {
        to_char(host, ch, "Oh.. it wasn't locked, after all..\r\n");
    } else if view.locked && need_unlocked && level < LVL_IMMORT {
        to_char(host, ch, "It seems to be locked.\r\n");
    } else if !has_key(host, ch, Some(&view.proto))
        && level < LVL_GOD
        && matches!(cmd, DoorCmd::Lock | DoorCmd::Unlock)
    {
        to_char(host, ch, "You don't seem to have the proper key.\r\n");
    } else if ok_pick(host, ch, &view, cmd) {
        do_doorcmd(host, ch, room, door, cmd);
    }
}

/// A door command that named a container. The pack keeps no per-instance
/// open or locked state, so a closeable container is a gap rather than a
/// guess; one that cannot close is refused as the C refuses it.
fn door_on_object(host: &mut EcsHost<'_>, ch: Uid, obj: Uid, name: &str) {
    let Some(entity) = host.entity(obj) else {
        return;
    };
    let closeable = host
        .world
        .get::<mib_stdlib::prelude::Item>(entity)
        .and_then(|item| {
            host.world
                .get_resource::<crate::module::TbaContent>()?
                .items
                .get(&item.def_id)
        })
        .filter(|def| def.kind == TbaItemKind::Container)
        .is_some_and(|def| def.values[1] & CONT_CLOSEABLE != 0);
    if closeable {
        host.unsupported(
            "mobcmd.container_door",
            &format!("{name} on a container: open/closed state is not kept per instance"),
        );
    } else {
        to_char(host, ch, &format!("You can't {name} that!\r\n"));
    }
}
