//! [`DispatchHost`] for the mock.
//!
//! Keys beyond the shared model of `mock/mod.rs`:
//!
//! | key | meaning (C) |
//! |---|---|
//! | `globals.nums["zone:<n>:empty"]` | `is_empty(zone)` for the zone numbered `n` (a room's `nums["zone"]`); absent = occupied |
//!
//! Effects recorded with [`MockHost::effect`]: `free_proto_script <uid>`.

use crate::dispatch::DispatchHost;
use crate::host::WorldHost;
use crate::mock::MockHost;
use crate::types::{Kind, Uid};

impl DispatchHost for MockHost {
    fn disp_zone_empty(&self, room: Uid) -> bool {
        let zone = self.num(room, "zone");
        self.globals
            .nums
            .get(&format!("zone:{zone}:empty"))
            .is_some_and(|empty| *empty != 0)
    }

    fn disp_free_proto_script(&mut self, uid: Uid) {
        // Only a mob or object that came from a prototype has one to free.
        if matches!(self.kind(uid), Some(Kind::Char | Kind::Obj)) && self.vnum(uid) >= 0 {
            self.effect(format!("free_proto_script {uid}"));
        }
    }
}
