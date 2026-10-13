//! `ObjHost`: object attributes.
//!
//! In the pack the values, cost, rent, weight, extra and affect flags and
//! applies of an object are prototype data (`TbaContent.items`), shared by
//! every instance. The C keeps them per instance and scripts change them
//! (`osetval`, `otimer`, `%obj.cost%`, `oset apply`), so [`DgObjOverride`]
//! holds the per-instance changes and every read falls back from it to the
//! prototype.
//!
//! The timer a script sets is kept in [`DgObjOverride::timer`] with the MUD
//! hour it was set, and read back aged by the pack's hour clock
//! ([`TbaObjectClock`]), so `GET_OBJ_TIMER` counts down in the unit of
//! `point_update`. The pack's own decay ([`TbaItemTimer`], from the
//! prototype's `timer`) despawns the item at zero, which the C does only for
//! a corpse (it fires the Timer trigger and keeps any other object), so a
//! script-set timer must never reach it: setting one parks the component at
//! [`PARKED`], a value `tick_item_timers` cannot run down in practice and
//! `attach_item_timers` leaves alone. Until the intake stream fires the Timer
//! trigger the countdown only reads; and the override is not saved, so a
//! restored item reads zero.
//!
//! Script writes to the values, cost, rent, weight and applies are seen by
//! scripts only: the pack's combat, equipment and shops read the prototype.
//! Each such write is counted with `BaseHost::unsupported`.
//!
//! The extra and affect flags map from the pack's [`ItemExtraFlag`] and
//! [`ItemAffectFlag`] to the bit numbers of `structs.h` (`ITEM_*`, `AFF_*`),
//! and an apply location maps from [`AffectLocation`] to its `APPLY_*` number.
//! No script command changes a flag, so flags are read from the prototype
//! alone.

use dg_vm::fields_obj::{MAX_OBJ_AFFECT, ObjHost};
use dg_vm::{BaseHost, Uid};
use mib_core::prelude::*;
use mib_stdlib::prelude::Item;

use super::EcsHost;
use crate::content::{AffectLocation, ItemAffectFlag, ItemExtraFlag, TbaItemDef};
use crate::items::TbaItemTimer;
use crate::items::TbaObjectClock;

/// The longest alias, short and long description the pack keeps; the VM has
/// already applied the C's tighter caps, this only bounds a stray caller.
const TEXT_MAX: usize = 4096;

/// The [`TbaItemTimer`] value that stands for "a script owns the timer": far
/// beyond any real life, so the pack's decay neither reaches zero nor
/// despawns the item.
const PARKED: u32 = u32::MAX / 2;

/// Per-instance changes a script made to an object's prototype data. A field
/// that is `None` still reads from the prototype. Runtime-only: it is not
/// saved, so a restored object starts from its prototype again.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct DgObjOverride {
    /// `GET_OBJ_VAL(obj, 0..4)`.
    pub values: [Option<i32>; 4],
    /// `GET_OBJ_TIMER` as a script last set it, with the MUD hour (counted
    /// from the clock's year 0) it was set at; a positive value ages from then.
    pub timer: Option<(i32, u64)>,
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

    /// MUD hours elapsed on the pack's clock since year 0 (zero without one).
    fn mud_hours(&self) -> u64 {
        let Some(clock) = self.world.get_resource::<TbaObjectClock>() else {
            return 0;
        };
        let days = u64::from(clock.year)
            .saturating_mul(17)
            .saturating_add(u64::from(clock.month))
            .saturating_mul(35)
            .saturating_add(u64::from(clock.day));
        days.saturating_mul(24)
            .saturating_add(u64::from(clock.hour_of_day))
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
        self.unsupported("obj.val.effect", "the pack's systems read the prototype");
    }

    /// The timer a script set, aged by the MUD hours since; else the pack's
    /// live [`TbaItemTimer`]; else the prototype's `timer`.
    fn obj_timer(&self, obj: Uid) -> i32 {
        let Some(entity) = self.item_entity(obj) else {
            return 0;
        };
        if let Some((value, set_at)) = self.obj_override(entity).and_then(|over| over.timer) {
            if value <= 0 {
                return value;
            }
            let elapsed = self.mud_hours().saturating_sub(set_at);
            let left = i64::from(value).saturating_sub(i64::try_from(elapsed).unwrap_or(i64::MAX));
            return i32::try_from(left.max(0)).unwrap_or(i32::MAX);
        }
        if let Some(timer) = self.world.get::<TbaItemTimer>(entity) {
            return if timer.remaining >= PARKED {
                0
            } else {
                to_i32(timer.remaining)
            };
        }
        self.item_def(entity)
            .and_then(|def| def.timer)
            .map_or(0, to_i32)
    }

    /// Stores the value and parks the pack's decay timer, so the item never
    /// despawns at zero (see the module doc).
    fn obj_set_timer(&mut self, obj: Uid, value: i32) {
        let Some(entity) = self.item_entity(obj) else {
            return;
        };
        let now = self.mud_hours();
        self.with_override(entity, |over| over.timer = Some((value, now)));
        self.world
            .entity_mut(entity)
            .insert(TbaItemTimer { remaining: PARKED });
        self.unsupported("obj.timer.persist", "a script-set timer is not saved");
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
            self.unsupported("obj.cost.effect", "shops price from the prototype");
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
            self.unsupported("obj.rent.effect", "the pack has no rent");
        }
    }

    /// The object's own weight; the C's `GET_OBJ_WEIGHT` of a container also
    /// counts its contents, which this does not.
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
            self.unsupported("obj.weight.effect", "the pack weighs from the prototype");
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
                // `sbyte modifier` takes the C's truncating assignment.
                (apply_number(affect.location), affect.modifier as i8)
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
        self.unsupported("obj.apply.effect", "equipment bonuses read the prototype");
    }

    /// Kept on the object but read by nothing: `WorldHost::keywords` and the
    /// pack's matching use the prototype, so `%obj.name%` and a player's `get`
    /// still see the old keywords. Counted as `obj.alias`.
    fn obj_set_alias(&mut self, obj: Uid, name: &str) {
        let Some(entity) = self.item_entity(obj) else {
            return;
        };
        let alias: String = name.chars().take(TEXT_MAX).collect();
        self.with_override(entity, |over| over.alias = Some(alias));
        self.unsupported(
            "obj.alias",
            "the new keywords are not what the pack matches",
        );
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
