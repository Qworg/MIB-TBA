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
/// This is the **narrow damage seam** for the deferred 15-slot equipment pass
/// (TBA-E28): when the future `TbaEquipment` system sets this component,
/// [`resolve_rounds`](crate::combat::resolve_rounds) reads the item's
/// `tba.items` weapon dice instead of the unarmed default. Nothing in this
/// phase inserts it, so combat degrades to unarmed.
#[derive(Debug, Clone, Component)]
pub struct WieldedWeapon(pub ItemId);

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

/// Vitals and CircleMUD combat statistics for any combatant (player or NPC).
///
/// This is deliberately *derived* state: [`crate::combat::derive_vitals`]
/// rebuilds a fresh block from a class and level, and a later equipment pass
/// adds worn-item affects on top. It is still persisted because current
/// HP/mana and position are play state, not pure derivation.
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
}
