//! The variables each trigger type receives from the C dispatcher
//! (`dg_triggers.c`), so a trigger can be fired without dispatch.

use dg_vm::types::{AttachType, trig::asciiflag_conv};

use super::corpus::Record;

/// First whitespace-delimited word of `text`, or `fallback` when the text is
/// empty or a wildcard.
fn first_word<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    match text.split_whitespace().next() {
        Some(word) if word != "*" => word,
        _ => fallback,
    }
}

/// Every event variable the trigger's types could receive, with plausible
/// values. `actor`, `victim`, `object`, `target` are entity references
/// supplied by the caller (already in `}id` form).
pub struct Cast {
    pub actor: String,
    pub victim: String,
    pub object: String,
    pub target: String,
}

/// The variable set for a record, by attach type and type letters.
///
/// Per type this follows the `add_var`/`ADD_UID_VAR` calls of the matching
/// `*_trigger` function in `dg_triggers.c`.
pub fn event_vars(record: &Record, owner_kind: AttachType, cast: &Cast) -> Vec<(String, String)> {
    let bits = asciiflag_conv(&record.flags);
    let has = |bit: u32| bits & (1 << bit) != 0;
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut put = |name: &str, value: &str| vars.push((name.to_owned(), value.to_owned()));

    let command = first_word(&record.arglist, "look").to_owned();
    match owner_kind {
        AttachType::Mob => {
            if has(2) {
                put("actor", &cast.actor);
                put("arg", "sword");
                put("cmd", &command);
            }
            if has(3) {
                put("actor", &cast.actor);
                put("speech", first_word(&record.arglist, "hello"));
            }
            if has(4) {
                put("actor", &cast.actor);
                put("victim", &cast.victim);
                put("object", &cast.object);
                put("target", &cast.target);
                put("arg", &record.arglist);
            }
            // death, greet, greet-all, entry, receive, fight, hitprcnt,
            // memory all hand over the actor.
            for bit in [5, 6, 7, 8, 9, 10, 11, 14] {
                if has(bit) {
                    put("actor", &cast.actor);
                }
            }
            if has(9) {
                put("object", &cast.object);
            }
            if has(6) || has(7) || has(8) || has(16) || has(17) {
                put("direction", "north");
            }
            if has(12) {
                put("actor", &cast.actor);
                put("amount", &record.narg.max(100).to_string());
            }
            if has(15) {
                put("actor", &cast.actor);
                put("spell", "32");
                put("spellname", "magic missile");
            }
            if has(16) {
                put("actor", &cast.actor);
            }
            if has(17) {
                put("actor", &cast.actor);
                put("cmd", "open");
            }
            if has(18) {
                put("actor", &cast.actor);
                put("victim", &cast.victim);
                put("damdealt", "7");
                put("attacktype", "hit");
            }
            if has(19) {
                put("time", &record.narg.to_string());
            }
        }
        AttachType::Obj => {
            if has(2) {
                put("actor", &cast.actor);
                put("arg", "sword");
                put("cmd", &command);
            }
            // get, drop, wear, remove
            for bit in [6, 7, 9, 11] {
                if has(bit) {
                    put("actor", &cast.actor);
                }
            }
            if has(8) {
                put("actor", &cast.actor);
                put("victim", &cast.victim);
            }
            if has(15) {
                put("actor", &cast.actor);
                put("spell", "32");
                put("spellname", "magic missile");
            }
            if has(16) {
                put("actor", &cast.actor);
                put("direction", "north");
            }
            if has(18) {
                put("actor", &cast.actor);
                put("command", "eat");
            }
            if has(19) {
                put("time", &record.narg.to_string());
            }
        }
        AttachType::Wld => {
            if has(2) {
                put("actor", &cast.actor);
                put("arg", "sword");
                put("cmd", &command);
            }
            if has(3) {
                put("actor", &cast.actor);
                put("speech", first_word(&record.arglist, "hello"));
            }
            if has(6) {
                put("actor", &cast.actor);
                put("direction", "south");
            }
            if has(7) {
                put("actor", &cast.actor);
                put("object", &cast.object);
            }
            if has(15) {
                put("actor", &cast.actor);
                put("victim", &cast.victim);
                put("object", &cast.object);
                put("spell", "32");
                put("spellname", "magic missile");
            }
            if has(16) {
                put("actor", &cast.actor);
                put("direction", "north");
            }
            if has(17) {
                put("actor", &cast.actor);
                put("cmd", "open");
                put("direction", "north");
            }
            if has(18) {
                put("actor", &cast.actor);
            }
            if has(19) {
                put("time", &record.narg.to_string());
            }
        }
    }
    vars
}
