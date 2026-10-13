//! `SubstHost`: the special variables a script substitutes that need the world.
//!
//! Every method is unmodelled for now and answers the neutral value
//! `HOST.md` lists for it.

use dg_vm::subst::{HappyHour, SubstHost};

use super::EcsHost;

impl SubstHost for EcsHost<'_> {
    fn subst_happy_hour(&self) -> HappyHour {
        HappyHour::default()
    }

    fn subst_diagonal_dirs(&self) -> bool {
        false
    }
}
