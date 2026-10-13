//! `act()`: the C's message formatter, which tells the room what a character
//! did and, on the way, offers each NPC that sees the line to its `act`
//! triggers (`comm.c`: `perform_act`, `act`).
//!
//! Only players receive text (an NPC has no session), so a recipient that is a
//! playing player gets the formatted line through [`crate::dg::out`], and an
//! NPC gets it through `act_mtrigger`. The pack models no sex, so the
//! pronoun codes (`$e`, `$m`, `$s` and their upper-case twins) always render
//! the neutral singular `they`, `them`, `their`, as the pack's social
//! messages do.

use dg_vm::Uid;
use dg_vm::dispatch::act_mtrigger;
use dg_vm::types::trig::MTRIG_ACT;

use super::world;
use crate::dg::host::EcsHost;
use crate::dg::out;

/// Who an `act()` line goes to (`TO_CHAR`, `TO_VICT`, `TO_NOTVICT`, `TO_ROOM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum To {
    /// The actor.
    Char,
    /// The victim.
    Vict,
    /// The room, less the actor and the victim.
    NotVict,
    /// The room, less the actor.
    Room,
}

/// The object slot of `act()`: an object (`$o`, `$p`, `$a`) or a string (`$t`).
#[derive(Debug, Clone, Copy)]
pub enum ActObj<'a> {
    None,
    Obj(Uid),
    Text(&'a str),
}

/// The victim slot of `act()`: a character (`$N`, `$E`, `$M`, `$S`), an
/// object (`$O`, `$P`, `$A`) or a string (`$T`, `$F`).
#[derive(Debug, Clone, Copy)]
pub enum ActVict<'a> {
    None,
    Char(Uid),
    Obj(Uid),
    Text(&'a str),
}

/// The flags `act()` takes in its `type` argument beside the target.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    /// `TO_SLEEP`: deliver to sleeping recipients too.
    pub sleep: bool,
    /// `DG_NO_TRIG`: do not offer the line to NPC `act` triggers.
    pub no_trig: bool,
}

/// The most recipients one walk of a room visits.
const WALK_LIMIT: usize = 100_000;

/// `act(text, hide_invisible, ch, NULL, NULL, to)`.
pub fn act(host: &mut EcsHost<'_>, text: &str, hide: bool, ch: Uid, to: To) {
    act_full(
        host,
        text,
        hide,
        Some(ch),
        ActObj::None,
        ActVict::None,
        to,
        Flags::default(),
    );
}

/// `act(text, hide_invisible, ch, obj, NULL, to)`.
pub fn act_obj(host: &mut EcsHost<'_>, text: &str, hide: bool, ch: Uid, obj: Uid, to: To) {
    act_full(
        host,
        text,
        hide,
        Some(ch),
        ActObj::Obj(obj),
        ActVict::None,
        to,
        Flags::default(),
    );
}

/// `act(text, hide_invisible, ch, NULL, vict, to)` with a character victim.
pub fn act_vict(host: &mut EcsHost<'_>, text: &str, hide: bool, ch: Uid, vict: Uid, to: To) {
    act_full(
        host,
        text,
        hide,
        Some(ch),
        ActObj::None,
        ActVict::Char(vict),
        to,
        Flags::default(),
    );
}

/// `act(text, hide_invisible, ch, obj, vict, to)` with an object and a
/// character victim.
#[allow(clippy::too_many_arguments)]
pub fn act_obj_vict(
    host: &mut EcsHost<'_>,
    text: &str,
    hide: bool,
    ch: Uid,
    obj: Uid,
    vict: Uid,
    to: To,
) {
    act_full(
        host,
        text,
        hide,
        Some(ch),
        ActObj::Obj(obj),
        ActVict::Char(vict),
        to,
        Flags::default(),
    );
}

/// `act(text, hide_invisible, ch, obj, cont, to)` with an object victim (the
/// `$P` container of `put` and `get`).
#[allow(clippy::too_many_arguments)]
pub fn act_obj_obj(
    host: &mut EcsHost<'_>,
    text: &str,
    hide: bool,
    ch: Uid,
    obj: Uid,
    other: Uid,
    to: To,
) {
    act_full(
        host,
        text,
        hide,
        Some(ch),
        ActObj::Obj(obj),
        ActVict::Obj(other),
        to,
        Flags::default(),
    );
}

/// `act()` in full. An empty text does nothing (the C returns before it
/// touches `dg_act_check`).
#[allow(clippy::too_many_arguments)]
pub fn act_full(
    host: &mut EcsHost<'_>,
    text: &str,
    hide: bool,
    ch: Option<Uid>,
    obj: ActObj<'_>,
    vict: ActVict<'_>,
    to: To,
    flags: Flags,
) {
    use dg_vm::BaseHost;
    if text.is_empty() {
        return;
    }
    // `dg_act_check = !IS_SET(type, DG_NO_TRIG)`, shared with the VM's own
    // `act()` so a trigger that says something silences the rest of the walk.
    host.vm_state().act_check = !flags.no_trig;
    match to {
        To::Char => {
            if let Some(ch) = ch
                && sendok(host, ch, flags.sleep)
            {
                perform_act(host, text, Some(ch), obj, vict, ch);
            }
        }
        To::Vict => {
            if let ActVict::Char(target) = vict
                && sendok(host, target, flags.sleep)
            {
                perform_act(host, text, ch, obj, vict, target);
            }
        }
        To::NotVict | To::Room => {
            let room = ch
                .and_then(|ch| dg_vm::WorldHost::in_room(host, ch))
                .or_else(|| match obj {
                    ActObj::Obj(obj) => dg_vm::WorldHost::in_room(host, obj),
                    _ => None,
                });
            let Some(room) = room else {
                host.script_log("SYSERR: no valid target to act()!");
                return;
            };
            let mut next = dg_vm::WorldHost::room_people(host, room).first().copied();
            let mut steps = 0_usize;
            while let Some(recipient) = next {
                steps += 1;
                if steps > WALK_LIMIT {
                    break;
                }
                let is_vict = matches!(vict, ActVict::Char(v) if v == recipient);
                let skip = !sendok(host, recipient, flags.sleep)
                    || Some(recipient) == ch
                    || (hide
                        && ch.is_some_and(|ch| !dg_vm::WorldHost::can_see(host, recipient, ch)))
                    || (to != To::Room && is_vict);
                if !skip {
                    perform_act(host, text, ch, obj, vict, recipient);
                }
                next = next_in_room(host, recipient);
            }
        }
    }
}

/// `to->next_in_room`, read from the room the recipient is in now.
fn next_in_room(host: &EcsHost<'_>, who: Uid) -> Option<Uid> {
    use dg_vm::WorldHost;
    let people = host.room_people(host.in_room(who)?);
    let at = people.iter().position(|person| *person == who)?;
    people.get(at + 1).copied()
}

/// `SENDOK(ch)`: a player with a session, or an NPC with an `act` trigger,
/// that is awake (or the line goes to sleepers too).
fn sendok(host: &EcsHost<'_>, who: Uid, to_sleeping: bool) -> bool {
    use dg_vm::BaseHost;
    let Some(entity) = host.entity(who) else {
        return false;
    };
    let has_desc = out::is_playing(host.world, entity);
    let acts = host
        .script(who)
        .is_some_and(|script| script.check(MTRIG_ACT));
    (has_desc || acts) && (to_sleeping || world::awake(host, who))
}

/// `perform_act`: render the line for `to`, deliver it, and offer it to `to`'s
/// `act` triggers.
fn perform_act(
    host: &mut EcsHost<'_>,
    orig: &str,
    ch: Option<Uid>,
    obj: ActObj<'_>,
    vict: ActVict<'_>,
    to: Uid,
) {
    use dg_vm::{BaseHost, WorldHost};
    let mut dg_victim = match vict {
        ActVict::Char(v) if v == to => Some(v),
        _ => None,
    };
    let mut dg_target = None;
    let mut dg_arg: Option<String> = None;
    let mut text = String::with_capacity(orig.len() + 16);
    let mut uppercase_next = false;
    let mut chars = orig.chars();
    while let Some(c) = chars.next() {
        if c != '$' {
            if uppercase_next && !c.is_whitespace() {
                text.extend(c.to_uppercase());
                uppercase_next = false;
            } else {
                text.push(c);
            }
            continue;
        }
        let Some(code) = chars.next() else {
            // The C reads past the end of a string that ends in `$`; the
            // line simply ends here.
            break;
        };
        let piece: String = match code {
            'n' => ch.map_or_else(String::new, |ch| pers(host, ch, to)),
            'N' => match vict {
                ActVict::Char(v) => {
                    dg_victim = Some(v);
                    pers(host, v, to)
                }
                _ => NULL_ARG.to_owned(),
            },
            'm' => "them".to_owned(),
            's' => "their".to_owned(),
            'e' => "they".to_owned(),
            'M' | 'S' | 'E' => match vict {
                ActVict::Char(v) => {
                    dg_victim = Some(v);
                    match code {
                        'M' => "them",
                        'S' => "their",
                        _ => "they",
                    }
                    .to_owned()
                }
                _ => NULL_ARG.to_owned(),
            },
            'o' => match obj {
                ActObj::Obj(o) => objn(host, o),
                _ => NULL_ARG.to_owned(),
            },
            'O' => match vict {
                ActVict::Obj(o) => {
                    dg_target = Some(o);
                    objn(host, o)
                }
                _ => NULL_ARG.to_owned(),
            },
            'p' => match obj {
                ActObj::Obj(o) => host.name(o),
                _ => NULL_ARG.to_owned(),
            },
            'P' => match vict {
                ActVict::Obj(o) => {
                    dg_target = Some(o);
                    host.name(o)
                }
                _ => NULL_ARG.to_owned(),
            },
            'a' => match obj {
                ActObj::Obj(o) => sana(host, o),
                _ => NULL_ARG.to_owned(),
            },
            'A' => match vict {
                ActVict::Obj(o) => {
                    dg_target = Some(o);
                    sana(host, o)
                }
                _ => NULL_ARG.to_owned(),
            },
            'T' => match vict {
                ActVict::Text(t) => {
                    dg_arg = Some(t.to_owned());
                    t.to_owned()
                }
                _ => NULL_ARG.to_owned(),
            },
            't' => match obj {
                ActObj::Text(t) => t.to_owned(),
                _ => NULL_ARG.to_owned(),
            },
            'F' => match vict {
                ActVict::Text(t) => fname(t).to_owned(),
                _ => NULL_ARG.to_owned(),
            },
            'u' => {
                uppercase_last_word(&mut text);
                String::new()
            }
            'U' => {
                uppercase_next = true;
                String::new()
            }
            '$' => "$".to_owned(),
            other => {
                host.script_log(&format!("SYSERR: Illegal $-code to act(): {other}"));
                String::new()
            }
        };
        for c in piece.chars() {
            if uppercase_next && !c.is_whitespace() {
                text.extend(c.to_uppercase());
                uppercase_next = false;
            } else {
                text.push(c);
            }
        }
    }
    text.push_str("\r\n");

    let entity = host.entity(to);
    let has_desc = entity.is_some_and(|entity| out::is_playing(host.world, entity));
    if has_desc {
        if let Some(entity) = entity {
            out::send_to_char(host.world, entity, &cap(&text));
        }
    } else if host.is_npc(to) && host.vm_state().act_check && Some(to) != ch {
        let obj_uid = match obj {
            ActObj::Obj(o) => Some(o),
            _ => None,
        };
        act_mtrigger(
            host,
            to,
            &text,
            ch,
            dg_victim,
            obj_uid,
            dg_target,
            dg_arg.as_deref(),
        );
    }
}

/// What the C prints for an absent argument (`ACTNULL`).
const NULL_ARG: &str = "<NULL>";

/// `PERS(ch, to)`: the name, or `someone` when `to` cannot see `ch`.
pub fn pers(host: &EcsHost<'_>, ch: Uid, to: Uid) -> String {
    use dg_vm::WorldHost;
    if host.can_see(to, ch) {
        host.name(ch)
    } else {
        "someone".to_owned()
    }
}

/// `OBJN(obj, to)`: the first keyword of the object. Objects are always
/// visible in the pack.
fn objn(host: &EcsHost<'_>, obj: Uid) -> String {
    use dg_vm::WorldHost;
    fname(&host.keywords(obj)).to_owned()
}

/// `SANA(obj)`: `an` before a vowel, else `a`, by the first keyword.
fn sana(host: &EcsHost<'_>, obj: Uid) -> String {
    use dg_vm::WorldHost;
    let keywords = host.keywords(obj);
    an(&keywords).to_owned()
}

/// `AN(string)`.
pub fn an(word: &str) -> &'static str {
    match word.chars().next() {
        Some(c) if "aeiouAEIOU".contains(c) => "an",
        _ => "a",
    }
}

/// `fname(namelist)`: the first word of a keyword list.
pub fn fname(namelist: &str) -> &str {
    let namelist = namelist.trim_start();
    let end = namelist
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(namelist.len());
    namelist.get(..end).unwrap_or("")
}

/// `CAP`: upper-case the first letter of the line.
pub fn cap(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
        }
        _ => text.to_owned(),
    }
}

/// `$u`: upper-case the first letter of the word just written.
fn uppercase_last_word(text: &mut String) {
    let start = text.rfind(char::is_whitespace).map_or(0, |at| {
        at + text
            .get(at..)
            .and_then(|tail| tail.chars().next())
            .map_or(1, char::len_utf8)
    });
    if let Some(rest) = text.get(start..)
        && let Some(first) = rest.chars().next()
        && first.is_ascii_lowercase()
    {
        let upper = first.to_ascii_uppercase();
        let tail = rest.get(first.len_utf8()..).unwrap_or("").to_owned();
        text.truncate(start);
        text.push(upper);
        text.push_str(&tail);
    }
}
