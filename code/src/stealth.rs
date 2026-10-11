//! Pack-side stealth: drive the engine's observer-aware perception seam.
//!
//! MIB owns the projection filter (`mib_stdlib::perception`): the engine
//! `Concealment` carrier on the observed and `Perception` on the observer, with
//! a pure `perceive` predicate and a [`VisibilityPolicy`](mib_stdlib::prelude::VisibilityPolicy)
//! resource. `mib-stdlib` never names a pack type; a pack with a richer private
//! model keeps it and syncs the relevant fields into the carriers from its own
//! system. That is what this module is for.
//!
//! TBA's private gameplay state is [`TbaHidden`]/[`TbaSneaking`] (set by
//! `hide`/`sneak`). [`sync_concealment`] mirrors them into
//! [`Concealment`] while either is set and removes the carrier when neither is,
//! so `look`/`Scene`/GMCP and the pack's own aggro/targeting all read one
//! definition of "concealed".
//!
//! True sight is the observer half: a `detect invisibility`/`sense life` cast
//! grants [`TbaTrueSight`], which [`sync_perception`] mirrors into the engine
//! [`Perception`] for the spell's duration. A small per-round timer
//! ([`tick_true_sight`]) is used rather than the `TbaBuffs` affect list because
//! `TbaBuffs` models *stat* affects ([`AffectDef`](crate::content::AffectDef))
//! that feed the additive equipment recompute, while true sight is a
//! categorical sense the engine consumes directly; keeping it separate means the
//! recompute never sees a pseudo-affect.
//!
//! [`Concealment`]: mib_stdlib::prelude::Concealment
//! [`Perception`]: mib_stdlib::prelude::Perception
//! [`TbaHidden`]: crate::combat::TbaHidden
//! [`TbaSneaking`]: crate::combat::TbaSneaking

use mib_core::prelude::*;
use mib_stdlib::prelude::{Concealment, Perception, Player, SEE_INVIS};

use crate::combat::{TbaHidden, TbaProgress, TbaSneaking};

/// The `detect` a true-sight spell grants.
///
/// True sight is categorical: it pierces any concealment regardless of the
/// hider's level. The engine compares `stealth > detect`, and `stealth` is a
/// `u8`, so `u8::MAX` is the identity that defeats every possible stealth
/// value.
pub const TRUE_SIGHT_DETECT: u8 = u8::MAX;

/// The pack's private true-sight grant, ticked on the combat round clock and
/// mirrored into the engine [`Perception`] by [`sync_perception`].
///
/// Runtime-only (never registered with the `SaveRegistry`): exactly like
/// [`TbaHidden`]/[`TbaSneaking`], a restart drops it, which is the design
/// doc's recommended policy for transient concealment state.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TbaTrueSight {
    /// Categorical senses, a mask of `SEE_INVIS`/`SEE_DARK`.
    pub sight: u16,
    /// Opposes a target's `Concealment::stealth`.
    pub detect: u8,
    /// Combat rounds left before the sense fades.
    pub rounds_left: u32,
}

/// The `stealth` a `TbaHidden`/`TbaSneaking` player syncs into [`Concealment`].
///
/// `hide`/`sneak` are binary in Circle, so the opposed axis is anchored to the
/// concealer's level (a proxy for skill): a missing [`TbaProgress`] or a
/// sub-level-1 value floors at `1` so a fresh character is still concealed from
/// a default observer (`detect == 0`), and the level is clamped to the `u8`
/// axis. Because the default rule's boundary is visible (`stealth == detect`
/// ⇒ seen), an observer needs `detect >= level` to pierce the concealment.
pub fn stealth_for(progress: Option<&TbaProgress>) -> u8 {
    progress
        .map(|progress| progress.level.clamp(1, u32::from(u8::MAX)) as u8)
        .unwrap_or(1)
}

/// Sync [`TbaHidden`]/[`TbaSneaking`] into the engine [`Concealment`] carrier.
///
/// Writes `Concealment { hidden: true, invisible: false, stealth }` while
/// either pack state is set, and removes the carrier when neither is. The
/// comparison against the current value keeps a steady-state player from
/// re-triggering change detection every frame. Runs after the command handlers
/// that set/clear the pack state, so the engine carrier is current by the end
/// of the frame.
#[allow(clippy::type_complexity)]
pub fn sync_concealment(
    mut commands: Commands,
    players: Query<
        (
            Entity,
            Has<TbaHidden>,
            Has<TbaSneaking>,
            Option<&TbaProgress>,
            Option<&Concealment>,
        ),
        With<Player>,
    >,
) {
    for (entity, hidden, sneaking, progress, current) in &players {
        if hidden || sneaking {
            let desired = Concealment {
                hidden: true,
                invisible: false,
                stealth: stealth_for(progress),
            };
            if current != Some(&desired) {
                commands.entity(entity).insert(desired);
            }
        } else if current.is_some() {
            commands.entity(entity).remove::<Concealment>();
        }
    }
}

/// Sync [`TbaTrueSight`] into the engine [`Perception`] carrier.
///
/// Mirrors the sense while the pack grant is present and removes the carrier
/// when it is not, so a detector receives concealed occupants through the
/// engine projection and the pack's own targeting.
#[allow(clippy::type_complexity)]
pub fn sync_perception(
    mut commands: Commands,
    subjects: Query<
        (Entity, Option<&TbaTrueSight>, Option<&Perception>),
        Or<(With<TbaTrueSight>, With<Perception>)>,
    >,
) {
    for (entity, true_sight, current) in &subjects {
        match true_sight {
            Some(true_sight) => {
                let desired = Perception {
                    sight: true_sight.sight,
                    detect: true_sight.detect,
                };
                if current != Some(&desired) {
                    commands.entity(entity).insert(desired);
                }
            }
            None => {
                if current.is_some() {
                    commands.entity(entity).remove::<Perception>();
                }
            }
        }
    }
}

/// `TbaRound`: age [`TbaTrueSight`] by one round, dropping the expired grant.
///
/// [`sync_perception`] then removes the engine [`Perception`] in the same
/// frame's `Update`, so the sense expires cleanly without a second removal
/// path.
pub fn tick_true_sight(mut commands: Commands, mut sighted: Query<(Entity, &mut TbaTrueSight)>) {
    for (entity, mut true_sight) in &mut sighted {
        true_sight.rounds_left = true_sight.rounds_left.saturating_sub(1);
        if true_sight.rounds_left == 0 {
            commands.entity(entity).remove::<TbaTrueSight>();
        }
    }
}

/// Grant (or refresh) true sight on `target` for `rounds` combat rounds.
///
/// Used by the `detect` spell handler; `rounds` is the spell's
/// `affect_rounds` value, so true sight shares the buff duration machinery
/// without joining the stat-affect recompute.
pub fn grant_true_sight(target: Entity, rounds: u32, commands: &mut Commands) {
    commands.entity(target).insert(TbaTrueSight {
        sight: SEE_INVIS,
        detect: TRUE_SIGHT_DETECT,
        rounds_left: rounds.max(1),
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use smol_str::SmolStr;

    fn progress(level: u32) -> TbaProgress {
        TbaProgress {
            class: SmolStr::new("thief"),
            level,
            xp: 0,
        }
    }

    #[test]
    fn stealth_is_level_derived_and_floored_at_one() {
        assert_eq!(stealth_for(None), 1, "a missing progress still conceals");
        assert_eq!(stealth_for(Some(&progress(0))), 1, "level floors at one");
        assert_eq!(stealth_for(Some(&progress(7))), 7);
        assert_eq!(
            stealth_for(Some(&progress(1000))),
            u8::MAX,
            "level clamps to the u8 axis"
        );
    }

    #[test]
    fn concealment_sync_inserts_then_removes() {
        let mut app = App::new();
        app.add_systems(Update, sync_concealment);
        let entity = app
            .world_mut()
            .spawn((Player {
                account_id: 1,
                name: "Shadow".to_owned(),
            },))
            .id();

        app.world_mut().entity_mut(entity).insert(TbaHidden);
        app.world_mut().run_schedule(Update);
        app.world_mut().flush();
        assert_eq!(
            app.world().get::<Concealment>(entity),
            Some(&Concealment {
                hidden: true,
                invisible: false,
                stealth: 1,
            }),
            "hidden syncs a Concealment"
        );

        app.world_mut().entity_mut(entity).remove::<TbaHidden>();
        app.world_mut().run_schedule(Update);
        app.world_mut().flush();
        assert!(
            app.world().get::<Concealment>(entity).is_none(),
            "clearing the pack state removes the carrier"
        );
    }

    #[test]
    fn perception_sync_mirrors_true_sight() {
        let mut app = App::new();
        app.add_systems(Update, sync_perception);
        let entity = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(entity).insert(TbaTrueSight {
            sight: SEE_INVIS,
            detect: TRUE_SIGHT_DETECT,
            rounds_left: 3,
        });

        app.world_mut().run_schedule(Update);
        app.world_mut().flush();
        assert_eq!(
            app.world().get::<Perception>(entity),
            Some(&Perception {
                sight: SEE_INVIS,
                detect: TRUE_SIGHT_DETECT,
            })
        );

        app.world_mut().entity_mut(entity).remove::<TbaTrueSight>();
        app.world_mut().run_schedule(Update);
        app.world_mut().flush();
        assert!(app.world().get::<Perception>(entity).is_none());
    }

    #[test]
    fn true_sight_ticks_down_and_expires() {
        let mut app = App::new();
        app.add_systems(Update, tick_true_sight);
        let entity = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(entity).insert(TbaTrueSight {
            sight: SEE_INVIS,
            detect: TRUE_SIGHT_DETECT,
            rounds_left: 2,
        });

        app.world_mut().run_schedule(Update);
        app.world_mut().flush();
        assert_eq!(
            app.world()
                .get::<TbaTrueSight>(entity)
                .map(|s| s.rounds_left),
            Some(1)
        );

        app.world_mut().run_schedule(Update);
        app.world_mut().flush();
        assert!(
            app.world().get::<TbaTrueSight>(entity).is_none(),
            "the grant expires after its last round"
        );
    }
}
