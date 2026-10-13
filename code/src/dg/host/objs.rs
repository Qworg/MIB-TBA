//! `ObjHost`: object attributes.
//!
//! In the pack the values, cost, rent, weight, extra and affect flags and
//! applies of an object are prototype data (`TbaContent.items`), shared by
//! every instance. The C keeps them per instance and scripts change them
//! (`osetval`, `otimer`, `%obj.cost%`, `oset apply`), so [`DgObjOverride`]
//! holds the per-instance changes and every read falls back from it to the
//! prototype.
//!
//! The decay timer is one fact: the pack's [`TbaItemTimer`] (whole MUD hours,
//! the unit of `GET_OBJ_TIMER`) is the live timer, so a script sets and reads
//! that component and [`tick_item_timers`](crate::items::tick_item_timers)
//! ages it. A timer of zero or less means "no timer" (`point_update` only
//! decrements a positive one); it is the one value the component cannot hold,
//! so [`DgObjOverride::timer`] keeps it.
//!
//! The extra and affect flags map from the pack's [`ItemExtraFlag`] and
//! [`ItemAffectFlag`] to the bit numbers of `structs.h` (`ITEM_*`, `AFF_*`),
//! and an apply location maps from [`AffectLocation`] to its `APPLY_*` number.
//! No script command changes a flag, so flags are read from the prototype
//! alone.

use dg_vm::Uid;
use dg_vm::fields_obj::{MAX_OBJ_AFFECT, ObjHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::Item;

use super::EcsHost;
use crate::content::{AffectLocation, ItemAffectFlag, ItemExtraFlag, TbaItemDef};
use crate::items::TbaItemTimer;

/// The longest alias, short and long description the pack keeps; the VM has
/// already applied the C's tighter caps, this only bounds a stray caller.
const TEXT_MAX: usize = 4096;

/// Per-instance changes a script made to an object's prototype data. A field
/// that is `None` still reads from the prototype. Runtime-only: it is not
/// saved, so a restored object starts from its prototype again (the timer is
/// the exception, being the persisted [`TbaItemTimer`]).
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct DgObjOverride {
    /// `GET_OBJ_VAL(obj, 0..4)`.
    pub values: [Option<i32>; 4],
    /// `GET_OBJ_TIMER` when it is zero or negative (no timer running); a
    /// positive timer is the [`TbaItemTimer`] component.
    pub timer: Option<i32>,
    /// `GET_OBJ_COST`.
    pub cost: Option<i32>,
    /// `GET_OBJ_RENT`.
    pub rent: Option<i32>,
    /// `GET_OBJ_WEIGHT`.
    pub weight: Option<i32>,
    /// `obj->affected[slot]` as `(location, modifier)`.
    pub applies: [Option<(u8, i8)>; MAX_OBJ_AFFECT],
    /// `obj->name`, the keyword list `oset alias` sets.
    pub alias: Option<String>,
}

/// The bit number (`ITEM_*`) of an extra flag.
pub fn item_extra_bit(flag: ItemExtraFlag) -> usize {
    match flag {
        ItemExtraFlag::Glow => 0,
        ItemExtraFlag::Hum => 1,
        ItemExtraFlag::NoRent => 2,
        ItemExtraFlag::NoDonate => 3,
        ItemExtraFlag::NoInvis => 4,
        ItemExtraFlag::Invisible => 5,
        ItemExtraFlag::Magic => 6,
        ItemExtraFlag::NoDrop => 7,
        ItemExtraFlag::Bless => 8,
        ItemExtraFlag::AntiGood => 9,
        ItemExtraFlag::AntiEvil => 10,
        ItemExtraFlag::AntiNeutral => 11,
        ItemExtraFlag::AntiMagicUser => 12,
        ItemExtraFlag::AntiCleric => 13,
        ItemExtraFlag::AntiThief => 14,
        ItemExtraFlag::AntiWarrior => 15,
        ItemExtraFlag::NoSell => 16,
        ItemExtraFlag::Quest => 17,
    }
}

/// The bit number (`AFF_*`) of an affect flag. The placeholder bits
/// `AFF_DONTUSE` (0), `AFF_UNUSED` (9) and `AFF_FREE` (21) have no flag.
pub fn item_affect_bit(flag: ItemAffectFlag) -> usize {
    match flag {
        ItemAffectFlag::Blind => 1,
        ItemAffectFlag::Invisible => 2,
        ItemAffectFlag::DetectAlign => 3,
        ItemAffectFlag::DetectInvis => 4,
        ItemAffectFlag::DetectMagic => 5,
        ItemAffectFlag::SenseLife => 6,
        ItemAffectFlag::Waterwalk => 7,
        ItemAffectFlag::Sanctuary => 8,
        ItemAffectFlag::Curse => 10,
        ItemAffectFlag::Infravision => 11,
        ItemAffectFlag::Poison => 12,
        ItemAffectFlag::ProtectEvil => 13,
        ItemAffectFlag::ProtectGood => 14,
        ItemAffectFlag::Sleep => 15,
        ItemAffectFlag::NoTrack => 16,
        ItemAffectFlag::Flying => 17,
        ItemAffectFlag::Scuba => 18,
        ItemAffectFlag::Sneak => 19,
        ItemAffectFlag::Hide => 20,
        ItemAffectFlag::Charm => 22,
    }
}

/// The `APPLY_*` number of an affect location.
pub fn apply_number(location: AffectLocation) -> u8 {
    match location {
        AffectLocation::Str => 1,
        AffectLocation::Dex => 2,
        AffectLocation::Int => 3,
        AffectLocation::Wis => 4,
        AffectLocation::Con => 5,
        AffectLocation::Cha => 6,
        AffectLocation::MaxMana => 12,
        AffectLocation::MaxHp => 13,
        AffectLocation::MaxMove => 14,
        AffectLocation::ArmorClass => 17,
        AffectLocation::Hitroll => 18,
        AffectLocation::Damroll => 19,
        AffectLocation::SavePara => 20,
        AffectLocation::SaveRod => 21,
        AffectLocation::SavePetri => 22,
        AffectLocation::SaveBreath => 23,
        AffectLocation::SaveSpell => 24,
    }
}

/// A prototype number as the C's `int`, saturating.
fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

impl EcsHost<'_> {
    /// The item entity a script id names; `None` for anything else.
    fn item_entity(&self, obj: Uid) -> Option<Entity> {
        let entity = self.entity(obj)?;
        self.world.get::<Item>(entity)?;
        Some(entity)
    }

    /// The object's per-instance changes, if it has any.
    fn obj_override(&self, entity: Entity) -> Option<&DgObjOverride> {
        self.world.get::<DgObjOverride>(entity)
    }

    /// Change the object's per-instance record, creating it on first use.
    fn with_override(&mut self, entity: Entity, change: impl FnOnce(&mut DgObjOverride)) {
        if self.world.get_entity(entity).is_err() {
            return;
        }
        let mut entity_mut = self.world.entity_mut(entity);
        if let Some(mut record) = entity_mut.get_mut::<DgObjOverride>() {
            change(&mut record);
        } else {
            let mut record = DgObjOverride::default();
            change(&mut record);
            entity_mut.insert(record);
        }
    }

    /// An object's prototype definition and its per-instance record.
    fn obj_sources(&self, obj: Uid) -> Option<(Option<&TbaItemDef>, Option<&DgObjOverride>)> {
        let entity = self.item_entity(obj)?;
        Some((self.item_def(entity), self.obj_override(entity)))
    }
}

impl ObjHost for EcsHost<'_> {
    fn obj_val(&self, obj: Uid, idx: usize) -> i32 {
        let Some((def, over)) = self.obj_sources(obj) else {
            return 0;
        };
        over.and_then(|over| over.values.get(idx).copied().flatten())
            .or_else(|| def.and_then(|def| def.values.get(idx).copied()))
            .unwrap_or(0)
    }

    fn obj_set_val(&mut self, obj: Uid, idx: usize, value: i32) {
        let Some(entity) = self.item_entity(obj) else {
            return;
        };
        self.with_override(entity, |over| {
            if let Some(slot) = over.values.get_mut(idx) {
                *slot = Some(value);
            }
        });
    }

    /// The live [`TbaItemTimer`] when one runs, else a zero or negative timer a
    /// script set, else the prototype's `timer`.
    fn obj_timer(&self, obj: Uid) -> i32 {
        let Some(entity) = self.item_entity(obj) else {
            return 0;
        };
        if let Some(timer) = self.world.get::<TbaItemTimer>(entity) {
            return to_i32(timer.remaining);
        }
        if let Some(timer) = self.obj_override(entity).and_then(|over| over.timer) {
            return timer;
        }
        self.item_def(entity)
            .and_then(|def| def.timer)
            .map_or(0, to_i32)
    }

    /// A positive timer starts (or restarts) the item's [`TbaItemTimer`]; zero
    /// or less stops it, as `point_update` only ages a positive timer.
    fn obj_set_timer(&mut self, obj: Uid, value: i32) {
        let Some(entity) = self.item_entity(obj) else {
            return;
        };
        if let Ok(remaining) = u32::try_from(value)
            && remaining > 0
        {
            self.with_override(entity, |over| over.timer = None);
            self.world
                .entity_mut(entity)
                .insert(TbaItemTimer { remaining });
        } else {
            self.world.entity_mut(entity).remove::<TbaItemTimer>();
            self.with_override(entity, |over| over.timer = Some(value));
        }
    }

    fn obj_cost(&self, obj: Uid) -> i32 {
        let Some((def, over)) = self.obj_sources(obj) else {
            return 0;
        };
        over.and_then(|over| over.cost)
            .or_else(|| def.map(|def| to_i32(def.cost)))
            .unwrap_or(0)
    }

    fn obj_set_cost(&mut self, obj: Uid, value: i32) {
        if let Some(entity) = self.item_entity(obj) {
            self.with_override(entity, |over| over.cost = Some(value));
        }
    }

    fn obj_rent(&self, obj: Uid) -> i32 {
        let Some((def, over)) = self.obj_sources(obj) else {
            return 0;
        };
        over.and_then(|over| over.rent)
            .or_else(|| def.map(|def| to_i32(def.rent)))
            .unwrap_or(0)
    }

    fn obj_set_rent(&mut self, obj: Uid, value: i32) {
        if let Some(entity) = self.item_entity(obj) {
            self.with_override(entity, |over| over.rent = Some(value));
        }
    }

    fn obj_weight(&self, obj: Uid) -> i32 {
        let Some((def, over)) = self.obj_sources(obj) else {
            return 0;
        };
        over.and_then(|over| over.weight)
            .or_else(|| def.map(|def| to_i32(def.weight)))
            .unwrap_or(0)
    }

    /// The field only: the pack's carried weight is computed from the
    /// prototypes, as the C's `GET_OBJ_WEIGHT = value` leaves
    /// `IS_CARRYING_W` alone.
    fn obj_set_weight(&mut self, obj: Uid, value: i32) {
        if let Some(entity) = self.item_entity(obj) {
            self.with_override(entity, |over| over.weight = Some(value));
        }
    }

    fn obj_extra_flagged(&self, obj: Uid, bit: usize) -> bool {
        self.obj_sources(obj)
            .and_then(|(def, _)| def)
            .is_some_and(|def| def.extra_flags.iter().any(|f| item_extra_bit(*f) == bit))
    }

    /// The pack carries the affect flags the world files set; the placeholder
    /// bits (`AFF_DONTUSE`, `AFF_UNUSED`, `AFF_FREE`) are never set.
    fn obj_affect_flagged(&self, obj: Uid, bit: usize) -> bool {
        self.obj_sources(obj)
            .and_then(|(def, _)| def)
            .is_some_and(|def| def.affect_flags.iter().any(|f| item_affect_bit(*f) == bit))
    }

    /// Slot `n` is the object's `n`th `A` block, then empty slots, unless a
    /// script assigned the slot.
    fn obj_apply(&self, obj: Uid, slot: usize) -> (u8, i8) {
        let Some((def, over)) = self.obj_sources(obj) else {
            return (0, 0);
        };
        if slot >= MAX_OBJ_AFFECT {
            return (0, 0);
        }
        if let Some(assigned) = over.and_then(|over| over.applies.get(slot).copied().flatten()) {
            return assigned;
        }
        def.and_then(|def| def.affects.get(slot))
            .map_or((0, 0), |affect| {
                let modifier = affect
                    .modifier
                    .clamp(i32::from(i8::MIN), i32::from(i8::MAX));
                (
                    apply_number(affect.location),
                    i8::try_from(modifier).unwrap_or(0),
                )
            })
    }

    fn obj_set_apply(&mut self, obj: Uid, slot: usize, location: u8, modifier: i8) {
        let Some(entity) = self.item_entity(obj) else {
            return;
        };
        self.with_override(entity, |over| {
            if let Some(cell) = over.applies.get_mut(slot) {
                *cell = Some((location, modifier));
            }
        });
    }

    /// Kept on the object; the pack matches an object's keywords against its
    /// prototype, so the new alias is not yet what a player's `get` matches.
    fn obj_set_alias(&mut self, obj: Uid, name: &str) {
        let Some(entity) = self.item_entity(obj) else {
            return;
        };
        let alias: String = name.chars().take(TEXT_MAX).collect();
        self.with_override(entity, |over| over.alias = Some(alias));
    }

    /// The pack's item `name` is the short description.
    fn obj_set_short_desc(&mut self, obj: Uid, text: &str) {
        let Some(entity) = self.item_entity(obj) else {
            return;
        };
        if let Some(mut item) = self.world.get_mut::<Item>(entity) {
            item.name = text.chars().take(TEXT_MAX).collect();
        }
    }

    /// The pack's item `description` is the long (on the ground) description.
    fn obj_set_long_desc(&mut self, obj: Uid, text: &str) {
        let Some(entity) = self.item_entity(obj) else {
            return;
        };
        if let Some(mut item) = self.world.get_mut::<Item>(entity) {
            item.description = text.chars().take(TEXT_MAX).collect();
        }
    }
}
