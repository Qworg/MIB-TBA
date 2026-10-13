//! [`RoomHost`] for the mock.
//!
//! Beyond the shared keys (`exit:<dir>*`, `zone`, `sector`, `flag:<n>`), this
//! file reads:
//!
//! | key | meaning (C) |
//! |---|---|
//! | `globals.nums["sky"]` | `weather_info.sky` |
//!
//! An exit exists (`R_EXIT(room, dir) != NULL`) when `links["exit:<dir>"]` or
//! `nums["exit:<dir>:nowhere"]` is present, as the shared table says. Its
//! `to_room` is the link, or `NOWHERE` when the link is absent, non-existent
//! or `nowhere` is set. An exit with no `exit:<dir>:key` has no key
//! (`NOTHING`, 65535). The zone is `nums["zone"]`, the zone's *number*
//! (`zone_table[r->zone].number`, not its index), and its name
//! `globals.texts["zone:<number>:name"]`.

use crate::fields_room::{RoomExit, RoomHost};
use crate::mock::MockHost;
use crate::types::Uid;

/// `NOTHING` as an `obj_vnum`: the key of an exit that has none.
const NO_KEY: i32 = 65535;

impl RoomHost for MockHost {
    fn room_exit(&self, room: Uid, dir: usize) -> Option<RoomExit> {
        let prefix = format!("exit:{dir}");
        let entity = self.entity(room)?;
        let exists =
            entity.links.contains_key(&prefix) || self.num(room, &format!("{prefix}:nowhere")) != 0;
        exists.then(|| RoomExit {
            to_room: if self.num(room, &format!("{prefix}:nowhere")) != 0 {
                None
            } else {
                self.link(room, &prefix)
            },
            key: entity
                .nums
                .get(&format!("{prefix}:key"))
                .map_or(NO_KEY, |key| i32::try_from(*key).unwrap_or(0)),
            info: u32::try_from(self.num(room, &format!("{prefix}:flags"))).unwrap_or(0),
        })
    }

    fn room_sector(&self, room: Uid) -> i32 {
        i32::try_from(self.num(room, "sector")).unwrap_or(0)
    }

    fn room_flagged(&self, room: Uid, flag: usize) -> bool {
        self.num(room, &format!("flag:{flag}")) != 0
    }

    fn room_zone_number(&self, room: Uid) -> i32 {
        i32::try_from(self.num(room, "zone")).unwrap_or(0)
    }

    fn room_zone_name(&self, room: Uid) -> String {
        let number = self.num(room, "zone");
        self.globals
            .texts
            .get(&format!("zone:{number}:name"))
            .cloned()
            .unwrap_or_default()
    }

    fn room_sky(&self) -> i32 {
        i32::try_from(self.globals.nums.get("sky").copied().unwrap_or(0)).unwrap_or(0)
    }
}
