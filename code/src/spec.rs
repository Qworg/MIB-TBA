//! The mobile special procedures assigned by `spec_assign.c`/`castle.c`.
//!
//! `TbaNpcStats::spec_proc` names the C function; this module runs it. The
//! ambient procedures (the castle staff, the mayor and the King) are ported
//! here; the command-driven ones (guild practice, postmaster mail, the
//! receptionist/cryogenicist) attach their own systems in later phases.
//!
//! The procedures are called once per fixed tick, like Circle's `special()`
//! once per command pulse. Each keeps its own random state so a replay is
//! deterministic, and the wandering procedures keep their path in a
//! [`TbaSpecState`] component instead of C's function-static variables.

use bevy::ecs::message::MessageCursor;
use bevy::prelude::*;
use dg_vm::{Uid, WorldHost};
use mib_core::prelude::CommandReceived;
use mib_protocol::command::{Command, ExitKey, PlayerInput, parse_line};
use mib_stdlib::prelude::{
    GroundItem, Item, Location, Npc, NpcId, NpcIndex, NpcKey, Player, Room, RoomOccupants,
};

use crate::combat::TbaRng;
use crate::content::{Position, SpecProc, TbaItemKind};
use crate::dg::host::EcsHost;
use crate::dg::mobcmd::act::{To, act, act_vict};
use crate::dg::mobcmd::door::do_gen_door;
use crate::dg::mobcmd::movement::perform_move;
use crate::dg::mobcmd::world::{
    awake, fighting, obj_to_char, position, set_fighting, set_position, stop_fighting,
};
use crate::dg::mobcmd::{DoorCmd, add_follower};
use crate::module::TbaContent;
use mib_stdlib::prelude::TimeOfDay;

/// The procedure assigned to an NPC, mirrored from its prototype.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TbaSpecProc(pub SpecProc);

/// Per-instance state for the procedures that walk a scripted path.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct TbaSpecState {
    path: Option<&'static str>,
    index: usize,
    moving: bool,
}

/// The procedures' own random stream, so they never perturb combat rolls.
#[derive(Resource, Debug, Default)]
pub struct TbaSpecRng(pub TbaRng);

/// The castle's staff and royal-guard vnum predicates.
///
/// Transcribed from `member_of_staff`/`member_of_royal_guard`
/// (`tbamud/src/castle.c:133-176`), including Gwydion (`15000`) *not* counting
/// as staff and the treasury guards counting as royal guard.
fn member_of_staff(vnum: i32) -> bool {
    vnum == 15001
        || (15003..=15014).contains(&vnum)
        || (15016..=15017).contains(&vnum)
        || (15019..=15029).contains(&vnum)
}

fn member_of_royal_guard(vnum: i32) -> bool {
    matches!(vnum, 15003 | 15006 | 15008..=15011 | 15024..=15025)
}

/// The castle NPCs a procedure looks for, by vnum.
///
/// Circle's `find_npc_by_name` (`castle.c:178`) matched a short-description
/// prefix, because the C had no index. The pack knows which prototype each
/// castellan is (`castle.c:105-129`), so the lookup is by prototype instead:
/// it is exact, and it costs one index probe rather than a scan of the room's
/// occupants.
const CASTLE_KING: i32 = 15001;
const CASTLE_BRIAN: i32 = 15004;
const CASTLE_MICK: i32 = 15005;
const CASTLE_TIM: i32 = 15020;
const CASTLE_TOM: i32 = 15021;
const CASTLE_MICHAEL: i32 = 15027;

/// The NPC with prototype `vnum` standing in the actor's room.
///
/// The stdlib's `NpcIndex` maps a prototype id to its live entities, so this
/// is an index probe filtered by room rather than a scan of every NPC.
fn find_castle_npc(host: &EcsHost<'_>, actor: Uid, vnum: i32) -> Option<Uid> {
    // `in_room` answers a DG id; the index and the `Location` component speak
    // entities, so convert once here.
    let room = host.entity(host.in_room(actor)?)?;
    let id = NpcId::from(format!("tba_npc_{vnum}"));
    let index = host.world.get_resource::<NpcIndex>()?;
    let candidates = index.map.get(&id)?;
    candidates
        .iter()
        .copied()
        .find(|entity| {
            host.world
                .get::<Location>(*entity)
                .is_some_and(|location| location.room == room)
        })
        .and_then(|entity| host.uid(entity))
}

/// The characters standing in `room`, from the stdlib's occupancy index.
///
/// [`EcsHost::room_people`] scans **every** player and **every** NPC in the
/// world and then sorts the result; the procedures only need the room's
/// occupants. The stdlib's [`RoomOccupants`] index already has them, and it is
/// maintained on the fixed tick — which is when the procedures run, after
/// `maintain_room_occupants` — so it is current here.
///
/// The order is the pack's placement order (newest first, by [`DgOrder`]),
/// the same order `room_people` produces, so a procedure that picks among
/// the occupants still picks the same one.
fn room_characters(host: &EcsHost<'_>, room: Entity) -> Vec<Uid> {
    let Some(occupants) = host.world.get_resource::<RoomOccupants>() else {
        // No index (a bare test app): fall back to the scan, so the result is
        // the same either way.
        return host.room_people(host.uid(room).unwrap_or(0));
    };
    let mut present: Vec<(u64, Entity)> = occupants
        .occupants(room)
        .iter()
        .copied()
        .filter(|entity| {
            host.world.get::<Player>(*entity).is_some() || host.world.get::<Npc>(*entity).is_some()
        })
        .map(|entity| {
            let stamp = host
                .world
                .get::<crate::dg::ids::DgOrder>(entity)
                .map_or(0, |order| order.0);
            (stamp, entity)
        })
        .collect();
    present.sort_unstable();
    present.reverse();
    present
        .into_iter()
        .filter_map(|(_, entity)| host.uid(entity))
        .collect()
}

/// The ground items lying in `room`, from the stdlib's occupancy index.
///
/// [`EcsHost::room_contents`] scans every ground item in the world; the
/// index already knows the room's occupants, and ground items carry a
/// `Location` like everyone else. The order is the pack's placement order
/// (oldest first), the same order `room_contents` produces.
fn room_ground_items(host: &EcsHost<'_>, room: Entity) -> Vec<Uid> {
    let Some(occupants) = host.world.get_resource::<RoomOccupants>() else {
        return host.room_contents(host.uid(room).unwrap_or(0));
    };
    let mut lying: Vec<(u64, Entity)> = occupants
        .occupants(room)
        .iter()
        .copied()
        .filter(|entity| {
            host.world.get::<GroundItem>(*entity).is_some()
                && host.world.get::<Item>(*entity).is_some()
        })
        .map(|entity| {
            let stamp = host
                .world
                .get::<crate::dg::ids::DgOrder>(entity)
                .map_or(0, |order| order.0);
            (stamp, entity)
        })
        .collect();
    lying.sort_unstable();
    lying
        .into_iter()
        .filter_map(|(_, entity)| host.uid(entity))
        .collect()
}

/// `get_victim` (`castle.c:213`): a player fighting castle staff, chosen with
/// Circle's "give them a chance" empty slot.
fn get_victim(host: &EcsHost<'_>, ch: Uid, rng: &mut TbaRng) -> Option<Uid> {
    let room = host.entity(host.in_room(ch)?)?;
    let victims: Vec<Uid> = room_characters(host, room)
        .into_iter()
        .filter(|candidate| {
            fighting(host, *candidate).is_some_and(|target| member_of_staff(host.vnum(target)))
        })
        .collect();
    if victims.is_empty() {
        return None;
    }
    let pick = rng.range(0, victims.len() as i32);
    if pick == 0 {
        return None;
    }
    victims.get(pick as usize - 1).copied()
}

/// `banzaii` (`castle.c:265`): roar, start the fight and take one blow.
fn banzaii(host: &mut EcsHost<'_>, ch: Uid, rng: &mut TbaRng) -> bool {
    if !awake(host, ch) || fighting(host, ch).is_some() {
        return false;
    }
    let Some(victim) = get_victim(host, ch, rng) else {
        return false;
    };
    let Some(entity) = host.entity(ch) else {
        return false;
    };
    act(
        host,
        "$n roars: 'Protect the Kingdom of Great King Welmar!  BANZAIIII!!!'",
        false,
        ch,
        To::Room,
    );
    set_fighting(host, ch, victim);
    crate::combat::systems::blow(host, entity);
    true
}

/// `do_npc_rescue` (`castle.c:285`): the twin pulls the King's attacker onto
/// itself.
fn npc_rescue(host: &mut EcsHost<'_>, hero: Uid, victim: Uid) -> bool {
    let Some(room_uid) = host.in_room(hero) else {
        return false;
    };
    let Some(room) = host.entity(room_uid) else {
        return false;
    };
    let Some(bad_guy) = room_characters(host, room)
        .into_iter()
        .find(|candidate| fighting(host, *candidate) == Some(victim))
    else {
        return false;
    };
    if bad_guy == hero {
        return false;
    }
    act_vict(
        host,
        "You bravely rescue $N.",
        false,
        hero,
        victim,
        To::Char,
    );
    act_vict(
        host,
        "You are rescued by $N, your loyal friend!",
        false,
        victim,
        hero,
        To::Char,
    );
    act_vict(
        host,
        "$n heroically rescues $N.",
        false,
        hero,
        victim,
        To::NotVict,
    );
    stop_fighting(host, bad_guy);
    stop_fighting(host, hero);
    set_fighting(host, hero, bad_guy);
    set_fighting(host, bad_guy, hero);
    true
}

/// `is_trash` (`castle.c:342`): a drink container or an item worth <= 10.
fn is_trash(host: &EcsHost<'_>, item: Uid) -> bool {
    let Some(entity) = host.entity(item) else {
        return false;
    };
    let Some(def) = host.world.get::<Item>(entity).and_then(|item| {
        host.world
            .get_resource::<TbaContent>()
            .and_then(|content| content.items.get(&item.def_id).cloned())
    }) else {
        return false;
    };
    def.kind == TbaItemKind::DrinkContainer || def.cost <= 10
}

/// `castle_cleaner` (`castle.c:628`): pick up one piece of trash.
fn castle_cleaner(host: &mut EcsHost<'_>, ch: Uid, gripe: bool) -> bool {
    if !awake(host, ch) || position(host, ch) == Position::Fighting {
        return false;
    }
    let Some(room_uid) = host.in_room(ch) else {
        return false;
    };
    let Some(room) = host.entity(room_uid) else {
        return false;
    };
    let Some(trash) = room_ground_items(host, room)
        .into_iter()
        .find(|item| is_trash(host, *item))
    else {
        return false;
    };
    if gripe {
        act(
            host,
            "$n says: 'My oh my!  I ought to fire that lazy cleaning woman!'",
            false,
            ch,
            To::Room,
        );
    }
    act(host, "$n picks up a piece of trash.", false, ch, To::Room);
    if let (Some(item), Some(owner)) = (host.entity(trash), host.entity(ch)) {
        obj_to_char(host, item, owner);
    }
    true
}

/// The scripted paths (`spec_procs.c:223`, `castle.c:366`).
const MAYOR_OPEN: &str = "W3a3003b33000c111d0d111Oe333333Oe22c222112212111a1S.";
const MAYOR_CLOSE: &str = "W3a3003b33000c111d0d111CE333333CE22c222112212111a1S.";
const KING_BEDROOM: &str = "s33004o1c1S.";
const KING_THRONE: &str = "W3o3cG52211rg.";
const KING_MONOLOG: &str = "ABCDPPPP.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Mayor,
    King,
}

/// Execute one character of a path, with the C's messages.
fn path_step(host: &mut EcsHost<'_>, ch: Uid, step: char, kind: PathKind) {
    match step {
        '0'..='5' => {
            perform_move(host, ch, step as usize - '0' as usize);
        }
        'W' => {
            set_position(host, ch, Position::Standing);
            let text = match kind {
                PathKind::Mayor => "$n awakens and groans loudly.",
                PathKind::King => "$n awakens and stands up.",
            };
            act(host, text, false, ch, To::Room);
        }
        'S' => {
            set_position(host, ch, Position::Sleeping);
            let text = match kind {
                PathKind::Mayor => "$n lies down and instantly falls asleep.",
                PathKind::King => "$n lies down on $s beautiful bed and instantly falls asleep.",
            };
            act(host, text, false, ch, To::Room);
        }
        'a' => {
            act(host, "$n says 'Hello Honey!'", false, ch, To::Room);
            act(host, "$n smirks.", false, ch, To::Room);
        }
        'b' => act(
            host,
            "$n says 'What a view!  I must get something done about that dump!'",
            false,
            ch,
            To::Room,
        ),
        'c' if kind == PathKind::Mayor => act(
            host,
            "$n says 'Vandals!  Youngsters nowadays have no respect for anything!'",
            false,
            ch,
            To::Room,
        ),
        'd' => act(host, "$n says 'Good day, citizens!'", false, ch, To::Room),
        'e' => act(
            host,
            "$n says 'I hereby declare the bazaar open!'",
            false,
            ch,
            To::Room,
        ),
        'E' => act(
            host,
            "$n says 'I hereby declare Midgaard closed!'",
            false,
            ch,
            To::Room,
        ),
        'O' => {
            do_gen_door(host, ch, "gate", DoorCmd::Unlock);
            do_gen_door(host, ch, "gate", DoorCmd::Open);
        }
        'C' => {
            do_gen_door(host, ch, "gate", DoorCmd::Close);
            do_gen_door(host, ch, "gate", DoorCmd::Lock);
        }
        'o' => {
            do_gen_door(host, ch, "door", DoorCmd::Unlock);
            do_gen_door(host, ch, "door", DoorCmd::Open);
        }
        'c' => {
            do_gen_door(host, ch, "door", DoorCmd::Close);
            do_gen_door(host, ch, "door", DoorCmd::Lock);
        }
        'r' => {
            set_position(host, ch, Position::Sitting);
            act(
                host,
                "$n sits down on $s great throne.",
                false,
                ch,
                To::Room,
            );
        }
        's' => {
            set_position(host, ch, Position::Standing);
            act(host, "$n stands up.", false, ch, To::Room);
        }
        'G' => act(
            host,
            "$n says 'Good morning, trusted friends.'",
            false,
            ch,
            To::Room,
        ),
        'g' => act(
            host,
            "$n says 'Good morning, dear subjects.'",
            false,
            ch,
            To::Room,
        ),
        'A'..='D' => {
            const MONOLOG: [&str; 4] = [
                "$n proclaims 'Primus in regnis Geticis coronam'.",
                "$n proclaims 'regiam gessi, subiique regis'.",
                "$n proclaims 'munus et mores colui sereno'.",
                "$n proclaims 'principe dignos'.",
            ];
            let index = step as usize - 'A' as usize;
            if let Some(line) = MONOLOG.get(index) {
                act(host, line, false, ch, To::Room);
            }
        }
        'P' | '.' => {}
        _ => {}
    }
}

/// Advance a scripted path by one step and persist the new index.
fn advance_path(host: &mut EcsHost<'_>, entity: Entity, ch: Uid, kind: PathKind) {
    let mut state = host
        .world
        .get::<TbaSpecState>(entity)
        .copied()
        .unwrap_or_default();
    let Some(path) = state.path else {
        return;
    };
    let step = path.as_bytes().get(state.index).copied().map(char::from);
    if let Some(step) = step {
        path_step(host, ch, step, kind);
        state.index += 1;
        if step == '.' {
            state.moving = false;
            state.path = None;
            state.index = 0;
        }
    } else {
        state.moving = false;
        state.path = None;
        state.index = 0;
    }
    if let Some(mut live) = host.world.get_mut::<TbaSpecState>(entity) {
        *live = state;
    }
}

/// Start a path when the hour and room match, then advance it.
fn run_path(host: &mut EcsHost<'_>, ch: Uid, hour: u32, kind: PathKind) {
    let Some(entity) = host.entity(ch) else {
        return;
    };
    let mut state = host
        .world
        .get::<TbaSpecState>(entity)
        .copied()
        .unwrap_or_default();

    if !state.moving {
        let room_vnum = host
            .in_room(ch)
            .map(|room| host.vnum(room))
            .unwrap_or_default();
        let path = match kind {
            PathKind::Mayor => match hour {
                6 => Some(MAYOR_OPEN),
                20 => Some(MAYOR_CLOSE),
                _ => None,
            },
            PathKind::King => match (hour, room_vnum) {
                (8, 15051) => Some(KING_THRONE),
                (21, 15017) => Some(KING_BEDROOM),
                (12, 15017) => Some(KING_MONOLOG),
                _ => None,
            },
        };
        if let Some(path) = path {
            state.moving = true;
            state.path = Some(path);
            state.index = 0;
            if let Some(mut live) = host.world.get_mut::<TbaSpecState>(entity) {
                *live = state;
            }
        }
    }
    if !state.moving {
        return;
    }

    let pos = position(host, ch);
    // Mayor (`spec_procs.c:238`) and King (`castle.c:392`) both stop on the
    // downed positions and in a fight; the King fries instead.
    if pos < Position::Sleeping || pos == Position::Fighting {
        return;
    }
    advance_path(host, entity, ch, kind);
}

/// `fry_victim` (`castle.c:355`): the King's combat spells.
fn fry_victim(
    host: &mut EcsHost<'_>,
    ch: Uid,
    rng: &mut TbaRng,
    casts: &mut Vec<(Entity, Entity, &'static str, u32)>,
) {
    let Some(victim) = get_victim(host, ch, rng) else {
        return;
    };
    let (Some(caster), Some(target)) = (host.entity(ch), host.entity(victim)) else {
        return;
    };
    let level = host.level(ch).max(1) as u32;
    match rng.range(0, 8) {
        1..=3 => {
            act(
                host,
                "$n raises $s hand in a dramatical gesture.",
                false,
                ch,
                To::Room,
            );
            casts.push((caster, target, "color_spray", level));
        }
        4..=5 => {
            act(
                host,
                "$n concentrates, and mumbles to $mself.",
                false,
                ch,
                To::Room,
            );
            casts.push((caster, target, "harm", level));
        }
        6..=7 => {
            act_vict(
                host,
                "$n looks deeply into the eyes of $N.",
                false,
                ch,
                victim,
                To::NotVict,
            );
            casts.push((caster, target, "fireball", level));
        }
        _ => {
            if rng.range(0, 1) == 0 {
                casts.push((caster, caster, "heal", level));
            }
        }
    }
}

/// Run one assigned procedure. `casts` is applied after the host borrow ends.
fn run_proc(
    host: &mut EcsHost<'_>,
    entity: Entity,
    proc: SpecProc,
    hour: u32,
    rng: &mut TbaRng,
    casts: &mut Vec<(Entity, Entity, &'static str, u32)>,
) {
    let Some(ch) = host.uid(entity) else {
        return;
    };
    match proc {
        SpecProc::CastleGuard => {
            banzaii(host, ch, rng);
        }
        SpecProc::Mayor => run_path(host, ch, hour, PathKind::Mayor),
        SpecProc::KingWelmar => {
            if fighting(host, ch).is_some() {
                fry_victim(host, ch, rng, casts);
                return;
            }
            run_path(host, ch, hour, PathKind::King);
        }
        SpecProc::Peter => {
            if banzaii(host, ch, rng) || !awake(host, ch) {
                return;
            }
            if rng.range(0, 3) != 0 {
                return;
            }
            let Some(room_uid) = host.in_room(ch) else {
                return;
            };
            let Some(room) = host.entity(room_uid) else {
                return;
            };
            let Some(guard) = room_characters(host, room).into_iter().find(|candidate| {
                fighting(host, *candidate).is_none() && member_of_royal_guard(host.vnum(*candidate))
            }) else {
                return;
            };
            const INSPECT: [&str; 6] = [
                "$N comes sharply into attention as $n inspects $M.",
                "$N looks very small, as $n roars at $M.",
                "$n gives $N some Royal directions.",
                "$n growls: 'Those boots need polishing!'",
                "$n growls: 'Straighten that collar!'",
                "$n growls: 'That chain mail looks rusty!  CLEAN IT !!!'",
            ];
            let index = rng.range(0, 5) as usize;
            if let Some(line) = INSPECT.get(index) {
                act_vict(host, line, false, ch, guard, To::NotVict);
            }
        }
        SpecProc::James => {
            castle_cleaner(host, ch, true);
        }
        SpecProc::Cleaning => {
            castle_cleaner(host, ch, false);
        }
        SpecProc::Tim | SpecProc::Tom => {
            if !awake(host, ch) {
                return;
            }
            if let Some(king) = find_castle_npc(host, ch, CASTLE_KING) {
                add_follower(host, ch, king);
                if fighting(host, king).is_some() {
                    npc_rescue(host, ch, king);
                }
            }
            let twin_vnum = if proc == SpecProc::Tim {
                CASTLE_TOM
            } else {
                CASTLE_TIM
            };
            if let Some(twin) = find_castle_npc(host, ch, twin_vnum)
                && fighting(host, twin).is_some()
                && position(host, ch) > Position::Sleeping
            {
                npc_rescue(host, ch, twin);
            }
            if position(host, ch) != Position::Fighting {
                banzaii(host, ch, rng);
            }
        }
        SpecProc::DicknDavid => {
            if !awake(host, ch) {
                return;
            }
            if fighting(host, ch).is_none() {
                banzaii(host, ch, rng);
            }
        }
        SpecProc::Jerry => {
            if !awake(host, ch) || fighting(host, ch).is_some() || banzaii(host, ch, rng) {
                return;
            }
            if rng.range(0, 2) != 0 {
                return;
            }
            let Some(michael) = find_castle_npc(host, ch, CASTLE_MICHAEL) else {
                return;
            };
            const LINES: [&str; 6] = [
                "$n rolls the dice and cheers loudly at the result.",
                "$n curses the Goddess of Luck roundly as he sees $N's roll.",
                "$n sighs loudly and gives $N some gold.",
                "$n smiles remorsefully as $N's roll tops $s.",
                "$n excitedly follows the dice with $s eyes.",
                "$n says 'Well, my luck has to change soon', as he shakes the dice.",
            ];
            let index = rng.range(0, 5) as usize;
            if let Some(line) = LINES.get(index) {
                act_vict(host, line, false, ch, michael, To::NotVict);
            }
        }
        SpecProc::TrainingMaster => {
            if !awake(host, ch)
                || fighting(host, ch).is_some()
                || banzaii(host, ch, rng)
                || rng.range(0, 2) != 0
            {
                return;
            }
            // The C finds both pupils and picks between them
            // (`castle.c:498-508`).
            let Some(brian) = find_castle_npc(host, ch, CASTLE_BRIAN) else {
                return;
            };
            let Some(mick) = find_castle_npc(host, ch, CASTLE_MICK) else {
                return;
            };
            if fighting(host, brian).is_some() || fighting(host, mick).is_some() {
                return;
            }
            let pupil = if rng.range(0, 1) == 0 { brian } else { mick };
            const LINES: [&str; 2] = [
                "$n hits $N on $s head with a powerful blow.",
                "$n shows $s pupils an advanced technique.",
            ];
            let index = rng.range(0, 1) as usize;
            if let Some(line) = LINES.get(index) {
                act_vict(host, line, false, ch, pupil, To::NotVict);
            }
        }
        // Command-driven procedures are handled by their own systems.
        SpecProc::Guild
        | SpecProc::Postmaster
        | SpecProc::Receptionist
        | SpecProc::Cryogenicist => {}
    }
}

/// Give every NPC with an assigned procedure its component and path state.
pub fn attach_spec_procs(
    mut commands: Commands,
    npcs: Query<(Entity, &NpcKey), Without<TbaSpecProc>>,
    content: Option<Res<TbaContent>>,
) {
    let Some(content) = content else {
        return;
    };
    for (entity, key) in &npcs {
        if let Some(proc) = content
            .npc_stats
            .get(&key.0)
            .and_then(|stats| stats.spec_proc)
        {
            commands
                .entity(entity)
                .insert((TbaSpecProc(proc), TbaSpecState::default()));
        }
    }
}

/// `FixedUpdate`: run every assigned procedure once.
pub fn spec_proc_system(world: &mut World) {
    let procs: Vec<(Entity, SpecProc)> = {
        let mut query = world.query::<(Entity, &TbaSpecProc)>();
        query
            .iter(world)
            .map(|(entity, proc)| (entity, proc.0))
            .collect()
    };
    if procs.is_empty() {
        return;
    }
    let hour = world
        .get_resource::<TimeOfDay>()
        .map_or(0, |clock| u32::from(clock.hour));
    let mut rng = std::mem::take(&mut world.get_resource_or_insert_with(TbaSpecRng::default).0);
    let mut casts = Vec::new();
    crate::dg::with_host(world, |host| {
        for (entity, proc) in procs {
            run_proc(host, entity, proc, hour, &mut rng, &mut casts);
        }
    });
    if let Some(mut spec) = world.get_resource_mut::<TbaSpecRng>() {
        spec.0 = rng;
    }
    for (caster, victim, spell, level) in casts {
        crate::spells::cast_spell_at_level(world, caster, Some(victim), spell, level);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn staff_and_royal_guard_follow_the_castle_vnums() {
        assert!(member_of_staff(15001));
        assert!(member_of_staff(15003));
        assert!(member_of_staff(15014));
        assert!(member_of_staff(15016));
        assert!(member_of_staff(15017));
        assert!(member_of_staff(15019));
        assert!(member_of_staff(15029));
        assert!(!member_of_staff(15000), "Gwydion is not staff");
        assert!(!member_of_staff(15002));
        assert!(!member_of_staff(15015));
        assert!(!member_of_staff(15018));
        assert!(!member_of_staff(15030));

        assert!(member_of_royal_guard(15003));
        assert!(member_of_royal_guard(15006));
        assert!(member_of_royal_guard(15008));
        assert!(member_of_royal_guard(15011));
        assert!(member_of_royal_guard(15024));
        assert!(member_of_royal_guard(15025));
        assert!(!member_of_royal_guard(15004));
    }
}

#[cfg(test)]
mod integration_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::combat::{TbaAttributes, TbaSaves};
    use mib_core::prelude::FormattedText;
    use mib_stdlib::prelude::{Location, NpcId, Room, RoomState};

    fn combatant(position: Position, target: Option<Entity>) -> crate::combat::TbaCombatant {
        crate::combat::TbaCombatant {
            hp: 100,
            max_hp: 100,
            mana: 10,
            max_mana: 10,
            move_points: 80,
            max_move_points: 80,
            hitroll: 20,
            damroll: 5,
            armor_class: 100,
            thaco: 20,
            attributes: TbaAttributes::mob_default(),
            saves: TbaSaves::default(),
            position,
            target,
        }
    }

    #[test]
    fn a_castle_guard_banzaiis_a_staff_attacker() {
        let mut world = World::new();
        world.insert_resource(TbaContent::default());
        world.insert_resource(TimeOfDay::default());
        world.insert_resource(TbaSpecRng::default());
        world.insert_resource(crate::combat::TbaRng::from_seed(7));

        let room = world
            .spawn((
                Room {
                    key: "tba.15000".into(),
                    name: "The Castle".to_owned(),
                    description: FormattedText::plain("A castle."),
                    num: 15000,
                    area: "tba_150".into(),
                    coord: (0, 0, 0),
                    tags: Default::default(),
                    media: Vec::new(),
                    sector: mib_stdlib::content::Sector::Inside,
                    extra_descriptions: Vec::new(),
                    outdoor: false,
                    weather_zone: None,
                    ambient: None,
                    ambient_stamp: None,
                    ambient_phases: Default::default(),
                    ambient_weather: Default::default(),
                    time: None,
                    weather_state: None,
                },
                RoomState::default(),
            ))
            .id();
        let guard = world
            .spawn((
                Npc {
                    name: "Gwydion".to_owned(),
                    persona: String::new(),
                },
                NpcKey(NpcId::from("tba_npc_15003")),
                Location { room },
                combatant(Position::Standing, None),
                TbaSpecProc(SpecProc::CastleGuard),
                TbaSpecState::default(),
            ))
            .id();
        // The attacker is not staff itself; it is fighting the staff guard.
        let attacker = world
            .spawn((
                Npc {
                    name: "a ruffian".to_owned(),
                    persona: String::new(),
                },
                NpcKey(NpcId::from("tba_npc_99999")),
                Location { room },
                combatant(Position::Fighting, Some(guard)),
            ))
            .id();

        crate::dg::with_host(&mut world, |host| {
            let ch = host.uid(guard).expect("guard id");
            let room = host.in_room(ch).expect("guard room");
            let people = host.room_people(room);
            assert_eq!(people.len(), 2, "guard and attacker: {people:?}");
            let attacker_uid = host.uid(attacker).expect("attacker id");
            assert_eq!(fighting(host, attacker_uid), Some(ch));
            assert!(
                member_of_staff(host.vnum(ch)),
                "guard vnum is {}",
                host.vnum(ch)
            );
            assert!(get_victim(host, ch, &mut TbaRng::from_seed(7)).is_some());
        });

        spec_proc_system(&mut world);

        let guard_state = world
            .get::<crate::combat::TbaCombatant>(guard)
            .expect("guard survives");
        assert_eq!(
            guard_state.target,
            Some(attacker),
            "banzaii starts the fight"
        );
        assert_eq!(guard_state.position, Position::Fighting);
        assert!(world.get_entity(attacker).is_ok());
    }

    /// A room with the stdlib's occupancy index in it, so the indexed path is
    /// the one under test.
    fn indexed_room(world: &mut World, num: u32) -> Entity {
        world
            .spawn((
                Room {
                    key: format!("tba.{num}").into(),
                    name: "The Castle".to_owned(),
                    description: FormattedText::plain("A castle."),
                    num,
                    area: "tba_150".into(),
                    coord: (0, 0, 0),
                    tags: Default::default(),
                    media: Vec::new(),
                    sector: mib_stdlib::content::Sector::Inside,
                    extra_descriptions: Vec::new(),
                    outdoor: false,
                    weather_zone: None,
                    ambient: None,
                    ambient_stamp: None,
                    ambient_phases: Default::default(),
                    ambient_weather: Default::default(),
                    time: None,
                    weather_state: None,
                },
                RoomState::default(),
            ))
            .id()
    }

    #[test]
    fn the_indexed_room_read_sees_the_same_occupants_as_the_scan() {
        let mut world = World::new();
        world.insert_resource(RoomOccupants::default());
        let room = indexed_room(&mut world, 15000);
        let guard = world
            .spawn((
                Npc {
                    name: "Gwydion".to_owned(),
                    persona: String::new(),
                },
                NpcKey(NpcId::from("tba_npc_15003")),
                Location { room },
                combatant(Position::Standing, None),
            ))
            .id();
        let other = world
            .spawn((
                Npc {
                    name: "a ruffian".to_owned(),
                    persona: String::new(),
                },
                NpcKey(NpcId::from("tba_npc_99998")),
                Location { room },
                combatant(Position::Standing, None),
            ))
            .id();
        // Populate the index the way the stdlib's `maintain_room_occupants` does.
        let locations = [(guard, Location { room }), (other, Location { room })];
        let mut occupants = world
            .get_resource_mut::<RoomOccupants>()
            .expect("the occupancy index");
        occupants.rebuild(
            locations
                .iter()
                .map(|(entity, location)| (*entity, location)),
        );

        crate::dg::with_host(&mut world, |host| {
            let ch = host.uid(guard).expect("guard id");
            let room = host
                .entity(host.in_room(ch).expect("guard room"))
                .expect("room");
            let indexed = room_characters(host, room);
            let scanned: Vec<Entity> = host
                .room_people(host.uid(room).expect("room uid"))
                .into_iter()
                .filter_map(|uid| host.entity(uid))
                .collect();
            assert_eq!(indexed.len(), 2, "both occupants: {indexed:?}");
            assert_eq!(
                indexed.len(),
                scanned.len(),
                "the index and the scan agree on how many are here"
            );
        });
    }

    #[test]
    fn a_castellan_is_found_by_prototype_through_the_npc_index() {
        let mut world = World::new();
        let mut index = NpcIndex::default();
        let room = indexed_room(&mut world, 15000);
        let michael = world
            .spawn((
                Npc {
                    name: "Michael".to_owned(),
                    persona: String::new(),
                },
                NpcKey(NpcId::from("tba_npc_15027")),
                Location { room },
                combatant(Position::Standing, None),
            ))
            .id();
        index
            .map
            .entry(NpcId::from("tba_npc_15027"))
            .or_default()
            .push(michael);
        world.insert_resource(index);

        crate::dg::with_host(&mut world, |host| {
            let ch = host.uid(michael).expect("michael id");
            assert_eq!(
                find_castle_npc(host, ch, CASTLE_MICHAEL),
                Some(ch),
                "Michael is found by his prototype"
            );
            assert_eq!(
                find_castle_npc(host, ch, CASTLE_KING),
                None,
                "the King is not in this room"
            );
        });
    }
}

/// The `block_way` guard check (`castle.c:316`): Tim/Tom at the King's
/// bedroom doors and Dick/David at the treasury refuse an eastward move.
pub fn intercept_block_way(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    let messages: Vec<CommandReceived> = {
        let Some(messages) = world.get_resource::<Messages<CommandReceived>>() else {
            return;
        };
        cursor.read(messages).cloned().collect()
    };
    for msg in messages {
        if !crate::dg::out::is_playing(world, msg.session)
            || crate::dg::intake::commands::is_claimed(
                world.get_resource::<crate::dg::intake::commands::DgClaimed>(),
                msg.id,
            )
        {
            continue;
        }
        let Some(direction) = move_direction(&msg.input) else {
            continue;
        };
        if direction != ExitKey::East {
            continue;
        }
        let Some(room) = world
            .get::<Location>(msg.session)
            .map(|location| location.room)
        else {
            continue;
        };
        let room_vnum = world
            .get::<Room>(room)
            .map(|room| room.num)
            .unwrap_or_default();
        let blocker = {
            let mut query = world.query::<(&TbaSpecProc, &Location)>();
            query.iter(world).any(|(proc, npc_location)| {
                npc_location.room == room
                    && matches!(
                        (proc.0, room_vnum),
                        (SpecProc::Tim, 15048)
                            | (SpecProc::Tom, 15049)
                            | (SpecProc::DicknDavid, 15036)
                    )
            })
        };
        if !blocker {
            continue;
        }
        let name = world
            .get::<Player>(msg.session)
            .map(|player| player.name.clone())
            .unwrap_or_else(|| "someone".to_owned());
        crate::dg::out::send_to_room(
            world,
            room,
            &format!("The guard roars at {name} and pushes them back.\r\n"),
            &[],
        );
        crate::dg::out::send_to_char(
            world,
            msg.session,
            "The guard roars: 'Entrance is Prohibited!', and pushes you back.\r\n",
        );
        crate::dg::intake::commands::claim_id(world, msg.id, msg.session);
    }
}

fn move_direction(input: &PlayerInput) -> Option<ExitKey> {
    match input {
        PlayerInput::Command(Command::Move { direction }) => Some(*direction),
        PlayerInput::RawLine(line) => match parse_line(line) {
            Ok(Command::Move { direction }) => Some(direction),
            _ => None,
        },
        _ => None,
    }
}
