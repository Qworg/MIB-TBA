//! Exits and doors: the effects of `%door%`, `mdoor` and `wdoor`.
//!
//! A C exit is one record per direction holding a destination, flags, a key,
//! a keyword and a description. The pack splits it: the destination is
//! [`Exits`], a door is an entry of [`Doors`] (the keyword, key and
//! pickability) with its mutable state in [`RoomState`] (closed, locked). An
//! exit whose destination is `NOWHERE` is kept in [`DgNowhereExits`], since
//! [`Exits`] holds only real rooms.
//!
//! As in the C, only the room addressed changes; the exit back is not touched.

use std::collections::{BTreeMap, BTreeSet};

use mib_core::prelude::*;
use mib_stdlib::prelude::{Door, DoorState, Doors, Exits, ItemId, Room, RoomId, RoomState};

/// `EX_ISDOOR`, `EX_CLOSED`, `EX_LOCKED` and `EX_PICKPROOF`.
const EX_ISDOOR: i32 = 1;
const EX_CLOSED: i32 = 2;
const EX_LOCKED: i32 = 4;
const EX_PICKPROOF: i32 = 8;

/// The directions whose exit exists but leads `NOWHERE`.
#[derive(Component, Debug, Clone, Default)]
pub struct DgNowhereExits(pub BTreeSet<ExitKey>);

/// The key and keyword of an exit that is not a door right now. A C exit
/// holds them regardless of its flags, while the pack keeps them only on a
/// [`Door`]; they wait here and move onto the door when the flags make one,
/// and back here when the flags take it away.
#[derive(Component, Debug, Clone, Default)]
pub struct DgPendingDoors(pub BTreeMap<ExitKey, (Option<ItemId>, Option<String>)>);

/// One field of an exit, with the destination already resolved.
pub(super) enum Field {
    Description,
    Flags(i32),
    Key(i32),
    Keyword(String),
    ToRoom(Option<Entity>),
}

/// A fact the pack does not model: the stable key and a detail for
/// `BaseHost::unsupported`.
pub(super) type Unmodelled = (&'static str, &'static str);

/// The exit of a DG direction number (`NORTH` 0 to `SOUTHWEST` 9).
pub(super) fn dir_key(dir: usize) -> Option<ExitKey> {
    const DIRS: [ExitKey; 10] = [
        ExitKey::North,
        ExitKey::East,
        ExitKey::South,
        ExitKey::West,
        ExitKey::Up,
        ExitKey::Down,
        ExitKey::Northwest,
        ExitKey::Northeast,
        ExitKey::Southeast,
        ExitKey::Southwest,
    ];
    DIRS.get(dir).copied()
}

/// Whether the room has an exit that way, leading anywhere or `NOWHERE`.
fn has_exit(world: &World, room: Entity, key: ExitKey) -> bool {
    world
        .get::<Exits>(room)
        .is_some_and(|exits| exits.map.contains_key(&key))
        || world
            .get::<DgNowhereExits>(room)
            .is_some_and(|nowhere| nowhere.0.contains(&key))
}

/// The room a fresh exit points at: room number 0, the first room of the
/// world table (the room with the lowest vnum).
fn first_room(world: &mut World) -> Option<Entity> {
    let mut rooms = world.query::<(Entity, &Room)>();
    rooms
        .iter(world)
        .min_by_key(|(_, room)| room.num)
        .map(|(entity, _)| entity)
}

/// Run `f` on a component of the room, adding it first when missing.
fn with<C: Component<Mutability = bevy::ecs::component::Mutable> + Default, R>(
    world: &mut World,
    room: Entity,
    f: impl FnOnce(&mut C) -> R,
) -> Option<R> {
    if world.get::<C>(room).is_none() {
        world.entity_mut(room).insert(C::default());
    }
    world
        .get_mut::<C>(room)
        .map(|mut component| f(&mut component))
}

/// The key of the room the exit leads to, which identifies a door's state.
fn destination_key(world: &World, room: Entity, key: ExitKey) -> Option<RoomId> {
    let target = world.get::<Exits>(room)?.map.get(&key).copied()?;
    world
        .get::<Room>(target)
        .map(|room| RoomId(room.key.clone()))
}

/// `do_mdoor` "purge": free the exit, if there is one.
pub(super) fn purge(world: &mut World, room: Entity, dir: usize) {
    let Some(key) = dir_key(dir) else {
        return;
    };
    if world.get::<Room>(room).is_none() {
        return;
    }
    if let Some(mut exits) = world.get_mut::<Exits>(room) {
        exits.map.remove(&key);
        exits.hidden.remove(&key);
    }
    if let Some(mut doors) = world.get_mut::<Doors>(room) {
        doors.by_dir.remove(&key);
    }
    if let Some(mut state) = world.get_mut::<RoomState>(room) {
        state.doors.remove(&key);
    }
    if let Some(mut nowhere) = world.get_mut::<DgNowhereExits>(room) {
        nowhere.0.remove(&key);
    }
    if let Some(mut pending) = world.get_mut::<DgPendingDoors>(room) {
        pending.0.remove(&key);
    }
}

/// `do_mdoor` field assignment. An exit that does not exist is created first,
/// leading to the first room of the world, as the C `CREATE`s it with every
/// field zero. Returns what the pack could not hold, if anything.
pub(super) fn set(world: &mut World, room: Entity, dir: usize, field: Field) -> Option<Unmodelled> {
    let key = dir_key(dir)?;
    world.get::<Room>(room)?;
    if !has_exit(world, room, key) {
        let first = first_room(world)?;
        with(world, room, |exits: &mut Exits| {
            exits.map.insert(key, first);
        });
    }
    match field {
        Field::Description => Some((
            "exit.description",
            "the pack keeps no exit descriptions (look <direction>)",
        )),
        Field::Flags(flags) => {
            set_flags(world, room, key, flags);
            None
        }
        Field::Key(vnum) => {
            let item = u16::try_from(vnum)
                .ok()
                .filter(|vnum| *vnum != u16::MAX)
                .map(|vnum| ItemId::from(format!("tba_item_{vnum}")));
            let applied = set_door(world, room, key, |door| door.key = item.clone());
            if !applied {
                with(world, room, |pending: &mut DgPendingDoors| {
                    pending.0.entry(key).or_default().0 = item;
                });
            }
            None
        }
        Field::Keyword(text) => {
            let name = Some(text).filter(|text| !text.is_empty());
            let applied = set_door(world, room, key, |door| door.name = name.clone());
            if !applied {
                with(world, room, |pending: &mut DgPendingDoors| {
                    pending.0.entry(key).or_default().1 = name;
                });
            }
            None
        }
        Field::ToRoom(target) => {
            set_destination(world, room, key, target);
            None
        }
    }
}

/// Change the door in a direction, if the exit has one.
fn set_door(world: &mut World, room: Entity, key: ExitKey, f: impl FnOnce(&mut Door)) -> bool {
    world
        .get_mut::<Doors>(room)
        .and_then(|doors| doors.into_inner().by_dir.get_mut(&key).map(f))
        .is_some()
}

/// The exit's flags. A C exit that is closed or locked blocks passage whether
/// or not it is flagged `EX_ISDOOR`, and the pack blocks only at a door, so
/// any of `EX_ISDOOR`, `EX_CLOSED` and `EX_LOCKED` makes a [`Door`]. A closed
/// exit that is not `EX_ISDOOR` cannot be opened in the C (`DOOR_IS_OPENABLE`),
/// so it becomes a locked, unpickable door with no key, which no door command
/// opens; its real key waits in [`DgPendingDoors`] until `EX_ISDOOR` is set.
/// (A key set afterwards, while the exit is still not a door, would unlock it.)
/// `EX_PICKPROOF` makes a door unpickable. With none of the three flags the
/// exit has no door, and its key and keyword wait in [`DgPendingDoors`].
fn set_flags(world: &mut World, room: Entity, key: ExitKey, flags: i32) {
    let is_door = flags & EX_ISDOOR != 0;
    let blocked_only = !is_door && flags & (EX_CLOSED | EX_LOCKED) != 0;
    let closed = flags & EX_CLOSED != 0 || blocked_only;
    let locked = flags & EX_LOCKED != 0 || blocked_only;
    if flags & (EX_ISDOOR | EX_CLOSED | EX_LOCKED) == 0 {
        let removed = world
            .get_mut::<Doors>(room)
            .and_then(|mut doors| doors.by_dir.remove(&key));
        if let Some(door) = removed {
            with(world, room, |pending: &mut DgPendingDoors| {
                pending.0.insert(key, (door.key, door.name));
            });
        }
        if let Some(mut state) = world.get_mut::<RoomState>(room) {
            state.doors.remove(&key);
        }
        return;
    }
    let to = destination_key(world, room, key);
    let (held_key, held_name) = world
        .get_mut::<DgPendingDoors>(room)
        .and_then(|mut pending| pending.0.remove(&key))
        .unwrap_or_default();
    let mut held_stash = held_key.clone();
    with(world, room, |doors: &mut Doors| {
        let door = doors.by_dir.entry(key).or_insert_with(|| Door {
            key: held_key,
            name: held_name,
            pickable: true,
            difficulty: 0,
            default_closed: closed,
            default_locked: locked,
        });
        door.pickable = flags & EX_PICKPROOF == 0 && !blocked_only;
        door.default_closed = closed;
        door.default_locked = locked;
        if blocked_only {
            held_stash = door.key.take();
        } else if door.key.is_none() {
            door.key = held_stash.take();
        } else {
            held_stash = None;
        }
    });
    if held_stash.is_some() {
        with(world, room, |pending: &mut DgPendingDoors| {
            pending.0.entry(key).or_default().0 = held_stash;
        });
    }
    with(world, room, |state: &mut RoomState| {
        let door = state.doors.entry(key).or_insert_with(|| DoorState {
            closed,
            locked,
            to: to.clone(),
        });
        door.closed = closed;
        door.locked = locked;
        if door.to.is_none() {
            door.to = to;
        }
    });
}

/// The exit's destination: a room, or `NOWHERE`.
fn set_destination(world: &mut World, room: Entity, key: ExitKey, target: Option<Entity>) {
    match target {
        Some(target) => {
            let Some(to) = world.get::<Room>(target).map(|r| RoomId(r.key.clone())) else {
                return;
            };
            with(world, room, |exits: &mut Exits| {
                exits.map.insert(key, target);
            });
            if let Some(mut nowhere) = world.get_mut::<DgNowhereExits>(room) {
                nowhere.0.remove(&key);
            }
            // A door's saved state belongs to the door it was made for.
            if let Some(mut state) = world.get_mut::<RoomState>(room)
                && let Some(door) = state.doors.get_mut(&key)
            {
                door.to = Some(to);
            }
        }
        None => {
            if let Some(mut exits) = world.get_mut::<Exits>(room) {
                exits.map.remove(&key);
            }
            with(world, room, |nowhere: &mut DgNowhereExits| {
                nowhere.0.insert(key);
            });
        }
    }
}
