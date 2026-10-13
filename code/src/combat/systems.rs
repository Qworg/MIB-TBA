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
use mib_stdlib::prelude::{Location, Npc, NpcId, NpcKey, Player, PlayerDefeated};

use crate::content::{Position, TbaItemKind};
use crate::dg::host::EcsHost;
use crate::dg::intake::combat::{self as dg_combat, is_live};
use crate::dg::with_host;
use crate::module::TbaContent;

use super::model::{TbaAttackDice, TbaCombatant, WieldedWeapon};
use super::progression::TbaRng;
use super::rewards::{NpcDefeated, TbaDamage, alignment_for, reward_for};

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
/// An exclusive system: attackers act one after another, as `perform_violence`
/// does, so a blow that kills stops its victim from striking back this round,
/// and the DG triggers (`fight`, `damage`, `death`, `hitprcnt`; see
/// [`crate::dg::intake::combat`]) run at the C's points, with the world as the
/// previous blows left it. A miss is a blow of zero damage, which the victim's
/// `damage` trigger sees.
pub fn resolve_rounds(world: &mut World) {
    let order: Vec<Entity> = {
        let mut query = world.query::<(Entity, &TbaCombatant)>();
        query
            .iter(world)
            .filter(|(_, combatant)| combatant.is_alive() && combatant.target.is_some())
            .map(|(entity, _)| entity)
            .collect()
    };
    if order.is_empty() || !world.contains_resource::<TbaRng>() {
        return;
    }
    with_host(world, |host| {
        for attacker in order {
            blow(host, attacker);
        }
    });
}

/// The target `attacker` can strike now: both alive and neither extracted.
fn live_target(world: &World, attacker: Entity) -> Option<Entity> {
    if !is_live(world, attacker) {
        return None;
    }
    let target = world.get::<TbaCombatant>(attacker)?.target?;
    is_live(world, target).then_some(target)
}

/// Whether the two characters are in different rooms (`perform_violence` and
/// `hit` both stop a fight whose sides are apart). A character with no
/// location is never apart.
fn apart(world: &World, a: Entity, b: Entity) -> bool {
    match (world.get::<Location>(a), world.get::<Location>(b)) {
        (Some(a), Some(b)) => a.room != b.room,
        _ => false,
    }
}

/// `stop_fighting(attacker)`.
fn stop_fighting(world: &mut World, attacker: Entity) {
    if let Some(mut combatant) = world.get_mut::<TbaCombatant>(attacker) {
        combatant.target = None;
    }
}

/// One attacker's blow (`hit`), from its `fight` trigger to the victim's
/// `hitprcnt` trigger.
fn blow(host: &mut EcsHost<'_>, attacker: Entity) {
    let Some(first) = live_target(host.world, attacker) else {
        return;
    };
    if apart(host.world, attacker, first) {
        stop_fighting(host.world, attacker);
        return;
    }
    dg_combat::fight(host, attacker);
    // A `fight` script may have killed, moved or purged either side, and `hit`
    // abandons the blow when they are no longer in one room.
    let Some(victim) = live_target(host.world, attacker) else {
        return;
    };
    if apart(host.world, attacker, victim) {
        stop_fighting(host.world, attacker);
        return;
    }

    let dam = swing(host.world, attacker, victim);
    let Some(dam) = dg_combat::damage(host, attacker, victim, dam) else {
        dg_combat::hitprcnt(host, victim);
        return;
    };
    // A `damage` script may have killed, moved or purged either side.
    if !is_live(host.world, victim) {
        return;
    }
    // `damage` starts the victim fighting back once the blow was not cancelled.
    start_fighting(host.world, victim, attacker);
    let lethal = dam > 0 && apply_blow(host.world, attacker, victim, dam);
    if lethal {
        defeat(host, victim, attacker);
    } else {
        dg_combat::hitprcnt(host, victim);
    }
}

/// `set_fighting(victim, attacker)` for a victim not already fighting.
fn start_fighting(world: &mut World, victim: Entity, attacker: Entity) {
    if let Some(mut combatant) = world.get_mut::<TbaCombatant>(victim)
        && combatant.is_alive()
        && combatant.target.is_none()
    {
        combatant.target = Some(attacker);
    }
}

/// Roll one swing: zero on a miss, else the weapon dice plus damroll (at least
/// one).
fn swing(world: &mut World, attacker: Entity, victim: Entity) -> i32 {
    let (Some(att), Some(def)) = (
        world.get::<TbaCombatant>(attacker).cloned(),
        world.get::<TbaCombatant>(victim).cloned(),
    ) else {
        return 0;
    };
    let dice = {
        let content = world.get_resource::<TbaContent>();
        combat_dice(
            world.get::<WieldedWeapon>(attacker),
            content,
            world.get::<TbaAttackDice>(attacker),
        )
    };
    let Some(mut rng) = world.get_resource_mut::<TbaRng>() else {
        return 0;
    };
    let roll = rng.d20() + att.hitroll + att.attributes.to_hit_bonus();
    if roll < att.needed_to_hit(def.armor_class) {
        return 0;
    }
    (rng.roll(&dice) + att.damroll).max(1)
}

/// Take `damage` hit points from the victim and report the blow. Returns
/// whether it was lethal.
fn apply_blow(world: &mut World, attacker: Entity, victim: Entity, damage: i32) -> bool {
    let Some(mut combatant) = world.get_mut::<TbaCombatant>(victim) else {
        return false;
    };
    if !combatant.is_alive() {
        return false;
    }
    combatant.hp = (combatant.hp - damage).max(0);
    let lethal = combatant.hp == 0;
    if lethal {
        combatant.position = Position::Dead;
        combatant.target = None;
    }
    if let Some(mut events) = world.get_resource_mut::<Messages<TbaDamage>>() {
        events.write(TbaDamage {
            attacker,
            victim,
            damage,
            lethal,
        });
    }
    lethal
}

/// A character `killer` has just reduced to zero hit points: its `death`
/// trigger runs while it is still in the world, then a player is reported
/// defeated and an NPC is rewarded for and removed.
fn defeat(host: &mut EcsHost<'_>, victim: Entity, killer: Entity) {
    dg_combat::death(host, victim, killer);
    let world = &mut *host.world;
    if world.get_entity(victim).is_err() {
        return;
    }
    if world.get::<Player>(victim).is_some() {
        if let Some(mut defeated) = world.get_resource_mut::<Messages<PlayerDefeated>>() {
            defeated.write(PlayerDefeated { player: victim });
        }
    } else if world.get::<Npc>(victim).is_some() {
        // The reward data is read here, before the despawn applies, so the
        // consumer never races the dropped entity.
        let key = world.get::<NpcKey>(victim).map(|key| NpcKey(key.0.clone()));
        let content = world.get_resource::<TbaContent>();
        let (experience, gold) = key
            .as_ref()
            .and_then(|key| content.map(|content| reward_for(content, key)))
            .unwrap_or((0, 0));
        let alignment = key
            .as_ref()
            .and_then(|key| content.map(|content| alignment_for(content, key)))
            .unwrap_or(0);
        let message = NpcDefeated {
            victim,
            killer,
            npc: key.map(|key| key.0).unwrap_or_else(|| NpcId::from("")),
            experience,
            gold,
            alignment,
        };
        if let Some(mut defeated) = world.get_resource_mut::<Messages<NpcDefeated>>() {
            defeated.write(message);
        }
        world.despawn(victim);
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
