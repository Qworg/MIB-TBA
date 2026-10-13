//! [`CmdHost`] for the mock.
//!
//! Uses the shared keys of [`super`] (`pos`, `sex`, `hit`, `max_hit`,
//! `level`, `zone`, `val0`..`val3`, `timer`, `exit:<dir>…`, `flag:<n>`,
//! `links["fighting"]`, `links["master"]`, `lists["followers"]`,
//! `aff:<n>`, `mobproto:<vnum>`, `objproto:<vnum>`) and these:
//!
//! | entity | key | meaning (C) |
//! |---|---|---|
//! | char | `nums["desc"]` | `ch->desc != NULL`; `send_to_char` only records for such a character |
//! | char | `nums["desc_original_level"]` | `GET_LEVEL(ch->desc->original)` (default 0) |
//! | char | `nums["writing"]` | `PLR_FLAGGED(ch, PLR_WRITING)` |
//! | char | `nums["playing"]` | on `descriptor_list` with `CON_PLAYING` |
//! | char | `nums["color"]` | `COLOR_LEV(ch)` |
//! | char | `nums["nohassle"]`, `nums["invis_level"]` | `PRF_NOHASSLE`, `GET_INVIS_LEV` |
//! | char | `links["hunting"]` | `HUNTING(ch)` |
//! | room | `nums["exit:<dir>:nowhere"]` | the exit exists and leads to `NOWHERE` |
//! | world | `texts["huh"]` | `CONFIG_HUH` (default `Huh?!?\r\n`) |
//! | world | `nums["dir_count"]` | `DIR_COUNT` (default 6) |
//! | world | `nums["house_enter:<vnum>"]` | `House_can_enter(ch, vnum)` |
//! | world | `nums["objproto:<vnum>:wear"]`, `nums["objproto:<vnum>:type"]` | a loadable object's wear flags and type |
//! | world | `nums["zone:<n>:bot"]`, `nums["zone:<n>:top"]` | a zone's vnum range |
//!
//! Effects recorded with [`MockHost::effect`]: `send <ch>: <text>`,
//! `act_trigger <to> <actor>: <text>`, `mudlog <level>: <text>`,
//! `syslog: <text>`, `die <ch>`, `hit <ch> <victim>`, `hunt <ch> <victim>`,
//! `extract_char <ch>`, `extract_obj <obj>`, `transform_mob <ch> <vnum>
//! <keep_hp>`, `transform_obj <obj> <vnum>`.

use crate::commands::{CmdHost, ExitField};
use crate::host::WorldHost;
use crate::mock::MockHost;
use crate::types::{Kind, Uid};

impl MockHost {
    fn exit_key(dir: usize, suffix: &str) -> String {
        if suffix.is_empty() {
            format!("exit:{dir}")
        } else {
            format!("exit:{dir}:{suffix}")
        }
    }

    fn has_exit(&self, room: Uid, dir: usize) -> bool {
        self.link(room, &Self::exit_key(dir, "")).is_some()
            || self.num(room, &Self::exit_key(dir, "nowhere")) != 0
    }

    /// The room `CREATE` leaves a fresh exit pointing at: room number 0, the
    /// first room of the world table (here the lowest-numbered room).
    fn first_room(&self) -> Option<Uid> {
        self.entities
            .iter()
            .find(|(_, entity)| entity.kind == Kind::Room)
            .map(|(uid, _)| *uid)
    }
}

impl CmdHost for MockHost {
    fn cmd_huh(&self) -> String {
        self.globals
            .texts
            .get("huh")
            .cloned()
            .unwrap_or_else(|| "Huh?!?\r\n".to_owned())
    }

    fn cmd_send(&mut self, ch: Uid, text: &str) {
        if self.num(ch, "desc") != 0 {
            self.effect(format!("send {ch}: {text}"));
        }
    }

    fn cmd_has_desc(&self, ch: Uid) -> bool {
        self.num(ch, "desc") != 0
    }

    fn cmd_desc_original_level(&self, ch: Uid) -> Option<i32> {
        self.cmd_has_desc(ch)
            .then(|| i32::try_from(self.num(ch, "desc_original_level")).unwrap_or(0))
    }

    fn cmd_is_writing(&self, ch: Uid) -> bool {
        self.num(ch, "writing") != 0
    }

    fn cmd_act_trigger(&mut self, to: Uid, text: &str, actor: Uid) {
        self.effect(format!("act_trigger {to} {actor}: {text}"));
    }

    fn cmd_mudlog(&mut self, level: i32, text: &str) {
        self.effect(format!("mudlog {level}: {text}"));
    }

    fn cmd_syslog(&mut self, text: &str) {
        self.effect(format!("syslog: {text}"));
    }

    fn cmd_playing_characters(&self) -> Vec<Uid> {
        self.character_list
            .iter()
            .copied()
            .filter(|ch| self.num(*ch, "playing") != 0)
            .collect()
    }

    fn cmd_pos(&self, ch: Uid) -> i32 {
        i32::try_from(self.num(ch, "pos")).unwrap_or(0)
    }

    fn cmd_sex(&self, ch: Uid) -> i32 {
        i32::try_from(self.num(ch, "sex")).unwrap_or(0)
    }

    fn cmd_color_level(&self, ch: Uid) -> i32 {
        i32::try_from(self.num(ch, "color")).unwrap_or(0)
    }

    fn cmd_is_charmed(&self, ch: Uid) -> bool {
        self.num(ch, "aff:22") != 0
    }

    fn cmd_nohassle(&self, ch: Uid) -> bool {
        self.num(ch, "nohassle") != 0
    }

    fn cmd_invis_level(&self, ch: Uid) -> i32 {
        i32::try_from(self.num(ch, "invis_level")).unwrap_or(0)
    }

    fn cmd_hit(&self, ch: Uid) -> i32 {
        i32::try_from(self.num(ch, "hit")).unwrap_or(0)
    }

    fn cmd_max_hit(&self, ch: Uid) -> i32 {
        i32::try_from(self.num(ch, "max_hit")).unwrap_or(0)
    }

    fn cmd_set_hit(&mut self, ch: Uid, hit: i32) {
        self.set_num(ch, "hit", i64::from(hit));
    }

    fn cmd_update_pos(&mut self, ch: Uid) {
        let hit = self.cmd_hit(ch);
        let pos = self.cmd_pos(ch);
        let new = if hit > 0 && pos > 3 {
            return;
        } else if hit > 0 {
            8
        } else if hit <= -11 {
            0
        } else if hit <= -6 {
            1
        } else if hit <= -3 {
            2
        } else {
            3
        };
        self.set_num(ch, "pos", new);
    }

    fn cmd_die(&mut self, ch: Uid) {
        self.effect(format!("die {ch}"));
    }

    fn cmd_fighting(&self, ch: Uid) -> Option<Uid> {
        self.link(ch, "fighting")
    }

    fn cmd_stop_fighting(&mut self, ch: Uid) {
        self.set_link(ch, "fighting", None);
    }

    fn cmd_hit_victim(&mut self, ch: Uid, victim: Uid) {
        self.effect(format!("hit {ch} {victim}"));
        self.set_link(ch, "fighting", Some(victim));
    }

    fn cmd_set_hunting(&mut self, ch: Uid, victim: Uid) {
        self.effect(format!("hunt {ch} {victim}"));
        self.set_link(ch, "hunting", Some(victim));
    }

    fn cmd_master(&self, ch: Uid) -> Option<Uid> {
        self.link(ch, "master")
    }

    fn cmd_unfollow(&mut self, ch: Uid) {
        if let Some(master) = self.link(ch, "master") {
            self.list_remove(master, "followers", ch);
        }
        self.set_link(ch, "master", None);
    }

    fn cmd_follow(&mut self, ch: Uid, leader: Uid) {
        self.set_link(ch, "master", Some(leader));
        self.list_push_front(leader, "followers", ch);
    }

    fn cmd_dir_count(&self) -> usize {
        self.globals
            .nums
            .get("dir_count")
            .and_then(|count| usize::try_from(*count).ok())
            .unwrap_or(6)
    }

    fn cmd_exit_to_room(&self, room: Uid, dir: usize) -> Option<Uid> {
        self.link(room, &Self::exit_key(dir, ""))
    }

    fn cmd_exit_purge(&mut self, room: Uid, dir: usize) {
        self.set_link(room, &Self::exit_key(dir, ""), None);
        if let Some(entity) = self.entity_mut(room) {
            for suffix in ["nowhere", "flags", "key"] {
                entity.nums.remove(&Self::exit_key(dir, suffix));
            }
            for suffix in ["keyword", "desc"] {
                entity.texts.remove(&Self::exit_key(dir, suffix));
            }
        }
    }

    fn cmd_exit_set(&mut self, room: Uid, dir: usize, field: ExitField) {
        if !self.has_exit(room, dir) {
            let first = self.first_room();
            self.set_link(room, &Self::exit_key(dir, ""), first);
        }
        match field {
            ExitField::Description(text) => {
                self.set_text(room, &Self::exit_key(dir, "desc"), &text);
            }
            ExitField::Flags(flags) => {
                self.set_num(room, &Self::exit_key(dir, "flags"), i64::from(flags));
            }
            ExitField::Key(key) => self.set_num(room, &Self::exit_key(dir, "key"), i64::from(key)),
            ExitField::Keyword(text) => {
                self.set_text(room, &Self::exit_key(dir, "keyword"), &text);
            }
            ExitField::ToRoom(to) => {
                self.set_link(room, &Self::exit_key(dir, ""), to);
                self.set_num(
                    room,
                    &Self::exit_key(dir, "nowhere"),
                    i64::from(to.is_none()),
                );
            }
        }
    }

    fn cmd_room_flagged(&self, room: Uid, flag: i32) -> bool {
        self.num(room, &format!("flag:{flag}")) != 0
    }

    fn cmd_house_can_enter(&self, _ch: Uid, room_vnum: i32) -> bool {
        self.globals
            .nums
            .get(&format!("house_enter:{room_vnum}"))
            .is_some_and(|allowed| *allowed != 0)
    }

    fn cmd_zone_of_vnum(&self, vnum: i32) -> Option<i32> {
        (0..64).find(|zone| {
            let bot = self.globals.nums.get(&format!("zone:{zone}:bot"));
            let top = self.globals.nums.get(&format!("zone:{zone}:top"));
            matches!((bot, top), (Some(bot), Some(top))
                if *bot <= i64::from(vnum) && i64::from(vnum) <= *top)
        })
    }

    fn cmd_room_zone(&self, room: Uid) -> i32 {
        i32::try_from(self.num(room, "zone")).unwrap_or(0)
    }

    fn cmd_rooms_in_range(&self, start: i32, finish: i32) -> Vec<Uid> {
        let mut rooms: Vec<(i32, Uid)> = self
            .entities
            .iter()
            .filter(|(_, entity)| entity.kind == Kind::Room)
            .map(|(uid, _)| (self.vnum(*uid), *uid))
            .filter(|(vnum, _)| start <= *vnum && *vnum <= finish)
            .collect();
        rooms.sort();
        rooms.into_iter().map(|(_, uid)| uid).collect()
    }

    fn cmd_char_from_room(&mut self, ch: Uid) {
        self.char_from_room(ch);
    }

    fn cmd_char_to_room(&mut self, ch: Uid, room: Uid) {
        self.char_to_room(ch, room);
    }

    fn cmd_obj_to_room(&mut self, obj: Uid, room: Uid) {
        self.obj_to_room(obj, room);
    }

    fn cmd_obj_to_char(&mut self, obj: Uid, ch: Uid) {
        self.obj_to_char(obj, ch);
    }

    fn cmd_obj_to_obj(&mut self, obj: Uid, container: Uid) {
        self.obj_to_obj(obj, container);
    }

    fn cmd_obj_detach(&mut self, obj: Uid) {
        self.obj_from_anywhere(obj);
    }

    fn cmd_equip_char(&mut self, ch: Uid, obj: Uid, pos: i32) {
        self.equip_char(ch, obj, pos);
    }

    fn cmd_unequip_char(&mut self, ch: Uid, pos: i32) -> Option<Uid> {
        let obj = self.equipment(ch, pos)?;
        self.obj_from_anywhere(obj);
        Some(obj)
    }

    fn cmd_extract_char(&mut self, ch: Uid) {
        self.effect(format!("extract_char {ch}"));
        self.extract_char(ch);
    }

    fn cmd_extract_obj(&mut self, obj: Uid) {
        self.effect(format!("extract_obj {obj}"));
        self.extract_obj(obj);
    }

    fn cmd_read_mobile(&mut self, vnum: i32) -> Option<Uid> {
        let name = self.globals.texts.get(&format!("mobproto:{vnum}"))?.clone();
        let mob = self.add_mob(vnum, &name);
        self.set_num(mob, "pos", 8);
        Some(mob)
    }

    fn cmd_read_object(&mut self, vnum: i32) -> Option<Uid> {
        let name = self.globals.texts.get(&format!("objproto:{vnum}"))?.clone();
        let obj = self.add_obj(vnum, &name);
        for key in ["wear", "type"] {
            let value = self
                .globals
                .nums
                .get(&format!("objproto:{vnum}:{key}"))
                .copied()
                .unwrap_or(0);
            self.set_num(obj, key, value);
        }
        Some(obj)
    }

    fn cmd_overwrite_mob(&mut self, ch: Uid, from: Uid, keep_hp: bool) {
        let vnum = self.vnum(from);
        let Some(name) = self.globals.texts.get(&format!("mobproto:{vnum}")).cloned() else {
            return;
        };
        self.effect(format!("transform_mob {ch} {vnum} {keep_hp}"));
        // `ch->nr` is restored, so the vnum stays.
        self.set_text(ch, "name", &name);
        self.set_text(ch, "alias", &name);
    }

    fn cmd_overwrite_obj(&mut self, obj: Uid, from: Uid) {
        let vnum = self.vnum(from);
        let Some(name) = self.globals.texts.get(&format!("objproto:{vnum}")).cloned() else {
            return;
        };
        self.effect(format!("transform_obj {obj} {vnum}"));
        self.set_num(obj, "vnum", i64::from(vnum));
        self.set_text(obj, "name", &name);
        self.set_text(obj, "alias", &name);
    }

    fn cmd_set_obj_val(&mut self, obj: Uid, position: usize, value: i32) {
        self.set_num(obj, &format!("val{position}"), i64::from(value));
    }

    fn cmd_set_obj_timer(&mut self, obj: Uid, timer: i32) {
        self.set_num(obj, "timer", i64::from(timer));
    }
}
