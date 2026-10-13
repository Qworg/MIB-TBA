//! [`DispatchHost`] for the mock.
//!
//! Effects recorded with [`MockHost::effect`]: `free_proto_script <uid>`.

use crate::dispatch::DispatchHost;
use crate::host::WorldHost;
use crate::mock::MockHost;
use crate::types::{Kind, Uid};

impl DispatchHost for MockHost {
    fn disp_free_proto_script(&mut self, uid: Uid) {
        // Only a mob or object that came from a prototype has one to free.
        if matches!(self.kind(uid), Some(Kind::Char | Kind::Obj)) && self.vnum(uid) >= 0 {
            self.effect(format!("free_proto_script {uid}"));
        }
    }
}
