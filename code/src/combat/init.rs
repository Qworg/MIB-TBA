//! Combatant initialisation: NPC vitals from content, player progression from
//! character creation (TBA-E27 / TBA-E18).
//!
//! Both are idempotent by construction. [`npc_combatant_system`] only touches
//! NPCs `Without<TbaCombatant>`, so a reload or a respawn cannot stack a second
//! vital block; [`player_combatant_system`] consults the components already on
//! the session before inserting. The systems are deliberately separate from
//! [`resolve_rounds`](super::systems::resolve_rounds), which only ever reads
//! entities that already opted into combat.

use mib_core::prelude::*;
use mib_stdlib::prelude::{CharacterCreated, Location, NpcKey, Room};
use smol_str::SmolStr;

use crate::content::{Position, TbaClassDef, TbaNpcStats};
use crate::module::TbaContent;

use super::ai::TbaSpawnHome;
use super::model::{TbaAlignment, TbaAttributes, TbaCombatant, TbaGold, TbaProgress, TbaSaves};
use super::progression::{BASE_MOVE, NAKED_ARMOR_CLASS, TbaRng, derive_vitals};

/// An NPC's unaffected base vitals, captured when its [`TbaCombatant`] is
/// first derived.
///
/// Unlike a player's deterministic [`derive_vitals`], an NPC's HP is rolled
/// from `tba.npc_stats.hp`, so the base cannot be regenerated every frame
/// without re-rolling it. This component keeps the rolled block so
/// [`recompute_npc_equipment_affects`](crate::equipment::recompute_npc_equipment_affects)
/// can rebuild `base + worn-item affects` without stacking and without a new
/// roll. Runtime-only: NPC vitals are not persisted (only players are), so the
/// base is always recomputed on spawn.
#[derive(Debug, Clone, Component)]
pub struct TbaNpcBase(pub TbaCombatant);

/// The class used when `tba.classes` has no matching entry at all.
///
/// It carries the real Warrior profile from tbaMUD's `src/class.c` (d10 hit
/// die, `level_exp` and `thaco` rows) so a character is always playable and
/// progresses correctly even with an empty class table.
pub fn default_class() -> TbaClassDef {
    TbaClassDef {
        name: "Warrior".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 10, 0),
        mana_dice: None,
        xp_table: vec![
            0, 1, 2000, 4000, 8000, 16000, 32000, 64000, 125000, 250000, 500000, 750000, 1000000,
            1250000, 1500000, 1850000, 2200000, 2550000, 2900000, 3250000, 3600000, 3900000,
            4200000, 4500000, 4800000, 5150000, 5500000, 5950000, 6400000, 6850000, 7400000,
            8000000,
        ],
        thaco_table: vec![
            100, 20, 19, 18, 17, 16, 15, 14, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        ],
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: Vec::new(),
    }
}

/// Map an archetype/class name to a `tba.classes` key.
///
/// Matching is forgiving because the name crosses a content boundary: the
/// exact key, the lowercased key, the lowercased-with-underscores form, and
/// the display `name` are all tried. An unknown (or absent) archetype falls
/// back to `warrior`, then to the first declared class, then to the literal
/// `warrior` key when the table is empty.
pub fn resolve_class_key(content: &TbaContent, archetype: Option<&str>) -> SmolStr {
    if let Some(raw) = archetype {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let lowercase = trimmed.to_lowercase();
            let underscored = lowercase.replace([' ', '-'], "_");
            for candidate in [trimmed, lowercase.as_str(), underscored.as_str()] {
                if let Some((key, _)) = content.classes.get_key_value(candidate) {
                    return key.clone();
                }
            }
            if let Some((key, _)) = content
                .classes
                .iter()
                .find(|(_, def)| def.name.eq_ignore_ascii_case(trimmed))
            {
                return key.clone();
            }
        }
    }
    if content.classes.contains_key("warrior") {
        return SmolStr::new("warrior");
    }
    content
        .classes
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| SmolStr::new("warrior"))
}

/// Derive an NPC's combat vitals from its `tba.npc_stats` record.
///
/// HP is rolled from the record's dice through the shared [`TbaRng`] so a
/// seeded replay is reproducible; a zero/absent THAC0 means the content did
/// not carry one, in which case Circle's level-1 value (`20`) applies.
pub fn npc_combatant_from_stats(stats: &TbaNpcStats, rng: &mut TbaRng) -> TbaCombatant {
    let max_hp = rng.roll(&stats.hp).max(1);
    TbaCombatant {
        hp: max_hp,
        max_hp,
        mana: 0,
        max_mana: 0,
        move_points: BASE_MOVE,
        max_move_points: BASE_MOVE,
        hitroll: stats.hitroll,
        damroll: stats.damroll,
        armor_class: stats.armor_class,
        thaco: if stats.thaco == 0 { 20 } else { stats.thaco },
        attributes: TbaAttributes::neutral(),
        saves: TbaSaves::default(),
        position: stats.default_position,
        target: None,
    }
}

/// A benign fallback for an NPC with no `tba.npc_stats` record.
///
/// The mob is still attackable (so `attack <name>` and aggro behave), but its
/// ten hit points and naked AC make it a non-threat rather than a silent
/// invulnerable entity.
pub fn fallback_combatant() -> TbaCombatant {
    TbaCombatant {
        hp: 10,
        max_hp: 10,
        mana: 0,
        max_mana: 0,
        move_points: BASE_MOVE,
        max_move_points: BASE_MOVE,
        hitroll: 0,
        damroll: 0,
        armor_class: NAKED_ARMOR_CLASS,
        thaco: 20,
        attributes: TbaAttributes::neutral(),
        saves: TbaSaves::default(),
        position: Position::Standing,
        target: None,
    }
}

/// `FixedUpdate`: give every NPC a [`TbaCombatant`] and its AI memory.
///
/// Only NPCs `Without<TbaCombatant>` are touched, which is what makes a
/// reload/respawn safe: an NPC that already has vitals keeps them (current HP
/// is play state), and a freshly spawned one gets a rolling from its stats.
/// `TbaAttackerMemory` is inserted for every NPC so a later `attack` can
/// record its attacker without a second system.
pub fn npc_combatant_system(
    npcs: Query<(Entity, &NpcKey, Option<&Location>), Without<TbaCombatant>>,
    rooms: Query<&Room>,
    content: Option<Res<TbaContent>>,
    mut rng: ResMut<TbaRng>,
    mut commands: Commands,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    for (entity, key, location) in &npcs {
        let combatant = match content.npc_stats.get(&key.0) {
            Some(stats) => npc_combatant_from_stats(stats, &mut rng),
            None => fallback_combatant(),
        };
        let home = location.map(|location| TbaSpawnHome {
            room: location.room,
            zone: rooms.get(location.room).ok().and_then(|room| {
                content
                    .zones
                    .values()
                    .find(|zone| room.num >= zone.bot && room.num <= zone.top)
                    .map(|zone| (zone.bot, zone.top))
            }),
        });

        let mut entity_commands = commands.entity(entity);
        entity_commands.insert((
            combatant.clone(),
            TbaNpcBase(combatant),
            super::ai::TbaAttackerMemory::default(),
        ));
        if let Some(home) = home {
            entity_commands.insert(home);
        }
    }
}

/// `BridgeSet::Game` (after the stdlib router): turn `CharacterCreated` into
/// `TbaProgress` + `TbaCombatant` for the chosen class.
///
/// Idempotent: an already-initialised session is left alone, so a duplicate
/// event (or a restored character) never resets current HP. A missing
/// archetype or an unknown class resolves through [`resolve_class_key`] and
/// [`default_class`].
#[allow(clippy::type_complexity)] // one ECS view per optional player component.
pub fn player_combatant_system(
    mut created: MessageReader<CharacterCreated>,
    content: Option<Res<TbaContent>>,
    existing: Query<(
        Option<&TbaProgress>,
        Option<&TbaCombatant>,
        Option<&TbaAlignment>,
        Option<&TbaGold>,
    )>,
    mut commands: Commands,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    for event in created.read() {
        let Ok((progress, combatant, alignment, gold)) = existing.get(event.session) else {
            continue;
        };
        let needs_combatant = combatant.is_none();
        if progress.is_some() && !needs_combatant && alignment.is_some() && gold.is_some() {
            continue;
        }

        let key = progress
            .map(|progress| progress.class.clone())
            .unwrap_or_else(|| resolve_class_key(content, event.archetype.as_deref()));
        let class = content
            .classes
            .get(&key)
            .cloned()
            .unwrap_or_else(default_class);
        let level = progress.map_or(1, |progress| progress.level);

        let mut entity_commands = commands.entity(event.session);
        if progress.is_none() {
            entity_commands.insert(TbaProgress::new(key));
        }
        if needs_combatant {
            entity_commands.insert(derive_vitals(&class, level));
        }
        if alignment.is_none() {
            entity_commands.insert(TbaAlignment(0));
        }
        if gold.is_none() {
            entity_commands.insert(TbaGold(0));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use super::*;
    use crate::content::Attribute;

    fn class(name: &str, hp: Dice) -> TbaClassDef {
        let mut def = default_class();
        def.name = name.to_owned();
        def.hp_dice = hp;
        def.prime_attribute = None::<Attribute>;
        def
    }

    fn content() -> TbaContent {
        let mut classes = BTreeMap::new();
        classes.insert(
            SmolStr::new("magic_user"),
            class("Magic User", Dice::new(1, 4, 0)),
        );
        classes.insert(
            SmolStr::new("warrior"),
            class("Warrior", Dice::new(1, 10, 0)),
        );
        TbaContent {
            classes,
            ..TbaContent::default()
        }
    }

    #[test]
    fn class_resolution_prefers_exact_then_fuzzy_then_warrior() {
        let content = content();
        assert_eq!(
            resolve_class_key(&content, Some("magic_user")),
            SmolStr::new("magic_user")
        );
        assert_eq!(
            resolve_class_key(&content, Some("Magic User")),
            SmolStr::new("magic_user")
        );
        assert_eq!(
            resolve_class_key(&content, Some("magic-user")),
            SmolStr::new("magic_user")
        );
        assert_eq!(
            resolve_class_key(&content, Some("assassin")),
            SmolStr::new("warrior")
        );
        assert_eq!(resolve_class_key(&content, None), SmolStr::new("warrior"));
    }

    #[test]
    fn class_resolution_falls_back_to_first_class_when_no_warrior() {
        let mut classes = BTreeMap::new();
        classes.insert(SmolStr::new("cleric"), class("Cleric", Dice::new(1, 8, 0)));
        let content = TbaContent {
            classes,
            ..TbaContent::default()
        };
        assert_eq!(resolve_class_key(&content, None), SmolStr::new("cleric"));
    }

    #[test]
    fn npc_vitals_use_the_stat_block() {
        let stats = TbaNpcStats {
            hp: Dice::new(2, 8, 2),
            level: 3,
            hitroll: 3,
            damroll: 4,
            armor_class: 20,
            thaco: 18,
            alignment: -100,
            gold: 0,
            experience: 0,
            default_position: Position::Standing,
            behaviour: vec![crate::content::NpcBehaviour::Aggressive],
        };
        let mut rng = TbaRng::from_seed(1);
        let combatant = npc_combatant_from_stats(&stats, &mut rng);
        assert_eq!(combatant.hitroll, 3);
        assert_eq!(combatant.damroll, 4);
        assert_eq!(combatant.armor_class, 20);
        assert_eq!(combatant.thaco, 18);
        assert!(combatant.max_hp >= 4 && combatant.max_hp <= 18);
        assert_eq!(combatant.hp, combatant.max_hp);
    }

    #[test]
    fn npc_vitals_default_thaco_when_absent() {
        let stats = TbaNpcStats {
            hp: Dice::new(1, 1, 0),
            level: 1,
            hitroll: 0,
            damroll: 0,
            armor_class: 100,
            thaco: 0,
            alignment: 0,
            gold: 0,
            experience: 0,
            default_position: Position::Standing,
            behaviour: Vec::new(),
        };
        let mut rng = TbaRng::from_seed(1);
        let combatant = npc_combatant_from_stats(&stats, &mut rng);
        assert_eq!(combatant.thaco, 20);
        assert_eq!(combatant.max_hp, 1);
    }

    #[test]
    fn fallback_is_benign_but_alive() {
        let combatant = fallback_combatant();
        assert!(combatant.is_alive());
        assert_eq!(combatant.armor_class, NAKED_ARMOR_CLASS);
    }
}
