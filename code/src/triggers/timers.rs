//! Trigger timers (issue #15, Phase 4).
//!
//! `wait <n> [sec|s|tick]` suspends a record's program counter and
//! `wait until <hh:mm>` hooks the in-world [`TbaObjectClock`]. This module owns
//! the queue that resumes them:
//!
//! - [`TriggerTimers`] is a runtime-only resource (DG parity: a reboot loses
//!   pending waits). [`PendingTimer`] carries the trigger id, the resume
//!   program counter, the trigger-local frame, and the substitution context
//!   captured at suspension, so a resume does not need to re-derive any of it.
//! - [`trigger_timer_system`] advances each timer on the **engine fixed tick**
//!   (`Time<Fixed>`), fires the due ones, and feeds the resumed
//!   [`EvalOutcome`] through the same `queue_effects`/`deliver` seam the
//!   original firing used. A resumed record that hits another `wait` is
//!   re-armed.
//!
//! A timer whose owner or actor has been despawned (a zone reset, a purge, a
//! logout) is **cancelled** and recorded as a named drop, so a purge cannot
//! leave a timer firing into a dead entity and nothing leaks across a reset.
//!
//! The per-record [`STEP_CAP`](crate::triggers::ast::STEP_CAP) is per resume:
//! a `wait` ends the slice, and the next slice gets a fresh budget. That is
//! what keeps a `wait` from wedging a tick — the interpreter returns instead of
//! blocking, and the queue is drained by its own system.

use std::collections::BTreeMap;

use mib_core::prelude::*;

use crate::combat::progression::TbaRng;
use crate::items::TbaObjectClock;
use crate::triggers::ast::TriggerWait;
use crate::triggers::content::TriggerId;
use crate::triggers::context::{NamedDrop, TriggerContext};
use crate::triggers::effects::PendingTriggerOps;
use crate::triggers::eval::run_from;
use crate::triggers::events::{TriggerEvent, TriggerKind, TriggerView, deliver, queue_effects};
use crate::triggers::index::TbaTriggers;

/// A suspended record waiting to resume.
#[derive(Debug, Clone)]
pub struct PendingTimer {
    /// The trigger owner (`self`), used to resolve `%self%` and to cancel.
    pub owner: Entity,
    /// The triggering player, used for delivery and `%actor%`.
    pub actor: Entity,
    /// The room the record fired in.
    pub room: Entity,
    pub trigger: TriggerId,
    /// The instruction after the `wait`.
    pub pc: usize,
    /// The trigger-local frame at suspension.
    pub locals: BTreeMap<String, String>,
    /// The substitution context captured at suspension.
    pub ctx: TriggerContext,
    pub wait: TriggerWait,
    /// Engine seconds left for a `wait <n> sec`.
    pub remaining_secs: f32,
    /// Fixed ticks left for a `wait <n> tick`.
    pub remaining_ticks: u32,
    /// The absolute in-world MUD hour a `wait until` resumes at.
    pub target_mud_hour: Option<i64>,
}

/// The runtime `wait` queue.
#[derive(Debug, Default, Resource)]
pub struct TriggerTimers {
    pub timers: Vec<PendingTimer>,
}

impl TriggerTimers {
    /// Whether no record is suspended.
    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }

    /// Arm a timer.
    pub fn schedule(&mut self, timer: PendingTimer) {
        self.timers.push(timer);
    }

    /// Advance every timer by `delta_secs` of fixed-tick time and return the
    /// due ones, preserving suspension order.
    pub fn advance(
        &mut self,
        delta_secs: f32,
        clock: Option<&TbaObjectClock>,
    ) -> Vec<PendingTimer> {
        let delta = if delta_secs.is_finite() && delta_secs > 0.0 {
            delta_secs
        } else {
            0.0
        };
        let mut due = Vec::new();
        let mut index = 0;
        while index < self.timers.len() {
            let ready = {
                let timer = &mut self.timers[index];
                match timer.wait {
                    TriggerWait::Seconds(_) => {
                        timer.remaining_secs -= delta;
                        timer.remaining_secs <= 0.0
                    }
                    TriggerWait::Ticks(_) => {
                        timer.remaining_ticks = timer.remaining_ticks.saturating_sub(1);
                        timer.remaining_ticks == 0
                    }
                    TriggerWait::Until { .. } => match (clock, timer.target_mud_hour) {
                        (Some(clock), Some(target)) => absolute_mud_hours(clock) >= target,
                        _ => false,
                    },
                }
            };
            if ready {
                due.push(self.timers.remove(index));
            } else {
                index += 1;
            }
        }
        due
    }
}

/// Arm a [`PendingTimer`] from a suspension.
pub fn arm_timer(
    owner: Entity,
    actor: Entity,
    room: Entity,
    trigger: TriggerId,
    ctx: &TriggerContext,
    suspension: crate::triggers::eval::Suspension,
    clock: Option<&TbaObjectClock>,
) -> PendingTimer {
    let (remaining_secs, remaining_ticks, target_mud_hour) = match &suspension.wait {
        TriggerWait::Seconds(count) => (*count as f32, 0, None),
        TriggerWait::Ticks(count) => (0.0, *count, None),
        TriggerWait::Until { hour, minute } => {
            let target = clock.and_then(|clock| until_target(clock, *hour, *minute));
            (0.0, 0, target)
        }
    };
    PendingTimer {
        owner,
        actor,
        room,
        trigger,
        pc: suspension.pc,
        locals: suspension.locals,
        ctx: ctx.clone(),
        wait: suspension.wait,
        remaining_secs,
        remaining_ticks,
        target_mud_hour,
    }
}

/// The absolute MUD hour `wait until hh:mm` should fire at: the next time the
/// clock reaches `hour`, or a full day later when it is already that hour.
fn until_target(clock: &TbaObjectClock, hour: u32, _minute: u32) -> Option<i64> {
    let now = absolute_mud_hours(clock);
    let current_hour = i64::from(clock.hour_of_day);
    let mut delta = (i64::from(hour) + 24 - current_hour) % 24;
    if delta == 0 {
        delta = 24;
    }
    Some(now + delta)
}

/// An absolute, monotonic MUD-hour counter derived from the calendar.
fn absolute_mud_hours(clock: &TbaObjectClock) -> i64 {
    let days = i64::from(clock.year) * 17 * 35 + i64::from(clock.month) * 35 + i64::from(clock.day);
    days * 24 + i64::from(clock.hour_of_day)
}

/// `FixedUpdate`, before the effect drain: resume every due `wait`.
#[allow(clippy::too_many_arguments)]
pub fn trigger_timer_system(
    time: Res<Time<Fixed>>,
    clock: Option<Res<TbaObjectClock>>,
    mut timers: ResMut<TriggerTimers>,
    triggers: Option<Res<TbaTriggers>>,
    view: TriggerView,
    mut rng: ResMut<TbaRng>,
    mut ops: ResMut<PendingTriggerOps>,
    mut output: MessageWriter<OutputEvent>,
) {
    if timers.is_empty() {
        return;
    }
    let Some(triggers) = triggers else {
        timers.timers.clear();
        return;
    };
    let delta = time.delta_secs();
    let due = timers.advance(delta, clock.as_deref());
    for timer in due {
        if !entity_alive(&view, timer.owner) || view.players.get(timer.actor).is_err() {
            ops.drops
                .push(NamedDrop::new("timer_cancelled", "owner or actor purged"));
            continue;
        }
        let Some(program) = triggers.program(&timer.trigger) else {
            ops.drops.push(NamedDrop::new(
                "timer_orphan",
                format!("missing trigger {}", timer.trigger),
            ));
            continue;
        };
        if !program.is_valid() {
            continue;
        }
        let mut outcome = run_from(
            &program.nodes,
            timer.pc,
            timer.locals.clone(),
            &timer.ctx,
            &mut rng,
        );
        let event = TriggerEvent {
            kind: TriggerKind::Wait,
            actor: timer.actor,
            room: timer.room,
            direction: timer.ctx.direction.clone(),
            ..TriggerEvent::default()
        };
        queue_effects(&mut outcome, &event, timer.owner, &view, &mut ops);
        deliver(&outcome, &event, timer.owner, &view, &mut output);
        if let Some(suspension) = outcome.suspension {
            let next = arm_timer(
                timer.owner,
                timer.actor,
                timer.room,
                timer.trigger.clone(),
                &timer.ctx,
                suspension,
                clock.as_deref(),
            );
            timers.schedule(next);
        }
    }
}

/// Whether the trigger owner still exists as an NPC, room, or item.
fn entity_alive(view: &TriggerView, entity: Entity) -> bool {
    view.npcs.get(entity).is_ok()
        || view.rooms.get(entity).is_ok()
        || view.players.get(entity).is_ok()
        || view.ground.get(entity).is_ok()
        || view.carried.get(entity).is_ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn clock(hour: u32) -> TbaObjectClock {
        clock_at(0, hour)
    }

    fn clock_at(day: u32, hour: u32) -> TbaObjectClock {
        TbaObjectClock {
            hour_of_day: hour,
            day,
            ..TbaObjectClock::default()
        }
    }

    fn timer(wait: TriggerWait) -> PendingTimer {
        let remaining_secs = match &wait {
            TriggerWait::Seconds(count) => *count as f32,
            _ => 0.0,
        };
        let remaining_ticks = match &wait {
            TriggerWait::Ticks(count) => *count,
            _ => 0,
        };
        let target_mud_hour = match &wait {
            TriggerWait::Until { hour, minute } => {
                until_target(&clock(hour.saturating_sub(1)), *hour, *minute)
            }
            _ => None,
        };
        PendingTimer {
            owner: Entity::PLACEHOLDER,
            actor: Entity::PLACEHOLDER,
            room: Entity::PLACEHOLDER,
            trigger: TriggerId::from_vnum(1),
            pc: 0,
            locals: BTreeMap::new(),
            ctx: TriggerContext::default(),
            wait,
            remaining_secs,
            remaining_ticks,
            target_mud_hour,
        }
    }

    #[test]
    fn seconds_advance_and_fire_once() {
        let mut timers = TriggerTimers::default();
        timers.schedule(timer(TriggerWait::Seconds(2)));
        assert!(timers.advance(1.0, None).is_empty());
        assert_eq!(timers.advance(1.0, None).len(), 1);
        assert!(timers.is_empty());
    }

    #[test]
    fn ticks_fire_after_the_count() {
        let mut timers = TriggerTimers::default();
        timers.schedule(timer(TriggerWait::Ticks(3)));
        assert!(timers.advance(0.0, None).is_empty());
        assert!(timers.advance(0.0, None).is_empty());
        assert_eq!(timers.advance(0.0, None).len(), 1);
    }

    #[test]
    fn until_fires_when_the_clock_reaches_the_hour() {
        let mut timers = TriggerTimers::default();
        // Armed at 06:00 for 21:00: 15 hours ahead.
        let mut pending = timer(TriggerWait::Until {
            hour: 21,
            minute: 0,
        });
        pending.target_mud_hour = until_target(&clock(6), 21, 0);
        timers.schedule(pending);
        assert!(timers.advance(1.0, Some(&clock(20))).is_empty());
        assert_eq!(timers.advance(1.0, Some(&clock(21))).len(), 1);
    }

    #[test]
    fn until_waits_for_the_next_day_when_the_hour_has_passed() {
        let mut timers = TriggerTimers::default();
        let mut pending = timer(TriggerWait::Until { hour: 6, minute: 0 });
        // Armed at day 0, 21:00 for 06:00: target is day 1, 06:00.
        pending.target_mud_hour = until_target(&clock(21), 6, 0);
        timers.schedule(pending);
        assert!(timers.advance(1.0, Some(&clock_at(0, 23))).is_empty());
        // Cross midnight into the next day.
        assert!(timers.advance(1.0, Some(&clock_at(1, 1))).is_empty());
        assert_eq!(timers.advance(1.0, Some(&clock_at(1, 6))).len(), 1);
    }

    #[test]
    fn absolute_hours_advance_across_days() {
        let mut clock = TbaObjectClock {
            day: 0,
            hour_of_day: 23,
            ..TbaObjectClock::default()
        };
        let before = absolute_mud_hours(&clock);
        clock.day = 1;
        clock.hour_of_day = 0;
        assert_eq!(absolute_mud_hours(&clock), before + 1);
    }
}
