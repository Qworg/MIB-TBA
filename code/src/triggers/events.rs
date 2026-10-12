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
//! Phase 4 adds two more paths:
//!
//! - **leave** ([`trigger_leave_system`], `CommandSet::PreRoute`) — runs before
//!   `route_commands` commits a move, so a `return 0` blocks it.
//! - **wait** ([`crate::triggers::timers::trigger_timer_system`]) — resumes a
//!   suspended record; it reuses this module's `queue_effects`/`deliver`.
//!
//! The `door` event type (a player opening a door) is **not** wired yet; the
//! `%door%` directive is. A record whose type list is not dispatched is recorded
//! as an `unsupported_event` parse drop on
//! [`TbaTriggers`](crate::triggers::index::TbaTriggers), so the gap is
//! explicit rather than silent.
//!
//! Schedule: the command/speech/act intake runs in `BridgeSet::Game` after
//! `route_commands` (so movement for a command is applied); greet/entry runs
//! on `FixedUpdate` after `maintain_room_occupants`; leave runs in
//! `CommandSet::PreRoute`. All only read; output goes through [`OutputEvent`].

use bevy::ecs::system::SystemParam;
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    CarriedBy, Exits, Item, Location, Npc, NpcKey, Player, Room, Session, SessionState,
};

use crate::combat::commands::text;
use crate::combat::model::TbaProgress;
use crate::combat::progression::TbaRng;
use crate::items::TbaObjectClock;
use crate::triggers::ast::{DoorOp, LoadKind, Node, parse_script};
use crate::triggers::context::{ActorView, ClockView, EntityView, NamedDrop, TriggerContext};
use crate::triggers::effects::{
    PendingAt, PendingDoor, PendingFlag, PendingForce, PendingPurge, PendingSpawn, PendingTeleport,
    PendingTriggerOps, PendingVarWrite, PendingZoneEcho, TbaTriggerVars,
};
use crate::triggers::eval::{EvalOutcome, TextTarget, VarScope, run};
use crate::triggers::index::TbaTriggers;
use crate::triggers::timers::{TriggerTimers, arm_timer};

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
    /// Persisted DG variables, read into the substitution context.
    pub vars: Query<'w, 's, &'static TbaTriggerVars>,
    /// The in-world clock, for `%time.hour%` and `wait until`.
    pub clock: Option<Res<'w, TbaObjectClock>>,
}

/// The kind of intake event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    Speech,
    Command,
    Act,
    Greet,
    Entry,
    Leave,
    /// A resumed `wait`; never dispatched as an intake event.
    Wait,
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
            TriggerKind::Leave => &["leave"],
            TriggerKind::Wait => &[],
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
#[allow(clippy::too_many_arguments)]
pub fn trigger_command_system(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    view: TriggerView,
    triggers: Option<Res<TbaTriggers>>,
    mut rng: ResMut<TbaRng>,
    mut ops: ResMut<PendingTriggerOps>,
    mut timers: ResMut<TriggerTimers>,
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
        dispatch(
            &event,
            &triggers,
            &view,
            &mut rng,
            &mut ops,
            &mut timers,
            &mut output,
        );
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
    mut ops: ResMut<PendingTriggerOps>,
    mut timers: ResMut<TriggerTimers>,
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
            dispatch(
                &event,
                &triggers,
                &view,
                &mut rng,
                &mut ops,
                &mut timers,
                &mut output,
            );
        }
    }
}

/// `CommandSet::PreRoute`: run `leave` triggers before movement commits.
///
/// A `leave` record that `return 0`s blocks the move: the command is marked
/// handled so `route_commands` never applies it, matching DG's
/// `leave_wtrigger`/`leave_mtrigger` gate. The record's text and queued effects
/// still go through the normal delivery/drain path.
#[allow(clippy::too_many_arguments)]
pub fn trigger_leave_system(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    view: TriggerView,
    triggers: Option<Res<TbaTriggers>>,
    mut rng: ResMut<TbaRng>,
    mut ops: ResMut<PendingTriggerOps>,
    mut timers: ResMut<TriggerTimers>,
    handled: Option<ResMut<HandledCommands>>,
    mut output: MessageWriter<OutputEvent>,
) {
    let Some(triggers) = triggers else {
        return;
    };
    if triggers.is_empty() {
        return;
    }
    let mut handled = handled;
    for msg in reader.read() {
        let playing = sessions
            .get(msg.session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        let Some(direction) = move_direction(&msg.input) else {
            continue;
        };
        let Some((_, _, location)) = view
            .players
            .iter()
            .find(|(entity, _, _)| *entity == msg.session)
        else {
            continue;
        };
        let event = TriggerEvent {
            kind: TriggerKind::Leave,
            actor: msg.session,
            room: location.room,
            text: None,
            command: None,
            direction: Some(direction),
        };
        let outcome = dispatch(
            &event,
            &triggers,
            &view,
            &mut rng,
            &mut ops,
            &mut timers,
            &mut output,
        );
        if outcome.blocked
            && let Some(handled) = handled.as_mut()
        {
            handled.mark_handled(msg.id);
        }
    }
}

/// The direction of a movement command, if the input is one.
fn move_direction(input: &PlayerInput) -> Option<String> {
    match input {
        PlayerInput::Command(Command::Move { direction }) => Some(direction.as_str().to_owned()),
        PlayerInput::RawLine(line) => match parse_line(line) {
            Ok(Command::Move { direction }) => Some(direction.as_str().to_owned()),
            _ => None,
        },
        _ => None,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DispatchOutcome {
    /// A record returned `0` (a `leave`/`door` block).
    pub blocked: bool,
    /// A record returned a non-zero value (an explicit allow).
    pub allowed: bool,
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    event: &TriggerEvent,
    triggers: &TbaTriggers,
    view: &TriggerView,
    rng: &mut TbaRng,
    ops: &mut PendingTriggerOps,
    timers: &mut TriggerTimers,
    output: &mut MessageWriter<OutputEvent>,
) -> DispatchOutcome {
    let mut result = DispatchOutcome::default();
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
                let ctx = build_context(event, view, self_entity, self_vnum, &self_name);
                let mut outcome = run(&program.nodes, &ctx, rng);
                match outcome.returned.as_deref() {
                    Some(value) if value.trim().parse::<i64>() == Ok(0) => result.blocked = true,
                    Some(_) => result.allowed = true,
                    None => {}
                }
                if let Some(suspension) = outcome.suspension.take() {
                    timers.schedule(arm_timer(
                        self_entity,
                        event.actor,
                        event.room,
                        id.clone(),
                        &ctx,
                        suspension,
                        view.clock.as_deref(),
                    ));
                }
                queue_effects(&mut outcome, event, self_entity, view, ops);
                deliver(&outcome, event, self_entity, view, output);
            }
        }
    }
    result
}

/// Translate the interpreter's queued effects into resolved [`PendingTriggerOps`].
///
/// An unresolved `%load%` recipient, a `%purge% %object%` (no receive event
/// this phase), or a cross-player `remote` is recorded as a named drop rather
/// than silently dropped.
pub(crate) fn queue_effects(
    outcome: &mut EvalOutcome,
    event: &TriggerEvent,
    self_entity: Entity,
    view: &TriggerView,
    ops: &mut PendingTriggerOps,
) {
    for spawn in &outcome.spawns {
        let victim = spawn
            .target
            .as_deref()
            .and_then(|token| recipient_entity(token, event, self_entity));
        if spawn.target.is_some() && victim.is_none() {
            outcome.drops.push(NamedDrop::new(
                "load_target",
                format!(
                    "unresolved recipient: {}",
                    spawn.target.as_deref().unwrap_or("")
                ),
            ));
        }
        ops.spawns.push(PendingSpawn {
            kind: spawn.kind,
            vnum: spawn.vnum.clone(),
            victim,
            room: Some(event.room),
            wear: spawn.wear.clone(),
            owner: self_entity,
        });
    }
    for purge in &outcome.purges {
        match resolve_purge_target(purge.target.as_deref(), event, self_entity) {
            Some(target) => ops.purges.push(PendingPurge { target }),
            None => outcome.drops.push(NamedDrop::new(
                "purge_target",
                format!(
                    "unresolved target: {}",
                    purge.target.as_deref().unwrap_or("self")
                ),
            )),
        }
    }
    let actor_id = event.actor.index_u32().to_string();
    for write in &outcome.var_writes {
        if write.scope == VarScope::Remote
            && !remote_targets_actor(write.target.as_deref(), &actor_id)
        {
            outcome.drops.push(NamedDrop::new(
                "remote_target",
                format!(
                    "cross-player remote is deferred: {} -> {}",
                    write.name,
                    write.target.as_deref().unwrap_or("")
                ),
            ));
            continue;
        }
        ops.vars.push(PendingVarWrite {
            owner: event.actor,
            scope: write.scope,
            name: write.name.clone(),
            value: write.value.clone(),
        });
    }
    for flag in &outcome.flags {
        ops.flags.push(PendingFlag {
            player: event.actor,
            flag: flag.clone(),
        });
    }
    queue_movement_effects(outcome, event, self_entity, view, ops);
    // Everything the interpreter recorded (including the unresolved targets
    // added above) is counted in the runtime drop log.
    for drop in &outcome.drops {
        ops.drops.push(drop.clone());
    }
}

/// Translate the Phase-4 movement/door/force/zone requests, resolving vnums and
/// recipient tokens and recording a named drop for anything unresolved.
fn queue_movement_effects(
    outcome: &mut EvalOutcome,
    event: &TriggerEvent,
    self_entity: Entity,
    view: &TriggerView,
    ops: &mut PendingTriggerOps,
) {
    for teleport in &outcome.teleports {
        let Some(target) = recipient_entity(&teleport.target, event, self_entity) else {
            outcome.drops.push(NamedDrop::new(
                "teleport_target",
                format!("unresolved recipient: {}", teleport.target),
            ));
            continue;
        };
        let room = room_by_vnum(view, &teleport.room);
        if room.is_none() {
            outcome.drops.push(NamedDrop::new(
                "teleport_room",
                format!("unresolved room: {}", teleport.room),
            ));
        }
        ops.teleports.push(PendingTeleport { target, room });
    }
    for door in &outcome.doors {
        let Some(room) = room_by_vnum(view, &door.room) else {
            outcome.drops.push(NamedDrop::new(
                "door_room",
                format!("unresolved room: {}", door.room),
            ));
            continue;
        };
        let Some(direction) = dir_key(&door.direction) else {
            outcome.drops.push(NamedDrop::new(
                "door_direction",
                format!("unresolved direction: {}", door.direction),
            ));
            continue;
        };
        match &door.op {
            DoorOp::Flags(flags) => {
                for flag in flags.to_ascii_lowercase().chars() {
                    if !matches!(flag, 'a' | 'b' | 'c') {
                        outcome.drops.push(NamedDrop::new(
                            "door_flag",
                            format!("unmodelled exit flag: {flag}"),
                        ));
                    }
                }
            }
            DoorOp::Purge => {
                outcome.drops.push(NamedDrop::new(
                    "door_purge",
                    "exit removal is content-owned",
                ));
                continue;
            }
            DoorOp::Deferred(field) => {
                outcome.drops.push(NamedDrop::new(
                    "door_field",
                    format!("unmodelled door field: {field}"),
                ));
                continue;
            }
            DoorOp::Open | DoorOp::Close | DoorOp::Lock | DoorOp::Unlock => {}
        }
        ops.doors.push(PendingDoor {
            room,
            direction,
            op: door.op.clone(),
        });
    }
    for force in &outcome.forces {
        let Some(target) = recipient_entity(&force.target, event, self_entity) else {
            outcome.drops.push(NamedDrop::new(
                "force_target",
                format!("unresolved recipient: {}", force.target),
            ));
            continue;
        };
        ops.forces.push(PendingForce {
            target,
            command: force.command.clone(),
        });
    }
    for at in &outcome.ats {
        let room = room_by_vnum(view, &at.room);
        if room.is_none() {
            outcome.drops.push(NamedDrop::new(
                "at_room",
                format!("unresolved room: {}", at.room),
            ));
        }
        let node = at_load(&at.command);
        match node {
            Some((kind, vnum, target, wear)) => {
                ops.ats.push(PendingAt {
                    kind,
                    vnum,
                    target: target
                        .as_deref()
                        .and_then(|token| recipient_entity(token, event, self_entity)),
                    room,
                    wear,
                    owner: self_entity,
                });
            }
            None => outcome.drops.push(NamedDrop::new(
                "at_command",
                format!("nested command is not a %load%: {}", at.command),
            )),
        }
    }
    for echo in &outcome.zone_echoes {
        let room = room_by_vnum(view, &echo.zone);
        if room.is_none() {
            outcome.drops.push(NamedDrop::new(
                "zoneecho_room",
                format!("unresolved room: {}", echo.zone),
            ));
        }
        ops.zone_echoes.push(PendingZoneEcho {
            room,
            text: echo.text.clone(),
        });
    }
    let _ = event;
}

/// The room entity whose converted `Room.num` equals `vnum`.
fn room_by_vnum(view: &TriggerView, vnum: &str) -> Option<Entity> {
    let vnum: u32 = vnum.trim().parse().ok()?;
    view.rooms
        .iter()
        .find(|(_, room)| room.num == vnum)
        .map(|(entity, _)| entity)
}

/// The canonical direction word for a `%door%` direction token.
fn dir_key(word: &str) -> Option<ExitKey> {
    ExitKey::from_word(word.trim())
}

/// Parse an `%at%` nested command as a single `%load%`.
///
/// `%at% <room> %load% …` is the only nested command Phase 4 executes; any
/// other command is a named drop the caller records.
fn at_load(command: &str) -> Option<(LoadKind, String, Option<String>, Option<String>)> {
    let program = parse_script(command);
    if !program.is_valid() || program.nodes.len() != 1 {
        return None;
    }
    match &program.nodes[0] {
        Node::Load {
            kind,
            vnum,
            target,
            wear,
        } => Some((*kind, vnum.clone(), target.clone(), wear.clone())),
        _ => None,
    }
}

/// The entity a `%load%` recipient token names.
fn recipient_entity(token: &str, event: &TriggerEvent, self_entity: Entity) -> Option<Entity> {
    match token.trim().trim_matches('%').to_ascii_lowercase().as_str() {
        "actor" => Some(event.actor),
        "self" => Some(self_entity),
        _ => None,
    }
}

/// The entity `%purge%` should despawn.
fn resolve_purge_target(
    target: Option<&str>,
    event: &TriggerEvent,
    self_entity: Entity,
) -> Option<Entity> {
    match target {
        None => Some(self_entity),
        Some(token) => match token.trim().trim_matches('%').to_ascii_lowercase().as_str() {
            "self" => Some(self_entity),
            "actor" => Some(event.actor),
            _ => None,
        },
    }
}

/// Whether a `remote` target names the triggering actor.
fn remote_targets_actor(target: Option<&str>, actor_id: &str) -> bool {
    match target {
        None => true,
        Some(target) => {
            let target = target.trim();
            target.is_empty() || target == actor_id
        }
    }
}

/// The text a speech/command/act `arglist` is matched against.
fn event_candidate(event: &TriggerEvent) -> Option<&str> {
    match event.kind {
        TriggerKind::Speech | TriggerKind::Act => event.text.as_deref(),
        TriggerKind::Command => event.command.as_deref(),
        TriggerKind::Greet | TriggerKind::Entry | TriggerKind::Leave | TriggerKind::Wait => None,
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
    self_entity: Entity,
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
                id: event.actor.index_u32() as i64,
                vnum: 0,
                level: progress.map(|p| p.level).unwrap_or(1),
                class: progress.map(|p| p.class.to_string()).unwrap_or_default(),
            }
        });
    let (cmd, speech) = match event.kind {
        TriggerKind::Speech => (None, event.text.clone()),
        TriggerKind::Command => (event.command.clone(), None),
        TriggerKind::Act => (event.command.clone(), event.text.clone()),
        TriggerKind::Greet | TriggerKind::Entry | TriggerKind::Leave | TriggerKind::Wait => {
            (None, None)
        }
    };
    let room_vnum = view
        .rooms
        .iter()
        .find(|(entity, _)| *entity == event.room)
        .map(|(_, room)| room.num as i32);
    // The persisted scopes: the actor's `remote`/`global`, and the owner's
    // `remote` (DG `%self.<name>%`).
    let actor_vars = view.vars.get(event.actor).ok();
    let (remote, global) = actor_vars
        .map(|vars| (vars.remote.clone(), vars.global.clone()))
        .unwrap_or_default();
    let self_remote = view
        .vars
        .get(self_entity)
        .map(|vars| vars.remote.clone())
        .unwrap_or_default();
    let actor_inventory = items_of(view, event.actor);
    let self_contents = items_of(view, self_entity);
    let mut room_characters: Vec<EntityView> = view
        .players
        .iter()
        .filter(|(_, _, location)| location.room == event.room)
        .map(|(_, player, _)| EntityView {
            name: player.name.clone(),
            vnum: 0,
        })
        .collect();
    room_characters.extend(
        view.npcs
            .iter()
            .filter(|(_, _, _, location)| location.room == event.room)
            .map(|(_, npc, key, _)| EntityView {
                name: npc.name.clone(),
                vnum: vnum_from_id(key.0.as_ref()),
            }),
    );
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
        clock: view.clock.as_deref().map(|clock| ClockView {
            hour: clock.hour_of_day,
            day: clock.day,
            month: clock.month,
            year: clock.year,
        }),
        vars: crate::triggers::context::TriggerVars {
            remote,
            global,
            self_remote,
            actor_inventory,
            self_contents,
            room_characters,
            ..Default::default()
        },
        ..TriggerContext::default()
    }
}

/// The carried items of `holder`, as pseudo-array entries.
fn items_of(view: &TriggerView, holder: Entity) -> Vec<EntityView> {
    view.carried
        .iter()
        .filter(|(_, _, carried_by)| carried_by.0 == holder)
        .map(|(_, item, _)| EntityView {
            name: item.name.clone(),
            vnum: vnum_from_id(item.def_id.as_ref()),
        })
        .collect()
}

/// Emit the record's lines to the right recipients.
pub(crate) fn deliver(
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
pub const RUNTIME_DROP_KINDS: &[&str] = &[
    "unknown_token",
    "step_cap",
    "nop",
    "send_target",
    "echoaround_target",
    "load_target",
    "purge_target",
    "remote_target",
    "teleport_target",
    "teleport_room",
    "door_room",
    "door_direction",
    "door_flag",
    "door_purge",
    "door_field",
    "force_target",
    "at_room",
    "at_command",
    "zoneecho_room",
    "timer_cancelled",
    "timer_orphan",
    "wait_minute",
    "internal_control",
];

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
    fn type_names_cover_the_events() {
        assert_eq!(TriggerKind::Greet.type_names(), &["greet", "greet-all"]);
        assert_eq!(TriggerKind::Speech.type_names(), &["speech"]);
        assert_eq!(TriggerKind::Command.type_names(), &["command"]);
        assert_eq!(TriggerKind::Entry.type_names(), &["entry"]);
        assert_eq!(TriggerKind::Leave.type_names(), &["leave"]);
        assert!(TriggerKind::Wait.type_names().is_empty());
    }

    #[test]
    fn at_load_reads_only_a_single_load() {
        let parsed = at_load("%load% m 15").expect("a single load");
        assert_eq!(parsed.0, LoadKind::Mob);
        assert_eq!(parsed.1, "15");
        assert!(parsed.2.is_none());
        assert!(at_load("drop quest_token_zone_1").is_none());
        assert!(at_load("%load% m 15\nsay after").is_none());
    }

    #[test]
    fn dir_key_reads_canonical_directions() {
        assert_eq!(dir_key("up"), Some(ExitKey::Up));
        assert_eq!(dir_key(" south "), Some(ExitKey::South));
        assert_eq!(dir_key("sideways"), None);
    }

    #[test]
    fn move_direction_reads_typed_and_raw_moves() {
        assert_eq!(
            move_direction(&PlayerInput::Command(Command::Move {
                direction: ExitKey::North,
            })),
            Some("north".to_owned())
        );
        assert_eq!(
            move_direction(&PlayerInput::RawLine("south".to_owned())),
            Some("south".to_owned())
        );
        assert_eq!(
            move_direction(&PlayerInput::RawLine("look".to_owned())),
            None
        );
    }
}
