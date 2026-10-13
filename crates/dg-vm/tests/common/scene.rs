//! A scene for the scenario tests: the standard cast plus a trigger owner
//! and a bystander with a descriptor.

use super::world::{Fixture, dress_mob, dress_player};
use dg_vm::mock::MockHost;
use dg_vm::types::Uid;

/// Room 3001 holds the player "Tester" (male, level 10), "Watcher" (another
/// player with a descriptor), the guard mob 3100 "a city guard" (no
/// descriptor) and the bench 3020. Lists are head first: Watcher, guard,
/// Tester.
pub struct Scene {
    pub fx: Fixture,
    pub watcher: Uid,
}

impl Scene {
    pub fn new() -> Self {
        let mut fx = Fixture::new();
        let watcher = fx.host.add_player("Watcher");
        dress_player(&mut fx.host, watcher, 5);
        fx.host.char_to_room(watcher, fx.room);
        Self { fx, watcher }
    }

    pub fn host(&mut self) -> &mut MockHost {
        &mut self.fx.host
    }

    pub fn player(&self) -> Uid {
        self.fx.player
    }

    pub fn guard(&self) -> Uid {
        self.fx.guard
    }

    pub fn room(&self) -> Uid {
        self.fx.room
    }

    /// A mob carrying triggers, standing in the room.
    pub fn add_owner_mob(&mut self, vnum: i32, name: &str) -> Uid {
        let room = self.fx.room;
        let mob = self.fx.host.add_mob(vnum, name);
        dress_mob(&mut self.fx.host, mob, 30);
        self.fx.host.char_to_room(mob, room);
        mob
    }

    /// An object lying in the room.
    pub fn add_room_obj(&mut self, vnum: i32, name: &str) -> Uid {
        let room = self.fx.room;
        let obj = self.fx.host.add_obj(vnum, name);
        self.fx.host.set_text(obj, "alias", name);
        self.fx.host.obj_to_room(obj, room);
        obj
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything sent to `ch` through `send_to_char`, in order, without the
/// line end (`sub_write` ends a message in `"\n\r"`, `act` in `"\r\n"`).
pub fn heard(host: &MockHost, ch: Uid) -> Vec<String> {
    let prefix = format!("send {ch}: ");
    host.effects
        .iter()
        .filter_map(|effect| effect.strip_prefix(&prefix))
        .map(|text| {
            text.strip_suffix("\n\r")
                .or_else(|| text.strip_suffix("\r\n"))
                .unwrap_or(text)
                .to_owned()
        })
        .collect()
}

/// The `mud_command` log as `(owner, line)`.
pub fn said(host: &MockHost) -> Vec<(Uid, String)> {
    host.mud_commands.clone()
}

/// Effects other than `send`.
pub fn other_effects(host: &MockHost) -> Vec<String> {
    host.effects
        .iter()
        .filter(|effect| !effect.starts_with("send "))
        .cloned()
        .collect()
}

/// The end state every finished run must share: the depth counter is back to
/// zero and no instance is left checked out of its slot.
pub fn assert_clean(host: &MockHost) {
    assert_eq!(host.depth, 0, "depth counter");
    assert!(super::world::checked_out(host).is_empty());
}

/// The `vnum` of each entity.
pub fn vnums(host: &MockHost, list: &[Uid]) -> Vec<i64> {
    list.iter().map(|uid| host.num(*uid, "vnum")).collect()
}
