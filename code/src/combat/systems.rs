//! CircleMUD combat round resolution (TBA-E31).
//!
//! Runs inside the [`TbaRound`](super::round::TbaRound) schedule, so it sees
//! the state as of the last round and emits the next. The rules are the doc
//! §4.3 formulas:
//!
//! ```text
//! roll    = d20 + hitroll + attributes.to_hit_bonus()
//! needed  = THAC0 - (target_ac / 10)
//! hit     = roll >= needed
//! damage  = weapon_dice() + damroll
//! ```
//!
//! The attribute term is Circle's `compute_thaco` (`tbamud/src/fight.c:830-833`),
//! which subtracts `str_app[].tohit` plus `(int - 13) / 1.5` and
//! `(wis - 13) / 1.5` from THAC0; adding the same term to the roll is the
//! identical comparison.
//!
//! A defeated player emits [`PlayerDefeated`] (the stdlib corpse + sanctuary
//! respawn path, TBA-E09); a defeated NPC is despawned, which the spawner pass
//! observes to repopulate the world.

use mib_core::prelude::*;
use mib_stdlib::prelude::{Npc, NpcId, NpcKey, Player, PlayerDefeated};

use crate::content::{Position, TbaItemKind};
use crate::module::TbaContent;

use super::model::{TbaAttackDice, TbaCombatant, WieldedWeapon};
use super::progression::TbaRng;
use super::rewards::{NpcDefeated, alignment_for, reward_for};

/// Unarmed damage. An equipped weapon's `tba.items` dice replace this through
/// [`weapon_dice`] once the equipment pass sets [`WieldedWeapon`].
const UNARMED: Dice = Dice::new(1, 4, 0);

/// The damage dice for one swing: the wielded weapon's `tba.items` values
/// `[count, sides, bonus]` when it is a real weapon prototype, else unarmed.
///
/// This is the weapon half of the damage seam: [`combat_dice`] layers an NPC's
/// natural attack dice underneath it. A missing [`WieldedWeapon`], an unknown
/// item, a non-weapon kind, or a malformed dice triple all degrade to `1d4`
/// rather than failing the round.
pub fn weapon_dice(wielded: Option<&WieldedWeapon>, content: Option<&TbaContent>) -> Dice {
    let Some(WieldedWeapon(item_id)) = wielded else {
        return UNARMED;
    };
    let Some(def) = content.and_then(|content| content.items.get(item_id)) else {
        return UNARMED;
    };
    if def.kind != TbaItemKind::Weapon {
        return UNARMED;
    }
    // `tba.items.values` is the source's four type-dependent fields verbatim
    // (Circle `value[0..3]`). For a weapon those are
    // `[unused, dice_count, dice_sides, damage_bonus]`.
    let count = def.values[1];
    let sides = def.values[2];
    let bonus = def.values[3];
    if count <= 0 || sides <= 0 {
        return UNARMED;
    }
    Dice::new(count as u32, sides as u32, bonus)
}

/// The damage dice for one swing, in priority order: a wielded weapon, then an
/// NPC's natural attack dice (`tba.npc_stats.damage`), then unarmed.
///
/// This is the whole damage seam for a round. [`weapon_dice`] resolves the
/// weapon half; an NPC with no weapon falls through to [`TbaAttackDice`].
pub fn combat_dice(
    wielded: Option<&WieldedWeapon>,
    content: Option<&TbaContent>,
    npc_attack: Option<&TbaAttackDice>,
) -> Dice {
    let weapon = weapon_dice(wielded, content);
    if weapon != UNARMED {
        return weapon;
    }
    npc_attack.map_or(UNARMED, |attack| attack.0)
}

/// Resolve one melee round for every living combatant with a target.
///
/// Two phases over one [`ParamSet`]: read every attacker/target pair and roll
/// the swing, then apply the queued damage. Splitting the phases avoids
/// aliasing `TbaCombatant` while a target is also an attacker.
#[allow(clippy::type_complexity, clippy::too_many_arguments)] // ECS system params.
pub fn resolve_rounds(
    mut rng: ResMut<TbaRng>,
    content: Option<Res<TbaContent>>,
    mut combatants: ParamSet<(
        Query<(
            Entity,
            &TbaCombatant,
            Option<&WieldedWeapon>,
            Option<&TbaAttackDice>,
        )>,
        Query<&mut TbaCombatant>,
    )>,
    players: Query<(), With<Player>>,
    npcs: Query<(), With<Npc>>,
    keys: Query<&NpcKey>,
    mut defeated: MessageWriter<PlayerDefeated>,
    mut npc_defeated: MessageWriter<NpcDefeated>,
    mut commands: Commands,
) {
    let content = content.as_deref();
    let mut damage_queue: Vec<(Entity, Entity, i32)> = Vec::new();
    {
        let read = combatants.p0();
        let intents: Vec<(Entity, Entity, Dice)> = read
            .iter()
            .filter(|(_, combatant, _, _)| combatant.is_alive())
            .filter_map(|(entity, combatant, wielded, npc_attack)| {
                combatant
                    .target
                    .map(|target| (entity, target, combat_dice(wielded, content, npc_attack)))
            })
            .collect();

        for (attacker, target, weapon) in intents {
            let (Ok((_, att, _, _)), Ok((_, def, _, _))) = (read.get(attacker), read.get(target))
            else {
                continue;
            };
            if !att.is_alive() || !def.is_alive() {
                continue;
            }
            let roll = rng.d20() + att.hitroll + att.attributes.to_hit_bonus();
            if roll < att.needed_to_hit(def.armor_class) {
                continue;
            }
            let damage = (rng.roll(&weapon) + att.damroll).max(1);
            damage_queue.push((attacker, target, damage));
        }
    }

    let mut defeated_entities: Vec<(Entity, Entity)> = Vec::new();
    {
        let mut write = combatants.p1();
        for (attacker, target, damage) in damage_queue {
            let Ok(mut combatant) = write.get_mut(target) else {
                continue;
            };
            if !combatant.is_alive() {
                continue;
            }
            combatant.hp = (combatant.hp - damage).max(0);
            if combatant.hp == 0 {
                combatant.position = Position::Dead;
                combatant.target = None;
                defeated_entities.push((target, attacker));
            }
        }
    }

    for (victim, killer) in defeated_entities {
        if players.get(victim).is_ok() {
            defeated.write(PlayerDefeated { player: victim });
        } else if npcs.get(victim).is_ok() {
            // The reward data is read here, before the despawn applies, so the
            // consumer never races the dropped entity.
            let key = keys.get(victim).ok();
            let (experience, gold) = key
                .and_then(|key| content.map(|content| reward_for(content, key)))
                .unwrap_or((0, 0));
            let alignment = key
                .and_then(|key| content.map(|content| alignment_for(content, key)))
                .unwrap_or(0);
            npc_defeated.write(NpcDefeated {
                victim,
                killer,
                npc: key
                    .map(|key| key.0.clone())
                    .unwrap_or_else(|| NpcId::from("")),
                experience,
                gold,
                alignment,
            });
            commands.entity(victim).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use mib_stdlib::prelude::ItemId;

    use super::*;
    use crate::content::TbaItemDef;

    fn item(kind: TbaItemKind, values: [i32; 4]) -> TbaItemDef {
        TbaItemDef {
            kind,
            slots: Vec::new(),
            values,
            affects: Vec::new(),
            keywords: Vec::new(),
            extra_flags: Vec::new(),
            affect_flags: Vec::new(),
            weight: 0,
            cost: 0,
            rent: 0,
            min_level: 0,
            timer: None,
        }
    }

    #[test]
    fn weapon_dice_degrades_to_unarmed() {
        assert_eq!(weapon_dice(None, None), UNARMED);

        let id = ItemId::from("sword");
        let mut items = BTreeMap::new();
        items.insert(id.clone(), item(TbaItemKind::Weapon, [0, 2, 6, 3]));
        let content = TbaContent {
            items,
            ..TbaContent::default()
        };
        assert_eq!(
            weapon_dice(Some(&WieldedWeapon(id.clone())), Some(&content)),
            Dice::new(2, 6, 3)
        );

        // Unknown prototype and non-weapon kind both fall back.
        assert_eq!(
            weapon_dice(
                Some(&WieldedWeapon(ItemId::from("missing"))),
                Some(&content)
            ),
            UNARMED
        );
        let mut items = BTreeMap::new();
        items.insert(id.clone(), item(TbaItemKind::Armor, [1, 8, 0, 0]));
        let armor = TbaContent {
            items,
            ..TbaContent::default()
        };
        assert_eq!(weapon_dice(Some(&WieldedWeapon(id)), Some(&armor)), UNARMED);
    }

    #[test]
    fn weapon_dice_rejects_malformed_values() {
        let id = ItemId::from("broken");
        let mut items = BTreeMap::new();
        items.insert(id.clone(), item(TbaItemKind::Weapon, [0, 0, 6, 1]));
        let content = TbaContent {
            items,
            ..TbaContent::default()
        };
        assert_eq!(
            weapon_dice(Some(&WieldedWeapon(id)), Some(&content)),
            UNARMED
        );
    }

    #[test]
    fn combat_dice_prefers_weapon_then_npc_attack_then_unarmed() {
        // Unarmed when nothing is set.
        assert_eq!(combat_dice(None, None, None), UNARMED);

        // A mob's natural dice replace unarmed.
        let bite = TbaAttackDice(Dice::new(2, 8, 4));
        assert_eq!(combat_dice(None, None, Some(&bite)), Dice::new(2, 8, 4));

        // A wielded weapon wins over the natural attack.
        let id = ItemId::from("sword");
        let mut items = BTreeMap::new();
        items.insert(id.clone(), item(TbaItemKind::Weapon, [0, 3, 5, 1]));
        let content = TbaContent {
            items,
            ..TbaContent::default()
        };
        assert_eq!(
            combat_dice(
                Some(&WieldedWeapon(id.clone())),
                Some(&content),
                Some(&bite)
            ),
            Dice::new(3, 5, 1)
        );

        // A malformed weapon falls through to the natural attack.
        let broken = ItemId::from("broken");
        let mut items = BTreeMap::new();
        items.insert(broken.clone(), item(TbaItemKind::Weapon, [0, 0, 6, 1]));
        let content = TbaContent {
            items,
            ..TbaContent::default()
        };
        assert_eq!(
            combat_dice(Some(&WieldedWeapon(broken)), Some(&content), Some(&bite)),
            Dice::new(2, 8, 4)
        );
    }
}
