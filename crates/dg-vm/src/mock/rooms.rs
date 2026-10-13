//! [`RoomHost`] for the mock.
//!
//! Beyond the shared keys (`exit:<dir>*`, `zone`, `sector`, `flag:<n>`), this
//! file reads:
//!
//! | key | meaning (C) |
//! |---|---|
//! | `globals.nums["sky"]` | `weather_info.sky` |
//!
//! An exit exists (`R_EXIT(room, dir) != NULL`) when the room has any of
//! `links["exit:<dir>"]`, `nums["exit:<dir>:flags"]`, `nums["exit:<dir>:key"]`,
//! `texts["exit:<dir>:keyword"]` or `texts["exit:<dir>:desc"]`; with only the
//! latter four it has no destination (`to_room == NOWHERE`). The zone number
//! is `nums["zone"]` and its name `globals.texts["zone:<number>:name"]`.

use crate::fields_room::{RoomExit, RoomHost};
use crate::mock::MockHost;
use crate::types::Uid;

impl RoomHost for MockHost {
    fn room_exit(&self, room: Uid, dir: usize) -> Option<RoomExit> {
        let prefix = format!("exit:{dir}");
        let entity = self.entity(room)?;
        let exists = entity.links.contains_key(&prefix)
            || entity.nums.contains_key(&format!("{prefix}:flags"))
            || entity.nums.contains_key(&format!("{prefix}:key"))
            || entity.texts.contains_key(&format!("{prefix}:keyword"))
            || entity.texts.contains_key(&format!("{prefix}:desc"));
        exists.then(|| RoomExit {
            to_room: self.link(room, &prefix),
            key: i32::try_from(self.num(room, &format!("{prefix}:key"))).unwrap_or(0),
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
