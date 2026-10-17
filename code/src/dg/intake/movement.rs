//! Intake for leave, enter, greet, entry, memory, door and login triggers.
//!
//! The C runs these around `do_simple_move` (`act.movement.c`), `do_doorcmd`
//! and `enter_player_game`. Here they sit on both sides of the engine router:
//!
//! - [`precommit`] (`DgIntake::PreCommit`): before a player's `Move` commits,
//!   `leave_mtrigger`, `leave_wtrigger` and `leave_otrigger`, in that order,
//!   and before a door command commits, `door_mtrigger` then `door_wtrigger`.
//!   A refusal claims the command, so the router and the pack handlers skip it.
//! - [`typed_arrivals`] (`DgIntake::PostRoute`): once the router has moved the
//!   player, [`arrive`]: `entry_mtrigger`, `enter_wtrigger`, `entry_memory_mtrigger`,
//!   `greet_mtrigger` and `greet_memory_mtrigger`. A refusal by `entry`, `enter`
//!   or `greet` puts the mover back in the room it came from.
//! - [`observe_movement`] (`FixedUpdate`): every other change of a character's
//!   `Location` made by the game gets the treatment its kind has in the C. A
//!   move along an exit (the engine's wander, pursuit, follow, flee) is
//!   `do_simple_move`: leave triggers, then the arrival triggers, either of
//!   which can undo the move. Any other relocation (the death respawn, a verb's
//!   teleport) is a transport: `entry_memory_mtrigger`, `greet_mtrigger(-1)`
//!   and `greet_memory_mtrigger` with the results ignored, as `spells.c` does,
//!   and it is never undone. A walking NPC whose move was refused is not
//!   offered the same move again for [`REFUSAL_COOLDOWN`] ticks (the C retries
//!   a hunting mob once per `PULSE_MOBILE`); the move is put back silently,
//!   and a player is shown the room they are held in. A leave trigger that
//!   moves the character itself leaves it where the script put it
//!   (`IN_ROOM(ch) != was_in`). A relocation along an exit that is no walk (a
//!   death respawn into a neighbouring room) cannot be told from one and is
//!   treated as a walk.
//!
//!   The pack's pursuit still announces `follows you!` on every tick of a
//!   cooldown: it must consult [`DgRefusals::refused`] to stop.
//! - [`player_logins`] (`DgIntake::PostRoute`): `enter_player_game` and `nanny`'s
//!   `login_wtrigger`, `greet_mtrigger(-1)` and `greet_memory_mtrigger`.
//!
//! The engine has finished a typed move (the `Visited` quest event, the scene
//! and the prompt) when the arrival triggers run, and nothing here can retract
//! them; a refused arrival shows the room it returns to in a plain form.
//!
//! # Moves the host made
//!
//! A script moves a character through the host, which stamps the mover's
//! [`DgOrder`] straight after changing its `Location`. The C runs no
//! `entry`/`greet` for those (a script `teleport` calls `enter_wtrigger` and
//! nothing else), so [`observe_movement`] leaves a `Location` change alone when
//! the character's `DgOrder` is at least as new as the change. A character
//! seen for the first time is only noted, so its first move gets no trigger.
//! A dispatch from
//! `Update` that stamps a character the game has just moved, before this
//! system has seen the move, hides that move from it.
//!
//! # Directions
//!
//! DG numbers directions north 0, east 1, south 2, west 3, up 4, down 5,
//! northwest 6, northeast 7, southeast 8, southwest 9; the pack's `in` and
//! `out` have no number and are `-1`, the value for "no direction".

use std::collections::HashMap;

use bevy::ecs::message::MessageCursor;
use dg_vm::WorldHost;
use dg_vm::dispatch::{
    door_mtrigger, door_wtrigger, enter_wtrigger, entry_memory_mtrigger, entry_mtrigger,
    greet_memory_mtrigger, greet_mtrigger, leave_mtrigger, leave_otrigger, leave_wtrigger,
    login_wtrigger,
};
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    CarriedBy, Discovered, Doors, Exits, Item, Location, Npc, OnlinePlayers, PendingBundleRestore,
    Player, Room, RoomId, RoomState,
};

use super::DgIntake;
use super::commands::{already_handled, claim, playing_commands, typed_command};
use crate::dg::host::EcsHost;
use crate::dg::ids::DgOrder;
use crate::dg::out::{is_playing, send_to_char};
use crate::dg::with_host;
use crate::prelude::{Position, TbaCombatant, TbaContent, TbaProgress};
use crate::skills::{SkillCommand, parse_skill_line, skill_gate};

/// `SCMD_OPEN`, `SCMD_CLOSE`, `SCMD_UNLOCK`, `SCMD_LOCK`, `SCMD_PICK`.
const SCMD_OPEN: i32 = 0;
const SCMD_CLOSE: i32 = 1;
const SCMD_UNLOCK: i32 = 2;
const SCMD_LOCK: i32 = 3;
const SCMD_PICK: i32 = 4;

/// The room a character was last seen in by movement intake: where a move
/// came from, and where a refused move puts the mover back.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DgLastRoom(pub Entity);

/// A player that has been through the login triggers (`enter_player_game`).
#[derive(Component, Debug, Clone, Copy)]
pub struct DgLoggedIn;

/// A typed move that passed its leave triggers and is waiting for the router:
/// the room it leaves and the direction it takes.
#[derive(Debug, Clone, Copy)]
struct PreMove {
    from: Entity,
    dir: i32,
}

/// The typed moves of this frame that [`precommit`] let through, by mover.
#[derive(Resource, Debug, Default)]
pub struct DgPreMoves(HashMap<Entity, PreMove>);

/// The DG direction number (`NORTH`..`SOUTHWEST`) of an exit, or `-1` for the
/// pack's `in` and `out`, from the host's one direction table.
pub fn dg_dir(key: ExitKey) -> i32 {
    crate::dg::host::dir_number(key).map_or(-1, |dir| i32::try_from(dir).unwrap_or(-1))
}

/// The room an entity stands in.
fn room_of(world: &World, entity: Entity) -> Option<Entity> {
    world.get::<Location>(entity).map(|location| location.room)
}

/// The direction of the exit that leads from `from` to `to`: the lowest DG
/// number if several do, `-1` if only `in` or `out` does, `None` if no exit
/// does (a transport).
fn dir_between(world: &World, from: Entity, to: Entity) -> Option<i32> {
    world
        .get::<Exits>(from)
        .into_iter()
        .flat_map(|exits| exits.map.iter())
        .filter(|(_, destination)| **destination == to)
        .map(|(key, _)| dg_dir(*key))
        .min_by_key(|dir| if *dir < 0 { i32::MAX } else { *dir })
}

/// Whether a door is closed (a locked door is closed too), from the room's
/// runtime state or, with none, the door's authored default. `None` for an exit
/// with no door.
fn door_state(world: &World, room: Entity, key: ExitKey) -> Option<(bool, bool)> {
    let door = world.get::<Doors>(room)?.by_dir.get(&key)?;
    let (closed, locked) = world
        .get::<RoomState>(room)
        .and_then(|state| state.doors.get(&key))
        .map_or((door.default_closed, door.default_locked), |state| {
            (state.closed, state.locked)
        });
    Some((closed || locked, locked))
}

/// Where the exit `key` of `room` leads for `mover`, when `perform_move`
/// would let `do_simple_move` run: the exit exists (a hidden one only for who
/// found it) and its door is not closed.
fn open_exit(world: &World, mover: Entity, room: Entity, key: ExitKey) -> Option<Entity> {
    let exits = world.get::<Exits>(room)?;
    let destination = *exits.map.get(&key)?;
    if exits.hidden.contains(&key) {
        let id = RoomId(world.get::<Room>(room)?.key.clone());
        let found = world
            .get::<Discovered>(mover)
            .is_some_and(|discovered| discovered.0.contains(&(id, key)));
        if !found {
            return None;
        }
    }
    if door_state(world, room, key).is_some_and(|(closed, _)| closed) {
        return None;
    }
    Some(destination)
}

/// `leave_mtrigger`, `leave_wtrigger`, `leave_otrigger`: none may refuse, and
/// none may move the actor (`IN_ROOM(ch) != was_in`).
fn leave_allowed(host: &mut EcsHost<'_>, mover: Entity, from: Entity, dir: i32) -> bool {
    let (Some(actor), Some(room)) = (host.uid(mover), host.uid(from)) else {
        return true;
    };
    leave_mtrigger(host, actor, dir) != 0
        && host.in_room(actor) == Some(room)
        && leave_wtrigger(host, room, actor, dir) != 0
        && host.in_room(actor) == Some(room)
        && leave_otrigger(host, room, actor, dir) != 0
        && host.in_room(actor) == Some(room)
}

/// The door command a typed command is: its `SCMD_*` and target.
fn door_command(command: &Command) -> Option<(i32, Option<&str>)> {
    match command {
        Command::Open { target } => Some((SCMD_OPEN, target.as_deref())),
        Command::Close { target } => Some((SCMD_CLOSE, target.as_deref())),
        Command::Unlock { target } => Some((SCMD_UNLOCK, target.as_deref())),
        Command::Lock { target } => Some((SCMD_LOCK, target.as_deref())),
        Command::Pick { target } => Some((SCMD_PICK, target.as_deref())),
        _ => None,
    }
}

/// The direction of the door a command names, as the engine's door commands
/// resolve it: a direction word, the room's only door when none is named, or a
/// word in a door's name.
fn resolve_door(world: &World, room: Entity, target: Option<&str>) -> Option<ExitKey> {
    let doors = &world.get::<Doors>(room)?.by_dir;
    let Some(target) = target else {
        return match (doors.len(), doors.keys().next()) {
            (1, Some(key)) => Some(*key),
            _ => None,
        };
    };
    let needle = target.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    if let Some(key) = ExitKey::from_word(&needle) {
        return doors.contains_key(&key).then_some(key);
    }
    doors
        .iter()
        .find(|(_, door)| {
            door.name
                .as_ref()
                .is_some_and(|name| name.to_lowercase().contains(&needle))
        })
        .map(|(key, _)| *key)
}

/// Whether the engine would carry out this door command: the point in
/// `do_gen_door` where `do_doorcmd` is reached and its triggers run.
fn door_would_change(
    world: &mut World,
    mover: Entity,
    room: Entity,
    key: ExitKey,
    subcmd: i32,
) -> bool {
    let Some((closed, locked)) = door_state(world, room, key) else {
        return false;
    };
    let Some(door) = world
        .get::<Doors>(room)
        .and_then(|doors| doors.by_dir.get(&key))
    else {
        return false;
    };
    let (door_key, pickable) = (door.key.clone(), door.pickable);
    let has_key = |world: &mut World| {
        door_key.as_ref().is_some_and(|wanted| {
            let mut items = world.query::<(&Item, &CarriedBy)>();
            items
                .iter(world)
                .any(|(item, holder)| holder.0 == mover && &item.def_id == wanted)
        })
    };
    match subcmd {
        SCMD_OPEN => closed && !locked,
        SCMD_CLOSE => !closed,
        SCMD_UNLOCK => locked && has_key(world),
        SCMD_LOCK => closed && !locked && has_key(world),
        SCMD_PICK => locked && pickable,
        _ => false,
    }
}

/// `DgIntake::PreCommit`: the triggers that can stop a player's move or door
/// command before it happens. A refusal claims the command, so the router and
/// the pack handlers skip it and the script's own text is all the player sees.
pub fn precommit(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    world
        .get_resource_or_insert_with(DgPreMoves::default)
        .0
        .clear();
    for msg in playing_commands(world, &mut cursor) {
        if already_handled(world, msg.id) {
            continue;
        }
        if let PlayerInput::RawLine(line) = &msg.input
            && let Some(SkillCommand::PickLock { direction }) = parse_skill_line(line)
        {
            // The pack's own lock-picking verb is `do_gen_door`'s `pick`. The
            // C reaches `do_doorcmd` only after `ok_pick` rolled a success; the
            // roll cannot be anticipated here, so the triggers run for every
            // attempt the handler's own checks would let as far as the roll.
            if pick_reaches_the_roll(world, msg.session, &direction)
                && !before_door(world, msg.session, SCMD_PICK, Some(&direction))
            {
                claim(world, &msg);
            }
            continue;
        }
        let Some(command) = typed_command(&msg.input) else {
            continue;
        };
        let allowed = match &command {
            Command::Move { direction } => before_move(world, msg.session, *direction),
            other => match door_command(other) {
                Some((subcmd, target)) => before_door(world, msg.session, subcmd, target),
                None => true,
            },
        };
        if !allowed {
            claim(world, &msg);
        }
    }
}

/// Whether the pack's `pick_lock <direction>` gets as far as the skill roll:
/// the skill is known at the character's level, the character stands, and a
/// direction word is named (`handle_skill_commands` answers anything else
/// before `ok_pick`).
fn pick_reaches_the_roll(world: &World, mover: Entity, direction: &str) -> bool {
    let (Some(content), Some(progress)) = (
        world.get_resource::<TbaContent>(),
        world.get::<TbaProgress>(mover),
    ) else {
        return false;
    };
    skill_gate(content, progress, "pick_lock", "").is_none()
        && world
            .get::<TbaCombatant>(mover)
            .is_some_and(|combatant| combatant.position >= Position::Standing)
        && ExitKey::from_word(&direction.to_lowercase()).is_some()
}

/// The leave triggers of a typed move; records the move when they pass. A
/// move through no exit, or a closed door, never reaches `do_simple_move`, so
/// no trigger runs and the router's own refusal stands.
fn before_move(world: &mut World, mover: Entity, key: ExitKey) -> bool {
    let Some(from) = room_of(world, mover) else {
        return true;
    };
    if open_exit(world, mover, from, key).is_none() {
        return true;
    }
    let dir = dg_dir(key);
    if !with_host(world, |host| leave_allowed(host, mover, from, dir)) {
        return false;
    }
    world
        .get_resource_or_insert_with(DgPreMoves::default)
        .0
        .insert(mover, PreMove { from, dir });
    true
}

/// `door_mtrigger` then `door_wtrigger` for a door command the engine would
/// carry out.
fn before_door(world: &mut World, mover: Entity, subcmd: i32, target: Option<&str>) -> bool {
    let Some(room) = room_of(world, mover) else {
        return true;
    };
    let Some(key) = resolve_door(world, room, target) else {
        return true;
    };
    if !door_would_change(world, mover, room, key, subcmd) {
        return true;
    }
    let dir = dg_dir(key);
    with_host(world, |host| {
        let Some(actor) = host.uid(mover) else {
            return true;
        };
        door_mtrigger(host, actor, subcmd, dir) != 0 && door_wtrigger(host, actor, subcmd, dir) != 0
    })
}

/// The room's name, description and exits, as a player sees it on looking.
fn describe_room(world: &World, room: Entity) -> String {
    let Some(data) = world.get::<Room>(room) else {
        return String::new();
    };
    let mut text = format!("{}\n{}", data.name, data.description.render_plain());
    if let Some(exits) = world.get::<Exits>(room) {
        let mut names: Vec<&str> = exits
            .map
            .keys()
            .filter(|key| !exits.hidden.contains(key))
            .map(|key| key.as_str())
            .collect();
        names.sort_unstable();
        text.push_str(&format!("\nExits: {}", names.join(" ")));
    }
    text
}

/// Record where a character now stands: its last-room record and the engine's
/// record of where the player is. Does nothing for a despawned character.
fn settle(world: &mut World, mover: Entity, room: Entity) {
    let Ok(mut entity) = world.get_entity_mut(mover) else {
        return;
    };
    entity.insert(DgLastRoom(room));
    if let Some(mut online) = world.get_resource_mut::<OnlinePlayers>() {
        for record in online.map.values_mut() {
            if record.entity == mover {
                record.room = room;
            }
        }
    }
}

/// Put a character in `room` again after a move was undone: its `Location`,
/// the engine's record of where the player is and its last-room record. An
/// arrival that was undone stamps the character as the newest in the room it
/// returns to (`char_to_room`); a character that never left keeps its place in
/// the room's people list. Does nothing for a despawned character.
fn restore(world: &mut World, mover: Entity, room: Entity, restamp: bool) {
    let Ok(mut entity) = world.get_entity_mut(mover) else {
        return;
    };
    if let Some(mut location) = entity.get_mut::<Location>() {
        location.room = room;
    }
    if !restamp && let Some(mut order) = entity.get_mut::<DgOrder>() {
        // The stamp is current again: placement stamping must not renew it.
        order.set_changed();
    }
    settle(world, mover, room);
    if restamp {
        with_host(world, |host| host.stamp_order(mover));
    }
}

/// Put a character back in the room it came from and, if it is a player, show
/// it that room (`look_at_room`).
fn send_back(world: &mut World, mover: Entity, was_in: Entity) {
    restore(world, mover, was_in, true);
    show_room(world, mover, was_in);
}

/// Show a player the room they stand in. NPCs see nothing.
fn show_room(world: &mut World, who: Entity, room: Entity) {
    if world.get::<Player>(who).is_some() {
        let text = describe_room(world, room);
        send_to_char(world, who, &text);
    }
}

/// How a character got to its new room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Travel {
    /// Along an exit, in this direction (`do_simple_move`).
    Walk(i32),
    /// Some other way (a transport): nothing can refuse it.
    Jump,
}

/// The triggers after the mover is in its new room, in the C's order. A walk
/// is `do_simple_move`: `entry_mtrigger` and `enter_wtrigger` (either refusing
/// sends the mover back), `entry_memory_mtrigger`, then `greet_mtrigger`
/// (refusing sends the mover back) and `greet_memory_mtrigger`. A jump is what
/// `spells.c` does after a transport: `entry_memory_mtrigger`,
/// `greet_mtrigger(-1)` and `greet_memory_mtrigger`, results ignored.
///
/// The router has already shown the new room, so a mover sent back is shown the
/// one it returns to, whichever trigger refused. The C shows it only after a
/// `greet` refusal. Returns whether the mover was sent back.
fn arrive(world: &mut World, mover: Entity, from: Entity, travel: Travel) -> bool {
    let Some(to) = room_of(world, mover) else {
        return false;
    };
    let verdict = with_host(world, |host| {
        // This runs straight after a move the game made.
        host.stamp_order(mover);
        let Some(actor) = host.uid(mover) else {
            return Verdict::Stay;
        };
        let Travel::Walk(dir) = travel else {
            entry_memory_mtrigger(host, actor);
            greet_mtrigger(host, actor, -1);
            greet_memory_mtrigger(host, actor);
            return Verdict::Stay;
        };
        let Some(room) = host.in_room(actor) else {
            return Verdict::Stay;
        };
        if entry_mtrigger(host, actor) == 0 || enter_wtrigger(host, room, actor, dir) == 0 {
            return Verdict::Back;
        }
        entry_memory_mtrigger(host, actor);
        if greet_mtrigger(host, actor, dir) == 0 {
            return Verdict::Back;
        }
        greet_memory_mtrigger(host, actor);
        Verdict::Stay
    });
    // A trigger may have extracted the mover; the sweep despawned it.
    if world.get_entity(mover).is_err() {
        return false;
    }
    match verdict {
        Verdict::Back => {
            send_back(world, mover, from);
            true
        }
        Verdict::Stay => {
            if let Ok(mut entity) = world.get_entity_mut(mover) {
                entity.insert(DgLastRoom(to));
            }
            false
        }
    }
}

/// What the arrival triggers decided.
enum Verdict {
    Stay,
    Back,
}

/// `DgIntake::PostRoute`: the arrival triggers of the typed moves
/// [`precommit`] let through, for those the router carried out.
pub fn typed_arrivals(world: &mut World) {
    let pending: Vec<(Entity, PreMove)> = world
        .get_resource_or_insert_with(DgPreMoves::default)
        .0
        .drain()
        .collect();
    for (mover, pre) in pending {
        match room_of(world, mover) {
            Some(now) if now != pre.from => {
                arrive(world, mover, pre.from, Travel::Walk(pre.dir));
            }
            _ => {}
        }
    }
}

/// The characters [`game_moves`] looks at, and what it reads of each.
type SeenQuery = QueryState<
    (
        Entity,
        Ref<'static, Location>,
        Option<Ref<'static, DgOrder>>,
        Option<&'static DgLastRoom>,
        Has<Player>,
        Has<DgLoggedIn>,
    ),
    Or<(With<Npc>, With<Player>)>,
>;

/// A character that changed room on its own since movement intake last saw it.
struct Mover {
    entity: Entity,
    from: Entity,
    to: Entity,
}

/// `FixedUpdate`, after the game's movers and before their placement is
/// stamped: the characters whose `Location` the game changed since the last
/// look, other than by a typed move (those are [`typed_arrivals`]) or a script
/// (see the module docs). A character seen for the first time is only noted.
fn game_moves(world: &mut World, seen: &mut Option<SeenQuery>) -> Vec<Mover> {
    let this_run = world.read_change_tick();
    let seen = seen.get_or_insert_with(|| world.query_filtered());
    let mut first_sight: Vec<(Entity, Entity)> = Vec::new();
    let mut moved: Vec<Mover> = Vec::new();
    let mut moved_by_host: Vec<(Entity, Entity)> = Vec::new();
    for (entity, location, order, last, is_player, logged_in) in seen.iter(world) {
        if is_player && !logged_in {
            continue;
        }
        let to = location.room;
        match last {
            None => first_sight.push((entity, to)),
            Some(last) if last.0 == to => {}
            Some(last) => {
                let by_game = order.is_none_or(|order| {
                    location
                        .last_changed()
                        .is_newer_than(order.last_changed(), this_run)
                });
                if by_game {
                    moved.push(Mover {
                        entity,
                        from: last.0,
                        to,
                    });
                } else {
                    moved_by_host.push((entity, to));
                }
            }
        }
    }
    for (entity, room) in first_sight.into_iter().chain(moved_by_host) {
        world.entity_mut(entity).insert(DgLastRoom(room));
    }
    moved.retain(|mover| world.get_entity(mover.entity).is_ok());
    moved
}

/// How many fixed ticks a walking NPC whose move a trigger refused is not
/// offered the same move again (`PULSE_MOBILE`, ten seconds at ten ticks a
/// second): the pack's pursuit retries on every tick.
pub const REFUSAL_COOLDOWN: u32 = 100;

/// The moves of NPCs that triggers refused, with the ticks left on each.
#[derive(Resource, Debug, Default)]
pub struct DgRefusals {
    tick: u32,
    moves: HashMap<Entity, (Entity, Entity, u32)>,
}

impl DgRefusals {
    /// Advance one tick and forget the refusals that ran out.
    fn advance(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        let now = self.tick;
        self.moves
            .retain(|_, (_, _, until)| until.wrapping_sub(now) < REFUSAL_COOLDOWN);
    }

    /// Whether the move `from` to `to` was refused recently for `mover`.
    pub fn refused(&self, mover: Entity, from: Entity, to: Entity) -> bool {
        self.moves
            .get(&mover)
            .is_some_and(|(was, wanted, _)| *was == from && *wanted == to)
    }

    /// Record that `mover`'s move from `from` to `to` was refused, so it is
    /// not offered again until the refusal cools. Public so a test can seed
    /// one; `observe_movement` calls it on a sent-back NPC.
    pub fn refuse(&mut self, mover: Entity, from: Entity, to: Entity) {
        let until = self.tick.wrapping_add(REFUSAL_COOLDOWN);
        self.moves.insert(mover, (from, to, until));
    }
}

/// `FixedUpdate`: the movement triggers for every move the game made that was
/// not a typed player move.
///
/// The move is already done when this sees it, so the leave triggers run with
/// the mover put back in the room it left, and a refusal leaves it there
/// (`do_simple_move` returns before the character leaves). A move along no exit
/// is a transport, which no trigger can refuse.
pub fn observe_movement(world: &mut World, mut seen: Local<Option<SeenQuery>>) {
    world
        .get_resource_or_insert_with(DgRefusals::default)
        .advance();
    for mover in game_moves(world, &mut seen) {
        let Some(dir) = dir_between(world, mover.from, mover.to) else {
            arrive(world, mover.entity, mover.from, Travel::Jump);
            continue;
        };
        let is_npc = world.get::<Npc>(mover.entity).is_some();
        let cooling = is_npc
            && world
                .get_resource::<DgRefusals>()
                .is_some_and(|refusals| refusals.refused(mover.entity, mover.from, mover.to));
        if cooling {
            restore(world, mover.entity, mover.from, false);
            continue;
        }
        if let Some(mut location) = world.get_mut::<Location>(mover.entity) {
            location.room = mover.from;
        }
        let allowed = with_host(world, |host| {
            leave_allowed(host, mover.entity, mover.from, dir)
        });
        let sent_back = if allowed {
            if let Some(mut location) = world.get_mut::<Location>(mover.entity) {
                location.room = mover.to;
            }
            arrive(world, mover.entity, mover.from, Travel::Walk(dir))
        } else {
            // The character never left, unless a trigger moved it itself
            // (`IN_ROOM(ch) != was_in`): then it stays where the script put it.
            match room_of(world, mover.entity) {
                Some(now) if now == mover.from => {
                    restore(world, mover.entity, mover.from, false);
                    show_room(world, mover.entity, mover.from);
                    true
                }
                Some(now) => {
                    settle(world, mover.entity, now);
                    false
                }
                None => false,
            }
        };
        if sent_back && is_npc {
            world
                .get_resource_or_insert_with(DgRefusals::default)
                .refuse(mover.entity, mover.from, mover.to);
        }
    }
}

/// `DgIntake::PostRoute`: `enter_player_game` and `nanny` for a player who has
/// just entered the game: `login_wtrigger` for their room, then
/// `greet_mtrigger` with direction `-1` and `greet_memory_mtrigger`.
pub fn player_logins(world: &mut World) {
    let mut fresh = world.query_filtered::<(Entity, &Location), (
        With<Player>,
        Without<DgLoggedIn>,
        Without<PendingBundleRestore>,
    )>();
    let entering: Vec<(Entity, Entity)> = fresh
        .iter(world)
        .map(|(entity, location)| (entity, location.room))
        .collect();
    for (player, room) in entering {
        if !is_playing(world, player) {
            continue;
        }
        world
            .entity_mut(player)
            .insert((DgLoggedIn, DgLastRoom(room)));
        with_host(world, |host| {
            let (Some(actor), Some(room)) = (host.uid(player), host.uid(room)) else {
                return;
            };
            login_wtrigger(host, room, actor);
            greet_mtrigger(host, actor, -1);
            greet_memory_mtrigger(host, actor);
        });
    }
}

/// Register this module's systems.
pub fn register(app: &mut App) {
    app.init_resource::<DgPreMoves>();
    app.init_resource::<DgRefusals>();
    app.add_systems(Update, precommit.in_set(DgIntake::PreCommit));
    app.add_systems(
        Update,
        (player_logins, typed_arrivals)
            .chain()
            .in_set(DgIntake::PostRoute),
    );
    app.add_systems(
        FixedUpdate,
        observe_movement
            .after(mib_stdlib::prelude::wander_system)
            .after(crate::combat::pursuit_system)
            .after(crate::party::follow_system)
            .before(crate::dg::ids::stamp_placement_order),
    );
}
