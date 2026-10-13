//! `RoomHost`: room attributes and exits.
//!
//! Every method is unmodelled for now and answers the neutral value
//! `HOST.md` lists for it.

use dg_vm::Uid;
use dg_vm::fields_room::{RoomExit, RoomHost};

use super::EcsHost;

impl RoomHost for EcsHost<'_> {
    fn room_exit(&self, _room: Uid, _dir: usize) -> Option<RoomExit> {
        None
    }

    fn room_sector(&self, _room: Uid) -> i32 {
        0
    }

    fn room_flagged(&self, _room: Uid, _flag: usize) -> bool {
        false
    }

    fn room_zone_number(&self, _room: Uid) -> i32 {
        0
    }

    fn room_zone_name(&self, _room: Uid) -> String {
        String::new()
    }

    fn room_sky(&self) -> i32 {
        0
    }
}
