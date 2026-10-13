//! [`ObjHost`] for the mock.
//!
//! Beyond the shared keys (`type`, `val0`..`val3`, `timer`, `cost`, `weight`,
//! `alias`, `name`), this file reads and writes, on an object:
//!
//! | key | meaning (C) |
//! |---|---|
//! | `nums["rent"]` | `GET_OBJ_RENT` (`cost_per_day`) |
//! | `nums["extra:<n>"]` | `IS_SET_AR(GET_OBJ_EXTRA(obj), n)` |
//! | `nums["affect:<n>"]` | `IS_SET_AR(GET_OBJ_AFFECT(obj), n)` |
//! | `nums["apply:<slot>:loc"]`, `nums["apply:<slot>:mod"]` | `obj->affected[slot].location` / `.modifier` |
//! | `texts["longdesc"]` | `obj->description` |
//!
//! `oset alias` writes `texts["alias"]`, `oset shortdesc` writes
//! `texts["name"]`. Every write also records an effect.

use crate::fields_obj::ObjHost;
use crate::mock::MockHost;
use crate::types::Uid;

fn to_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(0)
}

impl ObjHost for MockHost {
    fn obj_val(&self, obj: Uid, idx: usize) -> i32 {
        to_i32(self.num(obj, &format!("val{idx}")))
    }

    fn obj_timer(&self, obj: Uid) -> i32 {
        to_i32(self.num(obj, "timer"))
    }

    fn obj_cost(&self, obj: Uid) -> i32 {
        to_i32(self.num(obj, "cost"))
    }

    fn obj_set_cost(&mut self, obj: Uid, value: i32) {
        self.set_num(obj, "cost", i64::from(value));
        self.effect(format!("obj {obj} cost {value}"));
    }

    fn obj_rent(&self, obj: Uid) -> i32 {
        to_i32(self.num(obj, "rent"))
    }

    fn obj_set_rent(&mut self, obj: Uid, value: i32) {
        self.set_num(obj, "rent", i64::from(value));
        self.effect(format!("obj {obj} rent {value}"));
    }

    fn obj_weight(&self, obj: Uid) -> i32 {
        to_i32(self.num(obj, "weight"))
    }

    fn obj_set_weight(&mut self, obj: Uid, value: i32) {
        self.set_num(obj, "weight", i64::from(value));
        self.effect(format!("obj {obj} weight {value}"));
    }

    fn obj_extra_flagged(&self, obj: Uid, bit: usize) -> bool {
        self.num(obj, &format!("extra:{bit}")) != 0
    }

    fn obj_affect_flagged(&self, obj: Uid, bit: usize) -> bool {
        self.num(obj, &format!("affect:{bit}")) != 0
    }

    fn obj_apply(&self, obj: Uid, slot: usize) -> (u8, i8) {
        (
            u8::try_from(self.num(obj, &format!("apply:{slot}:loc"))).unwrap_or(0),
            i8::try_from(self.num(obj, &format!("apply:{slot}:mod"))).unwrap_or(0),
        )
    }

    fn obj_set_apply(&mut self, obj: Uid, slot: usize, location: u8, modifier: i8) {
        self.set_num(obj, &format!("apply:{slot}:loc"), i64::from(location));
        self.set_num(obj, &format!("apply:{slot}:mod"), i64::from(modifier));
        self.effect(format!("obj {obj} apply {slot} {location} {modifier}"));
    }

    fn obj_set_alias(&mut self, obj: Uid, name: &str) {
        self.set_text(obj, "alias", name);
        self.effect(format!("obj {obj} alias {name}"));
    }

    fn obj_set_short_desc(&mut self, obj: Uid, text: &str) {
        self.set_text(obj, "name", text);
        self.effect(format!("obj {obj} shortdesc {text}"));
    }

    fn obj_set_long_desc(&mut self, obj: Uid, text: &str) {
        self.set_text(obj, "longdesc", text);
        self.effect(format!("obj {obj} longdesc {text}"));
    }
}
