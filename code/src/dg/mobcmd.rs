//! The game's command interpreter as seen from a script: a mob command line
//! that is not a script command, and the `mudcommand` text function.
//!
//! [`mud_command`] is the C's `command_interpreter`. A player is forced
//! through the engine's own pipeline; an NPC, which the engine cannot give a
//! command, runs the NPC side of each command here, ported from the C source
//! (`act.comm.c`, `act.item.c`, `act.movement.c`, `act.offensive.c`,
//! `act.social.c`) with the same messages and the same triggers at the same
//! points. Text only the acting NPC would have received is dropped, as the C
//! drops it for a character with no descriptor.
//!
//! What an NPC does with each command word is [`table`], which is
//! `cmd_info[]` verbatim and in order: the order decides which command an
//! abbreviation means. Commands that do nothing for an NPC in the C, and
//! those the pack has no model for, are rows too; the latter are counted
//! under `mobcmd.<command>` in the [`DgLog`](super::DgLog).
//!
//! # What the pack does not model
//!
//! Skipped where the C consults them: carrying capacity (count and weight),
//! the strength a weapon needs, a container's weight and its open/locked
//! state, furniture occupancy, movement points for NPCs, death-trap and
//! indoor rooms, houses and zone restrictions, charm and `AFF_*` bits on
//! characters, and colour codes. `eat`, `drink`, `use` and their kin, `order`,
//! `group` and `leave` are counted gaps. The first blow of `hit` is left to
//! the pack's combat round. Pronouns render neutral (`they`, `them`,
//! `their`), as the pack's socials do.

use dg_vm::cstr::{any_one_arg, skip_spaces};
use dg_vm::dispatch::{command_mtrigger, command_otrigger, command_wtrigger};
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_core::prelude::Entity;
use mib_core::prelude::{
    CommandId, CommandReceived, CommandSequence, Messages, PlayerInput, Resource,
};
use mib_stdlib::prelude::{OnlinePlayers, Player};

use std::collections::VecDeque;

use super::host::EcsHost;
use super::script::DgScript;
use crate::combat::TbaHidden;
use crate::module::TbaContent;

pub(crate) mod act;
mod comm;
pub(crate) mod door;
mod fight;
mod item;
pub(crate) mod movement;
mod position;
mod social;
mod table;
pub(crate) mod world;

pub(crate) use table::DoorCmd;
use table::{COMMANDS, Handler};

pub use fight::{add_follower, circle_follow, stop_follower};
pub use world::TbaNpcGold;

/// `command_interpreter(ch, line)`: have `ch` perform an ordinary game command.
pub fn mud_command(host: &mut EcsHost<'_>, ch: Uid, line: &str) {
    let Some(entity) = host.entity(ch) else {
        return;
    };
    if host.world.get::<Player>(entity).is_some() {
        force_player(host, entity, line);
        return;
    }
    // `REMOVE_BIT_AR(AFF_FLAGS(ch), AFF_HIDE)`.
    host.world.entity_mut(entity).remove::<TbaHidden>();

    let argument = skip_spaces(line);
    let Some(first) = argument.chars().next() else {
        return;
    };
    // A first character that is not a letter is a command by itself (`'hi`).
    let (word, rest) = if first.is_ascii_alphabetic() {
        any_one_arg(argument)
    } else {
        let len = first.len_utf8();
        (first.to_string(), argument.get(len..).unwrap_or_default())
    };

    // The command triggers may take the command over: world, mob, object.
    if command_wtrigger(host, ch, &word, rest) != 0
        || command_mtrigger(host, ch, &word, rest) != 0
        || command_otrigger(host, ch, &word, rest) != 0
    {
        return;
    }
    if !host.exists(ch) {
        return;
    }

    let level = host.level(ch);
    let found = COMMANDS
        .iter()
        .find(|cmd| cmd.name.starts_with(word.as_str()) && level >= cmd.min_level);
    let Some(cmd) = found else {
        let social = host
            .world
            .get_resource::<TbaContent>()
            .and_then(|content| social::find_social(content, &word, level));
        if let Some(action) = social
            && gate_position(host, ch, action.min_char_position)
        {
            social::do_action(host, ch, &action, rest);
        }
        // Anything else is "Huh?!?", which only the NPC itself would read.
        return;
    };
    if cmd.min_level >= table::LVL_IMMORT {
        return;
    }
    if !gate_position(host, ch, cmd.min_position) {
        return;
    }
    run_handler(host, ch, cmd, rest);
}

/// Whether the character's position allows a command needing `minimum`; the
/// C tells the character why not, which an NPC would not read.
fn gate_position(host: &EcsHost<'_>, ch: Uid, minimum: i32) -> bool {
    world::position_number(world::position(host, ch)) >= minimum
}

/// Run the NPC side of one command.
fn run_handler(host: &mut EcsHost<'_>, ch: Uid, cmd: &table::Cmd, line: &str) {
    match cmd.handler {
        Handler::Noop => {}
        Handler::Unmodelled => {
            host.unsupported(&format!("mobcmd.{}", cmd.name), line);
        }
        Handler::Move(dir) => movement::do_move(host, ch, dir),
        Handler::Enter => movement::do_enter(host, ch, line),
        Handler::Door(door_cmd) => {
            door::do_gen_door(host, ch, line, door_cmd);
        }
        Handler::Drop(mode) => item::do_drop(host, ch, line, mode),
        Handler::Say => comm::do_say(host, ch, line),
        Handler::Tell => comm::do_tell(host, ch, line),
        Handler::SpecComm(kind) => {
            comm::do_spec_comm(host, ch, line, kind == table::SpecComm::Whisper)
        }
        Handler::GenComm(channel) => comm::do_gen_comm(host, ch, line, channel),
        Handler::Echo(emote) => comm::do_echo(host, ch, line, emote),
        Handler::Get => item::do_get(host, ch, line),
        Handler::Give => item::do_give(host, ch, line),
        Handler::Put => item::do_put(host, ch, line),
        Handler::Wear => item::do_wear(host, ch, line),
        Handler::Wield => item::do_wield(host, ch, line),
        Handler::Grab => item::do_grab(host, ch, line),
        Handler::Remove => item::do_remove(host, ch, line),
        Handler::Sacrifice => item::do_sac(host, ch, line),
        Handler::Sit => position::do_sit(host, ch, line),
        Handler::Rest => position::do_rest(host, ch),
        Handler::Sleep => position::do_sleep(host, ch),
        Handler::Wake => position::do_wake(host, ch, line),
        Handler::Stand => position::do_stand(host, ch),
        Handler::Follow => fight::do_follow(host, ch, line),
        Handler::Unfollow => fight::do_unfollow(host, ch),
        Handler::Hit => fight::do_hit(host, ch, line),
        Handler::Assist => fight::do_assist(host, ch, line),
        Handler::Flee => fight::do_flee(host, ch),
    }
}

/// The forced commands still in flight that must not be offered to triggers
/// that were running when they were forced: `command_interpreter` is called
/// inside the forcing trigger, and `TRIGGER_CHECK` skips a trigger whose depth
/// is non-zero. The injected line reaches the engine's pipeline a frame
/// later, so the pairs are kept here for command intake to consult.
#[derive(Resource, Debug, Default)]
pub struct ForcedCommands {
    entries: VecDeque<(CommandId, Vec<(Uid, u64)>)>,
}

/// Forced player input waiting to be published into the command queue.
///
/// A `%force%` runs inside the forcing trigger. Writing straight into
/// `Messages<CommandReceived>` would let the later pre-commit intake and the
/// router read the line in the same frame, before the `command` triggers have
/// been offered it (the C's `command_interpreter` offers the forced line to
/// them). The command gate drains this queue at the start of its next run, so
/// the line is gated like any other input and then flows downstream.
#[derive(Resource, Debug, Default)]
pub struct ForcedInputs(pub VecDeque<CommandReceived>);

/// The most forced commands [`ForcedCommands`] remembers.
const FORCED_KEPT: usize = 256;

impl ForcedCommands {
    /// Remember that command `id` was forced while the triggers in `busy` were
    /// running or waiting. Called by `force_player`; public so the command
    /// gate's test can seed a record.
    pub fn record(&mut self, id: CommandId, busy: Vec<(Uid, u64)>) {
        if self.entries.len() >= FORCED_KEPT {
            self.entries.pop_front();
        }
        self.entries.push_back((id, busy));
    }

    /// Whether trigger `trigger` (a [`dg_vm::TrigInstance::id`]) of `owner`
    /// was running or waiting when command `id` was forced, so that command
    /// is not offered to it.
    pub fn skips(&self, id: CommandId, owner: Uid, trigger: u64) -> bool {
        self.entries
            .iter()
            .any(|(forced, busy)| *forced == id && busy.contains(&(owner, trigger)))
    }

    /// The `(owner, trigger)` pairs that were running or waiting when command
    /// `id` was forced. The command gate holds each busy for the length of the
    /// offer, which is how `TRIGGER_CHECK` skips the trigger when the forced
    /// command runs inside the trigger that forced it.
    pub fn busy_for(&self, id: CommandId) -> Vec<(Uid, u64)> {
        self.entries
            .iter()
            .find(|(forced, _)| *forced == id)
            .map(|(_, busy)| busy.clone())
            .unwrap_or_default()
    }

    /// Forget command `id` once it has been routed.
    pub fn forget(&mut self, id: CommandId) {
        self.entries.retain(|(forced, _)| *forced != id);
    }
}

/// Every attached trigger that is running or waiting (`GET_TRIG_DEPTH != 0`),
/// as (owner, trigger instance id).
fn busy_triggers(host: &mut EcsHost<'_>) -> Vec<(Uid, u64)> {
    let mut found = Vec::new();
    let mut query = host.world.query::<(Entity, &DgScript)>();
    let held: Vec<(Entity, Vec<u64>)> = query
        .iter(host.world)
        .map(|(entity, script)| {
            let busy = script
                .0
                .triggers
                .iter()
                .filter(|trig| trig.is_busy())
                .map(|trig| trig.id)
                .collect();
            (entity, busy)
        })
        .collect();
    for (entity, ids) in held {
        if ids.is_empty() {
            continue;
        }
        if let Some(uid) = host.uid(entity) {
            found.extend(ids.into_iter().map(|id| (uid, id)));
        }
    }
    found
}

/// Force a player to run a line: it is injected as the player's own input
/// with a fresh command id, so next frame it goes through the engine's whole
/// pipeline (rate limit, `command` triggers, the pack's handlers). The id is
/// fresh because the router marks an id handled once it has routed a command,
/// which would skip a second forced command sharing id 0 in the same frame.
fn force_player(host: &mut EcsHost<'_>, player: Entity, line: &str) {
    // `command_interpreter` returns silently on a blank line.
    if skip_spaces(line).is_empty() {
        return;
    }
    let session_id = host
        .world
        .get_resource::<OnlinePlayers>()
        .and_then(|online| {
            online
                .map
                .iter()
                .find(|(_, record)| record.entity == player)
                .map(|(id, _)| *id)
        });
    let Some(session_id) = session_id else {
        host.unsupported("mobcmd.force_player", "the player has no online session");
        return;
    };
    if !host.world.contains_resource::<Messages<CommandReceived>>() {
        host.unsupported("mobcmd.force_player", "no command message queue");
        return;
    }
    let id = host
        .world
        .get_resource_or_insert_with(CommandSequence::default)
        .next_id();
    // The C runs a forced command inside the forcing trigger, whose depth is
    // non-zero, so none of the triggers running or waiting now see it.
    let busy = busy_triggers(host);
    if !busy.is_empty() {
        host.world
            .get_resource_or_insert_with(ForcedCommands::default)
            .record(id, busy);
    }
    // Stage it: the command gate publishes it at the start of its next run, so
    // it is offered to the `command` triggers before the router can see it.
    host.world
        .get_resource_or_insert_with(ForcedInputs::default)
        .0
        .push_back(CommandReceived::new(
            id,
            player,
            session_id,
            PlayerInput::RawLine(line.to_owned()),
        ));
}

/// The full name of the first game command that `prefix` abbreviates: the
/// first `cmd_info[]` entry (socials are not in it) whose name starts with
/// `prefix`, as `strncmp(command, prefix, strlen(prefix))` finds it.
pub fn mud_command_name(prefix: &str) -> Option<String> {
    COMMANDS
        .iter()
        .find(|cmd| cmd.name.starts_with(prefix))
        .map(|cmd| cmd.name.to_owned())
}

/// The mob command words the pack models, in `cmd_info[]` order: every command
/// the game's `command_interpreter` recognizes whose handler is not
/// [`Handler::Unmodelled`](table::Handler::Unmodelled). The converter's corpus
/// coverage reports a mob line whose first word is one of these as supported.
pub fn supported_mob_words() -> Vec<&'static str> {
    COMMANDS
        .iter()
        .filter(|cmd| !matches!(cmd.handler, Handler::Unmodelled))
        .map(|cmd| cmd.name)
        .collect()
}
