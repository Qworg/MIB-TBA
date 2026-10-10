//! Object decay timers (issue #9).
//!
//! tbaMUD gives an object prototype an optional `timer` (`src/structs.h:748`,
//! `GET_OBJ_TIMER`). `point_update` (`src/limits.c:469`) runs once per **MUD
//! hour** (`src/comm.c:1074`, `SECS_PER_MUD_HOUR * PASSES_PER_SEC`; the hour is
//! 75 real seconds, `src/utils.h:193`) and decrements every positive timer,
//! extracting the object when it reaches zero. This module reproduces that
//! cadence on the engine's fixed world-simulation tick:
//!
//! * [`attach_item_timers`] gives every newly spawned [`Item`] whose
//!   `tba.items` prototype carries a `timer` a [`TbaItemTimer`].
//! * [`tick_item_timers`] ages those timers by whole MUD hours and despawns the
//!   item at zero.
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

use mib_core::prelude::*;
use mib_stdlib::prelude::{Item, SaveRegistry};
use serde::{Deserialize, Serialize};

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

/// Frame-time accumulator that converts the fixed tick into whole MUD hours.
///
/// A resource rather than a plain per-tick counter so a long frame (or a test)
/// can bank fractional time; [`due_hours`](Self::due_hours) reports how many
/// whole hours have elapsed and keeps the remainder.
#[derive(Resource, Debug, Default)]
pub struct TbaObjectClock {
    /// Seconds accumulated since the last whole MUD hour.
    pub accumulator: f32,
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
    mut timers: Query<(Entity, &mut TbaItemTimer)>,
    mut commands: Commands,
) {
    let hours = clock.due_hours(time.delta_secs());
    if hours == 0 {
        return;
    }
    for (entity, mut timer) in &mut timers {
        timer.remaining = timer.remaining.saturating_sub(hours);
        if timer.remaining == 0 {
            commands.entity(entity).despawn();
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
