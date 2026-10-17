//! Socials: every verb of the pack's `tba.socials` catalog as a command
//! (`do_action` of `act.social.c`, and the social half of the command lookup
//! in `command_interpreter`).

use dg_vm::Uid;
use dg_vm::WorldHost;
use dg_vm::cstr::{one_argument, str_cmp, two_arguments};
use dg_vm::lookup::get_char_room_vis;

use super::act::{ActObj, ActVict, Flags, To, act_full};
use super::world::{self, position_number, to_char};
use crate::content::SocialDef;
use crate::dg::host::EcsHost;
use crate::module::TbaContent;

/// The social a typed word means: the first, in `sort_as` order, whose name
/// the word is a prefix of and whose level the character reaches. This is
/// the C's second search over `complete_cmd_info`, where the socials sit in
/// `sort_as` order.
pub fn find_social(content: &TbaContent, word: &str, level: i32) -> Option<SocialDef> {
    let mut matches: Vec<(&str, &SocialDef)> = content
        .socials
        .iter()
        .filter(|(name, def)| name.starts_with(word) && level >= def.min_level_char)
        .map(|(name, def)| {
            let sort = if def.sort_as.is_empty() {
                name.as_str()
            } else {
                def.sort_as.as_str()
            };
            (sort, def)
        })
        .collect();
    // `str_cmp` ordering, stable for equal keys (the catalog is by name).
    matches.sort_by(|a, b| str_cmp(a.0, b.0).cmp(&0));
    matches.first().map(|(_, def)| (*def).clone())
}

/// `do_action`.
pub fn do_action(host: &mut EcsHost<'_>, ch: Uid, action: &SocialDef, argument: &str) {
    let hide = action.hide;
    if argument.is_empty() {
        if let Some(line) = &action.char_no_arg {
            to_char(host, ch, &format!("{line}\r\n"));
        }
        if let Some(line) = &action.others_no_arg {
            act_full(
                host,
                line,
                hide,
                Some(ch),
                ActObj::None,
                ActVict::None,
                To::Room,
                Flags::default(),
            );
        }
        return;
    }

    let (_, part, _) = two_arguments(argument);
    if action.char_body_found.is_none() && !part.is_empty() {
        to_char(
            host,
            ch,
            "Sorry, this social does not support body parts.\r\n",
        );
        return;
    }
    let arg = if action.char_found.is_some() {
        one_argument(argument).0
    } else {
        String::new()
    };

    let Some(vict) = get_char_room_vis(host, ch, &arg) else {
        if action.char_obj_found.is_some() {
            // `do_action` looks in the actor's carrying list and, if that
            // fails, in the room's contents. The first lookup consumes the
            // `N.` prefix, so the second starts its count at 1 again.
            let mut arg = arg.clone();
            let target =
                super::item::pull_obj(host, ch, &mut arg, &host.carrying(ch)).or_else(|| {
                    let room = host.in_room(ch)?;
                    super::item::pull_obj(host, ch, &mut arg, &host.room_contents(room))
                });
            if let Some(target) = target {
                if let Some(line) = &action.char_obj_found {
                    act_full(
                        host,
                        line,
                        hide,
                        Some(ch),
                        ActObj::Obj(target),
                        ActVict::None,
                        To::Char,
                        Flags::default(),
                    );
                }
                if let Some(line) = &action.others_obj_found {
                    act_full(
                        host,
                        line,
                        hide,
                        Some(ch),
                        ActObj::Obj(target),
                        ActVict::None,
                        To::Room,
                        Flags::default(),
                    );
                }
                return;
            }
        }
        match &action.not_found {
            Some(line) => to_char(host, ch, &format!("{line}\r\n")),
            None => to_char(host, ch, "I don't see anything by that name here.\r\n"),
        }
        return;
    };

    if vict == ch {
        match &action.char_auto {
            Some(line) => to_char(host, ch, &format!("{line}\r\n")),
            None => to_char(host, ch, "Erm, no.\r\n"),
        }
        if let Some(line) = &action.others_auto {
            act_full(
                host,
                line,
                hide,
                Some(ch),
                ActObj::None,
                ActVict::None,
                To::Room,
                Flags::default(),
            );
        }
        return;
    }

    let sleeping_ok = Flags {
        sleep: true,
        no_trig: false,
    };
    if position_number(world::position(host, vict)) < action.min_victim_position {
        act_full(
            host,
            "$N is not in a proper position for that.",
            false,
            Some(ch),
            ActObj::None,
            ActVict::Char(vict),
            To::Char,
            sleeping_ok,
        );
        return;
    }
    let (to_actor, to_room, to_victim, obj) = if part.is_empty() {
        (
            &action.char_found,
            &action.others_found,
            &action.vict_found,
            ActObj::None,
        )
    } else {
        (
            &action.char_body_found,
            &action.others_body_found,
            &action.vict_body_found,
            ActObj::Text(&part),
        )
    };
    let lines = [
        (to_actor, false, To::Char, sleeping_ok),
        (to_room, hide, To::NotVict, Flags::default()),
        (to_victim, hide, To::Vict, Flags::default()),
    ];
    for (line, hide, to, flags) in lines {
        if let Some(line) = line {
            act_full(
                host,
                line,
                hide,
                Some(ch),
                obj,
                ActVict::Char(vict),
                to,
                flags,
            );
        }
    }
}
