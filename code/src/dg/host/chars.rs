//! `CharHost`: character attributes.
//!
//! Players keep their facts in the pack's components: `TbaProgress` (class,
//! level, experience), `TbaCombatant` (the vital pools, rolls, abilities,
//! saves, position, fight target), `TbaAlignment`, `TbaGold`, `TbaBuffs` and
//! `TbaFollowing`. NPCs have a `TbaCombatant` too, and everything else comes
//! from the prototype in `TbaContent::npc_stats`. What the pack does not model
//! but scripts use lives in [`chardata`]'s small components: title, practices,
//! quest points, conditions and skill percentages ([`TbaCharExtras`]), an NPC's
//! purse ([`TbaNpcPurse`]), act flags set on the fly ([`TbaActFlags`]).
//!
//! # Derived statistics
//!
//! A player's `TbaCombatant` is rebuilt every frame by
//! `recompute_equipment_affects` from class, level, worn items and
//! `TbaBuffs`, so a write to one of its derived fields (maxima, rolls, saves,
//! abilities) would last one frame. The setters that take such a field write
//! it (so a read in the same frame agrees) *and* the difference from what the
//! rebuild would give into a pinned permanent-adjustment `TbaBuffs` entry
//! ([`crate::spells::ActiveBuff::pinned`]: a cast never replaces it, the
//! rebuild keeps applying it and the save keeps it; the base value a script
//! reads back is derived from it). An NPC's `TbaCombatant` is only rebuilt for
//! an equipped NPC, from `TbaNpcBase`; its setters write the combatant and add
//! the same difference to the base.
//!
//! # Facts that cannot be logged at once
//!
//! `BaseHost::unsupported` needs `&mut self`, so a read of an unmodelled fact
//! (sex, weight, preference flags, `str_add`, quest completion) answers the
//! neutral value from `HOST.md` and counts itself in [`TbaUnmodelledReads`];
//! the counts reach `DgLog` at the next mutable call this host makes. The
//! resource is created by that first mutable call, so reads before it go
//! uncounted unless the runtime creates it at start.

#[path = "../chardata.rs"]
pub mod chardata;

use bevy::ecs::change_detection::Ref;
use dg_vm::fields_char::{CharAbil, CharFact, CharHost, CharStat};
use dg_vm::{BaseHost, Uid};
use mib_core::prelude::*;
use mib_stdlib::prelude::{Item, Npc, NpcBehaviors, Player, PlayerQuestLog, QuestId, QuestIndex};

use super::EcsHost;
use super::magic::spell_info;
use crate::combat::{
    TbaAlignment, TbaCombatant, TbaGold, TbaNpcBase, TbaProgress, default_class, derive_vitals,
    level_for_xp,
};
use crate::content::{AffectDef, AffectLocation, NpcBehaviour, Position, TbaItemDef, TbaNpcStats};
use crate::equipment::{TbaEquipment, recompute_combatant_full};
use crate::party::TbaFollowing;
use crate::skills::{SKILL_LEARNED, skill_min_level};
use crate::spells::{ActiveBuff, PINNED_FOREVER, TbaBuffs};
use chardata::{
    TbaActFlags, TbaCharExtras, TbaDgAffects, TbaDummyMob, TbaNpcPurse, TbaUnmodelledReads,
};

/// `MAX_GOLD`: the most gold a character holds.
const MAX_GOLD: i64 = 2_140_000_000;
/// `CONFIG_MAX_EXP_GAIN`: the most experience one `gain_exp` grants.
const MAX_EXP_GAIN: i32 = 100_000;
/// `CONFIG_MAX_EXP_LOSS`: the most experience one `gain_exp` takes.
const MAX_EXP_LOSS: i32 = 500_000;
/// `LVL_IMMORT`: `gain_exp` neither levels nor touches a character from here.
const LVL_IMMORT: i32 = 31;
/// Pulses per second of the script clock.
const PULSES_PER_SECOND: f32 = 10.0;
/// Pulses in one combat round.
pub(super) const ROUND_PULSES: i64 = (crate::combat::ROUND_SECONDS * PULSES_PER_SECOND) as i64;
/// `MOB_ISNPC`.
const MOB_ISNPC: u32 = 3;

/// The `MOB_*` act bits the pack's NPC behaviours stand for.
const BEHAVIOUR_BITS: [(NpcBehaviour, u32); 17] = [
    (NpcBehaviour::Sentinel, 1),
    (NpcBehaviour::Scavenger, 2),
    (NpcBehaviour::Aware, 4),
    (NpcBehaviour::Aggressive, 5),
    (NpcBehaviour::StayZone, 6),
    (NpcBehaviour::Wimpy, 7),
    (NpcBehaviour::AggrEvil, 8),
    (NpcBehaviour::AggrGood, 9),
    (NpcBehaviour::AggrNeutral, 10),
    (NpcBehaviour::Memory, 11),
    (NpcBehaviour::Helper, 12),
    (NpcBehaviour::NoCharm, 13),
    (NpcBehaviour::NoSummon, 14),
    (NpcBehaviour::NoSleep, 15),
    (NpcBehaviour::NoBash, 16),
    (NpcBehaviour::NoBlind, 17),
    (NpcBehaviour::NoKill, 18),
];

/// `CLASS_*` by number: the pack's class ids in `pc_class_types` order.
const CLASS_IDS: [&str; 4] = ["magic_user", "cleric", "thief", "warrior"];

/// `dex_app[].defensive` (`constants.c`), by dexterity `0..=25`.
const DEX_DEFENSIVE: [i32; 26] = [
    6, 5, 5, 4, 3, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, -1, -2, -3, -4, -4, -4, -5, -5, -5, -6, -6,
];

/// The `POS_*` number of a position.
fn position_number(position: Position) -> i32 {
    match position {
        Position::Dead => 0,
        Position::MortallyWounded => 1,
        Position::Incapacitated => 2,
        Position::Stunned => 3,
        Position::Sleeping => 4,
        Position::Resting => 5,
        Position::Sitting => 6,
        Position::Fighting => 7,
        Position::Standing => 8,
    }
}

/// The position at a `POS_*` number.
fn position_at(number: i32) -> Option<Position> {
    Some(match number {
        0 => Position::Dead,
        1 => Position::MortallyWounded,
        2 => Position::Incapacitated,
        3 => Position::Stunned,
        4 => Position::Sleeping,
        5 => Position::Resting,
        6 => Position::Sitting,
        7 => Position::Fighting,
        8 => Position::Standing,
        _ => return None,
    })
}

/// A class id folded to the form the pack authors them in.
pub(super) fn class_key(class: &str) -> String {
    class.trim().to_lowercase().replace(['-', ' '], "_")
}

/// The `CLASS_*` number of a pack class id; `CLASS_UNDEFINED` (-1) for one the
/// C has no number for.
fn class_number(class: &str) -> i32 {
    let key = class_key(class);
    CLASS_IDS
        .iter()
        .position(|id| *id == key)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
}

/// The affect location of an `APPLY_*` number; `None` for the ones the pack
/// has no field for (`NONE`, class, level, age, weight, height, gold, exp,
/// race).
pub(super) fn affect_location(apply: i32) -> Option<AffectLocation> {
    Some(match apply {
        1 => AffectLocation::Str,
        2 => AffectLocation::Dex,
        3 => AffectLocation::Int,
        4 => AffectLocation::Wis,
        5 => AffectLocation::Con,
        6 => AffectLocation::Cha,
        12 => AffectLocation::MaxMana,
        13 => AffectLocation::MaxHp,
        14 => AffectLocation::MaxMove,
        17 => AffectLocation::ArmorClass,
        18 => AffectLocation::Hitroll,
        19 => AffectLocation::Damroll,
        20 => AffectLocation::SavePara,
        21 => AffectLocation::SaveRod,
        22 => AffectLocation::SavePetri,
        23 => AffectLocation::SaveBreath,
        24 => AffectLocation::SaveSpell,
        _ => return None,
    })
}

/// The field of a combatant an affect location modifies.
fn derived_field(combatant: &mut TbaCombatant, location: AffectLocation) -> &mut i32 {
    match location {
        AffectLocation::Str => &mut combatant.attributes.strength,
        AffectLocation::Dex => &mut combatant.attributes.dexterity,
        AffectLocation::Int => &mut combatant.attributes.intelligence,
        AffectLocation::Wis => &mut combatant.attributes.wisdom,
        AffectLocation::Con => &mut combatant.attributes.constitution,
        AffectLocation::Cha => &mut combatant.attributes.charisma,
        AffectLocation::Hitroll => &mut combatant.hitroll,
        AffectLocation::Damroll => &mut combatant.damroll,
        AffectLocation::ArmorClass => &mut combatant.armor_class,
        AffectLocation::MaxHp => &mut combatant.max_hp,
        AffectLocation::MaxMana => &mut combatant.max_mana,
        AffectLocation::MaxMove => &mut combatant.max_move_points,
        AffectLocation::SavePara => &mut combatant.saves.para,
        AffectLocation::SaveRod => &mut combatant.saves.rod,
        AffectLocation::SavePetri => &mut combatant.saves.petri,
        AffectLocation::SaveBreath => &mut combatant.saves.breath,
        AffectLocation::SaveSpell => &mut combatant.saves.spell,
    }
}

/// The value of a derived field.
fn derived_value(combatant: &TbaCombatant, location: AffectLocation) -> i32 {
    let mut copy = combatant.clone();
    *derived_field(&mut copy, location)
}

/// The derived statistic a [`CharStat`] reads, if it is one.
fn stat_location(stat: CharStat) -> Option<AffectLocation> {
    Some(match stat {
        CharStat::Damroll => AffectLocation::Damroll,
        CharStat::Hitroll => AffectLocation::Hitroll,
        CharStat::MaxHit => AffectLocation::MaxHp,
        CharStat::MaxMana => AffectLocation::MaxMana,
        CharStat::MaxMove => AffectLocation::MaxMove,
        CharStat::SaveBreath => AffectLocation::SaveBreath,
        CharStat::SavePara => AffectLocation::SavePara,
        CharStat::SavePetri => AffectLocation::SavePetri,
        CharStat::SaveRod => AffectLocation::SaveRod,
        CharStat::SaveSpell => AffectLocation::SaveSpell,
        _ => return None,
    })
}

/// The affect location of an ability score; `None` for `str_add`.
fn abil_location(abil: CharAbil) -> Option<AffectLocation> {
    Some(match abil {
        CharAbil::Str => AffectLocation::Str,
        CharAbil::Int => AffectLocation::Int,
        CharAbil::Wis => AffectLocation::Wis,
        CharAbil::Dex => AffectLocation::Dex,
        CharAbil::Con => AffectLocation::Con,
        CharAbil::Cha => AffectLocation::Cha,
        CharAbil::StrAdd => return None,
    })
}

/// `AWAKE(ch)`: above `POS_SLEEPING`.
fn awake(position: Position) -> bool {
    position > Position::Sleeping
}

impl EcsHost<'_> {
    /// The live character (player or NPC) a script id names.
    pub(super) fn character(&self, uid: Uid) -> Option<Entity> {
        let entity = self.entity(uid)?;
        self.is_character(entity).then_some(entity)
    }

    /// Whether the entity is an NPC.
    fn is_npc_entity(&self, entity: Entity) -> bool {
        self.world.get::<Npc>(entity).is_some()
    }

    /// An NPC's prototype statistics.
    fn npc_stats(&self, entity: Entity) -> Option<&TbaNpcStats> {
        let key = self.world.get::<mib_stdlib::prelude::NpcKey>(entity)?;
        self.content()?.npc_stats.get(&key.0)
    }

    /// The script-written extras of a character, defaults when it has none.
    fn extras(&self, entity: Entity) -> TbaCharExtras {
        self.world
            .get::<TbaCharExtras>(entity)
            .cloned()
            .unwrap_or_default()
    }

    /// The extras component for writing, inserted if missing.
    fn extras_mut(&mut self, entity: Entity) -> Option<Mut<'_, TbaCharExtras>> {
        self.world.get_entity(entity).ok()?;
        if self.world.get::<TbaCharExtras>(entity).is_none() {
            self.world
                .entity_mut(entity)
                .insert(TbaCharExtras::default());
        }
        self.world.get_mut::<TbaCharExtras>(entity)
    }

    /// The player-special record of a character: its own, or for an NPC the
    /// one every NPC shares (`CHECK_PLAYER_SPECIAL`).
    fn special(&self, entity: Entity) -> (i32, i32, i32, i32, i32) {
        if self.is_npc_entity(entity) {
            let dummy = self.world.get_resource::<TbaDummyMob>();
            let dummy = dummy.cloned().unwrap_or_default();
            return (
                dummy.practices,
                dummy.quest_points,
                dummy.hunger,
                dummy.thirst,
                dummy.drunk,
            );
        }
        let extras = self.extras(entity);
        (
            extras.practices,
            extras.quest_points,
            extras.hunger,
            extras.thirst,
            extras.drunk,
        )
    }

    /// Store one player-special value: on the character, or on the shared NPC
    /// record.
    fn write_special(
        &mut self,
        entity: Entity,
        value: i32,
        field: fn(&mut TbaCharExtras) -> &mut i32,
        dummy_field: fn(&mut TbaDummyMob) -> &mut i32,
    ) {
        if self.is_npc_entity(entity) {
            let mut dummy = self.world.get_resource_or_insert_with(TbaDummyMob::default);
            *dummy_field(&mut dummy) = value;
        } else if let Some(mut extras) = self.extras_mut(entity) {
            *field(&mut extras) = value;
        }
    }

    /// The pack affects that make a player's rebuild: worn item definitions
    /// and live buffs.
    fn worn_defs(&self, entity: Entity) -> Vec<TbaItemDef> {
        let Some(content) = self.content() else {
            return Vec::new();
        };
        let Some(equipment) = self.world.get::<TbaEquipment>(entity) else {
            return Vec::new();
        };
        equipment
            .slots
            .values()
            .filter_map(|item| {
                let item = self.world.get::<Item>(*item)?;
                content.items.get(&item.def_id).cloned()
            })
            .collect()
    }

    /// Rebuild a character's `TbaCombatant` now, as the frame's recompute
    /// would: from class and level (players) or `TbaNpcBase` (equipped NPCs),
    /// worn items and, for players, buffs.
    pub(super) fn rebuild(&mut self, entity: Entity) {
        let Some(current) = self.world.get::<TbaCombatant>(entity).cloned() else {
            return;
        };
        let worn = self.worn_defs(entity);
        let next = if self.world.get::<Player>(entity).is_some() {
            let Some(progress) = self.world.get::<TbaProgress>(entity) else {
                return;
            };
            let class = self
                .content()
                .and_then(|content| content.classes.get(&progress.class))
                .cloned()
                .unwrap_or_else(default_class);
            let base = derive_vitals(&class, progress.level);
            let buffs: Vec<AffectDef> = self
                .world
                .get::<TbaBuffs>(entity)
                .map(|buffs| buffs.affects().cloned().collect())
                .unwrap_or_default();
            recompute_combatant_full(&current, base, worn.iter(), buffs.iter())
        } else if let (Some(base), Some(_)) = (
            self.world.get::<TbaNpcBase>(entity),
            self.world.get::<TbaEquipment>(entity),
        ) {
            recompute_combatant_full(&current, base.0.clone(), worn.iter(), std::iter::empty())
        } else {
            return;
        };
        if current != next
            && let Some(mut combatant) = self.world.get_mut::<TbaCombatant>(entity)
        {
            *combatant = next;
        }
    }

    /// Store a derived statistic so it lasts. See the module docs.
    fn write_derived(&mut self, entity: Entity, location: AffectLocation, value: i32) {
        self.flush_reads();
        let Some(current) = self
            .world
            .get::<TbaCombatant>(entity)
            .map(|combatant| derived_value(combatant, location))
        else {
            self.unsupported(
                "char_set_stat_no_combat_state",
                "the character has no combat state yet",
            );
            return;
        };
        if let Some(mut combatant) = self.world.get_mut::<TbaCombatant>(entity) {
            *derived_field(&mut combatant, location) = value;
        }
        let delta = value.saturating_sub(current);
        if self.is_npc_entity(entity) {
            // The rebuild adds the worn items to the base, so the base takes
            // the difference, not the affected value.
            if let Some(mut base) = self.world.get_mut::<TbaNpcBase>(entity) {
                let field = derived_field(&mut base.0, location);
                *field = field.saturating_add(delta);
            }
            return;
        }
        self.adjust_permanent(entity, location, delta);
    }

    /// Add `delta` to the permanent adjustment of a derived statistic: the
    /// pinned `TbaBuffs` entry that keeps the rebuild from undoing a script's
    /// write.
    fn adjust_permanent(&mut self, entity: Entity, location: AffectLocation, delta: i32) {
        if delta == 0 {
            return;
        }
        if self.world.get::<TbaBuffs>(entity).is_none() {
            self.world.entity_mut(entity).insert(TbaBuffs::default());
        }
        let Some(mut buffs) = self.world.get_mut::<TbaBuffs>(entity) else {
            return;
        };
        let slot = buffs
            .buffs
            .iter()
            .position(|buff| buff.is_adjustment() && buff.affect.location == location);
        match slot {
            Some(index) => {
                let new = buffs
                    .buffs
                    .get(index)
                    .map_or(delta, |buff| buff.affect.modifier.saturating_add(delta));
                if new == 0 {
                    buffs.buffs.remove(index);
                } else if let Some(buff) = buffs.buffs.get_mut(index) {
                    buff.affect.modifier = new;
                }
            }
            None => buffs.buffs.push(ActiveBuff::pinned(
                AffectDef {
                    location,
                    modifier: delta,
                },
                PINNED_FOREVER,
                true,
            )),
        }
    }

    /// The permanent adjustment scripts made to a derived statistic.
    fn permanent_adjustment(&self, entity: Entity, location: AffectLocation) -> i32 {
        self.world.get::<TbaBuffs>(entity).map_or(0, |buffs| {
            buffs
                .buffs
                .iter()
                .filter(|buff| buff.is_adjustment() && buff.affect.location == location)
                .fold(0, |sum, buff| sum.saturating_add(buff.affect.modifier))
        })
    }

    /// Count a read of a fact the pack does not model, if the counter exists.
    pub(super) fn note_read(&self, key: &'static str, detail: &'static str) {
        if let Some(reads) = self.world.get_resource::<TbaUnmodelledReads>() {
            reads.note(key, detail);
        }
    }

    /// Move the counted unmodelled reads into `DgLog`, creating the counter
    /// when it is not there yet.
    pub(super) fn flush_reads(&mut self) {
        let taken = self
            .world
            .get_resource_or_insert_with(TbaUnmodelledReads::default)
            .take();
        for (key, detail, count) in taken {
            for _ in 0..count.min(1000) {
                self.unsupported(key, detail);
            }
        }
    }

    /// The act-array bit a prototype and the engine's behaviour flags give an
    /// NPC.
    fn npc_act_base(&self, entity: Entity, bit: u32) -> bool {
        if bit == MOB_ISNPC {
            return true;
        }
        let prototype = self.npc_stats(entity).is_some_and(|stats| {
            BEHAVIOUR_BITS
                .iter()
                .any(|(behaviour, number)| *number == bit && stats.behaviour.contains(behaviour))
        });
        let engine = self.world.get::<NpcBehaviors>(entity).is_some_and(|flags| {
            (bit == 1 && flags.0.sentinel)
                || (bit == 2 && flags.0.scavenger)
                || (bit == 7 && flags.0.wimpy)
        });
        prototype || engine
    }

    /// `GET_SKILL` of a character: a script's percentage, else the class
    /// grant (see [`CharHost::char_skill`]).
    fn skill_value(&self, entity: Entity, skill: i32) -> i32 {
        if self.is_npc_entity(entity) {
            return self
                .world
                .get_resource::<TbaDummyMob>()
                .and_then(|dummy| dummy.skills.get(&skill).copied())
                .unwrap_or(0);
        }
        if let Some(value) = self
            .world
            .get::<TbaCharExtras>(entity)
            .and_then(|extras| extras.skills.get(&skill).copied())
        {
            return value;
        }
        let (Some(content), Some(progress)) =
            (self.content(), self.world.get::<TbaProgress>(entity))
        else {
            return 0;
        };
        let Some(info) = spell_info(skill) else {
            return 0;
        };
        let Some(class) = content.classes.get(&progress.class) else {
            return 0;
        };
        let granted = class
            .granted_skills
            .iter()
            .any(|granted| class_key(granted) == info.id);
        // The pack gates a cast on the grant alone and a skill on its level.
        let learned = granted
            && (skill <= dg_vm::magic::MAX_SPELLS
                || progress.level >= skill_min_level(content, &progress.class, info.id));
        if learned { SKILL_LEARNED } else { 0 }
    }
}

impl CharHost for EcsHost<'_> {
    /// Reads the pack's component for each statistic. Neutral where the pack
    /// has no model: sex (0), wait state (0).
    fn char_stat(&self, ch: Uid, stat: CharStat) -> i32 {
        let Some(entity) = self.character(ch) else {
            return 0;
        };
        let combatant = self.world.get::<TbaCombatant>(entity);
        let stats = self.npc_stats(entity);
        if let (Some(location), Some(combatant)) = (stat_location(stat), combatant) {
            return derived_value(combatant, location);
        }
        let (practices, quest_points, hunger, thirst, drunk) = self.special(entity);
        match stat {
            CharStat::Align => self.alignment(entity),
            CharStat::Damroll => stats.map_or(0, |stats| stats.damroll),
            CharStat::Hitroll => stats.map_or(0, |stats| stats.hitroll),
            CharStat::Hit => combatant.map_or(0, |combatant| combatant.hp),
            CharStat::Mana => combatant.map_or(0, |combatant| combatant.mana),
            CharStat::Move => combatant.map_or(0, |combatant| combatant.move_points),
            CharStat::Sex => {
                self.note_read("char_sex", "the pack models no sex");
                0
            }
            CharStat::WaitState => 0,
            CharStat::Class => self
                .world
                .get::<TbaProgress>(entity)
                .map_or(0, |progress| class_number(&progress.class)),
            // A character with no combat state is alive and on its feet.
            CharStat::Pos => position_number(
                combatant
                    .map(|combatant| combatant.position)
                    .or_else(|| stats.map(|stats| stats.default_position))
                    .unwrap_or(Position::Standing),
            ),
            CharStat::Practices => practices,
            CharStat::QuestPoints => quest_points,
            CharStat::Drunk => drunk,
            CharStat::Hunger => hunger,
            CharStat::Thirst => thirst,
            CharStat::MaxHit
            | CharStat::MaxMana
            | CharStat::MaxMove
            | CharStat::SaveBreath
            | CharStat::SavePara
            | CharStat::SavePetri
            | CharStat::SaveRod
            | CharStat::SaveSpell => 0,
        }
    }

    /// Gold is the player's `TbaGold` or the NPC's purse (the prototype's gold
    /// until a script changes it), experience the player's `TbaProgress.xp`
    /// or the NPC's purse. The carried weight is not `GET_WEIGHT` and the
    /// pack models neither: 0.
    fn char_fact(&self, ch: Uid, fact: CharFact) -> i32 {
        let Some(entity) = self.character(ch) else {
            return 0;
        };
        match fact {
            CharFact::Gold => self.purse(entity).0,
            CharFact::Exp => self.purse(entity).1,
            CharFact::Weight => {
                self.note_read("char_weight", "the pack models no character weight");
                0
            }
        }
    }

    /// Hit, mana and move points, the position and the alignment are state
    /// the pack keeps and persists; they are written as given. The derived
    /// statistics go through [`EcsHost::write_derived`]. Practices, quest
    /// points and conditions go to the character's extras (the shared record
    /// for an NPC). Sex and the wait state are not modelled and are logged.
    fn char_set_stat(&mut self, ch: Uid, stat: CharStat, value: i32) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        if let Some(location) = stat_location(stat) {
            self.write_derived(entity, location, value);
            return;
        }
        match stat {
            CharStat::Hit | CharStat::Mana | CharStat::Move | CharStat::Pos => {
                self.write_vital(entity, stat, value);
            }
            CharStat::Align => {
                self.world.entity_mut(entity).insert(TbaAlignment(value));
            }
            CharStat::Class => self.write_class(entity, value),
            CharStat::Practices => {
                self.write_special(entity, value, |e| &mut e.practices, |d| &mut d.practices);
            }
            CharStat::QuestPoints => {
                self.write_special(
                    entity,
                    value,
                    |e| &mut e.quest_points,
                    |d| &mut d.quest_points,
                );
            }
            CharStat::Hunger => {
                self.write_special(entity, value, |e| &mut e.hunger, |d| &mut d.hunger);
            }
            CharStat::Thirst => {
                self.write_special(entity, value, |e| &mut e.thirst, |d| &mut d.thirst);
            }
            CharStat::Drunk => {
                self.write_special(entity, value, |e| &mut e.drunk, |d| &mut d.drunk);
            }
            CharStat::Sex => self.unsupported("char_set_sex", "the pack models no sex"),
            CharStat::WaitState => self.unsupported(
                "char_set_wait_state",
                "the pack models no command wait state",
            ),
            _ => {}
        }
    }

    /// A player's level is `TbaProgress.level`, which the next recompute
    /// builds the vitals from and the kill reward never lowers. The pack has
    /// no level 0 (its vitals and saves are tabulated from level 1), so 0 is
    /// stored as 1 where the C stores 0. An NPC's level is its prototype's;
    /// `WorldHost::level` cannot see an override, so the write is logged and
    /// dropped.
    fn char_set_level(&mut self, ch: Uid, level: i32) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        if let Some(mut progress) = self.world.get_mut::<TbaProgress>(entity) {
            progress.level = u32::try_from(level).unwrap_or(0).max(1);
        } else {
            self.unsupported(
                "char_set_level_npc",
                "an NPC's level is its prototype's and cannot be set",
            );
        }
    }

    /// `aff_abils`: the combatant's attributes, base 13 plus worn items and
    /// buffs. `str_add` is not modelled: 0.
    fn char_abil(&self, ch: Uid, abil: CharAbil) -> i32 {
        let Some(location) = abil_location(abil) else {
            self.note_read("char_str_add", "the pack models no exceptional strength");
            return 0;
        };
        let Some(entity) = self.character(ch) else {
            return 0;
        };
        self.world
            .get::<TbaCombatant>(entity)
            .map_or(0, |combatant| derived_value(combatant, location))
    }

    /// `real_abils`: the attribute without affects. The pack gives every
    /// character the baseline 13; a script's `char_set_real_abil` moves it.
    fn char_real_abil(&self, ch: Uid, abil: CharAbil) -> i32 {
        let Some(location) = abil_location(abil) else {
            self.note_read("char_str_add", "the pack models no exceptional strength");
            return 0;
        };
        let Some(entity) = self.character(ch) else {
            return 0;
        };
        if self.world.get::<TbaCombatant>(entity).is_none() {
            return 0;
        }
        if self.is_npc_entity(entity) {
            return self
                .world
                .get::<TbaNpcBase>(entity)
                .map_or(crate::combat::BASE_ATTRIBUTE, |base| {
                    derived_value(&base.0, location)
                });
        }
        crate::combat::BASE_ATTRIBUTE.saturating_add(self.permanent_adjustment(entity, location))
    }

    /// Stores the baseline: a player's as a permanent adjustment (the affected
    /// ability moves with it), an NPC's in both its combatant and its base.
    /// `str_add` is not modelled and is logged.
    fn char_set_real_abil(&mut self, ch: Uid, abil: CharAbil, value: i32) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        let Some(location) = abil_location(abil) else {
            self.unsupported(
                "char_set_str_add",
                "the pack models no exceptional strength",
            );
            return;
        };
        if self.is_npc_entity(entity) {
            let old = self.char_real_abil(ch, abil);
            let aff = self.char_abil(ch, abil);
            self.write_derived(
                entity,
                location,
                aff.saturating_add(value.saturating_sub(old)),
            );
            return;
        }
        let want = value.saturating_sub(crate::combat::BASE_ATTRIBUTE);
        let have = self.permanent_adjustment(entity, location);
        let Some(current) = self
            .world
            .get::<TbaCombatant>(entity)
            .map(|combatant| derived_value(combatant, location))
        else {
            self.unsupported(
                "char_set_stat_no_combat_state",
                "the character has no combat state yet",
            );
            return;
        };
        self.write_derived(
            entity,
            location,
            current.saturating_add(want.saturating_sub(have)),
        );
    }

    /// Rebuilds the combatant from its definition now rather than at the next
    /// frame (`affect_total`). An NPC without equipment has nothing to
    /// rebuild: its buffs do not count in the pack (the recompute for NPCs
    /// passes none), which is logged when it carries any.
    fn char_affect_total(&mut self, ch: Uid) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        self.rebuild(entity);
        if self.is_npc_entity(entity)
            && self
                .world
                .get::<TbaDgAffects>(entity)
                .is_some_and(|affects| !affects.0.is_empty())
        {
            self.unsupported(
                "char_npc_affects",
                "buffs and script affects do not change an NPC's statistics",
            );
        }
    }

    /// `compute_armor_class`: `GET_AC` plus ten times the dexterity bonus for
    /// a character who is awake, never below -100. The pack's own combat uses
    /// `armor_class` unadjusted.
    fn char_armor_class(&self, ch: Uid) -> i32 {
        let Some(entity) = self.character(ch) else {
            return 0;
        };
        let Some(combatant) = self.world.get::<TbaCombatant>(entity) else {
            return self.npc_stats(entity).map_or(0, |stats| stats.armor_class);
        };
        let mut armor = combatant.armor_class;
        if awake(combatant.position) {
            let dex = usize::try_from(combatant.attributes.dexterity.clamp(0, 25)).unwrap_or(0);
            armor = armor.saturating_add(DEX_DEFENSIVE.get(dex).copied().unwrap_or(0) * 10);
        }
        armor.max(-100)
    }

    /// `update_pos`, mapped to the pack's death rule. The C leaves a
    /// character at 0 to -10 hit points stunned, incapacitated or mortally
    /// wounded and bleeds it out; the pack has none of those states (its
    /// recompute clamps hit points at zero, nothing regenerates or bleeds a
    /// downed character, and `heal` refuses one), so any hit point count at or
    /// below zero is `Position::Dead`, which is what the pack's own damage
    /// does at zero. A script's damage then reaches `cmd_die`. A positive
    /// count stands a character up who was dead or down, as the C does.
    fn char_update_pos(&mut self, ch: Uid) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        let Some(mut combatant) = self.world.get_mut::<TbaCombatant>(entity) else {
            return;
        };
        let hit = combatant.hp;
        if hit > 0 && combatant.position > Position::Stunned {
            return;
        }
        if hit > 0 {
            combatant.position = Position::Standing;
        } else if combatant.position != Position::Dead {
            combatant.position = Position::Dead;
            combatant.target = None;
        }
    }

    /// `gain_exp`: an NPC's experience just changes; a player's is capped per
    /// gain and loss, never goes below zero, and a gain advances the level
    /// from the class's `level_exp` table (below `LVL_IMMORT`), with the C's
    /// "You rise a level!" line. The title reset, the mudlog line and the happy
    /// hour are not modelled.
    fn char_gain_exp(&mut self, ch: Uid, gain: i32) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        if self.is_npc_entity(entity) {
            self.note_npc_purse(entity);
            let (gold, exp) = self.purse(entity);
            self.world.entity_mut(entity).insert(TbaNpcPurse {
                gold,
                exp: exp.wrapping_add(gain),
            });
            return;
        }
        let class = self.world.get::<TbaProgress>(entity).and_then(|progress| {
            self.content()
                .and_then(|content| content.classes.get(&progress.class))
                .cloned()
        });
        let Some(mut progress) = self.world.get_mut::<TbaProgress>(entity) else {
            return;
        };
        let level = i32::try_from(progress.level).unwrap_or(i32::MAX);
        if !(1..LVL_IMMORT).contains(&level) {
            return;
        }
        let mut gained = 0;
        if gain > 0 {
            progress.xp = progress
                .xp
                .saturating_add(u64::from(gain.min(MAX_EXP_GAIN).unsigned_abs()));
            if let Some(class) = class {
                let reached = level_for_xp(&class, progress.xp).min(LVL_IMMORT as u32 - 1);
                let before = progress.level;
                progress.level = before.max(reached);
                gained = progress.level - before;
            }
        } else if gain < 0 {
            let loss = u64::from(gain.max(-MAX_EXP_LOSS).unsigned_abs());
            progress.xp = progress.xp.saturating_sub(loss);
        }
        match gained {
            0 => {}
            1 => crate::dg::out::send_to_char(self.world, entity, "You rise a level!"),
            n => crate::dg::out::send_to_char(self.world, entity, &format!("You rise {n} levels!")),
        }
    }

    /// `increase_gold`: clamped to `0..=MAX_GOLD`, with the C's line when a
    /// player reaches the maximum. A player's purse is `TbaGold`; an NPC's is
    /// its [`TbaNpcPurse`], which the kill reward does not read (logged on the
    /// first change).
    fn char_increase_gold(&mut self, ch: Uid, amt: i32) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        if self.is_npc_entity(entity) {
            self.note_npc_purse(entity);
        }
        let (gold, exp) = self.purse(entity);
        let next = (i64::from(gold) + i64::from(amt)).clamp(0, MAX_GOLD);
        let next = i32::try_from(next).unwrap_or(i32::MAX);
        if self.is_npc_entity(entity) {
            self.world
                .entity_mut(entity)
                .insert(TbaNpcPurse { gold: next, exp });
        } else {
            self.world
                .entity_mut(entity)
                .insert(TbaGold(u32::try_from(next).unwrap_or(0)));
            if i64::from(next) == MAX_GOLD {
                crate::dg::out::send_to_char(
                    self.world,
                    entity,
                    "You have reached the maximum gold!",
                );
                crate::dg::out::send_to_char(
                    self.world,
                    entity,
                    "You must spend it or bank it before you can gain any more.",
                );
            }
        }
    }

    /// `FIGHTING(ch)`: the combatant's target while it is alive and exists.
    fn char_fighting(&self, ch: Uid) -> Option<Uid> {
        let entity = self.character(ch)?;
        let target = self.world.get::<TbaCombatant>(entity)?.target?;
        let alive = self
            .world
            .get::<TbaCombatant>(target)
            .is_none_or(|combatant| combatant.position != Position::Dead);
        if !alive {
            return None;
        }
        self.character(self.uid(target)?).and_then(|e| self.uid(e))
    }

    /// `ch->master`: whom the character follows (`TbaFollowing`).
    fn char_master(&self, ch: Uid) -> Option<Uid> {
        let entity = self.character(ch)?;
        let leader = self.world.get::<TbaFollowing>(entity)?.leader;
        self.character(self.uid(leader)?).and_then(|e| self.uid(e))
    }

    /// `ch->followers->follower`: the head of the follower list. `add_follower`
    /// prepends, so the head is the follower added last: the one whose
    /// `TbaFollowing` is newest.
    fn char_first_follower(&self, ch: Uid) -> Option<Uid> {
        let leader = self.character(ch)?;
        let now = self.world.read_change_tick();
        let mut query = self.world.try_query::<(Entity, Ref<TbaFollowing>)>()?;
        let mut head: Option<(Entity, Ref<TbaFollowing>)> = None;
        for (entity, following) in query.iter(self.world) {
            if following.leader != leader || !self.is_character(entity) {
                continue;
            }
            let newer = match &head {
                None => true,
                Some((other, held)) => {
                    let (a, b) = (following.added(), held.added());
                    a.is_newer_than(b, now)
                        || (a.get() == b.get() && self.uid(entity) > self.uid(*other))
                }
            };
            if newer {
                head = Some((entity, following));
            }
        }
        self.uid(head?.0)
    }

    /// `affected_by_spell`: see [`EcsHost::affected_by`].
    fn char_affected_by_spell(&self, ch: Uid, spell: i32) -> bool {
        let Some(entity) = self.character(ch) else {
            return false;
        };
        self.affected_by(entity, spell)
    }

    /// `GET_SKILL`: a script's percentage if it wrote one; otherwise 100 for a
    /// spell or skill the class grants (the pack has no practice sessions, so
    /// a granted ability is fully learned, and a skill needs its minimum
    /// level), else 0. An NPC reads the shared record.
    fn char_skill(&self, ch: Uid, skill: i32) -> i32 {
        let Some(entity) = self.character(ch) else {
            return 0;
        };
        self.skill_value(entity, skill)
    }

    /// `SET_SKILL`: stored per character (the shared record for an NPC), over
    /// the class grant.
    fn char_set_skill(&mut self, ch: Uid, skill: i32, value: i32) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        if self.is_npc_entity(entity) {
            let mut dummy = self.world.get_resource_or_insert_with(TbaDummyMob::default);
            dummy.skills.insert(skill, value);
        } else if let Some(mut extras) = self.extras_mut(entity) {
            extras.skills.insert(skill, value);
        }
    }

    /// `MOB_FLAGS` of an NPC (the prototype's behaviours, the engine's
    /// sentinel / scavenger / wimpy flags, and what scripts changed) or
    /// `PLR_FLAGS` of a player (only what scripts set: the pack has no player
    /// flags).
    fn char_act_flagged(&self, ch: Uid, bit: u32) -> bool {
        let Some(entity) = self.character(ch) else {
            return false;
        };
        if let Some(flags) = self.world.get::<TbaActFlags>(entity) {
            if flags.set.contains(&bit) {
                return true;
            }
            if flags.cleared.contains(&bit) {
                return false;
            }
        }
        self.is_npc_entity(entity) && self.npc_act_base(entity, bit)
    }

    /// Records the change on the character. On an NPC the engine's scavenger
    /// flag follows it, and its sentinel flag where the NPC wanders (the
    /// engine's wander system is the only reader); every other bit is stored
    /// and read back but nothing in the pack acts on it (its AI reads the
    /// prototype, and nothing reads the wimpy flag), which is logged.
    fn char_set_act_flag(&mut self, ch: Uid, bit: u32, on: bool) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        let mut flags = self
            .world
            .get::<TbaActFlags>(entity)
            .cloned()
            .unwrap_or_default();
        if on {
            flags.cleared.remove(&bit);
            flags.set.insert(bit);
        } else {
            flags.set.remove(&bit);
            flags.cleared.insert(bit);
        }
        self.world.entity_mut(entity).insert(flags);
        let mut acted = false;
        if self.is_npc_entity(entity)
            && let Some(mut behaviours) = self.world.get_mut::<NpcBehaviors>(entity)
        {
            match bit {
                2 => {
                    behaviours.0.scavenger = on;
                    acted = true;
                }
                1 => {
                    behaviours.0.sentinel = on;
                    acted = behaviours.0.wander;
                }
                7 => behaviours.0.wimpy = on,
                _ => {}
            }
        }
        if !acted {
            self.unsupported(
                "char_act_flag_inert",
                &format!("act bit {bit} is recorded but no pack system reads it"),
            );
        }
    }

    /// The pack keeps no preferences: no bit is ever set.
    fn char_pref_flagged(&self, _ch: Uid, _bit: u32) -> bool {
        self.note_read("char_pref_flags", "the pack keeps no preference flags");
        false
    }

    /// `ch->player.short_descr`: an NPC's `Npc.name` (what `WorldHost::name`
    /// reads). A player has no short description of its own and keeps its
    /// name.
    fn char_set_short_descr(&mut self, ch: Uid, text: &str) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        if let Some(mut npc) = self.world.get_mut::<Npc>(entity) {
            npc.name = text.to_owned();
        }
    }

    /// `GET_TITLE`: what a script set, else the C's NULL.
    fn char_title(&self, ch: Uid) -> Option<String> {
        let entity = self.character(ch)?;
        self.world.get::<TbaCharExtras>(entity)?.title.clone()
    }

    fn char_set_title(&mut self, ch: Uid, title: &str) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        if let Some(mut extras) = self.extras_mut(entity) {
            extras.title = Some(title.to_owned());
        }
    }

    /// `GET_QUEST`: the vnum of the converted tbaMUD quest (`tba_quest_<vnum>`)
    /// the player has accepted and not turned in, else `NOTHING`. The C keeps
    /// one quest per player; the pack's log can hold several, and the lowest
    /// converted vnum answers.
    fn char_quest(&self, ch: Uid) -> i32 {
        let Some(entity) = self.character(ch) else {
            return -1;
        };
        self.world
            .get::<PlayerQuestLog>(entity)
            .and_then(|log| {
                log.quests
                    .keys()
                    .filter_map(|id| id.0.strip_prefix(QUEST_PREFIX).and_then(parse_vnum))
                    .min()
            })
            .unwrap_or(-1)
    }

    /// `real_quest(vnum) != NOTHING`: whether the quest index holds the
    /// converted quest `tba_quest_<vnum>`.
    fn char_quest_exists(&self, vnum: i32) -> bool {
        self.world
            .get_resource::<QuestIndex>()
            .is_some_and(|index| {
                index
                    .map
                    .contains_key(&QuestId::from(quest_key(vnum).as_str()))
            })
    }

    /// `is_complete`: the pack's turn-in removes the quest from the log and
    /// keeps no record of it, so a finished quest cannot be told from one never
    /// taken: always false, counted as a gap.
    fn char_quest_completed(&self, _ch: Uid, _vnum: i32) -> bool {
        self.note_read(
            "char_quest_completed",
            "the pack keeps no record of completed quests",
        );
        false
    }
}

/// The id prefix of a quest converted from a tbaMUD `.qst` file.
const QUEST_PREFIX: &str = "tba_quest_";

/// The quest id string for a vnum.
fn quest_key(vnum: i32) -> String {
    format!("{QUEST_PREFIX}{vnum}")
}

/// A vnum from the digits of a quest id.
fn parse_vnum(digits: &str) -> Option<i32> {
    digits.parse().ok()
}

impl EcsHost<'_> {
    /// `GET_ALIGNMENT`: the component, else the NPC prototype's, else neutral.
    fn alignment(&self, entity: Entity) -> i32 {
        if let Some(alignment) = self.world.get::<TbaAlignment>(entity) {
            return alignment.0;
        }
        self.npc_stats(entity).map_or(0, |stats| stats.alignment)
    }

    /// `(GET_GOLD, GET_EXP)` of a character.
    fn purse(&self, entity: Entity) -> (i32, i32) {
        let clamp = |value: u64| i32::try_from(value).unwrap_or(i32::MAX);
        if self.is_npc_entity(entity) {
            if let Some(purse) = self.world.get::<TbaNpcPurse>(entity) {
                return (purse.gold, purse.exp);
            }
            return self.npc_stats(entity).map_or((0, 0), |stats| {
                (
                    clamp(u64::from(stats.gold)),
                    clamp(u64::from(stats.experience)),
                )
            });
        }
        (
            self.world
                .get::<TbaGold>(entity)
                .map_or(0, |gold| clamp(u64::from(gold.0))),
            self.world
                .get::<TbaProgress>(entity)
                .map_or(0, |progress| clamp(progress.xp)),
        )
    }

    /// Write hit, mana, move or position straight into the combatant.
    fn write_vital(&mut self, entity: Entity, stat: CharStat, value: i32) {
        let Some(mut combatant) = self.world.get_mut::<TbaCombatant>(entity) else {
            self.unsupported(
                "char_set_stat_no_combat_state",
                "the character has no combat state yet",
            );
            return;
        };
        match stat {
            CharStat::Hit => combatant.hp = value,
            CharStat::Mana => combatant.mana = value,
            CharStat::Move => combatant.move_points = value,
            _ => {
                if let Some(position) = position_at(value) {
                    combatant.position = position;
                }
            }
        }
    }

    /// `GET_CLASS(ch) = value` for a player: the pack class id at that
    /// number. An NPC's class is not kept, and a number outside the four
    /// classes has no id; both are logged.
    fn write_class(&mut self, entity: Entity, value: i32) {
        let id = usize::try_from(value).ok().and_then(|n| CLASS_IDS.get(n));
        match (id, self.world.get_mut::<TbaProgress>(entity)) {
            (Some(id), Some(mut progress)) => progress.class = (*id).into(),
            _ => self.unsupported(
                "char_set_class",
                "only a player can take one of the four classes",
            ),
        }
    }

    /// Whether the character is affected by the spell number: a live script
    /// affect of it (a numeric one needs its pinned buff on a player), an
    /// unpinned buff equal to the pack spell's affect (buffs carry no spell
    /// tag, but a script's pinned buffs never read as a cast spell), or the
    /// true sight a detect spell grants.
    pub(super) fn affected_by(&self, entity: Entity, spell: i32) -> bool {
        let pulse = self.pulse();
        let player = self.world.get::<Player>(entity).is_some();
        let buffs = self.world.get::<TbaBuffs>(entity);
        let marked = self.world.get::<TbaDgAffects>(entity).is_some_and(|marks| {
            marks.0.iter().any(|mark| {
                mark.spell == spell
                    && mark.expires.is_none_or(|expires| expires > pulse)
                    && match (&mark.numeric, player) {
                        (Some(affect), true) => buffs.is_some_and(|buffs| {
                            buffs
                                .buffs
                                .iter()
                                .any(|buff| buff.is_pinned() && &buff.affect == affect)
                        }),
                        _ => true,
                    }
            })
        });
        if marked {
            return true;
        }
        let Some(info) = spell_info(spell) else {
            return false;
        };
        let Some(def) = self.content().and_then(|c| c.spells.get(info.id)) else {
            return false;
        };
        let by_buff = def.affect.as_ref().is_some_and(|affect| {
            buffs.is_some_and(|buffs| {
                buffs
                    .buffs
                    .iter()
                    .any(|buff| !buff.is_pinned() && &buff.affect == affect)
            })
        });
        let by_sight = matches!(info.id, "detect_invis" | "sense_life")
            && self
                .world
                .get::<crate::stealth::TbaTrueSight>(entity)
                .is_some();
        by_buff || by_sight
    }

    /// Log the first script write to an NPC's purse: the kill reward reads the
    /// prototype's gold and experience, not the purse.
    fn note_npc_purse(&mut self, entity: Entity) {
        if self.world.get::<TbaNpcPurse>(entity).is_none() {
            self.unsupported(
                "char_npc_purse_reward",
                "an NPC's changed gold and experience are not what its kill pays",
            );
        }
    }
}
