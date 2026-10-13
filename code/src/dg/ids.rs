//! Entity ids: the mapping between ECS entities and the VM's `Uid`.
//!
//! DG names every entity by a script id whose range says what it is
//! (`dg_vm::types`): players below `MOB_ID_BASE`, mobs from `MOB_ID_BASE`,
//! rooms `ROOM_ID_BASE + vnum`, objects from `OBJ_ID_BASE`. [`DgId`] is the id
//! an entity carries and [`DgIds`] the reverse index.
//!
//! The C walks linked lists whose order scripts can observe (`room->people`,
//! `ch->carrying`, `character_list`); an ECS has none. [`DgOrder`] records the
//! moment an entity last changed place and [`DgBirth`] the moment it was
//! created, both drawn from one counter in [`DgIds`], and the world-graph
//! queries sort by them to reproduce the C's list order.

use std::collections::HashMap;

use crate::equipment::TbaEquipment;
use dg_vm::Uid;
use dg_vm::types::{MOB_ID_BASE, OBJ_ID_BASE, ROOM_ID_BASE};
use mib_stdlib::prelude::{
    CarriedBy, ContainedBy, Item, Location, Npc, PendingHolder, Player, Room,
};

use bevy::ecs::system::SystemChangeTick;
use mib_core::prelude::*;

use super::host::EcsHost;

/// The script id of an entity.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DgId(pub Uid);

/// When the entity last changed place (created, moved to a room, picked up,
/// put in a container). Higher is newer.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DgOrder(pub u64);

/// When the entity was created. Higher is newer. Unlike [`DgOrder`] it never
/// changes, which is what `character_list` and `object_list` order by.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DgBirth(pub u64);

/// The id index and the counters ids and list order are drawn from.
#[derive(Resource, Debug)]
pub struct DgIds {
    map: HashMap<Uid, Entity>,
    /// The last mob id handed out.
    next_mob: Uid,
    /// The last object id handed out.
    next_obj: Uid,
    /// The last player id handed out from the top of the player range.
    next_player: Uid,
    /// The last list-order stamp handed out.
    clock: u64,
}

impl Default for DgIds {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
            next_mob: MOB_ID_BASE,
            next_obj: OBJ_ID_BASE,
            next_player: MOB_ID_BASE,
            clock: 0,
        }
    }
}

impl DgIds {
    /// The entity last recorded for this id. It may have been despawned since;
    /// [`EcsHost::entity`] checks.
    pub fn get(&self, uid: Uid) -> Option<Entity> {
        self.map.get(&uid).copied()
    }

    /// How many ids are recorded.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether no id is recorded.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The next list-order stamp: larger than every earlier one.
    pub fn stamp(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// Record an id for an entity.
    fn insert(&mut self, uid: Uid, entity: Entity) {
        self.map.insert(uid, entity);
    }

    /// Whether the id is recorded for a live entity other than `entity`.
    fn taken(&self, world: &World, uid: Uid, entity: Entity) -> bool {
        self.map
            .get(&uid)
            .is_some_and(|other| *other != entity && world.get_entity(*other).is_ok())
    }

    /// The next free id in `(base, limit)`, counting up from `*cursor` and
    /// wrapping once to the bottom of the range. `None` when the range is full.
    fn next_in_range(
        &self,
        world: &World,
        cursor: &mut Uid,
        base: Uid,
        limit: Uid,
        entity: Entity,
    ) -> Option<Uid> {
        let span = limit.saturating_sub(base).max(0);
        for _ in 0..span {
            let next = cursor.saturating_add(1);
            *cursor = if next >= limit { base + 1 } else { next };
            if !self.taken(world, *cursor, entity) {
                return Some(*cursor);
            }
        }
        None
    }

    /// The next free player id, counting down from the top of the player range.
    fn next_player_id(&mut self, world: &World, entity: Entity) -> Option<Uid> {
        for _ in 0..MOB_ID_BASE {
            let next = self.next_player.saturating_sub(1);
            self.next_player = if next < 1 { MOB_ID_BASE - 1 } else { next };
            if !self.taken(world, self.next_player, entity) {
                return Some(self.next_player);
            }
        }
        None
    }
}

/// The trailing decimal number of a converter id (`tba_npc_3001`, `tba.22`),
/// the vnum it was made from. `None` when the id does not end in digits or the
/// number is not a valid 16-bit vnum (the reference build's `IDXTYPE`).
pub fn vnum_from_id(id: &str) -> Option<i32> {
    let digits = id.len() - id.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let tail = id.get(id.len() - digits..)?;
    let vnum: i32 = tail.parse().ok()?;
    (0..=65_534).contains(&vnum).then_some(vnum)
}

/// Whether an item is out of the game because it, or the container it sits in
/// (at any depth), is parked for an offline holder (`PendingHolder`). The C
/// extracts a quitting player's whole inventory tree; the pack keeps it aside
/// and re-links it at the next login.
pub fn is_parked(world: &World, entity: Entity) -> bool {
    let mut at = entity;
    // The bound only guards against a containment cycle.
    for _ in 0..64 {
        if world.get::<PendingHolder>(at).is_some() {
            return true;
        }
        match world.get::<ContainedBy>(at) {
            Some(container) => at = container.0,
            None => return false,
        }
    }
    false
}

/// What kind of entity is getting its first id.
enum Fresh {
    Player(u64),
    Npc,
    Item,
}

/// Give every room, NPC, item and player a [`DgId`], and every character and
/// item its list-order stamps. Entities that already have an id keep it; the
/// index forgets entities that no longer exist.
pub fn ensure_ids(world: &mut World) {
    world.init_resource::<DgIds>();
    world.resource_scope(|world, mut ids: Mut<DgIds>| {
        ids.map
            .retain(|_, entity| world.get_entity(*entity).is_ok());

        let mut rooms = world.query_filtered::<(Entity, &Room), Without<DgId>>();
        let fresh_rooms: Vec<(Entity, u32)> = rooms
            .iter(world)
            .map(|(entity, room)| (entity, room.num))
            .collect();
        for (entity, num) in fresh_rooms {
            if num > 65_534 {
                tracing::debug!("room {num} is outside the vnum range and gets no script id");
                continue;
            }
            let uid = ROOM_ID_BASE.saturating_add(Uid::from(num));
            if ids.taken(world, uid, entity) {
                // Two rooms with one vnum: the second is unreachable by id.
                continue;
            }
            ids.insert(uid, entity);
            world.entity_mut(entity).insert(DgId(uid));
        }

        // Characters and items stamped at their spawn site (`stamp_spawn`) are
        // given ids in that order. The rest were created by code that does not
        // stamp; they follow, in entity-index order, which is creation order
        // only until the allocator recycles an index.
        let mut fresh: Vec<(Entity, Fresh)> = Vec::new();
        let mut players = world.query_filtered::<(Entity, &Player), Without<DgId>>();
        fresh.extend(
            players
                .iter(world)
                .map(|(entity, player)| (entity, Fresh::Player(player.account_id))),
        );
        let mut npcs = world.query_filtered::<Entity, (With<Npc>, Without<DgId>)>();
        fresh.extend(npcs.iter(world).map(|entity| (entity, Fresh::Npc)));
        let mut items = world.query_filtered::<Entity, (With<Item>, Without<DgId>)>();
        fresh.extend(items.iter(world).map(|entity| (entity, Fresh::Item)));
        fresh.sort_by_key(|(entity, _)| {
            let birth = world.get::<DgBirth>(*entity).map_or(u64::MAX, |b| b.0);
            (birth, entity.index(), entity.to_bits())
        });

        for (entity, what) in fresh {
            let uid = match what {
                Fresh::Player(account) => Uid::try_from(account)
                    .ok()
                    .filter(|uid| (1..MOB_ID_BASE).contains(uid))
                    .filter(|uid| !ids.taken(world, *uid, entity))
                    .or_else(|| ids.next_player_id(world, entity)),
                Fresh::Npc => {
                    let mut cursor = ids.next_mob;
                    let uid =
                        ids.next_in_range(world, &mut cursor, MOB_ID_BASE, ROOM_ID_BASE, entity);
                    ids.next_mob = cursor;
                    uid
                }
                Fresh::Item => {
                    let mut cursor = ids.next_obj;
                    let uid = ids.next_in_range(world, &mut cursor, OBJ_ID_BASE, Uid::MAX, entity);
                    ids.next_obj = cursor;
                    uid
                }
            };
            let Some(uid) = uid else {
                continue;
            };
            let stamp = ids.stamp();
            ids.insert(uid, entity);
            let mut fresh_entity = world.entity_mut(entity);
            fresh_entity.insert(DgId(uid));
            if fresh_entity.get::<DgBirth>().is_none() {
                fresh_entity.insert(DgBirth(stamp));
            }
            if fresh_entity.get::<DgOrder>().is_none() {
                fresh_entity.insert(DgOrder(stamp));
            }
        }
        // Placement changes the game makes after this pass are newer than the
        // stamps it just gave.
        world.increment_change_tick();
    });
}

/// Record the moment a character or item was created, for code that spawns
/// several in one run: their ids and list order follow the order they were
/// stamped in, however the entity allocator numbered them.
pub fn stamp_spawn(world: &mut World, entity: Entity) {
    let stamp = world.get_resource_or_insert_with(DgIds::default).stamp();
    if let Ok(mut spawned) = world.get_entity_mut(entity) {
        spawned.insert((DgBirth(stamp), DgOrder(stamp)));
    }
    world.increment_change_tick();
}

/// Entities whose placement changed.
type Moved = Or<(Changed<Location>, Changed<CarriedBy>, Changed<ContainedBy>)>;

/// The placement-changed entities with their stamp and placement components.
type MovedQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        Ref<'static, DgOrder>,
        Option<Ref<'static, Location>>,
        Option<Ref<'static, CarriedBy>>,
        Option<Ref<'static, ContainedBy>>,
    ),
    Moved,
>;

/// `FixedUpdate`: restamp every character and item whose placement the rest
/// of the game changed since this system last ran, so a newly arrived player
/// is at the head of the room's `people` and a newly carried item at the head
/// of `carrying`. A placement change older than the entity's stamp (one the
/// host made and stamped itself, or one before the entity's first stamp) is
/// already ordered and keeps that stamp; a change newer than the stamp
/// restamps it. A worn item taken off is restamped too, because unequipping
/// puts it at the head of `carrying` (`obj_to_char`).
///
/// Moves made between two runs of this system are stamped oldest change tick
/// first; moves within one tick (one system run, or the stretch between two
/// tick bumps) share a tick and are stamped in entity order. A dispatch run outside `FixedUpdate` straight after a game move
/// must call [`EcsHost::stamp_order`] on the mover first.
pub fn stamp_placement_order(
    mut ids: ResMut<DgIds>,
    ticks: SystemChangeTick,
    mut queries: ParamSet<(MovedQuery, Query<&mut DgOrder>)>,
    equipped: Query<(), With<TbaEquipment>>,
    holders: Query<(Entity, &TbaEquipment), Changed<TbaEquipment>>,
    mut worn_before: Local<HashMap<Entity, Vec<Entity>>>,
) {
    let mut moved: Vec<(u32, Entity)> = Vec::new();
    for (entity, order, location, carried, contained) in &queries.p0() {
        let stamped = order.last_changed();
        let newest = [
            location.map(|r| r.last_changed()),
            carried.map(|r| r.last_changed()),
            contained.map(|r| r.last_changed()),
        ]
        .into_iter()
        .flatten()
        .filter(|tick| tick.is_newer_than(stamped, ticks.this_run()))
        .map(|tick| ticks.this_run().get().wrapping_sub(tick.get()))
        .min();
        if let Some(age) = newest {
            moved.push((age, entity));
        }
    }
    // Oldest move first, so the newest arrival gets the highest stamp.
    moved.sort_by_key(|(age, entity)| (std::cmp::Reverse(*age), entity.index()));
    for (_, entity) in moved {
        if let Ok(mut order) = queries.p1().get_mut(entity) {
            order.0 = ids.stamp();
        }
    }

    for (holder, equipment) in &holders {
        let now: Vec<Entity> = equipment.slots.values().copied().collect();
        let before = worn_before.insert(holder, now.clone()).unwrap_or_default();
        for item in before.into_iter().filter(|item| !now.contains(item)) {
            if let Ok(mut order) = queries.p1().get_mut(item) {
                order.0 = ids.stamp();
            }
        }
    }
    worn_before.retain(|holder, _| equipped.contains(*holder));
}

impl EcsHost<'_> {
    /// The live entity a script id names.
    pub fn entity(&self, uid: Uid) -> Option<Entity> {
        let entity = self.world.get_resource::<DgIds>()?.get(uid)?;
        self.world.get_entity(entity).ok()?;
        if is_parked(self.world, entity) {
            return None;
        }
        (self.world.get::<DgId>(entity)?.0 == uid).then_some(entity)
    }

    /// The entity a script id names, whether or not it is parked for an
    /// offline holder.
    pub fn entity_or_parked(&self, uid: Uid) -> Option<Entity> {
        let entity = self.world.get_resource::<DgIds>()?.get(uid)?;
        self.world.get_entity(entity).ok()?;
        (self.world.get::<DgId>(entity)?.0 == uid).then_some(entity)
    }

    /// The script id of an entity, if it has one.
    pub fn uid(&self, entity: Entity) -> Option<Uid> {
        self.world.get::<DgId>(entity).map(|id| id.0)
    }

    /// Stamp the entity as having just changed place, after the host moved it.
    pub fn stamp_order(&mut self, entity: Entity) {
        let stamp = self
            .world
            .get_resource_or_insert_with(DgIds::default)
            .stamp();
        if self.world.get_entity(entity).is_ok() {
            self.world.entity_mut(entity).insert(DgOrder(stamp));
            // Anything the game changes from here on is newer than the stamp.
            self.world.increment_change_tick();
        }
    }
}
