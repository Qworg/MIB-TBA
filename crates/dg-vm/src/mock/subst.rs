//! [`SubstHost`] for the mock.
//!
//! Keys beyond the shared data model:
//!
//! | where | key | meaning (C) |
//! |---|---|---|
//! | `globals` | `nums["happy:qp"]`, `nums["happy:exp"]`, `nums["happy:gold"]` | `happy_data.qp_rate`, `exp_rate`, `gold_rate` |
//! | `globals` | `nums["happy:ticks"]` | `happy_data.ticks_left` |
//! | `globals` | `nums["config:diagonal_dirs"]` | `CONFIG_DIAGONAL_DIRS` (nonzero = on) |

use crate::mock::MockHost;
use crate::subst::{HappyHour, SubstHost};

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

    fn subst_diagonal_dirs(&self) -> bool {
        self.globals
            .nums
            .get("config:diagonal_dirs")
            .is_some_and(|value| *value != 0)
    }
}
