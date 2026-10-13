//! An in-memory [`DgHost`](crate::host::DgHost) for tests.
//!
//! The mock world is a property bag: every entity is a [`MockEntity`] with
//! named numbers, texts, links to other entities, and lists of entities. Each
//! host sub-trait is implemented in its own file in this directory and
//! documents the keys it reads beyond the shared ones below.
//!
//! # Shared data model
//!
//! These keys are the mock's world. Every sub-trait implementation uses them,
//! so tests that combine modules agree on where things are and what they
//! are. Add module-specific keys freely (and document them in that module's
//! mock file), but never a second key for a fact listed here. Use the
//! `MockHost` movement helpers rather than editing the lists by hand.
//!
//! | entity | key | meaning (C) |
//! |---|---|---|
//! | any | `nums["vnum"]` | `GET_MOB_VNUM` / `GET_OBJ_VNUM` / `room->number`; `-1` for a player |
//! | any | `texts["name"]` | `GET_NAME` / `short_description` / `room->name` |
//! | char, obj | `texts["alias"]` | keyword list (`player.name`, `obj->name`) |
//! | char | `nums["is_npc"]` | `IS_NPC` |
//! | char | `nums["level"]` | `GET_LEVEL` |
//! | char | `nums["dead"]` | `DEAD(ch)`: extracted, awaiting [`MockHost::sweep_dead`] |
//! | char | `nums["dg_invalid"]` | non-zero makes `valid_dg_target` false |
//! | char | `lists["cannot_see"]` | entities this character fails `CAN_SEE` for |
//! | char | `nums["pos"]` | `GET_POS` (`0` dead … `8` standing) |
//! | char | `nums["hit"]`, `nums["max_hit"]`, `nums["mana"]`, `nums["max_mana"]`, `nums["move"]`, `nums["max_move"]` | the point pools |
//! | char | `nums["gold"]`, `nums["exp"]`, `nums["align"]`, `nums["sex"]`, `nums["class"]` | the same-named `GET_*` |
//! | char | `links["fighting"]`, `links["master"]` | `FIGHTING(ch)`, `ch->master` |
//! | char | `lists["followers"]` | `ch->followers`, head first |
//! | char | `nums["aff:<n>"]` | `AFF_FLAGGED(ch, n)`, `<n>` the `AFF_*` number |
//! | char | `nums["spell:<n>"]` | `affected_by_spell(ch, n)` |
//! | char, obj | `links["room"]` | `IN_ROOM` (objects: only when lying in a room) |
//! | char | `lists["inventory"]` | `ch->carrying`, head first |
//! | char | `links["eq:<pos>"]` | `GET_EQ(ch, pos)`, `<pos>` the numeric wear position |
//! | obj | `links["carried_by"]` / `links["worn_by"]` / `links["in_obj"]` | the same-named C fields |
//! | obj | `nums["type"]` | `GET_OBJ_TYPE` (`15` is a container) |
//! | obj | `nums["wear"]` | wear flags as a bitvector, bit `n` = `ITEM_WEAR_*` `n` |
//! | obj | `nums["val0"]`..`nums["val3"]`, `nums["timer"]`, `nums["cost"]`, `nums["weight"]` | `GET_OBJ_VAL` and the same-named `GET_OBJ_*` |
//! | room, obj | `lists["contents"]` | `room->contents` / `obj->contains` |
//! | room | `lists["people"]` | `room->people`, head first |
//! | room | `links["exit:<dir>"]` | `R_EXIT(room, dir)->to_room`, `<dir>` the direction number; absent = no exit |
//! | room | `nums["exit:<dir>:flags"]`, `nums["exit:<dir>:key"]` | `exit_info`, `key` |
//! | room | `texts["exit:<dir>:keyword"]`, `texts["exit:<dir>:desc"]` | `keyword`, `general_description` |
//! | room | `nums["zone"]`, `nums["sector"]` | zone number, `sector_type` |
//! | room | `nums["flag:<n>"]` | `ROOM_FLAGGED(room, n)` |
//!
//! World-level facts live in [`MockHost::globals`], a bag of the same shape:
//!
//! | key | meaning |
//! |---|---|
//! | `nums["script_players"]` | `CONFIG_SCRIPT_PLAYERS` |
//! | `texts["commands"]` | the game command table for `mudcommand`, space-separated in table order |
//! | `texts["zone:<n>:name"]`, `nums["zone:<n>:empty"]` | a zone's name; whether `is_empty(zone)` |
//! | `texts["mobproto:<vnum>"]`, `texts["objproto:<vnum>"]` | a loadable prototype's name; absent = no such vnum |
//!
//! Like the C lists, a newly placed character or object goes to the **head**,
//! with one exception the C also makes: `obj_to_room` appends, so a room's
//! contents are oldest first.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::host::{BaseHost, WorldHost};
use crate::script::Program;
use crate::types::{
    AttachType, Kind, MOB_ID_BASE, MudTime, OBJ_ID_BASE, ROOM_ID_BASE, Script, ScriptMemory,
    TrigProto, Uid, trig::asciiflag_conv,
};

mod chars;
mod commands;
mod dispatch;
mod magic;
mod objs;
mod rooms;
mod subst;

/// One mock entity.
#[derive(Debug, Clone)]
pub struct MockEntity {
    pub kind: Kind,
    pub nums: BTreeMap<String, i64>,
    pub texts: BTreeMap<String, String>,
    pub links: BTreeMap<String, Uid>,
    pub lists: BTreeMap<String, Vec<Uid>>,
    pub script: Option<Script>,
    /// `SCRIPT_MEM`, head first.
    pub memory: Vec<ScriptMemory>,
}

impl Default for MockEntity {
    fn default() -> Self {
        Self::new(Kind::Room)
    }
}

impl MockEntity {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            nums: BTreeMap::new(),
            texts: BTreeMap::new(),
            links: BTreeMap::new(),
            lists: BTreeMap::new(),
            script: None,
            memory: Vec::new(),
        }
    }
}

/// The in-memory host.
#[derive(Debug, Default)]
pub struct MockHost {
    pub entities: BTreeMap<Uid, MockEntity>,
    /// World-level facts (see the module table). Its `kind` is meaningless.
    pub globals: MockEntity,
    /// The global `pulse` counter.
    pub pulse: i64,
    /// `dg_owner_purged`.
    pub owner_purged: bool,
    /// Every [`BaseHost::cancel_wait`] call: `(owner, trigger id)`.
    pub cancelled_waits: Vec<(Uid, u64)>,
    /// Trigger prototypes by vnum, for `attach`.
    pub protos: BTreeMap<i32, Arc<TrigProto>>,
    pub time: MudTime,
    pub depth: u32,
    /// Scripted results for [`BaseHost::rand_number`], consumed front first
    /// and clamped into the requested range. When empty, the low bound is
    /// returned.
    pub rolls: VecDeque<i64>,
    /// Every [`BaseHost::schedule_wait`] call: `(owner, trigger id, pulses)`.
    pub waits: Vec<(Uid, u64, i64)>,
    /// Every [`BaseHost::mud_command`] call.
    pub mud_commands: Vec<(Uid, String)>,
    /// Every [`BaseHost::script_log`] line.
    pub logs: Vec<String>,
    /// Every [`BaseHost::unsupported`] call.
    pub unsupported: Vec<(String, String)>,
    /// A free-form record of world effects, appended by the sub-trait
    /// implementations (`"send }5: hello"`), for assertions.
    pub effects: Vec<String>,
    /// `character_list`, newest first (the C prepends).
    pub character_list: Vec<Uid>,
    /// `object_list`, newest first.
    pub object_list: Vec<Uid>,
    next_player: Uid,
    next_mob: Uid,
    next_obj: Uid,
}

impl MockHost {
    pub fn new() -> Self {
        Self::default()
    }

    fn insert(&mut self, uid: Uid, kind: Kind, vnum: i64, name: &str) -> Uid {
        let mut entity = MockEntity::new(kind);
        entity.nums.insert("vnum".to_owned(), vnum);
        entity.texts.insert("name".to_owned(), name.to_owned());
        self.entities.insert(uid, entity);
        match kind {
            Kind::Char => self.character_list.insert(0, uid),
            Kind::Obj => self.object_list.insert(0, uid),
            Kind::Room => {}
        }
        uid
    }

    /// Add a room; its id is `ROOM_ID_BASE + vnum`.
    pub fn add_room(&mut self, vnum: i32, name: &str) -> Uid {
        self.insert(
            ROOM_ID_BASE + Uid::from(vnum),
            Kind::Room,
            i64::from(vnum),
            name,
        )
    }

    /// Add a player character (ids count up from 1).
    pub fn add_player(&mut self, name: &str) -> Uid {
        self.next_player += 1;
        let uid = self.insert(self.next_player, Kind::Char, -1, name);
        self.set_num(uid, "is_npc", 0);
        self.set_text(uid, "alias", name);
        uid
    }

    /// Add a mob (ids count up from `MOB_ID_BASE + 1`).
    pub fn add_mob(&mut self, vnum: i32, name: &str) -> Uid {
        self.next_mob += 1;
        let uid = self.insert(
            MOB_ID_BASE + self.next_mob,
            Kind::Char,
            i64::from(vnum),
            name,
        );
        self.set_num(uid, "is_npc", 1);
        self.set_text(uid, "alias", name);
        uid
    }

    /// Add an object (ids count up from `OBJ_ID_BASE + 1`).
    pub fn add_obj(&mut self, vnum: i32, name: &str) -> Uid {
        self.next_obj += 1;
        let uid = self.insert(
            OBJ_ID_BASE + self.next_obj,
            Kind::Obj,
            i64::from(vnum),
            name,
        );
        self.set_text(uid, "alias", name);
        uid
    }

    pub fn entity(&self, uid: Uid) -> Option<&MockEntity> {
        self.entities.get(&uid)
    }

    pub fn entity_mut(&mut self, uid: Uid) -> Option<&mut MockEntity> {
        self.entities.get_mut(&uid)
    }

    /// A number, `0` when absent.
    pub fn num(&self, uid: Uid, key: &str) -> i64 {
        self.entity(uid)
            .and_then(|entity| entity.nums.get(key))
            .copied()
            .unwrap_or(0)
    }

    pub fn set_num(&mut self, uid: Uid, key: &str, value: i64) {
        if let Some(entity) = self.entity_mut(uid) {
            entity.nums.insert(key.to_owned(), value);
        }
    }

    /// A text, empty when absent.
    pub fn text(&self, uid: Uid, key: &str) -> String {
        self.entity(uid)
            .and_then(|entity| entity.texts.get(key))
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_text(&mut self, uid: Uid, key: &str, value: &str) {
        if let Some(entity) = self.entity_mut(uid) {
            entity.texts.insert(key.to_owned(), value.to_owned());
        }
    }

    /// A link, `None` when absent or when its target no longer exists.
    pub fn link(&self, uid: Uid, key: &str) -> Option<Uid> {
        self.entity(uid)
            .and_then(|entity| entity.links.get(key))
            .copied()
            .filter(|target| self.entities.contains_key(target))
    }

    pub fn set_link(&mut self, uid: Uid, key: &str, target: Option<Uid>) {
        if let Some(entity) = self.entity_mut(uid) {
            match target {
                Some(target) => entity.links.insert(key.to_owned(), target),
                None => entity.links.remove(key),
            };
        }
    }

    /// A list, empty when absent.
    pub fn list(&self, uid: Uid, key: &str) -> Vec<Uid> {
        self.entity(uid)
            .and_then(|entity| entity.lists.get(key))
            .cloned()
            .unwrap_or_default()
    }

    fn list_push_front(&mut self, uid: Uid, key: &str, member: Uid) {
        if let Some(entity) = self.entity_mut(uid) {
            entity
                .lists
                .entry(key.to_owned())
                .or_default()
                .insert(0, member);
        }
    }

    fn list_remove(&mut self, uid: Uid, key: &str, member: Uid) {
        if let Some(list) = self
            .entity_mut(uid)
            .and_then(|entity| entity.lists.get_mut(key))
        {
            list.retain(|entry| *entry != member);
        }
    }

    /// `char_from_room`.
    pub fn char_from_room(&mut self, ch: Uid) {
        if let Some(room) = self.link(ch, "room") {
            self.list_remove(room, "people", ch);
        }
        self.set_link(ch, "room", None);
    }

    /// `char_to_room` (after a `char_from_room`).
    pub fn char_to_room(&mut self, ch: Uid, room: Uid) {
        self.char_from_room(ch);
        self.set_link(ch, "room", Some(room));
        self.list_push_front(room, "people", ch);
    }

    /// Detach an object from wherever it is: `obj_from_room`, `obj_from_char`,
    /// `obj_from_obj`, or `unequip_char`.
    pub fn obj_from_anywhere(&mut self, obj: Uid) {
        if let Some(room) = self.link(obj, "room") {
            self.list_remove(room, "contents", obj);
        }
        if let Some(holder) = self.link(obj, "carried_by") {
            self.list_remove(holder, "inventory", obj);
        }
        if let Some(container) = self.link(obj, "in_obj") {
            self.list_remove(container, "contents", obj);
        }
        if let Some(wearer) = self.link(obj, "worn_by")
            && let Some(entity) = self.entity_mut(wearer)
        {
            entity
                .links
                .retain(|key, target| !(key.starts_with("eq:") && *target == obj));
        }
        for key in ["room", "carried_by", "in_obj", "worn_by"] {
            self.set_link(obj, key, None);
        }
    }

    /// `obj_to_room`: appended, as in the C.
    pub fn obj_to_room(&mut self, obj: Uid, room: Uid) {
        self.obj_from_anywhere(obj);
        self.set_link(obj, "room", Some(room));
        if let Some(entity) = self.entity_mut(room) {
            entity
                .lists
                .entry("contents".to_owned())
                .or_default()
                .push(obj);
        }
    }

    /// `obj_to_char`.
    pub fn obj_to_char(&mut self, obj: Uid, ch: Uid) {
        self.obj_from_anywhere(obj);
        self.set_link(obj, "carried_by", Some(ch));
        self.list_push_front(ch, "inventory", obj);
    }

    /// `obj_to_obj`.
    pub fn obj_to_obj(&mut self, obj: Uid, container: Uid) {
        self.obj_from_anywhere(obj);
        self.set_link(obj, "in_obj", Some(container));
        self.list_push_front(container, "contents", obj);
    }

    /// `equip_char(ch, obj, pos)`.
    pub fn equip_char(&mut self, ch: Uid, obj: Uid, pos: i32) {
        self.obj_from_anywhere(obj);
        self.set_link(obj, "worn_by", Some(ch));
        self.set_link(ch, &format!("eq:{pos}"), Some(obj));
    }

    /// `extract_obj`: remove an object and everything in it from the world,
    /// immediately.
    pub fn extract_obj(&mut self, obj: Uid) {
        if self
            .entity(obj)
            .is_none_or(|entity| entity.kind != Kind::Obj)
        {
            return;
        }
        for item in self.list(obj, "contents") {
            self.extract_obj(item);
        }
        self.obj_from_anywhere(obj);
        self.entities.remove(&obj);
        self.object_list.retain(|entry| *entry != obj);
    }

    /// `extract_char`: mark a character dead. As in the C it stays in the
    /// world, in its room and holding its things, until [`Self::sweep_dead`].
    pub fn extract_char(&mut self, ch: Uid) {
        if self
            .entity(ch)
            .is_some_and(|entity| entity.kind == Kind::Char)
        {
            self.set_num(ch, "dead", 1);
        }
    }

    /// `extract_pending_chars`: the end-of-pulse sweep that really removes
    /// the characters [`Self::extract_char`] marked, and what they held.
    pub fn sweep_dead(&mut self) {
        let dead: Vec<Uid> = self
            .character_list
            .iter()
            .copied()
            .filter(|ch| self.num(*ch, "dead") != 0)
            .collect();
        for ch in dead {
            let mut held = self.list(ch, "inventory");
            if let Some(entity) = self.entity(ch) {
                held.extend(
                    entity
                        .links
                        .iter()
                        .filter(|(key, _)| key.starts_with("eq:"))
                        .map(|(_, target)| *target),
                );
            }
            for item in held {
                self.extract_obj(item);
            }
            self.char_from_room(ch);
            self.entities.remove(&ch);
            self.character_list.retain(|entry| *entry != ch);
        }
    }

    /// Build and register a trigger prototype. `types` is the `.trg` type
    /// letters (`"g"`, `"ab"`).
    pub fn add_proto(
        &mut self,
        vnum: i32,
        attach_type: AttachType,
        types: &str,
        narg: i32,
        arglist: &str,
        script: &str,
    ) -> Arc<TrigProto> {
        let proto = Arc::new(TrigProto {
            vnum,
            name: format!("trigger {vnum}"),
            attach_type,
            trigger_type: asciiflag_conv(types),
            narg,
            arglist: arglist.to_owned(),
            program: Arc::new(Program::parse(script)),
        });
        self.protos.insert(vnum, Arc::clone(&proto));
        proto
    }

    /// Attach a prototype to an entity, returning the instance id.
    pub fn attach(&mut self, uid: Uid, proto: &Arc<TrigProto>) -> Option<u64> {
        self.ensure_script(uid)
            .map(|script| script.add_trigger(Arc::clone(proto), -1))
    }

    /// Record a world effect for assertions.
    pub fn effect(&mut self, text: impl Into<String>) {
        self.effects.push(text.into());
    }
}

impl BaseHost for MockHost {
    fn exists(&self, uid: Uid) -> bool {
        self.entities.contains_key(&uid)
    }

    fn script(&self, uid: Uid) -> Option<&Script> {
        self.entity(uid).and_then(|entity| entity.script.as_ref())
    }

    fn script_mut(&mut self, uid: Uid) -> Option<&mut Script> {
        self.entity_mut(uid)
            .and_then(|entity| entity.script.as_mut())
    }

    fn ensure_script(&mut self, uid: Uid) -> Option<&mut Script> {
        self.entity_mut(uid)
            .map(|entity| entity.script.get_or_insert_with(Script::default))
    }

    fn remove_script(&mut self, uid: Uid) -> Option<Script> {
        self.entity_mut(uid).and_then(|entity| entity.script.take())
    }

    fn depth_mut(&mut self) -> &mut u32 {
        &mut self.depth
    }

    fn owner_purged_mut(&mut self) -> &mut bool {
        &mut self.owner_purged
    }

    fn script_players(&self) -> bool {
        self.globals
            .nums
            .get("script_players")
            .copied()
            .unwrap_or(0)
            != 0
    }

    fn pulse(&self) -> i64 {
        self.pulse
    }

    fn rand_number(&mut self, from: i64, to: i64) -> i64 {
        let (low, high) = if from > to { (to, from) } else { (from, to) };
        match self.rolls.pop_front() {
            Some(roll) => roll.clamp(low, high),
            None => low,
        }
    }

    fn mud_time(&self) -> MudTime {
        self.time
    }

    fn trig_proto(&self, vnum: i32) -> Option<Arc<TrigProto>> {
        self.protos.get(&vnum).cloned()
    }

    fn schedule_wait(&mut self, owner: Uid, trigger_id: u64, pulses: i64) {
        self.waits.push((owner, trigger_id, pulses));
    }

    fn cancel_wait(&mut self, owner: Uid, trigger_id: u64) {
        self.cancelled_waits.push((owner, trigger_id));
        self.waits
            .retain(|(waiter, id, _)| !(*waiter == owner && *id == trigger_id));
    }

    fn mud_command(&mut self, ch: Uid, line: &str) {
        self.mud_commands.push((ch, line.to_owned()));
    }

    fn mud_command_name(&self, prefix: &str) -> Option<String> {
        self.globals
            .texts
            .get("commands")?
            .split(' ')
            .find(|command| command.starts_with(prefix))
            .map(str::to_owned)
    }

    fn script_log(&mut self, message: &str) {
        self.logs.push(message.to_owned());
    }

    fn unsupported(&mut self, what: &str, detail: &str) {
        self.unsupported.push((what.to_owned(), detail.to_owned()));
    }
}

impl WorldHost for MockHost {
    fn kind(&self, uid: Uid) -> Option<Kind> {
        self.entity(uid).map(|entity| entity.kind)
    }

    fn is_npc(&self, ch: Uid) -> bool {
        self.num(ch, "is_npc") != 0
    }

    fn valid_dg_target(&self, ch: Uid, _allow_gods: bool) -> bool {
        self.num(ch, "dg_invalid") == 0
    }

    fn name(&self, uid: Uid) -> String {
        self.text(uid, "name")
    }

    fn keywords(&self, uid: Uid) -> String {
        self.text(uid, "alias")
    }

    fn vnum(&self, uid: Uid) -> i32 {
        self.entity(uid)
            .and_then(|entity| entity.nums.get("vnum"))
            .and_then(|vnum| i32::try_from(*vnum).ok())
            .unwrap_or(-1)
    }

    fn in_room(&self, uid: Uid) -> Option<Uid> {
        self.link(uid, "room")
    }

    fn room_people(&self, room: Uid) -> Vec<Uid> {
        self.list(room, "people")
    }

    fn room_contents(&self, room: Uid) -> Vec<Uid> {
        self.list(room, "contents")
    }

    fn carrying(&self, ch: Uid) -> Vec<Uid> {
        self.list(ch, "inventory")
    }

    fn equipment(&self, ch: Uid, pos: i32) -> Option<Uid> {
        self.link(ch, &format!("eq:{pos}"))
    }

    fn obj_contains(&self, obj: Uid) -> Vec<Uid> {
        self.list(obj, "contents")
    }

    fn obj_carried_by(&self, obj: Uid) -> Option<Uid> {
        self.link(obj, "carried_by")
    }

    fn obj_worn_by(&self, obj: Uid) -> Option<Uid> {
        self.link(obj, "worn_by")
    }

    fn obj_in_obj(&self, obj: Uid) -> Option<Uid> {
        self.link(obj, "in_obj")
    }

    fn character_list(&self) -> Vec<Uid> {
        self.character_list.clone()
    }

    fn object_list(&self) -> Vec<Uid> {
        self.object_list.clone()
    }

    fn real_room(&self, vnum: i32) -> Option<Uid> {
        let uid = ROOM_ID_BASE + Uid::from(vnum);
        (vnum >= 0 && self.kind(uid) == Some(Kind::Room)).then_some(uid)
    }

    fn can_see(&self, ch: Uid, target: Uid) -> bool {
        !self.list(ch, "cannot_see").contains(&target)
    }

    fn level(&self, ch: Uid) -> i32 {
        i32::try_from(self.num(ch, "level")).unwrap_or(0)
    }

    fn is_dead(&self, ch: Uid) -> bool {
        self.num(ch, "dead") != 0
    }

    fn is_container(&self, obj: Uid) -> bool {
        self.num(obj, "type") == 15
    }

    fn wear_flags(&self, obj: Uid) -> u32 {
        u32::try_from(self.num(obj, "wear")).unwrap_or(0)
    }

    fn script_mem(&self, ch: Uid) -> Vec<ScriptMemory> {
        self.entity(ch)
            .map(|entity| entity.memory.clone())
            .unwrap_or_default()
    }

    fn set_script_mem(&mut self, ch: Uid, memory: Vec<ScriptMemory>) {
        if let Some(entity) = self.entity_mut(ch) {
            entity.memory = memory;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::driver::{Mode, script_driver};
    use crate::host::Cx;
    use crate::subst::var_subst;
    use crate::types::TrigInstance;

    #[test]
    fn the_world_graph_helpers_keep_both_sides_in_step() {
        let mut host = MockHost::new();
        let room = host.add_room(3001, "Temple");
        let other = host.add_room(3002, "Altar");
        let mob = host.add_mob(3000, "the guard");
        let sword = host.add_obj(3020, "a sword");
        host.char_to_room(mob, room);
        host.obj_to_char(sword, mob);
        assert_eq!(host.list(room, "people"), vec![mob]);
        assert_eq!(host.link(sword, "carried_by"), Some(mob));

        host.char_to_room(mob, other);
        assert!(host.list(room, "people").is_empty());
        host.equip_char(mob, sword, 16);
        assert!(host.list(mob, "inventory").is_empty());
        assert_eq!(host.link(mob, "eq:16"), Some(sword));

        // A character is only marked dead until the end-of-pulse sweep.
        host.extract_char(mob);
        assert!(host.exists(mob) && host.is_dead(mob));
        assert_eq!(host.list(other, "people"), vec![mob]);
        host.sweep_dead();
        assert!(!host.exists(mob));
        assert!(!host.exists(sword));
        assert!(host.list(other, "people").is_empty());

        // Room contents are oldest first; an object goes at once.
        let (a, b) = (host.add_obj(1, "a"), host.add_obj(2, "b"));
        host.obj_to_room(a, room);
        host.obj_to_room(b, room);
        assert_eq!(host.list(room, "contents"), vec![a, b]);
        host.extract_obj(a);
        assert!(!host.exists(a));
        assert_eq!(host.list(room, "contents"), vec![b]);
    }

    #[test]
    fn a_mock_host_satisfies_the_full_contract() {
        let mut host = MockHost::new();
        let mob = host.add_mob(1, "a mob");
        let proto = host.add_proto(1, AttachType::Mob, "g", 100, "", "say hi");
        let id = host.attach(mob, &proto).unwrap();
        assert_eq!(script_driver(&mut host, mob, id, Mode::New), 1);

        let mut trig = TrigInstance::new(99, proto);
        trig.vars.add_var("who", "world", 0);
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        assert_eq!(
            var_subst(&mut cx, "hello %who%, 100%%"),
            "hello world, 100%"
        );
    }
}
