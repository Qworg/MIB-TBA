//! The `stdlib.quests` bridge (issue #15).
//!
//! The hybrid the design recommends splits responsibility two ways:
//!
//! - the DG interpreter owns scripted text and gate conditions;
//! - `stdlib.quests` owns objective persistence, the journal, and rewards.
//!
//! A satisfied objective reaches `stdlib.quests` as a [`QuestEvent::FlagSet`].
//! This module supplies the other half: TBA native
//! combat (melee, skills, spells) emits its own
//! [`NpcDefeated`](crate::combat::rewards::NpcDefeated) rather than
//! `classic-rpg`'s, so nothing was translating a kill into
//! [`QuestEvent::Killed`] and the zone-1 "Kill the Mice!" `Kill` step could
//! never advance.
//!
//! [`trigger_quest_bridge_system`] closes that loop. It carries the prototype
//! id on the message precisely because the victim entity is despawned in the
//! same frame the reward is read.

use mib_core::prelude::*;
use mib_stdlib::prelude::QuestEvent;

use crate::combat::rewards::NpcDefeated;

/// Translate each TBA native kill into a stdlib [`QuestEvent::Killed`].
///
/// Runs after the kill producers (`award_kill_rewards` reads the same stream),
/// so it never races the victim's despawn: the prototype id rides on the
/// [`NpcDefeated`] message.
pub fn trigger_quest_bridge_system(
    mut defeated: MessageReader<NpcDefeated>,
    mut events: MessageWriter<QuestEvent>,
) {
    for event in defeated.read() {
        if event.npc.as_ref().is_empty() {
            // An NPC with no prototype cannot match a quest step.
            continue;
        }
        events.write(QuestEvent::Killed {
            player: event.killer,
            npc: event.npc.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use mib_stdlib::prelude::NpcId;

    #[test]
    fn a_kill_becomes_a_quest_event() {
        let mut app = App::new();
        app.add_message::<NpcDefeated>();
        app.add_message::<QuestEvent>();
        app.add_systems(Update, trigger_quest_bridge_system);

        let killer = app.world_mut().spawn_empty().id();
        let victim = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<Messages<NpcDefeated>>()
            .write(NpcDefeated {
                victim,
                killer,
                npc: NpcId::from("tba_npc_194"),
                experience: 10,
                gold: 1,
                alignment: 0,
            });
        app.update();

        let events: Vec<QuestEvent> = app
            .world_mut()
            .resource_mut::<Messages<QuestEvent>>()
            .drain()
            .collect();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            QuestEvent::Killed { player, npc } if *player == killer && npc.as_ref() == "tba_npc_194"
        ));
    }

    #[test]
    fn a_kill_without_a_prototype_is_ignored() {
        let mut app = App::new();
        app.add_message::<NpcDefeated>();
        app.add_message::<QuestEvent>();
        app.add_systems(Update, trigger_quest_bridge_system);

        let killer = app.world_mut().spawn_empty().id();
        let victim = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<Messages<NpcDefeated>>()
            .write(NpcDefeated {
                victim,
                killer,
                npc: NpcId::from(""),
                experience: 10,
                gold: 1,
                alignment: 0,
            });
        app.update();

        let events: Vec<QuestEvent> = app
            .world_mut()
            .resource_mut::<Messages<QuestEvent>>()
            .drain()
            .collect();
        assert!(events.is_empty());
    }
}
