//! Kill rewards and level-up (TBA-E09/E18; issue #2).
//!
//! Defeating an NPC was a no-op beyond despawning it: nothing awarded its
//! `experience`/`gold`, and no level ever advanced. This module closes the
//! loop.
//!
//! The producer side ([`resolve_rounds`](super::systems::resolve_rounds) for
//! melee, [`handle_cast_command`](crate::spells::handle_cast_command) for
//! spells) emits an [`NpcDefeated`] carrying the reward data **read before the
//! victim is despawned**, so the consumer never has to re-query a dead entity.
//! [`award_kill_rewards`] then grants the XP/gold and advances the level from
//! the class's Circle `level_exp` table. The vitals follow on the next
//! [`recompute_equipment_affects`](crate::equipment::recompute_equipment_affects),
//! which regenerates the base block from the new level.

use mib_core::prelude::*;
use mib_stdlib::prelude::{NpcKey, Player};

use crate::content::TbaClassDef;
use crate::module::TbaContent;

use super::model::{TbaAlignment, TbaGold, TbaProgress};
use super::progression::level_for_xp;

/// A defeated NPC, with the reward data the killer is owed.
///
/// Carrying the numbers on the message is deliberate: the victim is despawned
/// in the same frame, so a consumer that re-queried it would race the despawn.
#[derive(Debug, Clone, Message)]
pub struct NpcDefeated {
    /// The defeated NPC.
    pub victim: Entity,
    /// The entity that landed the killing blow (a player, for a reward).
    pub killer: Entity,
    /// XP the prototype yields (`tba.npc_stats.experience`), or 0.
    pub experience: u64,
    /// Gold the prototype yields (`tba.npc_stats.gold`), or 0.
    pub gold: u32,
    /// The victim's moral alignment (`tba.npc_stats.alignment`), or 0.
    ///
    /// Carried on the message because the killer's alignment shift (Circle's
    /// `change_alignment`) reads the victim's alignment, and the victim is
    /// despawned in the same frame.
    pub alignment: i32,
}

/// What one kill changed for a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillReward {
    pub experience: u64,
    pub gold: u32,
    pub previous_level: u32,
    pub level: u32,
}

impl KillReward {
    /// Did this kill cross a level threshold?
    pub fn leveled_up(&self) -> bool {
        self.level > self.previous_level
    }
}

/// Grant one kill's `experience`/`gold`, advancing the level from `class`'s
/// `level_exp` table.
///
/// Pure, so the table interaction is unit-testable. `class = None` (a class id
/// with no content) still awards the XP and gold but cannot compute a level, so
/// the character keeps its current one. The level never decreases here — a
/// reward cannot de-level.
pub fn grant_kill(
    progress: &mut TbaProgress,
    gold: &mut TbaGold,
    experience: u64,
    gold_reward: u32,
    class: Option<&TbaClassDef>,
) -> KillReward {
    let previous_level = progress.level;
    progress.xp = progress.xp.saturating_add(experience);
    let level = class
        .map(|class| level_for_xp(class, progress.xp))
        .unwrap_or(previous_level)
        .max(previous_level);
    progress.level = level;
    gold.0 = gold.0.saturating_add(gold_reward);
    KillReward {
        experience,
        gold: gold_reward,
        previous_level,
        level,
    }
}

/// The `(experience, gold)` an NPC prototype yields, or `(0, 0)` when it has no
/// `tba.npc_stats` entry.
pub fn reward_for(content: &TbaContent, npc: &NpcKey) -> (u64, u32) {
    content
        .npc_stats
        .get(&npc.0)
        .map(|stats| (u64::from(stats.experience), stats.gold))
        .unwrap_or((0, 0))
}

/// The moral alignment an NPC prototype carries, or `0` (neutral) when it has
/// no `tba.npc_stats` entry.
pub fn alignment_for(content: &TbaContent, npc: &NpcKey) -> i32 {
    content
        .npc_stats
        .get(&npc.0)
        .map(|stats| stats.alignment)
        .unwrap_or(0)
}

/// Circle's alignment shift on a kill (`tbamud/src/fight.c:264`,
/// `change_alignment`):
///
/// ```text
/// GET_ALIGNMENT(killer) += (-GET_ALIGNMENT(victim) - GET_ALIGNMENT(killer)) / 16
/// ```
///
/// i.e. the killer moves one sixteenth of the way toward the *opposite* of the
/// victim's alignment. Integer division truncates toward zero exactly as C
/// does. A neutral killer defeating a neutral victim therefore stays at `0`,
/// while a good victim (positive) drags the killer negative and an evil victim
/// (negative) drags the killer positive.
pub fn alignment_after_kill(killer: i32, victim: i32) -> i32 {
    killer + (-victim - killer) / 16
}

/// `Update`, after the round runner: award every NPC defeated this frame.
///
/// Melee kills are emitted by `resolve_rounds` on the `TbaRound` schedule, and
/// spell kills by `handle_cast_command` in `BridgeSet::Game`; both land before
/// this system, which the plugin orders after `run_due_round`.
pub fn award_kill_rewards(
    mut defeated: MessageReader<NpcDefeated>,
    mut players: Query<(&mut TbaProgress, &mut TbaGold, Option<&mut TbaAlignment>), With<Player>>,
    content: Option<Res<TbaContent>>,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);
    for event in defeated.read() {
        let Ok((mut progress, mut gold, mut alignment)) = players.get_mut(event.killer) else {
            // An NPC (or an uninitialised entity) killed it — no reward to pay.
            continue;
        };
        // Circle shifts alignment alongside the XP/gold grant
        // (`tbamud/src/fight.c:350`, `change_alignment`). A killer that has no
        // `TbaAlignment` (a bare test fixture) simply skips the shift; player
        // init always inserts one.
        if let Some(alignment) = alignment.as_deref_mut() {
            alignment.0 = alignment_after_kill(alignment.0, event.alignment);
        }
        let class = content.classes.get(&progress.class);
        let reward = grant_kill(
            &mut progress,
            &mut gold,
            event.experience,
            event.gold,
            class,
        );

        if reward.experience > 0 {
            output.write(system_message(
                event.killer,
                format!("You receive {} experience.", reward.experience),
            ));
        }
        if reward.gold > 0 {
            output.write(system_message(
                event.killer,
                format!("You receive {} gold coins.", reward.gold),
            ));
        }
        if reward.leveled_up() {
            output.write(system_message(
                event.killer,
                format!("You have reached level {}!", reward.level),
            ));
        }
    }
}

/// A `System`-channel line to one session.
fn system_message(session: Entity, body: String) -> OutputEvent {
    OutputEvent {
        session,
        payload: OutboundPayload::text(TextChannel::System, body),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use mib_core::prelude::Dice;
    use smol_str::SmolStr;

    /// A class whose table levels at 100 XP and again at 300.
    fn class() -> TbaClassDef {
        TbaClassDef {
            name: "Warrior".to_owned(),
            description: String::new(),
            hp_dice: Dice::new(1, 10, 0),
            mana_dice: None,
            xp_table: vec![0, 1, 100, 300],
            thaco_table: vec![100, 20, 20, 19],
            prime_attribute: None,
            starting_items: Vec::new(),
            granted_skills: Vec::new(),
        }
    }

    #[test]
    fn grant_kill_awards_and_levels() {
        let mut progress = TbaProgress::new(SmolStr::new("warrior"));
        let mut gold = TbaGold(5);
        let reward = grant_kill(&mut progress, &mut gold, 100, 7, Some(&class()));
        assert_eq!(reward.experience, 100);
        assert_eq!(reward.gold, 7);
        assert_eq!(progress.xp, 100);
        assert_eq!(progress.level, 2, "100 XP is the level-2 threshold");
        assert!(reward.leveled_up());
        assert_eq!(gold.0, 12);
    }

    #[test]
    fn grant_kill_never_delevels_and_accumulates() {
        let mut progress = TbaProgress::new(SmolStr::new("warrior"));
        let mut gold = TbaGold(0);
        // Two kills cross two thresholds: 100 -> 2, 200 more -> 3.
        grant_kill(&mut progress, &mut gold, 100, 0, Some(&class()));
        assert_eq!(progress.level, 2);
        let reward = grant_kill(&mut progress, &mut gold, 200, 0, Some(&class()));
        assert_eq!(progress.xp, 300);
        assert_eq!(reward.previous_level, 2);
        assert_eq!(reward.level, 3);
        assert!(reward.leveled_up());
        assert_eq!(gold.0, 0);
    }

    #[test]
    fn grant_kill_without_a_class_awards_but_cannot_level() {
        let mut progress = TbaProgress {
            class: SmolStr::new("unknown"),
            level: 1,
            xp: 0,
        };
        let mut gold = TbaGold(0);
        let reward = grant_kill(&mut progress, &mut gold, 10_000, 3, None);
        assert_eq!(progress.xp, 10_000);
        assert_eq!(progress.level, 1, "no table means no level change");
        assert!(!reward.leveled_up());
        assert_eq!(gold.0, 3);
    }

    #[test]
    fn reward_for_reads_npc_stats() {
        use crate::content::TbaNpcStats;
        use mib_stdlib::prelude::NpcId;
        let content = TbaContent {
            npc_stats: std::collections::BTreeMap::from([(
                NpcId::from("goblin"),
                TbaNpcStats {
                    hp: Dice::constant(10),
                    level: 1,
                    hitroll: 0,
                    damroll: 0,
                    armor_class: 100,
                    thaco: 20,
                    alignment: 0,
                    gold: 9,
                    experience: 42,
                    default_position: crate::content::Position::Standing,
                    behaviour: Vec::new(),
                    damage: None,
                },
            )]),
            ..TbaContent::default()
        };
        assert_eq!(
            reward_for(&content, &NpcKey(NpcId::from("goblin"))),
            (42, 9)
        );
        assert_eq!(reward_for(&content, &NpcKey(NpcId::from("absent"))), (0, 0));
    }

    #[test]
    fn alignment_for_reads_npc_stats_and_defaults_neutral() {
        use crate::content::TbaNpcStats;
        use mib_stdlib::prelude::NpcId;
        let content = TbaContent {
            npc_stats: std::collections::BTreeMap::from([(
                NpcId::from("angel"),
                TbaNpcStats {
                    hp: Dice::constant(10),
                    level: 1,
                    hitroll: 0,
                    damroll: 0,
                    armor_class: 100,
                    thaco: 20,
                    alignment: 900,
                    gold: 0,
                    experience: 0,
                    default_position: crate::content::Position::Standing,
                    behaviour: Vec::new(),
                    damage: None,
                },
            )]),
            ..TbaContent::default()
        };
        assert_eq!(alignment_for(&content, &NpcKey(NpcId::from("angel"))), 900);
        assert_eq!(alignment_for(&content, &NpcKey(NpcId::from("absent"))), 0);
    }

    #[test]
    fn alignment_after_kill_is_circles_one_sixteenth_rule() {
        // A neutral killer defeating a good victim moves negative; the source
        // truncates integer division toward zero (`-1000 / 16 == -62`).
        assert_eq!(alignment_after_kill(0, 1000), -62);
        // An evil victim drags the killer positive.
        assert_eq!(alignment_after_kill(0, -1000), 62);
        // A neutral killer defeating a neutral victim is unchanged.
        assert_eq!(alignment_after_kill(0, 0), 0);
        // Repeated good kills keep marching evil and converge to within the
        // truncation window of the target (integer division stalls once the
        // remaining distance is under 16).
        let mut killer = 0;
        let mut previous = 0;
        for _ in 0..256 {
            previous = killer;
            killer = alignment_after_kill(killer, 1000);
        }
        assert!(killer <= previous, "never turns back toward good");
        assert!(
            (-1000 - killer).abs() <= 16,
            "converges on the opposite of the victim, got {killer}"
        );
    }

    #[test]
    fn alignment_round_trips_through_serde() {
        let json = serde_json::to_string(&TbaAlignment(-437)).expect("serialize");
        let back: TbaAlignment = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, TbaAlignment(-437));
    }
}
