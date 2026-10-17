//! Mock worlds and the way triggers are fired at them.
//!
//! A scenario attaches its triggers with [`install`] / [`install_record`] and
//! fires them by calling the dispatch function of the event
//! (`dg_vm::dispatch`). The corpus smoke test does the same for every record
//! and falls back to adding the event variables by hand with
//! [`set_event_vars`] and calling `script_driver` only for a trigger no
//! dispatch function runs. A `wait` is resumed with `driver::resume`.

use std::sync::Arc;

use dg_vm::driver::resume;
use dg_vm::host::BaseHost;
use dg_vm::mock::MockHost;
use dg_vm::script::Program;
use dg_vm::types::{AttachType, TrigProto, Uid, trig::asciiflag_conv, uid_text};

/// A scripted `rand_number` result that wins every percent roll the dispatch
/// functions make (`rand_number(1, 100) <= narg`) for a trigger whose `narg`
/// is at least 1. A dispatch function that rolls takes it before the script's
/// own `%random%` rolls.
pub const PERCENT_ROLL: i64 = 1;

/// The usual cast of a scenario: a central room with an exit each way, a
/// player with an inventory and equipment, another mob, and objects.
pub struct Fixture {
    pub host: MockHost,
    /// Room 3001.
    pub room: Uid,
    /// The neighbouring rooms, indexed by direction (north..down).
    pub neighbours: Vec<Uid>,
    /// Player "Tester", level 10, in `room`.
    pub player: Uid,
    /// Mob 3100 "a city guard", in `room`.
    pub guard: Uid,
    /// Object 3020 "a bench" lying in `room`.
    pub bench: Uid,
    /// Player's inventory, head first: a dagger (3021) then a lantern (3022).
    pub inventory: Vec<Uid>,
    /// Player's equipment: a sword (3023) wielded, a cloak (3024) about.
    pub equipment: Vec<Uid>,
}

/// Give a player the attributes a script reads.
pub fn dress_player(host: &mut MockHost, ch: Uid, level: i64) {
    host.set_num(ch, "level", level);
    host.set_num(ch, "hit", 100);
    host.set_num(ch, "max_hit", 100);
    host.set_num(ch, "mana", 50);
    host.set_num(ch, "max_mana", 50);
    host.set_num(ch, "move", 80);
    host.set_num(ch, "max_move", 80);
    host.set_num(ch, "gold", 500);
    host.set_num(ch, "exp", 1000);
    host.set_num(ch, "sex", 1);
    host.set_num(ch, "pos", 8);
    host.set_num(ch, "desc", 1);
    host.set_num(ch, "playing", 1);
    // enter_player_game: `if (!SCRIPT(d->character)) read_saved_vars(d->character);`
    // and read_saved_vars always does `CREATE(SCRIPT(ch), struct script_data, 1)`,
    // so every playing PC owns a script (the home of its global variables).
    host.ensure_script(ch);
}

/// Give a mob the attributes a script reads.
pub fn dress_mob(host: &mut MockHost, ch: Uid, level: i64) {
    host.set_num(ch, "level", level);
    host.set_num(ch, "hit", 60);
    host.set_num(ch, "max_hit", 60);
    host.set_num(ch, "pos", 8);
}

impl Fixture {
    /// Build the standard cast. No trigger is attached to anything yet.
    pub fn new() -> Self {
        let mut host = MockHost::new();
        host.globals.nums.insert("script_players".to_owned(), 0);
        host.globals.texts.insert(
            "commands".to_owned(),
            "north east south west up down look say emote give drop get open close".to_owned(),
        );
        let room = host.add_room(3001, "The Temple");
        let mut neighbours = Vec::new();
        for dir in 0..6usize {
            let vnum = 3002 + dir as i32;
            let other = host.add_room(vnum, &format!("Room {vnum}"));
            host.set_link(room, &format!("exit:{dir}"), Some(other));
            let back = dg_vm::types::REV_DIR[dir];
            host.set_link(other, &format!("exit:{back}"), Some(room));
            host.set_text(room, &format!("exit:{dir}:keyword"), "door");
            neighbours.push(other);
        }
        let player = host.add_player("Tester");
        dress_player(&mut host, player, 10);
        host.char_to_room(player, room);
        let guard = host.add_mob(3100, "a city guard");
        host.set_text(guard, "alias", "guard city");
        dress_mob(&mut host, guard, 20);
        host.char_to_room(guard, room);
        let bench = host.add_obj(3020, "a bench");
        host.set_text(bench, "alias", "bench");
        host.obj_to_room(bench, room);
        let lantern = host.add_obj(3022, "a lantern");
        host.set_text(lantern, "alias", "lantern");
        let dagger = host.add_obj(3021, "a dagger");
        host.set_text(dagger, "alias", "dagger");
        host.obj_to_char(lantern, player);
        host.obj_to_char(dagger, player);
        let sword = host.add_obj(3023, "a sword");
        host.set_text(sword, "alias", "sword");
        let cloak = host.add_obj(3024, "a cloak");
        host.set_text(cloak, "alias", "cloak");
        host.equip_char(player, sword, 16);
        host.equip_char(player, cloak, 12);
        Self {
            host,
            room,
            neighbours,
            player,
            guard,
            bench,
            inventory: vec![dagger, lantern],
            equipment: vec![sword, cloak],
        }
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Register a prototype in the host and return it.
#[allow(clippy::too_many_arguments)]
pub fn make_proto(
    host: &mut MockHost,
    vnum: i32,
    name: &str,
    attach: AttachType,
    types: &str,
    narg: i32,
    arglist: &str,
    body: &str,
) -> Arc<TrigProto> {
    let proto = Arc::new(TrigProto {
        vnum,
        name: name.to_owned(),
        attach_type: attach,
        trigger_type: asciiflag_conv(types),
        narg,
        arglist: arglist.to_owned(),
        program: Arc::new(Program::parse(body)),
    });
    host.protos.insert(vnum, Arc::clone(&proto));
    proto
}

/// Register and attach a trigger in one step; returns the instance id.
#[allow(clippy::too_many_arguments)]
pub fn install(
    host: &mut MockHost,
    owner: Uid,
    vnum: i32,
    attach: AttachType,
    types: &str,
    narg: i32,
    arglist: &str,
    body: &str,
) -> u64 {
    let proto = make_proto(
        host,
        vnum,
        &format!("t{vnum}"),
        attach,
        types,
        narg,
        arglist,
        body,
    );
    host.attach(owner, &proto)
        .expect("the owner of an installed trigger exists")
}

/// Add the event variables to the instance (`add_var(&GET_TRIG_VARS(t), …)`).
pub fn set_event_vars(host: &mut MockHost, owner: Uid, id: u64, vars: &[(&str, String)]) {
    if let Some(trigger) = host
        .script_mut(owner)
        .and_then(|script| script.trigger_mut(id))
    {
        for (name, value) in vars {
            trigger.vars.add_var(name, value, 0);
        }
    }
}

/// `ADD_UID_VAR` value for an entity.
pub fn uid(uid: Uid) -> String {
    uid_text(uid)
}

/// Resume every queued wait, round after round, up to `rounds` rounds.
/// Returns how many resumes ran.
pub fn run_waits(host: &mut MockHost, rounds: usize) -> usize {
    let mut resumed = 0;
    for _ in 0..rounds {
        let due = std::mem::take(&mut host.waits);
        if due.is_empty() {
            break;
        }
        for (owner, id, _pulses) in due {
            resume(host, owner, id);
            resumed += 1;
        }
    }
    resumed
}

/// The mock's `mud_command` log reduced to the command lines.
pub fn commands(host: &MockHost) -> Vec<String> {
    host.mud_commands.iter().map(|(_, c)| c.clone()).collect()
}

/// Whether any instance in the world is still checked out of its slot.
pub fn checked_out(host: &MockHost) -> Vec<(Uid, i32)> {
    let mut found = Vec::new();
    for (owner, entity) in &host.entities {
        if let Some(script) = &entity.script {
            for trigger in &script.triggers {
                if trigger.checked_out {
                    found.push((*owner, trigger.proto.vnum));
                }
            }
        }
    }
    found
}

/// A global variable's value on an owner, if set.
pub fn global_var(host: &MockHost, owner: Uid, name: &str) -> Option<String> {
    host.script(owner)
        .and_then(|script| script.global_vars.get(name))
        .map(|var| var.value.clone())
}

/// A trigger-local variable's value, if set (it lives only while the run is
/// unfinished, i.e. while waiting).
pub fn local_var(host: &MockHost, owner: Uid, id: u64, name: &str) -> Option<String> {
    host.script(owner)
        .and_then(|script| script.trigger(id))
        .and_then(|trigger| trigger.vars.get(name))
        .map(|var| var.value.clone())
}

/// Attach a corpus record to an owner, as its `.trg` header says it is built.
pub fn install_record(host: &mut MockHost, owner: Uid, record: &super::corpus::Record) -> u64 {
    let attach = AttachType::from_index(record.attach_type).unwrap_or(AttachType::Mob);
    let proto = make_proto(
        host,
        record.vnum,
        &record.name,
        attach,
        &record.flags,
        record.narg,
        &record.arglist,
        &record.body,
    );
    host.attach(owner, &proto)
        .expect("the owner of an installed trigger exists")
}
