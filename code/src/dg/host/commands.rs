//! `CmdHost`: the effects of script commands.
//!
//! Every method is unmodelled for now and answers the neutral value
//! `HOST.md` lists for it.

use dg_vm::Uid;
use dg_vm::commands::{CmdHost, ExitField};

use super::EcsHost;

impl CmdHost for EcsHost<'_> {
    fn cmd_huh(&self) -> String {
        "Huh?!?\r\n".to_owned()
    }

    fn cmd_send(&mut self, _ch: Uid, _text: &str) {}

    fn cmd_desc(&self, _ch: Uid) -> Option<Option<Uid>> {
        None
    }

    fn cmd_mudlog(&mut self, _log_type: i32, _level: i32, _text: &str) {}

    fn cmd_syslog(&mut self, _text: &str) {}

    fn cmd_playing_characters(&self) -> Vec<Uid> {
        Vec::new()
    }

    fn cmd_is_charmed(&self, _ch: Uid) -> bool {
        false
    }

    fn cmd_invis_level(&self, _ch: Uid) -> i32 {
        0
    }

    fn cmd_die(&mut self, _ch: Uid) {}

    fn cmd_stop_fighting(&mut self, _ch: Uid) {}

    fn cmd_hit_victim(&mut self, _ch: Uid, _victim: Uid) {}

    fn cmd_set_hunting(&mut self, _ch: Uid, _victim: Uid) {}

    fn cmd_unfollow(&mut self, _ch: Uid) {}

    fn cmd_follow(&mut self, _ch: Uid, _leader: Uid) {}

    fn cmd_exit_purge(&mut self, _room: Uid, _dir: usize) {}

    fn cmd_exit_set(&mut self, _room: Uid, _dir: usize, _field: ExitField) {}

    fn cmd_house_can_enter(&self, _ch: Uid, _room_vnum: i32) -> bool {
        true
    }

    fn cmd_zone_of_vnum(&self, _vnum: i32) -> Option<i32> {
        None
    }

    fn cmd_rooms_in_range(&self, _start: i32, _finish: i32) -> Vec<Uid> {
        Vec::new()
    }

    fn cmd_char_from_room(&mut self, _ch: Uid) {}

    fn cmd_char_to_room(&mut self, _ch: Uid, _room: Uid) {}

    fn cmd_obj_to_room(&mut self, _obj: Uid, _room: Uid) {}

    fn cmd_obj_to_char(&mut self, _obj: Uid, _ch: Uid) {}

    fn cmd_obj_to_obj(&mut self, _obj: Uid, _container: Uid) {}

    fn cmd_obj_detach(&mut self, _obj: Uid) {}

    fn cmd_equip_char(&mut self, _ch: Uid, _obj: Uid, _pos: i32) {}

    fn cmd_unequip_char(&mut self, _ch: Uid, _pos: i32) -> Option<Uid> {
        None
    }

    fn cmd_extract_char(&mut self, _ch: Uid) {}

    fn cmd_extract_obj(&mut self, _obj: Uid) {}

    fn cmd_read_mobile(&mut self, _vnum: i32) -> Option<Uid> {
        None
    }

    fn cmd_read_object(&mut self, _vnum: i32) -> Option<Uid> {
        None
    }

    fn cmd_overwrite_mob(&mut self, _ch: Uid, _from: Uid, _keep_hp: bool) {}

    fn cmd_overwrite_obj(&mut self, _obj: Uid, _from: Uid) {}
}
