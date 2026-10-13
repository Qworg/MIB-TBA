//! [`SubstHost`] for the mock.
//!
//! Keys beyond the shared data model:
//!
//! | where | key | meaning (C) |
//! |---|---|---|
//! | `globals` | `nums["happy:qp"]`, `nums["happy:exp"]`, `nums["happy:gold"]` | `happy_data.qp_rate`, `exp_rate`, `gold_rate` |
//! | `globals` | `nums["happy:ticks"]` | `happy_data.ticks_left` |
//!
//! An exit exists when the room has `links["exit:<dir>"]` or
//! `nums["exit:<dir>:flags"]` (an exit that leads nowhere has only flags).

use crate::mock::MockHost;
use crate::subst::{HappyHour, SubstHost};
use crate::types::Uid;

impl SubstHost for MockHost {
    fn subst_happy_hour(&self) -> HappyHour {
        let get = |key: &str| {
            self.globals
                .nums
                .get(key)
                .and_then(|value| i32::try_from(*value).ok())
                .unwrap_or(0)
        };
        HappyHour {
            qp_rate: get("happy:qp"),
            exp_rate: get("happy:exp"),
            gold_rate: get("happy:gold"),
            ticks_left: get("happy:ticks"),
        }
    }

    fn subst_has_exit(&self, room: Uid, dir: usize) -> bool {
        self.entity(room).is_some_and(|entity| {
            entity.links.contains_key(&format!("exit:{dir}"))
                || entity.nums.contains_key(&format!("exit:{dir}:flags"))
        })
    }
}
