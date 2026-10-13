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

use crate::host::{BaseHost, WorldHost};
use crate::mock::MockHost;
use crate::types::{Kind, Uid};

/// What a [`MockHook`] does to the world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockAction {
    /// `char_from_room` then `char_to_room`: the character is put at the head
    /// of `room`'s people, as a command that walks it there would. A
    /// character or room that does not exist leaves things as they are.
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

/// One queued hook (see [`MockHost::hook`] for how it is matched).
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
    ///
    /// A hook matches a `mud_command` call when `ch` is `None` or the
    /// character that runs the command, and the command line starts with
    /// `prefix` (an empty prefix matches every line; leading spaces on the
    /// line are skipped, as `command_interpreter` skips them, and `mat` and
    /// `force` do pass one). The first queued hook that matches runs, once,
    /// after the call is recorded in [`MockHost::mud_commands`]; it is removed
    /// *before* its action, so an action that runs more commands can match
    /// the hooks queued behind it.
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

    fn exists_as(&self, uid: Uid, kind: Kind) -> bool {
        self.exists(uid) && WorldHost::kind(self, uid) == Some(kind)
    }

    fn perform(&mut self, action: MockAction) {
        use crate::dispatch as d;
        match action {
            MockAction::MoveChar { ch, room } => {
                if self.exists_as(ch, Kind::Char) && self.exists_as(room, Kind::Room) {
                    self.char_from_room(ch);
                    self.char_to_room(ch, room);
                }
            }
            MockAction::ObjToRoom { obj, room } => {
                if self.exists_as(obj, Kind::Obj) && self.exists_as(room, Kind::Room) {
                    self.obj_from_anywhere(obj);
                    self.obj_to_room(obj, room);
                }
            }
            MockAction::ObjToChar { obj, ch } => {
                if self.exists_as(obj, Kind::Obj) && self.exists_as(ch, Kind::Char) {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::commands::support::World;
    use crate::types::AttachType;

    fn ran(w: &World, owner: Uid) -> bool {
        w.host
            .script(owner)
            .and_then(|script| script.global_vars.get("ran").map(|var| var.value.clone()))
            .as_deref()
            == Some("1")
    }

    const RAN: &str = "set ran 1\nglobal ran";

    #[test]
    fn matching_takes_the_first_queued_hook_by_character_and_prefix() {
        let mut w = World::new();
        let (alice, bob, guard, room2) = (w.player, w.player2, w.mob, w.room2);
        w.host
            .hook(Some(bob), "say", MockAction::ExtractChar(guard));
        w.host.hook(
            None,
            "say",
            MockAction::MoveChar {
                ch: alice,
                room: room2,
            },
        );
        // Wrong character, wrong prefix (a substring is not a prefix).
        w.host.mud_command(alice, "x say hi");
        assert!(w.host.hooks.len() == 2);
        // Leading spaces are skipped; the first hook whose character does not
        // match is passed over for the second.
        w.host.mud_command(alice, "   say hi");
        assert_eq!(w.host.in_room(alice), Some(room2));
        assert!(!w.host.is_dead(guard));
        // The first hook is still waiting, and runs for its own character.
        w.host.mud_command(bob, "say hi");
        assert!(w.host.is_dead(guard));
        assert!(w.host.hooks.is_empty());
    }

    #[test]
    fn the_first_of_two_matching_hooks_runs_and_only_once() {
        let mut w = World::new();
        let (alice, guard, lamp) = (w.player, w.mob, w.obj);
        w.host.hook(None, "", MockAction::ExtractChar(guard));
        w.host.hook(None, "", MockAction::ExtractObj(lamp));
        w.host.mud_command(alice, "anything");
        assert!(w.host.is_dead(guard));
        assert!(w.host.exists(lamp));
        w.host.mud_command(alice, "anything");
        assert!(!w.host.exists(lamp));
    }

    #[test]
    fn the_action_runs_after_the_call_is_recorded_and_after_the_hook_is_removed() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        // The action of the first hook sees its own call already recorded.
        w.host.hook(None, "a", MockAction::RecordDepth);
        w.host.hook(None, "b", MockAction::ActNoTrig);
        w.host.vm.act_check = true;
        w.host.mud_command(alice, "a");
        assert_eq!(w.host.mud_commands, [(alice, "a".to_owned())]);
        assert_eq!(w.host.effects, ["depth 0"]);
        assert_eq!(w.host.hooks.len(), 1);
        assert!(w.host.vm.act_check);
        w.host.mud_command(bob, "b");
        assert!(!w.host.vm.act_check);
    }

    #[test]
    fn a_nested_command_can_match_the_hook_queued_behind() {
        // The hook is gone before its action runs, so the `say` a speech
        // trigger makes inside the action finds the next hook.
        let mut w = World::new();
        let (alice, guard, lamp) = (w.player, w.mob, w.obj);
        let proto = w
            .host
            .add_proto(806, AttachType::Mob, "d", 0, "hello", "say nested");
        w.host.attach(guard, &proto);
        w.host.hook(
            None,
            "",
            MockAction::Fire(MockEvent::Speech {
                actor: alice,
                text: "hello".to_owned(),
            }),
        );
        w.host.hook(None, "", MockAction::ExtractObj(lamp));
        w.host.mud_command(alice, "go");
        assert_eq!(
            w.host.mud_commands,
            [(alice, "go".to_owned()), (guard, "say nested".to_owned())]
        );
        assert!(!w.host.exists(lamp));
    }

    #[test]
    fn move_char_and_obj_actions_need_real_entities_of_the_right_kind() {
        let mut w = World::new();
        let (alice, guard, lamp, room, room2) = (w.player, w.mob, w.obj, w.room, w.room2);
        let before = w.host.room_people(room);
        let lamp_carrier = w.host.obj_carried_by(lamp);
        let lamp_room = w.host.in_room(lamp);
        let room_items = w.host.room_contents(room);
        w.host
            .hook(None, "", MockAction::MoveChar { ch: 999_999, room });
        w.host.mud_command(alice, "1");
        w.host.hook(
            None,
            "",
            MockAction::MoveChar {
                ch: alice,
                room: guard,
            },
        );
        w.host.mud_command(alice, "2");
        w.host.hook(
            None,
            "",
            MockAction::MoveChar {
                ch: lamp,
                room: room2,
            },
        );
        w.host.mud_command(alice, "3");
        w.host.hook(
            None,
            "",
            MockAction::ObjToRoom {
                obj: guard,
                room: room2,
            },
        );
        w.host.mud_command(alice, "4");
        w.host.hook(
            None,
            "",
            MockAction::ObjToChar {
                obj: lamp,
                ch: room2,
            },
        );
        w.host.mud_command(alice, "5");
        assert_eq!(w.host.room_people(room), before);
        assert!(w.host.room_people(room2).is_empty());
        assert_eq!(w.host.in_room(alice), Some(room));
        assert_eq!(w.host.in_room(guard), Some(room));
        assert!(w.host.room_contents(room2).is_empty());
        assert_eq!(w.host.room_contents(room), room_items);
        assert_eq!(w.host.obj_carried_by(lamp), lamp_carrier);
        assert_eq!(w.host.in_room(lamp), lamp_room);
        assert!(w.host.hooks.is_empty());
    }

    #[test]
    fn object_actions_move_the_object() {
        let mut w = World::new();
        let (alice, lamp, room2) = (w.player, w.obj, w.room2);
        w.host.hook(
            None,
            "",
            MockAction::ObjToChar {
                obj: lamp,
                ch: alice,
            },
        );
        w.host.mud_command(alice, "x");
        assert_eq!(w.host.obj_carried_by(lamp), Some(alice));
        w.host.hook(
            None,
            "",
            MockAction::ObjToRoom {
                obj: lamp,
                room: room2,
            },
        );
        w.host.mud_command(alice, "x");
        assert_eq!(w.host.obj_carried_by(lamp), None);
        assert_eq!(w.host.room_contents(room2), [lamp]);
        w.host.hook(None, "", MockAction::ExtractObj(lamp));
        w.host.mud_command(alice, "x");
        assert!(!w.host.exists(lamp));
        assert!(w.host.room_contents(room2).is_empty());
    }

    #[test]
    fn load_mob_puts_a_new_mob_at_the_head_of_the_room_of_near() {
        let mut w = World::new();
        let (alice, guard, room) = (w.player, w.mob, w.room);
        w.host.hook(
            None,
            "",
            MockAction::LoadMob {
                vnum: 3100,
                near: guard,
            },
        );
        w.host.mud_command(alice, "x");
        let people = w.host.room_people(room);
        assert_eq!(people.len(), 4);
        assert_eq!(w.host.vnum(people[0]), 3100);
    }

    #[test]
    fn set_var_creates_the_script_and_the_global() {
        let mut w = World::new();
        let alice = w.player;
        assert!(w.host.script(alice).is_none());
        w.host.hook(
            None,
            "",
            MockAction::SetVar {
                owner: alice,
                name: "k".to_owned(),
                value: "v".to_owned(),
            },
        );
        w.host.mud_command(alice, "x");
        let value = w
            .host
            .script(alice)
            .and_then(|script| script.global_vars.get("k").map(|var| var.value.clone()));
        assert_eq!(value.as_deref(), Some("v"));
    }

    #[test]
    fn record_depth_reads_the_depth_when_the_command_runs() {
        let mut w = World::new();
        let alice = w.player;
        w.host.vm.depth = 3;
        w.host.hook(None, "", MockAction::RecordDepth);
        w.host.mud_command(alice, "x");
        assert_eq!(w.host.effects, ["depth 3"]);
    }

    fn fire(w: &mut World, event: MockEvent) {
        let alice = w.player;
        w.host.hook(None, "", MockAction::Fire(event));
        w.host.mud_command(alice, "x");
    }

    #[test]
    fn fire_load_mob_runs_the_load_trigger() {
        let mut w = World::new();
        let guard = w.mob;
        let proto = w.host.add_proto(801, AttachType::Mob, "n", 100, "", RAN);
        w.host.attach(guard, &proto);
        assert!(!ran(&w, guard));
        fire(&mut w, MockEvent::LoadMob(guard));
        assert!(ran(&w, guard));
    }

    #[test]
    fn fire_load_obj_runs_the_load_trigger() {
        let mut w = World::new();
        let lamp = w.obj;
        let proto = w.host.add_proto(802, AttachType::Obj, "n", 100, "", RAN);
        w.host.attach(lamp, &proto);
        fire(&mut w, MockEvent::LoadObj(lamp));
        assert!(ran(&w, lamp));
    }

    #[test]
    fn fire_greet_runs_the_greet_trigger_of_an_occupant() {
        let mut w = World::new();
        let (guard, room) = (w.mob, w.room);
        let proto = w.host.add_proto(803, AttachType::Mob, "g", 100, "", RAN);
        w.host.attach(guard, &proto);
        let visitor = w.host.add_mob(3200, "a visitor");
        w.host.char_to_room(visitor, room);
        fire(
            &mut w,
            MockEvent::Greet {
                actor: visitor,
                dir: 0,
            },
        );
        assert!(ran(&w, guard));
    }

    #[test]
    fn fire_enter_runs_the_enter_trigger_of_the_room() {
        let mut w = World::new();
        let (room, alice) = (w.room, w.player);
        let proto = w.host.add_proto(804, AttachType::Wld, "g", 100, "", RAN);
        w.host.attach(room, &proto);
        fire(
            &mut w,
            MockEvent::Enter {
                room,
                actor: alice,
                dir: 0,
            },
        );
        assert!(ran(&w, room));
    }

    #[test]
    fn fire_speech_runs_the_speech_trigger_of_an_occupant() {
        let mut w = World::new();
        let (guard, alice) = (w.mob, w.player);
        let proto = w.host.add_proto(805, AttachType::Mob, "d", 0, "hello", RAN);
        w.host.attach(guard, &proto);
        fire(
            &mut w,
            MockEvent::Speech {
                actor: alice,
                text: "hello there".to_owned(),
            },
        );
        assert!(ran(&w, guard));
        // The same hook run with other words leaves a fresh trigger alone.
        let mut w = World::new();
        let (guard, alice) = (w.mob, w.player);
        let proto = w.host.add_proto(805, AttachType::Mob, "d", 0, "hello", RAN);
        w.host.attach(guard, &proto);
        fire(
            &mut w,
            MockEvent::Speech {
                actor: alice,
                text: "bye".to_owned(),
            },
        );
        assert!(!ran(&w, guard));
    }
}
