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

/// The kind an id's range implies. This says nothing about whether the entity
/// exists; [`crate::lookup::find_char`] and friends do.
///
/// Deviation: the C `find_room` has no upper bound and truncates to a 16-bit
/// vnum, so an object id can alias a room there. Here the three ranges are
/// disjoint.
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

/// The script type an owner of this kind runs its triggers as. The C passes
/// `MOB_TRIGGER` for anything attached to a character, whatever the record's
/// own `attach_type` says, so the run type always comes from the owner.
impl From<Kind> for AttachType {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Char => AttachType::Mob,
            Kind::Obj => AttachType::Obj,
            Kind::Room => AttachType::Wld,
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
    /// the 32 a trigger type holds). A string of digits, optionally led by a
    /// `-`, is the number itself.
    pub fn asciiflag_conv(flags: &str) -> u32 {
        let mut bits: u64 = 0;
        let mut is_num = true;
        for (index, c) in flags.chars().enumerate() {
            if c.is_ascii_lowercase() {
                bits |= 1 << (c as u8 - b'a');
            } else if c.is_ascii_uppercase() {
                bits |= 1 << (26 + (c as u8 - b'A'));
            }
            if !(c.is_ascii_digit() || (c == '-' && index == 0)) {
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

/// What the interpreter shares across nested runs: the C's file-scope
/// globals and function statics. The host owns one and hands it out through
/// [`crate::host::BaseHost::vm_state`]; the crate keeps no state of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmState {
    /// `script_driver`'s `static int depth`: the current trigger nesting.
    pub depth: u32,
    /// The global `dg_owner_purged`: set by a purge command when it removes
    /// the running trigger's own owner, zeroed on entry to `script_driver`,
    /// and tested after each command to stop the script. It is a flag, not an
    /// existence test: a purged mob still exists until the end of the pulse.
    pub owner_purged: bool,
    /// Whether the most recently finished `script_driver` run left through
    /// its purge path (`dg_owner_purged` tested after a command sent to the
    /// command interpreter), where the C NULLs `go_adress`. Cleared on every
    /// driver entry, so it is read only straight after a run. Unlike
    /// `owner_purged` it is not set by a nested script left running under a
    /// `dg_cast`, `dg_affect` or other line the driver does not test the
    /// flag after.
    pub run_purged: bool,
    /// `comm.c`'s `static int dg_act_check`, which every `act()` call sets.
    /// The host's `act()` must write `!IS_SET(type, DG_NO_TRIG)` here on every
    /// call that has text (nested ones included), and its `perform_act` must
    /// read this field, not the line's own flag, in `IS_NPC(to) &&
    /// dg_act_check && to != ch` before calling `act_mtrigger`.
    pub act_check: bool,
    /// How deeply `var_subst` is nested right now.
    pub subst_nesting: u32,
    /// How many `var_subst` entries the current top-level call has made.
    pub subst_work: u32,
    /// A `var_subst` limit tripped; the rest of the top-level call runs
    /// nothing.
    pub subst_aborted: bool,
    /// How deeply `eval_expr` is nested right now.
    pub expr_depth: usize,
    /// A level past the `eval_expr` depth limit was refused; the outermost
    /// call then answers the empty string.
    pub expr_poisoned: bool,
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
    /// Set on the slot left in [`Script::triggers`] while the instance is
    /// checked out for a run (see [`Script::check_out`]).
    pub checked_out: bool,
}

impl TrigInstance {
    pub fn new(id: u64, proto: Arc<TrigProto>) -> Self {
        Self {
            id,
            proto,
            vars: VarList::default(),
            run: RunState::default(),
            checked_out: false,
        }
    }

    /// `GET_TRIG_DEPTH(t) != 0`: running or waiting.
    pub fn is_busy(&self) -> bool {
        self.checked_out || self.run.is_active()
    }

    /// `TRIGGER_CHECK(t, type)`: the trigger has this type bit and is neither
    /// running nor waiting.
    pub fn check(&self, type_bit: u32) -> bool {
        self.proto.trigger_type & type_bit != 0 && !self.is_busy()
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

    /// `add_trigger(sc, t, loc)`. Returns the new instance's id.
    ///
    /// `loc` `0` puts it at the head. Otherwise the C walks `loc` links from
    /// the head, stopping at the last trigger, and inserts **after** where it
    /// stopped: `loc` `1` on `[A, B, C]` gives `[A, B, T, C]`, and `-1` (or
    /// anything past the end) appends.
    pub fn add_trigger(&mut self, proto: Arc<TrigProto>, loc: i32) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let instance = TrigInstance::new(id, proto);
        let len = self.triggers.len();
        let index = match usize::try_from(loc) {
            Ok(0) => 0,
            Ok(steps) => (steps.min(len.saturating_sub(1)) + 1).min(len),
            Err(_) => len,
        };
        self.triggers.insert(index, instance);
        id
    }

    /// `trig_is_attached(sc, vnum)`.
    pub fn is_attached(&self, vnum: i32) -> bool {
        self.triggers
            .iter()
            .any(|trigger| trigger.proto.vnum == vnum)
    }

    /// The attached triggers' ids in order: a snapshot for a dispatch loop,
    /// which must re-fetch each trigger by id because a run can attach and
    /// detach.
    pub fn trigger_ids(&self) -> Vec<u64> {
        self.triggers.iter().map(|trigger| trigger.id).collect()
    }

    /// Take an instance for a run.
    ///
    /// The instance's variables and run state move out; its **slot stays** in
    /// [`Script::triggers`] at the same index with the same id and prototype,
    /// marked busy. So while it runs, everything that walks the list sees
    /// what the C sees: `TRIGGER_CHECK` finds it busy, `SCRIPT_TYPES` keeps
    /// its bits, the list is non-empty, and positional `detach` and
    /// `hasattached` count it.
    ///
    /// `None` when there is no such instance or it is already checked out.
    pub fn check_out(&mut self, id: u64) -> Option<TrigInstance> {
        let slot = self.trigger_mut(id).filter(|slot| !slot.checked_out)?;
        slot.checked_out = true;
        Some(TrigInstance {
            id,
            proto: Arc::clone(&slot.proto),
            vars: std::mem::take(&mut slot.vars),
            run: std::mem::take(&mut slot.run),
            checked_out: false,
        })
    }

    /// Return a checked-out instance to its slot. `false` when the slot is
    /// gone (the trigger was detached during its own run), in which case the
    /// instance is dropped.
    pub fn check_in(&mut self, trig: TrigInstance) -> bool {
        match self.trigger_mut(trig.id).filter(|slot| slot.checked_out) {
            Some(slot) => {
                *slot = trig;
                true
            }
            None => false,
        }
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

/// `script_memory`: a character a mob remembers, and the command to run when
/// it next sees them (`mremember`; the `memory` trigger when `cmd` is `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptMemory {
    pub id: Uid,
    pub cmd: Option<String>,
}

/// Real seconds per in-world hour (`SECS_PER_MUD_HOUR`).
pub const SECS_PER_MUD_HOUR: i64 = 75;
/// `SECS_PER_MUD_DAY`.
pub const SECS_PER_MUD_DAY: i64 = 24 * SECS_PER_MUD_HOUR;
/// Pulses per real second (`PASSES_PER_SEC` with the stock `OPT_USEC`).
pub const PASSES_PER_SEC: i64 = 10;
/// Pulses per in-world hour.
pub const PULSES_PER_MUD_HOUR: i64 = SECS_PER_MUD_HOUR * PASSES_PER_SEC;
/// `PULSE_DG_SCRIPT`: how often random triggers are checked, in pulses.
pub const PULSE_DG_SCRIPT: i64 = 13 * PASSES_PER_SEC;
/// `PULSE_VIOLENCE`: a combat round, in pulses.
pub const PULSE_VIOLENCE: i64 = 2 * PASSES_PER_SEC;

/// `MAX_INPUT_LENGTH`: the C's working-buffer size. `var_subst` and
/// `eval_expr` refuse a line this long or longer, and a substituted line is
/// truncated to one byte less.
pub const MAX_INPUT_LENGTH: usize = 512;

// ---- Numbers a host interprets ---------------------------------------------
//
// Flag and position numbers the VM passes to the host's primitives. `structs.h`
// is the reference; the host maps each to its own representation.

/// `POS_*`: `GET_POS` values (`CharStat::Pos`).
pub const POS_DEAD: i32 = 0;
pub const POS_MORTALLYW: i32 = 1;
pub const POS_INCAP: i32 = 2;
pub const POS_STUNNED: i32 = 3;
/// `AWAKE(ch)` is `GET_POS(ch) > POS_SLEEPING`.
pub const POS_SLEEPING: i32 = 4;
pub const POS_RESTING: i32 = 5;
pub const POS_SITTING: i32 = 6;
pub const POS_FIGHTING: i32 = 7;
pub const POS_STANDING: i32 = 8;

/// Act-array bit numbers (`char_act_flagged`, `char_set_act_flag`).
/// `PLR_KILLER` and `PLR_THIEF`.
pub const PLR_KILLER: u32 = 0;
pub const PLR_THIEF: u32 = 1;
/// `MOB_ISNPC`: read through `WorldHost::is_npc`, never `char_act_flagged`.
pub const MOB_ISNPC: u32 = 3;
/// `PLR_WRITING`.
pub const PLR_WRITING: u32 = 4;
/// `PLR_NOTDEADYET`: read through `WorldHost::is_dead`.
pub const PLR_NOTDEADYET: u32 = 16;
/// `MOB_NOTDEADYET`: read through `WorldHost::is_dead`.
pub const MOB_NOTDEADYET: u32 = 19;

/// Preference-array bit numbers (`char_pref_flagged`).
/// `PRF_NOHASSLE`.
pub const PRF_NOHASSLE: u32 = 8;
/// `PRF_COLOR_1`.
pub const PRF_COLOR_1: u32 = 13;
/// `PRF_COLOR_2`.
pub const PRF_COLOR_2: u32 = 14;

/// `AFF_CHARM`: the affect-array bit `CmdHost::cmd_is_charmed` stands for.
pub const AFF_CHARM: u32 = 22;

/// Room-flag numbers (`RoomHost::room_flagged`).
/// `ROOM_INDOORS`: suppresses weather.
pub const ROOM_INDOORS: usize = 3;
/// `ROOM_PRIVATE`.
pub const ROOM_PRIVATE: usize = 9;
/// `ROOM_GODROOM`.
pub const ROOM_GODROOM: usize = 10;
/// `ROOM_HOUSE`.
pub const ROOM_HOUSE: usize = 11;

/// `mudlog` types (`utils.h`): the lowest log setting that receives the line.
pub const MUDLOG_OFF: i32 = 0;
/// `BRF`: only the most important messages.
pub const MUDLOG_BRF: i32 = 1;
/// `NRM`: the standard messages.
pub const MUDLOG_NRM: i32 = 2;
/// `CMP`: every message.
pub const MUDLOG_CMP: i32 = 3;

/// `ITEM_CONTAINER`: the `GET_OBJ_TYPE` of an object that holds others.
pub const ITEM_CONTAINER: i32 = 15;

/// `NUM_OF_DIRS`.
pub const NUM_OF_DIRS: usize = 10;
/// `dirs[]` (`constants.c`), indexed by direction number.
pub const DIRS: [&str; NUM_OF_DIRS] = [
    "north",
    "east",
    "south",
    "west",
    "up",
    "down",
    "northwest",
    "northeast",
    "southeast",
    "southwest",
];
/// `rev_dir[]` (`constants.c`): the opposite of each direction.
pub const REV_DIR: [usize; NUM_OF_DIRS] = [2, 3, 0, 1, 5, 4, 8, 9, 6, 7];

/// `LVL_IMMORT`.
pub const LVL_IMMORT: i32 = 31;
/// `LVL_GOD`.
pub const LVL_GOD: i32 = 32;
/// `LVL_GRGOD`.
pub const LVL_GRGOD: i32 = 33;
/// `LVL_IMPL`.
pub const LVL_IMPL: i32 = 34;

/// `OCMD_EQUIP`: an object command trigger fires when the object is worn.
pub const OCMD_EQUIP: i32 = 1 << 0;
/// `OCMD_INVEN`: … when it is carried.
pub const OCMD_INVEN: i32 = 1 << 1;
/// `OCMD_ROOM`: … when it lies in the actor's room.
pub const OCMD_ROOM: i32 = 1 << 2;

/// `OCMD_EAT`/`OCMD_DRINK`/`OCMD_QUAFF`: the `consume` trigger's command.
pub const OCMD_EAT: i32 = 1;
pub const OCMD_DRINK: i32 = 2;
pub const OCMD_QUAFF: i32 = 3;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

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
        assert_eq!(trig::asciiflag_conv("1-2"), 0);
    }

    fn proto(vnum: i32) -> Arc<TrigProto> {
        Arc::new(TrigProto {
            vnum,
            name: format!("t{vnum}"),
            attach_type: AttachType::Mob,
            trigger_type: trig::MTRIG_GREET,
            narg: 100,
            arglist: String::new(),
            program: Arc::new(Program::default()),
        })
    }

    fn vnums(script: &Script) -> Vec<i32> {
        script.triggers.iter().map(|t| t.proto.vnum).collect()
    }

    #[test]
    fn add_trigger_inserts_after_the_loc_th_link() {
        let mut script = Script::default();
        for vnum in [1, 2, 3] {
            script.add_trigger(proto(vnum), -1);
        }
        script.add_trigger(proto(9), 1);
        assert_eq!(vnums(&script), vec![1, 2, 9, 3]);
        script.add_trigger(proto(8), 0);
        assert_eq!(vnums(&script), vec![8, 1, 2, 9, 3]);
        script.add_trigger(proto(7), 99);
        assert_eq!(vnums(&script), vec![8, 1, 2, 9, 3, 7]);

        let mut empty = Script::default();
        empty.add_trigger(proto(1), 5);
        assert_eq!(vnums(&empty), vec![1]);
    }

    #[test]
    fn a_checked_out_trigger_keeps_its_slot_and_reads_busy() {
        let mut script = Script::default();
        let first = script.add_trigger(proto(1), -1);
        script.add_trigger(proto(2), -1);
        script.triggers[0].vars.add_var("x", "1", 0);

        let mut running = script.check_out(first).unwrap();
        assert_eq!(running.vars.get("x").map(|v| v.value.as_str()), Some("1"));
        assert_eq!(vnums(&script), vec![1, 2]);
        assert!(script.is_attached(1));
        assert!(script.check(trig::MTRIG_GREET));
        assert!(!script.triggers[0].check(trig::MTRIG_GREET));
        assert!(script.check_out(first).is_none());

        running.vars.add_var("x", "2", 0);
        assert!(script.check_in(running));
        assert!(script.triggers[0].check(trig::MTRIG_GREET));
        assert_eq!(
            script.triggers[0].vars.get("x").map(|v| v.value.as_str()),
            Some("2")
        );

        // Detached during its own run: the instance has nowhere to go back to.
        let running = script.check_out(first).unwrap();
        script.triggers.retain(|t| t.id != first);
        assert!(!script.check_in(running));
    }
}
