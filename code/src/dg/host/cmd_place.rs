//! Where things are: creating mobs and objects, moving them between rooms,
//! carriers, wear positions and containers, and removing them.
//!
//! These are free functions over the `World`; [`super`] wraps them in the
//! `CmdHost` methods, which resolve script ids and stamp the list order.

use bevy::ecs::system::SystemState;
use mib_core::prelude::*;
use mib_persist::prelude::DocKind;
use mib_stdlib::prelude::{
    CarriedBy, Concealment, ContainedBy, Container, Discovered, Exits, GroundItem, Item, ItemId,
    ItemQuery, Location, Npc, NpcBehaviors, NpcId, NpcIndex, NpcKey, OnlinePlayers, Perception,
    Player, QuestEvent, Room, RoomId, SaveId, SaveKind, VisibilityPolicy, find_autonomy_def,
    find_item_def, find_npc_def, next_item_save_id, spawn_npc_with_autonomy,
};

use crate::combat::{
    TbaAttackDice, TbaAttackerMemory, TbaCombatant, TbaNpcBase, TbaRng, TbaSpawnHome,
    fallback_combatant, npc_combatant_from_stats,
};
use crate::content::WearSlot;
use crate::dg::DgLog;
use crate::equipment::{TbaEquipment, TbaEquipmentSave};
use crate::items::TbaItemTimer;
use crate::module::TbaContent;

/// How far up a container chain to look for a cycle; deeper than any real
/// nesting.
const NESTING_LIMIT: usize = 64;

/// A player or an NPC.
pub(super) fn is_character(world: &World, entity: Entity) -> bool {
    world.get::<Player>(entity).is_some() || world.get::<Npc>(entity).is_some()
}

/// Seconds on the fixed clock, which a floor item's `spawned_at` is relative to.
fn fixed_elapsed(world: &World) -> f32 {
    world
        .get_resource::<Time<Fixed>>()
        .map_or(0.0, |time| time.elapsed_secs())
}

/// The `[bot, top]` vnum range of the zone holding `room`, if the pack
/// declares one.
fn zone_bounds(world: &World, room: Entity) -> Option<(u32, u32)> {
    let num = world.get::<Room>(room)?.num;
    world
        .get_resource::<TbaContent>()?
        .zones
        .values()
        .find(|zone| num >= zone.bot && num <= zone.top)
        .map(|zone| (zone.bot, zone.top))
}

/// Give an NPC its combat state now, the way `npc_combatant_system` would at
/// the next tick, so a script can fight or damage it at once. An NPC that
/// already has vitals keeps them.
pub(super) fn init_npc_combat(world: &mut World, npc: Entity) {
    if world.get::<TbaCombatant>(npc).is_some() {
        return;
    }
    let Some(key) = world.get::<NpcKey>(npc).map(|key| key.0.clone()) else {
        return;
    };
    let stats = world
        .get_resource::<TbaContent>()
        .and_then(|content| content.npc_stats.get(&key).cloned());
    let combatant = match &stats {
        Some(stats) => {
            let mut rng = world.get_resource_or_insert_with(TbaRng::default);
            npc_combatant_from_stats(stats, &mut rng)
        }
        None => fallback_combatant(),
    };
    let mut entity = world.entity_mut(npc);
    entity.insert((
        combatant.clone(),
        TbaNpcBase(combatant),
        TbaAttackerMemory::default(),
    ));
    if let Some(attack) = stats.and_then(|stats| stats.damage) {
        entity.insert(TbaAttackDice(attack));
    }
}

/// `read_mobile(vnum, VIRTUAL)`: a new mob in no room, with its combat state.
/// `None` when the pack has no mob with this vnum. The caller initialises
/// its script state.
pub(super) fn read_mobile(world: &mut World, vnum: i32) -> Option<Entity> {
    if !(0..=65_534).contains(&vnum) {
        return None;
    }
    let npc_id = NpcId::from(format!("tba_npc_{vnum}"));
    let def = find_npc_def(world, &npc_id)?;
    let autonomy = find_autonomy_def(world, &npc_id);
    let npc = spawn_npc_with_autonomy(world, None, &npc_id, &def, autonomy.as_ref());
    crate::dg::ids::stamp_spawn(world, npc);
    init_npc_combat(world, npc);
    Some(npc)
}

/// `read_object(vnum, VIRTUAL)`: a new object in no place. `None` when the
/// pack has no object with this vnum. The caller initialises its script
/// state.
pub(super) fn read_object(world: &mut World, vnum: i32) -> Option<Entity> {
    if !(0..=65_534).contains(&vnum) {
        return None;
    }
    let item_id = ItemId::from(format!("tba_item_{vnum}"));
    let def = find_item_def(world, &item_id)?;
    let save_id = next_item_save_id(world);
    let mut entity = world.spawn((
        Item {
            def_id: item_id,
            name: def.name.clone(),
            description: def.description.clone(),
            value: def.value,
            tags: def.tags.clone(),
        },
        save_id,
        SaveKind(DocKind::Item),
    ));
    if let Some(container) = &def.container {
        entity.insert(Container {
            capacity: container.capacity,
            contents: Vec::new(),
        });
    }
    let item = entity.id();
    crate::dg::ids::stamp_spawn(world, item);
    Some(item)
}

/// Take an item out of wherever it is: a room's floor, a carrier's inventory,
/// a wear position or a container. Afterwards it is in no place.
pub(super) fn lift_item(world: &mut World, item: Entity) {
    if let Some(holder) = world.get::<CarriedBy>(item).map(|carried| carried.0)
        && let Some(mut equipment) = world.get_mut::<TbaEquipment>(holder)
        && equipment.slots.values().any(|worn| *worn == item)
    {
        equipment.slots.retain(|_, worn| *worn != item);
        sync_equipment_save(world, holder);
    }
    if let Some(parent) = world.get::<ContainedBy>(item).map(|inside| inside.0)
        && let Some(mut container) = world.get_mut::<Container>(parent)
        && container.contents.contains(&item)
    {
        container.contents.retain(|held| *held != item);
    }
    if let Ok(mut entity) = world.get_entity_mut(item) {
        entity.remove::<(Location, GroundItem, CarriedBy, ContainedBy)>();
    }
}

/// Rewrite a player's persisted worn set from its runtime equipment, as the
/// pack's own equip and unequip commands do, so what a script wears or takes
/// off survives a relog. Items without a `SaveId` are left out.
fn sync_equipment_save(world: &mut World, holder: Entity) {
    if world.get::<Player>(holder).is_none() {
        return;
    }
    let Some(slots) = world
        .get::<TbaEquipment>(holder)
        .map(|equipment| equipment.slots.clone())
    else {
        return;
    };
    let worn = slots
        .iter()
        .filter_map(|(slot, item)| {
            let save_id = world.get::<SaveId>(*item)?;
            Some((*slot, save_id.0.to_string()))
        })
        .collect();
    world.entity_mut(holder).insert(TbaEquipmentSave { worn });
}

/// Whether `entity` is an item the placement functions may move.
fn is_item(world: &World, entity: Entity) -> bool {
    world.get::<Item>(entity).is_some()
}

/// `obj_to_room`. Returns whether the object moved.
pub(super) fn obj_to_room(world: &mut World, obj: Entity, room: Entity) -> bool {
    if !is_item(world, obj) || world.get::<Room>(room).is_none() {
        return false;
    }
    lift_item(world, obj);
    let spawned_at = fixed_elapsed(world);
    world
        .entity_mut(obj)
        .insert((Location { room }, GroundItem { spawned_at }));
    true
}

/// `obj_to_char`. Returns whether the object moved.
pub(super) fn obj_to_char(world: &mut World, obj: Entity, ch: Entity) -> bool {
    if !is_item(world, obj) || !is_character(world, ch) {
        return false;
    }
    lift_item(world, obj);
    world.entity_mut(obj).insert(CarriedBy(ch));
    true
}

/// Whether putting `obj` into `container` would make `obj` hold itself.
fn would_nest(world: &World, obj: Entity, container: Entity) -> bool {
    let mut at = container;
    for _ in 0..NESTING_LIMIT {
        if at == obj {
            return true;
        }
        match world.get::<ContainedBy>(at) {
            Some(inside) => at = inside.0,
            None => return false,
        }
    }
    true
}

/// `obj_to_obj`. Like the C it does not check the container's capacity.
/// Returns whether the object moved.
pub(super) fn obj_to_obj(world: &mut World, obj: Entity, container: Entity) -> bool {
    if !is_item(world, obj) || !is_item(world, container) || would_nest(world, obj, container) {
        return false;
    }
    lift_item(world, obj);
    world.entity_mut(obj).insert(ContainedBy(container));
    if let Some(mut holder) = world.get_mut::<Container>(container)
        && !holder.contents.contains(&obj)
    {
        holder.contents.push(obj);
    }
    true
}

/// `equip_char`: wear `obj` at `slot`. Like the C, a slot that is taken is a
/// logged error and the object stays where it was; `false` reports it.
pub(super) fn equip_char(world: &mut World, ch: Entity, obj: Entity, slot: WearSlot) -> bool {
    if !is_item(world, obj) || !is_character(world, ch) {
        return false;
    }
    let taken = world
        .get::<TbaEquipment>(ch)
        .is_some_and(|equipment| equipment.worn(slot).is_some_and(|worn| worn != obj));
    if taken {
        return false;
    }
    lift_item(world, obj);
    world.entity_mut(obj).insert(CarriedBy(ch));
    if world.get::<TbaEquipment>(ch).is_none() {
        world.entity_mut(ch).insert(TbaEquipment::default());
    }
    if let Some(mut equipment) = world.get_mut::<TbaEquipment>(ch) {
        equipment.slots.insert(slot, obj);
    }
    sync_equipment_save(world, ch);
    true
}

/// `unequip_char`: take the object at `slot` off and leave it in no place.
pub(super) fn unequip_char(world: &mut World, ch: Entity, slot: WearSlot) -> Option<Entity> {
    let obj = world.get::<TbaEquipment>(ch)?.worn(slot)?;
    lift_item(world, obj);
    Some(obj)
}

/// The object and everything inside it, nested containers included.
pub(super) fn obj_tree(world: &mut World, obj: Entity) -> Vec<Entity> {
    if !is_item(world, obj) {
        return Vec::new();
    }
    let mut inside = world.query::<(Entity, &ContainedBy)>();
    let nested: Vec<(Entity, Entity)> = inside
        .iter(world)
        .map(|(item, parent)| (item, parent.0))
        .collect();
    let mut doomed = vec![obj];
    let mut next = 0;
    while let Some(parent) = doomed.get(next).copied() {
        next += 1;
        for (item, holder) in &nested {
            if *holder == parent && !doomed.contains(item) {
                doomed.push(*item);
            }
        }
    }
    doomed
}

/// `extract_obj`: remove the object and everything inside it, at once.
pub(super) fn extract_obj(world: &mut World, doomed: &[Entity]) {
    if let Some(obj) = doomed.first() {
        lift_item(world, *obj);
    }
    for item in doomed {
        world.despawn(*item);
    }
}

/// `char_from_room`: the character stops fighting and is in no room.
pub(super) fn char_from_room(world: &mut World, ch: Entity) {
    if !is_character(world, ch) {
        return;
    }
    super::fight::stop_fighting(world, ch);
    world.entity_mut(ch).remove::<Location>();
}

/// `char_to_room`. Keeps the online registry in step for a player, records
/// the visit for quests, and shows a playing player the room. Returns
/// whether the character moved.
pub(super) fn char_to_room(world: &mut World, ch: Entity, room: Entity) -> bool {
    if world.get::<Room>(room).is_none() || !is_character(world, ch) {
        return false;
    }
    world.entity_mut(ch).insert(Location { room });
    if world.get::<Npc>(ch).is_some() && world.get::<TbaSpawnHome>(ch).is_none() {
        let zone = zone_bounds(world, room);
        world.entity_mut(ch).insert(TbaSpawnHome { room, zone });
    }
    if world.get::<Player>(ch).is_none() {
        return true;
    }
    if let Some(mut online) = world.get_resource_mut::<OnlinePlayers>() {
        for record in online.map.values_mut().filter(|record| record.entity == ch) {
            record.room = room;
        }
    }
    if !crate::dg::out::is_playing(world, ch) {
        return true;
    }
    let key = world.get::<Room>(room).map(|room| RoomId(room.key.clone()));
    if let Some(key) = key
        && let Some(mut events) = world.get_resource_mut::<Messages<QuestEvent>>()
    {
        events.write(QuestEvent::Visited {
            player: ch,
            room: key,
        });
    }
    show_room(world, ch, room);
    true
}

/// The parameters of the engine's scene projection.
type Scene<'w, 's> = (
    MessageWriter<'w, OutputEvent>,
    Query<'w, 's, &'static Connection>,
    Query<'w, 's, (&'static Room, &'static Exits)>,
    Query<'w, 's, (Entity, &'static Player, &'static Location)>,
    Query<'w, 's, (Entity, &'static Npc, &'static Location)>,
    Query<'w, 's, &'static Concealment>,
    Query<'w, 's, &'static Perception>,
    Res<'w, VisibilityPolicy>,
    ItemQuery<'w, 's>,
    Query<'w, 's, &'static Discovered>,
);

/// The scene of `room` for `player`, as the engine's own movement sends it.
/// Nothing is sent when the world has no output channel or visibility policy
/// (a bare test world).
fn show_room(world: &mut World, player: Entity, room: Entity) {
    if !world.contains_resource::<Messages<OutputEvent>>()
        || !world.contains_resource::<VisibilityPolicy>()
    {
        return;
    }
    let mut state = SystemState::<Scene>::new(world);
    let Ok((
        mut output,
        connections,
        rooms,
        players,
        npcs,
        concealment,
        perception,
        policy,
        items,
        discovered,
    )) = state.get_mut(world)
    else {
        return;
    };
    let seen = match (rooms.get(room), discovered.get(player)) {
        (Ok((room, _)), Ok(found)) => found.exits_in(&RoomId(room.key.clone())),
        _ => Default::default(),
    };
    mib_stdlib::prelude::emit_scene(
        &mut output,
        &connections,
        player,
        room,
        &rooms,
        &players,
        &npcs,
        &concealment,
        &perception,
        &policy,
        &items,
        &seen,
    );
}

/// `extract_char`, deferred: the character is marked and the runtime sweeps
/// it at the end of the outermost dispatch. Nobody keeps fighting it.
pub(super) fn extract_char(world: &mut World, ch: Entity) {
    if !is_character(world, ch) {
        return;
    }
    super::fight::stop_fighting(world, ch);
    super::fight::drop_targets(world, ch);
    super::fight::drop_followers(world, ch);
    forget_npc(world, ch);
    world.entity_mut(ch).insert(crate::dg::script::DgDead);
}

/// Take an NPC out of the engine's spawn index, which never forgets an
/// entity on its own; temporary mobs (the `dg_cast` proxy, the `mtransform`
/// template) would otherwise leave an entry each.
fn forget_npc(world: &mut World, npc: Entity) {
    let Some(key) = world.get::<NpcKey>(npc).map(|key| key.0.clone()) else {
        return;
    };
    if let Some(mut index) = world.get_resource_mut::<NpcIndex>()
        && let Some(entities) = index.map.get_mut(&key)
    {
        entities.retain(|entity| *entity != npc);
        if entities.is_empty() {
            index.map.remove(&key);
        }
    }
}

/// Forget the waits of removed entities.
pub(super) fn forget_waits(world: &mut World, uids: &[dg_vm::Uid]) {
    if let Some(mut waits) = world.get_resource_mut::<crate::dg::waits::DgWaits>() {
        for uid in uids {
            waits.cancel_owner(*uid);
        }
    }
}

/// `memcpy(ch, &tmpmob, sizeof(*ch))` of `do_mtransform`, for what the pack
/// models of a mob: its descriptions, behaviour flags, combat statistics and
/// attack dice. Facts the pack looks up by the mob's prototype key (level,
/// alignment, rewards, aggression and memory behaviour) stay those of the
/// vnum the mob was created from; the caller reports that gap. The
/// character keeps its prototype key (so its vnum), position, target,
/// memory, home, script, items and followers; with `keep_hp` it also keeps
/// its hit points.
pub(super) fn overwrite_mob(world: &mut World, ch: Entity, from: Entity, keep_hp: bool) {
    if ch == from || world.get::<Npc>(ch).is_none() {
        return;
    }
    let Some((name, persona)) = world
        .get::<Npc>(from)
        .map(|npc| (npc.name.clone(), npc.persona.clone()))
    else {
        return;
    };
    if let Some(mut npc) = world.get_mut::<Npc>(ch) {
        npc.name = name;
        npc.persona = persona;
    }
    let old = world.get::<TbaCombatant>(ch).cloned();
    if let Some(mut next) = world.get::<TbaCombatant>(from).cloned() {
        let mut base = world
            .get::<TbaNpcBase>(from)
            .map_or_else(|| next.clone(), |base| base.0.clone());
        if let Some(old) = &old {
            next.position = old.position;
            next.target = old.target;
            if keep_hp {
                next.hp = old.hp;
                next.max_hp = old.max_hp;
                base.hp = old.hp;
                base.max_hp = old.max_hp;
            }
        }
        world.entity_mut(ch).insert((next, TbaNpcBase(base)));
    }
    let dice = world.get::<TbaAttackDice>(from).copied();
    let behaviors = world.get::<NpcBehaviors>(from).copied();
    let mut entity = world.entity_mut(ch);
    if let Some(behaviors) = behaviors {
        entity.insert(behaviors);
    }
    match dice {
        Some(dice) => {
            entity.insert(dice);
        }
        None => {
            entity.remove::<TbaAttackDice>();
        }
    }
}

/// `memcpy(obj, &tmpobj, sizeof(*obj))` of `do_otransform`: the object takes
/// the prototype data of `from` (so its vnum) and keeps its place, contents
/// and script.
pub(super) fn overwrite_obj(world: &mut World, obj: Entity, from: Entity) {
    if obj == from || !is_item(world, obj) {
        return;
    }
    let Some(item) = world.get::<Item>(from).cloned() else {
        return;
    };
    let capacity = world
        .get::<Container>(from)
        .map(|container| container.capacity);
    // The timer is the new prototype's (the template has not been through the
    // pack's timer attachment), as `attach_item_timers` would give it.
    let timer = world
        .get_resource::<TbaContent>()
        .and_then(|content| content.items.get(&item.def_id))
        .and_then(|def| def.timer)
        .filter(|timer| *timer > 0)
        .map(|remaining| TbaItemTimer { remaining });
    let held = world
        .get::<Container>(obj)
        .map(|container| container.contents.clone());
    let mut entity = world.entity_mut(obj);
    entity.insert(item);
    match (capacity, held) {
        (Some(capacity), held) => {
            entity.insert(Container {
                capacity,
                contents: held.unwrap_or_default(),
            });
        }
        (None, Some(held)) if held.is_empty() => {
            entity.remove::<Container>();
        }
        (None, _) => {}
    }
    match timer {
        Some(timer) => {
            entity.insert(timer);
        }
        None => {
            entity.remove::<TbaItemTimer>();
        }
    }
}

/// Record a line in the script log's sinks.
pub(super) fn log_line(world: &mut World, key: &str, line: String) {
    tracing::info!(target: "dg", "{line}");
    world
        .get_resource_or_insert_with(DgLog::default)
        .record(key, line);
}
