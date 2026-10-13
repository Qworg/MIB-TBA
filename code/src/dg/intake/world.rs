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
//!   Losses: the C resets every zone in `boot_db`, so the `reset` triggers run
//!   as the world comes up; the pack first resets a zone one lifespan after
//!   start, and only then do they run. An `E` reset equips without the item's
//!   `wear` trigger (counted as `reset_wear_otrigger`).
//! - `timer_otrigger` when an object's timer runs out (`point_update`,
//!   `limits.c`): [`item_timer_triggers`].

use dg_vm::dispatch::{reset_wtrigger, timer_otrigger};
use mib_core::prelude::*;
use mib_stdlib::prelude::Room;

use crate::dg::ids;
use crate::dg::script::DgScript;
use crate::dg::waits::dg_pulse_system;
use crate::dg::with_host;
use crate::items::{ExpiredItemTimers, TbaItemTimer, tick_item_timers};
use crate::zones::{ZoneResets, zone_reset_system};
use dg_vm::types::trig::OTRIG_TIMER;

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
            .after(ids::stamp_placement_order)
            .after(mib_stdlib::prelude::maintain_room_occupants)
            .after(crate::party::cleanup_parties_on_logout)
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
/// zero, which means no timer. The component stays at zero as the persisted
/// "spent" state ([`tick_item_timers`] skips it, and a restored item is not
/// re-armed from its prototype); a script that gave the item a new timer has
/// replaced the zero. Where the C walks `object_list` (newest first) after
/// `check_time_triggers`, this runs before the hour's time triggers and in
/// table order; several hours folded into one tick fire once.
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
    // An item handed over before its scripts were attached that has no
    // `timer` trigger decays like any timed item.
    for item in items {
        let timed = world
            .get::<DgScript>(item)
            .is_some_and(|script| script.0.check(OTRIG_TIMER));
        if !timed
            && world
                .get::<TbaItemTimer>(item)
                .is_some_and(|timer| timer.remaining == 0)
        {
            world.despawn(item);
        }
    }
}
