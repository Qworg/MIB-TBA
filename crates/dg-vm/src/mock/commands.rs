//! [`CmdHost`] for the mock.
//!
//! Uses the shared keys of [`super`] (`desc`, `con_state`, `level`, `zone`,
//! `exit:<dir>…` including `exit:<dir>:nowhere`, `links["room"]`,
//! `links["master"]`, `lists["followers"]`, `aff:<n>`, `mobproto:<vnum>`,
//! `objproto:<vnum>`, `val0`..`val3`, `timer`) and these, which no other
//! module needs:
//!
//! | entity | key | meaning (C) |
//! |---|---|---|
//! | char | `links["desc_original"]` | `ch->desc->original`; absent = the descriptor is not switched |
//! | char | `nums["invis_level"]` | `GET_INVIS_LEV` |
//! | char | `links["hunting"]` | `HUNTING(ch)` |
//! | world | `texts["huh"]` | `CONFIG_HUH` (default `Huh?!?\r\n`) |
//! | world | `nums["house_enter:<vnum>"]` | `House_can_enter(ch, vnum)` |
//! | world | `nums["objproto:<vnum>:wear"]`, `nums["objproto:<vnum>:type"]` | a loadable object's wear flags and type |
//! | world | `nums["zone:<n>:bot"]`, `nums["zone:<n>:top"]` | a zone's vnum range |
//! | world | `texts["hook:send:<ch>"]`, `texts["hook:arrive:<ch>"]` | a test hook, run once and removed: after the next text delivered to `<ch>` (what an `act` trigger of `<ch>` would do) or after the next time `<ch>` is put into a room (what the command of an `at` would do). `load <mob vnum>` puts a new mob at the head of `<ch>`'s room, `move <room vnum>` takes `<ch>` to that room |
//! | world | `nums["mobtrig:<vnum>"]`, `nums["objtrig:<vnum>"]` | a trigger (a `protos` key) that `read_mobile` / `read_object` attach to what they build, as the prototype's trigger list does |
//!
//! A character is on `descriptor_list` playing when it has `desc` and its
//! `con_state` is 0. The facts that other traits own are read through them:
//! `PLR_WRITING` is bit 4 of `nums["act"]`, `PRF_NOHASSLE` bit 8 and the colour
//! bits 13 and 14 of `nums["pref"]` (see `mock/chars.rs`), the direction count
//! follows `config:diagonal_dirs`.
//!
//! Effects recorded with [`MockHost::effect`]: `send <ch>: <text>`,
//! `mudlog <level>: <text>`, `syslog: <text>`, `die <ch>`, `hit <ch>
//! <victim>`, `hunt <ch> <victim>`, `extract_char <ch>`, `extract_obj <obj>`,
//! `transform_mob <ch> <vnum> <keep_hp>`, `transform_obj <obj> <vnum>`.

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

    /// Run the one-shot test hook `key` for `ch` (see the module table).
    fn run_hook(&mut self, key: &str, ch: Uid) {
        let Some(spec) = self.globals.texts.remove(key) else {
            return;
        };
        let mut words = spec.split_whitespace();
        let (Some(action), Some(vnum)) = (words.next(), words.next()) else {
            return;
        };
        let Ok(vnum) = vnum.parse::<i32>() else {
            return;
        };
        match action {
            "load" => {
                if let (Some(mob), Some(room)) = (self.cmd_read_mobile(vnum), self.in_room(ch)) {
                    self.char_to_room(mob, room);
                }
            }
            "move" => {
                if let Some(room) = self.real_room(vnum) {
                    self.char_from_room(ch);
                    self.char_to_room(ch, room);
                }
            }
            _ => {}
        }
    }

    /// Attach the trigger the world key names (a vnum in `protos`) to `uid`.
    fn attach_proto_trigger(&mut self, uid: Uid, key: &str) {
        let proto = self
            .globals
            .nums
            .get(key)
            .and_then(|vnum| i32::try_from(*vnum).ok())
            .and_then(|vnum| self.protos.get(&vnum).cloned());
        if let Some(proto) = proto {
            self.attach(uid, &proto);
        }
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
            self.run_hook(&format!("hook:send:{ch}"), ch);
        }
    }

    fn cmd_desc(&self, ch: Uid) -> Option<Option<Uid>> {
        (self.num(ch, "desc") != 0).then(|| self.link(ch, "desc_original"))
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
            .filter(|ch| self.num(*ch, "desc") != 0 && self.num(*ch, "con_state") == 0)
            .collect()
    }

    fn cmd_is_charmed(&self, ch: Uid) -> bool {
        self.num(ch, "aff:22") != 0
    }

    fn cmd_invis_level(&self, ch: Uid) -> i32 {
        i32::try_from(self.num(ch, "invis_level")).unwrap_or(0)
    }

    fn cmd_die(&mut self, ch: Uid) {
        self.effect(format!("die {ch}"));
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

    fn cmd_house_can_enter(&self, _ch: Uid, room_vnum: i32) -> bool {
        self.globals
            .nums
            .get(&format!("house_enter:{room_vnum}"))
            .is_some_and(|allowed| *allowed != 0)
    }

    fn cmd_zone_of_vnum(&self, vnum: i32) -> Option<i32> {
        let mut zones: Vec<i32> = self
            .globals
            .nums
            .keys()
            .filter_map(|key| {
                key.strip_prefix("zone:")?
                    .strip_suffix(":bot")?
                    .parse()
                    .ok()
            })
            .collect();
        zones.sort_unstable();
        zones.into_iter().find(|zone| {
            let bot = self.globals.nums.get(&format!("zone:{zone}:bot"));
            let top = self.globals.nums.get(&format!("zone:{zone}:top"));
            matches!((bot, top), (Some(bot), Some(top))
                if *bot <= i64::from(vnum) && i64::from(vnum) <= *top)
        })
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
        self.run_hook(&format!("hook:arrive:{ch}"), ch);
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
        self.attach_proto_trigger(mob, &format!("mobtrig:{vnum}"));
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
        self.attach_proto_trigger(obj, &format!("objtrig:{vnum}"));
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
