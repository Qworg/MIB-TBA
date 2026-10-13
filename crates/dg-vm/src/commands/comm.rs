//! `dg_comm.c` (`sub_write`, `any_one_name`, `send_to_zone`) and the parts of
//! `comm.c` the script commands lean on (`send_to_range`, `act`,
//! `perform_act`).
//!
//! `sub_write` takes the room to work in as a parameter. The C reads
//! `IN_ROOM(ch)` and `do_masound` temporarily assigns it to a neighbouring
//! room without moving the character; passing the room reproduces that
//! without the host having to model a character that is "in" a room it is not
//! listed in.
//!
//! `act` is ported for what the script commands pass it: no object and no
//! victim, so every `$`-code that names one prints `<NULL>` as the C does.
//!
//! This tbaMUD version's `sub_write` does no capitalisation of its own; only
//! `act` (through `CAP`) capitalises, and only for a character with a
//! descriptor.

use super::shared::{PLR_WRITING, awake, has_desc};
use crate::cstr::is_space;
use crate::dispatch::act_mtrigger;
use crate::fields_char::CharStat;
use crate::host::DgHost;
use crate::lookup::{
    get_char_in_room, get_char_room_vis, get_obj_in_equip_vis, get_obj_in_list_vis, get_obj_in_room,
};
use crate::types::{LVL_IMMORT, Uid, trig::MTRIG_ACT};

/// `TO_ROOM` (`comm.h`): to everyone in the room except the character.
pub const TO_ROOM: u32 = 1;
/// `TO_CHAR`: to the character.
pub const TO_CHAR: u32 = 4;

/// `any_one_name` (`dg_comm.c`): like `any_one_arg`, but the word also ends
/// at punctuation (except `#` and `-`). Returns the lowercased word and the
/// rest of the string.
///
/// `}` is punctuation, so a name written as an id (`}1234`) is empty here;
/// the C behaves the same.
pub fn any_one_name(argument: &str) -> (String, &str) {
    let argument = argument.trim_start_matches(is_space);
    let end = argument
        .find(|c: char| is_space(c) || (c.is_ascii_punctuation() && c != '#' && c != '-'))
        .unwrap_or(argument.len());
    (argument[..end].to_ascii_lowercase(), &argument[end..])
}

/// What a `~`/`|`/`^`/`&`/`*`/`` ` `` marker resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Nobody,
    Id(Uid),
}

/// `SENDOK(ch)` (`utils.h`). `to_sleeping` is `sub_write`'s constant `1` and
/// `act`'s `type & TO_SLEEP`, which is `0` here.
pub(crate) fn sendok<H: DgHost + ?Sized>(host: &H, ch: Uid, to_sleeping: bool) -> bool {
    let reacts = host
        .script(ch)
        .is_some_and(|script| script.check(MTRIG_ACT));
    (has_desc(host, ch) || reacts)
        && (to_sleeping || awake(host, ch))
        && !(!host.is_npc(ch) && host.char_act_flagged(ch, PLR_WRITING))
}

/// `PERS(ch, vict)`: the name `viewer` sees `target` by.
pub(crate) fn pers<H: DgHost + ?Sized>(host: &H, target: Uid, viewer: Uid) -> String {
    if target == viewer || host.can_see(viewer, target) {
        host.name(target)
    } else if host.level(target) > LVL_IMMORT {
        "an immortal".to_owned()
    } else {
        "someone".to_owned()
    }
}

/// `HSHR`: his / her / its.
pub(crate) fn hshr(sex: i32) -> &'static str {
    match sex {
        0 => "its",
        1 => "his",
        _ => "her",
    }
}

/// `HSSH`: he / she / it.
pub(crate) fn hssh(sex: i32) -> &'static str {
    match sex {
        0 => "it",
        1 => "he",
        _ => "she",
    }
}

/// `HMHR`: him / her / it.
pub(crate) fn hmhr(sex: i32) -> &'static str {
    match sex {
        0 => "it",
        1 => "him",
        _ => "her",
    }
}

/// `sub_write_to_char`: expand the markers for one recipient and send.
fn sub_write_to_char<H: DgHost + ?Sized>(
    host: &mut H,
    ch: Uid,
    tokens: &[String],
    marks: &[(char, Target)],
) {
    let mut sb = String::new();
    for (token, (kind, target)) in tokens.iter().zip(marks) {
        sb.push_str(token);
        let id = match target {
            Target::Nobody => None,
            Target::Id(id) => Some(*id),
        };
        match kind {
            '~' => match id {
                None => sb.push_str("someone"),
                Some(id) if id == ch => sb.push_str("you"),
                Some(id) => sb.push_str(&pers(host, id, ch)),
            },
            '|' => match id {
                None => sb.push_str("someone's"),
                Some(id) if id == ch => sb.push_str("your"),
                Some(id) => {
                    sb.push_str(&pers(host, id, ch));
                    sb.push_str("'s");
                }
            },
            '^' => match id {
                Some(id) if id == ch => sb.push_str("your"),
                Some(id) if host.can_see(ch, id) => {
                    sb.push_str(hshr(host.char_stat(id, CharStat::Sex)))
                }
                _ => sb.push_str("its"),
            },
            '&' => match id {
                Some(id) if id == ch => sb.push_str("you"),
                Some(id) if host.can_see(ch, id) => {
                    sb.push_str(hssh(host.char_stat(id, CharStat::Sex)))
                }
                _ => sb.push_str("it"),
            },
            '*' => match id {
                Some(id) if id == ch => sb.push_str("you"),
                Some(id) if host.can_see(ch, id) => {
                    sb.push_str(hmhr(host.char_stat(id, CharStat::Sex)))
                }
                _ => sb.push_str("it"),
            },
            '`' => match id {
                None => sb.push_str("something"),
                Some(id) => {
                    if host.can_see(ch, id) {
                        sb.push_str(&host.name(id));
                    } else {
                        sb.push_str("something");
                    }
                }
            },
            _ => {}
        }
    }
    if let Some(last) = tokens.get(marks.len()) {
        sb.push_str(last);
    }
    sb.push_str("\n\r");
    host.cmd_send(ch, &sb);
}

/// `sub_write(arg, ch, find_invis, targets)`: send `arg`, with the name
/// markers expanded for each recipient, to `ch` (`TO_CHAR`) and/or everyone
/// else in `ch`'s room (`TO_ROOM`).
///
/// A character in no room (the C indexes `world[NOWHERE]`) resolves no names
/// and has no room to send to; `TO_CHAR` still works.
pub fn sub_write<H: DgHost + ?Sized>(
    host: &mut H,
    arg: &str,
    ch: Uid,
    find_invis: bool,
    targets: u32,
) {
    let room = host.in_room(ch);
    sub_write_in(host, arg, ch, room, find_invis, targets);
}

/// [`sub_write`] with the room given: the C's `IN_ROOM(ch)` as `do_masound`
/// has it set.
///
/// Deviation: the C holds at most `MAX_INPUT_LENGTH` markers and a
/// `2 * MAX_INPUT_LENGTH` byte text; neither is limited here.
pub(crate) fn sub_write_in<H: DgHost + ?Sized>(
    host: &mut H,
    arg: &str,
    ch: Uid,
    room: Option<Uid>,
    find_invis: bool,
    targets: u32,
) {
    let mut tokens: Vec<String> = vec![String::new()];
    let mut marks: Vec<(char, Target)> = Vec::new();
    let mut rest = arg;
    while let Some(c) = rest.chars().next() {
        match c {
            '~' | '|' | '^' | '&' | '*' => {
                let (name, after) = any_one_name(&rest[c.len_utf8()..]);
                rest = after;
                let found = room.and_then(|room| {
                    if find_invis {
                        get_char_in_room(host, room, &name)
                    } else {
                        get_char_room_vis(host, ch, &name)
                    }
                });
                marks.push((c, found.map_or(Target::Nobody, Target::Id)));
                tokens.push(String::new());
            }
            '`' => {
                let (name, after) = any_one_name(&rest[c.len_utf8()..]);
                rest = after;
                let found =
                    room.and_then(|room| find_marker_obj(host, ch, room, &name, find_invis));
                marks.push((c, found.map_or(Target::Nobody, Target::Id)));
                tokens.push(String::new());
            }
            '\\' => {
                // A backslash at the very end escapes nothing; the C steps
                // over it and stops (its comment explains the overrun it
                // would otherwise cause).
                rest = &rest[1..];
                if let Some(escaped) = rest.chars().next() {
                    if let Some(token) = tokens.last_mut() {
                        token.push(escaped);
                    }
                    rest = &rest[escaped.len_utf8()..];
                }
            }
            _ => {
                if let Some(token) = tokens.last_mut() {
                    token.push(c);
                }
                rest = &rest[c.len_utf8()..];
            }
        }
    }

    if targets & TO_CHAR != 0 && sendok(host, ch, true) {
        sub_write_to_char(host, ch, &tokens, &marks);
    }
    if targets & TO_ROOM != 0
        && let Some(room) = room
    {
        for to in host.room_people(room) {
            if to != ch && sendok(host, to, true) {
                sub_write_to_char(host, to, &tokens, &marks);
            }
        }
    }
}

/// The object lookup of a `` ` `` marker. With `find_invis` it is
/// `get_obj_in_room`. Without, the C chains the three lookups with
/// `else if (!(obj = …))`, so an object found in the room is looked up
/// *again* among the worn equipment and then among the inventory, each
/// overwriting the last: only a name that matches in all three survives, and
/// the answer is the carried one.
fn find_marker_obj<H: DgHost + ?Sized>(
    host: &H,
    ch: Uid,
    room: Uid,
    name: &str,
    find_invis: bool,
) -> Option<Uid> {
    if find_invis {
        return get_obj_in_room(host, room, name);
    }
    get_obj_in_list_vis(host, ch, name, &host.room_contents(room))?;
    get_obj_in_equip_vis(host, ch, ch, name)?;
    get_obj_in_list_vis(host, ch, name, &host.carrying(ch))
}

/// `send_to_zone(messg, zone)`: write to every playing, awake character whose
/// room is in `zone`.
pub fn send_to_zone<H: DgHost + ?Sized>(host: &mut H, messg: &str, zone: i32) {
    if messg.is_empty() {
        return;
    }
    for ch in host.cmd_playing_characters() {
        let in_zone = host
            .in_room(ch)
            .is_some_and(|room| host.room_zone_number(room) == zone);
        if awake(host, ch) && in_zone {
            host.cmd_send(ch, messg);
        }
    }
}

/// `send_to_range(start, finish, "%s\r\n", msg)` (`comm.c`): write `msg` to
/// every character with a descriptor in a room numbered `start..=finish`.
pub fn send_to_range<H: DgHost + ?Sized>(host: &mut H, start: i32, finish: i32, msg: &str) {
    // `room_vnum` is `ush_int`: the arguments wrap to 16 bits.
    let (start, finish) = (i32::from(start as u16), i32::from(finish as u16));
    if start > finish {
        host.cmd_syslog("send_to_range passed start room value greater then finish.");
        return;
    }
    let text = format!("{msg}\r\n");
    for room in host.cmd_rooms_in_range(start, finish) {
        for ch in host.room_people(room) {
            if has_desc(host, ch) {
                host.cmd_send(ch, &text);
            }
        }
    }
}

/// `CAP` (`utils.c`): uppercase the first real character, after skipping
/// leading colour codes (`\t` and the character after it) and ANSI
/// `ESC [ ... letter` sequences.
fn cap(text: &mut String) {
    let bytes = text.as_bytes();
    let at = |i: usize| bytes.get(i).copied().unwrap_or(0);
    let mut p = 0;
    while (at(p) == b'\t' && at(p + 1) != 0) || (at(p) == 0x1B && at(p + 1) == b'[') {
        if at(p) == b'\t' {
            p += 2;
        } else {
            p += 2;
            while at(p) != 0 && !at(p).is_ascii_alphabetic() {
                p += 1;
            }
            if at(p) != 0 {
                p += 1;
            }
        }
    }
    // Only an ASCII byte is replaced, so the index is a character boundary.
    if at(p).is_ascii_lowercase() {
        let upper = char::from(at(p).to_ascii_uppercase()).to_string();
        text.replace_range(p..=p, &upper);
    }
}

/// The text `perform_act` builds for `to`, without its line end. The object
/// and victim are always absent.
fn act_text<H: DgHost + ?Sized>(host: &mut H, orig: &str, ch: Uid, to: Uid) -> String {
    const NULL_ARG: &str = "<NULL>";
    let src = orig.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(src.len() + 8);
    let mut uppercase_next = false;
    let mut at = 0;
    while at < src.len() {
        let byte = src[at];
        if byte != b'$' {
            out.push(byte);
            at += 1;
            if uppercase_next && !is_space(char::from(byte)) {
                if let Some(last) = out.last_mut() {
                    *last = last.to_ascii_uppercase();
                }
                uppercase_next = false;
            }
            continue;
        }
        at += 1;
        let code = src.get(at).copied();
        let text: String = match code {
            Some(b'n') => pers(host, ch, to),
            Some(b'm') => hmhr(host.char_stat(ch, CharStat::Sex)).to_owned(),
            Some(b's') => hshr(host.char_stat(ch, CharStat::Sex)).to_owned(),
            Some(b'e') => hssh(host.char_stat(ch, CharStat::Sex)).to_owned(),
            Some(
                b'N' | b'M' | b'S' | b'E' | b'o' | b'O' | b'p' | b'P' | b'a' | b'A' | b'T' | b't'
                | b'F',
            ) => NULL_ARG.to_owned(),
            Some(b'u') => {
                let mut start = out.len();
                while start > 0 && !is_space(char::from(out[start - 1])) {
                    start -= 1;
                }
                if start != out.len() {
                    out[start] = out[start].to_ascii_uppercase();
                }
                String::new()
            }
            Some(b'U') => {
                uppercase_next = true;
                String::new()
            }
            Some(b'$') => "$".to_owned(),
            other => {
                // The C also reads past the terminator when the string ends
                // in `$`; here the scan simply ends.
                let tail = String::from_utf8_lossy(src.get(at..).unwrap_or_default()).into_owned();
                let shown = other.map_or_else(String::new, |b| char::from(b).to_string());
                host.cmd_syslog(&format!("SYSERR: Illegal $-code to act(): {shown}"));
                host.cmd_syslog(&format!("SYSERR: {tail}"));
                String::new()
            }
        };
        for &b in text.as_bytes() {
            let mut b = b;
            if uppercase_next && !is_space(char::from(b)) {
                b = b.to_ascii_uppercase();
                uppercase_next = false;
            }
            out.push(b);
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The buffer `perform_act` ends up with for `to`: the text, its line end,
/// and (`CAP`, in place) a capital first letter when `to` has a descriptor to
/// write it to.
fn act_line<H: DgHost + ?Sized>(host: &mut H, orig: &str, ch: Uid, to: Uid) -> String {
    let mut text = act_text(host, orig, ch, to);
    text.push_str("\r\n");
    if has_desc(host, to) {
        cap(&mut text);
    }
    text
}

/// `IS_NPC(to) && dg_act_check && to != ch`: whether `perform_act` offers the
/// line to `to`'s `act` triggers. `dg_act_check` is true for every `act()` a
/// script command makes.
fn offers_to_triggers<H: DgHost + ?Sized>(host: &H, ch: Uid, to: Uid) -> bool {
    host.is_npc(to) && to != ch
}

/// `perform_act(orig, ch, NULL, NULL, to)`.
fn perform_act<H: DgHost + ?Sized>(host: &mut H, orig: &str, ch: Uid, to: Uid) {
    let text = act_line(host, orig, ch, to);
    if has_desc(host, to) {
        host.cmd_send(to, &text);
    }
    if offers_to_triggers(host, ch, to) {
        // The victim, object, target and argument are all absent (`dg_victim`
        // is NULL because `vict_obj` is).
        act_mtrigger(host, to, &text, Some(ch), None, None, None, None);
    }
}

/// `act(str, hide_invisible, ch, NULL, NULL, type)` for `type` `TO_CHAR` or
/// `TO_ROOM`.
pub fn act<H: DgHost + ?Sized>(host: &mut H, text: &str, hide_invisible: bool, ch: Uid, kind: u32) {
    if text.is_empty() {
        return;
    }
    if kind == TO_CHAR {
        if sendok(host, ch, false) {
            perform_act(host, text, ch, ch);
        }
        return;
    }
    if kind != TO_ROOM {
        return;
    }
    let Some(room) = host.in_room(ch) else {
        host.cmd_syslog("SYSERR: no valid target to act()!");
        return;
    };
    for to in host.room_people(room) {
        if !sendok(host, to, false) || to == ch {
            continue;
        }
        if hide_invisible && !host.can_see(to, ch) {
            continue;
        }
        perform_act(host, text, ch, to);
    }
}

/// `act_to_room(str, room)` (`dg_wldcmd.c`): `act()` to everyone in a room
/// by way of its first occupant (`TO_ROOM`, then `TO_CHAR` for that occupant).
pub fn act_to_room<H: DgHost + ?Sized>(host: &mut H, text: &str, room: Uid) {
    let Some(first) = host.room_people(room).first().copied() else {
        return;
    };
    act(host, text, false, first, TO_ROOM);
    act(host, text, false, first, TO_CHAR);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::commands::support::World;
    use crate::host::BaseHost;
    use crate::types::AttachType;

    #[test]
    fn any_one_name_stops_at_space_and_punctuation_but_not_hash_or_dash() {
        assert_eq!(
            any_one_name("  Bob's sword"),
            ("bob".to_owned(), "'s sword")
        );
        assert_eq!(any_one_name("#3-x."), ("#3-x".to_owned(), "."));
        assert_eq!(any_one_name("\t \nTom\there"), ("tom".to_owned(), "\there"));
        assert_eq!(any_one_name(""), (String::new(), ""));
        assert_eq!(any_one_name("   "), (String::new(), ""));
        // `}` is punctuation, so an id is not a name here.
        assert_eq!(any_one_name("}1234 x"), (String::new(), "}1234 x"));
        assert_eq!(any_one_name("a_b"), ("a".to_owned(), "_b"));
        // Non-ASCII bytes are neither space nor punctuation.
        assert_eq!(
            any_one_name("caf\u{e9}s, x"),
            ("caf\u{e9}s".to_owned(), ", x")
        );
    }

    #[test]
    fn markers_become_you_or_the_name_the_recipient_sees() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        sub_write(&mut w.host, "Hello ~bob!", alice, true, TO_CHAR | TO_ROOM);
        assert_eq!(w.sent_to(alice), ["Hello Bob!\n\r"]);
        assert_eq!(w.sent_to(bob), ["Hello you!\n\r"]);
        // The guard has no descriptor: nothing is delivered to it.
        assert_eq!(w.host.effects.len(), 2);
        // Delivery order: the character first, then the room head first.
        assert!(w.host.effects[0].starts_with(&format!("send {alice}:")));
    }

    #[test]
    fn possessives_and_missing_names() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        sub_write(
            &mut w.host,
            "Take |bob sword, ~nobody |nobody",
            alice,
            true,
            TO_CHAR | TO_ROOM,
        );
        assert_eq!(
            w.sent_to(alice),
            ["Take Bob's sword, someone someone's\n\r"]
        );
        assert_eq!(w.sent_to(bob), ["Take your sword, someone someone's\n\r"]);
        // A space after the marker is skipped before the name is read.
        w.clear();
        sub_write(&mut w.host, "hi ~ bob.", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["hi Bob.\n\r"]);
        // A marker with nothing after it names nobody.
        w.clear();
        sub_write(&mut w.host, "x ~", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["x someone\n\r"]);
        // An id is cut off at the `}` (punctuation) and so names nobody.
        w.clear();
        let id = crate::types::uid_text(bob);
        sub_write(&mut w.host, &format!("x ~{id}"), alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["x someone}2\n\r"]);
    }

    #[test]
    fn pronoun_markers_follow_sex_and_visibility() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        w.host.set_num(bob, "sex", 1);
        sub_write(
            &mut w.host,
            "^bob &bob *bob",
            alice,
            true,
            TO_CHAR | TO_ROOM,
        );
        assert_eq!(w.sent_to(alice), ["his he him\n\r"]);
        assert_eq!(w.sent_to(bob), ["your you you\n\r"]);
        w.clear();
        w.host.set_num(bob, "sex", 2);
        sub_write(&mut w.host, "^bob &bob *bob", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["her she her\n\r"]);
        w.clear();
        w.host.set_num(bob, "sex", 0);
        sub_write(&mut w.host, "^bob &bob *bob", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["its it it\n\r"]);
        // Unseen or unknown targets read "its", "it", "it"; a name needs PERS.
        w.clear();
        w.host.set_num(bob, "sex", 1);
        w.host
            .entity_mut(alice)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![bob]);
        sub_write(
            &mut w.host,
            "^bob &bob *bob ~bob ^zed",
            alice,
            true,
            TO_CHAR,
        );
        assert_eq!(w.sent_to(alice), ["its it it someone its\n\r"]);
        // Above LVL_IMMORT the unseen are "an immortal".
        w.clear();
        w.host.set_num(bob, "level", 32);
        sub_write(&mut w.host, "~bob |bob", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["an immortal an immortal's\n\r"]);
        w.clear();
        w.host.set_num(bob, "level", 31);
        sub_write(&mut w.host, "~bob", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["someone\n\r"]);
    }

    #[test]
    fn object_markers_and_the_find_invis_cascade() {
        let mut w = World::new();
        let alice = w.player;
        sub_write(&mut w.host, "See `lamp and `nothing.", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["See a lamp and something.\n\r"]);

        // Unseen objects are "something".
        w.clear();
        w.host
            .entity_mut(alice)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![w.obj]);
        sub_write(&mut w.host, "`lamp", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["something\n\r"]);

        // Without find_invis the room lookup, the worn lookup and the carried
        // lookup chain: the name must match in all three, the last answers.
        w.clear();
        w.host.entity_mut(alice).unwrap().lists.clear();
        sub_write(&mut w.host, "`lamp", alice, false, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["something\n\r"]);
        let worn = w.host.add_obj(1, "a worn lamp");
        w.host.set_text(worn, "alias", "lamp");
        w.host.equip_char(alice, worn, 16);
        let carried = w.host.add_obj(2, "a carried lamp");
        w.host.set_text(carried, "alias", "lamp");
        w.host.obj_to_char(carried, alice);
        w.clear();
        sub_write(&mut w.host, "`lamp", alice, false, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["a carried lamp\n\r"]);
    }

    #[test]
    fn without_find_invis_characters_must_be_visible() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        w.host
            .entity_mut(alice)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![bob]);
        // Bob is not found at all, so he is told "someone", not "you".
        sub_write(&mut w.host, "~bob", alice, false, TO_ROOM);
        assert_eq!(w.sent_to(bob), ["someone\n\r"]);
        w.clear();
        sub_write(&mut w.host, "~bob", alice, true, TO_ROOM);
        assert_eq!(w.sent_to(bob), ["you\n\r"]);
    }

    #[test]
    fn backslash_escapes_and_trailing_backslash() {
        let mut w = World::new();
        let alice = w.player;
        sub_write(&mut w.host, r"a\~b \\ c\", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["a~b \\ c\n\r"]);
        w.clear();
        sub_write(&mut w.host, "\\", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["\n\r"]);
        w.clear();
        sub_write(&mut w.host, "", alice, true, TO_CHAR);
        assert_eq!(w.sent_to(alice), ["\n\r"]);
        // A multi-byte character after a backslash and plain multi-byte text.
        w.clear();
        sub_write(
            &mut w.host,
            "\\\u{e9}\u{e9} ~bob \u{1f600}",
            alice,
            true,
            TO_CHAR,
        );
        assert_eq!(w.sent_to(alice), ["\u{e9}\u{e9} Bob \u{1f600}\n\r"]);
    }

    #[test]
    fn sendok_needs_a_descriptor_or_act_trigger_and_not_writing() {
        let mut w = World::new();
        let (alice, bob, mob) = (w.player, w.player2, w.mob);
        w.set_writing(bob, true);
        sub_write(&mut w.host, "x", alice, true, TO_CHAR | TO_ROOM);
        assert_eq!(w.sent_to(alice).len(), 1);
        assert!(w.sent_to(bob).is_empty());
        // Sleeping characters are still sent to (to_sleeping is 1).
        w.clear();
        w.set_writing(bob, false);
        w.host.set_num(bob, "pos", 4);
        sub_write(&mut w.host, "x", alice, true, TO_ROOM);
        assert_eq!(w.sent_to(bob).len(), 1);
        // A mob with an act trigger passes SENDOK but has no descriptor to
        // write to, and sub_write offers it nothing.
        let act_proto = w
            .host
            .add_proto(901, AttachType::Mob, "e", 100, "", "say hi");
        w.host.attach(mob, &act_proto);
        assert!(sendok(&w.host, mob, true));
        w.clear();
        sub_write(&mut w.host, "x", alice, true, TO_ROOM);
        assert!(w.host.mud_commands.is_empty());
        // A character in no room has nobody to send to and resolves no names.
        w.clear();
        w.host.char_from_room(alice);
        sub_write(&mut w.host, "~bob", alice, true, TO_CHAR | TO_ROOM);
        assert_eq!(w.sent_to(alice), ["someone\n\r"]);
        assert!(w.sent_to(bob).is_empty());
    }

    #[test]
    fn sub_write_in_uses_the_given_room() {
        let mut w = World::new();
        let (alice, bob, room2) = (w.player, w.player2, w.room2);
        w.host.char_to_room(bob, room2);
        sub_write_in(&mut w.host, "hi ~bob", alice, Some(room2), true, TO_ROOM);
        assert_eq!(w.sent_to(bob), ["hi you\n\r"]);
        assert!(w.sent_to(alice).is_empty());
    }

    #[test]
    fn act_expands_dollar_codes_per_recipient() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        w.host.set_num(bob, "sex", 1);
        act(
            &mut w.host,
            "$n waves $s hand. $e smiles at $m. $$5 $N$o",
            false,
            bob,
            TO_ROOM,
        );
        assert_eq!(
            w.sent_to(alice),
            ["Bob waves his hand. he smiles at him. $5 <NULL><NULL>\r\n"]
        );
        // Not to the guard (no descriptor), nor the actor.
        assert_eq!(w.host.effects.len(), 1);
        // $n is "someone" to a recipient who cannot see the actor.
        w.clear();
        w.host
            .entity_mut(alice)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![bob]);
        act(&mut w.host, "$n leaves.", false, bob, TO_ROOM);
        assert_eq!(w.sent_to(alice), ["Someone leaves.\r\n"]);
        // hide_invisible drops that recipient altogether.
        w.clear();
        act(&mut w.host, "$n leaves.", true, bob, TO_ROOM);
        assert!(w.sent_to(alice).is_empty());
        // TO_CHAR goes to the actor; PERS of oneself is the name.
        act(&mut w.host, "you are $n.", false, bob, TO_CHAR);
        assert_eq!(w.sent_to(bob), ["You are Bob.\r\n"]);
    }

    #[test]
    fn act_case_codes_and_bad_codes() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        act(&mut w.host, "ab cd$u ef$Ugh ij $U kl", false, bob, TO_ROOM);
        // $u uppercases the previous word's first letter; $U the next
        // non-space character, however far on.
        assert_eq!(w.sent_to(alice), ["Ab Cd efGh ij  Kl\r\n"]);

        w.clear();
        act(&mut w.host, "a$zb", false, bob, TO_ROOM);
        assert_eq!(w.sent_to(alice), ["Ab\r\n"]);
        assert_eq!(
            w.effects("syslog"),
            [
                "syslog: SYSERR: Illegal $-code to act(): z",
                "syslog: SYSERR: zb"
            ]
        );
        // A `$` at the very end is an illegal code too, with nothing to show.
        w.clear();
        act(&mut w.host, "end$", false, bob, TO_ROOM);
        assert_eq!(w.sent_to(alice), ["End\r\n"]);
        assert_eq!(
            w.effects("syslog"),
            [
                "syslog: SYSERR: Illegal $-code to act(): ",
                "syslog: SYSERR: "
            ]
        );
        // An empty string sends nothing; neither does an unknown kind.
        w.clear();
        act(&mut w.host, "", false, bob, TO_ROOM);
        act(&mut w.host, "x", false, bob, 99);
        assert!(w.host.effects.is_empty());
        // Non-ASCII text survives untouched and is not capitalised.
        act(&mut w.host, "\u{e9}t\u{e9} $n", false, bob, TO_ROOM);
        assert_eq!(w.sent_to(alice), ["\u{e9}t\u{e9} Bob\r\n"]);
    }

    #[test]
    fn act_skips_sleepers_and_a_character_in_no_room_has_nobody_to_act_to() {
        let mut w = World::new();
        let (alice, bob) = (w.player, w.player2);
        w.host.set_num(alice, "pos", 4);
        act(&mut w.host, "waves at $n.", false, bob, TO_ROOM);
        assert!(w.sent_to(alice).is_empty());
        w.clear();
        w.host.char_from_room(bob);
        act(&mut w.host, "x", false, bob, TO_ROOM);
        assert_eq!(
            w.effects("syslog"),
            ["syslog: SYSERR: no valid target to act()!"]
        );
    }

    #[test]
    fn the_line_a_trigger_is_offered_is_capitalised_only_for_a_descriptor() {
        let mut w = World::new();
        let (alice, bob, mob) = (w.player, w.player2, w.mob);
        // CAP works on the buffer in place before `write_to_output`, so a
        // character with a descriptor gets, and a trigger would see, a
        // capital; a mob with no descriptor leaves the buffer as written.
        assert_eq!(
            act_line(&mut w.host, "waves at $n.", bob, mob),
            "waves at Bob.\r\n"
        );
        assert_eq!(
            act_line(&mut w.host, "waves at $n.", bob, alice),
            "Waves at Bob.\r\n"
        );
        // A switched mob has a descriptor.
        w.host.set_num(mob, "desc", 1);
        assert_eq!(
            act_line(&mut w.host, "waves at $n.", bob, mob),
            "Waves at Bob.\r\n"
        );
    }

    #[test]
    fn only_an_npc_other_than_the_actor_is_offered_the_line() {
        let mut w = World::new();
        let (alice, bob, mob) = (w.player, w.player2, w.mob);
        let rat = w.host.add_mob(3001, "a rat");
        assert!(offers_to_triggers(&w.host, bob, mob));
        assert!(offers_to_triggers(&w.host, mob, rat));
        // Players are never offered it, and an NPC is not offered its own
        // TO_CHAR line.
        assert!(!offers_to_triggers(&w.host, bob, alice));
        assert!(!offers_to_triggers(&w.host, mob, mob));
        assert!(!offers_to_triggers(&w.host, alice, alice));
    }

    #[test]
    fn act_offers_each_npc_in_the_room_its_line_through_the_dispatch_module() {
        // `act_mtrigger` runs the `act` triggers whose argument matches. A
        // trigger that says what it was shown makes the call visible; until
        // the dispatch module fires triggers, nothing is said and the test
        // asserts only that nothing else was.
        let mut w = World::new();
        let (bob, mob) = (w.player2, w.mob);
        let act_proto = w
            .host
            .add_proto(901, AttachType::Mob, "e", 0, "waves", "say saw %arg%");
        w.host.attach(mob, &act_proto);
        act(&mut w.host, "waves at $n.", false, bob, TO_ROOM);
        let said: Vec<_> = w.host.mud_commands.iter().map(|(who, _)| *who).collect();
        assert!(said.iter().all(|who| *who == mob), "{said:?}");
        for (_, line) in &w.host.mud_commands {
            assert_eq!(line, "say saw waves at Bob.");
        }
    }

    #[test]
    fn act_to_room_goes_through_the_first_occupant() {
        let mut w = World::new();
        let (alice, bob, room, room2) = (w.player, w.player2, w.room, w.room2);
        // The room's head is Bob: TO_ROOM reaches Alice, then TO_CHAR Bob.
        act_to_room(&mut w.host, "the ground trembles.", room);
        assert_eq!(w.host.effects.len(), 2);
        assert_eq!(
            w.host.effects[0],
            format!("send {alice}: The ground trembles.\r\n")
        );
        assert_eq!(
            w.host.effects[1],
            format!("send {bob}: The ground trembles.\r\n")
        );
        // An empty room does nothing.
        w.clear();
        act_to_room(&mut w.host, "x", room2);
        assert!(w.host.effects.is_empty());
    }

    #[test]
    fn send_to_zone_reaches_awake_players_in_the_zone() {
        let mut w = World::new();
        let (alice, bob, room, room2) = (w.player, w.player2, w.room, w.room2);
        w.host.set_num(room, "zone", 5);
        w.host.set_num(room2, "zone", 6);
        w.host.char_to_room(bob, room2);
        send_to_zone(&mut w.host, "Thunder\r\n", 5);
        assert_eq!(w.sent_to(alice), ["Thunder\r\n"]);
        assert!(w.sent_to(bob).is_empty());
        // Asleep: skipped. Empty message: skipped.
        w.clear();
        w.host.set_num(alice, "pos", 4);
        send_to_zone(&mut w.host, "Thunder\r\n", 5);
        w.host.set_num(alice, "pos", 8);
        send_to_zone(&mut w.host, "", 5);
        assert!(w.host.effects.is_empty());
        // Not playing: skipped. In no room: skipped.
        w.host.set_num(alice, "con_state", 1);
        send_to_zone(&mut w.host, "Thunder\r\n", 5);
        w.host.set_num(alice, "con_state", 0);
        w.host.char_from_room(alice);
        send_to_zone(&mut w.host, "Thunder\r\n", 5);
        assert!(w.host.effects.is_empty());
    }

    #[test]
    fn send_to_range_reaches_every_descriptor_in_the_rooms() {
        let mut w = World::new();
        let (alice, bob, room2) = (w.player, w.player2, w.room2);
        w.host.char_to_room(bob, room2);
        send_to_range(&mut w.host, 3001, 3002, "Rumble");
        assert_eq!(w.sent_to(alice), ["Rumble\r\n"]);
        assert_eq!(w.sent_to(bob), ["Rumble\r\n"]);
        // The guard has no descriptor.
        assert_eq!(w.host.effects.len(), 2);
        w.clear();
        send_to_range(&mut w.host, 3002, 3002, "Rumble");
        assert!(w.sent_to(alice).is_empty());
        assert_eq!(w.sent_to(bob).len(), 1);
        w.clear();
        send_to_range(&mut w.host, 3002, 3001, "Rumble");
        assert_eq!(
            w.host.effects,
            ["syslog: send_to_range passed start room value greater then finish."]
        );
        let _ = w.host.pulse();
    }

    #[test]
    fn cap_skips_colour_codes_before_uppercasing() {
        for (input, want) in [
            ("hello", "Hello"),
            ("\tythe ground.\tn", "\tyThe ground.\tn"),
            ("\x1B[0;31mred", "\x1B[0;31mRed"),
            ("\t", "\t"),
            ("\x1B[", "\x1B["),
            ("\x1B[31", "\x1B[31"),
            ("\t\u{e9}a", "\t\u{e9}a"),
            ("\u{e9}a", "\u{e9}a"),
            ("", ""),
            ("1abc", "1abc"),
        ] {
            let mut text = input.to_owned();
            cap(&mut text);
            assert_eq!(text, want, "{input:?}");
        }
    }

    #[test]
    fn send_to_range_wraps_its_arguments_to_sixteen_bits() {
        let mut w = World::new();
        let alice = w.player;
        // -1 is 65535, greater than 3001.
        send_to_range(&mut w.host, -1, 3001, "hi");
        assert!(w.sent_to(alice).is_empty());
        assert_eq!(w.effects("syslog").len(), 1);
        // 65536 + 3001 is 3001.
        send_to_range(&mut w.host, 65536 + 3001, 3001, "yo");
        assert_eq!(w.sent_to(alice), ["yo\r\n"]);
    }
}
