//! The TBA round runner: combat resolves on its own 3-second cadence.
//!
//! CircleMUD combat is round-based, not tick-based. Tying a round to the
//! engine's fixed `tick_hz` would either force that global clock to change or
//! make a round's length a function of `tick_hz`. So TBA owns a dedicated
//! [`TbaRound`] schedule, driven by [`run_due_round`] on the **frame** clock:
//! it accumulates real frame time and fires the schedule once per
//! [`ROUND_SECONDS`], leaving `FixedUpdate`/`tick_hz` for the world simulation
//! (movement, spawners, persistence) untouched.
//!
//! `run_due_round` is an exclusive system and runs after
//! [`BridgeSet::Game`](mib_core::prelude::BridgeSet), so a command processed
//! this frame (attacking, fleeing) is visible to the round it starts.

use mib_core::prelude::*;
// `mib-core`'s prelude re-exports the `ScheduleLabel` *trait* from Bevy but not
// the derive macro, so the macro is imported explicitly here (D2 otherwise
// routes Bevy imports through the prelude).
use bevy::ecs::schedule::ScheduleLabel;

/// Seconds per combat round (doc §4.3).
pub const ROUND_SECONDS: f32 = 3.0;

/// The dedicated combat-round schedule. Systems added here run once per
/// [`ROUND_SECONDS`] of frame time, never on the fixed tick.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TbaRound;

/// Accumulates frame time between combat rounds.
#[derive(Debug, Default, Resource)]
pub struct TbaRoundClock {
    /// Seconds accumulated since the last round fired.
    pub accumulator: f32,
}

impl TbaRoundClock {
    /// Add `delta` seconds and report whether a round is due, consuming one
    /// round's worth of time when it is. The `while` is a loop in all but
    /// syntax: a single huge frame should not swallow multiple rounds silently,
    /// so the surplus is preserved for the next frame instead of firing a
    /// burst of rounds at once.
    fn due(&mut self, delta: f32) -> bool {
        if !delta.is_finite() || delta < 0.0 {
            return false;
        }
        self.accumulator += delta;
        if self.accumulator >= ROUND_SECONDS {
            self.accumulator -= ROUND_SECONDS;
            true
        } else {
            false
        }
    }
}

/// Exclusive system: run the [`TbaRound`] schedule when a round is due.
///
/// Uses the default (frame) [`Time`] clock deliberately; see the module docs.
/// A missing `Time` resource (a bare test app) is treated as zero delta, so
/// this never panics.
pub fn run_due_round(world: &mut World) {
    let delta = world
        .get_resource::<Time>()
        .map(Time::delta_secs)
        .unwrap_or(0.0);
    if !world.contains_resource::<TbaRoundClock>() {
        world.init_resource::<TbaRoundClock>();
    }
    let due = world.resource_mut::<TbaRoundClock>().due(delta);
    if due {
        world.run_schedule(TbaRound);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn clock_fires_once_per_round_and_keeps_surplus() {
        let mut clock = TbaRoundClock::default();
        assert!(!clock.due(1.0));
        assert!(!clock.due(1.0));
        assert!(clock.due(1.0), "3.0s of frame time is one round");
        assert!(!clock.due(1.0), "the clock resets after firing");
        // A long frame fires exactly one round and banks the rest.
        assert!(clock.due(ROUND_SECONDS + 0.5));
        assert!(!clock.due(0.0));
        assert!(clock.accumulator < ROUND_SECONDS);
    }

    #[test]
    fn non_finite_delta_is_ignored() {
        let mut clock = TbaRoundClock::default();
        assert!(!clock.due(f32::NAN));
        assert!(!clock.due(-1.0));
        assert_eq!(clock.accumulator, 0.0);
    }
}
