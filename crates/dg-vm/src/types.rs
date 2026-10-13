//! Core data types: ids, variables, scripts, trigger instances, type bits.
//!
//! Ports of the structures in `dg_scripts.h` (`trig_var_data`, `trig_data`,
//! `script_data`) and the `add_var`/`remove_var` list operations
//! (`dg_variables.c`, `dg_handler.c`).

use std::sync::Arc;

use crate::cstr::str_eq;
use crate::driver::RunState;
use crate::script::Program;

/// A script id (`long` in the C). Text form is [`UID_CHAR`] followed by the
/// decimal id, e.g. `}10000042`.
pub type Uid = i64;

/// `UID_CHAR` (`dg_scripts.h`): the prefix that marks a string as an entity
/// reference.
pub const UID_CHAR: char = '}';

/// `MOB_ID_BASE`: player ids are below this, mob ids at or above it.
pub const MOB_ID_BASE: Uid = 10_000_000;
/// `ROOM_ID_BASE`: a room's id is its vnum plus this.
pub const ROOM_ID_BASE: Uid = 20_000_000;
/// `OBJ_ID_BASE`: object ids are at or above this.
pub const OBJ_ID_BASE: Uid = 30_000_000;

/// `MAX_SCRIPT_DEPTH`: how deep triggers may recurse into one another.
pub const MAX_SCRIPT_DEPTH: u32 = 10;
/// `SCRIPT_ERROR_CODE`: `script_driver`'s return when the depth cap trips.
pub const SCRIPT_ERROR_CODE: i32 = -9_999_999;

/// What a [`Uid`] names. Players and mobs are both characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Char,
    Obj,
    Room,
}

/// The kind an id's range implies (`find_char`/`find_obj`/`find_room` test the
/// same ranges). This says nothing about whether the entity exists.
pub fn uid_kind(uid: Uid) -> Kind {
    if uid >= OBJ_ID_BASE {
        Kind::Obj
    } else if uid >= ROOM_ID_BASE {
        Kind::Room
    } else {
        Kind::Char
    }
}

/// The text form of an id: `}<id>`.
pub fn uid_text(uid: Uid) -> String {
    format!("{UID_CHAR}{uid}")
}

/// Parse the text form of an id. The C tests only the first character and
/// then `atol`s the rest; this does the same, so `}12abc` is id 12.
pub fn parse_uid(text: &str) -> Option<Uid> {
    text.strip_prefix(UID_CHAR).map(crate::cstr::atol)
}

/// What a trigger is attached to: `MOB_TRIGGER`/`OBJ_TRIGGER`/`WLD_TRIGGER`.
///
/// It selects the meaning of the type letters, the command interpreter, and
/// the `m`/`o`/`w` spelling `%send%` and friends expand to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AttachType {
    Mob,
    Obj,
    Wld,
}

impl AttachType {
    /// The C constant's value, which indexes the per-type command tables.
    pub fn index(self) -> usize {
        match self {
            AttachType::Mob => 0,
            AttachType::Obj => 1,
            AttachType::Wld => 2,
        }
    }

    /// From the `.trg` record's `attach_type` field.
    pub fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(AttachType::Mob),
            1 => Some(AttachType::Obj),
            2 => Some(AttachType::Wld),
            _ => None,
        }
    }

    /// The kind of entity that owns a script of this type.
    pub fn kind(self) -> Kind {
        match self {
            AttachType::Mob => Kind::Char,
            AttachType::Obj => Kind::Obj,
            AttachType::Wld => Kind::Room,
        }
    }
}

/// Trigger-type bits (`dg_scripts.h`). A record's type letters `a`..`z` set
/// bits `0`..`25` (`asciiflag_conv`); what a bit means depends on the
/// [`AttachType`].
pub mod trig {
    pub const MTRIG_GLOBAL: u32 = 1 << 0;
    pub const MTRIG_RANDOM: u32 = 1 << 1;
    pub const MTRIG_COMMAND: u32 = 1 << 2;
    pub const MTRIG_SPEECH: u32 = 1 << 3;
    pub const MTRIG_ACT: u32 = 1 << 4;
    pub const MTRIG_DEATH: u32 = 1 << 5;
    pub const MTRIG_GREET: u32 = 1 << 6;
    pub const MTRIG_GREET_ALL: u32 = 1 << 7;
    pub const MTRIG_ENTRY: u32 = 1 << 8;
    pub const MTRIG_RECEIVE: u32 = 1 << 9;
    pub const MTRIG_FIGHT: u32 = 1 << 10;
    pub const MTRIG_HITPRCNT: u32 = 1 << 11;
    pub const MTRIG_BRIBE: u32 = 1 << 12;
    pub const MTRIG_LOAD: u32 = 1 << 13;
    pub const MTRIG_MEMORY: u32 = 1 << 14;
    pub const MTRIG_CAST: u32 = 1 << 15;
    pub const MTRIG_LEAVE: u32 = 1 << 16;
    pub const MTRIG_DOOR: u32 = 1 << 17;
    pub const MTRIG_DAMAGE: u32 = 1 << 18;
    pub const MTRIG_TIME: u32 = 1 << 19;

    pub const OTRIG_GLOBAL: u32 = 1 << 0;
    pub const OTRIG_RANDOM: u32 = 1 << 1;
    pub const OTRIG_COMMAND: u32 = 1 << 2;
    pub const OTRIG_TIMER: u32 = 1 << 5;
    pub const OTRIG_GET: u32 = 1 << 6;
    pub const OTRIG_DROP: u32 = 1 << 7;
    pub const OTRIG_GIVE: u32 = 1 << 8;
    pub const OTRIG_WEAR: u32 = 1 << 9;
    pub const OTRIG_REMOVE: u32 = 1 << 11;
    pub const OTRIG_LOAD: u32 = 1 << 13;
    pub const OTRIG_CAST: u32 = 1 << 15;
    pub const OTRIG_LEAVE: u32 = 1 << 16;
    pub const OTRIG_CONSUME: u32 = 1 << 18;
    pub const OTRIG_TIME: u32 = 1 << 19;

    pub const WTRIG_GLOBAL: u32 = 1 << 0;
    pub const WTRIG_RANDOM: u32 = 1 << 1;
    pub const WTRIG_COMMAND: u32 = 1 << 2;
    pub const WTRIG_SPEECH: u32 = 1 << 3;
    pub const WTRIG_RESET: u32 = 1 << 5;
    pub const WTRIG_ENTER: u32 = 1 << 6;
    pub const WTRIG_DROP: u32 = 1 << 7;
    pub const WTRIG_CAST: u32 = 1 << 15;
    pub const WTRIG_LEAVE: u32 = 1 << 16;
    pub const WTRIG_DOOR: u32 = 1 << 17;
    pub const WTRIG_LOGIN: u32 = 1 << 18;
    pub const WTRIG_TIME: u32 = 1 << 19;

    /// `asciiflag_conv` (`db.c`) for trigger type letters: lowercase `a`..`z`
    /// set bits `0`..`25`, uppercase `A`..`Z` bits `26`..`51` (truncated to
    /// the 32 a trigger type holds). A string of digits is the number itself.
    pub fn asciiflag_conv(flags: &str) -> u32 {
        let mut bits: u64 = 0;
        let mut is_num = true;
        for c in flags.chars() {
            if c.is_ascii_lowercase() {
                bits |= 1 << (c as u8 - b'a');
            } else if c.is_ascii_uppercase() {
                bits |= 1 << (26 + (c as u8 - b'A'));
            }
            if !(c.is_ascii_digit() || c == '-') {
                is_num = false;
            }
        }
        if is_num {
            bits = crate::cstr::atol(flags) as u64;
        }
        bits as u32
    }
}

/// One variable (`trig_var_data`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Var {
    pub name: String,
    pub value: String,
    /// The `context` a global was saved under; `0` is "any context".
    pub context: i64,
}

/// A variable list (`trig_var_data *`).
///
/// The C list is singly linked with new entries **prepended**; lookups take
/// the first name match. Index `0` here is the head, so iteration order is
/// the C's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarList {
    pub vars: Vec<Var>,
}

impl VarList {
    /// `add_var` (`dg_variables.c`).
    ///
    /// A name containing `.` is rejected. The first entry with this name is
    /// overwritten when its context is `0` or equals `context`; otherwise a
    /// new entry is prepended. Returns `false` when the name was rejected.
    pub fn add_var(&mut self, name: &str, value: &str, context: i64) -> bool {
        if name.contains('.') {
            return false;
        }
        match self.vars.iter_mut().find(|var| str_eq(&var.name, name)) {
            Some(var) if var.context == 0 || var.context == context => {
                var.value = value.to_owned();
            }
            _ => self.vars.insert(
                0,
                Var {
                    name: name.to_owned(),
                    value: value.to_owned(),
                    context,
                },
            ),
        }
        true
    }

    /// `remove_var` (`dg_handler.c`): remove the first entry with this name.
    /// Returns whether one was removed.
    pub fn remove_var(&mut self, name: &str) -> bool {
        match self.vars.iter().position(|var| str_eq(&var.name, name)) {
            Some(index) => {
                self.vars.remove(index);
                true
            }
            None => false,
        }
    }

    /// The first entry with this name, any context (the trigger-local lookup
    /// in `find_replacement`).
    pub fn get(&self, name: &str) -> Option<&Var> {
        self.vars.iter().find(|var| str_eq(&var.name, name))
    }

    /// The first entry with this name whose context is `0` or `context` (the
    /// global lookup in `find_replacement`).
    pub fn get_in_context(&self, name: &str, context: i64) -> Option<&Var> {
        self.vars
            .iter()
            .find(|var| str_eq(&var.name, name) && (var.context == 0 || var.context == context))
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// `free_varlist`.
    pub fn clear(&mut self) {
        self.vars.clear();
    }
}

/// A trigger prototype: one `.trg` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrigProto {
    pub vnum: i32,
    pub name: String,
    /// The record's `attach_type` field.
    pub attach_type: AttachType,
    /// The type bitvector (`GET_TRIG_TYPE`), see [`trig`].
    pub trigger_type: u32,
    /// `GET_TRIG_NARG`.
    pub narg: i32,
    /// `GET_TRIG_ARG`.
    pub arglist: String,
    /// The script body (`cmdlist`).
    pub program: Arc<Program>,
}

/// A trigger attached to an entity (`trig_data`).
#[derive(Debug, Clone)]
pub struct TrigInstance {
    /// Unique within its [`Script`]; stable across attach/detach of others.
    /// A scheduled `wait` names its trigger by this.
    pub id: u64,
    pub proto: Arc<TrigProto>,
    /// `GET_TRIG_VARS`: the trigger-local variables.
    pub vars: VarList,
    /// The interpreter's position and nesting state. `run.depth != 0` means
    /// the trigger is running or waiting, which is what `TRIGGER_CHECK` tests
    /// to keep it from firing again.
    pub run: RunState,
    /// `purged` in `trig_data`: detached while running; dropped when the run
    /// unwinds.
    pub purged: bool,
}

impl TrigInstance {
    pub fn new(id: u64, proto: Arc<TrigProto>) -> Self {
        Self {
            id,
            proto,
            vars: VarList::default(),
            run: RunState::default(),
            purged: false,
        }
    }

    /// `TRIGGER_CHECK(t, type)`: the trigger has this type bit and is neither
    /// running nor waiting.
    pub fn check(&self, type_bit: u32) -> bool {
        self.proto.trigger_type & type_bit != 0 && !self.run.is_active()
    }
}

/// An entity's script (`script_data`): its attached triggers and globals.
#[derive(Debug, Clone, Default)]
pub struct Script {
    /// `trig_list`, in attach order. Order matters: events try triggers in
    /// this order and several stop at the first that runs.
    pub triggers: Vec<TrigInstance>,
    /// `global_vars`.
    pub global_vars: VarList,
    /// `context`: selects which contextual globals are visible.
    pub context: i64,
    /// The next [`TrigInstance::id`] to hand out.
    pub next_id: u64,
}

impl Script {
    /// `SCRIPT_TYPES`: the union of the attached triggers' type bits.
    pub fn types(&self) -> u32 {
        self.triggers
            .iter()
            .fold(0, |bits, trigger| bits | trigger.proto.trigger_type)
    }

    /// `SCRIPT_CHECK(go, type)`.
    pub fn check(&self, type_bit: u32) -> bool {
        self.types() & type_bit != 0
    }

    /// `add_trigger(sc, t, loc)`: insert at `loc` (`-1` or past the end
    /// appends). Returns the new instance's id.
    pub fn add_trigger(&mut self, proto: Arc<TrigProto>, loc: i32) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let instance = TrigInstance::new(id, proto);
        match usize::try_from(loc) {
            Ok(index) if index < self.triggers.len() => self.triggers.insert(index, instance),
            _ => self.triggers.push(instance),
        }
        id
    }

    pub fn trigger(&self, id: u64) -> Option<&TrigInstance> {
        self.triggers.iter().find(|trigger| trigger.id == id)
    }

    pub fn trigger_mut(&mut self, id: u64) -> Option<&mut TrigInstance> {
        self.triggers.iter_mut().find(|trigger| trigger.id == id)
    }
}

/// `time_info_data`: the in-world clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MudTime {
    pub hours: i32,
    pub day: i32,
    pub month: i32,
    pub year: i32,
}

/// Real seconds per in-world hour (`SECS_PER_MUD_HOUR`).
pub const SECS_PER_MUD_HOUR: i64 = 75;
/// Pulses per real second (`PASSES_PER_SEC` with the stock `OPT_USEC`).
pub const PASSES_PER_SEC: i64 = 10;
/// `PULSE_DG_SCRIPT`: how often random triggers are checked, in pulses.
pub const PULSE_DG_SCRIPT: i64 = 13 * PASSES_PER_SEC;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_text_round_trips_and_ranges_classify() {
        assert_eq!(uid_text(42), "}42");
        assert_eq!(parse_uid("}42"), Some(42));
        assert_eq!(parse_uid("42"), None);
        assert_eq!(uid_kind(5), Kind::Char);
        assert_eq!(uid_kind(MOB_ID_BASE + 1), Kind::Char);
        assert_eq!(uid_kind(ROOM_ID_BASE + 3001), Kind::Room);
        assert_eq!(uid_kind(OBJ_ID_BASE), Kind::Obj);
    }

    #[test]
    fn add_var_overwrites_in_context_and_prepends_otherwise() {
        let mut vars = VarList::default();
        assert!(vars.add_var("x", "1", 0));
        assert!(vars.add_var("X", "2", 0));
        assert_eq!(vars.vars.len(), 1);
        assert_eq!(vars.get("x").map(|v| v.value.as_str()), Some("2"));

        // A contextual global is shadowed, not overwritten, by another context.
        let mut globals = VarList::default();
        globals.add_var("g", "a", 5);
        globals.add_var("g", "b", 7);
        assert_eq!(globals.vars.len(), 2);
        assert_eq!(globals.vars[0].value, "b");
        assert_eq!(
            globals.get_in_context("g", 5).map(|v| v.value.as_str()),
            Some("a")
        );
        assert!(!vars.add_var("a.b", "x", 0));
    }

    #[test]
    fn remove_var_takes_the_first_match() {
        let mut vars = VarList::default();
        vars.add_var("a", "1", 0);
        assert!(vars.remove_var("A"));
        assert!(!vars.remove_var("a"));
    }

    #[test]
    fn type_letters_convert_to_bits() {
        assert_eq!(trig::asciiflag_conv("g"), trig::MTRIG_GREET);
        assert_eq!(
            trig::asciiflag_conv("ab"),
            trig::MTRIG_GLOBAL | trig::MTRIG_RANDOM
        );
        assert_eq!(trig::asciiflag_conv("64"), 64);
    }
}
