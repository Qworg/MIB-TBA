//! The pulse clock and the queue of scripts suspended in `wait`.
//!
//! A pulse is a tenth of a second (`PASSES_PER_SEC`), the unit the C counts
//! `wait`s, random triggers and the time of day in. [`dg_pulse_system`] turns
//! the fixed-tick clock into pulses and, once per tick, runs everything the
//! C's `heartbeat` runs for scripts: the waits that came due, the random
//! triggers every `PULSE_DG_SCRIPT` pulses, and the time triggers at the start
//! of a mud hour.

use std::time::Duration;

use dg_vm::Uid;
use dg_vm::dispatch::{check_time_triggers, script_trigger_check};
use dg_vm::driver::resume;
use dg_vm::types::PULSE_DG_SCRIPT;
use mib_core::prelude::*;

use super::chardata::TbaDgAffects;
use super::host::EcsHost;
use super::with_host;
use mib_stdlib::prelude::TimeOfDay;

/// Real time per pulse.
const PULSE: Duration = Duration::from_millis(100);

/// The most waits one tick resumes. Each resume removes a wait and a script
/// queues at most one, always for a later pulse, so this is only a guard.
const MAX_RESUMES_PER_TICK: usize = 100_000;

/// The pulse counter: the C's global `pulse`.
#[derive(Resource, Debug, Default)]
pub struct DgPulse {
    /// Pulses elapsed since the world started.
    pub pulse: i64,
    /// Time not yet worth a whole pulse.
    accumulator: Duration,
    /// The mud hour the time triggers last saw.
    last_hour: Option<u32>,
}

impl DgPulse {
    /// Add elapsed time and return how many whole pulses it made.
    pub fn advance(&mut self, delta: Duration) -> i64 {
        self.accumulator = self.accumulator.saturating_add(delta);
        let whole = (self.accumulator.as_millis() / PULSE.as_millis()).min(u128::from(u32::MAX));
        let whole = whole as u32;
        self.accumulator = self.accumulator.saturating_sub(PULSE.saturating_mul(whole));
        self.pulse = self.pulse.saturating_add(i64::from(whole));
        i64::from(whole)
    }

    /// Record the mud hour and report whether it changed since the last call.
    /// The first call only records it.
    pub fn hour_changed(&mut self, hour: u32) -> bool {
        let changed = self.last_hour.is_some_and(|last| last != hour);
        self.last_hour = Some(hour);
        changed
    }
}

/// One queued `wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DgWait {
    /// The pulse the trigger resumes on.
    pub due: i64,
    pub owner: Uid,
    /// The trigger instance's id within the owner's script.
    pub trigger: u64,
}

/// The scripts suspended in `wait`, in the order they were queued.
#[derive(Resource, Debug, Default)]
pub struct DgWaits {
    pub waits: Vec<DgWait>,
}

impl DgWaits {
    /// Queue a wait, replacing the trigger's earlier one.
    pub fn schedule(&mut self, wait: DgWait) {
        self.cancel(wait.owner, wait.trigger);
        self.waits.push(wait);
    }

    /// Forget a trigger's wait, if any.
    pub fn cancel(&mut self, owner: Uid, trigger: u64) {
        self.waits
            .retain(|wait| !(wait.owner == owner && wait.trigger == trigger));
    }

    /// Forget every wait an owner has.
    pub fn cancel_owner(&mut self, owner: Uid) {
        self.waits.retain(|wait| wait.owner != owner);
    }

    /// Remove and return the wait that came due first (the earliest pulse,
    /// then the earliest queued), if one is due by `pulse`.
    pub fn pop_due(&mut self, pulse: i64) -> Option<DgWait> {
        let index = self
            .waits
            .iter()
            .enumerate()
            .filter(|(_, wait)| wait.due <= pulse)
            .min_by_key(|(_, wait)| wait.due)
            .map(|(index, _)| index)?;
        Some(self.waits.remove(index))
    }
}

/// `FixedUpdate`: advance the pulse clock and run the script heartbeat.
///
/// Every tick opens one with-host call, which also gives newly created
/// entities their ids and prototype triggers and fires their `load` triggers.
pub fn dg_pulse_system(world: &mut World) {
    let delta = world
        .get_resource::<Time<Fixed>>()
        .map_or(Duration::ZERO, |time| time.delta());
    let (before, advanced, pulse) = {
        let mut clock = world.get_resource_or_insert_with(DgPulse::default);
        let before = clock.pulse;
        let advanced = clock.advance(delta);
        (before, advanced, clock.pulse)
    };
    let hour = world
        .get_resource::<TimeOfDay>()
        .map(|clock| u32::from(clock.hour));
    let hour_changed = hour.is_some_and(|hour| {
        world
            .get_resource_or_insert_with(DgPulse::default)
            .hour_changed(hour)
    });
    let random_due =
        advanced > 0 && pulse.div_euclid(PULSE_DG_SCRIPT) > before.div_euclid(PULSE_DG_SCRIPT);

    age_dg_affects(world, advanced);

    with_host(world, |host| {
        resume_due(host, pulse);
        if random_due {
            drop_orphan_waits(host);
            script_trigger_check(host);
        }
        if hour_changed {
            check_time_triggers(host);
        }
    });
}

/// Age every timed script affect by the pulses this tick made. `DgAffect`
/// stores its life as a remaining count, so a marker is meaningful across a
/// restart; this is the only place time takes it down.
fn age_dg_affects(world: &mut World, pulses: i64) {
    if pulses <= 0 {
        return;
    }
    let mut query = world.query::<&mut TbaDgAffects>();
    for mut marks in query.iter_mut(world) {
        for mark in &mut marks.0 {
            if let Some(expires) = mark.expires.as_mut() {
                *expires = expires.saturating_sub(pulses);
            }
        }
    }
}

/// Resume every wait due by `pulse`, earliest first. A wait whose owner or
/// trigger instance is gone is dropped; one whose owner is parked for an
/// offline holder stays queued and resumes after the holder returns.
fn resume_due(host: &mut EcsHost<'_>, pulse: i64) {
    // Waits of items parked for an offline holder, put back when the loop ends
    // so they resume once the holder is back.
    let mut parked = Vec::new();
    for _ in 0..MAX_RESUMES_PER_TICK {
        let Some(wait) = host
            .world
            .get_resource_mut::<DgWaits>()
            .and_then(|mut waits| waits.pop_due(pulse))
        else {
            break;
        };
        if host.entity(wait.owner).is_none() && host.entity_or_parked(wait.owner).is_some() {
            parked.push(wait);
            continue;
        }
        let alive = dg_vm::BaseHost::exists(&*host, wait.owner)
            && dg_vm::BaseHost::script(&*host, wait.owner)
                .is_some_and(|script| script.trigger(wait.trigger).is_some());
        if alive {
            resume(host, wait.owner, wait.trigger);
        }
    }
    if let Some(mut waits) = host.world.get_resource_mut::<DgWaits>() {
        waits.waits.extend(parked);
    }
}

/// Forget the waits of owners the game despawned, so a long wait on a mob
/// that was killed does not linger until its due pulse.
fn drop_orphan_waits(host: &mut EcsHost<'_>) {
    let Some(waits) = host.world.get_resource::<DgWaits>() else {
        return;
    };
    let gone: std::collections::HashSet<Uid> = waits
        .waits
        .iter()
        .map(|wait| wait.owner)
        .filter(|owner| host.entity_or_parked(*owner).is_none())
        .collect();
    if let Some(mut waits) = host.world.get_resource_mut::<DgWaits>() {
        waits.waits.retain(|wait| !gone.contains(&wait.owner));
    }
}
