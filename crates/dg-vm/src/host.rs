//! The host contract: how the VM reaches the game world.
//!
//! [`DgHost`] is the union of small sub-traits. [`BaseHost`] (here) carries
//! what the interpreter core needs: script storage, existence, randomness,
//! time, the wait queue, logging. [`WorldHost`] (here) is the world graph
//! every module shares. Each other sub-trait lives beside the module
//! that uses it and holds that module's world primitives:
//!
//! | sub-trait | module | covers |
//! |---|---|---|
//! | [`WorldHost`] | here | the world graph: what exists and where it is |
//! | [`crate::subst::SubstHost`] | `subst` | name lookup and the special variables |
//! | [`crate::fields_char::CharHost`] | `fields_char` | character attributes |
//! | [`crate::fields_obj::ObjHost`] | `fields_obj` | object attributes |
//! | [`crate::fields_room::RoomHost`] | `fields_room` | room attributes and exits |
//! | [`crate::commands::CmdHost`] | `commands` | the effects of script commands |
//! | [`crate::magic::MagicHost`] | `magic` | spells and affects |
//! | [`crate::dispatch::DispatchHost`] | `dispatch` | enumerating who can react to an event |
//!
//! # Rules for primitives
//!
//! A primitive is **data-level and typed**: it reads or changes one fact about
//! the world (`GET_GOLD(ch)`, `obj_to_char(obj, ch)`). All DG parsing, quirks
//! and text formatting stay in the VM, where the mock can test them. A
//! primitive never takes a DG command string to interpret, with the single
//! exception of [`BaseHost::mud_command`].
//!
//! Every primitive documents the C expression it stands for. A host that does
//! not model something returns the neutral value (`0`, empty, `None`) and
//! reports it through [`BaseHost::unsupported`], so the gap is counted rather
//! than silent.
//!
//! Entities are named by [`Uid`]. A primitive handed an id that no longer
//! exists returns the neutral value; it never panics.
//!
//! # Naming
//!
//! `DgHost` unions the sub-traits and every VM function is generic over it,
//! so two sub-traits declaring the same method name make every call to it
//! ambiguous. Therefore:
//!
//! - a sub-trait never re-declares something [`BaseHost`] or [`WorldHost`]
//!   already provides — use theirs;
//! - every method a module's own sub-trait adds carries that module's prefix:
//!   `subst_`, `char_`, `obj_`, `room_`, `cmd_`, `magic_`, `disp_`.
//!
//! # Extraction
//!
//! Objects are removed immediately (`extract_obj`): afterwards
//! [`BaseHost::exists`] is false. Characters are not (`extract_char` only
//! marks them `NOTDEADYET` until the end of the pulse): a purged or dead
//! character still exists and is still in its room's list, with
//! [`WorldHost::is_dead`] true, until the host sweeps it. Code that the C
//! guards with `DEAD(ch)` must test `is_dead`.

use std::sync::Arc;

use crate::types::{
    AttachType, Kind, MudTime, Script, ScriptMemory, TrigInstance, TrigProto, Uid, VmState,
};

/// What the interpreter core needs from the world.
pub trait BaseHost {
    /// Whether this id names a live entity (`find_char`/`find_obj`/
    /// `find_room` returning non-NULL). See the module notes on extraction.
    fn exists(&self, uid: Uid) -> bool;

    /// `SCRIPT(go)`: the entity's script, if it has one.
    fn script(&self, uid: Uid) -> Option<&Script>;

    /// `SCRIPT(go)` for mutation, if it has one.
    fn script_mut(&mut self, uid: Uid) -> Option<&mut Script>;

    /// The entity's script, created empty if absent (the C's
    /// `if (!SCRIPT(x)) CREATE(SCRIPT(x), …)`). `None` when the entity does
    /// not exist.
    fn ensure_script(&mut self, uid: Uid) -> Option<&mut Script>;

    /// `extract_script`'s `SCRIPT(go) = NULL`: take the entity's script away,
    /// globals and all. The caller cancels the triggers' waits first.
    fn remove_script(&mut self, uid: Uid) -> Option<Script>;

    /// The C globals and statics the interpreter shares across nested runs
    /// (see [`VmState`]). The host stores one for the life of the world.
    fn vm_state(&mut self) -> &mut VmState;

    /// `CONFIG_SCRIPT_PLAYERS`: whether triggers may be attached to players.
    fn script_players(&self) -> bool;

    /// The global `pulse` counter (tenths of a second), which `wait until`
    /// reads to find the time of day.
    fn pulse(&self) -> i64;

    /// `rand_number(from, to)`, inclusive. `from > to` is swapped, as in the C.
    fn rand_number(&mut self, from: i64, to: i64) -> i64;

    /// `time_info`.
    fn mud_time(&self) -> MudTime;

    /// `read_trigger(real_trigger(vnum))`: the prototype for `attach`.
    fn trig_proto(&self, vnum: i32) -> Option<Arc<TrigProto>>;

    /// Queue a `wait`: after `pulses` (tenths of a second) the game calls
    /// [`crate::driver::resume`] for this owner and trigger instance. Replaces
    /// `event_create(trig_wait_event, …)`; like it, fewer than 1 pulse is 1.
    fn schedule_wait(&mut self, owner: Uid, trigger_id: u64, pulses: i64);

    /// `event_cancel(GET_TRIG_WAIT(trig))`: forget a queued wait, if any.
    /// Called when a trigger is detached or its script extracted, so a stale
    /// wait can never resume a later trigger that reuses the id.
    fn cancel_wait(&mut self, owner: Uid, trigger_id: u64);

    /// `command_interpreter(ch, line)`: have a character — any character, a
    /// player included — perform an ordinary game command. This is the
    /// fallback for a mob script line that is not a script command (`say`,
    /// `give`, `north`, a social…), and what `force`, `at` and a remembered
    /// command run. The command may itself fire triggers.
    fn mud_command(&mut self, ch: Uid, line: &str);

    /// The `mudcommand` text function: the full name of the first game
    /// command that `prefix` abbreviates (the first `cmd_info` entry with
    /// `strncmp(command, prefix, strlen(prefix)) == 0`), or `None`.
    fn mud_command_name(&self, prefix: &str) -> Option<String>;

    /// `script_log`: a script error a builder should see.
    fn script_log(&mut self, message: &str);

    /// Something DG does that this host does not model. `what` is a short
    /// stable key (`"char.pref"`, `"mhunt"`); `detail` is free text.
    fn unsupported(&mut self, what: &str, detail: &str);
}

/// The world graph: what exists, what it is called, and where it is.
///
/// These are the primitives [`crate::lookup`] resolves names with, and the
/// common base every other sub-trait builds on. Use them rather than adding a
/// second primitive for the same fact.
///
/// The `Vec`s are in the order the C walks the corresponding linked list
/// (`room->people`, `ch->carrying`, `character_list`, …), head first. Script
/// behaviour depends on that order: "the first guard in the room" is whoever
/// is nearest the head.
pub trait WorldHost {
    /// What kind of entity this id names, or `None` if it names nothing live.
    fn kind(&self, uid: Uid) -> Option<Kind>;

    /// `IS_NPC(ch)`.
    fn is_npc(&self, ch: Uid) -> bool;

    /// `valid_dg_target(ch, allow_gods ? DG_ALLOW_GODS : 0)` (`dg_misc.c`):
    /// whether scripts may target this character. NPCs and ordinary playing
    /// characters always are.
    fn valid_dg_target(&self, ch: Uid, allow_gods: bool) -> bool;

    /// `GET_NAME(ch)` / `obj->short_description` / `room->name`.
    fn name(&self, uid: Uid) -> String;

    /// The keyword list names are matched against: `ch->player.name` (for a
    /// player, their name) / `obj->name`.
    fn keywords(&self, uid: Uid) -> String;

    /// `GET_MOB_VNUM(ch)` / `GET_OBJ_VNUM(obj)` / `room->number`. An entity
    /// with no prototype (a player, an object made without one) answers
    /// `-1`: the C's `NOBODY` / `NOTHING` is `(IDXTYPE)~0`, 65535, and the VM
    /// maps any negative vnum to it ([`crate::lookup::c_vnum`]). Vnums are 16
    /// bits in the reference build (`CIRCLE_UNSIGNED_INDEX`), so a host never
    /// reports one above 65534.
    fn vnum(&self, uid: Uid) -> i32;

    /// `IN_ROOM(ch)` / `IN_ROOM(obj)`: the room a character is in, or the room
    /// an object lies in (`None` when it is carried, worn or contained).
    fn in_room(&self, uid: Uid) -> Option<Uid>;

    /// `room->people`.
    fn room_people(&self, room: Uid) -> Vec<Uid>;

    /// `room->contents`.
    fn room_contents(&self, room: Uid) -> Vec<Uid>;

    /// `ch->carrying`.
    fn carrying(&self, ch: Uid) -> Vec<Uid>;

    /// `GET_EQ(ch, pos)`, `pos` in `0..NUM_WEARS` (see [`crate::lookup`]).
    fn equipment(&self, ch: Uid, pos: i32) -> Option<Uid>;

    /// `obj->contains`.
    fn obj_contains(&self, obj: Uid) -> Vec<Uid>;

    /// `obj->carried_by`.
    fn obj_carried_by(&self, obj: Uid) -> Option<Uid>;

    /// `obj->worn_by`.
    fn obj_worn_by(&self, obj: Uid) -> Option<Uid>;

    /// `obj->in_obj`.
    fn obj_in_obj(&self, obj: Uid) -> Option<Uid>;

    /// `character_list`.
    fn character_list(&self) -> Vec<Uid>;

    /// `object_list`.
    fn object_list(&self) -> Vec<Uid>;

    /// `real_room(vnum)`: the room with this vnum.
    fn real_room(&self, vnum: i32) -> Option<Uid>;

    /// `CAN_SEE(ch, target)` for a character target, `CAN_SEE_OBJ(ch, target)`
    /// for an object.
    fn can_see(&self, ch: Uid, target: Uid) -> bool;

    /// `GET_LEVEL(ch)`.
    fn level(&self, ch: Uid) -> i32;

    /// `DEAD(ch)`: extracted this pulse and awaiting the sweep.
    fn is_dead(&self, ch: Uid) -> bool;

    /// `GET_OBJ_TYPE(obj)`: the `ITEM_*` number ([`crate::types::ITEM_CONTAINER`]
    /// is 15). `0` for something that is not an object.
    fn obj_type(&self, obj: Uid) -> i32;

    /// `GET_OBJ_WEAR(obj)` as a bitvector: bit `n` is `ITEM_WEAR_*` number
    /// `n` (`0` take, `1` finger, … `14` hold; see
    /// [`crate::lookup::can_wear_on_pos`]).
    fn wear_flags(&self, obj: Uid) -> u32;

    /// `SCRIPT_MEM(ch)`: who this mob remembers, head first.
    fn script_mem(&self, ch: Uid) -> Vec<ScriptMemory>;

    /// Replace `SCRIPT_MEM(ch)`.
    fn set_script_mem(&mut self, ch: Uid, memory: Vec<ScriptMemory>);
}

/// Everything the VM needs from the world.
pub trait DgHost:
    BaseHost
    + WorldHost
    + crate::subst::SubstHost
    + crate::fields_char::CharHost
    + crate::fields_obj::ObjHost
    + crate::fields_room::RoomHost
    + crate::commands::CmdHost
    + crate::magic::MagicHost
    + crate::dispatch::DispatchHost
{
}

impl<T> DgHost for T where
    T: ?Sized
        + BaseHost
        + WorldHost
        + crate::subst::SubstHost
        + crate::fields_char::CharHost
        + crate::fields_obj::ObjHost
        + crate::fields_room::RoomHost
        + crate::commands::CmdHost
        + crate::magic::MagicHost
        + crate::dispatch::DispatchHost
{
}

/// The state one running trigger carries through the interpreter: the C's
/// `(go, sc, trig, type)` argument quadruple.
///
/// `trig` is the running instance, **checked out of** the owner's script
/// with [`Script::check_out`] for the duration of the run and returned with
/// [`Script::check_in`] when it unwinds or waits. The interpreter can then
/// hold it mutably while the host stays free to touch the owner's script (its
/// globals, its other triggers); the slot left behind keeps the list looking
/// as it does in the C. `sc` is reached through `host.script(owner)`.
///
/// `attach` is the **owner's** kind (`AttachType::from(kind)`), never the
/// prototype's `attach_type`: the C runs anything attached to a character as
/// a `MOB_TRIGGER`.
///
/// A trigger can detach itself mid-run. The C frees it and carries on down
/// the shared command list with its variables gone; here the run likewise
/// continues, with `trig.vars` cleared at the detach, and when it ends or
/// waits `check_in` fails, so the instance is dropped and no wait is queued.
pub struct Cx<'a, H: DgHost + ?Sized> {
    pub host: &'a mut H,
    /// `go`: the entity the trigger is attached to.
    pub owner: Uid,
    /// `type`.
    pub attach: AttachType,
    /// `trig`.
    pub trig: &'a mut TrigInstance,
}

impl<H: DgHost + ?Sized> Cx<'_, H> {
    /// `sc->context`, or `0` when the owner has no script.
    pub fn context(&self) -> i64 {
        self.host
            .script(self.owner)
            .map_or(0, |script| script.context)
    }

    /// `script_log` with the C's commonest prefix naming the trigger
    /// (`"Trigger: %s, VNum %d. …"`). Messages the C words differently go to
    /// `host.script_log` directly.
    pub fn log(&mut self, message: &str) {
        let line = format!(
            "Trigger: {}, VNum {}. {}",
            self.trig.proto.name, self.trig.proto.vnum, message
        );
        self.host.script_log(&line);
    }

    /// Reborrow for a nested call.
    pub fn reborrow(&mut self) -> Cx<'_, H> {
        Cx {
            host: &mut *self.host,
            owner: self.owner,
            attach: self.attach,
            trig: &mut *self.trig,
        }
    }
}
