//! `EcsHost`: the [`dg_vm::DgHost`] implementation over the Bevy world.
//!
//! An `EcsHost` borrows the `World` for the length of one dispatch. The nine
//! traits `DgHost` unions are implemented one per file beside this one;
//! [`BaseHost`] and [`WorldHost`] live here, with the two vocabularies several
//! of the others need: the mapping between the pack's [`WearSlot`] and DG's
//! wear positions and `ITEM_WEAR_*` bits ([`wear_position`], [`wear_slot`],
//! [`item_wear_bit`]), and between [`TbaItemKind`] and the `ITEM_*` type
//! numbers ([`item_type_number`]).
//!
//! Where the pack models less than the C, the answer is the neutral value
//! `HOST.md` lists, and the gap is stated at the method.

mod chars;
mod commands;
mod dispatch;
mod magic;
mod objs;
mod rooms;
mod subst;

pub use objs::DgObjOverride;
pub use rooms::{dir_number, exit_key};

pub use commands::DgNowhereExits;

use std::collections::HashSet;
use std::sync::Arc;

use dg_vm::lookup::{
    WEAR_ABOUT, WEAR_ARMS, WEAR_BODY, WEAR_FEET, WEAR_FINGER_L, WEAR_FINGER_R, WEAR_HANDS,
    WEAR_HEAD, WEAR_HOLD, WEAR_LEGS, WEAR_LIGHT, WEAR_NECK_1, WEAR_NECK_2, WEAR_SHIELD, WEAR_WAIST,
    WEAR_WIELD, WEAR_WRIST_L, WEAR_WRIST_R,
};
use dg_vm::types::{MudTime, ScriptMemory, VmState};
use dg_vm::{BaseHost, Kind, Script, TrigProto, Uid, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    CarriedBy, ContainedBy, GroundItem, Item, Location, Npc, NpcKey, PendingBundleRestore,
    Perceiver, Perception, Player, Room, Session, SessionState, perceive,
};

use super::script::{DgDead, DgMemory, DgProtos, DgScript};
use super::vars::seed_player_script;
use super::waits::{DgPulse, DgWait, DgWaits};
use super::{DgLog, DgVm, ids, mobcmd};
use crate::combat::{TbaCombatant, TbaProgress, TbaRng};
use crate::content::{Position, TbaItemKind, WearSlot};
use crate::equipment::TbaEquipment;
use crate::module::TbaContent;
use mib_stdlib::prelude::TimeOfDay;

/// The host the VM runs against: the ECS world, borrowed for one dispatch.
pub struct EcsHost<'w> {
    pub world: &'w mut World,
}

/// `EcsHost` implements every host trait, which is what `DgHost` requires.
const _: fn() = || {
    fn assert_dg_host<T: dg_vm::DgHost>() {}
    assert_dg_host::<EcsHost<'static>>();
};

/// `ITEM_WEAR_TAKE`.
const ITEM_WEAR_TAKE: u32 = 0;
/// `ITEM_WEAR_FINGER` and the bits that follow it, by number.
const ITEM_WEAR_FINGER: u32 = 1;
const ITEM_WEAR_NECK: u32 = 2;
const ITEM_WEAR_BODY: u32 = 3;
const ITEM_WEAR_HEAD: u32 = 4;
const ITEM_WEAR_LEGS: u32 = 5;
const ITEM_WEAR_FEET: u32 = 6;
const ITEM_WEAR_HANDS: u32 = 7;
const ITEM_WEAR_ARMS: u32 = 8;
const ITEM_WEAR_SHIELD: u32 = 9;
const ITEM_WEAR_ABOUT: u32 = 10;
const ITEM_WEAR_WAIST: u32 = 11;
const ITEM_WEAR_WRIST: u32 = 12;
const ITEM_WEAR_WIELD: u32 = 13;
const ITEM_WEAR_HOLD: u32 = 14;

/// The DG wear position (`WEAR_*`, `0..NUM_WEARS`) of a wear slot.
pub fn wear_position(slot: WearSlot) -> i32 {
    match slot {
        WearSlot::Light => WEAR_LIGHT,
        WearSlot::FingerR => WEAR_FINGER_R,
        WearSlot::FingerL => WEAR_FINGER_L,
        WearSlot::Neck1 => WEAR_NECK_1,
        WearSlot::Neck2 => WEAR_NECK_2,
        WearSlot::Body => WEAR_BODY,
        WearSlot::Head => WEAR_HEAD,
        WearSlot::Legs => WEAR_LEGS,
        WearSlot::Feet => WEAR_FEET,
        WearSlot::Hands => WEAR_HANDS,
        WearSlot::Arms => WEAR_ARMS,
        WearSlot::Shield => WEAR_SHIELD,
        WearSlot::About => WEAR_ABOUT,
        WearSlot::Waist => WEAR_WAIST,
        WearSlot::WristR => WEAR_WRIST_R,
        WearSlot::WristL => WEAR_WRIST_L,
        WearSlot::Wield => WEAR_WIELD,
        WearSlot::Hold => WEAR_HOLD,
    }
}

/// The wear slot at a DG wear position, or `None` outside `0..NUM_WEARS`.
pub fn wear_slot(position: i32) -> Option<WearSlot> {
    Some(match position {
        WEAR_LIGHT => WearSlot::Light,
        WEAR_FINGER_R => WearSlot::FingerR,
        WEAR_FINGER_L => WearSlot::FingerL,
        WEAR_NECK_1 => WearSlot::Neck1,
        WEAR_NECK_2 => WearSlot::Neck2,
        WEAR_BODY => WearSlot::Body,
        WEAR_HEAD => WearSlot::Head,
        WEAR_LEGS => WearSlot::Legs,
        WEAR_FEET => WearSlot::Feet,
        WEAR_HANDS => WearSlot::Hands,
        WEAR_ARMS => WearSlot::Arms,
        WEAR_SHIELD => WearSlot::Shield,
        WEAR_ABOUT => WearSlot::About,
        WEAR_WAIST => WearSlot::Waist,
        WEAR_WRIST_R => WearSlot::WristR,
        WEAR_WRIST_L => WearSlot::WristL,
        WEAR_WIELD => WearSlot::Wield,
        WEAR_HOLD => WearSlot::Hold,
        _ => return None,
    })
}

/// The `ITEM_WEAR_*` bit number an item must carry to be worn in a slot. Both
/// halves of a pair share one bit, and a light is held (the number
/// `dg_vm::lookup::can_wear_on_pos` tests for `WEAR_LIGHT`).
pub fn item_wear_bit(slot: WearSlot) -> u32 {
    match slot {
        WearSlot::Light | WearSlot::Hold => ITEM_WEAR_HOLD,
        WearSlot::FingerR | WearSlot::FingerL => ITEM_WEAR_FINGER,
        WearSlot::Neck1 | WearSlot::Neck2 => ITEM_WEAR_NECK,
        WearSlot::Body => ITEM_WEAR_BODY,
        WearSlot::Head => ITEM_WEAR_HEAD,
        WearSlot::Legs => ITEM_WEAR_LEGS,
        WearSlot::Feet => ITEM_WEAR_FEET,
        WearSlot::Hands => ITEM_WEAR_HANDS,
        WearSlot::Arms => ITEM_WEAR_ARMS,
        WearSlot::Shield => ITEM_WEAR_SHIELD,
        WearSlot::About => ITEM_WEAR_ABOUT,
        WearSlot::Waist => ITEM_WEAR_WAIST,
        WearSlot::WristR | WearSlot::WristL => ITEM_WEAR_WRIST,
        WearSlot::Wield => ITEM_WEAR_WIELD,
    }
}

/// The `ITEM_*` number (`GET_OBJ_TYPE`) of an item kind.
pub fn item_type_number(kind: TbaItemKind) -> i32 {
    match kind {
        TbaItemKind::Undefined => 0,
        TbaItemKind::Light => 1,
        TbaItemKind::Scroll => 2,
        TbaItemKind::Wand => 3,
        TbaItemKind::Staff => 4,
        TbaItemKind::Weapon => 5,
        TbaItemKind::Furniture => 6,
        TbaItemKind::Free => 7,
        TbaItemKind::Treasure => 8,
        TbaItemKind::Armor => 9,
        TbaItemKind::Potion => 10,
        TbaItemKind::Worn => 11,
        TbaItemKind::Other => 12,
        TbaItemKind::Trash => 13,
        TbaItemKind::Free2 => 14,
        TbaItemKind::Container => 15,
        TbaItemKind::Note => 16,
        TbaItemKind::DrinkContainer => 17,
        TbaItemKind::Key => 18,
        TbaItemKind::Food => 19,
        TbaItemKind::Money => 20,
        TbaItemKind::Pen => 21,
        TbaItemKind::Boat => 22,
        TbaItemKind::Fountain => 23,
    }
}

/// The match words of an NPC: the lowercased words of its short description
/// without articles. The pack keeps no separate keyword list for NPCs.
pub fn npc_keywords(name: &str) -> String {
    let words: Vec<String> = name
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty() && !matches!(word.as_str(), "a" | "an" | "the"))
        .collect();
    if words.is_empty() {
        name.trim().to_lowercase()
    } else {
        words.join(" ")
    }
}

impl EcsHost<'_> {
    /// The entities carrying `T`.
    fn all_with<T: Component>(&self) -> Vec<Entity> {
        self.world
            .try_query_filtered::<Entity, With<T>>()
            .map(|mut query| query.iter(self.world).collect())
            .unwrap_or_default()
    }

    /// The pack content, if installed.
    fn content(&self) -> Option<&TbaContent> {
        self.world.get_resource::<TbaContent>()
    }

    fn is_character(&self, entity: Entity) -> bool {
        self.world.get::<Player>(entity).is_some() || self.world.get::<Npc>(entity).is_some()
    }

    fn placement(&self, entity: Entity) -> u64 {
        self.world
            .get::<ids::DgOrder>(entity)
            .map_or(0, |order| order.0)
    }

    fn birth(&self, entity: Entity) -> u64 {
        self.world
            .get::<ids::DgBirth>(entity)
            .map_or(0, |birth| birth.0)
    }

    /// The ids of `entities`, ordered by `stamp` (newest first when
    /// `newest_first`), ties broken by id.
    fn ordered(
        &self,
        entities: impl IntoIterator<Item = Entity>,
        stamp: fn(&Self, Entity) -> u64,
        newest_first: bool,
    ) -> Vec<Uid> {
        let mut keyed: Vec<(u64, Uid)> = entities
            .into_iter()
            .filter_map(|entity| Some((stamp(self, entity), self.uid(entity)?)))
            .collect();
        keyed.sort_unstable();
        if newest_first {
            keyed.reverse();
        }
        keyed.into_iter().map(|(_, uid)| uid).collect()
    }

    /// The items a character wears, from its equipment map.
    fn worn_items(&self, holder: Entity) -> HashSet<Entity> {
        self.world
            .get::<TbaEquipment>(holder)
            .map(|equipment| equipment.slots.values().copied().collect())
            .unwrap_or_default()
    }

    /// The character an item is carried by, worn or not.
    fn holder_of(&self, obj: Entity) -> Option<Entity> {
        let holder = self.world.get::<CarriedBy>(obj)?.0;
        self.world.get_entity(holder).is_ok().then_some(holder)
    }

    /// The prototype definition of an item.
    fn item_def(&self, entity: Entity) -> Option<&crate::content::TbaItemDef> {
        let item = self.world.get::<Item>(entity)?;
        self.content()?.items.get(&item.def_id)
    }
}

impl BaseHost for EcsHost<'_> {
    fn exists(&self, uid: Uid) -> bool {
        self.entity(uid).is_some()
    }

    fn script(&self, uid: Uid) -> Option<&Script> {
        let entity = self.entity(uid)?;
        self.world.get::<DgScript>(entity).map(|script| &script.0)
    }

    fn script_mut(&mut self, uid: Uid) -> Option<&mut Script> {
        let entity = self.entity(uid)?;
        self.world
            .get_mut::<DgScript>(entity)
            .map(|script| &mut script.into_inner().0)
    }

    fn ensure_script(&mut self, uid: Uid) -> Option<&mut Script> {
        let entity = self.entity(uid)?;
        if self.world.get::<DgScript>(entity).is_none() {
            if self.world.get::<Player>(entity).is_some() {
                // A player whose saved variables are not restored yet has no
                // script until they are.
                if self.world.get::<PendingBundleRestore>(entity).is_some() {
                    return None;
                }
                seed_player_script(self.world, entity);
            } else {
                self.world.entity_mut(entity).insert(DgScript::default());
            }
        }
        self.world
            .get_mut::<DgScript>(entity)
            .map(|script| &mut script.into_inner().0)
    }

    fn remove_script(&mut self, uid: Uid) -> Option<Script> {
        let entity = self.entity(uid)?;
        self.world
            .entity_mut(entity)
            .take::<DgScript>()
            .map(|script| script.0)
    }

    fn vm_state(&mut self) -> &mut VmState {
        &mut self
            .world
            .get_resource_or_insert_with(DgVm::default)
            .into_inner()
            .state
    }

    /// Players never run triggers: the pack attaches scripts to rooms, NPCs
    /// and items only (`CONFIG_SCRIPT_PLAYERS` off).
    fn script_players(&self) -> bool {
        false
    }

    fn pulse(&self) -> i64 {
        self.world
            .get_resource::<DgPulse>()
            .map_or(0, |clock| clock.pulse)
    }

    fn rand_number(&mut self, from: i64, to: i64) -> i64 {
        let (low, high) = if from <= to { (from, to) } else { (to, from) };
        let span = (high as u64).wrapping_sub(low as u64).wrapping_add(1);
        let raw = self
            .world
            .get_resource_or_insert_with(TbaRng::default)
            .next_u64();
        if span == 0 {
            raw as i64
        } else {
            low.wrapping_add((raw % span) as i64)
        }
    }

    fn mud_time(&self) -> MudTime {
        let Some(clock) = self.world.get_resource::<TimeOfDay>() else {
            return MudTime::default();
        };
        // Circle's `time_info` fields are 0-based and derived from the engine
        // clock's day index.
        let (day, month, year) = crate::clock::circle_calendar(clock);
        let to_i32 = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
        MudTime {
            hours: i32::from(clock.hour),
            day: to_i32(day),
            month: to_i32(month),
            year: to_i32(year),
        }
    }

    fn trig_proto(&self, vnum: i32) -> Option<Arc<TrigProto>> {
        self.world.get_resource::<DgProtos>()?.get(vnum)
    }

    fn schedule_wait(&mut self, owner: Uid, trigger_id: u64, pulses: i64) {
        let now = self.pulse();
        let due = now.saturating_add(pulses.max(1));
        self.world
            .get_resource_or_insert_with(DgWaits::default)
            .schedule(DgWait {
                due,
                owner,
                trigger: trigger_id,
            });
    }

    fn cancel_wait(&mut self, owner: Uid, trigger_id: u64) {
        if let Some(mut waits) = self.world.get_resource_mut::<DgWaits>() {
            waits.cancel(owner, trigger_id);
        }
    }

    fn mud_command(&mut self, ch: Uid, line: &str) {
        mobcmd::mud_command(self, ch, line);
    }

    fn mud_command_name(&self, prefix: &str) -> Option<String> {
        mobcmd::mud_command_name(prefix)
    }

    fn script_log(&mut self, message: &str) {
        tracing::debug!(target: "dg", "{message}");
        self.world
            .get_resource_or_insert_with(DgLog::default)
            .record("script_log", message.to_owned());
    }

    fn unsupported(&mut self, what: &str, detail: &str) {
        tracing::debug!(target: "dg", "unsupported {what}: {detail}");
        self.world
            .get_resource_or_insert_with(DgLog::default)
            .record(what, format!("unsupported {what}: {detail}"));
    }
}

impl WorldHost for EcsHost<'_> {
    fn kind(&self, uid: Uid) -> Option<Kind> {
        let entity = self.entity(uid)?;
        if self.is_character(entity) {
            Some(Kind::Char)
        } else if self.world.get::<Item>(entity).is_some() {
            Some(Kind::Obj)
        } else if self.world.get::<Room>(entity).is_some() {
            Some(Kind::Room)
        } else {
            None
        }
    }

    fn is_npc(&self, ch: Uid) -> bool {
        self.entity(ch)
            .is_some_and(|entity| self.world.get::<Npc>(entity).is_some())
    }

    /// NPCs are always valid targets; a player only while playing. The pack
    /// has no immortal levels or `NOHASSLE`, so `allow_gods` changes nothing.
    fn valid_dg_target(&self, ch: Uid, _allow_gods: bool) -> bool {
        let Some(entity) = self.entity(ch) else {
            return false;
        };
        if self.world.get::<Npc>(entity).is_some() {
            return true;
        }
        self.world.get::<Player>(entity).is_some()
            && self
                .world
                .get::<Session>(entity)
                .is_none_or(|session| matches!(session.state, SessionState::Playing))
    }

    fn name(&self, uid: Uid) -> String {
        let Some(entity) = self.entity(uid) else {
            return String::new();
        };
        if let Some(npc) = self.world.get::<Npc>(entity) {
            return npc.name.clone();
        }
        if let Some(item) = self.world.get::<Item>(entity) {
            return item.name.clone();
        }
        if let Some(player) = self.world.get::<Player>(entity) {
            return player.name.clone();
        }
        self.world
            .get::<Room>(entity)
            .map(|room| room.name.clone())
            .unwrap_or_default()
    }

    fn keywords(&self, uid: Uid) -> String {
        let Some(entity) = self.entity(uid) else {
            return String::new();
        };
        if let Some(npc) = self.world.get::<Npc>(entity) {
            return npc_keywords(&npc.name);
        }
        if let Some(item) = self.world.get::<Item>(entity) {
            if let Some(alias) = self
                .world
                .get::<objs::DgObjOverride>(entity)
                .and_then(|over| over.alias.as_deref())
                .filter(|alias| !alias.is_empty())
            {
                return alias.to_owned();
            }
            return match self.item_def(entity) {
                Some(def) if !def.keywords.is_empty() => def.keywords.join(" "),
                _ => npc_keywords(&item.name),
            };
        }
        self.world
            .get::<Player>(entity)
            .map(|player| player.name.clone())
            .unwrap_or_default()
    }

    fn vnum(&self, uid: Uid) -> i32 {
        let Some(entity) = self.entity(uid) else {
            return -1;
        };
        if let Some(room) = self.world.get::<Room>(entity) {
            return i32::try_from(room.num)
                .ok()
                .filter(|num| (0..=65_534).contains(num))
                .unwrap_or(-1);
        }
        if let Some(key) = self.world.get::<NpcKey>(entity) {
            return ids::vnum_from_id(key.0.as_ref()).unwrap_or(-1);
        }
        self.world
            .get::<Item>(entity)
            .and_then(|item| ids::vnum_from_id(item.def_id.as_ref()))
            .unwrap_or(-1)
    }

    fn in_room(&self, uid: Uid) -> Option<Uid> {
        let entity = self.entity(uid)?;
        let on_ground = self.world.get::<Item>(entity).is_none()
            || self.world.get::<GroundItem>(entity).is_some();
        if !on_ground {
            return None;
        }
        let room = self.world.get::<Location>(entity)?.room;
        self.uid(room)
    }

    fn room_people(&self, room: Uid) -> Vec<Uid> {
        let Some(room) = self.entity(room) else {
            return Vec::new();
        };
        let present = self
            .all_with::<Player>()
            .into_iter()
            .chain(self.all_with::<Npc>())
            .filter(|entity| {
                self.world
                    .get::<Location>(*entity)
                    .is_some_and(|location| location.room == room)
            });
        self.ordered(present, Self::placement, true)
    }

    fn room_contents(&self, room: Uid) -> Vec<Uid> {
        let Some(room) = self.entity(room) else {
            return Vec::new();
        };
        let lying = self.all_with::<GroundItem>().into_iter().filter(|entity| {
            self.world.get::<Item>(*entity).is_some()
                && self
                    .world
                    .get::<Location>(*entity)
                    .is_some_and(|location| location.room == room)
        });
        self.ordered(lying, Self::placement, false)
    }

    fn carrying(&self, ch: Uid) -> Vec<Uid> {
        let Some(holder) = self.entity(ch) else {
            return Vec::new();
        };
        let worn = self.worn_items(holder);
        let held = self.all_with::<CarriedBy>().into_iter().filter(|entity| {
            self.world
                .get::<CarriedBy>(*entity)
                .is_some_and(|carried| carried.0 == holder)
                && !worn.contains(entity)
        });
        self.ordered(held, Self::placement, true)
    }

    fn equipment(&self, ch: Uid, pos: i32) -> Option<Uid> {
        let holder = self.entity(ch)?;
        let slot = wear_slot(pos)?;
        let item = self.world.get::<TbaEquipment>(holder)?.worn(slot)?;
        self.uid(item)
    }

    fn obj_contains(&self, obj: Uid) -> Vec<Uid> {
        let Some(container) = self.entity(obj) else {
            return Vec::new();
        };
        let inside = self.all_with::<ContainedBy>().into_iter().filter(|entity| {
            self.world
                .get::<ContainedBy>(*entity)
                .is_some_and(|contained| contained.0 == container)
        });
        self.ordered(inside, Self::placement, true)
    }

    /// An equipped item is `CarriedBy` its wearer in the pack; it counts as
    /// worn, not carried, as in the C.
    fn obj_carried_by(&self, obj: Uid) -> Option<Uid> {
        let entity = self.entity(obj)?;
        let holder = self.holder_of(entity)?;
        if self.worn_items(holder).contains(&entity) {
            return None;
        }
        self.uid(holder)
    }

    fn obj_worn_by(&self, obj: Uid) -> Option<Uid> {
        let entity = self.entity(obj)?;
        let holder = self.holder_of(entity)?;
        if !self.worn_items(holder).contains(&entity) {
            return None;
        }
        self.uid(holder)
    }

    fn obj_in_obj(&self, obj: Uid) -> Option<Uid> {
        let entity = self.entity(obj)?;
        let container = self.world.get::<ContainedBy>(entity)?.0;
        let uid = self.uid(container)?;
        self.entity(uid).map(|_| uid)
    }

    fn character_list(&self) -> Vec<Uid> {
        let everyone = self
            .all_with::<Player>()
            .into_iter()
            .chain(self.all_with::<Npc>());
        self.ordered(everyone, Self::birth, true)
    }

    fn object_list(&self) -> Vec<Uid> {
        let present = self
            .all_with::<Item>()
            .into_iter()
            .filter(|entity| !super::ids::is_parked(self.world, *entity));
        self.ordered(present, Self::birth, true)
    }

    fn real_room(&self, vnum: i32) -> Option<Uid> {
        let uid = dg_vm::types::ROOM_ID_BASE.checked_add(Uid::from(u32::try_from(vnum).ok()?))?;
        let entity = self.entity(uid)?;
        self.world.get::<Room>(entity)?;
        Some(uid)
    }

    /// The pack's perception rule (`mib_stdlib::perceive`) over the target's
    /// `Concealment` and the observer's `Perception`. Darkness is not
    /// evaluated (no light model is reachable from a script), and objects are
    /// always visible.
    fn can_see(&self, ch: Uid, target: Uid) -> bool {
        let (Some(observer), Some(seen)) = (self.entity(ch), self.entity(target)) else {
            return true;
        };
        if observer == seen || !self.is_character(seen) {
            return true;
        }
        let concealment = self
            .world
            .get::<mib_stdlib::prelude::Concealment>(seen)
            .copied()
            .unwrap_or_default();
        let observer = Perceiver::from_perception(self.world.get::<Perception>(observer), false);
        perceive(&observer, &concealment, false)
    }

    fn level(&self, ch: Uid) -> i32 {
        let Some(entity) = self.entity(ch) else {
            return 0;
        };
        let level = if let Some(progress) = self.world.get::<TbaProgress>(entity) {
            progress.level
        } else if let Some(key) = self.world.get::<NpcKey>(entity) {
            self.content()
                .and_then(|content| content.npc_stats.get(&key.0))
                .map_or(0, |stats| stats.level)
        } else {
            0
        };
        i32::try_from(level).unwrap_or(i32::MAX)
    }

    fn is_dead(&self, ch: Uid) -> bool {
        let Some(entity) = self.entity(ch) else {
            return false;
        };
        self.world.get::<DgDead>(entity).is_some()
            || self
                .world
                .get::<TbaCombatant>(entity)
                .is_some_and(|combatant| combatant.position == Position::Dead)
    }

    fn obj_type(&self, obj: Uid) -> i32 {
        self.entity(obj)
            .and_then(|entity| self.item_def(entity))
            .map_or(0, |def| item_type_number(def.kind))
    }

    /// The take bit (the pack does not model untakeable objects) and the bit
    /// of every slot the item's prototype lists.
    fn wear_flags(&self, obj: Uid) -> u32 {
        let Some(entity) = self.entity(obj) else {
            return 0;
        };
        if self.world.get::<Item>(entity).is_none() {
            return 0;
        }
        let mut flags = 1 << ITEM_WEAR_TAKE;
        if let Some(def) = self.item_def(entity) {
            for slot in &def.slots {
                flags |= 1 << item_wear_bit(*slot);
            }
        }
        flags
    }

    fn script_mem(&self, ch: Uid) -> Vec<ScriptMemory> {
        self.entity(ch)
            .and_then(|entity| self.world.get::<DgMemory>(entity))
            .map(|memory| memory.0.clone())
            .unwrap_or_default()
    }

    fn set_script_mem(&mut self, ch: Uid, memory: Vec<ScriptMemory>) {
        let Some(entity) = self.entity(ch) else {
            return;
        };
        self.world.entity_mut(entity).insert(DgMemory(memory));
    }
}
