//! `ObjHost`: object attributes.
//!
//! Every method is unmodelled for now and answers the neutral value
//! `HOST.md` lists for it.

use dg_vm::Uid;
use dg_vm::fields_obj::ObjHost;

use super::EcsHost;

impl ObjHost for EcsHost<'_> {
    fn obj_val(&self, _obj: Uid, _idx: usize) -> i32 {
        0
    }

    fn obj_set_val(&mut self, _obj: Uid, _idx: usize, _value: i32) {}

    fn obj_timer(&self, _obj: Uid) -> i32 {
        0
    }

    fn obj_set_timer(&mut self, _obj: Uid, _value: i32) {}

    fn obj_cost(&self, _obj: Uid) -> i32 {
        0
    }

    fn obj_set_cost(&mut self, _obj: Uid, _value: i32) {}

    fn obj_rent(&self, _obj: Uid) -> i32 {
        0
    }

    fn obj_set_rent(&mut self, _obj: Uid, _value: i32) {}

    fn obj_weight(&self, _obj: Uid) -> i32 {
        0
    }

    fn obj_set_weight(&mut self, _obj: Uid, _value: i32) {}

    fn obj_extra_flagged(&self, _obj: Uid, _bit: usize) -> bool {
        false
    }

    fn obj_affect_flagged(&self, _obj: Uid, _bit: usize) -> bool {
        false
    }

    fn obj_apply(&self, _obj: Uid, _slot: usize) -> (u8, i8) {
        (0, 0)
    }

    fn obj_set_apply(&mut self, _obj: Uid, _slot: usize, _location: u8, _modifier: i8) {}

    fn obj_set_alias(&mut self, _obj: Uid, _name: &str) {}

    fn obj_set_short_desc(&mut self, _obj: Uid, _text: &str) {}

    fn obj_set_long_desc(&mut self, _obj: Uid, _text: &str) {}
}
