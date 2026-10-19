//! Object decay timers (issue #9).
//!
//! tbaMUD gives an object prototype an optional `timer` (`src/structs.h:748`,
//! `GET_OBJ_TIMER`). `point_update` (`src/limits.c:469`) runs once per **MUD
//! hour** (`src/comm.c:1074`, `SECS_PER_MUD_HOUR * PASSES_PER_SEC`; the hour is
//! 75 real seconds, `src/utils.h:193`) and decrements every positive timer,
//! running the object's Timer trigger (`timer_otrigger`, `dg_triggers.c:682`)
//! when it reaches zero. That trigger returns immediately for an object with no
//! Timer trigger, so tbaMUD leaves a non-corpse object in place; the pack
//! instead decays it (see [`tick_item_timers`]). This module reproduces the
//! hourly cadence on the engine's fixed world-simulation tick:
//!
//! * [`attach_item_timers`] gives every newly spawned [`Item`] whose
//!   `tba.items` prototype carries a `timer` a [`TbaItemTimer`].
//! * [`tick_item_timers`] ages those timers by whole MUD hours; at zero it
//!   hands the item to its Timer trigger, or despawns it when it has none.
//!
//! ## Persistence split
//!
//! [`TbaItemTimer`] is itself the serialisable component: it is registered
//! with the stdlib [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry) under
//! [`DocKind::Item`](mib_persist::prelude::DocKind::Item), so an item entity
//! (which carries a [`SaveId`](mib_stdlib::prelude::SaveId) and
//! `SaveKind(DocKind::Item)` from every stdlib spawn path) saves its remaining
//! life and a restored item resumes decaying where it left off. No runtime-only
//! mirror is needed: the only non-portable part of a timer is which entity it
//! lives on, and the save pipeline already keys items by `SaveId`.
//!
//! [`attach_item_timers`] never overwrites a restored timer: an entity that
//! already has [`TbaItemTimer`] is left alone, so a save always beats the
//! prototype default.

use dg_vm::types::trig::OTRIG_TIMER;
use mib_core::prelude::*;
use mib_stdlib::prelude::{Item, SaveRegistry, TimeOfDay};
use serde::{Deserialize, Serialize};

use crate::dg::script::{DgScript, DgScriptInit};
use crate::module::TbaContent;

/// Real seconds in one MUD hour (Circle's `SECS_PER_MUD_HOUR`,
/// `tbamud/src/utils.h:193`). `point_update` fires once per MUD hour, which is
/// the schedule object timers age on.
pub const MUD_HOUR_SECONDS: f32 = 75.0;

/// An item's remaining lifetime, in MUD hours.
///
/// Attached from [`TbaItemDef::timer`](crate::content::TbaItemDef::timer) when
/// the item spawns and decremented by [`tick_item_timers`]. Serialisable and
/// registered under `DocKind::Item`, so the remaining life survives a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaItemTimer {
    /// Whole MUD hours left before the item decays.
    pub remaining: u32,
}

/// The items whose timer ran out in the latest [`tick_item_timers`] run and
/// that carry a DG `timer` trigger, left in the world for the DG intake to run
/// `timer_otrigger` on. Cleared by its consumer. Absent when no DG intake is
/// installed, in which case every expired item decays.
#[derive(Resource, Debug, Default)]
pub struct ExpiredItemTimers {
    pub items: Vec<Entity>,
}

/// Whole-MUD-hour edge detector for object timers, derived from the engine's
/// D-42 world clock.
///
/// The pack no longer keeps a clock of its own: [`TimeOfDay`] (wall-anchored,
/// from `content/stdlib.time.yaml`) is the single authority for world time, and
/// this resource only remembers the last sample so an item timer can be aged by
/// the number of whole MUD hours that elapsed between two ticks.
#[derive(Resource, Debug, Default)]
pub struct TbaHourTicker {
    last_epoch_secs: Option<f64>,
}

impl TbaHourTicker {
    /// A ticker whose first sample is `time`, so the next tick measures from
    /// here rather than treating the first tick as "no previous instant".
    ///
    /// Used where the clock is already established (a test app, a restored
    /// world); production lets the first fixed tick prime it.
    pub fn at(time: &TimeOfDay) -> Self {
        Self {
            last_epoch_secs: Some(time.world_epoch_secs),
        }
    }

    /// Whole MUD hours since the previous sample, consuming the sample.
    ///
    /// `time` is the engine clock; one MUD hour is 3600 world seconds whatever
    /// the content's real-time rate is. The first sample reports zero (there is
    /// no previous instant to measure from).
    pub fn due_hours(&mut self, time: &TimeOfDay) -> u32 {
        let now = time.world_epoch_secs;
        let previous = self.last_epoch_secs.replace(now);
        let Some(previous) = previous else {
            return 0;
        };
        let elapsed = now - previous;
        if !elapsed.is_finite() || elapsed < 0.0 {
            return 0;
        }
        (elapsed / WORLD_SECONDS_PER_HOUR) as u32
    }
}

/// World seconds in one MUD hour (`SECS_PER_MUD_DAY / 24`, `tbamud/src/utils.h:196`).
/// World time advances at `seconds_per_real_second`, so this is independent of
/// the content's real-time rate.
pub const WORLD_SECONDS_PER_HOUR: f64 = 3600.0;

/// `FixedUpdate`: attach a [`TbaItemTimer`] to every newly spawned item whose
/// prototype declares a `timer`.
///
/// `Added<Item>` catches every spawn path (content instantiation, zone reset,
/// `get`/shop spawns, restored runtime items) without each having to know
/// about timers. A `Some(0)` prototype timer is Circle's "no timer" sentinel
/// (`point_update` only decrements `GET_OBJ_TIMER(j) > 0`) and is skipped. A
/// restored timer already on the entity wins, so a save is never clobbered by
/// the prototype default.
pub fn attach_item_timers(
    content: Option<Res<TbaContent>>,
    items: Query<(Entity, &Item, Option<&TbaItemTimer>), Added<Item>>,
    mut commands: Commands,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);
    for (entity, item, existing) in &items {
        if existing.is_some() {
            continue;
        }
        let Some(remaining) = content
            .items
            .get(&item.def_id)
            .and_then(|def| def.timer)
            .filter(|timer| *timer > 0)
        else {
            continue;
        };
        commands.entity(entity).insert(TbaItemTimer { remaining });
    }
}

/// `FixedUpdate`: age timed items by whole MUD hours and despawn them at zero.
///
/// Runs on the fixed world-simulation tick because object decay is world time,
/// not combat time: `point_update` fires on the MUD-hour pulse, not on the
/// 3-second combat round, so [`TbaRound`](crate::combat::TbaRound) would decay
/// items ~25× too fast. The engine's fixed tick is the closest analogue to
/// Circle's heartbeat, and [`TbaHourTicker`] folds it into whole hours from the
/// shared world clock.
pub fn tick_item_timers(
    clock: Option<Res<TimeOfDay>>,
    mut ticker: ResMut<TbaHourTicker>,
    mut timers: Query<(
        Entity,
        &mut TbaItemTimer,
        Option<&DgScript>,
        Has<DgScriptInit>,
    )>,
    mut expired: Option<ResMut<ExpiredItemTimers>>,
    mut commands: Commands,
) {
    let empty = TimeOfDay::default();
    let hours = ticker.due_hours(clock.as_deref().unwrap_or(&empty));
    if hours == 0 {
        return;
    }
    for (entity, mut timer, script, initialised) in &mut timers {
        // A spent timer stays at zero, which is no timer (`point_update` ages
        // only a positive `GET_OBJ_TIMER`): it neither fires nor decays again.
        if timer.remaining == 0 {
            continue;
        }
        timer.remaining = timer.remaining.saturating_sub(hours);
        if timer.remaining != 0 {
            continue;
        }
        // An item with a DG `timer` trigger is Circle's timed object: the
        // trigger decides its fate (`point_update` calls `timer_otrigger` and
        // nothing follows). It is handed to the DG intake instead of decaying;
        // so is an item whose prototype scripts are not attached yet, which
        // the intake despawns if it turns out to have no `timer` trigger.
        let timed = script.is_some_and(|script| script.0.check(OTRIG_TIMER));
        match &mut expired {
            Some(expired) if timed || !initialised => expired.items.push(entity),
            _ => commands.entity(entity).despawn(),
        }
    }
}

/// Register the timer schema. Called from [`crate::module::TbaPlugin::build`]
/// so a bare plugin composition still persists timers.
pub fn register(registry: &mut SaveRegistry) {
    registry.register::<TbaItemTimer>("TbaItemTimer", mib_persist::prelude::DocKind::Item);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use mib_stdlib::prelude::ItemId;

    use super::*;
    use crate::content::{TbaItemDef, TbaItemKind};

    fn item(timer: Option<u32>) -> TbaItemDef {
        TbaItemDef {
            kind: TbaItemKind::Treasure,
            slots: Vec::new(),
            values: [0; 4],
            affects: Vec::new(),
            keywords: Vec::new(),
            extra_flags: Vec::new(),
            affect_flags: Vec::new(),
            weight: 0,
            cost: 0,
            rent: 0,
            min_level: 0,
            timer,
            action_description: None,
            extra_descriptions: Vec::new(),
        }
    }

    fn content(timer: Option<u32>) -> TbaContent {
        TbaContent {
            items: BTreeMap::from([(ItemId::from("torch"), item(timer))]),
            ..TbaContent::default()
        }
    }

    fn spawn_timed_item(app: &mut App, def_id: &str) -> Entity {
        app.world_mut()
            .spawn(Item {
                def_id: ItemId::from(def_id),
                name: "a torch".to_owned(),
                description: String::new(),
                value: None,
                tags: Vec::new(),
            })
            .id()
    }

    #[test]
    fn the_hour_ticker_reports_whole_mud_hours_from_the_world_clock() {
        let mut ticker = TbaHourTicker::default();
        let mut time = TimeOfDay {
            world_epoch_secs: 10_000.0,
            ..TimeOfDay::default()
        };
        // The first sample has no previous instant to measure from.
        assert_eq!(ticker.due_hours(&time), 0);
        // Part of an hour is not an hour.
        time.world_epoch_secs += WORLD_SECONDS_PER_HOUR - 1.0;
        assert_eq!(ticker.due_hours(&time), 0);
        // A full hour measured from the previous sample fires exactly once.
        time.world_epoch_secs += WORLD_SECONDS_PER_HOUR;
        assert_eq!(ticker.due_hours(&time), 1, "the hour fires exactly once");
        // A long gap reports every whole hour it crossed.
        time.world_epoch_secs += WORLD_SECONDS_PER_HOUR * 3.0 + 0.5;
        assert_eq!(ticker.due_hours(&time), 3);
    }

    #[test]
    fn a_backwards_or_non_finite_clock_reports_no_hours() {
        let mut ticker = TbaHourTicker::default();
        let mut time = TimeOfDay {
            world_epoch_secs: 10_000.0,
            ..TimeOfDay::default()
        };
        assert_eq!(ticker.due_hours(&time), 0);
        time.world_epoch_secs = 5_000.0;
        assert_eq!(ticker.due_hours(&time), 0, "time never runs backwards");
        time.world_epoch_secs = f64::NAN;
        assert_eq!(ticker.due_hours(&time), 0, "a bad clock ages nothing");
    }

    #[test]
    fn attach_reads_the_prototype_timer_and_skips_zero() {
        let mut app = App::new();
        app.insert_resource(content(Some(5)));
        app.add_systems(Update, attach_item_timers);
        let timed = spawn_timed_item(&mut app, "torch");
        let untimed = spawn_timed_item(&mut app, "unknown");
        app.update();
        assert_eq!(
            app.world().get::<TbaItemTimer>(timed),
            Some(&TbaItemTimer { remaining: 5 })
        );
        assert!(app.world().get::<TbaItemTimer>(untimed).is_none());

        // A zero prototype timer is Circle's "no timer".
        let mut app = App::new();
        app.insert_resource(content(Some(0)));
        app.add_systems(Update, attach_item_timers);
        let item = spawn_timed_item(&mut app, "torch");
        app.update();
        assert!(app.world().get::<TbaItemTimer>(item).is_none());
    }

    #[test]
    fn attach_never_overwrites_a_restored_timer() {
        let mut app = App::new();
        app.insert_resource(content(Some(5)));
        app.add_systems(Update, attach_item_timers);
        let item = app
            .world_mut()
            .spawn((
                Item {
                    def_id: ItemId::from("torch"),
                    name: "a torch".to_owned(),
                    description: String::new(),
                    value: None,
                    tags: Vec::new(),
                },
                TbaItemTimer { remaining: 2 },
            ))
            .id();
        app.update();
        assert_eq!(
            app.world().get::<TbaItemTimer>(item),
            Some(&TbaItemTimer { remaining: 2 }),
            "the saved remaining life beats the prototype default"
        );
    }

    #[test]
    fn tick_decrements_and_despawns_at_zero() {
        let mut app = App::new();
        app.insert_resource(Time::<Fixed>::from_hz(1.0));
        app.init_resource::<TbaHourTicker>();
        app.insert_resource(TimeOfDay {
            world_epoch_secs: 0.0,
            ..TimeOfDay::default()
        });
        app.add_systems(Update, tick_item_timers);
        let durable = app.world_mut().spawn(TbaItemTimer { remaining: 3 }).id();
        let dying = app.world_mut().spawn(TbaItemTimer { remaining: 1 }).id();

        // The first tick only primes the hour ticker (there is no previous
        // sample to measure from), so the clock is stepped after it.
        app.update();
        assert_eq!(
            app.world().get::<TbaItemTimer>(durable),
            Some(&TbaItemTimer { remaining: 3 }),
            "the priming tick ages nothing"
        );
        app.world_mut().resource_mut::<TimeOfDay>().world_epoch_secs += WORLD_SECONDS_PER_HOUR;
        app.update();
        assert_eq!(
            app.world().get::<TbaItemTimer>(durable),
            Some(&TbaItemTimer { remaining: 2 })
        );
        assert!(
            app.world().get_entity(dying).is_err(),
            "the timer hit zero and the item was despawned"
        );

        // A sub-hour tick changes nothing.
        app.update();
        assert_eq!(
            app.world().get::<TbaItemTimer>(durable),
            Some(&TbaItemTimer { remaining: 2 })
        );
    }

    #[test]
    fn timer_round_trips_through_serde() {
        let json = serde_json::to_string(&TbaItemTimer { remaining: 7 }).expect("serialize");
        let back: TbaItemTimer = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, TbaItemTimer { remaining: 7 });
    }
}
