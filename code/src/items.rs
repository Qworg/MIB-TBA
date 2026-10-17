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
use mib_stdlib::prelude::{Item, SaveRegistry};
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

/// Frame-time accumulator that converts the fixed tick into whole MUD hours.
///
/// A resource rather than a plain per-tick counter so a long frame (or a test)
/// can bank fractional time; [`due_hours`](Self::due_hours) reports how many
/// whole hours have elapsed and keeps the remainder.
///
/// The clock is also the game's **in-world calendar** (issue #19): the same
/// resource that ages object timers carries the Circle `time_info` fields, so
/// there is exactly one MUD clock, not a timer clock plus a calendar clock.
/// The cadence is Circle's: 75 real seconds per MUD hour (`SECS_PER_MUD_HOUR`,
/// `tbamud/src/utils.h:193`), 24 hours per day, 35 days per month and 17 months
/// per year (`mud_time_passed`, `tbamud/src/utils.c:505`).
///
/// Persistence: the clock is **runtime-only**. Circle writes the elapsed MUD
/// seconds to a `time` file on every boot/shutdown (`save_mud_time`,
/// `tbamud/src/db.c:880`); MIB has no generic resource-persistence slot for a
/// global clock, and item timers persist per item, not as a world time. A
/// restart therefore begins a fresh year 0, day 0, hour 0. Persisting it is a
/// named follow-up, not a silent omission.
#[derive(Resource, Debug, Default, Clone, PartialEq)]
pub struct TbaObjectClock {
    /// Seconds accumulated since the last whole MUD hour.
    pub accumulator: f32,
    /// Circle's `time_info.hours` (0..=23).
    pub hour_of_day: u32,
    /// Circle's `time_info.day`, 0..=34 (35-day months).
    pub day: u32,
    /// Circle's `time_info.month`, 0..=16 (17-month years).
    pub month: u32,
    /// Circle's `time_info.year`, unbounded.
    pub year: u32,
}

impl TbaObjectClock {
    /// Add `delta` seconds and report how many whole MUD hours are now due,
    /// consuming them from the accumulator. A non-finite or negative delta is
    /// ignored (a bare test app can present a zero clock without panicking).
    pub fn due_hours(&mut self, delta: f32) -> u32 {
        if !delta.is_finite() || delta < 0.0 {
            return 0;
        }
        self.accumulator += delta;
        let hours = (self.accumulator / MUD_HOUR_SECONDS).floor();
        if hours < 1.0 {
            return 0;
        }
        self.accumulator -= hours * MUD_HOUR_SECONDS;
        // `hours` is bounded by the delta the caller feeds; a huge delta is
        // clamped rather than saturating the timer arithmetic below.
        hours.min(u32::MAX as f32) as u32
    }

    /// Advance the calendar by `hours` whole MUD hours, carrying into days,
    /// months and years exactly as Circle's `mud_time_passed` does for
    /// continuous time. A huge `hours` value is carried correctly rather than
    /// wrapping the hour without touching the day.
    pub fn advance(&mut self, hours: u32) {
        let hour_total = self.hour_of_day + hours;
        self.hour_of_day = hour_total % 24;
        let day_total = self.day + hour_total / 24;
        self.day = day_total % 35;
        let month_total = self.month + day_total / 35;
        self.month = month_total % 17;
        self.year += month_total / 17;
    }

    /// Circle's night predicate: `SUN_DARK` is set for hours `<= 4` and `>= 22`
    /// (`reset_time`/`weather_and_time`, `tbamud/src/db.c:847`,
    /// `tbamud/src/weather.c:44`). Hours 5 and 21 are the dawn/dusk `SUN_RISE`
    /// and `SUN_SET` transitions and do **not** count as night.
    pub fn is_night(&self) -> bool {
        self.hour_of_day <= 4 || self.hour_of_day >= 22
    }
}

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
/// Circle's heartbeat, and the [`TbaObjectClock`] accumulator folds it into
/// whole hours.
pub fn tick_item_timers(
    time: Res<Time<Fixed>>,
    mut clock: ResMut<TbaObjectClock>,
    mut timers: Query<(
        Entity,
        &mut TbaItemTimer,
        Option<&DgScript>,
        Has<DgScriptInit>,
    )>,
    mut expired: Option<ResMut<ExpiredItemTimers>>,
    mut commands: Commands,
) {
    let hours = clock.due_hours(time.delta_secs());
    if hours == 0 {
        return;
    }
    // Advance the whole MUD calendar alongside the object timers, so shop
    // opening hours and the day/night predicate share the timer clock.
    clock.advance(hours);
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
    fn clock_banks_whole_mud_hours() {
        let mut clock = TbaObjectClock::default();
        assert_eq!(clock.due_hours(MUD_HOUR_SECONDS - 1.0), 0);
        assert_eq!(clock.due_hours(1.0), 1, "the hour fires exactly once");
        assert_eq!(clock.accumulator, 0.0);
        // A long frame banks multiple hours and keeps the remainder.
        assert_eq!(clock.due_hours(MUD_HOUR_SECONDS * 3.0 + 0.5), 3);
        assert!(clock.accumulator > 0.0 && clock.accumulator < MUD_HOUR_SECONDS);
        // A bad delta is ignored.
        assert_eq!(clock.due_hours(f32::NAN), 0);
        assert_eq!(clock.due_hours(-1.0), 0);
    }

    #[test]
    fn clock_advances_hours_days_months_and_years() {
        let mut clock = TbaObjectClock::default();
        clock.advance(5);
        assert_eq!((clock.hour_of_day, clock.day), (5, 0));
        // 24 hours is one day.
        clock.advance(24);
        assert_eq!((clock.hour_of_day, clock.day), (5, 1));
        // 35 days is one month.
        clock.advance(24 * 35);
        assert_eq!((clock.day, clock.month), (1, 1));
        // 17 months is one year.
        clock.advance(24 * 35 * 17);
        assert_eq!((clock.day, clock.month, clock.year), (1, 1, 1));
        // The calendar wraps exactly like Circle's `mud_time_passed`.
        let mut full = TbaObjectClock::default();
        full.advance(24 * 35 * 17);
        assert_eq!(
            (full.hour_of_day, full.day, full.month, full.year),
            (0, 0, 0, 1)
        );
        // A jump larger than a year carries through every field.
        let mut jump = TbaObjectClock::default();
        jump.advance(24 * 35 * 17 * 2 + 24 * 35 * 3 + 24 * 4 + 7);
        assert_eq!(
            (jump.hour_of_day, jump.day, jump.month, jump.year),
            (7, 4, 3, 2)
        );
    }

    #[test]
    fn is_night_matches_circles_sun_dark_window() {
        let mut clock = TbaObjectClock::default();
        for hour in 0..24 {
            clock.hour_of_day = hour;
            let expected = hour <= 4 || hour >= 22;
            assert_eq!(clock.is_night(), expected, "hour {hour}");
        }
        // The dawn/dusk transition hours are not night.
        clock.hour_of_day = 5;
        assert!(!clock.is_night());
        clock.hour_of_day = 21;
        assert!(!clock.is_night());
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
        app.init_resource::<TbaObjectClock>();
        app.add_systems(Update, tick_item_timers);
        let durable = app.world_mut().spawn(TbaItemTimer { remaining: 3 }).id();
        let dying = app.world_mut().spawn(TbaItemTimer { remaining: 1 }).id();

        // Bank exactly one hour's worth; `app.update()` may present a zero
        // fixed delta in a bare test app, so the clock is pre-charged.
        app.world_mut().resource_mut::<TbaObjectClock>().accumulator = MUD_HOUR_SECONDS;
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
