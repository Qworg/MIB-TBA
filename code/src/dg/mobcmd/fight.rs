//! Fighting and following: `hit`/`kill`, `assist`, `flee`, `follow` and
//! `unfollow` (`act.offensive.c`, `act.movement.c`, `utils.c`).
//!
//! The pack resolves blows in its combat round, so starting a fight here
//! aims both sides at each other and leaves the first blow to the round
//! (the C lands it at once inside `hit()`).

use dg_vm::cstr::one_argument;
use dg_vm::lookup::get_char_room_vis;
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_stdlib::prelude::RoomTag;

use super::act::{To, act, act_vict};
use super::movement;
use super::world::{self, DIR_COUNT, position, room_of, to_char};
use crate::content::Position;
use crate::dg::host::EcsHost;
use crate::party::TbaFollowing;

/// `CONFIG_NOPERSON`.
const NO_PERSON: &str = "No one by that name here.\r\n";

/// `hit(ch, vict)` as the pack models it: both sides are fighting each other,
/// the victim only if it is conscious and not already fighting.
fn start_fight(host: &mut EcsHost<'_>, ch: Uid, vict: Uid) {
    if ch == vict {
        return;
    }
    if position(host, ch) > Position::Stunned && world::fighting(host, ch).is_none() {
        world::set_fighting(host, ch, vict);
    }
    if position(host, vict) > Position::Stunned && world::fighting(host, vict).is_none() {
        world::set_fighting(host, vict, ch);
        world::remember_attacker(host, vict, ch);
    }
}

/// Whether `ch` may fight in its room: `damage()` refuses in a peaceful room.
fn peaceful(host: &EcsHost<'_>, ch: Uid) -> bool {
    room_of(host, ch).is_some_and(|room| world::room_flagged(host, room, RoomTag::Peaceful))
}

/// `do_hit` (and `do_kill`, which is `do_hit` for everyone but a god).
pub fn do_hit(host: &mut EcsHost<'_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        to_char(host, ch, "Hit who?\r\n");
        return;
    }
    let Some(vict) = get_char_room_vis(host, ch, &arg) else {
        to_char(host, ch, "That player is not here.\r\n");
        return;
    };
    if vict == ch {
        to_char(host, ch, "You hit yourself...OUCH!.\r\n");
        act_vict(
            host,
            "$n hits $mself, and says OUCH!",
            false,
            ch,
            vict,
            To::Room,
        );
        return;
    }
    if position(host, ch) == Position::Standing && world::fighting(host, ch) != Some(vict) {
        if peaceful(host, ch) {
            to_char(
                host,
                ch,
                "This room just has such a peaceful, easy feeling...\r\n",
            );
            return;
        }
        start_fight(host, ch, vict);
    } else {
        to_char(host, ch, "You're fighting the best you can!\r\n");
    }
}

/// `do_assist`.
pub fn do_assist(host: &mut EcsHost<'_>, ch: Uid, argument: &str) {
    if world::fighting(host, ch).is_some() {
        to_char(
            host,
            ch,
            "You're already fighting!  How can you assist someone else?\r\n",
        );
        return;
    }
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        to_char(host, ch, "Whom do you wish to assist?\r\n");
        return;
    }
    let Some(helpee) = get_char_room_vis(host, ch, &arg) else {
        to_char(host, ch, NO_PERSON);
        return;
    };
    if helpee == ch {
        to_char(host, ch, "You can't help yourself any more than this!\r\n");
        return;
    }
    let opponent = world::fighting(host, helpee).or_else(|| {
        let room = host.in_room(ch)?;
        host.room_people(room)
            .into_iter()
            .find(|person| world::fighting(host, *person) == Some(helpee))
    });
    let Some(opponent) = opponent else {
        act_vict(
            host,
            "But nobody is fighting $M!",
            false,
            ch,
            helpee,
            To::Char,
        );
        return;
    };
    if !host.can_see(ch, opponent) {
        act_vict(
            host,
            "You can't see who is fighting $M!",
            false,
            ch,
            helpee,
            To::Char,
        );
        return;
    }
    to_char(host, ch, "You join the fight!\r\n");
    act_vict(host, "$N assists you!", false, helpee, ch, To::Char);
    act_vict(host, "$n assists $N.", false, ch, helpee, To::NotVict);
    if !peaceful(host, ch) {
        start_fight(host, ch, opponent);
    }
}

/// `do_flee`.
pub fn do_flee(host: &mut EcsHost<'_>, ch: Uid) {
    if position(host, ch) < Position::Fighting {
        to_char(host, ch, "You are in pretty bad shape, unable to flee!\r\n");
        return;
    }
    let Some(room) = room_of(host, ch) else {
        return;
    };
    for _ in 0..6 {
        let attempt = usize::try_from(host.rand_number(0, DIR_COUNT as i64 - 1)).unwrap_or(0);
        // `CAN_GO`: an exit that leads somewhere and is not shut. Death-trap
        // rooms are not modelled, so none is avoided.
        let Some(exit) = super::door::exit_of(host, room, attempt)
            .filter(|exit| !exit.door.as_ref().is_some_and(|door| door.closed))
        else {
            continue;
        };
        act(host, "$n panics, and attempts to flee!", true, ch, To::Room);
        let was_fighting = world::fighting(host, ch);
        if movement::simple_move(host, ch, attempt, &exit) {
            to_char(host, ch, "You flee head over heels.\r\n");
            if world::fighting(host, ch).is_some() {
                world::stop_fighting(host, ch);
            }
            if let Some(foe) = was_fighting
                && world::fighting(host, foe) == Some(ch)
            {
                world::stop_fighting(host, foe);
            }
        } else {
            act(host, "$n tries to flee, but can't!", true, ch, To::Room);
        }
        return;
    }
    to_char(host, ch, "PANIC!  You couldn't escape!\r\n");
}

// -- following ----------------------------------------------------------------

/// `circle_follow(ch, victim)`: whether `ch` is `victim` or anyone `victim`
/// follows, so that following would close a loop.
pub fn circle_follow(host: &EcsHost<'_>, ch: Uid, victim: Uid) -> bool {
    let mut at = Some(victim);
    for _ in 0..1024 {
        match at {
            Some(k) if k == ch => return true,
            Some(k) => at = world::master(host, k),
            None => return false,
        }
    }
    true
}

/// `stop_follower(ch)`.
pub fn stop_follower(host: &mut EcsHost<'_>, ch: Uid) {
    let Some(master) = world::master(host, ch) else {
        return;
    };
    act_vict(host, "You stop following $N.", false, ch, master, To::Char);
    act_vict(
        host,
        "$n stops following $N.",
        true,
        ch,
        master,
        To::NotVict,
    );
    if host.can_see(master, ch) {
        act_vict(host, "$n stops following you.", true, ch, master, To::Vict);
    }
    if let Some(entity) = host.entity(ch) {
        host.world.entity_mut(entity).remove::<TbaFollowing>();
    }
}

/// `add_follower(ch, leader)`. `ch` must not follow anyone yet.
pub fn add_follower(host: &mut EcsHost<'_>, ch: Uid, leader: Uid) {
    if world::master(host, ch).is_some() {
        return;
    }
    let (Some(entity), Some(leader_entity)) = (host.entity(ch), host.entity(leader)) else {
        return;
    };
    let Some(room) = room_of(host, leader) else {
        return;
    };
    host.world.entity_mut(entity).insert(TbaFollowing {
        leader: leader_entity,
        last_room: room,
    });
    act_vict(host, "You now follow $N.", false, ch, leader, To::Char);
    if host.can_see(leader, ch) {
        act_vict(host, "$n starts following you.", true, ch, leader, To::Vict);
    }
    act_vict(
        host,
        "$n starts to follow $N.",
        true,
        ch,
        leader,
        To::NotVict,
    );
}

/// `do_follow`.
pub fn do_follow(host: &mut EcsHost<'_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    let leader = if arg.is_empty() {
        match world::master(host, ch) {
            Some(master) => {
                let name = host.name(master);
                to_char(host, ch, &format!("You are following {name}.\r\n"));
            }
            None => to_char(host, ch, "Whom do you wish to follow?\r\n"),
        }
        return;
    } else {
        let Some(leader) = get_char_room_vis(host, ch, &arg) else {
            to_char(host, ch, NO_PERSON);
            return;
        };
        leader
    };
    if world::master(host, ch) == Some(leader) {
        act_vict(
            host,
            "You are already following $M.",
            false,
            ch,
            leader,
            To::Char,
        );
        return;
    }
    // The pack has no charm affect, so a follower is never bound to its master.
    if leader == ch {
        if world::master(host, ch).is_none() {
            to_char(host, ch, "You are already following yourself.\r\n");
            return;
        }
        stop_follower(host, ch);
    } else {
        if circle_follow(host, ch, leader) {
            to_char(
                host,
                ch,
                "Sorry, but following in loops is not allowed.\r\n",
            );
            return;
        }
        if world::master(host, ch).is_some() {
            stop_follower(host, ch);
        }
        add_follower(host, ch, leader);
    }
}

/// `do_unfollow`.
pub fn do_unfollow(host: &mut EcsHost<'_>, ch: Uid) {
    if world::master(host, ch).is_some() {
        stop_follower(host, ch);
    } else {
        to_char(host, ch, "You are not following anyone.\r\n");
    }
}
