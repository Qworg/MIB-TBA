//! Body positions: `stand`, `sit`, `rest`, `sleep` and `wake`
//! (`act.movement.c`).
//!
//! The pack keeps no furniture occupancy (per-instance object values), so
//! sitting on furniture checks only that the object is furniture; and it has
//! no `AFF_SLEEP` or flight, so the sleep-spell and floating branches are
//! never reached.

use dg_vm::cstr::one_argument;
use dg_vm::lookup::{get_char_room_vis, get_obj_in_list_vis};
use dg_vm::{Uid, WorldHost};

use super::act::{ActObj, ActVict, Flags, To, act, act_full, act_obj, act_vict};
use super::world::{self, position, set_position, to_char};
use crate::content::Position;
use crate::dg::host::EcsHost;

/// `ITEM_FURNITURE`.
const ITEM_FURNITURE: i32 = 6;

/// `do_stand`.
pub fn do_stand(host: &mut EcsHost<'_>, ch: Uid) {
    match position(host, ch) {
        Position::Standing => to_char(host, ch, "You are already standing.\r\n"),
        Position::Sitting => {
            to_char(host, ch, "You stand up.\r\n");
            act(host, "$n clambers to $s feet.", true, ch, To::Room);
            let next = if world::fighting(host, ch).is_some() {
                Position::Fighting
            } else {
                Position::Standing
            };
            set_position(host, ch, next);
        }
        Position::Resting => {
            to_char(host, ch, "You stop resting, and stand up.\r\n");
            act(host, "$n stops resting, and clambers on $s feet.", true, ch, To::Room);
            set_position(host, ch, Position::Standing);
        }
        Position::Sleeping => to_char(host, ch, "You have to wake up first!\r\n"),
        Position::Fighting => to_char(host, ch, "Do you not consider fighting as standing?\r\n"),
        _ => {
            to_char(host, ch, "You stop floating around, and put your feet on the ground.\r\n");
            act(
                host,
                "$n stops floating around, and puts $s feet on the ground.",
                true,
                ch,
                To::Room,
            );
            set_position(host, ch, Position::Standing);
        }
    }
}

/// `do_sit`.
pub fn do_sit(host: &mut EcsHost<'_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    let furniture = host
        .in_room(ch)
        .and_then(|room| get_obj_in_list_vis(host, ch, &arg, &host.room_contents(room)));
    match position(host, ch) {
        Position::Standing => match furniture {
            None => {
                to_char(host, ch, "You sit down.\r\n");
                act(host, "$n sits down.", false, ch, To::Room);
                set_position(host, ch, Position::Sitting);
            }
            Some(furniture) if host.obj_type(furniture) != ITEM_FURNITURE => {
                to_char(host, ch, "You can't sit on that!\r\n");
            }
            Some(furniture) => {
                act_obj(host, "You sit down upon $p.", true, ch, furniture, To::Char);
                act_obj(host, "$n sits down upon $p.", true, ch, furniture, To::Room);
                set_position(host, ch, Position::Sitting);
            }
        },
        Position::Sitting => to_char(host, ch, "You're sitting already.\r\n"),
        Position::Resting => {
            to_char(host, ch, "You stop resting, and sit up.\r\n");
            act(host, "$n stops resting.", true, ch, To::Room);
            set_position(host, ch, Position::Sitting);
        }
        Position::Sleeping => to_char(host, ch, "You have to wake up first.\r\n"),
        Position::Fighting => to_char(host, ch, "Sit down while fighting? Are you MAD?\r\n"),
        _ => {
            to_char(host, ch, "You stop floating around, and sit down.\r\n");
            act(host, "$n stops floating around, and sits down.", true, ch, To::Room);
            set_position(host, ch, Position::Sitting);
        }
    }
}

/// `do_rest`.
pub fn do_rest(host: &mut EcsHost<'_>, ch: Uid) {
    match position(host, ch) {
        Position::Standing => {
            to_char(host, ch, "You sit down and rest your tired bones.\r\n");
            act(host, "$n sits down and rests.", true, ch, To::Room);
            set_position(host, ch, Position::Resting);
        }
        Position::Sitting => {
            to_char(host, ch, "You rest your tired bones.\r\n");
            act(host, "$n rests.", true, ch, To::Room);
            set_position(host, ch, Position::Resting);
        }
        Position::Resting => to_char(host, ch, "You are already resting.\r\n"),
        Position::Sleeping => to_char(host, ch, "You have to wake up first.\r\n"),
        Position::Fighting => to_char(host, ch, "Rest while fighting?  Are you MAD?\r\n"),
        _ => {
            to_char(host, ch, "You stop floating around, and stop to rest your tired bones.\r\n");
            act(host, "$n stops floating around, and rests.", false, ch, To::Room);
            set_position(host, ch, Position::Resting);
        }
    }
}

/// `do_sleep`.
pub fn do_sleep(host: &mut EcsHost<'_>, ch: Uid) {
    match position(host, ch) {
        Position::Standing | Position::Sitting | Position::Resting => {
            to_char(host, ch, "You go to sleep.\r\n");
            act(host, "$n lies down and falls asleep.", true, ch, To::Room);
            set_position(host, ch, Position::Sleeping);
        }
        Position::Sleeping => to_char(host, ch, "You are already sound asleep.\r\n"),
        Position::Fighting => to_char(host, ch, "Sleep while fighting?  Are you MAD?\r\n"),
        _ => {
            to_char(host, ch, "You stop floating around, and lie down to sleep.\r\n");
            act(host, "$n stops floating around, and lie down to sleep.", true, ch, To::Room);
            set_position(host, ch, Position::Sleeping);
        }
    }
}

/// `do_wake`.
pub fn do_wake(host: &mut EcsHost<'_>, ch: Uid, argument: &str) {
    let (arg, _) = one_argument(argument);
    let mut myself = false;
    if !arg.is_empty() {
        if position(host, ch) == Position::Sleeping {
            to_char(host, ch, "Maybe you should wake yourself up first.\r\n");
        } else if let Some(vict) = get_char_room_vis(host, ch, &arg) {
            if vict == ch {
                myself = true;
            } else if world::awake(host, vict) {
                act_vict(host, "$E is already awake.", false, ch, vict, To::Char);
            } else if position(host, vict) < Position::Sleeping {
                act_vict(host, "$E's in pretty bad shape!", false, ch, vict, To::Char);
            } else {
                act_vict(host, "You wake $M up.", false, ch, vict, To::Char);
                act_full(
                    host,
                    "You are awakened by $n.",
                    false,
                    Some(ch),
                    ActObj::None,
                    ActVict::Char(vict),
                    To::Vict,
                    Flags {
                        sleep: true,
                        no_trig: false,
                    },
                );
                set_position(host, vict, Position::Sitting);
            }
        } else {
            to_char(host, ch, "No one by that name here.\r\n");
        }
        if !myself {
            return;
        }
    }
    if position(host, ch) > Position::Sleeping {
        to_char(host, ch, "You are already awake...\r\n");
    } else {
        to_char(host, ch, "You awaken, and sit up.\r\n");
        act(host, "$n awakens.", true, ch, To::Room);
        set_position(host, ch, Position::Sitting);
    }
}
