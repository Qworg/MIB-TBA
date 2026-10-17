//! `SubstHost`: the special variables a script substitutes that need the world.
//!
//! Both facts are configuration the pack fixes at the C's defaults, so the
//! answers are modelled, not unmodelled: there is no immortal command that
//! starts a happy hour, and the world uses the six compass directions.

use dg_vm::subst::{HappyHour, SubstHost};

use super::EcsHost;

impl SubstHost for EcsHost<'_> {
    /// `happy_data`, which the C initialises to `{0, 0, 0, 0}` (`db.c`) and only
    /// the immortal `hh` command ever changes. The pack has no such command and
    /// no bonus-rate hooks in its rewards, so a happy hour never runs: all zero.
    fn subst_happy_hour(&self) -> HappyHour {
        HappyHour::default()
    }

    /// `CONFIG_DIAGONAL_DIRS`, `NO` in the C's `config.c`. The converted world
    /// has only the six exits `north` to `down`, so `DIR_COUNT` is 6.
    fn subst_diagonal_dirs(&self) -> bool {
        false
    }
}
