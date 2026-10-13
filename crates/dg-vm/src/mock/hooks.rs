//! A data-driven hook for changing the world from inside
//! [`BaseHost::mud_command`].
//!
//! Real `mud_command` runs `command_interpreter`, which can do anything: a
//! forced `north` moves the character, a `quit` extracts it, a social fires
//! `act` triggers. The mock cannot interpret commands, so a test says in
//! advance what a command it expects does: it queues a [`MockHook`], and the
//! first later `mud_command` call that matches runs the hook's
//! [`MockAction`] right after the call is recorded in
//! [`MockHost::mud_commands`].
//!
//! A hook matches a call when its `ch` is `None` or the character that
//! runs the command, and the command line starts with its `prefix` (an empty
//! prefix matches every line; leading spaces on the line are skipped, as
//! `command_interpreter` skips them, and `mat` and `force` do pass one). A hook runs once and is removed *before* its action, so an
//! action that runs more commands can match the hooks queued behind it.
//!
//! ```
//! # use dg_vm::mock::{MockAction, MockHost};
//! # use dg_vm::host::{BaseHost, WorldHost};
//! let mut host = MockHost::new();
//! let room = host.add_room(3001, "Temple");
//! let guard = host.add_mob(3000, "a guard");
//! host.char_to_room(guard, room);
//! // When the guard next runs a `say`, the guard is removed from the game.
//! host.hook(Some(guard), "say", MockAction::ExtractChar(guard));
//! host.mud_command(guard, "say hello");
//! assert!(host.is_dead(guard));
//! ```

use crate::host::BaseHost;
use crate::mock::MockHost;
use crate::types::Uid;

/// What a [`MockHook`] does to the world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockAction {
    /// `char_from_room` then `char_to_room`: the character is put at the head
    /// of `room`'s people, as a command that walks it there would. A room
    /// that does not exist leaves it where it is.
    MoveChar { ch: Uid, room: Uid },
    /// `obj_from_anywhere` then `obj_to_room`: the object is appended to the
    /// room's contents.
    ObjToRoom { obj: Uid, room: Uid },
    /// `obj_from_anywhere` then `obj_to_char`: the object goes to the head of
    /// the character's inventory.
    ObjToChar { obj: Uid, ch: Uid },
    /// `extract_char`: the character is marked dead and stays in its room
    /// until [`MockHost::sweep_dead`].
    ExtractChar(Uid),
    /// `extract_obj`: the object is gone at once.
    ExtractObj(Uid),
    /// `read_mobile(vnum)` into the room the character `near` is in, at the
    /// head of its people (the `load` of a command; no load trigger runs).
    LoadMob { vnum: i32, near: Uid },
    /// `add_var` of a global variable on the owner's script (created if
    /// absent), as a command that ran `global` would.
    SetVar {
        owner: Uid,
        name: String,
        value: String,
    },
    /// Clear [`VmState::act_check`](crate::types::VmState), as the
    /// `act(..., DG_NO_TRIG)` inside `do_say` does.
    ActNoTrig,
    /// Record `depth <n>` with [`MockHost::effect`], `n` being
    /// [`VmState::depth`](crate::types::VmState) as it is when the command
    /// runs: how deeply triggers are nested at that point.
    RecordDepth,
    /// Fire a trigger through the dispatch module.
    Fire(MockEvent),
}

/// An event [`MockAction::Fire`] raises through [`crate::dispatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockEvent {
    /// `load_mtrigger(ch)`.
    LoadMob(Uid),
    /// `load_otrigger(obj)`.
    LoadObj(Uid),
    /// `greet_mtrigger(actor, dir)`.
    Greet { actor: Uid, dir: i32 },
    /// `enter_wtrigger(room, actor, dir)`.
    Enter { room: Uid, actor: Uid, dir: i32 },
    /// `speech_mtrigger(actor, text)`.
    Speech { actor: Uid, text: String },
}

/// One queued hook (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockHook {
    /// Only calls for this character match; `None` matches any.
    pub ch: Option<Uid>,
    /// Only command lines starting with this match.
    pub prefix: String,
    pub action: MockAction,
}

impl MockHost {
    /// Queue a hook behind the ones already waiting.
    pub fn hook(&mut self, ch: Option<Uid>, prefix: &str, action: MockAction) {
        self.hooks.push_back(MockHook {
            ch,
            prefix: prefix.to_owned(),
            action,
        });
    }

    /// Run the first queued hook that matches this `mud_command` call.
    pub(super) fn run_hooks(&mut self, ch: Uid, line: &str) {
        let Some(at) = self.hooks.iter().position(|hook| {
            hook.ch.is_none_or(|only| only == ch) && line.trim_start().starts_with(&hook.prefix)
        }) else {
            return;
        };
        let Some(hook) = self.hooks.remove(at) else {
            return;
        };
        self.perform(hook.action);
    }

    fn perform(&mut self, action: MockAction) {
        use crate::dispatch as d;
        match action {
            MockAction::MoveChar { ch, room } => {
                if self.exists(room) {
                    self.char_from_room(ch);
                    self.char_to_room(ch, room);
                }
            }
            MockAction::ObjToRoom { obj, room } => {
                if self.exists(obj) && self.exists(room) {
                    self.obj_from_anywhere(obj);
                    self.obj_to_room(obj, room);
                }
            }
            MockAction::ObjToChar { obj, ch } => {
                if self.exists(obj) && self.exists(ch) {
                    self.obj_from_anywhere(obj);
                    self.obj_to_char(obj, ch);
                }
            }
            MockAction::ExtractChar(ch) => self.extract_char(ch),
            MockAction::ExtractObj(obj) => self.extract_obj(obj),
            MockAction::LoadMob { vnum, near } => {
                let room = crate::host::WorldHost::in_room(self, near);
                let mob = crate::commands::CmdHost::cmd_read_mobile(self, vnum);
                if let (Some(mob), Some(room)) = (mob, room) {
                    self.char_to_room(mob, room);
                }
            }
            MockAction::SetVar { owner, name, value } => {
                if let Some(script) = self.ensure_script(owner) {
                    script.global_vars.add_var(&name, &value, 0);
                }
            }
            MockAction::RecordDepth => {
                let depth = self.vm.depth;
                self.effect(format!("depth {depth}"));
            }
            MockAction::ActNoTrig => self.vm.act_check = false,
            MockAction::Fire(event) => match event {
                MockEvent::LoadMob(ch) => d::load_mtrigger(self, ch),
                MockEvent::LoadObj(obj) => d::load_otrigger(self, obj),
                MockEvent::Greet { actor, dir } => {
                    d::greet_mtrigger(self, actor, dir);
                }
                MockEvent::Enter { room, actor, dir } => {
                    d::enter_wtrigger(self, room, actor, dir);
                }
                MockEvent::Speech { actor, text } => d::speech_mtrigger(self, actor, &text),
            },
        }
    }
}
