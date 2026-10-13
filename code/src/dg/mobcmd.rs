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
use mib_core::prelude::{CommandReceived, CommandSequence, Messages, PlayerInput};
use mib_stdlib::prelude::{OnlinePlayers, Player};

use super::host::EcsHost;
use crate::combat::TbaHidden;
use crate::module::TbaContent;

mod act;
mod comm;
mod door;
mod fight;
mod item;
mod movement;
mod position;
mod social;
mod table;
mod world;

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

/// Force a player to run a line: it is injected as the player's own input
/// with a fresh command id, so next frame it goes through the engine's whole
/// pipeline (rate limit, `command` triggers, the pack's handlers). The id is
/// fresh because the router marks an id handled once it has routed a command,
/// which would skip a second forced command sharing id 0 in the same frame.
fn force_player(host: &mut EcsHost<'_>, player: Entity, line: &str) {
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
    host.world.write_message(CommandReceived::new(
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
