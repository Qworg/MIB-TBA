//! Name and id lookup: how a script's text names an entity.
//!
//! Ports of the `get_*`/`find_*` family in `dg_scripts.c`, the visibility-aware
//! `get_*_vis` family and `isname`/`get_number` in `handler.c`, and
//! `obj_room`/`dg_room_of_obj`. Every module that turns an argument into a
//! character, object or room goes through here so they all agree.
//!
//! A name beginning with [`UID_CHAR`] is an id; anything else is matched
//! against keyword lists in the order the C walks its linked lists, which the
//! host reproduces through [`WorldHost`].

use crate::cstr::{atoi, is_abbrev, is_number, is_space, str_eq};
use crate::host::WorldHost;
use crate::types::{Kind, OBJ_ID_BASE, ROOM_ID_BASE, UID_CHAR, Uid, parse_uid};

/// `NUM_WEARS`: the number of equipment positions.
pub const NUM_WEARS: i32 = 18;

pub const WEAR_LIGHT: i32 = 0;
pub const WEAR_FINGER_R: i32 = 1;
pub const WEAR_FINGER_L: i32 = 2;
pub const WEAR_NECK_1: i32 = 3;
pub const WEAR_NECK_2: i32 = 4;
pub const WEAR_BODY: i32 = 5;
pub const WEAR_HEAD: i32 = 6;
pub const WEAR_LEGS: i32 = 7;
pub const WEAR_FEET: i32 = 8;
pub const WEAR_HANDS: i32 = 9;
pub const WEAR_ARMS: i32 = 10;
pub const WEAR_SHIELD: i32 = 11;
pub const WEAR_ABOUT: i32 = 12;
pub const WEAR_WAIST: i32 = 13;
pub const WEAR_WRIST_R: i32 = 14;
pub const WEAR_WRIST_L: i32 = 15;
pub const WEAR_WIELD: i32 = 16;
pub const WEAR_HOLD: i32 = 17;

/// `FIND_INDEX_LAST`: what [`get_number`] returns for `last.<name>`.
pub const FIND_INDEX_LAST: i32 = -1;

/// `handler.c` `isname`: `name` equals the whole list, or abbreviates one of
/// its whitespace-separated keywords. A name starting with a digit must also
/// match the keyword numerically ("don't allow abbreviated numbers").
pub fn isname(name: &str, namelist: &str) -> bool {
    if name.is_empty() || namelist.is_empty() {
        return false;
    }
    if name == namelist {
        return true;
    }
    namelist
        .split(is_space)
        .filter(|token| !token.is_empty())
        .any(|token| {
            is_abbrev(name, token)
                && !(name.starts_with(|c: char| c.is_ascii_digit()) && atoi(name) != atoi(token))
        })
}

/// `handler.c` `get_number`: split `2.sword` into `(2, "sword")`.
///
/// No dot yields `1`. `last.` yields [`FIND_INDEX_LAST`]. A non-numeric
/// prefix yields `0` (which the callers treat as "no match"), and an empty
/// prefix (`.sword`) yields `0` too, as `atoi("")` does.
pub fn get_number(name: &str) -> (i32, &str) {
    match name.split_once('.') {
        None => (1, name),
        Some((number, rest)) => {
            if str_eq(number, "last") {
                (FIND_INDEX_LAST, rest)
            } else if number.chars().all(|c| c.is_ascii_digit()) {
                (atoi(number), rest)
            } else {
                (0, rest)
            }
        }
    }
}

/// `find_char(n)`: the live character with this id.
pub fn find_char<H: WorldHost + ?Sized>(host: &H, n: Uid) -> Option<Uid> {
    (n < ROOM_ID_BASE && host.kind(n) == Some(Kind::Char)).then_some(n)
}

/// `find_obj(n)`: the live object with this id.
pub fn find_obj<H: WorldHost + ?Sized>(host: &H, n: Uid) -> Option<Uid> {
    (n >= OBJ_ID_BASE && host.kind(n) == Some(Kind::Obj)).then_some(n)
}

/// `find_room(n)`: the room with this id (`ROOM_ID_BASE` + vnum).
///
/// Deviation: ids at or above `OBJ_ID_BASE` are never rooms (see
/// [`crate::types::uid_kind`]).
pub fn find_room<H: WorldHost + ?Sized>(host: &H, n: Uid) -> Option<Uid> {
    if !(ROOM_ID_BASE..OBJ_ID_BASE).contains(&n) {
        return None;
    }
    host.real_room(i32::try_from(n - ROOM_ID_BASE).ok()?)
}

/// A character id that is also a valid script target.
fn uid_char<H: WorldHost + ?Sized>(host: &H, name: &str) -> Option<Uid> {
    let ch = find_char(host, parse_uid(name)?)?;
    host.valid_dg_target(ch, true).then_some(ch)
}

/// The first character in `list` that `name` names and scripts may target.
fn char_in_list<H: WorldHost + ?Sized>(host: &H, name: &str, list: &[Uid]) -> Option<Uid> {
    list.iter()
        .copied()
        .find(|ch| isname(name, &host.keywords(*ch)) && host.valid_dg_target(*ch, true))
}

/// `get_char(name)`: by id, or the first match in the character list.
pub fn get_char<H: WorldHost + ?Sized>(host: &H, name: &str) -> Option<Uid> {
    if name.starts_with(UID_CHAR) {
        return uid_char(host, name);
    }
    char_in_list(host, name, &host.character_list())
}

/// `get_char_near_obj(obj, name)`: by id, or in the object's room.
pub fn get_char_near_obj<H: WorldHost + ?Sized>(host: &H, obj: Uid, name: &str) -> Option<Uid> {
    if name.starts_with(UID_CHAR) {
        return uid_char(host, name);
    }
    let room = obj_room(host, obj)?;
    char_in_list(host, name, &host.room_people(room))
}

/// `get_char_in_room(room, name)`: by id, or in the room.
pub fn get_char_in_room<H: WorldHost + ?Sized>(host: &H, room: Uid, name: &str) -> Option<Uid> {
    if name.starts_with(UID_CHAR) {
        return uid_char(host, name);
    }
    char_in_list(host, name, &host.room_people(room))
}

/// `get_char_by_obj(obj, name)`: by id, the carrier, the wearer, then the
/// world.
pub fn get_char_by_obj<H: WorldHost + ?Sized>(host: &H, obj: Uid, name: &str) -> Option<Uid> {
    if name.starts_with(UID_CHAR) {
        return uid_char(host, name);
    }
    let holders: Vec<Uid> = [host.obj_carried_by(obj), host.obj_worn_by(obj)]
        .into_iter()
        .flatten()
        .collect();
    char_in_list(host, name, &holders).or_else(|| char_in_list(host, name, &host.character_list()))
}

/// `get_char_by_room(room, name)`: by id, in the room, then the world.
pub fn get_char_by_room<H: WorldHost + ?Sized>(host: &H, room: Uid, name: &str) -> Option<Uid> {
    if name.starts_with(UID_CHAR) {
        return uid_char(host, name);
    }
    char_in_list(host, name, &host.room_people(room))
        .or_else(|| char_in_list(host, name, &host.character_list()))
}

/// `get_obj_in_list(name, list)`: by id or keyword, within `list` only.
pub fn get_obj_in_list<H: WorldHost + ?Sized>(host: &H, name: &str, list: &[Uid]) -> Option<Uid> {
    match parse_uid(name) {
        Some(id) => list.iter().copied().find(|obj| *obj == id),
        None => list
            .iter()
            .copied()
            .find(|obj| isname(name, &host.keywords(*obj))),
    }
}

/// `get_object_in_equip(ch, name)`: by id, by vnum when `name` is a number,
/// else by `N.keyword`.
pub fn get_object_in_equip<H: WorldHost + ?Sized>(host: &H, ch: Uid, name: &str) -> Option<Uid> {
    let worn = (0..NUM_WEARS).filter_map(|pos| host.equipment(ch, pos));
    if let Some(id) = parse_uid(name) {
        return worn.into_iter().find(|obj| *obj == id);
    }
    if is_number(name) {
        let vnum = atoi(name);
        return worn.into_iter().find(|obj| host.vnum(*obj) == vnum);
    }
    let (number, keyword) = get_number(name);
    if number == 0 {
        return None;
    }
    // The C counts matches up to `number`; `last.` (-1) therefore never
    // matches here.
    let mut seen = 0;
    for obj in worn {
        if isname(keyword, &host.keywords(obj)) {
            seen += 1;
            if seen == number {
                return Some(obj);
            }
        }
    }
    None
}

/// `find_eq_pos_script(arg)`: a wear position from its number or script name.
pub fn find_eq_pos_script(arg: &str) -> Option<i32> {
    const POSITIONS: &[(&str, i32)] = &[
        ("hold", WEAR_HOLD),
        ("held", WEAR_HOLD),
        ("light", WEAR_LIGHT),
        ("wield", WEAR_WIELD),
        ("rfinger", WEAR_FINGER_R),
        ("lfinger", WEAR_FINGER_L),
        ("neck1", WEAR_NECK_1),
        ("neck2", WEAR_NECK_2),
        ("body", WEAR_BODY),
        ("head", WEAR_HEAD),
        ("legs", WEAR_LEGS),
        ("feet", WEAR_FEET),
        ("hands", WEAR_HANDS),
        ("arms", WEAR_ARMS),
        ("shield", WEAR_SHIELD),
        ("about", WEAR_ABOUT),
        ("waist", WEAR_WAIST),
        ("rwrist", WEAR_WRIST_R),
        ("lwrist", WEAR_WRIST_L),
    ];
    if is_number(arg) {
        let pos = atoi(arg);
        if (0..NUM_WEARS).contains(&pos) {
            return Some(pos);
        }
    }
    POSITIONS
        .iter()
        .find(|(name, _)| str_eq(name, arg))
        .map(|(_, pos)| *pos)
}

/// `can_wear_on_pos(obj, pos)`: whether the object's wear flags allow this
/// equipment position.
pub fn can_wear_on_pos<H: WorldHost + ?Sized>(host: &H, obj: Uid, pos: i32) -> bool {
    // `ITEM_WEAR_*` bit numbers.
    let flag = match pos {
        WEAR_HOLD | WEAR_LIGHT => 14,
        WEAR_WIELD => 13,
        WEAR_FINGER_R | WEAR_FINGER_L => 1,
        WEAR_NECK_1 | WEAR_NECK_2 => 2,
        WEAR_BODY => 3,
        WEAR_HEAD => 4,
        WEAR_LEGS => 5,
        WEAR_FEET => 6,
        WEAR_HANDS => 7,
        WEAR_ARMS => 8,
        WEAR_SHIELD => 9,
        WEAR_ABOUT => 10,
        WEAR_WAIST => 11,
        WEAR_WRIST_R | WEAR_WRIST_L => 12,
        _ => return false,
    };
    host.wear_flags(obj) & (1 << flag) != 0
}

/// `item_in_list(item, list)` (`dg_variables.c`): how many objects in `list`
/// — and, recursively, inside any containers in it — `item` names, by id, by
/// vnum when it is a number, else by keyword.
pub fn item_in_list<H: WorldHost + ?Sized>(host: &H, item: &str, list: &[Uid]) -> i32 {
    fn count<H: WorldHost + ?Sized>(
        host: &H,
        list: &[Uid],
        matches: &dyn Fn(Uid) -> bool,
        depth: u32,
    ) -> i32 {
        let mut total = 0;
        for obj in list.iter().copied() {
            if matches(obj) {
                total += 1;
            }
            // The bound only guards against a corrupt containment cycle.
            if depth < 64 && host.is_container(obj) {
                total += count(host, &host.obj_contains(obj), matches, depth + 1);
            }
        }
        total
    }
    if item.is_empty() {
        return 0;
    }
    if let Some(id) = parse_uid(item) {
        count(host, list, &|obj| obj == id, 0)
    } else if is_number(item) {
        let vnum = atoi(item);
        count(host, list, &|obj| host.vnum(obj) == vnum, 0)
    } else {
        count(host, list, &|obj| isname(item, &host.keywords(obj)), 0)
    }
}

/// `trgvar_in_room(vnum)`: how many characters are in the room with this
/// vnum, or `None` when there is no such room (the C logs and returns `-1`).
pub fn trgvar_in_room<H: WorldHost + ?Sized>(host: &H, vnum: i32) -> Option<i32> {
    let room = host.real_room(vnum)?;
    i32::try_from(host.room_people(room).len()).ok()
}

/// `obj_room(obj)`: the room an object is ultimately in, through its carrier,
/// wearer or container.
pub fn obj_room<H: WorldHost + ?Sized>(host: &H, obj: Uid) -> Option<Uid> {
    let mut current = obj;
    // Containers nest; the bound only guards against a corrupt cycle.
    for _ in 0..64 {
        if let Some(room) = host.in_room(current) {
            return Some(room);
        }
        if let Some(holder) = host.obj_carried_by(current) {
            return host.in_room(holder);
        }
        if let Some(wearer) = host.obj_worn_by(current) {
            return host.in_room(wearer);
        }
        current = host.obj_in_obj(current)?;
    }
    None
}

/// `dg_room_of_obj(obj)`. Same walk as [`obj_room`]; the C versions differ
/// only in how they treat a carrier that is itself nowhere.
pub fn dg_room_of_obj<H: WorldHost + ?Sized>(host: &H, obj: Uid) -> Option<Uid> {
    let mut current = obj;
    for _ in 0..64 {
        if let Some(room) = host.in_room(current) {
            return Some(room);
        }
        if let Some(room) = host.obj_carried_by(current).and_then(|ch| host.in_room(ch)) {
            return Some(room);
        }
        if let Some(room) = host.obj_worn_by(current).and_then(|ch| host.in_room(ch)) {
            return Some(room);
        }
        current = host.obj_in_obj(current)?;
    }
    None
}

/// `get_obj(name)`: by id, or the first match in the object list.
pub fn get_obj<H: WorldHost + ?Sized>(host: &H, name: &str) -> Option<Uid> {
    match parse_uid(name) {
        Some(id) => find_obj(host, id),
        None => get_obj_in_list(host, name, &host.object_list()),
    }
}

/// `get_room(name)`: by id, or by the vnum `atoi(name)`.
pub fn get_room<H: WorldHost + ?Sized>(host: &H, name: &str) -> Option<Uid> {
    match parse_uid(name) {
        Some(id) => find_room(host, id),
        None => host.real_room(atoi(name)),
    }
}

/// `get_obj_near_obj(obj, name)`: `self`/`me`, the object's contents, its
/// container, else its wearer's equipment, else its carrier's inventory, else
/// its room and what the people there wear.
pub fn get_obj_near_obj<H: WorldHost + ?Sized>(host: &H, obj: Uid, name: &str) -> Option<Uid> {
    if str_eq(name, "self") || str_eq(name, "me") {
        return Some(obj);
    }
    if let Some(found) = get_obj_in_list(host, name, &host.obj_contains(obj)) {
        return Some(found);
    }
    // The C's `if / else if` chain: only the first placement that applies is
    // searched.
    if let Some(container) = host.obj_in_obj(obj) {
        let matches = match parse_uid(name) {
            Some(id) => id == container,
            None => isname(name, &host.keywords(container)),
        };
        return matches.then_some(container);
    }
    if let Some(wearer) = host.obj_worn_by(obj)
        && let Some(found) = get_object_in_equip(host, wearer, name)
    {
        return Some(found);
    }
    if let Some(carrier) = host.obj_carried_by(obj)
        && let Some(found) = get_obj_in_list(host, name, &host.carrying(carrier))
    {
        return Some(found);
    }
    let room = obj_room(host, obj)?;
    if let Some(found) = get_obj_in_list(host, name, &host.room_contents(room)) {
        return Some(found);
    }
    host.room_people(room)
        .into_iter()
        .find_map(|ch| get_object_in_equip(host, ch, name))
}

/// `get_obj_by_obj(obj, name)`: by id, `self`/`me`, contents, container,
/// wearer's equipment, carrier's inventory, room, then the world.
pub fn get_obj_by_obj<H: WorldHost + ?Sized>(host: &H, obj: Uid, name: &str) -> Option<Uid> {
    if let Some(id) = parse_uid(name) {
        return find_obj(host, id);
    }
    if str_eq(name, "self") || str_eq(name, "me") {
        return Some(obj);
    }
    if let Some(found) = get_obj_in_list(host, name, &host.obj_contains(obj)) {
        return Some(found);
    }
    if let Some(container) = host.obj_in_obj(obj)
        && isname(name, &host.keywords(container))
    {
        return Some(container);
    }
    if let Some(wearer) = host.obj_worn_by(obj)
        && let Some(found) = get_object_in_equip(host, wearer, name)
    {
        return Some(found);
    }
    if let Some(carrier) = host.obj_carried_by(obj)
        && let Some(found) = get_obj_in_list(host, name, &host.carrying(carrier))
    {
        return Some(found);
    }
    if let Some(room) = obj_room(host, obj)
        && let Some(found) = get_obj_in_list(host, name, &host.room_contents(room))
    {
        return Some(found);
    }
    get_obj(host, name)
}

/// `get_obj_in_room(room, name)`: by id or keyword, among the room's contents
/// only.
pub fn get_obj_in_room<H: WorldHost + ?Sized>(host: &H, room: Uid, name: &str) -> Option<Uid> {
    get_obj_in_list(host, name, &host.room_contents(room))
}

/// `get_obj_by_room(room, name)`: by id, in the room, then the world.
pub fn get_obj_by_room<H: WorldHost + ?Sized>(host: &H, room: Uid, name: &str) -> Option<Uid> {
    if let Some(id) = parse_uid(name) {
        return find_obj(host, id);
    }
    get_obj_in_list(host, name, &host.room_contents(room))
        .or_else(|| get_obj_in_list(host, name, &host.object_list()))
}

/// Where the `get_*_vis` family looks (`FIND_CHAR_ROOM`/`FIND_CHAR_WORLD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Room,
    World,
}

/// `get_player_vis(ch, name, &number, inroom)` with an explicit count: the
/// `number`th visible player whose name is exactly `name`.
fn player_vis<H: WorldHost + ?Sized>(
    host: &H,
    ch: Uid,
    name: &str,
    number: &mut i32,
    in_room: bool,
) -> Option<Uid> {
    let mut last = None;
    for i in host.character_list() {
        if host.is_npc(i)
            || (in_room && host.in_room(i) != host.in_room(ch))
            || !str_eq(&host.name(i), name)
            || !host.can_see(ch, i)
        {
            continue;
        }
        if *number == FIND_INDEX_LAST {
            last = Some(i);
            continue;
        }
        *number -= 1;
        if *number == 0 {
            return Some(i);
        }
    }
    last
}

/// `get_player_vis(ch, name, NULL, inroom)`: the count is read from `name`
/// (`2.bob`).
pub fn get_player_vis<H: WorldHost + ?Sized>(
    host: &H,
    ch: Uid,
    name: &str,
    in_room: bool,
) -> Option<Uid> {
    let (mut number, name) = get_number(name);
    player_vis(host, ch, name, &mut number, in_room)
}

/// The counted walk the `_vis` lookups share: the `number`th entry of `list`
/// that passes `matches`, decrementing `number` as the C does so a caller can
/// continue the count in another list.
fn nth_visible(
    list: impl IntoIterator<Item = Uid>,
    number: &mut i32,
    mut matches: impl FnMut(Uid) -> bool,
) -> Option<Uid> {
    let mut last = None;
    for entry in list {
        if *number == 0 {
            break;
        }
        if !matches(entry) {
            continue;
        }
        if *number == FIND_INDEX_LAST {
            last = Some(entry);
        } else {
            *number -= 1;
            if *number == 0 {
                return Some(entry);
            }
        }
    }
    last
}

fn char_room_vis<H: WorldHost + ?Sized>(
    host: &H,
    ch: Uid,
    name: &str,
    number: &mut i32,
) -> Option<Uid> {
    if str_eq(name, "self") || str_eq(name, "me") {
        return Some(ch);
    }
    if *number == 0 {
        // `0.<name>` means the player with that name.
        return get_player_vis(host, ch, name, true);
    }
    let room = host.in_room(ch)?;
    nth_visible(host.room_people(room), number, |i| {
        isname(name, &host.keywords(i)) && host.can_see(ch, i)
    })
}

/// `get_char_room_vis(ch, name, NULL)`: `self`/`me`, `0.name` for a player,
/// else the `N`th visible match in `ch`'s room.
pub fn get_char_room_vis<H: WorldHost + ?Sized>(host: &H, ch: Uid, name: &str) -> Option<Uid> {
    let (mut number, name) = get_number(name);
    char_room_vis(host, ch, name, &mut number)
}

/// `get_char_world_vis(ch, name, NULL)`: the room first, then the rest of the
/// world, the count running across both.
pub fn get_char_world_vis<H: WorldHost + ?Sized>(host: &H, ch: Uid, name: &str) -> Option<Uid> {
    let (mut number, name) = get_number(name);
    if let Some(found) = char_room_vis(host, ch, name, &mut number) {
        return Some(found);
    }
    if number == 0 {
        return get_player_vis(host, ch, name, false);
    }
    if number == FIND_INDEX_LAST {
        return None;
    }
    let room = host.in_room(ch);
    for i in host.character_list() {
        if host.in_room(i) == room || !isname(name, &host.keywords(i)) || !host.can_see(ch, i) {
            continue;
        }
        number -= 1;
        if number == 0 {
            return Some(i);
        }
    }
    None
}

/// `get_char_vis(ch, name, NULL, where)`.
pub fn get_char_vis<H: WorldHost + ?Sized>(
    host: &H,
    ch: Uid,
    name: &str,
    scope: Where,
) -> Option<Uid> {
    match scope {
        Where::Room => get_char_room_vis(host, ch, name),
        Where::World => get_char_world_vis(host, ch, name),
    }
}

/// `get_obj_in_list_vis(ch, name, NULL, list)`: the `N`th object in `list`
/// that `name` names and `ch` can see.
pub fn get_obj_in_list_vis<H: WorldHost + ?Sized>(
    host: &H,
    ch: Uid,
    name: &str,
    list: &[Uid],
) -> Option<Uid> {
    let (mut number, name) = get_number(name);
    nth_visible(list.iter().copied(), &mut number, |obj| {
        isname(name, &host.keywords(obj)) && host.can_see(ch, obj)
    })
}

/// `get_obj_vis(ch, name, NULL)`: inventory, then the room, then the world,
/// the count running across all three.
pub fn get_obj_vis<H: WorldHost + ?Sized>(host: &H, ch: Uid, name: &str) -> Option<Uid> {
    let (mut number, name) = get_number(name);
    if number == 0 {
        return None;
    }
    let sees = |obj: Uid| isname(name, &host.keywords(obj)) && host.can_see(ch, obj);
    if let Some(found) = nth_visible(host.carrying(ch), &mut number, sees) {
        return Some(found);
    }
    if let Some(room) = host.in_room(ch)
        && let Some(found) = nth_visible(host.room_contents(room), &mut number, sees)
    {
        return Some(found);
    }
    if number == FIND_INDEX_LAST {
        return None;
    }
    nth_visible(host.object_list(), &mut number, sees)
}

/// `get_obj_pos_in_equip_vis(ch, arg, NULL, ch->equipment)`: the wear
/// position of the `N`th visible worn object `arg` names.
pub fn get_obj_pos_in_equip_vis<H: WorldHost + ?Sized>(
    host: &H,
    ch: Uid,
    wearer: Uid,
    arg: &str,
) -> Option<i32> {
    let (mut number, name) = get_number(arg);
    if number == 0 {
        return None;
    }
    let mut last = None;
    for pos in 0..NUM_WEARS {
        let Some(obj) = host.equipment(wearer, pos) else {
            continue;
        };
        if !(host.can_see(ch, obj) && isname(name, &host.keywords(obj))) {
            continue;
        }
        if number == FIND_INDEX_LAST {
            last = Some(pos);
        } else {
            number -= 1;
            if number == 0 {
                return Some(pos);
            }
        }
    }
    last
}

/// `get_obj_in_equip_vis(ch, arg, NULL, ch->equipment)`.
pub fn get_obj_in_equip_vis<H: WorldHost + ?Sized>(
    host: &H,
    ch: Uid,
    wearer: Uid,
    arg: &str,
) -> Option<Uid> {
    get_obj_pos_in_equip_vis(host, ch, wearer, arg).and_then(|pos| host.equipment(wearer, pos))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::mock::MockHost;
    use crate::types::uid_text;

    #[test]
    fn isname_matches_abbreviations_but_not_abbreviated_numbers() {
        assert!(isname("guard", "cityguard guard"));
        assert!(isname("gua", "cityguard guard"));
        assert!(!isname("uard", "cityguard guard"));
        assert!(isname("two words", "two words"));
        assert!(isname("12", "12 key"));
        assert!(!isname("1", "12 key"));
        assert!(!isname("", "x"));
    }

    #[test]
    fn get_number_splits_the_count() {
        assert_eq!(get_number("sword"), (1, "sword"));
        assert_eq!(get_number("2.sword"), (2, "sword"));
        assert_eq!(get_number("last.sword"), (FIND_INDEX_LAST, "sword"));
        assert_eq!(get_number("x.sword"), (0, "sword"));
        assert_eq!(get_number(".sword"), (0, "sword"));
    }

    fn world() -> (MockHost, Uid, Uid, Uid, Uid, Uid) {
        let mut host = MockHost::new();
        let room = host.add_room(3001, "Temple");
        let guard = host.add_mob(3060, "the cityguard");
        host.set_text(guard, "alias", "cityguard guard");
        let second = host.add_mob(3060, "the cityguard");
        host.set_text(second, "alias", "cityguard guard");
        let bob = host.add_player("Bob");
        let sword = host.add_obj(3022, "a long sword");
        host.set_text(sword, "alias", "sword long");
        for ch in [guard, second, bob] {
            host.char_to_room(ch, room);
        }
        host.obj_to_char(sword, bob);
        (host, room, guard, second, bob, sword)
    }

    #[test]
    fn characters_resolve_by_id_and_by_keyword_in_list_order() {
        let (host, room, guard, second, bob, _) = world();
        assert_eq!(get_char(&host, &uid_text(guard)), Some(guard));
        assert_eq!(get_char(&host, "}999"), None);
        // `room_people` is head-first: bob entered last.
        assert_eq!(host.room_people(room), vec![bob, second, guard]);
        assert_eq!(get_char_in_room(&host, room, "guard"), Some(second));
        assert_eq!(get_char_room_vis(&host, bob, "2.guard"), Some(guard));
        assert_eq!(get_char_room_vis(&host, bob, "last.guard"), Some(guard));
        assert_eq!(get_char_room_vis(&host, bob, "me"), Some(bob));
        assert_eq!(get_char_room_vis(&host, guard, "0.bob"), Some(bob));
        assert_eq!(get_char_room_vis(&host, guard, "3.guard"), None);
    }

    #[test]
    fn objects_resolve_through_their_placement() {
        let (mut host, room, _, _, bob, sword) = world();
        assert_eq!(obj_room(&host, sword), Some(room));
        assert_eq!(get_obj_vis(&host, bob, "sword"), Some(sword));
        assert_eq!(get_obj_in_room(&host, room, "sword"), None);
        assert_eq!(get_obj_by_room(&host, room, "sword"), Some(sword));

        let bag = host.add_obj(3032, "a bag");
        host.obj_to_room(bag, room);
        host.obj_to_obj(sword, bag);
        assert_eq!(dg_room_of_obj(&host, sword), Some(room));
        assert_eq!(get_obj_near_obj(&host, sword, "bag"), Some(bag));
        assert_eq!(get_obj_near_obj(&host, bag, "sword"), Some(sword));
        assert_eq!(get_obj_near_obj(&host, bag, "self"), Some(bag));

        host.equip_char(bob, sword, WEAR_WIELD);
        assert_eq!(get_object_in_equip(&host, bob, "3022"), Some(sword));
        assert_eq!(get_object_in_equip(&host, bob, "sword"), Some(sword));
        assert_eq!(
            get_obj_pos_in_equip_vis(&host, bob, bob, "sword"),
            Some(WEAR_WIELD)
        );
        assert_eq!(find_eq_pos_script("wield"), Some(WEAR_WIELD));
        assert_eq!(find_eq_pos_script("5"), Some(WEAR_BODY));
        assert_eq!(find_eq_pos_script("tail"), None);
    }

    #[test]
    fn items_are_counted_through_containers() {
        let (mut host, room, _, _, bob, sword) = world();
        let bag = host.add_obj(3032, "a bag");
        host.set_num(bag, "type", 15);
        host.obj_to_char(bag, bob);
        let second = host.add_obj(3022, "a long sword");
        host.set_text(second, "alias", "sword long");
        host.obj_to_obj(second, bag);
        let carried = host.carrying(bob);
        assert_eq!(item_in_list(&host, "3022", &carried), 2);
        assert_eq!(item_in_list(&host, "sword", &carried), 2);
        assert_eq!(item_in_list(&host, &uid_text(sword), &carried), 1);
        assert_eq!(item_in_list(&host, "", &carried), 0);
        assert_eq!(trgvar_in_room(&host, 3001), Some(3));
        assert_eq!(trgvar_in_room(&host, 1), None);
        assert_eq!(host.room_people(room).len(), 3);

        host.set_num(sword, "wear", 1 << 13);
        assert!(can_wear_on_pos(&host, sword, WEAR_WIELD));
        assert!(!can_wear_on_pos(&host, sword, WEAR_HEAD));
    }

    #[test]
    fn rooms_resolve_by_id_or_vnum() {
        let (host, room, ..) = world();
        assert_eq!(get_room(&host, "3001"), Some(room));
        assert_eq!(get_room(&host, &uid_text(room)), Some(room));
        assert_eq!(get_room(&host, "9"), None);
        assert_eq!(find_room(&host, 5), None);
    }
}
