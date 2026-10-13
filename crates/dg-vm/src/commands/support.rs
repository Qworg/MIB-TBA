//! Test scaffolding for the command modules: a small world and a way to run
//! one command line as a mob, object or room.

use std::sync::Arc;

use crate::host::Cx;
use crate::mock::MockHost;
use crate::types::{AttachType, TrigInstance, TrigProto, Uid};

/// Room 3001 (with a lamp, a guard and two players) and room 3002 to its
/// north, plus loadable prototypes.
pub struct World {
    pub host: MockHost,
    pub room: Uid,
    pub room2: Uid,
    pub mob: Uid,
    pub obj: Uid,
    pub player: Uid,
    pub player2: Uid,
    pub proto: Arc<TrigProto>,
}

impl World {
    pub fn new() -> Self {
        let mut host = MockHost::new();
        let room = host.add_room(3001, "The Temple");
        let room2 = host.add_room(3002, "The Altar");
        host.set_link(room, "exit:0", Some(room2));
        host.set_link(room2, "exit:2", Some(room));
        let mob = host.add_mob(3000, "the guard");
        host.set_text(mob, "alias", "guard cityguard");
        let obj = host.add_obj(7000, "a lamp");
        host.set_text(obj, "alias", "lamp");
        let player = host.add_player("Alice");
        let player2 = host.add_player("Bob");
        for ch in [mob, player, player2] {
            host.char_to_room(ch, room);
            host.set_num(ch, "pos", 8);
            host.set_num(ch, "hit", 20);
            host.set_num(ch, "max_hit", 20);
        }
        for ch in [player, player2] {
            host.set_num(ch, "desc", 1);
        }
        host.obj_to_room(obj, room);
        host.globals
            .texts
            .insert("mobproto:3100".to_owned(), "a rat".to_owned());
        host.globals
            .texts
            .insert("objproto:7100".to_owned(), "a sack".to_owned());
        host.globals
            .nums
            .insert("objproto:7100:type".to_owned(), 15);
        host.globals
            .texts
            .insert("objproto:7101".to_owned(), "a ring".to_owned());
        // Takeable and wearable on a finger.
        host.globals.nums.insert("objproto:7101:wear".to_owned(), 3);
        host.globals
            .texts
            .insert("objproto:7102".to_owned(), "a rock".to_owned());
        host.globals
            .texts
            .insert("objproto:7000".to_owned(), "a lamp".to_owned());
        // A GLOBAL trigger ('a'): no event the commands raise (a load, an
        // entry, an act line) can fire it, so the fixture does not depend on
        // the dispatch stubs being inert.
        let proto = host.add_proto(900, AttachType::Mob, "a", 100, "", "say hi");
        for owner in [mob, obj, room] {
            host.attach(owner, &proto);
        }
        Self {
            host,
            room,
            room2,
            mob,
            obj,
            player,
            player2,
            proto,
        }
    }

    fn cx_run<R>(
        &mut self,
        owner: Uid,
        attach: AttachType,
        run: impl FnOnce(&mut Cx<'_, MockHost>) -> R,
    ) -> R {
        let mut trig = TrigInstance::new(99, Arc::clone(&self.proto));
        let mut cx = Cx {
            host: &mut self.host,
            owner,
            attach,
            trig: &mut trig,
        };
        run(&mut cx)
    }

    /// `script_command_interpreter(mob, line)`.
    pub fn run_mob(&mut self, mob: Uid, line: &str) -> bool {
        self.cx_run(mob, AttachType::Mob, |cx| {
            super::script_command_interpreter(cx, line)
        })
    }

    /// `obj_command_interpreter(obj, line)`.
    pub fn run_obj(&mut self, obj: Uid, line: &str) {
        self.cx_run(obj, AttachType::Obj, |cx| {
            super::obj_command_interpreter(cx, line);
        });
    }

    /// `wld_command_interpreter(room, line)`.
    pub fn run_wld(&mut self, room: Uid, line: &str) {
        self.cx_run(room, AttachType::Wld, |cx| {
            super::wld_command_interpreter(cx, line);
        });
    }

    /// Set or clear `PRF_NOHASSLE`, in both places the mock reads it
    /// (`valid_dg_target` reads the shared `nohassle` key, `char_pref_flagged`
    /// the `pref` bit vector).
    pub fn set_nohassle(&mut self, ch: Uid, on: bool) {
        self.host.set_num(ch, "nohassle", i64::from(on));
        self.set_bit(ch, "pref", 8, on);
    }

    /// Set or clear `PLR_WRITING`.
    pub fn set_writing(&mut self, ch: Uid, on: bool) {
        self.set_bit(ch, "act", 4, on);
    }

    /// Set or clear one bit of a bit-vector number.
    pub fn set_bit(&mut self, ch: Uid, key: &str, bit: u32, on: bool) {
        let bits = self.host.num(ch, key);
        let mask = 1i64 << bit;
        self.host
            .set_num(ch, key, if on { bits | mask } else { bits & !mask });
    }

    /// Every text delivered to `ch` so far.
    pub fn sent_to(&self, ch: Uid) -> Vec<String> {
        let prefix = format!("send {ch}: ");
        self.host
            .effects
            .iter()
            .filter_map(|effect| effect.strip_prefix(&prefix).map(str::to_owned))
            .collect()
    }

    /// The effects that start with `prefix`.
    pub fn effects(&self, prefix: &str) -> Vec<String> {
        self.host
            .effects
            .iter()
            .filter(|effect| effect.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// The last script log line.
    pub fn log(&self) -> String {
        self.host.logs.last().cloned().unwrap_or_default()
    }

    /// Forget recorded effects and log lines.
    pub fn clear(&mut self) {
        self.host.effects.clear();
        self.host.logs.clear();
    }
}
