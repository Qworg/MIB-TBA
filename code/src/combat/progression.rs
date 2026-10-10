//! Progression: the Circle XP/THAC0 tables, combat-vital derivation, and the
//! TBA RNG.
//!
//! A TBA character's stats are a function of class + level ([`derive_vitals`]),
//! and the XP curve is the class's exact `level_exp` table
//! ([`TbaClassDef::xp_table`]). Nothing here touches the ECS, so the tables and
//! the derivation are unit-testable in isolation.

use mib_core::prelude::*;

use crate::content::{Position, TbaClassDef};

/// `table[level]`, clamped to the table's last entry when `level` runs past it
/// (so an implementor keeps the implementor row's value). `None` for an empty
/// table — a class with no content data.
fn table_entry<T>(table: &[T], level: u32) -> Option<&T> {
    if table.is_empty() {
        return None;
    }
    let index = (level as usize).min(table.len() - 1);
    table.get(index)
}

use super::model::{TbaAttributes, TbaCombatant, TbaSaves};

/// Move points a level-1 character starts with (Circle base movement).
pub const BASE_MOVE: i32 = 80;
/// Movement gained per level beyond the first.
pub const MOVE_PER_LEVEL: i32 = 8;
/// Naked Circle armour class — the worst value on the `100..=-100` scale.
pub const NAKED_ARMOR_CLASS: i32 = 100;

/// Total XP required to reach `level` for `class`, straight from Circle's
/// `level_exp` table ([`TbaClassDef::xp_table`]).
///
/// Index 0 is the source's level-0 sentinel (`0`) and index `L` is `level_exp`
/// for level `L` (the source returns `1` for level 1). A level beyond the table
/// clamps to its last entry. A class without a table (no content loaded)
/// reports `0`, which leaves every character at level 1.
pub fn xp_threshold(class: &TbaClassDef, level: u32) -> u64 {
    table_entry(&class.xp_table, level).copied().unwrap_or(0)
}

/// XP still required to advance from `level` to `level + 1`, from the class's
/// table. Reaching the table's final level leaves nothing to advance to, so
/// the result is `0`.
pub fn xp_to_advance(class: &TbaClassDef, level: u32) -> u64 {
    xp_threshold(class, level.saturating_add(1)).saturating_sub(xp_threshold(class, level))
}

/// The level a character with `xp` total experience has reached, capped at the
/// class table's final (immortal) level. A class without a table is level 1.
pub fn level_for_xp(class: &TbaClassDef, xp: u64) -> u32 {
    let max_level = u32::try_from(class.xp_table.len().saturating_sub(1))
        .unwrap_or(u32::MAX)
        .max(1);
    let mut level = 1;
    while level < max_level && xp_threshold(class, level + 1) <= xp {
        level += 1;
    }
    level
}

/// Derive a fresh combat-vital block from a class definition and level.
///
/// HP/mana use each die's average so a restored character is deterministic;
/// per-level rolls are a later refinement once creation is wired. Worn-item
/// [`AffectDef`](crate::content::AffectDef)s are applied on top by the
/// equipment pass.
pub fn derive_vitals(class: &TbaClassDef, level: u32) -> TbaCombatant {
    let levels = f32::from(u16::try_from(level.max(1)).unwrap_or(u16::MAX));
    let max_hp = (class.hp_dice.average() * levels).round() as i32;
    let max_hp = max_hp.max(1);
    let max_mana = class
        .mana_dice
        .map(|dice| (dice.average() * levels).round() as i32)
        .unwrap_or(0)
        .max(0);
    let levels_i = level.max(1) as i32;
    let move_points = BASE_MOVE + MOVE_PER_LEVEL * (levels_i - 1);
    // Circle's per-class THAC0 chart, clamped past the final row. `20` is the
    // level-1 value and the playable fallback when the class carries no table.
    let thaco = table_entry(&class.thaco_table, level.max(1))
        .copied()
        .unwrap_or(20);
    TbaCombatant {
        hp: max_hp,
        max_hp,
        mana: max_mana,
        max_mana,
        move_points,
        max_move_points: move_points,
        hitroll: 0,
        damroll: 0,
        armor_class: NAKED_ARMOR_CLASS,
        thaco,
        attributes: TbaAttributes::neutral(),
        saves: TbaSaves::default(),
        position: Position::Standing,
        target: None,
    }
}

/// Deterministic xorshift RNG for TBA combat rolls.
///
/// A resource rather than a global so a test (or a replay) can seed it. The
/// engine's [`Dice`] rolls are routed through it, so every damage roll is
/// reproducible from the seed.
#[derive(Debug, Clone, Resource)]
pub struct TbaRng {
    state: u64,
}

impl Default for TbaRng {
    fn default() -> Self {
        // splitmix64's golden-ratio constant; non-zero, so xorshift never
        // collapses to the all-zero fixed point.
        Self {
            state: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

impl TbaRng {
    /// A seeded RNG. A zero seed is nudged to a non-zero state.
    pub fn from_seed(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    /// The next raw xorshift64 value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// An inclusive integer in `[low, high]`.
    pub fn range(&mut self, low: i32, high: i32) -> i32 {
        if high <= low {
            return low;
        }
        let span = u64::from((high - low).unsigned_abs()) + 1;
        low + (self.next_u64() % span) as i32
    }

    /// A d20 attack roll.
    pub fn d20(&mut self) -> i32 {
        self.range(1, 20)
    }

    /// Roll a dice expression through this RNG.
    pub fn roll(&mut self, dice: &Dice) -> i32 {
        let mut x = self.state;
        let value = dice.roll_with(|| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        });
        self.state = x;
        value
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::content::TbaClassDef;
    use mib_core::prelude::Dice;

    /// Circle's `thaco(CLASS_WARRIOR, level)` chart, levels 0..=34.
    const WARRIOR_THACO: [i32; 35] = [
        100, 20, 19, 18, 17, 16, 15, 14, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1,
    ];
    /// Circle's `level_exp(CLASS_WARRIOR, level)`, levels 0..=31.
    const WARRIOR_XP: [u64; 32] = [
        0, 1, 2000, 4000, 8000, 16000, 32000, 64000, 125000, 250000, 500000, 750000, 1000000,
        1250000, 1500000, 1850000, 2200000, 2550000, 2900000, 3250000, 3600000, 3900000, 4200000,
        4500000, 4800000, 5150000, 5500000, 5950000, 6400000, 6850000, 7400000, 8000000,
    ];
    /// Circle's `thaco(CLASS_MAGIC_USER, level)`, levels 0..=34.
    const MAGE_THACO: [i32; 35] = [
        100, 20, 20, 20, 19, 19, 19, 18, 18, 18, 17, 17, 17, 16, 16, 16, 15, 15, 15, 14, 14, 14,
        13, 13, 13, 12, 12, 12, 11, 11, 11, 10, 10, 10, 9,
    ];
    /// Circle's `level_exp(CLASS_MAGIC_USER, level)`, levels 0..=31.
    const MAGE_XP: [u64; 32] = [
        0, 1, 2500, 5000, 10000, 20000, 40000, 60000, 90000, 135000, 250000, 375000, 750000,
        1125000, 1500000, 1875000, 2250000, 2625000, 3000000, 3375000, 3750000, 4000000, 4300000,
        4600000, 4900000, 5200000, 5500000, 5950000, 6400000, 6850000, 7400000, 8000000,
    ];

    fn warrior() -> TbaClassDef {
        TbaClassDef {
            name: "Warrior".into(),
            description: String::new(),
            hp_dice: Dice::new(1, 10, 0),
            mana_dice: None,
            xp_table: WARRIOR_XP.to_vec(),
            thaco_table: WARRIOR_THACO.to_vec(),
            prime_attribute: None,
            starting_items: Vec::new(),
            granted_skills: Vec::new(),
        }
    }

    fn magic_user() -> TbaClassDef {
        TbaClassDef {
            name: "Magic User".into(),
            description: String::new(),
            hp_dice: Dice::new(1, 4, 0),
            mana_dice: Some(Dice::new(1, 4, 0)),
            xp_table: MAGE_XP.to_vec(),
            thaco_table: MAGE_THACO.to_vec(),
            prime_attribute: None,
            starting_items: Vec::new(),
            granted_skills: Vec::new(),
        }
    }

    #[test]
    fn xp_thresholds_come_from_the_circle_table() {
        let warrior = warrior();
        // `level_exp(class, 1)` is 1 in the source (only the level-0 sentinel
        // is 0); a fresh character is simply assigned level 1.
        assert_eq!(xp_threshold(&warrior, 0), 0);
        assert_eq!(xp_threshold(&warrior, 1), 1);
        assert_eq!(xp_threshold(&warrior, 2), 2000);
        assert_eq!(xp_threshold(&warrior, 3), 4000);
        assert_eq!(xp_to_advance(&warrior, 1), 1999);
        assert_eq!(xp_to_advance(&warrior, 2), 2000);
        assert_eq!(level_for_xp(&warrior, 0), 1);
        assert_eq!(level_for_xp(&warrior, 1999), 1);
        assert_eq!(level_for_xp(&warrior, 2000), 2);
        assert_eq!(level_for_xp(&warrior, 3999), 2);
        assert_eq!(level_for_xp(&warrior, 4000), 3);
        // The magic-user's real `level_exp` level-2 threshold is 2500.
        assert_eq!(xp_threshold(&magic_user(), 2), 2500);
        assert_eq!(xp_threshold(&magic_user(), 3), 5000);
        // Past the final row both tables clamp to it.
        assert_eq!(xp_threshold(&warrior, 100), 8000000);
        assert_eq!(level_for_xp(&warrior, u64::MAX), 31);
    }

    #[test]
    fn thaco_comes_from_the_circle_table_and_clamps() {
        let mage = magic_user();
        assert_eq!(derive_vitals(&mage, 1).thaco, 20);
        assert_eq!(derive_vitals(&mage, 4).thaco, 19);
        let warrior = warrior();
        assert_eq!(derive_vitals(&warrior, 1).thaco, 20);
        assert_eq!(derive_vitals(&warrior, 5).thaco, 16);
        // Level 100 clamps to the last (implementor) row, THAC0 1.
        assert_eq!(derive_vitals(&warrior, 100).thaco, 1);
    }

    #[test]
    fn vitals_scale_with_level_and_class() {
        let level1 = derive_vitals(&warrior(), 1);
        // 1d10 average is 5.5 → 6 HP at level 1.
        assert_eq!(level1.max_hp, 6);
        assert_eq!(level1.thaco, 20);
        assert_eq!(level1.armor_class, NAKED_ARMOR_CLASS);
        assert_eq!(level1.hp, level1.max_hp);

        let level5 = derive_vitals(&warrior(), 5);
        assert_eq!(level5.max_hp, 28); // round(5.5 * 5)
        assert_eq!(level5.thaco, 16); // Circle's level-5 warrior row
        assert_eq!(level5.max_move_points, BASE_MOVE + 4 * MOVE_PER_LEVEL);
        assert_eq!(level5.max_mana, 0, "warriors do not cast");

        // A class with no content table still gets Circle's level-1 THAC0.
        let empty = TbaClassDef {
            name: "Empty".into(),
            description: String::new(),
            hp_dice: Dice::new(1, 4, 0),
            mana_dice: None,
            xp_table: Vec::new(),
            thaco_table: Vec::new(),
            prime_attribute: None,
            starting_items: Vec::new(),
            granted_skills: Vec::new(),
        };
        assert_eq!(derive_vitals(&empty, 3).thaco, 20);
        assert_eq!(level_for_xp(&empty, 10_000_000), 1);
    }

    #[test]
    fn seeded_rng_is_deterministic_and_in_range() {
        let mut a = TbaRng::from_seed(42);
        let mut b = TbaRng::from_seed(42);
        for _ in 0..100 {
            let x = a.d20();
            assert_eq!(x, b.d20());
            assert!((1..=20).contains(&x));
        }
        let mut c = TbaRng::from_seed(7);
        let d = Dice::new(2, 6, 3);
        for _ in 0..50 {
            let value = c.roll(&d);
            assert!(
                (d.min()..=d.max()).contains(&value),
                "{value} within the 2d6+3 range"
            );
        }
    }
}
