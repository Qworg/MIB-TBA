//! Speech: `say`, `tell`, `whisper`/`ask`, the shout channels and `emote`
//! (`act.comm.c`, and `do_echo` of `act.wizard.c`).
//!
//! The acting NPC never receives its own lines (it has no session), so only
//! what the room or the addressee sees is produced. The pack has no colour
//! codes, so the `@` colour syntax `parse_at` translates is left as typed.
//! Channel preferences (`NOSHOUT`, `NOGOSS`, ...) are not modelled: every
//! playing player is on every channel.

use dg_vm::cstr::{half_chop, skip_spaces};
use dg_vm::dispatch::{speech_mtrigger, speech_wtrigger};
use dg_vm::lookup::{Where, get_char_vis, get_player_vis};
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_stdlib::prelude::{Player, RoomTag};

use super::act::{ActObj, ActVict, Flags, To, act, act_full, act_vict};
use super::table::{Channel, LVL_GOD, LVL_IMMORT};
use super::world;
use crate::combat::TbaCombatant;
use crate::dg::host::EcsHost;
use crate::dg::out;

/// `CONFIG_LEVEL_CAN_SHOUT`.
const LEVEL_CAN_SHOUT: i32 = 1;
/// `CONFIG_HOLLER_MOVE_COST`.
const HOLLER_MOVE_COST: i32 = 20;

/// `do_say`.
pub fn do_say(host: &mut EcsHost<'_>, ch: Uid, argument: &str) {
    let argument = skip_spaces(argument);
    if !argument.is_empty() {
        let quiet = Flags {
            sleep: false,
            no_trig: true,
        };
        act_full(
            host,
            &format!("$n says, '{argument}'"),
            false,
            Some(ch),
            ActObj::None,
            ActVict::None,
            To::Room,
            quiet,
        );
        act_full(
            host,
            &format!("You say, '{argument}'"),
            false,
            Some(ch),
            ActObj::None,
            ActVict::None,
            To::Char,
            quiet,
        );
    }
    speech_mtrigger(host, ch, argument);
    speech_wtrigger(host, ch, argument);
}

/// `do_echo` for `emote` (`$n <text>`) and, for the immortal `echo`, the bare
/// text.
pub fn do_echo(host: &mut EcsHost<'_>, ch: Uid, argument: &str, emote: bool) {
    let argument = skip_spaces(argument);
    if argument.is_empty() {
        return;
    }
    let line = if emote {
        format!("$n {argument}")
    } else {
        argument.to_owned()
    };
    act(host, &line, false, ch, To::Room);
    act(host, &line, false, ch, To::Char);
}

/// `is_tell_ok` for an NPC speaker: whether the line may reach `vict`.
fn tell_ok(host: &mut EcsHost<'_>, ch: Uid, vict: Uid) -> bool {
    if ch == vict {
        return false;
    }
    if let Some(room) = host
        .entity(ch)
        .and_then(|e| host.world.get::<mib_stdlib::prelude::Location>(e))
        .map(|l| l.room)
        && world::room_flagged(host, room, RoomTag::Soundproof)
        && host.level(ch) < LVL_GOD
    {
        return false;
    }
    let vict_room = host
        .entity(vict)
        .and_then(|e| host.world.get::<mib_stdlib::prelude::Location>(e))
        .map(|l| l.room);
    if let Some(room) = vict_room
        && world::room_flagged(host, room, RoomTag::Soundproof)
        && host.level(ch) < LVL_GOD
    {
        act_full(
            host,
            "$E can't hear you.",
            false,
            Some(ch),
            ActObj::None,
            ActVict::Char(vict),
            To::Char,
            Flags {
                sleep: true,
                no_trig: false,
            },
        );
        return false;
    }
    let playing = host
        .entity(vict)
        .is_some_and(|entity| out::is_playing(host.world, entity));
    if host.is_npc(vict) || !playing {
        if !host.is_npc(vict) {
            act_full(
                host,
                "$E's linkless at the moment.",
                false,
                Some(ch),
                ActObj::None,
                ActVict::Char(vict),
                To::Char,
                Flags {
                    sleep: true,
                    no_trig: false,
                },
            );
            return false;
        }
        return true;
    }
    true
}

/// `do_tell`: to a player anywhere in the world.
pub fn do_tell(host: &mut EcsHost<'_>, ch: Uid, argument: &str) {
    let (target, rest) = half_chop(argument);
    if target.is_empty() || rest.is_empty() {
        return;
    }
    let vict = if host.level(ch) >= LVL_IMMORT {
        get_char_vis(host, ch, &target, Where::World)
    } else {
        get_player_vis(host, ch, &target, false)
    };
    let Some(vict) = vict else {
        return;
    };
    if !tell_ok(host, ch, vict) {
        return;
    }
    let sleep = Flags {
        sleep: true,
        no_trig: false,
    };
    act_full(
        host,
        &format!("$n tells you, '{rest}'"),
        false,
        Some(ch),
        ActObj::None,
        ActVict::Char(vict),
        To::Vict,
        sleep,
    );
    act_full(
        host,
        &format!("You tell $N, '{rest}'"),
        false,
        Some(ch),
        ActObj::None,
        ActVict::Char(vict),
        To::Char,
        sleep,
    );
}

/// `do_spec_comm`: `whisper` and `ask`, to someone in the room.
pub fn do_spec_comm(host: &mut EcsHost<'_>, ch: Uid, argument: &str, whisper: bool) {
    let (plural, others) = if whisper {
        ("whispers to", "$n whispers something to $N.")
    } else {
        ("asks", "$n asks $N a question.")
    };
    let (target, rest) = half_chop(argument);
    if target.is_empty() || rest.is_empty() {
        return;
    }
    let Some(vict) = dg_vm::lookup::get_char_room_vis(host, ch, &target) else {
        return;
    };
    if vict == ch {
        return;
    }
    act_vict(
        host,
        &format!("$n {plural} you, '{rest}'"),
        false,
        ch,
        vict,
        To::Vict,
    );
    act_vict(host, others, false, ch, vict, To::NotVict);
}

/// `do_gen_comm`: `holler`, `shout`, `gossip`, `auction` and `grats`, heard
/// by every playing player the channel reaches.
pub fn do_gen_comm(host: &mut EcsHost<'_>, ch: Uid, argument: &str, channel: Channel) {
    let name = match channel {
        Channel::Holler => "holler",
        Channel::Shout => "shout",
        Channel::Gossip => "gossip",
        Channel::Auction => "auction",
        Channel::Grats => "congrat",
        Channel::Gemote => {
            host.unsupported("mobcmd.gemote", "global emotes are not modelled");
            return;
        }
    };
    let Some(entity) = host.entity(ch) else {
        return;
    };
    let room = host
        .world
        .get::<mib_stdlib::prelude::Location>(entity)
        .map(|location| location.room);
    if room.is_some_and(|room| world::room_flagged(host, room, RoomTag::Soundproof))
        && host.level(ch) < LVL_GOD
    {
        return;
    }
    if host.level(ch) < LEVEL_CAN_SHOUT {
        return;
    }
    let argument = skip_spaces(argument);
    if argument.is_empty() {
        return;
    }
    if channel == Channel::Holler {
        // A holler costs movement points; an NPC whose combat state exists and
        // is too tired cannot.
        if let Some(mut combatant) = host.world.get_mut::<TbaCombatant>(entity) {
            if combatant.move_points < HOLLER_MOVE_COST {
                return;
            }
            combatant.move_points = combatant.move_points.saturating_sub(HOLLER_MOVE_COST);
        }
    }
    let sleep = Flags {
        sleep: true,
        no_trig: false,
    };
    act_full(
        host,
        &format!("You {name}, '{argument}'"),
        false,
        Some(ch),
        ActObj::None,
        ActVict::None,
        To::Char,
        sleep,
    );
    let line = format!("$n {name}s, '{argument}'");
    let mut listeners: Vec<Uid> = Vec::new();
    let mut query = host.world.query::<(mib_core::prelude::Entity, &Player)>();
    let players: Vec<mib_core::prelude::Entity> = query.iter(host.world).map(|(e, _)| e).collect();
    for player in players {
        if out::is_playing(host.world, player)
            && let Some(uid) = host.uid(player)
        {
            listeners.push(uid);
        }
    }
    for listener in listeners {
        if listener == ch {
            continue;
        }
        let Some(listener_entity) = host.entity(listener) else {
            continue;
        };
        let listener_room = host
            .world
            .get::<mib_stdlib::prelude::Location>(listener_entity)
            .map(|location| location.room);
        if listener_room.is_some_and(|room| world::room_flagged(host, room, RoomTag::Soundproof))
            && host.level(ch) < LVL_GOD
        {
            continue;
        }
        if channel == Channel::Shout {
            let same_zone = match (room, listener_room) {
                (Some(a), Some(b)) => world::zone_of(host, a) == world::zone_of(host, b),
                _ => false,
            };
            if !same_zone || !world::awake(host, listener) {
                continue;
            }
        }
        act_full(
            host,
            &line,
            false,
            Some(ch),
            ActObj::None,
            ActVict::Char(listener),
            To::Vict,
            sleep,
        );
    }
}
