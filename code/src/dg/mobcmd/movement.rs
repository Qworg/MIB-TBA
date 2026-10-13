//! Walking: `north`..`southwest`, `enter` and the following that goes with
//! them (`perform_move` and `do_simple_move` of `act.movement.c`).
//!
//! The pack models neither movement points for NPCs, death-trap rooms,
//! houses, nor zone restrictions, so those checks are absent; the rest of
//! `do_simple_move` is here, in order, with the triggers it runs.

use mib_core::prelude::Entity;
use dg_vm::cstr::{one_argument, str_cmp};
use dg_vm::dispatch::{
    enter_wtrigger, entry_memory_mtrigger, entry_mtrigger, greet_memory_mtrigger, greet_mtrigger,
    leave_mtrigger, leave_otrigger, leave_wtrigger,
};
use dg_vm::lookup::NUM_WEARS;
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_stdlib::prelude::{Location, Player, Room, RoomTag};
use mib_stdlib::content::Sector;

use super::act::{To, act, act_vict, fname};
use super::door::{ExitView, exit_of};
use super::world::{self, DIAGONAL_DIRS, DIR_COUNT, dir_name, room_of, to_char};
use crate::content::{ItemAffectFlag, Position, TbaItemKind};
use crate::dg::host::EcsHost;
use crate::party::TbaFollowing;

/// `LVL_IMMORT`, `LVL_GOD` and `CONFIG_TUNNEL_SIZE`.
const LVL_IMMORT: i32 = 31;
const LVL_GOD: i32 = 32;
const TUNNEL_SIZE: usize = 2;

/// `IS_DIAGONAL(dir)`.
fn is_diagonal(dir: usize) -> bool {
    (6..10).contains(&dir)
}

/// The items a character holds or wears with their kind and affect flags
/// asked for by the movement requirements.
fn held(host: &EcsHost<'_>, ch: Uid) -> (Vec<Uid>, Vec<Uid>) {
    let carried = host.carrying(ch);
    let worn = (0..NUM_WEARS)
        .filter_map(|pos| host.equipment(ch, pos))
        .collect();
    (carried, worn)
}

fn def_of<'a>(host: &'a EcsHost<'_>, obj: Uid) -> Option<&'a crate::content::TbaItemDef> {
    super::item::item_def(host, obj)
}

/// `has_boat`: a boat carried that cannot be worn, or any boat worn.
fn has_boat(host: &EcsHost<'_>, ch: Uid) -> bool {
    if host.level(ch) > LVL_IMMORT {
        return true;
    }
    let (carried, worn) = held(host, ch);
    carried.into_iter().any(|obj| {
        def_of(host, obj).is_some_and(|def| def.kind == TbaItemKind::Boat && def.slots.is_empty())
    }) || worn
        .into_iter()
        .any(|obj| def_of(host, obj).is_some_and(|def| def.kind == TbaItemKind::Boat))
}

/// `has_flight`: anything held or worn that gives flight.
fn has_flight(host: &EcsHost<'_>, ch: Uid) -> bool {
    if host.level(ch) > LVL_IMMORT {
        return true;
    }
    let (carried, worn) = held(host, ch);
    carried.into_iter().chain(worn).any(|obj| {
        def_of(host, obj).is_some_and(|def| def.affect_flags.contains(&ItemAffectFlag::Flying))
    })
}

/// The sector of a room.
fn sector(host: &EcsHost<'_>, room: Entity) -> Option<Sector> {
    host.world.get::<Room>(room).map(|room| room.sector)
}

/// How many players stand in a room (`num_pc_in_room`).
fn players_in(host: &mut EcsHost<'_>, room: Entity) -> usize {
    let mut query = host.world.query_filtered::<&Location, mib_core::prelude::With<Player>>();
    query
        .iter(host.world)
        .filter(|location| location.room == room)
        .count()
}

/// `do_simple_move`: one step, with every check and trigger. Returns whether
/// the character ended up in the new room.
pub fn simple_move(host: &mut EcsHost<'_>, ch: Uid, dir: usize, exit: &ExitView) -> bool {
    let (Some(was_in), Some(ch_entity)) = (room_of(host, ch), host.entity(ch)) else {
        return false;
    };
    let going_to = exit.to;
    let (Some(was_uid), Some(going_uid)) = (host.uid(was_in), host.uid(going_to)) else {
        return false;
    };
    let dir_i = dir as i32;

    // A trigger that blocks the move, or moves the character itself, ends it.
    if leave_mtrigger(host, ch, dir_i) == 0 || room_of(host, ch) != Some(was_in) {
        return false;
    }
    if leave_wtrigger(host, was_uid, ch, dir_i) == 0 || room_of(host, ch) != Some(was_in) {
        return false;
    }
    if leave_otrigger(host, was_uid, ch, dir_i) == 0 || room_of(host, ch) != Some(was_in) {
        return false;
    }

    if matches!(sector(host, was_in), Some(Sector::WaterNoSwim))
        || matches!(sector(host, going_to), Some(Sector::WaterNoSwim))
    {
        if !has_boat(host, ch) {
            to_char(host, ch, "You need a boat to go there.\r\n");
            return false;
        }
    }
    if (matches!(sector(host, was_in), Some(Sector::Flying))
        || matches!(sector(host, going_to), Some(Sector::Flying)))
        && !has_flight(host, ch)
    {
        to_char(host, ch, "You need to be flying to go there!\r\n");
        return false;
    }
    // Breathing underwater is only demanded of players.
    if world::room_flagged(host, going_to, RoomTag::Tunnel)
        && players_in(host, going_to) >= TUNNEL_SIZE
    {
        to_char(host, ch, "There isn't enough room for you to go there!\r\n");
        return false;
    }
    if world::room_flagged(host, going_to, RoomTag::Godroom) && host.level(ch) < LVL_GOD {
        to_char(host, ch, "You aren't godly enough to use that room!\r\n");
        return false;
    }

    // An NPC pays no movement points.
    let sneak = world::sneaking(host, ch);
    if !sneak {
        act(host, &format!("$n leaves {}.", dir_name(dir)), true, ch, To::Room);
    }

    world::char_to_room(host, ch_entity, going_to);

    if entry_mtrigger(host, ch) == 0 || enter_wtrigger(host, going_uid, ch, dir_i) == 0 {
        world::char_to_room(host, ch_entity, was_in);
        return false;
    }

    if !sneak {
        act(host, "$n has arrived.", true, ch, To::Room);
    }

    entry_memory_mtrigger(host, ch);
    if greet_mtrigger(host, ch, dir_i) == 0 {
        world::char_to_room(host, ch_entity, was_in);
        return false;
    }
    greet_memory_mtrigger(host, ch);
    true
}

/// `perform_move`: walk `dir`, then take the followers along.
pub fn perform_move(host: &mut EcsHost<'_>, ch: Uid, dir: usize) -> bool {
    if !host.exists(ch) || dir >= 10 || world::fighting(host, ch).is_some() {
        return false;
    }
    if !DIAGONAL_DIRS && is_diagonal(dir) {
        to_char(host, ch, "Alas, you cannot go that way...\r\n");
        return false;
    }
    let Some(was_in) = room_of(host, ch) else {
        return false;
    };
    let Some(exit) = exit_of(host, was_in, dir) else {
        to_char(host, ch, "Alas, you cannot go that way...\r\n");
        return false;
    };
    if let Some(door) = exit.door.as_ref().filter(|door| door.closed) {
        match door.proto.name.as_deref() {
            Some(keyword) => to_char(
                host,
                ch,
                &format!("The {} seems to be closed.\r\n", fname(keyword)),
            ),
            None => to_char(host, ch, "It seems to be closed.\r\n"),
        }
        return false;
    }
    let followers = world::followers(host, ch);
    if !simple_move(host, ch, dir, &exit) {
        return false;
    }
    let new_room = room_of(host, ch);
    for follower in followers {
        // The follower system drags players along; an NPC walks the exit
        // itself, so the system must not drag it a second time.
        if host.is_npc(follower) {
            if let (Some(entity), Some(room)) = (host.entity(follower), new_room)
                && let Some(mut following) = host.world.get_mut::<TbaFollowing>(entity)
            {
                following.last_room = room;
            }
            if room_of(host, follower) == Some(was_in)
                && world::position(host, follower) >= Position::Standing
            {
                act_vict(host, "You follow $N.", false, follower, ch, To::Char);
                perform_move(host, follower, dir);
            }
        }
    }
    true
}

/// `do_move`.
pub fn do_move(host: &mut EcsHost<'_>, ch: Uid, dir: usize) {
    perform_move(host, ch, dir);
}

/// `do_enter`: walk through the exit whose door keyword is the argument.
pub fn do_enter(host: &mut EcsHost<'_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        // Which rooms are indoors is not modelled, so there is no entrance to find.
        host.unsupported("mobcmd.indoors", "enter without an argument needs ROOM_INDOORS");
        return;
    }
    let Some(room) = room_of(host, ch) else {
        return;
    };
    for door in 0..DIR_COUNT {
        let keyword = exit_of(host, room, door)
            .and_then(|exit| exit.door)
            .and_then(|door| door.proto.name);
        if keyword.is_some_and(|keyword| str_cmp(&keyword, &arg) == 0) {
            perform_move(host, ch, door);
            return;
        }
    }
    to_char(host, ch, &format!("There is no {arg} here.\r\n"));
}
