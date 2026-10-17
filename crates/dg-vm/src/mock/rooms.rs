//! [`RoomHost`] for the mock.
//!
//! Reads only keys of the shared table (`exit:<dir>*`, `zone`, `sector`,
//! `flag:<n>`, and the world's `sky` and `zone:<n>:name`); the exit rules
//! are stated there. The zone is `nums["zone"]`, the zone's *number*
//! (`zone_table[r->zone].number`, not its index).

use crate::fields_room::{RoomExit, RoomHost};
use crate::mock::MockHost;
use crate::types::Uid;

impl RoomHost for MockHost {
    fn room_exit(&self, room: Uid, dir: usize) -> Option<RoomExit> {
        let prefix = format!("exit:{dir}");
        let entity = self.entity(room)?;
        let nowhere = self.num(room, &format!("{prefix}:nowhere")) != 0;
        let exists = self.link(room, &prefix).is_some() || nowhere;
        exists.then(|| RoomExit {
            to_room: if nowhere {
                None
            } else {
                self.link(room, &prefix)
            },
            key: entity
                .nums
                .get(&format!("{prefix}:key"))
                .map_or(0, |key| i32::try_from(*key).unwrap_or(0)),
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
