//! `CmdHost`: the effects of script commands.
//!
//! The VM parses, resolves targets and words the messages; these methods only
//! change the world. The work is split by theme into the submodules below
//! (free functions over the `World`); the methods here resolve script ids,
//! keep the list-order stamps and report what the pack does not model.
//!
//! What the pack models differently from the C:
//!
//! - A character is never in a "no room" state for long: `char_from_room`
//!   removes its `Location` and the caller puts it back.
//! - Objects have no state of their own beyond their place, so an object
//!   taken out of its place (`cmd_obj_detach`, `cmd_unequip_char`) is simply
//!   in none until placed again.
//! - Read facts the pack does not model are answered without being counted
//!   by `BaseHost::unsupported`, which needs `&mut self`.
//! - There is no charm, invisibility level, house or descriptor switching:
//!   `cmd_is_charmed` and `cmd_invis_level` answer `false` and `0`,
//!   `cmd_house_can_enter` always allows entry, and a descriptor is a
//!   playing player.

#[path = "cmd_exits.rs"]
mod exits;
#[path = "cmd_fight.rs"]
mod fight;
#[path = "cmd_place.rs"]
mod place;

// Shared with the sibling hosts (`room_exit` must consult `DgNowhereExits`).
#[allow(unused_imports)]
pub use exits::{DgNowhereExits, DgPendingDoors};
#[allow(unused_imports)]
pub use fight::DgHunting;

use dg_vm::commands::{CmdHost, ExitField};
use dg_vm::{BaseHost, Uid};
use mib_core::prelude::*;
use mib_stdlib::prelude::{Player, Room};

use super::{EcsHost, wear_slot};
use crate::dg::{out, script};
use crate::module::TbaContent;

impl EcsHost<'_> {
    /// Stamp an entity the host just moved, so list order follows the move.
    fn moved(&mut self, entity: Entity) {
        self.stamp_order(entity);
    }

    /// Give a mob or object that was just created its id and prototype
    /// scripts, without firing any trigger.
    fn start_scripts(&mut self, entity: Entity) -> Option<Uid> {
        script::init_entity(self.world, entity)
    }
}

impl CmdHost for EcsHost<'_> {
    /// `CONFIG_HUH`.
    fn cmd_huh(&self) -> String {
        "Huh?!?\r\n".to_owned()
    }

    fn cmd_send(&mut self, ch: Uid, text: &str) {
        if let Some(entity) = self.entity(ch) {
            out::send_to_char(self.world, entity, text);
        }
    }

    /// A playing player has a descriptor; no one is ever switched into a mob.
    fn cmd_desc(&self, ch: Uid) -> Option<Option<Uid>> {
        let entity = self.entity(ch)?;
        out::is_playing(self.world, entity).then_some(None)
    }

    /// Logged and counted under `mudlog`; the pack has no immortals to show it
    /// to, so `level` only labels the line.
    fn cmd_mudlog(&mut self, log_type: i32, level: i32, text: &str) {
        let line = format!("mudlog {log_type} {level}: {text}");
        place::log_line(self.world, "mudlog", line);
    }

    fn cmd_syslog(&mut self, text: &str) {
        place::log_line(self.world, "syslog", format!("syslog: {text}"));
    }

    /// In `character_list` order (newest first); only players can play, so
    /// the NPCs are never visited.
    fn cmd_playing_characters(&self) -> Vec<Uid> {
        let playing = self
            .all_with::<Player>()
            .into_iter()
            .filter(|entity| out::is_playing(self.world, *entity));
        self.ordered(playing, Self::birth, true)
    }

    /// The pack has no charm affect (an item's charm flag is data only).
    fn cmd_is_charmed(&self, _ch: Uid) -> bool {
        false
    }

    /// The pack has no immortal invisibility level.
    fn cmd_invis_level(&self, _ch: Uid) -> i32 {
        0
    }

    fn cmd_die(&mut self, ch: Uid) {
        if let Some(entity) = self.entity(ch) {
            fight::die(self.world, entity);
        }
    }

    fn cmd_stop_fighting(&mut self, ch: Uid) {
        if let Some(entity) = self.entity(ch) {
            fight::stop_fighting(self.world, entity);
        }
    }

    fn cmd_hit_victim(&mut self, ch: Uid, victim: Uid) {
        if let (Some(ch), Some(victim)) = (self.entity(ch), self.entity(victim)) {
            fight::hit_victim(self.world, ch, victim);
        }
    }

    fn cmd_set_hunting(&mut self, ch: Uid, victim: Uid) {
        if let (Some(ch), Some(victim)) = (self.entity(ch), self.entity(victim))
            && fight::set_hunting(self.world, ch, victim)
        {
            self.unsupported(
                "mhunt",
                "the pack's mobs do not track a character by hunting",
            );
        }
    }

    fn cmd_unfollow(&mut self, ch: Uid) {
        if let Some(entity) = self.entity(ch) {
            fight::unfollow(self.world, entity);
        }
    }

    fn cmd_follow(&mut self, ch: Uid, leader: Uid) {
        if let (Some(ch), Some(leader)) = (self.entity(ch), self.entity(leader)) {
            fight::follow(self.world, ch, leader);
        }
    }

    fn cmd_exit_purge(&mut self, room: Uid, dir: usize) {
        if let Some(room) = self.entity(room) {
            exits::purge(self.world, room, dir);
        }
    }

    fn cmd_exit_set(&mut self, room: Uid, dir: usize, field: ExitField) {
        let Some(room) = self.entity(room) else {
            return;
        };
        let field = match field {
            ExitField::Description(_) => exits::Field::Description,
            ExitField::Flags(flags) => exits::Field::Flags(flags),
            ExitField::Key(key) => exits::Field::Key(key),
            ExitField::Keyword(text) => exits::Field::Keyword(text),
            ExitField::ToRoom(None) => exits::Field::ToRoom(None),
            ExitField::ToRoom(Some(target)) => match self.entity(target) {
                Some(target) => exits::Field::ToRoom(Some(target)),
                None => return,
            },
        };
        if let Some((what, detail)) = exits::set(self.world, room, dir, field) {
            self.unsupported(what, detail);
        }
    }

    /// The pack has no player houses.
    fn cmd_house_can_enter(&self, _ch: Uid, _room_vnum: i32) -> bool {
        true
    }

    fn cmd_zone_of_vnum(&self, vnum: i32) -> Option<i32> {
        let vnum = u32::try_from(vnum).ok()?;
        let (number, _) = self
            .world
            .get_resource::<TbaContent>()?
            .zones
            .iter()
            .find(|(_, zone)| zone.bot <= vnum && vnum <= zone.top)?;
        i32::try_from(*number).ok()
    }

    fn cmd_rooms_in_range(&self, start: i32, finish: i32) -> Vec<Uid> {
        let Some(mut rooms) = self.world.try_query::<(Entity, &Room)>() else {
            return Vec::new();
        };
        let mut found: Vec<(u32, Uid)> = rooms
            .iter(self.world)
            .filter(|(_, room)| i64::from(start) <= i64::from(room.num))
            .filter(|(_, room)| i64::from(room.num) <= i64::from(finish))
            .filter_map(|(entity, room)| Some((room.num, self.uid(entity)?)))
            .collect();
        found.sort_unstable();
        found.into_iter().map(|(_, uid)| uid).collect()
    }

    fn cmd_char_from_room(&mut self, ch: Uid) {
        if let Some(entity) = self.entity(ch) {
            place::char_from_room(self.world, entity);
        }
    }

    fn cmd_char_to_room(&mut self, ch: Uid, room: Uid) {
        if let (Some(ch), Some(room)) = (self.entity(ch), self.entity(room))
            && place::char_to_room(self.world, ch, room)
        {
            self.moved(ch);
        }
    }

    fn cmd_obj_to_room(&mut self, obj: Uid, room: Uid) {
        if let (Some(obj), Some(room)) = (self.entity(obj), self.entity(room))
            && place::obj_to_room(self.world, obj, room)
        {
            self.moved(obj);
        }
    }

    fn cmd_obj_to_char(&mut self, obj: Uid, ch: Uid) {
        if let (Some(obj), Some(ch)) = (self.entity(obj), self.entity(ch))
            && place::obj_to_char(self.world, obj, ch)
        {
            self.moved(obj);
        }
    }

    fn cmd_obj_to_obj(&mut self, obj: Uid, container: Uid) {
        if let (Some(obj), Some(container)) = (self.entity(obj), self.entity(container))
            && place::obj_to_obj(self.world, obj, container)
        {
            self.moved(obj);
        }
    }

    fn cmd_obj_detach(&mut self, obj: Uid) {
        if let Some(entity) = self.entity(obj) {
            place::lift_item(self.world, entity);
        }
    }

    fn cmd_equip_char(&mut self, ch: Uid, obj: Uid, pos: i32) {
        let (Some(ch), Some(obj)) = (self.entity(ch), self.entity(obj)) else {
            return;
        };
        let Some(slot) = wear_slot(pos) else {
            self.cmd_syslog(&format!("SYSERR: equip_char: bad wear position {pos}"));
            return;
        };
        if place::equip_char(self.world, ch, obj, slot) {
            self.moved(obj);
        } else {
            self.cmd_syslog("SYSERR: Char is already equipped, or not a char or object");
        }
    }

    fn cmd_unequip_char(&mut self, ch: Uid, pos: i32) -> Option<Uid> {
        let ch = self.entity(ch)?;
        let obj = place::unequip_char(self.world, ch, wear_slot(pos)?)?;
        self.moved(obj);
        self.uid(obj)
    }

    fn cmd_extract_char(&mut self, ch: Uid) {
        if let Some(entity) = self.entity(ch) {
            place::extract_char(self.world, entity);
        }
    }

    fn cmd_extract_obj(&mut self, obj: Uid) {
        let Some(entity) = self.entity(obj) else {
            return;
        };
        let doomed = place::obj_tree(self.world, entity);
        let uids: Vec<Uid> = doomed.iter().filter_map(|gone| self.uid(*gone)).collect();
        place::extract_obj(self.world, &doomed);
        place::forget_waits(self.world, &uids);
    }

    fn cmd_read_mobile(&mut self, vnum: i32) -> Option<Uid> {
        let mob = place::read_mobile(self.world, vnum)?;
        self.start_scripts(mob)
    }

    fn cmd_read_object(&mut self, vnum: i32) -> Option<Uid> {
        let obj = place::read_object(self.world, vnum)?;
        self.start_scripts(obj)
    }

    fn cmd_overwrite_mob(&mut self, ch: Uid, from: Uid, keep_hp: bool) {
        if let (Some(ch), Some(from)) = (self.entity(ch), self.entity(from))
            && place::overwrite_mob(self.world, ch, from, keep_hp)
        {
            self.unsupported(
                "mtransform.stats",
                "level, alignment, rewards and key-based behaviour stay those of the original vnum",
            );
        }
    }

    fn cmd_overwrite_obj(&mut self, obj: Uid, from: Uid) {
        if let (Some(obj), Some(from)) = (self.entity(obj), self.entity(from)) {
            place::overwrite_obj(self.world, obj, from);
        }
    }
}
