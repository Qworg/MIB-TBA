//! Items: `get`, `drop`/`junk`/`donate`, `give`, `put`, `wear`, `wield`,
//! `hold`/`grab` and `remove` (`act.item.c`).
//!
//! Where the C consults something the pack does not model, the check is
//! skipped, never invented: a character's carrying capacity (count and
//! weight), whether a container is closed or locked, the weight a container
//! holds, and the strength a weapon needs. A container's capacity is the
//! pack's count (`Container::capacity`), so "won't fit" means it holds that
//! many items already. Text only the acting NPC would receive is not
//! produced; see [`super`].

use dg_vm::cstr::{atoi, is_number, one_argument, search_block, str_cmp, two_arguments};
use dg_vm::dispatch::{
    bribe_mtrigger, drop_otrigger, drop_wtrigger, get_otrigger, give_otrigger, receive_mtrigger,
    remove_otrigger, wear_otrigger,
};
use dg_vm::lookup::{NUM_WEARS, get_char_room_vis, get_obj_in_list_vis, isname};
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_core::prelude::Entity;
use mib_stdlib::prelude::{Container, Item, Location, Player};

use super::act::{To, act, act_obj, act_obj_obj, act_obj_vict, act_vict};
use super::table::DropMode;
use super::world;
use crate::content::{ItemExtraFlag, TbaItemDef, TbaItemKind, WearSlot};
use crate::dg::host::{item_wear_bit, wear_position, wear_slot};
use crate::module::TbaContent;

/// What `find_all_dots` makes of an argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dot {
    /// `FIND_INDIV`: one item by name.
    Indiv,
    /// `FIND_ALL`: `all`.
    All,
    /// `FIND_ALLDOT`: `all.name`; the argument is cut to the name.
    Prefixed,
}

/// `find_all_dots(arg)`: the mode and the argument as the caller goes on to
/// use it.
fn find_all_dots(arg: &str) -> (Dot, String) {
    if arg == "all" {
        (Dot::All, arg.to_owned())
    } else if let Some(name) = arg.strip_prefix("all.") {
        (Dot::Prefixed, name.to_owned())
    } else {
        (Dot::Indiv, arg.to_owned())
    }
}

/// The prototype definition of an item.
pub(super) fn item_def<'a>(
    host: &'a crate::dg::host::EcsHost<'_>,
    obj: Uid,
) -> Option<&'a TbaItemDef> {
    let entity = host.entity(obj)?;
    let item = host.world.get::<Item>(entity)?;
    host.world
        .get_resource::<TbaContent>()?
        .items
        .get(&item.def_id)
}

/// `OBJ_FLAGGED(obj, flag)` for the prototype's extra flags.
fn extra_flagged(host: &crate::dg::host::EcsHost<'_>, obj: Uid, flag: ItemExtraFlag) -> bool {
    item_def(host, obj).is_some_and(|def| def.extra_flags.contains(&flag))
}

/// `GET_OBJ_TYPE(obj) == kind`.
fn is_kind(host: &crate::dg::host::EcsHost<'_>, obj: Uid, kind: TbaItemKind) -> bool {
    item_def(host, obj).is_some_and(|def| def.kind == kind)
}

type Host<'a, 'w> = &'a mut crate::dg::host::EcsHost<'w>;

/// The items of `list` after `obj`, that still exist: the C's `obj->next_content`
/// read before the body runs and followed afterwards.
fn remaining(host: &crate::dg::host::EcsHost<'_>, list: &[Uid], obj: Uid) -> Vec<Uid> {
    match list.iter().position(|entry| *entry == obj) {
        Some(at) => list
            .iter()
            .skip(at + 1)
            .copied()
            .filter(|entry| host.exists(*entry))
            .collect(),
        None => Vec::new(),
    }
}

/// `generic_find(name, FIND_OBJ_INV | FIND_OBJ_ROOM, ...)`: an item the actor
/// carries, else one on the floor; and whether it was the inventory.
fn find_inv_or_room(
    host: &crate::dg::host::EcsHost<'_>,
    ch: Uid,
    name: &str,
) -> Option<(Uid, bool)> {
    if let Some(obj) = get_obj_in_list_vis(host, ch, name, &host.carrying(ch)) {
        return Some((obj, true));
    }
    let room = host.in_room(ch)?;
    get_obj_in_list_vis(host, ch, name, &host.room_contents(room)).map(|obj| (obj, false))
}

/// `get_check_money`: coins picked up go into the purse and the pile is gone.
fn get_check_money(host: Host<'_, '_>, ch: Uid, obj: Uid) {
    let Some(entity) = host.entity(obj) else {
        return;
    };
    let Some(value) = world::money_value(host, entity).filter(|value| *value > 0) else {
        return;
    };
    world::extract_obj(host, entity);
    let purse = world::gold(host, ch);
    world::set_gold(host, ch, purse.saturating_add(value));
}

// -- get ----------------------------------------------------------------------

/// `perform_get_from_room`.
fn perform_get_from_room(host: Host<'_, '_>, ch: Uid, obj: Uid) -> bool {
    // The pack has no untakeable objects, no weight or count limit to carry,
    // and no furniture occupancy to refuse on.
    if get_otrigger(host, obj, ch) == 0 {
        return false;
    }
    let (Some(ch_entity), Some(obj_entity)) = (host.entity(ch), host.entity(obj)) else {
        return false;
    };
    world::obj_to_char(host, obj_entity, ch_entity);
    act_obj(host, "You get $p.", false, ch, obj, To::Char);
    act_obj(host, "$n gets $p.", true, ch, obj, To::Room);
    get_check_money(host, ch, obj);
    true
}

/// `get_from_room`.
fn get_from_room(host: Host<'_, '_>, ch: Uid, arg: &str, howmany: i32) {
    let Some(room) = host.in_room(ch) else {
        return;
    };
    let (dotmode, arg) = find_all_dots(arg);
    let list = host.room_contents(room);
    if dotmode == Dot::Indiv {
        let mut howmany = howmany;
        let mut obj = get_obj_in_list_vis(host, ch, &arg, &list);
        while let Some(current) = obj {
            if howmany == 0 {
                break;
            }
            howmany = howmany.wrapping_sub(1);
            let next = remaining(host, &list, current);
            perform_get_from_room(host, ch, current);
            obj = get_obj_in_list_vis(host, ch, &arg, &next);
        }
    } else {
        if dotmode == Dot::Prefixed && arg.is_empty() {
            return;
        }
        for obj in list {
            if !host.exists(obj) || host.in_room(obj) != Some(room) {
                continue;
            }
            if host.can_see(ch, obj) && (dotmode == Dot::All || isname(&arg, &host.keywords(obj))) {
                perform_get_from_room(host, ch, obj);
            }
        }
    }
}

/// `perform_get_from_container`.
fn perform_get_from_container(host: Host<'_, '_>, ch: Uid, obj: Uid, cont: Uid) {
    if get_otrigger(host, obj, ch) == 0 {
        return;
    }
    let (Some(ch_entity), Some(obj_entity)) = (host.entity(ch), host.entity(obj)) else {
        return;
    };
    world::obj_to_char(host, obj_entity, ch_entity);
    act_obj_obj(host, "You get $p from $P.", false, ch, obj, cont, To::Char);
    act_obj_obj(host, "$n gets $p from $P.", true, ch, obj, cont, To::Room);
    get_check_money(host, ch, obj);
}

/// `get_from_container`.
fn get_from_container(host: Host<'_, '_>, ch: Uid, cont: Uid, arg: &str, howmany: i32) {
    let (dotmode, arg) = find_all_dots(arg);
    let list = host.obj_contains(cont);
    if dotmode == Dot::Indiv {
        let mut howmany = howmany;
        let mut obj = get_obj_in_list_vis(host, ch, &arg, &list);
        while let Some(current) = obj {
            if howmany == 0 {
                break;
            }
            howmany = howmany.wrapping_sub(1);
            let next = remaining(host, &list, current);
            perform_get_from_container(host, ch, current, cont);
            obj = get_obj_in_list_vis(host, ch, &arg, &next);
        }
    } else {
        if dotmode == Dot::Prefixed && arg.is_empty() {
            return;
        }
        for obj in list {
            if !host.exists(obj) || host.obj_in_obj(obj) != Some(cont) {
                continue;
            }
            if host.can_see(ch, obj) && (dotmode == Dot::All || isname(&arg, &host.keywords(obj))) {
                perform_get_from_container(host, ch, obj, cont);
            }
        }
    }
}

/// `do_get`.
pub fn do_get(host: Host<'_, '_>, ch: Uid, argument: &str) {
    let (mut arg1, mut arg2, rest) = two_arguments(argument);
    let (arg3, _) = one_argument(rest);
    if arg1.is_empty() {
        return;
    }
    if arg2.is_empty() {
        get_from_room(host, ch, &arg1, 1);
        return;
    }
    if is_number(&arg1) && arg3.is_empty() {
        get_from_room(host, ch, &arg2, atoi(&arg1));
        return;
    }
    let mut amount = 1;
    if is_number(&arg1) {
        amount = atoi(&arg1);
        arg1 = arg2;
        arg2 = arg3;
    }
    let (cont_dotmode, arg2) = find_all_dots(&arg2);
    if cont_dotmode == Dot::Indiv {
        let Some((cont, _inv)) = find_inv_or_room(host, ch, &arg2) else {
            return;
        };
        if is_kind(host, cont, TbaItemKind::Container) {
            get_from_container(host, ch, cont, &arg1, amount);
        }
        return;
    }
    if cont_dotmode == Dot::Prefixed && arg2.is_empty() {
        return;
    }
    let mut lists = vec![host.carrying(ch)];
    if let Some(room) = host.in_room(ch) {
        lists.push(host.room_contents(room));
    }
    for list in lists {
        for cont in list {
            if !host.exists(cont) {
                continue;
            }
            if host.can_see(ch, cont)
                && (cont_dotmode == Dot::All || isname(&arg2, &host.keywords(cont)))
                && is_kind(host, cont, TbaItemKind::Container)
            {
                get_from_container(host, ch, cont, &arg1, amount);
            }
        }
    }
}

// -- put ----------------------------------------------------------------------

/// `perform_put`.
fn perform_put(host: Host<'_, '_>, ch: Uid, obj: Uid, cont: Uid) {
    if drop_otrigger(host, obj, ch) == 0 || !host.exists(obj) {
        return;
    }
    let (Some(obj_entity), Some(cont_entity)) = (host.entity(obj), host.entity(cont)) else {
        return;
    };
    let corpse = host
        .world
        .get::<Item>(cont_entity)
        .is_some_and(|item| item.def_id.as_ref() == "corpse");
    let full = host
        .world
        .get::<Container>(cont_entity)
        .is_some_and(|c| c.contents.len() >= usize::try_from(c.capacity).unwrap_or(usize::MAX));
    let cont_lies_in_room = host.in_room(cont).is_some();
    if corpse {
        act_obj_obj(
            host,
            "You can't put anything in $P.",
            false,
            ch,
            obj,
            cont,
            To::Char,
        );
    } else if full {
        act_obj_obj(host, "$p won't fit in $P.", false, ch, obj, cont, To::Char);
    } else if extra_flagged(host, obj, ItemExtraFlag::NoDrop) && cont_lies_in_room {
        act_obj(
            host,
            "You can't get $p out of your hand.",
            false,
            ch,
            obj,
            To::Char,
        );
    } else {
        world::obj_to_obj(host, obj_entity, cont_entity);
        act_obj_obj(host, "$n puts $p in $P.", true, ch, obj, cont, To::Room);
        act_obj_obj(host, "You put $p in $P.", false, ch, obj, cont, To::Char);
    }
}

/// `do_put`.
pub fn do_put(host: Host<'_, '_>, ch: Uid, argument: &str) {
    let (arg1, arg2, rest) = two_arguments(argument);
    let (arg3, _) = one_argument(rest);
    let mut howmany = 1;
    let (theobj, thecont) = if !arg3.is_empty() && is_number(&arg1) {
        howmany = atoi(&arg1);
        (arg2, arg3)
    } else {
        (arg1, arg2)
    };
    let (obj_dotmode, theobj) = find_all_dots(&theobj);
    let (cont_dotmode, thecont) = find_all_dots(&thecont);
    if theobj.is_empty() || cont_dotmode != Dot::Indiv || thecont.is_empty() {
        return;
    }
    let Some((cont, _)) = find_inv_or_room(host, ch, &thecont) else {
        return;
    };
    if !is_kind(host, cont, TbaItemKind::Container) {
        return;
    }
    if obj_dotmode == Dot::Indiv {
        let list = host.carrying(ch);
        let Some(mut obj) = get_obj_in_list_vis(host, ch, &theobj, &list) else {
            return;
        };
        if obj == cont && howmany == 1 {
            return;
        }
        let mut list = list;
        loop {
            if howmany == 0 {
                break;
            }
            let next = remaining(host, &list, obj);
            if obj != cont {
                howmany = howmany.wrapping_sub(1);
                perform_put(host, ch, obj, cont);
            }
            let Some(found) = get_obj_in_list_vis(host, ch, &theobj, &next) else {
                break;
            };
            obj = found;
            list = next;
        }
    } else {
        for obj in host.carrying(ch) {
            if obj != cont
                && host.exists(obj)
                && host.obj_carried_by(obj) == Some(ch)
                && host.can_see(ch, obj)
                && (obj_dotmode == Dot::All || isname(&theobj, &host.keywords(obj)))
            {
                perform_put(host, ch, obj, cont);
            }
        }
    }
}

// -- drop and junk --------------------------------------------------------------

/// `perform_drop_gold`.
fn perform_drop_gold(
    host: Host<'_, '_>,
    ch: Uid,
    amount: i32,
    mode: DropMode,
    donate: Option<Uid>,
) {
    let amount = i64::from(amount);
    if amount <= 0 || world::gold(host, ch) < amount {
        return;
    }
    if mode == DropMode::Junk {
        let line = format!(
            "$n drops {} which disappears in a puff of smoke!",
            world::money_desc(amount)
        );
        act(host, &line, false, ch, To::Room);
    } else {
        let Some(here) = host.in_room(ch).and_then(|room| host.entity(room)) else {
            return;
        };
        if mode == DropMode::Donate {
            let Some(target) = donate.and_then(|room| host.entity(room)) else {
                return;
            };
            act(
                host,
                "$n throws some gold into the air where it disappears in a puff of smoke!",
                false,
                ch,
                To::Room,
            );
            if let Some(pile) = world::create_money_in_room(host, target, amount)
                && let Some(pile_uid) = host.uid(pile)
            {
                act_full_appears(host, pile_uid);
            }
        } else {
            let Some(pile) = world::create_money_in_room(host, here, amount) else {
                return;
            };
            let Some(pile_uid) = host.uid(pile) else {
                return;
            };
            // The coins are lying in the room already; `drop_wtrigger` on them
            // asks for a holder, so (as the C's note says) a world `drop`
            // trigger that ran always refuses them.
            if drop_wtrigger(host, pile_uid, ch) == 0 {
                if host.exists(pile_uid) {
                    world::extract_obj(host, pile);
                }
                return;
            }
            let line = format!("$n drops {}.", world::money_desc(amount));
            act(host, &line, true, ch, To::Room);
        }
    }
    let purse = world::gold(host, ch);
    world::set_gold(host, ch, purse.saturating_sub(amount));
}

/// `act("$p suddenly appears in a puff of orange smoke!", 0, 0, obj, 0, TO_ROOM)`.
fn act_full_appears(host: Host<'_, '_>, pile: Uid) {
    use super::act::{ActObj, ActVict, Flags, act_full};
    act_full(
        host,
        "$p suddenly appears in a puff of orange smoke!",
        false,
        None,
        ActObj::Obj(pile),
        ActVict::None,
        To::Room,
        Flags::default(),
    );
}

/// `perform_drop`: the gold a junked item is worth, else zero.
fn perform_drop(
    host: Host<'_, '_>,
    ch: Uid,
    obj: Uid,
    mode: DropMode,
    sname: &str,
    donate: Option<Uid>,
) -> i64 {
    if drop_otrigger(host, obj, ch) == 0 || !host.exists(obj) {
        return 0;
    }
    if mode == DropMode::Drop && drop_wtrigger(host, obj, ch) == 0 {
        return 0;
    }
    if !host.exists(obj) {
        return 0;
    }
    if extra_flagged(host, obj, ItemExtraFlag::NoDrop) {
        act_obj(
            host,
            &format!("You can't {sname} $p, it must be CURSED!"),
            false,
            ch,
            obj,
            To::Char,
        );
        return 0;
    }
    let vanish = if mode == DropMode::Drop {
        ""
    } else {
        "  It vanishes in a puff of smoke!"
    };
    act_obj(
        host,
        &format!("You {sname} $p.{vanish}"),
        false,
        ch,
        obj,
        To::Char,
    );
    act_obj(
        host,
        &format!("$n {sname}s $p.{vanish}"),
        true,
        ch,
        obj,
        To::Room,
    );
    let Some(obj_entity) = host.entity(obj) else {
        return 0;
    };
    world::detach_item(host, obj_entity);
    let mut mode = mode;
    if mode == DropMode::Donate && extra_flagged(host, obj, ItemExtraFlag::NoDonate) {
        mode = DropMode::Junk;
    }
    match mode {
        DropMode::Drop => {
            if let Some(room) = host.in_room(ch).and_then(|room| host.entity(room)) {
                world::obj_to_room(host, obj_entity, room);
            }
            0
        }
        DropMode::Donate => {
            if let Some(room) = donate.and_then(|room| host.entity(room)) {
                world::obj_to_room(host, obj_entity, room);
                use super::act::{ActObj, ActVict, Flags, act_full};
                act_full(
                    host,
                    "$p suddenly appears in a puff a smoke!",
                    false,
                    None,
                    ActObj::Obj(obj),
                    ActVict::None,
                    To::Room,
                    Flags::default(),
                );
            }
            0
        }
        DropMode::Junk => {
            let cost = item_def(host, obj).map_or(0, |def| i64::from(def.cost));
            world::extract_obj(host, obj_entity);
            (cost / 16).clamp(1, 200)
        }
    }
}

/// The donation room `do_drop` rolls for: `Err` when the donation is refused,
/// `Ok(None)` when the roll turns it into a junking.
fn donation_room(host: Host<'_, '_>) -> Result<Option<Uid>, ()> {
    // `CONFIG_DON_ROOM_1..3` are rooms 3063, 5510 and 235; a roll of 5 leaves
    // the destination at room index 0, the first room of the world.
    let roll = host.rand_number(0, 5);
    let vnum = match roll {
        0 => return Ok(None),
        1 | 2 => 3063,
        3 => 5510,
        4 => 235,
        _ => {
            let lowest = {
                let mut rooms = host.world.query::<&mib_stdlib::prelude::Room>();
                rooms.iter(host.world).map(|room| room.num).min()
            };
            return match lowest.and_then(|num| i32::try_from(num).ok()) {
                Some(num) => host.real_room(num).map(Some).ok_or(()),
                None => Err(()),
            };
        }
    };
    host.real_room(vnum).map(Some).ok_or(())
}

/// `do_drop` for `drop`, `junk` and `donate`.
pub fn do_drop(host: Host<'_, '_>, ch: Uid, argument: &str, command: DropMode) {
    let mut mode = command;
    let mut donate = None;
    let sname = match command {
        DropMode::Junk => "junk",
        DropMode::Donate => "donate",
        DropMode::Drop => "drop",
    };
    if command == DropMode::Donate {
        match donation_room(host) {
            Ok(None) => mode = DropMode::Junk,
            Ok(Some(room)) => donate = Some(room),
            Err(()) => return,
        }
    }
    let (arg, rest) = one_argument(argument);
    if arg.is_empty() {
        return;
    }
    let mut amount: i64 = 0;
    if is_number(&arg) {
        let multi = atoi(&arg);
        let (arg, _) = one_argument(rest);
        if str_cmp("coins", &arg) == 0 || str_cmp("coin", &arg) == 0 {
            perform_drop_gold(host, ch, multi, mode, donate);
        } else if multi <= 0 || arg.is_empty() {
            return;
        } else {
            let list = host.carrying(ch);
            let Some(mut obj) = get_obj_in_list_vis(host, ch, &arg, &list) else {
                return;
            };
            let mut multi = multi;
            let mut list = list;
            loop {
                let next = remaining(host, &list, obj);
                amount += perform_drop(host, ch, obj, mode, sname, donate);
                multi = multi.wrapping_sub(1);
                let Some(found) = get_obj_in_list_vis(host, ch, &arg, &next) else {
                    break;
                };
                if multi == 0 {
                    break;
                }
                obj = found;
                list = next;
            }
        }
    } else {
        let (dotmode, arg) = find_all_dots(&arg);
        if dotmode == Dot::All && command != DropMode::Drop {
            return;
        }
        let list = host.carrying(ch);
        if dotmode == Dot::All {
            for obj in list {
                if host.exists(obj) && host.obj_carried_by(obj) == Some(ch) {
                    amount += perform_drop(host, ch, obj, mode, sname, donate);
                }
            }
        } else if dotmode == Dot::Prefixed {
            if arg.is_empty() {
                return;
            }
            let mut obj = get_obj_in_list_vis(host, ch, &arg, &list);
            while let Some(current) = obj {
                let next = remaining(host, &list, current);
                amount += perform_drop(host, ch, current, mode, sname, donate);
                obj = get_obj_in_list_vis(host, ch, &arg, &next);
            }
        } else if let Some(obj) = get_obj_in_list_vis(host, ch, &arg, &list) {
            amount += perform_drop(host, ch, obj, mode, sname, donate);
        }
    }
    if amount != 0 && command == DropMode::Junk {
        act(
            host,
            "$n has been rewarded by the gods!",
            true,
            ch,
            To::Room,
        );
        let purse = world::gold(host, ch);
        world::set_gold(host, ch, purse.saturating_add(amount));
    }
}

// -- give ---------------------------------------------------------------------

/// `perform_give`.
fn perform_give(host: Host<'_, '_>, ch: Uid, vict: Uid, obj: Uid) {
    if give_otrigger(host, obj, ch, vict) == 0 {
        return;
    }
    if receive_mtrigger(host, vict, ch, obj) == 0 {
        return;
    }
    if extra_flagged(host, obj, ItemExtraFlag::NoDrop) {
        act_obj(
            host,
            "You can't let go of $p!!  Yeech!",
            false,
            ch,
            obj,
            To::Char,
        );
        return;
    }
    let (Some(vict_entity), Some(obj_entity)) = (host.entity(vict), host.entity(obj)) else {
        return;
    };
    world::obj_to_char(host, obj_entity, vict_entity);
    act_obj_vict(host, "You give $p to $N.", false, ch, obj, vict, To::Char);
    act_obj_vict(host, "$n gives you $p.", false, ch, obj, vict, To::Vict);
    act_obj_vict(host, "$n gives $p to $N.", true, ch, obj, vict, To::NotVict);
    world::emit_collected(host, vict_entity, obj_entity);
}

/// `give_find_vict`.
fn give_find_vict(host: &crate::dg::host::EcsHost<'_>, ch: Uid, arg: &str) -> Option<Uid> {
    let arg = dg_vm::cstr::skip_spaces(arg);
    if arg.is_empty() {
        return None;
    }
    let vict = get_char_room_vis(host, ch, arg)?;
    (vict != ch).then_some(vict)
}

/// `perform_give_gold`.
fn perform_give_gold(host: Host<'_, '_>, ch: Uid, vict: Uid, amount: i32) {
    let amount = i64::from(amount);
    if amount <= 0 || world::gold(host, ch) < amount {
        return;
    }
    let plural = if amount == 1 { "" } else { "s" };
    act_vict(
        host,
        &format!("$n gives you {amount} gold coin{plural}."),
        false,
        ch,
        vict,
        To::Vict,
    );
    act_vict(
        host,
        &format!("$n gives {} to $N.", world::money_desc(amount)),
        true,
        ch,
        vict,
        To::NotVict,
    );
    let mine = world::gold(host, ch);
    world::set_gold(host, ch, mine.saturating_sub(amount));
    let theirs = world::gold(host, vict);
    world::set_gold(host, vict, theirs.saturating_add(amount));
    bribe_mtrigger(host, vict, ch, i32::try_from(amount).unwrap_or(i32::MAX));
}

/// `do_give`.
pub fn do_give(host: Host<'_, '_>, ch: Uid, argument: &str) {
    let (arg, rest) = one_argument(argument);
    if arg.is_empty() {
        return;
    }
    if is_number(&arg) {
        let mut amount = atoi(&arg);
        let (arg, rest) = one_argument(rest);
        if str_cmp("coins", &arg) == 0 || str_cmp("coin", &arg) == 0 {
            let (target, _) = one_argument(rest);
            if let Some(vict) = give_find_vict(host, ch, &target) {
                perform_give_gold(host, ch, vict, amount);
            }
            return;
        }
        if arg.is_empty() {
            return;
        }
        let Some(vict) = give_find_vict(host, ch, rest) else {
            return;
        };
        let list = host.carrying(ch);
        let mut obj = get_obj_in_list_vis(host, ch, &arg, &list);
        while let Some(current) = obj {
            if amount == 0 {
                break;
            }
            amount = amount.wrapping_sub(1);
            let next = remaining(host, &list, current);
            perform_give(host, ch, vict, current);
            obj = get_obj_in_list_vis(host, ch, &arg, &next);
        }
        return;
    }
    let (target, _) = one_argument(rest);
    let Some(vict) = give_find_vict(host, ch, &target) else {
        return;
    };
    let (dotmode, arg) = find_all_dots(&arg);
    let list = host.carrying(ch);
    if dotmode == Dot::Indiv {
        if let Some(obj) = get_obj_in_list_vis(host, ch, &arg, &list) {
            perform_give(host, ch, vict, obj);
        }
    } else if dotmode == Dot::Prefixed && arg.is_empty() {
    } else {
        for obj in list {
            if host.exists(obj)
                && host.obj_carried_by(obj) == Some(ch)
                && host.can_see(ch, obj)
                && (dotmode == Dot::All || isname(&arg, &host.keywords(obj)))
            {
                perform_give(host, ch, vict, obj);
            }
        }
    }
}

// -- wear, wield, hold, remove ----------------------------------------------------

/// `wear_messages[where]`: what the room sees and what the wearer is told.
fn wear_messages(slot: WearSlot) -> (&'static str, &'static str) {
    match slot {
        WearSlot::Light => ("$n lights $p and holds it.", "You light $p and hold it."),
        WearSlot::FingerR => (
            "$n slides $p on to $s right ring finger.",
            "You slide $p on to your right ring finger.",
        ),
        WearSlot::FingerL => (
            "$n slides $p on to $s left ring finger.",
            "You slide $p on to your left ring finger.",
        ),
        WearSlot::Neck1 | WearSlot::Neck2 => (
            "$n wears $p around $s neck.",
            "You wear $p around your neck.",
        ),
        WearSlot::Body => ("$n wears $p on $s body.", "You wear $p on your body."),
        WearSlot::Head => ("$n wears $p on $s head.", "You wear $p on your head."),
        WearSlot::Legs => ("$n puts $p on $s legs.", "You put $p on your legs."),
        WearSlot::Feet => ("$n wears $p on $s feet.", "You wear $p on your feet."),
        WearSlot::Hands => ("$n puts $p on $s hands.", "You put $p on your hands."),
        WearSlot::Arms => ("$n wears $p on $s arms.", "You wear $p on your arms."),
        WearSlot::Shield => (
            "$n straps $p around $s arm as a shield.",
            "You start to use $p as a shield.",
        ),
        WearSlot::About => (
            "$n wears $p about $s body.",
            "You wear $p around your body.",
        ),
        WearSlot::Waist => (
            "$n wears $p around $s waist.",
            "You wear $p around your waist.",
        ),
        WearSlot::WristR => (
            "$n puts $p on around $s right wrist.",
            "You put $p on around your right wrist.",
        ),
        WearSlot::WristL => (
            "$n puts $p on around $s left wrist.",
            "You put $p on around your left wrist.",
        ),
        WearSlot::Wield => ("$n wields $p.", "You wield $p."),
        WearSlot::Hold => ("$n grabs $p.", "You grab $p."),
    }
}

/// Whether the item may be worn in this position: `CAN_WEAR(obj,
/// wear_bitvectors[where])`, where the light and hold positions take anything.
fn can_wear_in(host: &crate::dg::host::EcsHost<'_>, obj: Uid, slot: WearSlot) -> bool {
    if matches!(slot, WearSlot::Light | WearSlot::Hold) {
        return true;
    }
    host.wear_flags(obj) & (1 << item_wear_bit(slot)) != 0
}

/// `perform_wear`.
fn perform_wear(host: Host<'_, '_>, ch: Uid, obj: Uid, slot: WearSlot) {
    if !can_wear_in(host, obj, slot) {
        return;
    }
    let Some(ch_entity) = host.entity(ch) else {
        return;
    };
    let worn_in = |host: &crate::dg::host::EcsHost<'_>, slot: WearSlot| {
        host.world
            .get::<crate::equipment::TbaEquipment>(ch_entity)
            .is_some_and(|equipment| equipment.worn(slot).is_some())
    };
    // The right finger, first neck and right wrist give way to their pair.
    let mut slot = slot;
    if matches!(slot, WearSlot::FingerR | WearSlot::Neck1 | WearSlot::WristR)
        && worn_in(host, slot)
        && let Some(partner) = slot.partner()
    {
        slot = partner;
    }
    if worn_in(host, slot) {
        return;
    }
    if wear_otrigger(host, obj, ch, wear_position(slot)) == 0
        || host.obj_carried_by(obj) != Some(ch)
    {
        return;
    }
    let (room_line, char_line) = wear_messages(slot);
    act_obj(host, room_line, true, ch, obj, To::Room);
    act_obj(host, char_line, false, ch, obj, To::Char);
    if let Some(obj_entity) = host.entity(obj) {
        world::equip_char(host, ch_entity, obj_entity, slot);
    }
}

/// `find_eq_pos(ch, obj, arg)`: where an item goes, `None` for nowhere.
fn find_eq_pos(host: &crate::dg::host::EcsHost<'_>, obj: Uid, arg: &str) -> Option<WearSlot> {
    if arg.is_empty() {
        // Each later match overrides an earlier one, as in the C.
        const ORDER: [(WearSlot, WearSlot); 12] = [
            (WearSlot::FingerR, WearSlot::FingerR),
            (WearSlot::Neck1, WearSlot::Neck1),
            (WearSlot::Body, WearSlot::Body),
            (WearSlot::Head, WearSlot::Head),
            (WearSlot::Legs, WearSlot::Legs),
            (WearSlot::Feet, WearSlot::Feet),
            (WearSlot::Hands, WearSlot::Hands),
            (WearSlot::Arms, WearSlot::Arms),
            (WearSlot::Shield, WearSlot::Shield),
            (WearSlot::About, WearSlot::About),
            (WearSlot::Waist, WearSlot::Waist),
            (WearSlot::WristR, WearSlot::WristR),
        ];
        let mut found = None;
        for (flag_slot, target) in ORDER {
            if host.wear_flags(obj) & (1 << item_wear_bit(flag_slot)) != 0 {
                found = Some(target);
            }
        }
        return found;
    }
    const KEYWORDS: [&str; 18] = [
        "!RESERVED!",
        "finger",
        "!RESERVED!",
        "neck",
        "!RESERVED!",
        "body",
        "head",
        "legs",
        "feet",
        "hands",
        "arms",
        "shield",
        "about",
        "waist",
        "wrist",
        "!RESERVED!",
        "!RESERVED!",
        "!RESERVED!",
    ];
    let at = search_block(arg, &KEYWORDS, false)?;
    wear_slot(i32::try_from(at).ok()?)
}

/// `GET_OBJ_LEVEL(obj)` against `GET_LEVEL(ch)`: whether the wearer is too
/// inexperienced.
fn too_low_level(host: &crate::dg::host::EcsHost<'_>, ch: Uid, obj: Uid) -> bool {
    let needed =
        item_def(host, obj).map_or(0, |def| i32::try_from(def.min_level).unwrap_or(i32::MAX));
    host.level(ch) < needed
}

/// `do_wear`.
pub fn do_wear(host: Host<'_, '_>, ch: Uid, argument: &str) {
    let (arg1, arg2, _) = two_arguments(argument);
    if arg1.is_empty() {
        return;
    }
    let (dotmode, arg1) = find_all_dots(&arg1);
    if !arg2.is_empty() && dotmode != Dot::Indiv {
        return;
    }
    let list = host.carrying(ch);
    match dotmode {
        Dot::All => {
            for obj in list {
                if !host.exists(obj) || host.obj_carried_by(obj) != Some(ch) {
                    continue;
                }
                if host.can_see(ch, obj)
                    && let Some(slot) = find_eq_pos(host, obj, "")
                    && !too_low_level(host, ch, obj)
                {
                    perform_wear(host, ch, obj, slot);
                }
            }
        }
        Dot::Prefixed => {
            if arg1.is_empty() {
                return;
            }
            let Some(mut obj) = get_obj_in_list_vis(host, ch, &arg1, &list) else {
                return;
            };
            if too_low_level(host, ch, obj) {
                return;
            }
            loop {
                let next = remaining(host, &list, obj);
                if let Some(slot) = find_eq_pos(host, obj, "") {
                    perform_wear(host, ch, obj, slot);
                }
                let Some(found) = get_obj_in_list_vis(host, ch, &arg1, &next) else {
                    break;
                };
                obj = found;
            }
        }
        Dot::Indiv => {
            let Some(obj) = get_obj_in_list_vis(host, ch, &arg1, &list) else {
                return;
            };
            if too_low_level(host, ch, obj) {
                return;
            }
            if let Some(slot) = find_eq_pos(host, obj, &arg2) {
                perform_wear(host, ch, obj, slot);
            }
        }
    }
}

/// `do_wield`.
pub fn do_wield(host: Host<'_, '_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        return;
    }
    let Some(obj) = get_obj_in_list_vis(host, ch, &arg, &host.carrying(ch)) else {
        return;
    };
    // The weight a character's strength can wield is not modelled, so only the
    // wear flag and the level gate.
    if !can_wear_in(host, obj, WearSlot::Wield) || too_low_level(host, ch, obj) {
        return;
    }
    perform_wear(host, ch, obj, WearSlot::Wield);
}

/// `do_grab` (`hold`).
pub fn do_grab(host: Host<'_, '_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        return;
    }
    let Some(obj) = get_obj_in_list_vis(host, ch, &arg, &host.carrying(ch)) else {
        return;
    };
    if too_low_level(host, ch, obj) {
        return;
    }
    if is_kind(host, obj, TbaItemKind::Light) {
        perform_wear(host, ch, obj, WearSlot::Light);
    } else {
        let holdable = host.wear_flags(obj) & (1 << item_wear_bit(WearSlot::Hold)) != 0
            || [
                TbaItemKind::Wand,
                TbaItemKind::Staff,
                TbaItemKind::Scroll,
                TbaItemKind::Potion,
            ]
            .iter()
            .any(|kind| is_kind(host, obj, *kind));
        if holdable {
            perform_wear(host, ch, obj, WearSlot::Hold);
        }
    }
}

/// `perform_remove`.
fn perform_remove(host: Host<'_, '_>, ch: Uid, pos: i32) {
    let (Some(slot), Some(ch_entity)) = (wear_slot(pos), host.entity(ch)) else {
        return;
    };
    let Some(obj) = host.equipment(ch, pos) else {
        return;
    };
    if extra_flagged(host, obj, ItemExtraFlag::NoDrop) {
        act_obj(
            host,
            "You can't remove $p, it must be CURSED!",
            false,
            ch,
            obj,
            To::Char,
        );
        return;
    }
    if remove_otrigger(host, obj, ch) == 0 {
        return;
    }
    if world::unequip_char(host, ch_entity, slot).is_none() {
        return;
    }
    act_obj(host, "You stop using $p.", false, ch, obj, To::Char);
    act_obj(host, "$n stops using $p.", true, ch, obj, To::Room);
}

/// `do_remove`.
pub fn do_remove(host: Host<'_, '_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        return;
    }
    let (dotmode, arg) = find_all_dots(&arg);
    match dotmode {
        Dot::All => {
            for pos in 0..NUM_WEARS {
                if host.equipment(ch, pos).is_some() {
                    perform_remove(host, ch, pos);
                }
            }
        }
        Dot::Prefixed => {
            if arg.is_empty() {
                return;
            }
            for pos in 0..NUM_WEARS {
                if let Some(worn) = host.equipment(ch, pos)
                    && host.can_see(ch, worn)
                    && isname(&arg, &host.keywords(worn))
                {
                    perform_remove(host, ch, pos);
                }
            }
        }
        Dot::Indiv => {
            if let Some(pos) = dg_vm::lookup::get_obj_pos_in_equip_vis(host, ch, ch, &arg) {
                perform_remove(host, ch, pos);
            }
        }
    }
}

/// Whether the entity is lying in a room (used where the C asks `IN_ROOM(obj)
/// != NOWHERE`).
#[allow(dead_code)]
fn lies_in_room(host: &crate::dg::host::EcsHost<'_>, obj: Entity) -> bool {
    host.world.get::<Location>(obj).is_some() && host.world.get::<Player>(obj).is_none()
}

// -- sacrifice ----------------------------------------------------------------

/// `do_sac`: the item goes to the gods and what it held falls to the floor.
/// An NPC has no experience to gain, so only the coin rewards are kept.
pub fn do_sac(host: Host<'_, '_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        world::to_char(host, ch, "Sacrifice what?\r\n");
        return;
    }
    let Some(room) = host.in_room(ch) else {
        return;
    };
    let target = get_obj_in_list_vis(host, ch, &arg, &host.room_contents(room))
        .or_else(|| get_obj_in_list_vis(host, ch, &arg, &host.carrying(ch)));
    let Some(obj) = target else {
        world::to_char(host, ch, "It doesn't seem to be here.\r\n");
        return;
    };
    act_obj(host, "$n sacrifices $p.", false, ch, obj, To::Room);
    let level = i64::from(item_def(host, obj).map_or(0, |def| def.min_level));
    let reward = match host.rand_number(0, 5) {
        0 => 1,
        4 => level.saturating_add(1),
        5 => level.saturating_mul(2).saturating_add(1),
        _ => 0,
    };
    if reward > 0 {
        let purse = world::gold(host, ch);
        world::set_gold(host, ch, purse.saturating_add(reward));
    }
    let (Some(obj_entity), Some(room_entity)) = (host.entity(obj), host.entity(room)) else {
        return;
    };
    let inside: Vec<Entity> = host
        .world
        .get::<Container>(obj_entity)
        .map(|container| container.contents.clone())
        .unwrap_or_default();
    for item in inside {
        world::obj_to_room(host, item, room_entity);
    }
    world::extract_obj(host, obj_entity);
}
