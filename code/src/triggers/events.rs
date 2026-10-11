//! Trigger event intake and dispatch (issue #15, Phase 2).
//!
//! Four events fire this pass, matching `docs/design/triggers.md` §6.4:
//!
//! - **speech** — a player `say`s a phrase; the `speech` triggers whose
//!   `arglist` matches the text run against the mobs in the room.
//! - **command** — a raw/typed line's command word matches a `command`
//!   trigger's `arglist`.
//! - **greet/entry** — a player changes room; the mobs in the new room with
//!   `greet`/`greet-all`/`entry` run, with `%direction%` derived from the exit
//!   that connects the old room to the new one.
//! - **act** — an `emote`/`social` phrase matches an `act` trigger.
//!
//! Nothing else fires: `%load%`, `wait`, combat, door and timer triggers stay
//! parsed-but-deferred and are counted as named drops.
//!
//! Schedule: the command/speech/act intake runs in `BridgeSet::Game` after
//! `route_commands` (so movement for a command is applied); greet/entry runs
//! on `FixedUpdate` after `maintain_room_occupants`. Both only read; output
//! goes through [`OutputEvent`].

use bevy::ecs::system::SystemParam;
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    CarriedBy, Exits, Item, Location, Npc, NpcKey, Player, Room, Session, SessionState,
};

use crate::combat::commands::text;
use crate::combat::model::TbaProgress;
use crate::combat::progression::TbaRng;
use crate::triggers::context::{ActorView, EntityView, NamedDrop, TriggerContext};
use crate::triggers::eval::{EvalOutcome, TextTarget, run};
use crate::triggers::index::TbaTriggers;

/// Remembers each player's previous room so a greet can name the direction.
#[derive(Debug, Default, Resource)]
pub struct TriggerLastRoom {
    rooms: std::collections::HashMap<Entity, Entity>,
}

impl TriggerLastRoom {
    /// The room the player was last seen in, if any.
    pub fn previous(&self, player: Entity) -> Option<Entity> {
        self.rooms.get(&player).copied()
    }

    /// Record the player's current room.
    pub fn remember(&mut self, player: Entity, room: Entity) {
        self.rooms.insert(player, room);
    }
}

/// The queries one trigger dispatch consults.
#[derive(SystemParam)]
pub struct TriggerView<'w, 's> {
    pub players: Query<'w, 's, (Entity, &'static Player, &'static Location)>,
    pub progress: Query<'w, 's, &'static TbaProgress>,
    pub npcs: Query<'w, 's, (Entity, &'static Npc, &'static NpcKey, &'static Location)>,
    pub rooms: Query<'w, 's, (Entity, &'static Room)>,
    pub ground: Query<'w, 's, (Entity, &'static Item, &'static Location)>,
    pub carried: Query<'w, 's, (Entity, &'static Item, &'static CarriedBy)>,
}

/// The kind of intake event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    Speech,
    Command,
    Act,
    Greet,
    Entry,
}

impl TriggerKind {
    /// The content type names that respond to this event.
    pub fn type_names(self) -> &'static [&'static str] {
        match self {
            TriggerKind::Speech => &["speech"],
            TriggerKind::Command => &["command"],
            TriggerKind::Act => &["act"],
            TriggerKind::Greet => &["greet", "greet-all"],
            TriggerKind::Entry => &["entry"],
        }
    }
}

/// One event to dispatch.
#[derive(Debug, Clone)]
pub struct TriggerEvent {
    pub kind: TriggerKind,
    pub actor: Entity,
    pub room: Entity,
    pub text: Option<String>,
    pub command: Option<String>,
    pub direction: Option<String>,
}

/// `BridgeSet::Game`, after `route_commands`: speech, command, and act.
pub fn trigger_command_system(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    view: TriggerView,
    triggers: Option<Res<TbaTriggers>>,
    mut rng: ResMut<TbaRng>,
    mut output: MessageWriter<OutputEvent>,
) {
    let Some(triggers) = triggers else {
        return;
    };
    if triggers.is_empty() {
        return;
    }
    let mut events: Vec<TriggerEvent> = Vec::new();
    for msg in reader.read() {
        let playing = sessions
            .get(msg.session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        let Some((_, _, location)) = view.players.iter().find(|(e, _, _)| *e == msg.session) else {
            continue;
        };
        let room = location.room;
        match &msg.input {
            PlayerInput::Command(Command::Say { text }) => events.push(TriggerEvent {
                kind: TriggerKind::Speech,
                actor: msg.session,
                room,
                text: Some(text.clone()),
                command: None,
                direction: None,
            }),
            PlayerInput::RawLine(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let (word, rest) = split_word(trimmed);
                let lower = word.to_ascii_lowercase();
                match lower.as_str() {
                    "say" | "s" => events.push(TriggerEvent {
                        kind: TriggerKind::Speech,
                        actor: msg.session,
                        room,
                        text: Some(rest.to_owned()),
                        command: None,
                        direction: None,
                    }),
                    "emote" | "em" => events.push(TriggerEvent {
                        kind: TriggerKind::Act,
                        actor: msg.session,
                        room,
                        text: Some(rest.to_owned()),
                        command: Some(lower),
                        direction: None,
                    }),
                    "social" => {
                        let (name, _) = split_word(rest);
                        events.push(TriggerEvent {
                            kind: TriggerKind::Act,
                            actor: msg.session,
                            room,
                            text: Some(name.to_owned()),
                            command: Some(name.to_ascii_lowercase()),
                            direction: None,
                        });
                    }
                    _ => events.push(TriggerEvent {
                        kind: TriggerKind::Command,
                        actor: msg.session,
                        room,
                        text: None,
                        command: Some(lower),
                        direction: None,
                    }),
                }
            }
            PlayerInput::Command(command) => {
                let word = match command {
                    Command::Move { direction } => direction.as_str().to_owned(),
                    Command::Custom { verb, .. } => verb.to_ascii_lowercase(),
                    other => other.label().to_owned(),
                };
                events.push(TriggerEvent {
                    kind: TriggerKind::Command,
                    actor: msg.session,
                    room,
                    text: None,
                    command: Some(word),
                    direction: None,
                });
            }
        }
    }
    for event in events {
        dispatch(&event, &triggers, &view, &mut rng, &mut output);
    }
}

/// `FixedUpdate`, after `maintain_room_occupants`: greet/entry on room change.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn trigger_greet_system(
    changed: Query<(Entity, &Location), (Changed<Location>, With<Player>)>,
    view: TriggerView,
    exits: Query<&Exits>,
    mut last_rooms: ResMut<TriggerLastRoom>,
    triggers: Option<Res<TbaTriggers>>,
    mut rng: ResMut<TbaRng>,
    mut output: MessageWriter<OutputEvent>,
) {
    let Some(triggers) = triggers else {
        return;
    };
    if triggers.is_empty() {
        return;
    }
    for (player, location) in changed.iter() {
        let new_room = location.room;
        let previous = last_rooms.previous(player);
        last_rooms.remember(player, new_room);
        let Some(previous) = previous else {
            // First sight: record the room but do not greet on boot.
            continue;
        };
        if previous == new_room {
            continue;
        }
        let direction = direction_between(previous, new_room, &exits);
        for kind in [TriggerKind::Greet, TriggerKind::Entry] {
            let event = TriggerEvent {
                kind,
                actor: player,
                room: new_room,
                text: None,
                command: None,
                direction: direction.clone(),
            };
            dispatch(&event, &triggers, &view, &mut rng, &mut output);
        }
    }
}

/// The direction word of the exit from `from` that leads to `to`.
fn direction_between(from: Entity, to: Entity, exits: &Query<&Exits>) -> Option<String> {
    let exits = exits.get(from).ok()?;
    exits
        .map
        .iter()
        .find(|(_, target)| **target == to)
        .map(|(key, _)| key.as_str().to_owned())
}

/// Find candidates and run each matching trigger.
fn dispatch(
    event: &TriggerEvent,
    triggers: &TbaTriggers,
    view: &TriggerView,
    rng: &mut TbaRng,
    output: &mut MessageWriter<OutputEvent>,
) {
    for (self_entity, attach_key, self_vnum, self_name) in candidates(event, view) {
        let candidate = event_candidate(event);
        for ty in event.kind.type_names() {
            for id in triggers.index.lookup(&attach_key, ty) {
                let Some(def) = triggers.def(id) else {
                    continue;
                };
                let Some(program) = triggers.program(id) else {
                    continue;
                };
                if !program.is_valid() {
                    continue;
                }
                if let Some(candidate) = candidate
                    && !arglist_matches(&def.arglist, candidate)
                {
                    continue;
                }
                let ctx = build_context(event, view, self_vnum, &self_name);
                let outcome = run(&program.nodes, &ctx, rng);
                deliver(&outcome, event, self_entity, view, output);
            }
        }
    }
}

/// The text a speech/command/act `arglist` is matched against.
fn event_candidate(event: &TriggerEvent) -> Option<&str> {
    match event.kind {
        TriggerKind::Speech | TriggerKind::Act => event.text.as_deref(),
        TriggerKind::Command => event.command.as_deref(),
        TriggerKind::Greet | TriggerKind::Entry => None,
    }
}

/// Whether a DG `arglist` matches the event text.
///
/// `*` and an empty list match everything. Otherwise a whitespace-separated
/// keyword matches when it equals the candidate word or appears in it, which
/// covers both the command form (`arglist: teleport`) and the speech form
/// (`arglist: help`).
fn arglist_matches(arglist: &str, candidate: &str) -> bool {
    let arglist = arglist.trim();
    if arglist.is_empty() || arglist == "*" {
        return true;
    }
    let candidate = candidate.trim().to_ascii_lowercase();
    arglist.split_whitespace().any(|keyword| {
        let keyword = keyword.to_ascii_lowercase();
        !keyword.is_empty() && (keyword == candidate || candidate.contains(&keyword))
    })
}

/// The `(entity, attach key, vnum, name)` of every thing in the room whose
/// triggers could respond to `event`.
fn candidates(event: &TriggerEvent, view: &TriggerView) -> Vec<(Entity, String, i32, String)> {
    let mut out = Vec::new();
    for (entity, npc, key, location) in view.npcs.iter() {
        if location.room != event.room {
            continue;
        }
        let id = key.0.as_ref();
        out.push((
            entity,
            format!("npc:{id}"),
            vnum_from_id(id),
            npc.name.clone(),
        ));
    }
    if let Some((_, room)) = view.rooms.iter().find(|(e, _)| *e == event.room) {
        out.push((
            event.room,
            format!("room:{}", room.key),
            i64::from(room.num) as i32,
            room.name.clone(),
        ));
    }
    for (entity, item, location) in view.ground.iter() {
        if location.room == event.room {
            out.push((
                entity,
                format!("item:{}", item.def_id),
                vnum_from_id(item.def_id.as_ref()),
                item.name.clone(),
            ));
        }
    }
    for (entity, item, holder) in view.carried.iter() {
        if holder.0 == event.actor {
            out.push((
                entity,
                format!("item:{}", item.def_id),
                vnum_from_id(item.def_id.as_ref()),
                item.name.clone(),
            ));
        }
    }
    out
}

/// Build the substitution context for one record.
fn build_context(
    event: &TriggerEvent,
    view: &TriggerView,
    self_vnum: i32,
    self_name: &str,
) -> TriggerContext {
    let actor = view
        .players
        .iter()
        .find(|(entity, _, _)| *entity == event.actor)
        .map(|(_, player, _)| {
            let progress = view.progress.get(event.actor).ok();
            ActorView {
                name: player.name.clone(),
                is_pc: true,
                id: 0,
                vnum: 0,
                level: progress.map(|p| p.level).unwrap_or(1),
                class: progress.map(|p| p.class.to_string()).unwrap_or_default(),
            }
        });
    let (cmd, speech) = match event.kind {
        TriggerKind::Speech => (None, event.text.clone()),
        TriggerKind::Command => (event.command.clone(), None),
        TriggerKind::Act => (event.command.clone(), event.text.clone()),
        TriggerKind::Greet | TriggerKind::Entry => (None, None),
    };
    let room_vnum = view
        .rooms
        .iter()
        .find(|(entity, _)| *entity == event.room)
        .map(|(_, room)| room.num as i32);
    TriggerContext {
        actor,
        self_: Some(EntityView {
            name: self_name.to_owned(),
            vnum: self_vnum,
        }),
        direction: event.direction.clone(),
        cmd,
        speech,
        room_vnum,
        ..TriggerContext::default()
    }
}

/// Emit the record's lines to the right recipients.
fn deliver(
    outcome: &EvalOutcome,
    event: &TriggerEvent,
    self_entity: Entity,
    view: &TriggerView,
    output: &mut MessageWriter<OutputEvent>,
) {
    for line in &outcome.emitted {
        let recipients: Vec<Entity> = match line.target {
            TextTarget::Actor => vec![event.actor],
            TextTarget::Room => players_in_room(view, event.room),
            TextTarget::RoomExceptActor => players_in_room(view, event.room)
                .into_iter()
                .filter(|entity| *entity != event.actor)
                .collect(),
            TextTarget::RoomExceptSelf => players_in_room(view, event.room)
                .into_iter()
                .filter(|entity| *entity != self_entity)
                .collect(),
            // Self/victim/object are NPC/item entities with no session, and a
            // zone echo is Phase 4.
            TextTarget::SelfOnly | TextTarget::Victim | TextTarget::Object | TextTarget::Zone => {
                Vec::new()
            }
        };
        for recipient in recipients {
            output.write(text(recipient, line.text.clone()));
        }
    }
}

/// The player entities in `room`, in query order.
fn players_in_room(view: &TriggerView, room: Entity) -> Vec<Entity> {
    view.players
        .iter()
        .filter(|(_, _, location)| location.room == room)
        .map(|(entity, _, _)| entity)
        .collect()
}

/// The numeric vnum embedded in a converted id (`tba_npc_3` -> `3`).
fn vnum_from_id(id: &str) -> i32 {
    id.rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|tail| tail.parse::<i32>().ok())
        .unwrap_or(0)
}

/// Split the first whitespace-delimited word from the rest.
fn split_word(line: &str) -> (&str, &str) {
    let trimmed = line.trim();
    match trimmed.find(char::is_whitespace) {
        Some(index) => (&trimmed[..index], trimmed[index + 1..].trim()),
        None => (trimmed, ""),
    }
}

/// The kinds of named drop the runtime records. Kept here so the vocabulary is
/// one place.
pub const RUNTIME_DROP_KINDS: &[&str] = &["unknown_token", "step_cap", "nop", "zoneecho"];

/// The outcome drops as reportable [`NamedDrop`]s (a small convenience for
/// tests and future telemetry).
pub fn outcome_drops(outcome: &EvalOutcome) -> &[NamedDrop] {
    &outcome.drops
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn vnum_from_id_reads_the_trailing_number() {
        assert_eq!(vnum_from_id("tba_npc_3"), 3);
        assert_eq!(vnum_from_id("tba_item_1300"), 1300);
        assert_eq!(vnum_from_id("tba.3001"), 3001);
        assert_eq!(vnum_from_id("no_number"), 0);
    }

    #[test]
    fn split_word_splits_once() {
        assert_eq!(split_word("say hello there"), ("say", "hello there"));
        assert_eq!(split_word("look"), ("look", ""));
    }

    #[test]
    fn arglist_matches_wildcards_words_and_substrings() {
        assert!(arglist_matches("", "anything"));
        assert!(arglist_matches("*", "anything"));
        assert!(arglist_matches("help", "help"));
        assert!(arglist_matches("help", "can you help me"));
        assert!(arglist_matches("yes sure", "sure"));
        assert!(!arglist_matches("help", "good morning"));
    }

    #[test]
    fn type_names_cover_the_four_events() {
        assert_eq!(TriggerKind::Greet.type_names(), &["greet", "greet-all"]);
        assert_eq!(TriggerKind::Speech.type_names(), &["speech"]);
        assert_eq!(TriggerKind::Command.type_names(), &["command"]);
        assert_eq!(TriggerKind::Entry.type_names(), &["entry"]);
    }
}
