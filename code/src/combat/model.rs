//! TBA combat state: persistent progression and per-entity combat vitals.
//!
//! CircleMUD/TBA is an **independent genre** — native round-based combat
//! (THAC0/AC/damroll), not `classic-rpg`'s 3×3 tactical grid — so it owns its
//! own [`TbaCombatant`] rather than reusing `mib-classic-rpg`'s `Combatant`.
//!
//! [`TbaProgress`] is the character's class and experience; [`TbaCombatant`]
//! is the derived vital block. Both are registered with the standard-library
//! [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry) under
//! [`DocKind::Player`], so a character's level, XP, current HP/mana and worn
//! state survive a restart.

use mib_core::prelude::*;
use mib_stdlib::prelude::ItemId;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::content::Position;

/// Below this alignment a character counts as evil (Circle's `IS_EVIL`).
pub const EVIL_ALIGNMENT: i32 = -350;
/// Above this alignment a character counts as good (Circle's `IS_GOOD`).
pub const GOOD_ALIGNMENT: i32 = 350;

/// A combatant's moral alignment on Circle's `-1000..=1000` scale.
///
/// It is play state: a fresh character starts neutral (`0`) and every kill
/// shifts it toward the opposite of the victim's alignment (Circle's
/// `change_alignment`). Alignment-conditional aggro (`AggrEvil`/`AggrGood`/
/// `AggrNeutral`, [`crate::content::NpcBehaviour`]) reads it. Registered with
/// the stdlib [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry) under
/// [`DocKind::Player`](mib_persist::prelude::DocKind::Player), so a character's
/// moral standing survives a restart. A missing component is treated as
/// neutral (`0`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaAlignment(pub i32);

/// Coins carried by a player.
///
/// CircleMUD keeps gold on the character and, on death, moves it to the
/// corpse (TBA-E09). This component is that purse: the death penalty system
/// converts it into a carried [`tba_gold`](crate::combat::death::GOLD_ITEM_ID)
/// item so the stdlib corpse sweep collects it. Registered with the stdlib
/// [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry) under
/// [`DocKind::Player`](mib_persist::prelude::DocKind::Player), so a purse
/// survives a restart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaGold(pub u32);

/// The melee weapon a combatant currently wields.
///
/// This is the **narrow damage seam** for the equipment pass (TBA-E28): when
/// [`sync_wielded_weapon`](crate::equipment::sync_wielded_weapon) sets this
/// component, [`resolve_rounds`](crate::combat::resolve_rounds) reads the
/// item's `tba.items` weapon dice instead of the unarmed default. A missing
/// component falls through to [`TbaAttackDice`] (NPCs) then unarmed.
#[derive(Debug, Clone, Component)]
pub struct WieldedWeapon(pub ItemId);

/// An NPC's natural melee damage dice, from `tba.npc_stats.damage`.
///
/// Circle mobs deal the source record's `#d#+#` rather than an unarmed roll.
/// [`npc_combatant_system`](crate::combat::npc_combatant_system) attaches this
/// from the prototype; [`resolve_rounds`](crate::combat::resolve_rounds)
/// prefers a wielded weapon, then these dice, then `1d4`. Runtime-only: the
/// prototype lives in `tba.npc_stats` and is not persisted for NPCs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component)]
pub struct TbaAttackDice(pub Dice);

/// A character's Diku class and experience (TBA-E18/E19).
///
/// `class` keys into [`TbaContent::classes`](crate::module::TbaContent::classes);
/// the level and XP are authoritative and the vitals are recomputed from them.
#[derive(Debug, Clone, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaProgress {
    /// Key into `tba.classes` (e.g. `warrior`, `magic_user`).
    pub class: SmolStr,
    /// Character level, starting at 1.
    pub level: u32,
    /// Total experience points accumulated.
    pub xp: u64,
}

impl TbaProgress {
    /// A level-1 character of `class`.
    pub fn new(class: impl Into<SmolStr>) -> Self {
        Self {
            class: class.into(),
            level: 1,
            xp: 0,
        }
    }
}

/// Circle's `"no bonus"` baseline for a prime attribute.
///
/// `tbamud/src/fight.c:832-833` subtracts `(stat - 13) / 1.5` from THAC0, so a
/// score of 13 contributes nothing; `str_app[13].tohit` is likewise `0`
/// (`tbamud/src/constants.c:621`). It is the value an affect is applied to.
pub const BASE_ATTRIBUTE: i32 = 13;

/// The six Diku prime attributes, rebuilt from [`BASE_ATTRIBUTE`] plus the
/// worn-item and buff affects. Keeping them grouped keeps a [`TbaCombatant`]
/// literal short and makes the "base + affects" rebuild a single assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbaAttributes {
    pub strength: i32,
    pub dexterity: i32,
    pub intelligence: i32,
    pub wisdom: i32,
    pub constitution: i32,
    pub charisma: i32,
}

impl Default for TbaAttributes {
    fn default() -> Self {
        Self::neutral()
    }
}

impl TbaAttributes {
    /// Every attribute at Circle's no-bonus baseline (13).
    pub const fn neutral() -> Self {
        Self {
            strength: BASE_ATTRIBUTE,
            dexterity: BASE_ATTRIBUTE,
            intelligence: BASE_ATTRIBUTE,
            wisdom: BASE_ATTRIBUTE,
            constitution: BASE_ATTRIBUTE,
            charisma: BASE_ATTRIBUTE,
        }
    }

    /// The THAC0 reduction contributed by INT and WIS.
    ///
    /// Exactly `compute_thaco`'s terms (`tbamud/src/fight.c:832-833`): the C
    /// integer cast truncates toward zero, which Rust's float-to-int `as`
    /// matches.
    fn ability_thaco_bonus(stat: i32) -> i32 {
        (f64::from(stat - BASE_ATTRIBUTE) / 1.5) as i32
    }

    /// The total to-hit adjustment from the attributes, in the sense of
    /// "subtract this from the attack roll" (i.e. Circle's `compute_thaco`
    /// reductions): `str_app[].tohit` plus the INT and WIS terms.
    ///
    /// `str_app` is transcribed at `tbamud/src/constants.c:607-637`; exceptional
    /// strength (`str_add`) is not modelled this phase, so the index is the
    /// plain score clamped to the table's `0..=25` range.
    pub fn to_hit_bonus(&self) -> i32 {
        strength_to_hit(self.strength)
            + Self::ability_thaco_bonus(self.intelligence)
            + Self::ability_thaco_bonus(self.wisdom)
    }
}

/// The `str_app[].tohit` column (`tbamud/src/constants.c:607-637`).
///
/// Index 0..=25 are the plain-strength rows; 26..=30 are the exceptional
/// 18/xx rows and are unreachable without a `str_add` model. An out-of-range
/// score clamps to the table's ends.
fn strength_to_hit(strength: i32) -> i32 {
    const STR_TO_HIT: [i32; 26] = [
        -5, -5, -3, -3, -2, -2, -1, -1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 3, 3, 4, 4, 5, 6, 7,
    ];
    let index = strength.clamp(0, 25) as usize;
    STR_TO_HIT[index]
}

/// The five Circle saving-throw categories (`SAVING_*`, `tbamud/src/spells.h:152-156`),
/// in the source's numeric order.
///
/// `cast` always resolves `SAVING_SPELL` (`call_magic` maps `CAST_SPELL` to
/// it, `tbamud/src/spell_parser.c:205-222`); the other four exist because the
/// same `apply_saving_throw[]` block feeds wands, staves and scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveType {
    /// `SAVING_PARA`.
    Para,
    /// `SAVING_ROD`.
    Rod,
    /// `SAVING_PETRI`.
    Petri,
    /// `SAVING_BREATH`.
    Breath,
    /// `SAVING_SPELL`.
    Spell,
}

impl SaveType {
    /// Every category, in `SAVING_*` order.
    pub const ALL: [SaveType; 5] = [
        SaveType::Para,
        SaveType::Rod,
        SaveType::Petri,
        SaveType::Breath,
        SaveType::Spell,
    ];

    /// The source's numeric value (`SAVING_PARA` 0 .. `SAVING_SPELL` 4).
    pub const fn index(self) -> usize {
        match self {
            SaveType::Para => 0,
            SaveType::Rod => 1,
            SaveType::Petri => 2,
            SaveType::Breath => 3,
            SaveType::Spell => 4,
        }
    }
}

/// Affect-applied saving-throw modifiers, Circle's `apply_saving_throw[]`.
///
/// These start at zero ("neutral") and accumulate the `save_*` item/buff
/// affects; the class/level base throw lives in [`saving_throws`](super::saves::saving_throws)
/// (`tbamud/src/class.c:160`) and is applied when a save is actually rolled.
/// A negative modifier makes a save *better*, exactly as the source comment at
/// `tbamud/src/magic.c:30-34` warns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbaSaves {
    pub para: i32,
    pub rod: i32,
    pub petri: i32,
    pub breath: i32,
    pub spell: i32,
}

impl TbaSaves {
    /// The modifier for one category, indexed like `apply_saving_throw[]`.
    pub const fn get(&self, save: SaveType) -> i32 {
        match save {
            SaveType::Para => self.para,
            SaveType::Rod => self.rod,
            SaveType::Petri => self.petri,
            SaveType::Breath => self.breath,
            SaveType::Spell => self.spell,
        }
    }
}

/// A player hidden from NPC aggro and room listings (Circle's `AFF_HIDE`).
///
/// Set by the `hide` skill ([`crate::skills`]); cleared by attacking (Circle's
/// `appear()`, `tbamud/src/fight.c:70-84`) or by a failed re-hide. It is
/// **runtime-only** and never registered with the stdlib
/// [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry): MIB does not persist
/// the affect-flag bitvector Circle keeps in its player file, so stealth is
/// transient play state here. A missing component is "visible".
///
/// Circle gates a mob's `CAN_SEE` on `AFF_HIDE` (`tbamud/src/utils.h:798-799`);
/// [`crate::combat::ai`] honours that for aggro. The stdlib `look`/`who`
/// renderers cannot be made to respect it without an engine change, so a hidden
/// player is still listed there (documented in `docs/runbook.md` §8).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Component)]
pub struct TbaHidden;

/// A player moving silently (Circle's `AFF_SNEAK`).
///
/// Set by the `sneak` skill ([`crate::skills`]); runtime-only and never
/// persisted, exactly like [`TbaHidden`]. Circle uses `AFF_SNEAK` to suppress
/// the leave/arrive messages of a move (`tbamud/src/act.movement.c:277,302`);
/// MIB movement does not route through this pack, so the only effect here is
/// the aggro gate [`crate::combat::ai`] applies (a pack extension: Circle gates
/// only `CAN_SEE` on `AFF_HIDE`, not `AFF_SNEAK`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Component)]
pub struct TbaSneaking;

/// Vitals and CircleMUD combat statistics for any combatant (player or NPC).
///
/// This is deliberately *derived* state: [`crate::combat::derive_vitals`]
/// rebuilds a fresh block from a class and level, and a later equipment pass
/// adds worn-item affects on top. It is still persisted because current
/// HP/mana and position are play state, not pure derivation.
///
/// The attribute and saving-throw blocks live in the same component rather
/// than a companion: they participate in the same single-pass
/// `base + worn + buffs` rebuild as `hitroll`/`armor_class`, so keeping them
/// here avoids a second component that could desync from the vitals it is
/// derived beside.
#[derive(Debug, Clone, Component, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbaCombatant {
    pub hp: i32,
    pub max_hp: i32,
    pub mana: i32,
    pub max_mana: i32,
    pub move_points: i32,
    pub max_move_points: i32,
    /// To-hit modifier added to the d20 attack roll.
    pub hitroll: i32,
    /// Damage bonus added to the weapon dice.
    pub damroll: i32,
    /// Circle armour class: `100` (worst) down to `-100` (best).
    pub armor_class: i32,
    /// `THAC0 - (target_ac / 10)` is the d20 roll needed to hit.
    pub thaco: i32,
    /// Circle's six prime attributes, rebuilt from [`TbaAttributes::neutral`]
    /// plus worn/buff affects (never stacked). Defaults to neutral so a save
    /// written before the block existed still loads.
    #[serde(default)]
    pub attributes: TbaAttributes,
    /// Affect-applied saving-throw modifiers, rebuilt from zero plus worn/buff
    /// affects. Defaults to neutral so an older save still loads.
    #[serde(default)]
    pub saves: TbaSaves,
    /// Current position in the Circle position ladder.
    pub position: Position,
    /// Current melee target. Runtime-only: an `Entity` is not portable across
    /// a restart, so it is never persisted.
    #[serde(skip)]
    pub target: Option<Entity>,
}

impl TbaCombatant {
    /// Alive means HP-positive and not in the `Dead` position.
    pub fn is_alive(&self) -> bool {
        self.position != Position::Dead && self.hp > 0
    }

    /// The d20 threshold `roll + hitroll` must meet to hit `target_ac`
    /// (doc §4.3: `needed = THAC0 - target_ac / 10`).
    pub fn needed_to_hit(&self, target_ac: i32) -> i32 {
        self.thaco - target_ac / 10
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn alignment_defaults_to_neutral() {
        assert_eq!(TbaAlignment::default().0, 0);
    }

    #[test]
    fn needed_to_hit_uses_tenths_of_ac() {
        let mut c = TbaCombatant {
            hp: 10,
            max_hp: 10,
            mana: 0,
            max_mana: 0,
            move_points: 80,
            max_move_points: 80,
            hitroll: 0,
            damroll: 0,
            armor_class: 100,
            thaco: 15,
            attributes: TbaAttributes::neutral(),
            saves: TbaSaves::default(),
            position: Position::Standing,
            target: None,
        };
        // Better armour (negative AC) makes the roll harder.
        assert_eq!(c.needed_to_hit(100), 5);
        assert_eq!(c.needed_to_hit(-100), 25);
        assert!(c.is_alive());
        c.position = Position::Dead;
        assert!(!c.is_alive());
    }

    #[test]
    fn neutral_attributes_and_saves_carry_no_bonus() {
        let attributes = TbaAttributes::neutral();
        assert_eq!(attributes.strength, BASE_ATTRIBUTE);
        assert_eq!(attributes.to_hit_bonus(), 0, "13 is Circle's zero point");
        assert_eq!(TbaAttributes::default(), attributes);
        assert_eq!(TbaSaves::default().spell, 0);
    }

    #[test]
    fn to_hit_bonus_follows_the_str_int_wis_tables() {
        // str_app: 18 -> +1 (`constants.c:625`); 8 and 13 -> 0.
        let mut attributes = TbaAttributes {
            strength: 18,
            ..TbaAttributes::neutral()
        };
        assert_eq!(attributes.to_hit_bonus(), 1);
        attributes.strength = 8;
        assert_eq!(attributes.to_hit_bonus(), 0, "str_app[8].tohit is 0");
        // `(stat - 13) / 1.5` truncates toward zero, like the C cast.
        attributes = TbaAttributes {
            strength: 13,
            intelligence: 18,
            wisdom: 18,
            ..TbaAttributes::neutral()
        };
        assert_eq!(attributes.to_hit_bonus(), 6, "3 + 3 for two 18s");
        attributes.intelligence = 8;
        attributes.wisdom = 8;
        assert_eq!(attributes.to_hit_bonus(), -6, "-3 + -3 for two 8s");
        attributes.intelligence = 12;
        attributes.wisdom = 12;
        assert_eq!(attributes.to_hit_bonus(), 0, "-0.66 truncates to 0");
    }

    #[test]
    fn saves_carry_independent_modifiers() {
        let saves = TbaSaves {
            spell: 3,
            para: -2,
            ..TbaSaves::default()
        };
        assert_eq!(saves.spell, 3);
        assert_eq!(saves.para, -2);
        assert_eq!(saves.rod, 0);
    }
}
