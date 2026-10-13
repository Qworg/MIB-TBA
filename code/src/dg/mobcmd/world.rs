//! The world changes an NPC's commands make: positions and fighting, purses,
//! item placement, worn equipment, character moves, following, and the
//! text-level helpers the command ports share.
//!
//! Everything here tolerates a despawned entity or a missing component by
//! doing nothing (a write) or answering the neutral value (a read).

use std::collections::BTreeMap;

use bevy::ecs::system::RunSystemOnce;
use dg_vm::Uid;
use mib_core::prelude::*;
use mib_protocol::prelude::ExitKey;
use mib_stdlib::prelude::{
    CarriedBy, Container, ContainedBy, GroundItem, Item, ItemId, Location, Npc, NpcKey,
    OnlinePlayers, Player, QuestEvent, Room, SaveId,
};

use super::table;
use crate::combat::{
    GOLD_ITEM_DESCRIPTION, GOLD_ITEM_ID, GOLD_ITEM_NAME, TbaAttackerMemory, TbaCombatant, TbaGold,
    TbaSneaking, npc_combatant_system,
};
use crate::content::{Position, TbaItemKind, WearSlot};
use crate::dg::host::EcsHost;
use crate::equipment::{TbaEquipment, TbaEquipmentSave};
use crate::module::TbaContent;
use crate::party::TbaFollowing;

/// The purse of an NPC, once a command has changed it. The pack keeps NPC gold
/// only as prototype data (`TbaNpcStats::gold`), so an NPC that has never
/// given or received coins has no component and its purse is the
/// prototype's; the first change writes this component.
#[derive(Debug, Clone, Copy, Component, PartialEq, Eq)]
pub struct TbaNpcGold(pub u32);

// -- positions and fighting ---------------------------------------------------

/// The numbers of the C's `POS_*` ladder, which the command table uses.
pub fn position_number(position: Position) -> i32 {
    match position {
        Position::Dead => table::DEAD,
        Position::MortallyWounded => table::MORTALLY_WOUNDED,
        Position::Incapacitated => table::INCAPACITATED,
        Position::Stunned => table::STUNNED,
        Position::Sleeping => table::SLEEPING,
        Position::Resting => table::RESTING,
        Position::Sitting => table::SITTING,
        Position::Fighting => table::FIGHTING,
        Position::Standing => table::STANDING,
    }
}

/// `GET_POS(ch)`. An NPC the combat systems have not reached yet is in its
/// prototype's default position; a character that was extracted is dead.
pub fn position(host: &EcsHost<'_>, ch: Uid) -> Position {
    let Some(entity) = host.entity(ch) else {
        return Position::Dead;
    };
    if host.world.get::<crate::dg::script::DgDead>(entity).is_some() {
        return Position::Dead;
    }
    if let Some(combatant) = host.world.get::<TbaCombatant>(entity) {
        return combatant.position;
    }
    host.world
        .get::<NpcKey>(entity)
        .and_then(|key| {
            host.world
                .get_resource::<TbaContent>()?
                .npc_stats
                .get(&key.0)
                .map(|stats| stats.default_position)
        })
        .unwrap_or(Position::Standing)
}

/// `AWAKE(ch)`: above sleeping.
pub fn awake(host: &EcsHost<'_>, ch: Uid) -> bool {
    position(host, ch) > Position::Sleeping
}

/// Make sure an NPC has the combat state its position lives in, by running
/// the system that would give it one at the next tick.
fn ensure_combatant(world: &mut World, entity: Entity) {
    if world.get::<TbaCombatant>(entity).is_some() || world.get::<Npc>(entity).is_none() {
        return;
    }
    // `Err` means the combat resources are not installed; the write below
    // then has nothing to land on.
    let _ = world.run_system_once(npc_combatant_system);
}

/// `GET_POS(ch) = position`.
pub fn set_position(host: &mut EcsHost<'_>, ch: Uid, position: Position) {
    let Some(entity) = host.entity(ch) else {
        return;
    };
    ensure_combatant(host.world, entity);
    if let Some(mut combatant) = host.world.get_mut::<TbaCombatant>(entity) {
        combatant.position = position;
    }
}

/// `FIGHTING(ch)`: the live target of the character's fight.
pub fn fighting(host: &EcsHost<'_>, ch: Uid) -> Option<Uid> {
    let entity = host.entity(ch)?;
    let target = host.world.get::<TbaCombatant>(entity)?.target?;
    let uid = host.uid(target)?;
    host.entity(uid).map(|_| uid)
}

/// `set_fighting(ch, vict)`: aim `ch` at `vict`, and stand `ch` up into the
/// fight. A target already set is replaced.
pub fn set_fighting(host: &mut EcsHost<'_>, ch: Uid, vict: Uid) {
    let (Some(entity), Some(target)) = (host.entity(ch), host.entity(vict)) else {
        return;
    };
    ensure_combatant(host.world, entity);
    if let Some(mut combatant) = host.world.get_mut::<TbaCombatant>(entity) {
        combatant.target = Some(target);
        if combatant.position == Position::Standing {
            combatant.position = Position::Fighting;
        }
    }
    if let Some(mut memory) = host.world.get_mut::<TbaAttackerMemory>(target) {
        memory.0.insert(entity);
    }
}

/// `stop_fighting(ch)`.
pub fn stop_fighting(host: &mut EcsHost<'_>, ch: Uid) {
    let Some(entity) = host.entity(ch) else {
        return;
    };
    if let Some(mut combatant) = host.world.get_mut::<TbaCombatant>(entity) {
        combatant.target = None;
        if combatant.position == Position::Fighting {
            combatant.position = Position::Standing;
        }
    }
}

// -- purses -------------------------------------------------------------------

/// `GET_GOLD(ch)`.
pub fn gold(host: &EcsHost<'_>, ch: Uid) -> i64 {
    let Some(entity) = host.entity(ch) else {
        return 0;
    };
    if host.world.get::<Player>(entity).is_some() {
        return host
            .world
            .get::<TbaGold>(entity)
            .map_or(0, |gold| i64::from(gold.0));
    }
    if let Some(gold) = host.world.get::<TbaNpcGold>(entity) {
        return i64::from(gold.0);
    }
    host.world
        .get::<NpcKey>(entity)
        .and_then(|key| {
            host.world
                .get_resource::<TbaContent>()?
                .npc_stats
                .get(&key.0)
                .map(|stats| i64::from(stats.gold))
        })
        .unwrap_or(0)
}

/// `GET_GOLD(ch) = amount`, clamped to what a purse holds.
pub fn set_gold(host: &mut EcsHost<'_>, ch: Uid, amount: i64) {
    let Some(entity) = host.entity(ch) else {
        return;
    };
    let amount = u32::try_from(amount.max(0)).unwrap_or(u32::MAX);
    if host.world.get::<Player>(entity).is_some() {
        match host.world.get_mut::<TbaGold>(entity) {
            Some(mut gold) => gold.0 = amount,
            None => {
                host.world.entity_mut(entity).insert(TbaGold(amount));
            }
        }
    } else if host.world.get::<Npc>(entity).is_some() {
        host.world.entity_mut(entity).insert(TbaNpcGold(amount));
    }
}

// -- item placement -----------------------------------------------------------

/// Take an item out of whatever holds it: the room floor, a holder's hands
/// (or worn slot), or a container. The item is left with no placement.
pub fn detach_item(host: &mut EcsHost<'_>, obj: Entity) {
    if host.world.get_entity(obj).is_err() {
        return;
    }
    if let Some(holder) = host.world.get::<CarriedBy>(obj).map(|carried| carried.0) {
        let mut changed = false;
        if let Some(mut equipment) = host.world.get_mut::<TbaEquipment>(holder) {
            let before = equipment.slots.len();
            equipment.slots.retain(|_, worn| *worn != obj);
            changed = equipment.slots.len() != before;
        }
        if changed {
            write_equipment_save(host.world, holder);
        }
    }
    if let Some(container) = host.world.get::<ContainedBy>(obj).map(|inside| inside.0)
        && let Some(mut contents) = host.world.get_mut::<Container>(container)
    {
        contents.contents.retain(|entry| *entry != obj);
    }
    host.world
        .entity_mut(obj)
        .remove::<(Location, GroundItem, CarriedBy, ContainedBy)>();
}

/// `obj_to_char(obj, ch)`.
pub fn obj_to_char(host: &mut EcsHost<'_>, obj: Entity, ch: Entity) {
    if host.world.get_entity(ch).is_err() || host.world.get_entity(obj).is_err() {
        return;
    }
    detach_item(host, obj);
    host.world.entity_mut(obj).insert(CarriedBy(ch));
    host.stamp_order(obj);
}

/// `obj_to_room(obj, room)`.
pub fn obj_to_room(host: &mut EcsHost<'_>, obj: Entity, room: Entity) {
    if host.world.get_entity(room).is_err() || host.world.get_entity(obj).is_err() {
        return;
    }
    detach_item(host, obj);
    let spawned_at = host
        .world
        .get_resource::<Time<Fixed>>()
        .map_or(0.0, |time| time.elapsed_secs());
    host.world
        .entity_mut(obj)
        .insert((Location { room }, GroundItem { spawned_at }));
    host.stamp_order(obj);
}

/// `obj_to_obj(obj, cont)`.
pub fn obj_to_obj(host: &mut EcsHost<'_>, obj: Entity, cont: Entity) {
    if obj == cont || host.world.get_entity(cont).is_err() || host.world.get_entity(obj).is_err() {
        return;
    }
    detach_item(host, obj);
    let (capacity, mut contents) = host
        .world
        .get::<Container>(cont)
        .map(|container| (container.capacity, container.contents.clone()))
        .unwrap_or((u32::MAX, Vec::new()));
    if !contents.contains(&obj) {
        contents.push(obj);
    }
    host.world
        .entity_mut(cont)
        .insert(Container { capacity, contents });
    host.world.entity_mut(obj).insert(ContainedBy(cont));
    host.stamp_order(obj);
}

/// `extract_obj(obj)`: the item and everything inside it leave the game.
pub fn extract_obj(host: &mut EcsHost<'_>, obj: Entity) {
    detach_item(host, obj);
    let mut doomed = vec![obj];
    let mut at = 0;
    while let Some(entity) = doomed.get(at).copied() {
        at += 1;
        if let Some(container) = host.world.get::<Container>(entity) {
            for inside in container.contents.clone() {
                if !doomed.contains(&inside) {
                    doomed.push(inside);
                }
            }
        }
        if doomed.len() > 4096 {
            break;
        }
    }
    for entity in doomed {
        if host.world.get_entity(entity).is_ok() {
            host.world.despawn(entity);
        }
    }
}

/// The coins an item stands for: a gold pile's value, or the first value of a
/// money prototype. `None` for anything that is not money.
pub fn money_value(host: &EcsHost<'_>, obj: Entity) -> Option<i64> {
    let item = host.world.get::<Item>(obj)?;
    if item.def_id.as_ref() == GOLD_ITEM_ID {
        return item.value.map(i64::from);
    }
    let def = host
        .world
        .get_resource::<TbaContent>()?
        .items
        .get(&item.def_id)?;
    (def.kind == TbaItemKind::Money).then(|| i64::from(def.values[0]))
}

/// `create_money(amount)` lying in `room`.
pub fn create_money_in_room(host: &mut EcsHost<'_>, room: Entity, amount: i64) -> Option<Entity> {
    let amount = u32::try_from(amount).ok().filter(|amount| *amount > 0)?;
    host.world.get_entity(room).ok()?;
    let spawned_at = host
        .world
        .get_resource::<Time<Fixed>>()
        .map_or(0.0, |time| time.elapsed_secs());
    let pile = host
        .world
        .spawn((
            Item {
                def_id: ItemId::from(GOLD_ITEM_ID),
                name: GOLD_ITEM_NAME.to_owned(),
                description: GOLD_ITEM_DESCRIPTION.to_owned(),
                value: Some(amount),
                tags: Vec::new(),
            },
            Location { room },
            GroundItem { spawned_at },
        ))
        .id();
    crate::dg::ids::stamp_spawn(host.world, pile);
    Some(pile)
}

/// `money_desc(amount)`.
pub fn money_desc(amount: i64) -> &'static str {
    match amount {
        i64::MIN..=9 => "a gold coin",
        10..=20 => "a tiny pile of gold coins",
        21..=75 => "a small pile of gold coins",
        76..=200 => "a pile of gold coins",
        201..=1000 => "a large pile of gold coins",
        1001..=5000 => "a huge pile of gold coins",
        5001..=10000 => "an enormous pile of gold coins",
        10001..=20000 => "a massive pile of gold coins",
        20001..=75000 => "an immense pile of gold coins",
        75001..=150000 => "a pile of gold coins as big as a house",
        150001..=250000 => "a heap of gold coins",
        250001..=500000 => "a tremendous heap of gold coins",
        _ => "a mountain of gold coins",
    }
}

/// Tell the quest log a player now carries an item.
pub fn emit_collected(host: &mut EcsHost<'_>, player: Entity, obj: Entity) {
    if host.world.get::<Player>(player).is_none() {
        return;
    }
    let Some(item) = host.world.get::<Item>(obj).map(|item| item.def_id.clone()) else {
        return;
    };
    if let Some(mut events) = host.world.get_resource_mut::<Messages<QuestEvent>>() {
        events.write(QuestEvent::Collected { player, item });
    }
}

// -- equipment ----------------------------------------------------------------

/// Rewrite a player's persisted worn set from its runtime one. NPCs have no
/// persisted equipment, so one without the component is left alone.
pub fn write_equipment_save(world: &mut World, holder: Entity) {
    if world.get::<Player>(holder).is_none() && world.get::<TbaEquipmentSave>(holder).is_none() {
        return;
    }
    let slots: Vec<(WearSlot, Entity)> = world
        .get::<TbaEquipment>(holder)
        .map(|equipment| equipment.slots.iter().map(|(s, e)| (*s, *e)).collect())
        .unwrap_or_default();
    let mut worn = BTreeMap::new();
    for (slot, item) in slots {
        if let Some(save_id) = world.get::<SaveId>(item) {
            worn.insert(slot, save_id.0.to_string());
        }
    }
    world.entity_mut(holder).insert(TbaEquipmentSave { worn });
}

/// `equip_char(ch, obj, pos)`: the item, already out of the carrying list,
/// goes into the slot. In the pack a worn item stays `CarriedBy` its wearer;
/// the equipment map says it is worn.
pub fn equip_char(host: &mut EcsHost<'_>, ch: Entity, obj: Entity, slot: WearSlot) {
    if host.world.get_entity(ch).is_err() || host.world.get_entity(obj).is_err() {
        return;
    }
    detach_item(host, obj);
    host.world.entity_mut(obj).insert(CarriedBy(ch));
    if host.world.get::<TbaEquipment>(ch).is_none() {
        host.world.entity_mut(ch).insert(TbaEquipment::default());
    }
    if let Some(mut equipment) = host.world.get_mut::<TbaEquipment>(ch) {
        equipment.slots.insert(slot, obj);
    }
    write_equipment_save(host.world, ch);
}

/// `unequip_char(ch, pos)`: take the item out of the slot and back into the
/// carrying list (the C returns it for the caller's `obj_to_char`).
pub fn unequip_char(host: &mut EcsHost<'_>, ch: Entity, slot: WearSlot) -> Option<Entity> {
    let obj = {
        let mut equipment = host.world.get_mut::<TbaEquipment>(ch)?;
        equipment.slots.remove(&slot)?
    };
    write_equipment_save(host.world, ch);
    host.stamp_order(obj);
    Some(obj)
}

// -- moving characters and following ------------------------------------------

/// The pack's exit for a direction index of the C (`NORTH`..`SOUTHWEST`).
pub fn exit_key(dir: usize) -> Option<ExitKey> {
    Some(match dir {
        0 => ExitKey::North,
        1 => ExitKey::East,
        2 => ExitKey::South,
        3 => ExitKey::West,
        4 => ExitKey::Up,
        5 => ExitKey::Down,
        6 => ExitKey::Northwest,
        7 => ExitKey::Northeast,
        8 => ExitKey::Southeast,
        9 => ExitKey::Southwest,
        _ => return None,
    })
}

/// `rev_dir[dir]`.
pub fn rev_dir(dir: usize) -> usize {
    match dir {
        0 => 2,
        1 => 3,
        2 => 0,
        3 => 1,
        4 => 5,
        5 => 4,
        6 => 8,
        7 => 9,
        8 => 6,
        _ => 7,
    }
}

/// `dirs[]`.
pub const DIRS: [&str; 10] = [
    "north",
    "east",
    "south",
    "west",
    "up",
    "down",
    "northwest",
    "northeast",
    "southeast",
    "southwest",
];

/// `CONFIG_DIAGONAL_DIRS`: tbaMUD ships with the diagonal exits off, so a
/// character cannot walk them and door and fleeing searches skip them.
pub const DIAGONAL_DIRS: bool = false;

/// `DIR_COUNT`: how many directions movement considers.
pub const DIR_COUNT: usize = if DIAGONAL_DIRS { 10 } else { 6 };

/// `dirs[dir]`.
pub fn dir_name(dir: usize) -> &'static str {
    DIRS.get(dir).copied().unwrap_or("")
}

/// `char_from_room` + `char_to_room`: the character stands in `room` and is
/// at the head of its people. A player's online record follows.
pub fn char_to_room(host: &mut EcsHost<'_>, ch: Entity, room: Entity) {
    if host.world.get_entity(ch).is_err() || host.world.get::<Room>(room).is_none() {
        return;
    }
    host.world.entity_mut(ch).insert(Location { room });
    host.stamp_order(ch);
    if host.world.get::<Player>(ch).is_some()
        && let Some(mut online) = host.world.get_resource_mut::<OnlinePlayers>()
    {
        for record in online.map.values_mut() {
            if record.entity == ch {
                record.room = room;
            }
        }
    }
}

/// `AFF_SNEAK`: the pack's sneaking marker.
pub fn sneaking(host: &EcsHost<'_>, ch: Uid) -> bool {
    host.entity(ch)
        .is_some_and(|entity| host.world.get::<TbaSneaking>(entity).is_some())
}

/// The master `ch` follows, if any.
pub fn master(host: &EcsHost<'_>, ch: Uid) -> Option<Uid> {
    let entity = host.entity(ch)?;
    let leader = host.world.get::<TbaFollowing>(entity)?.leader;
    let uid = host.uid(leader)?;
    host.entity(uid).map(|_| uid)
}

/// Everyone who follows `leader`, in room order where they share one.
pub fn followers(host: &EcsHost<'_>, leader: Uid) -> Vec<Uid> {
    let Some(entity) = host.entity(leader) else {
        return Vec::new();
    };
    let mut found: Vec<Uid> = Vec::new();
    let mut query = host
        .world
        .try_query::<(Entity, &TbaFollowing)>();
    if let Some(query) = query.as_mut() {
        for (follower, following) in query.iter(host.world) {
            if following.leader == entity
                && let Some(uid) = host.uid(follower)
                && host.entity(uid).is_some()
            {
                found.push(uid);
            }
        }
    }
    found.sort_unstable();
    found
}

// -- rooms --------------------------------------------------------------------

/// `ROOM_FLAGGED(room, flag)`: the authored tags with the runtime overrides.
pub fn room_flagged(host: &EcsHost<'_>, room: Entity, tag: mib_stdlib::prelude::RoomTag) -> bool {
    let Some(data) = host.world.get::<Room>(room) else {
        return false;
    };
    match host.world.get::<mib_stdlib::prelude::RoomState>(room) {
        Some(state) => data.effective_flags(state).contains(&tag),
        None => data.tags.contains(&tag),
    }
}

/// The zone a room belongs to: the number of the `tba.zones` entry whose
/// range holds the room's vnum.
pub fn zone_of(host: &EcsHost<'_>, room: Entity) -> Option<u32> {
    let num = host.world.get::<Room>(room)?.num;
    host.world
        .get_resource::<TbaContent>()?
        .zones
        .iter()
        .find(|(_, zone)| num >= zone.bot && num <= zone.top)
        .map(|(key, _)| *key)
}

// -- text ---------------------------------------------------------------------

/// `send_to_char(ch, text)`. An NPC has no session, so a line meant only for
/// the acting NPC is dropped here, as the C drops it for a descriptorless mob.
pub fn to_char(host: &mut EcsHost<'_>, ch: Uid, text: &str) {
    if let Some(entity) = host.entity(ch) {
        crate::dg::out::send_to_char(host.world, entity, text);
    }
}

/// The room a character stands in.
pub fn room_of(host: &EcsHost<'_>, ch: Uid) -> Option<Entity> {
    let entity = host.entity(ch)?;
    let room = host.world.get::<Location>(entity)?.room;
    host.world.get::<Room>(room).is_some().then_some(room)
}
