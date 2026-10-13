//! `DispatchHost`: enumerating who can react to an event.
//!
//! Every method is unmodelled for now and answers the neutral value
//! `HOST.md` lists for it.

use dg_vm::Uid;
use dg_vm::dispatch::DispatchHost;

use super::EcsHost;

impl DispatchHost for EcsHost<'_> {
    fn disp_free_proto_script(&mut self, _uid: Uid) {}
}
