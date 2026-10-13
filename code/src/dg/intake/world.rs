//! Intake for zone reset, load and object timer triggers.
//!
//! - `reset_wtrigger` for every room of a zone that reset this tick, after the
//!   zone's commands ran (`reset_zone`, `db.c`): [`zone_reset_triggers`].
//! - `load` triggers of what a reset creates need no system here. They are
//!   fired by [`with_host`] for every mob and object it sees for the first
//!   time, and [`zone_reset_triggers`] is itself a `with_host` call, so the
//!   new mobs and objects load before the rooms' `reset` triggers run, which
//!   is the C's order (`read_mobile` loads at creation; `reset_wtrigger` comes
//!   last).
//! - `timer_otrigger` when an object's timer runs out (`point_update`,
//!   `limits.c`): [`item_timer_triggers`].

use dg_vm::dispatch::{reset_wtrigger, timer_otrigger};
use mib_core::prelude::*;
use mib_stdlib::prelude::Room;

use crate::dg::waits::dg_pulse_system;
use crate::dg::with_host;
use crate::items::{ExpiredItemTimers, TbaItemTimer, tick_item_timers};
use crate::zones::{ZoneResets, zone_reset_system};

/// Register this module's systems.
pub fn register(app: &mut App) {
    app.init_resource::<ZoneResets>();
    app.init_resource::<ExpiredItemTimers>();
    app.add_systems(
        FixedUpdate,
        (zone_reset_triggers, item_timer_triggers)
            .chain()
            .after(zone_reset_system)
            .after(tick_item_timers)
            .before(dg_pulse_system),
    );
}

/// `FixedUpdate`, after the zone reset: run the `reset` trigger of every room
/// of each zone that reset, zone by zone and room by room in vnum order, as
/// `reset_zone` does. The list of resets is consumed.
pub fn zone_reset_triggers(world: &mut World) {
    let zones: Vec<(u32, u32)> = match world.get_resource_mut::<ZoneResets>() {
        Some(mut resets) => std::mem::take(&mut resets.zones),
        None => return,
    };
    if zones.is_empty() {
        return;
    }
    let mut rooms: Vec<(u32, Entity)> = {
        let mut query = world.query::<(Entity, &Room)>();
        query
            .iter(world)
            .map(|(entity, room)| (room.num, entity))
            .collect()
    };
    rooms.sort_unstable();
    with_host(world, |host| {
        for (bot, top) in zones {
            for (_, room) in rooms.iter().filter(|(num, _)| (bot..=top).contains(num)) {
                if let Some(uid) = host.uid(*room) {
                    reset_wtrigger(host, uid);
                }
            }
        }
    });
}

/// `FixedUpdate`, after the item timers aged: run the `timer` triggers of the
/// items whose timer ran out. The C calls `timer_otrigger` and nothing
/// follows: the object stays unless a script removed it, with its timer at
/// zero, which means no timer. An item still in the world after its trigger
/// and still at zero therefore loses its timer; one a script gave a new timer
/// keeps it.
pub fn item_timer_triggers(world: &mut World) {
    let items: Vec<Entity> = match world.get_resource_mut::<ExpiredItemTimers>() {
        Some(mut expired) => std::mem::take(&mut expired.items),
        None => return,
    };
    if items.is_empty() {
        return;
    }
    with_host(world, |host| {
        for item in &items {
            if let Some(uid) = host.uid(*item) {
                timer_otrigger(host, uid);
            }
        }
    });
    for item in items {
        let spent = world
            .get::<TbaItemTimer>(item)
            .is_some_and(|timer| timer.remaining == 0);
        if spent {
            world.entity_mut(item).remove::<TbaItemTimer>();
        }
    }
}
