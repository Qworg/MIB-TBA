//! TBA-E09 — death penalties: the XP loss and the gold-on-the-corpse rule.
//!
//! [`apply_death_penalty`] runs in
//! [`BridgeSet::Game`](mib_core::prelude::BridgeSet) **before** the stdlib
//! [`handle_player_death`](mib_stdlib::prelude::handle_player_death), so the
//! standard corpse pass sees the consequences already applied:
//!
//! 1. subtract 25% of the XP earned within the current level, floored at the
//!    level threshold so a character never de-levels;
//! 2. convert the player's [`TbaGold`] into a carried `tba_gold` item whose
//!    [`Item::value`] is the amount, then zero the purse. The stdlib sweep then
//!    moves that carried item into the corpse it creates.
//!
//! The system is ordered with `.before(handle_player_death)` rather than by
//! reimplementing the corpse pass: the engine's stdlib owns corpse creation and
//! sanctuary respawn, and this module only owns the genre rules that must be in
//! place before that pass runs.

use mib_core::prelude::*;
use mib_stdlib::prelude::{CarriedBy, Item, ItemId, PlayerDefeated};

use crate::content::TbaClassDef;
use crate::module::TbaContent;

use super::init::default_class;
use super::model::{TbaGold, TbaProgress};
use super::progression::xp_threshold;

/// Prototype id of the synthetic gold item (see `content/stdlib.items.yaml`).
pub const GOLD_ITEM_ID: &str = "tba_gold";
/// Display name copied onto the dropped pile.
pub const GOLD_ITEM_NAME: &str = "a pile of gold coins";
/// Description copied onto the dropped pile.
pub const GOLD_ITEM_DESCRIPTION: &str =
    "A glittering pile of gold coins, stamped with the seal of the realm.";
/// Death costs a quarter of the XP earned since the current level began.
const XP_PENALTY_DIVISOR: u64 = 4;

/// Apply Circle's death penalty to every defeated player this frame.
///
/// A player without a [`TbaProgress`]/[`TbaGold`] pair is skipped, and an
/// unknown class falls back to [`default_class`] (the real Warrior table) so a
/// content gap cannot swallow the penalty.
pub fn apply_death_penalty(
    mut defeated: MessageReader<PlayerDefeated>,
    content: Option<Res<TbaContent>>,
    mut players: Query<(&mut TbaProgress, &mut TbaGold)>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    // Built once: the fallback for every class key the content does not know.
    let fallback = default_class();
    for event in defeated.read() {
        let Ok((mut progress, mut gold)) = players.get_mut(event.player) else {
            continue;
        };
        let class = content
            .as_deref()
            .and_then(|content| content.classes.get(&progress.class))
            .unwrap_or(&fallback);

        let lost = apply_xp_penalty(class, &mut progress);
        if lost > 0 {
            output.write(OutputEvent {
                session: event.player,
                payload: OutboundPayload::text(
                    TextChannel::System,
                    format!("You have lost {lost} experience points."),
                ),
            });
        }

        let amount = gold.0;
        if amount > 0 {
            // Zero the purse first, then spawn the pile carried by the player.
            // `handle_player_death`, which runs after this system, sweeps every
            // carried item into the corpse it creates.
            gold.0 = 0;
            commands.spawn((
                Item {
                    def_id: ItemId::from(GOLD_ITEM_ID),
                    name: GOLD_ITEM_NAME.to_owned(),
                    description: GOLD_ITEM_DESCRIPTION.to_owned(),
                    value: Some(amount),
                    tags: Vec::new(),
                },
                CarriedBy(event.player),
            ));
            output.write(OutputEvent {
                session: event.player,
                payload: OutboundPayload::text(
                    TextChannel::System,
                    format!("You have dropped {amount} gold coins."),
                ),
            });
        }
    }
}

/// Subtract 25% of the XP earned within the current level, returning how much
/// was lost.
///
/// The result is floored at the current level's threshold, so the character
/// never de-levels. A character already below its threshold (a corrupt save,
/// say) is left untouched rather than being raised to it.
fn apply_xp_penalty(class: &TbaClassDef, progress: &mut TbaProgress) -> u64 {
    let threshold = xp_threshold(class, progress.level);
    let earned = progress.xp.saturating_sub(threshold);
    let penalty = earned / XP_PENALTY_DIVISOR;
    let floor = threshold.min(progress.xp);
    let new_xp = progress.xp.saturating_sub(penalty).max(floor);
    let lost = progress.xp - new_xp;
    progress.xp = new_xp;
    lost
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use smol_str::SmolStr;

    use super::*;

    fn content() -> TbaContent {
        TbaContent {
            classes: BTreeMap::from([(SmolStr::new("warrior"), default_class())]),
            ..TbaContent::default()
        }
    }

    fn app(content: TbaContent) -> App {
        let mut app = App::new();
        app.insert_resource(content);
        app.add_message::<PlayerDefeated>();
        app.add_message::<OutputEvent>();
        app.add_systems(Update, apply_death_penalty);
        app
    }

    fn run(app: &mut App) {
        app.world_mut().run_schedule(Update);
        app.world_mut().flush();
    }

    fn defeated(app: &mut App, player: Entity) {
        app.world_mut()
            .resource_mut::<Messages<PlayerDefeated>>()
            .write(PlayerDefeated { player });
    }

    fn progress(app: &App, player: Entity) -> TbaProgress {
        app.world()
            .get::<TbaProgress>(player)
            .cloned()
            .expect("progress")
    }

    #[test]
    fn xp_penalty_floors_at_the_level_threshold() {
        let class = default_class();
        // Warrior level 2 begins at 2000 XP.
        assert_eq!(xp_threshold(&class, 2), 2000);

        // 8 XP earned this level costs 2, leaving 2006.
        let mut progress = TbaProgress {
            class: SmolStr::new("warrior"),
            level: 2,
            xp: 2008,
        };
        assert_eq!(apply_xp_penalty(&class, &mut progress), 2);
        assert_eq!(progress.xp, 2006);

        // A character exactly at the threshold keeps every point; the floor
        // guarantees the loss can never cross it.
        let mut at_threshold = TbaProgress {
            class: SmolStr::new("warrior"),
            level: 2,
            xp: 2000,
        };
        assert_eq!(apply_xp_penalty(&class, &mut at_threshold), 0);
        assert_eq!(at_threshold.xp, 2000);

        // Sweeping the first level's worth of XP never de-levels.
        for offset in 0..1000u64 {
            let mut p = TbaProgress {
                class: SmolStr::new("warrior"),
                level: 2,
                xp: 2000 + offset,
            };
            apply_xp_penalty(&class, &mut p);
            assert!(
                p.xp >= 2000,
                "XP {offset} above the threshold must stay at or above it, got {}",
                p.xp
            );
        }
    }

    #[test]
    fn xp_penalty_leaves_a_below_threshold_total_alone() {
        let class = default_class();
        let mut progress = TbaProgress {
            class: SmolStr::new("warrior"),
            level: 2,
            xp: 1200,
        };
        assert_eq!(apply_xp_penalty(&class, &mut progress), 0);
        assert_eq!(progress.xp, 1200, "the floor must not invent XP");
    }

    #[test]
    fn gold_becomes_a_carried_item_with_the_amount() {
        let mut app = app(content());
        let player = app
            .world_mut()
            .spawn((
                TbaProgress {
                    class: SmolStr::new("warrior"),
                    level: 1,
                    xp: 0,
                },
                TbaGold(37),
            ))
            .id();

        defeated(&mut app, player);
        run(&mut app);

        assert_eq!(
            app.world().get::<TbaGold>(player).unwrap().0,
            0,
            "purse emptied"
        );

        let gold: Vec<(Item, Option<CarriedBy>)> = app
            .world_mut()
            .query::<(Entity, &Item, Option<&CarriedBy>)>()
            .iter(app.world())
            .filter(|(_, item, _)| item.def_id == ItemId::from(GOLD_ITEM_ID))
            .map(|(_, item, carried)| (item.clone(), carried.copied()))
            .collect();
        assert_eq!(gold.len(), 1, "exactly one pile created");
        assert_eq!(gold[0].0.value, Some(37), "value carries the amount");
        assert_eq!(
            gold[0].1.map(|carried| carried.0),
            Some(player),
            "the pile is carried, so the stdlib corpse sweep collects it"
        );
    }

    #[test]
    fn zero_gold_creates_no_item() {
        let mut app = app(content());
        let player = app
            .world_mut()
            .spawn((
                TbaProgress {
                    class: SmolStr::new("warrior"),
                    level: 1,
                    xp: 0,
                },
                TbaGold(0),
            ))
            .id();

        defeated(&mut app, player);
        run(&mut app);

        let count = app
            .world_mut()
            .query::<&Item>()
            .iter(app.world())
            .filter(|item| item.def_id == ItemId::from(GOLD_ITEM_ID))
            .count();
        assert_eq!(count, 0, "an empty purse drops nothing");
        assert_eq!(progress(&app, player).xp, 0);
    }
}
