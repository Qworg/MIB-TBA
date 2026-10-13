//! Intake for command and speech triggers and the pre-route command gate.
//!
//! [`command_gate`] is `command_interpreter`'s trigger prologue: before a
//! playing session's command is routed, the world, mob and object `command`
//! triggers are offered it in that order and the first to return non-zero
//! takes it over. A taken-over command is *claimed*: it is marked handled for
//! the engine router and recorded in [`DgClaimed`], which the pack's own
//! command handlers consult through [`is_claimed`].
//!
//! The pack handlers cannot test the engine's `HandledCommands` themselves:
//! `route_commands` marks every command it routes handled (the genre commands
//! excepted), so by the time a handler runs the mark says nothing about a
//! trigger. [`DgClaimed`] holds only what the DG intake claimed this frame.
//!
//! Every command received in a frame is offered to the gate (and to the
//! pre-commit triggers in [`super::movement`]) before any is routed, so a
//! second command from one session in the same frame is judged against the
//! room the player stood in when the frame began. Client input arrives one
//! line per frame in practice; the C, which runs each line to completion,
//! would differ only for several lines packed into one frame.
//!
//! [`speech_and_acts`] runs after the router: the `speech` triggers of a
//! player's `say` (`do_say`) and the `act` triggers of a social, both in
//! [`acts`].

use std::collections::HashSet;

use bevy::ecs::message::MessageCursor;
use dg_vm::BaseHost;
use dg_vm::dispatch::{
    command_mtrigger, command_otrigger, command_wtrigger, speech_mtrigger, speech_wtrigger,
};
use mib_core::prelude::*;

use super::DgIntake;
use crate::dg::host::EcsHost;
use crate::dg::out::is_playing;
use crate::dg::with_host;

#[path = "acts.rs"]
mod acts;

/// The commands the DG intake took over this frame (a `command`, `leave` or
/// `door` trigger refused them). Cleared by the next [`command_gate`] run.
#[derive(Resource, Debug, Default)]
pub struct DgClaimed(HashSet<CommandId>);

impl DgClaimed {
    /// Whether a trigger took this command over.
    pub fn contains(&self, id: CommandId) -> bool {
        self.0.contains(&id)
    }

    /// Record a command as taken over.
    pub fn insert(&mut self, id: CommandId) {
        self.0.insert(id);
    }
}

/// Whether `id` was taken over by a trigger. A pack handler tests this before
/// it acts on a command; with no [`DgClaimed`] resource nothing is claimed.
pub fn is_claimed(claimed: Option<&DgClaimed>, id: CommandId) -> bool {
    claimed.is_some_and(|claimed| claimed.contains(id))
}

/// Take a command over: mark it handled for the engine router, record it for
/// the pack handlers, and give the player a prompt (the line the command would
/// have ended with).
pub(super) fn claim(world: &mut World, msg: &CommandReceived) {
    world
        .get_resource_or_insert_with(DgClaimed::default)
        .insert(msg.id);
    if let Some(mut handled) = world.get_resource_mut::<HandledCommands>() {
        handled.mark_handled(msg.id);
    }
    if let Some(mut output) = world.get_resource_mut::<Messages<OutputEvent>>() {
        output.write(OutputEvent {
            session: msg.session,
            payload: OutboundPayload::text(TextChannel::Prompt, "> "),
        });
    }
}

/// Whether a command was already taken over or handled before this system.
pub(super) fn already_handled(world: &World, id: CommandId) -> bool {
    world
        .get_resource::<DgClaimed>()
        .is_some_and(|claimed| claimed.contains(id))
        || world
            .get_resource::<HandledCommands>()
            .is_some_and(|handled| handled.is_handled(id))
}

/// The commands received since `cursor` last read, from playing sessions only.
pub(super) fn playing_commands(
    world: &World,
    cursor: &mut MessageCursor<CommandReceived>,
) -> Vec<CommandReceived> {
    let Some(messages) = world.get_resource::<Messages<CommandReceived>>() else {
        return Vec::new();
    };
    cursor
        .read(messages)
        .filter(|msg| is_playing(world, msg.session))
        .cloned()
        .collect()
}

/// The command a player's input amounts to: a typed command as it is, a raw
/// line as the engine grammar parses it.
pub(super) fn typed_command(input: &PlayerInput) -> Option<Command> {
    match input {
        PlayerInput::Command(command) => Some(command.clone()),
        PlayerInput::RawLine(line) => parse_line(line).ok(),
    }
}

/// `any_one_arg`, as `command_interpreter` applies it to a typed line: the
/// command word, lowercased, and the rest of the line (not yet stripped of its
/// leading spaces). A first character that is not a letter is the command by
/// itself, so `'hi` is `'` and `hi`. `None` for a blank line.
pub fn split_line(line: &str) -> Option<(String, String)> {
    let line = line.trim_start();
    let first = line.chars().next()?;
    if !first.is_ascii_alphabetic() {
        let (word, rest) = line.split_at(first.len_utf8());
        return Some((word.to_owned(), rest.to_owned()));
    }
    let end = line.find(char::is_whitespace).unwrap_or(line.len());
    let (word, rest) = line.split_at(end);
    Some((word.to_ascii_lowercase(), rest.to_owned()))
}

/// The command word and argument string `command_interpreter` would see for a
/// player's input. A raw line is split as typed; a typed command is rendered
/// as the line a player would type for it: a `Move` is its direction word,
/// `Say` is `say` and the text, `Give` is `give <item> to <who>`, and a
/// pack-declared `Custom` verb is the verb and its arguments. `None` for the
/// login commands, which are not gameplay.
pub fn command_words(input: &PlayerInput) -> Option<(String, String)> {
    let command = match input {
        PlayerInput::RawLine(line) => return split_line(line),
        PlayerInput::Command(command) => command,
    };
    let optional = |text: &Option<String>| text.clone().unwrap_or_default();
    let (word, args): (&str, String) = match command {
        Command::Login { .. } | Command::CreateAccount { .. } | Command::CreateCharacter { .. } => {
            return None;
        }
        Command::Move { direction } => (direction.as_str(), String::new()),
        Command::Look { target } => ("look", optional(target)),
        Command::Say { text } => ("say", text.clone()),
        Command::Get { item, from } => (
            "get",
            match from {
                Some(from) => format!("{item} from {from}"),
                None => item.clone(),
            },
        ),
        Command::Drop { item } => ("drop", item.clone()),
        Command::Give { item, to } => ("give", format!("{item} to {to}")),
        Command::Put { item, into } => ("put", format!("{item} in {into}")),
        Command::Inventory => ("inventory", String::new()),
        Command::Equip { item } => ("equip", item.clone()),
        Command::Unequip { slot } => ("unequip", slot.clone()),
        Command::Equipment => ("equipment", String::new()),
        Command::Open { target } => ("open", optional(target)),
        Command::Close { target } => ("close", optional(target)),
        Command::Unlock { target } => ("unlock", optional(target)),
        Command::Lock { target } => ("lock", optional(target)),
        Command::Pick { target } => ("pick", optional(target)),
        Command::Search => ("search", String::new()),
        Command::Tell { target, text } => ("tell", format!("{target} {text}")),
        // A channel is named as the verb (`ooc hello`).
        Command::Channel { channel, text } => return Some((channel.to_lowercase(), text.clone())),
        Command::Join { channel } => ("join", channel.clone()),
        Command::Leave { channel } => ("leave", channel.clone()),
        Command::Channels => ("channels", String::new()),
        Command::Quests => ("quests", String::new()),
        Command::Accept { quest } => ("accept", quest.clone()),
        Command::TurnIn { quest } => ("turnin", quest.clone()),
        Command::Attack { target } => ("attack", target.clone()),
        Command::Flee => ("flee", String::new()),
        Command::Advance => ("advance", String::new()),
        Command::Retreat => ("retreat", String::new()),
        Command::CombatMove { dir } => ("step", dir.as_str().to_owned()),
        Command::Who => ("who", String::new()),
        Command::TalkTo { npc, text } => ("talk", format!("{npc} {text}")),
        Command::Quit => ("quit", String::new()),
        Command::Help { topic } => ("help", optional(topic)),
        Command::Custom { verb, args } => return Some((verb.to_lowercase(), args.join(" "))),
    };
    Some((word.to_owned(), args))
}

/// `DgIntake::CommandGate`: `command_interpreter`'s trigger prologue. The
/// command is offered to the world, then the mob, then the object `command`
/// triggers, stopping at the first that returns non-zero; that one took the
/// command over, so it is claimed and neither the engine router nor any pack
/// handler runs it.
///
/// Public so other intake systems can order against it.
pub fn command_gate(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    world
        .get_resource_or_insert_with(DgClaimed::default)
        .0
        .clear();
    for msg in playing_commands(world, &mut cursor) {
        if already_handled(world, msg.id) {
            continue;
        }
        let Some((cmd, argument)) = command_words(&msg.input) else {
            continue;
        };
        let taken = with_host(world, |host| {
            let Some(actor) = host.uid(msg.session) else {
                return false;
            };
            command_wtrigger(host, actor, &cmd, &argument) != 0
                || command_mtrigger(host, actor, &cmd, &argument) != 0
                || command_otrigger(host, actor, &cmd, &argument) != 0
        });
        if taken {
            claim(world, &msg);
        }
    }
}

/// `DgIntake::PostRoute`: the triggers that react to what a player said or did
/// once the router has run it. A `say` offers its text to the `speech` triggers
/// of the room's mobs, then of the room (`do_say` ends with `speech_mtrigger`
/// and `speech_wtrigger`, whatever was said, and does not offer its own line
/// to `act` triggers: it acts with `DG_NO_TRIG`). A social is acted to the
/// mobs that see it ([`acts`]).
pub fn speech_and_acts(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    for msg in playing_commands(world, &mut cursor) {
        let claimed = world
            .get_resource::<DgClaimed>()
            .is_some_and(|claimed| claimed.contains(msg.id));
        if claimed {
            continue;
        }
        if let PlayerInput::RawLine(line) = &msg.input
            && let Some(social) = crate::socials::parse_social_line(line)
        {
            acts::social_acts(world, msg.session, &social);
            continue;
        }
        if let Some(Command::Say { text }) = typed_command(&msg.input) {
            say_triggers(world, msg.session, &text);
        } else if is_bare_say(&msg.input) {
            // `do_say` offers the empty string to the speech triggers too.
            say_triggers(world, msg.session, "");
        }
    }
}

/// Whether the input is `say` (or `'`) with nothing after it, which the
/// grammar rejects but `do_say` still passes to the speech triggers.
fn is_bare_say(input: &PlayerInput) -> bool {
    let PlayerInput::RawLine(line) = input else {
        return false;
    };
    split_line(line)
        .is_some_and(|(word, rest)| (word == "say" || word == "'") && rest.trim().is_empty())
}

/// `speech_mtrigger` and `speech_wtrigger` for what `speaker` said.
fn say_triggers(world: &mut World, speaker: Entity, text: &str) {
    let spoken = text.trim_start();
    with_host(world, |host: &mut EcsHost<'_>| {
        let Some(actor) = host.uid(speaker) else {
            return;
        };
        // `do_say`'s own `act(..., DG_NO_TRIG)` switches `dg_act_check` off.
        host.vm_state().act_check = false;
        speech_mtrigger(host, actor, spoken);
        speech_wtrigger(host, actor, spoken);
    });
}

/// Register this module's systems.
pub fn register(app: &mut App) {
    app.init_resource::<DgClaimed>();
    app.add_systems(Update, command_gate.in_set(DgIntake::CommandGate));
    // A `command` trigger sees `who` before the pack answers it.
    app.configure_sets(
        Update,
        DgIntake::CommandGate.before(crate::visibility::intercept_who),
    );
    app.add_systems(
        Update,
        speech_and_acts
            .in_set(DgIntake::PostRoute)
            .after(crate::socials::handle_social_commands)
            .after(super::movement::typed_arrivals),
    );
}
