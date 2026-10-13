//! The host contract: how the VM reaches the game world.
//!
//! [`DgHost`] is the union of small sub-traits. [`BaseHost`] (here) carries
//! what the interpreter core needs: script storage, existence, randomness,
//! time, the wait queue, logging. Each other sub-trait lives beside the module
//! that uses it and holds that module's world primitives:
//!
//! | sub-trait | module | covers |
//! |---|---|---|
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

use std::sync::Arc;

use crate::types::{AttachType, MudTime, Script, TrigInstance, TrigProto, Uid};

/// What the interpreter core needs from the world.
pub trait BaseHost {
    /// Whether this id names a live entity (`find_char`/`find_obj`/
    /// `find_room` returning non-NULL).
    fn exists(&self, uid: Uid) -> bool;

    /// `SCRIPT(go)`: the entity's script, if it has one.
    fn script(&self, uid: Uid) -> Option<&Script>;

    /// `SCRIPT(go)` for mutation, if it has one.
    fn script_mut(&mut self, uid: Uid) -> Option<&mut Script>;

    /// The entity's script, created empty if absent (the C's
    /// `if (!SCRIPT(x)) CREATE(SCRIPT(x), …)`). `None` when the entity does
    /// not exist.
    fn ensure_script(&mut self, uid: Uid) -> Option<&mut Script>;

    /// `script_driver`'s `static int depth`: the current trigger nesting.
    fn depth_mut(&mut self) -> &mut u32;

    /// `rand_number(from, to)`, inclusive. `from > to` is swapped, as in the C.
    fn rand_number(&mut self, from: i64, to: i64) -> i64;

    /// `time_info`.
    fn mud_time(&self) -> MudTime;

    /// `read_trigger(real_trigger(vnum))`: the prototype for `attach`.
    fn trig_proto(&self, vnum: i32) -> Option<Arc<TrigProto>>;

    /// Queue a `wait`: after `pulses` (tenths of a second) the game calls
    /// [`crate::driver::resume`] for this owner and trigger instance. Replaces
    /// `event_create(trig_wait_event, …)`.
    fn schedule_wait(&mut self, owner: Uid, trigger_id: u64, pulses: i64);

    /// `command_interpreter(ch, line)` for a mob: the fallback for a script
    /// line that is not a script command (`say`, `give`, `north`, a social…).
    /// Only ever called for a character owner.
    fn mud_command(&mut self, mob: Uid, line: &str);

    /// `script_log`: a script error a builder should see.
    fn script_log(&mut self, message: &str);

    /// Something DG does that this host does not model. `what` is a short
    /// stable key (`"char.pref"`, `"mhunt"`); `detail` is free text.
    fn unsupported(&mut self, what: &str, detail: &str);
}

/// Everything the VM needs from the world.
pub trait DgHost:
    BaseHost
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
    T: BaseHost
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
/// `trig` is the running instance, **taken out of** the owner's
/// [`Script::triggers`] for the duration of the run and put back when it
/// unwinds, so the interpreter can hold it mutably while the host is free to
/// touch the owner's script (its globals, its other triggers). `sc` is reached
/// through `host.script(owner)`.
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

    /// `script_log` with the C's standard prefix naming the trigger.
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
