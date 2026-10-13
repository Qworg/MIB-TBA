//! Whole-corpus tests: read all 1,461 real tbaMUD triggers and run each one.
//!
//! The corpus is found through `TBAMUD_SOURCE` (default
//! `/home/qworg/Development/tbamud`); without it every test here prints a
//! skip message and passes. Run `cargo test -p dg-vm --test corpus --
//! --nocapture` for the summary of what the VM still mishandles.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::corpus::{self, EXPECTED_RECORDS, Record};
use common::events::{Cast, event_vars};
use common::world::{Fixture, checked_out, make_proto, run_waits, set_event_vars};
use dg_vm::driver::{Mode, script_driver};
use dg_vm::host::WorldHost;
use dg_vm::mock::MockHost;
use dg_vm::types::{AttachType, TrigProto, Uid};

#[test]
fn the_corpus_reader_reads_every_record() {
    let Some(records) = corpus::load_corpus() else {
        corpus::skip_message("the_corpus_reader_reads_every_record");
        return;
    };
    assert_eq!(records.len(), EXPECTED_RECORDS);

    let count = |attach: i32| records.iter().filter(|r| r.attach_type == attach).count();
    // docs/design/triggers.md section 1: mob 795, world 384, object 282.
    assert_eq!(count(0), 795);
    assert_eq!(count(1), 282);
    assert_eq!(count(2), 384);

    // Vnums are unique.
    let vnums: BTreeSet<i32> = records.iter().map(|r| r.vnum).collect();
    assert_eq!(vnums.len(), EXPECTED_RECORDS);

    // Trigger 9 of the tutorial zone: "snarl with glee".
    let snarl = corpus::find(&records, 901).expect("trigger 901");
    assert_eq!(snarl.name, "snarl with glee");
    assert_eq!(snarl.attach_type, 0);
    assert_eq!(snarl.flags, "g");
    assert_eq!(snarl.narg, 100);
    assert_eq!(snarl.arglist, "");
    assert_eq!(snarl.body, "wait 1 s\r\nsnarl\r\n");
}

#[test]
fn a_tilde_inside_a_line_does_not_end_the_string() {
    // fread_string strips only a trailing '~' (db.c).
    let text = "#7\nname~\n0 d 1\n*~\nsay small bit of fun~ I am all that.\nsay two\n~\n$~\n";
    let records = corpus::parse_file("t.trg", text).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].body,
        "say small bit of fun~ I am all that.\r\nsay two\r\n"
    );
    assert_eq!(records[0].arglist, "*");
    assert_eq!(records[0].narg, 1);
}

#[test]
fn at_signs_become_tabs_unless_doubled() {
    // parse_at (db.c): '@' not followed by '@' is replaced by a tab.
    let text = "#8\nn~\n1 b 0\n~\nsay @Rred@n @@ x~\n$~\n";
    let records = corpus::parse_file("t.trg", text).unwrap();
    assert_eq!(records[0].body, "say \tRred\tn @@ x");
}

// get_line (utils.c) skips lines that start with '*', '\n' or '\r', for the
// '#' line and for the header; discrete_load stops at a '#' number of 99999
// or more and reads the number with sscanf("#%d"), which accepts text after it;
// sscanf("%d %s %d") reads the 100 of "100x".
#[test]
fn the_reader_follows_get_line_and_discrete_load() {
    let text = "* a comment\n\n#7 trailing words\n\
                name~\n\n* another\n0 d 100x\n*~\nbody~\n\
                #99999\n#8\nlater~\n0 a 1\n~\nx~\n$~\n";
    let records = corpus::parse_file("t.trg", text).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].vnum, 7);
    assert_eq!(records[0].name, "name");
    assert_eq!(records[0].flags, "d");
    assert_eq!(records[0].narg, 100);
    assert_eq!(records[0].body, "body");
}

// fread_string reads fgets chunks of at most 511 bytes: a longer line gets
// "\r\n" after the first chunk, and a '~' that is the 511th byte ends the
// string there (the rest of the line is read as a line of its own).
#[test]
fn the_reader_chunks_long_lines_like_fgets() {
    let long = "a".repeat(600);
    let text = format!("#7\nn~\n0 d 1\n~\n{long}\n~\n$~\n");
    let records = corpus::parse_file("t.trg", &text).unwrap();
    let expected = format!("{}\r\n{}\r\n", "a".repeat(511), "a".repeat(89));
    assert_eq!(records[0].body, expected);

    let name = "x".repeat(510);
    let text = format!("#7\n{name}~*junk\n0 d 1\n~\nb~\n$~\n");
    let records = corpus::parse_file("t.trg", &text).unwrap();
    assert_eq!(records[0].name, name);
    assert_eq!(records[0].narg, 1);
}

/// Vnums and spell names a script's lines refer to, so the mock world can
/// hold what the script expects to find.
#[derive(Default)]
struct Refs {
    mobs: BTreeSet<i32>,
    objs: BTreeSet<i32>,
    rooms: BTreeSet<i32>,
    spells: BTreeSet<String>,
    /// Names of characters a script addresses (`%at% rumble ...`).
    people: BTreeSet<String>,
}

/// What a first run of a trigger showed its world to lack, learned from the
/// `script_log` lines of that run: the next run's world has it.
#[derive(Default, Clone, PartialEq, Eq)]
struct Needs {
    rooms: BTreeSet<i32>,
    people: BTreeSet<String>,
    /// The `arg` the command triggers receive is the name of a character
    /// (the script sends to it or reads its fields).
    arg_is_character: bool,
}

/// Learn what the world lacked from a run's log. Returns whether anything
/// was learned that the run did not already have.
fn harvest(logs: &[String], needs: &mut Needs) -> bool {
    let before = needs.clone();
    for message in logs {
        // `wat: location not found (2301)`, `wat: ... (rumble)`.
        if let Some(at) = message.find("location not found (") {
            let name = message[at + 20..].trim_end_matches(')');
            match name.parse::<i32>() {
                Ok(room) => {
                    needs.rooms.insert(room);
                }
                Err(_) => {
                    needs.people.insert(name.to_owned());
                }
            }
        }
        // `findmob.vnum(ovnum): No room with vnum 21102`.
        if let Some(at) = message.find("No room with vnum ")
            && let Ok(room) = message[at + 18..].trim().parse::<i32>()
        {
            needs.rooms.insert(room);
        }
        // `msend: victim (sword) does not exist`.
        if let Some(at) = message.find(": victim (")
            && let Some(name) = message[at + 10..].strip_suffix(") does not exist")
        {
            if name == EVENT_ARG {
                needs.arg_is_character = true;
            } else {
                needs.people.insert(name.to_owned());
            }
        }
    }
    *needs != before
}

/// The text of `text` with the `%var%` references to the constants in `vars`
/// replaced, and `%random.N%` by 1, the mock's roll when none is scripted.
fn substitute_constants(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('%') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        if let Some(value) = vars.get(name) {
            out.push_str(value);
        } else if name.starts_with("random.") {
            out.push('1');
        } else {
            out.push('%');
            out.push_str(name);
            out.push('%');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// `eval`'s arithmetic on constants: integers joined by `+ - * /`, with `*`
/// and `/` binding tighter. `None` for anything else.
fn evaluate_constants(expr: &str) -> Option<i64> {
    let mut terms: Vec<(char, i64)> = Vec::new();
    let mut sign = '+';
    let mut product: Option<i64> = None;
    let mut times = '*';
    let mut number = String::new();
    let flush = |number: &mut String, product: &mut Option<i64>, times: char| -> Option<()> {
        if number.is_empty() {
            return None;
        }
        let value: i64 = number.parse().ok()?;
        number.clear();
        *product = Some(match *product {
            None => value,
            Some(left) if times == '*' => left.checked_mul(value)?,
            Some(left) => left.checked_div(value)?,
        });
        Some(())
    };
    for c in expr.chars().filter(|c| !c.is_whitespace()) {
        match c {
            '0'..='9' => number.push(c),
            '*' | '/' => {
                flush(&mut number, &mut product, times)?;
                times = c;
            }
            '+' | '-' => {
                flush(&mut number, &mut product, times)?;
                terms.push((sign, product.take()?));
                sign = c;
                times = '*';
            }
            _ => return None,
        }
    }
    flush(&mut number, &mut product, times)?;
    terms.push((sign, product?));
    terms.into_iter().try_fold(0i64, |sum, (sign, value)| {
        if sign == '+' {
            sum.checked_add(value)
        } else {
            sum.checked_sub(value)
        }
    })
}

/// The integers a script uses or computes from constants: the digit runs of
/// three or more digits on every line, and the values of the `set` and `eval`
/// lines whose operands are known (`eval num %random.99% + 2300`, `set zone
/// 211` followed by `mgoto %zone%02`). Straight-line only: both branches of
/// an `if` and every pass of a loop are read once.
fn candidate_numbers(body: &str) -> BTreeSet<i32> {
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    let mut found = BTreeSet::new();
    let mut note = |text: &str, shortest: usize| {
        for run in text.split(|c: char| !c.is_ascii_digit()) {
            if run.len() >= shortest
                && let Ok(number) = run.parse::<i32>()
            {
                found.insert(number);
            }
        }
    };
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('*') {
            continue;
        }
        let text = substitute_constants(line, &vars);
        note(&text, 3);
        let mut words = text.splitn(3, char::is_whitespace);
        let (Some(verb), Some(name), Some(value)) = (words.next(), words.next(), words.next())
        else {
            continue;
        };
        let value = value.trim();
        let value = match verb {
            "set" => value.to_owned(),
            "eval" => evaluate_constants(value).map_or_else(|| value.to_owned(), |n| n.to_string()),
            _ => continue,
        };
        note(&value, 1);
        vars.insert(name.to_owned(), value);
    }
    found
}

/// The `arg` the command and act triggers are given unless a run learned
/// that the script wants a character.
const EVENT_ARG: &str = "sword";

/// `is_abbrev(arg, full)` (`utils.c`): a non-empty prefix of `full`, any case.
fn is_abbrev(arg: &str, full: &str) -> bool {
    !arg.is_empty() && arg.len() <= full.len() && full[..arg.len()].eq_ignore_ascii_case(arg)
}

fn number(word: &str) -> Option<i32> {
    if word.is_empty() || !word.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    word.parse().ok()
}

/// Collect what one command line (already split into words) refers to.
fn scan_command(words: &[&str], line: &str, refs: &mut Refs) {
    let Some(first) = words.first().map(|w| w.to_ascii_lowercase()) else {
        return;
    };
    // `%load%`, `mload`, `wload`… are the same command.
    let command = first.trim_matches('%');
    let command = command
        .strip_prefix(['m', 'o', 'w'])
        .filter(|rest| {
            matches!(
                *rest,
                "load" | "at" | "teleport" | "door" | "transform" | "goto"
            )
        })
        .unwrap_or(command);
    match command {
        // do_mload / do_oload / do_wload: `is_abbrev(arg1, "mob")` or "obj",
        // then the vnum.
        "load" => {
            if let (Some(kind), Some(vnum)) = (words.get(1), words.get(2).and_then(|w| number(w))) {
                if is_abbrev(kind, "obj") {
                    refs.objs.insert(vnum);
                } else if is_abbrev(kind, "mob") {
                    refs.mobs.insert(vnum);
                }
            }
        }
        // do_wat: `at <room> <command>`; the rest of the line is a command.
        "at" => {
            if let Some(room) = words.get(1).and_then(|w| number(w)) {
                refs.rooms.insert(room);
            }
            if words.len() > 2 {
                scan_command(&words[2..], line, refs);
            }
        }
        // do_otransform / do_mtransform: the vnum of an object or mobile
        // prototype (the owner's kind decides, so make both exist).
        "transform" => {
            if let Some(vnum) = words.get(1).and_then(|w| number(w)) {
                refs.objs.insert(vnum);
                refs.mobs.insert(vnum);
            }
        }
        "teleport" | "goto" => refs
            .rooms
            .extend(words.iter().skip(1).filter_map(|w| number(w))),
        // `door <room> <dir> <field> <value>`: the room, and the value of a
        // `room` field.
        "door" => {
            if let Some(room) = words.get(1).and_then(|w| number(w)) {
                refs.rooms.insert(room);
            }
            if words.get(3).is_some_and(|w| w.eq_ignore_ascii_case("room"))
                && let Some(room) = words.get(4).and_then(|w| number(w))
            {
                refs.rooms.insert(room);
            }
        }
        "dg_cast" => {
            if let Some(open) = line.find('\'')
                && let Some(len) = line[open + 1..].find('\'')
            {
                refs.spells
                    .insert(line[open + 1..open + 1 + len].to_ascii_lowercase());
            }
        }
        _ => {}
    }
}

fn referenced_vnums(record: &Record) -> Refs {
    let mut refs = Refs::default();
    for line in record.body.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        scan_command(&words, line, &mut refs);
    }
    // `%findobj.<room>(<vnum>)%` and `%findmob.<room>(<vnum>)%` look in a room
    // by number (dg_variables.c findobj/findmob).
    for key in ["findobj.", "findmob."] {
        for (at, _) in record.body.match_indices(key) {
            let digits: String = record.body[at + key.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(room) = digits.parse::<i32>() {
                refs.rooms.insert(room);
            }
        }
    }
    // Numbers the script computes may name a room, an object or a mobile.
    for number in candidate_numbers(&record.body) {
        refs.rooms.insert(number);
        refs.objs.insert(number);
        refs.mobs.insert(number);
    }
    // `%at% rumble ...`: the character it names stands somewhere.
    for line in record.body.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if let Some(position) = words
            .iter()
            .position(|word| matches!(word.trim_matches('%'), "at" | "mat" | "wat" | "oat"))
            && let Some(target) = words.get(position + 1)
            && !target.starts_with('%')
            && target.parse::<i32>().is_err()
        {
            refs.people.insert((*target).to_owned());
        }
    }
    refs
}

/// How the owner is placed: objects rotate through lying in the room,
/// carried by the player and worn by the player.
fn build(
    record: &Record,
    protos: &BTreeMap<i32, Arc<TrigProto>>,
    index: usize,
    needs: &Needs,
) -> BuiltWorld {
    let mut fx = Fixture::new();
    fx.host.protos = protos.clone();
    let proto = Arc::clone(&protos[&record.vnum]);
    // `DG_CASTER_PROXY`: the mob that casts for objects and rooms.
    fx.host
        .globals
        .texts
        .insert("mobproto:1".to_owned(), "a caster proxy".to_owned());
    let mut refs = referenced_vnums(record);
    refs.rooms.extend(needs.rooms.iter().copied());
    refs.people.extend(needs.people.iter().cloned());
    // Each character a script addresses by name stands in a room of its own.
    let people: Vec<(String, i32)> = refs
        .people
        .iter()
        .zip(98_000..)
        .map(|(name, vnum)| (name.clone(), vnum))
        .collect();
    refs.rooms.extend(people.iter().map(|(_, vnum)| *vnum));
    for vnum in &refs.mobs {
        fx.host
            .globals
            .texts
            .insert(format!("mobproto:{vnum}"), "a loaded mob".to_owned());
    }
    for vnum in &refs.objs {
        fx.host
            .globals
            .texts
            .insert(format!("objproto:{vnum}"), "a loaded object".to_owned());
    }
    // Every object and mob the fixture creates has a prototype in the game
    // (the C's `read_object` succeeds for `%load% obj %item.vnum%`).
    for vnum in [3020, 3021, 3022, 3023, 3024, 3030, 3031] {
        fx.host
            .globals
            .texts
            .insert(format!("objproto:{vnum}"), "a fixture object".to_owned());
    }
    fx.host
        .globals
        .texts
        .insert("mobproto:3100".to_owned(), "a city guard".to_owned());
    // One zone that covers every vnum, so `%zoneecho% <vnum>` finds it.
    let nums = &mut fx.host.globals.nums;
    nums.insert("zone:30:bot".to_owned(), 0);
    nums.insert("zone:30:top".to_owned(), 99_999);
    fx.host
        .globals
        .texts
        .insert("zone:30:name".to_owned(), "The Test Zone".to_owned());
    // `%load% obj %self.vnum%` and the like clone the owner itself.
    fx.host
        .globals
        .texts
        .insert("objproto:3031".to_owned(), "the owner object".to_owned());
    fx.host
        .globals
        .texts
        .insert("mobproto:3000".to_owned(), "the owner".to_owned());
    for (number, name) in refs.spells.iter().enumerate() {
        // TAR_CHAR_ROOM | TAR_FIGHT_VICT, MAG_DAMAGE
        fx.host
            .add_spell(name, 1 + number as i32, (1 << 1) | (1 << 4), 1);
    }
    for vnum in &refs.rooms {
        if (0..100_000).contains(vnum) && fx.host.real_room(*vnum).is_none() {
            fx.host.add_room(*vnum, &format!("Room {vnum}"));
        }
    }
    let rooms: Vec<Uid> = std::iter::once(fx.room)
        .chain(fx.neighbours.iter().copied())
        .chain(
            refs.rooms
                .iter()
                .filter_map(|vnum| fx.host.real_room(*vnum)),
        )
        .collect();
    for room in rooms {
        fx.host.set_num(room, "zone", 30);
    }
    for (name, vnum) in &people {
        if let Some(room) = fx.host.real_room(*vnum) {
            let person = fx.host.add_player(name);
            common::world::dress_player(&mut fx.host, person, 10);
            fx.host.char_to_room(person, room);
        }
    }
    let attach = AttachType::from_index(record.attach_type).unwrap_or(AttachType::Mob);
    let owner = match attach {
        AttachType::Mob => {
            let mob = fx.host.add_mob(3000, "the owner");
            fx.host.set_text(mob, "alias", "owner");
            common::world::dress_mob(&mut fx.host, mob, 30);
            fx.host.char_to_room(mob, fx.room);
            let item = fx.host.add_obj(3030, "a key");
            fx.host.set_text(item, "alias", "key");
            fx.host.obj_to_char(item, mob);
            mob
        }
        AttachType::Obj => {
            let obj = fx.host.add_obj(3031, "the owner object");
            fx.host.set_text(obj, "alias", "owner object");
            match index % 3 {
                0 => fx.host.obj_to_room(obj, fx.room),
                1 => fx.host.obj_to_char(obj, fx.player),
                _ => fx.host.equip_char(fx.player, obj, 5),
            }
            obj
        }
        AttachType::Wld => fx.room,
    };
    let id = fx.host.attach(owner, &proto).expect("owner exists");
    // perform_give / perform_drop run the receive and drop triggers before
    // the object leaves the actor's inventory (`obj->carried_by != actor`).
    let bits = dg_vm::types::trig::asciiflag_conv(&record.flags);
    let carried = match attach {
        AttachType::Mob => bits & (1 << 9) != 0,
        AttachType::Wld => bits & (1 << 7) != 0,
        AttachType::Obj => false,
    };
    let object = if carried { fx.inventory[0] } else { fx.bench };
    let cast = Cast {
        actor: dg_vm::types::uid_text(fx.player),
        victim: dg_vm::types::uid_text(fx.guard),
        object: dg_vm::types::uid_text(object),
        target: dg_vm::types::uid_text(fx.inventory[0]),
        arg: if needs.arg_is_character {
            "Tester".to_owned()
        } else {
            EVENT_ARG.to_owned()
        },
    };
    let vars = event_vars(record, attach, &cast);
    let vars: Vec<(&str, String)> = vars.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    set_event_vars(&mut fx.host, owner, id, &vars);
    BuiltWorld {
        host: fx.host,
        owner,
        id,
    }
}

struct BuiltWorld {
    host: MockHost,
    owner: Uid,
    id: u64,
}

/// What one record's run left behind.
struct Outcome {
    vnum: i32,
    logs: Vec<String>,
    unsupported: Vec<String>,
    commands: Vec<String>,
    resumes: usize,
    problems: Vec<String>,
}

fn proto_table(records: &[Record]) -> BTreeMap<i32, Arc<TrigProto>> {
    let mut scratch = MockHost::new();
    records
        .iter()
        .map(|record| {
            let proto = make_proto(
                &mut scratch,
                record.vnum,
                &record.name,
                AttachType::from_index(record.attach_type).unwrap_or(AttachType::Mob),
                &record.flags,
                record.narg,
                &record.arglist,
                &record.body,
            );
            (record.vnum, proto)
        })
        .collect()
}

/// Most runs of one trigger: each learns what the world lacked (see
/// [`harvest`]) for the next.
const MAX_ROUNDS: usize = 4;

fn run_record(record: &Record, protos: &BTreeMap<i32, Arc<TrigProto>>, index: usize) -> Outcome {
    let mut needs = Needs::default();
    let mut outcome = run_once(record, protos, index, &needs);
    for _ in 1..MAX_ROUNDS {
        if !harvest(&outcome.logs, &mut needs) {
            break;
        }
        outcome = run_once(record, protos, index, &needs);
    }
    outcome
}

fn run_once(
    record: &Record,
    protos: &BTreeMap<i32, Arc<TrigProto>>,
    index: usize,
    needs: &Needs,
) -> Outcome {
    let mut world = build(record, protos, index, needs);
    let mut problems = Vec::new();
    script_driver(&mut world.host, world.owner, world.id, Mode::New);
    let resumes = run_waits(&mut world.host, 25);
    if world.host.vm.depth != 0 {
        problems.push(format!("depth counter is {}", world.host.vm.depth));
    }
    let stuck = checked_out(&world.host);
    if !stuck.is_empty() {
        problems.push(format!("left checked out: {stuck:?}"));
    }
    Outcome {
        vnum: record.vnum,
        logs: std::mem::take(&mut world.host.logs),
        unsupported: world
            .host
            .unsupported
            .iter()
            .map(|(what, _)| what.clone())
            .collect(),
        commands: world
            .host
            .mud_commands
            .iter()
            .map(|(_, line)| line.clone())
            .collect(),
        resumes,
        problems,
    }
}

/// Reduce a script_log line to its shape: drop the `Trigger: name, Vnum N`
/// prefix (any case, with the optional `, type N`), digits become `#` and the
/// text of a `'%s'` becomes `'..'`. The literal quoted keywords of the
/// driver's own `Trigger VNum N has 'else' without 'if'.` messages and their
/// `(error N)` number stay, so different C messages keep different shapes.
fn log_shape(message: &str) -> String {
    let literal = message.starts_with("Trigger VNum ");
    let mut rest = message;
    if let Some(after) = message.strip_prefix("Trigger: ") {
        let lower = after.to_ascii_lowercase();
        if let Some(at) = lower.find(", vnum ") {
            let m = after[at + 7..].trim_start_matches(|c: char| c.is_ascii_digit());
            let m = m
                .strip_prefix(", type")
                .map(|t| {
                    t.trim_start_matches(':')
                        .trim_start()
                        .trim_start_matches(|c: char| c.is_ascii_digit())
                })
                .unwrap_or(m);
            rest = m
                .strip_prefix(". ")
                .or_else(|| m.strip_prefix('.'))
                .unwrap_or(m);
        }
    }
    // The C formats put the quoted text (which may hold apostrophes itself)
    // at or near the end of the line: everything from the first quote to the
    // last one is the quoted text.
    let (head, tail) = match (rest.find('\''), rest.rfind('\'')) {
        (Some(first), Some(last)) if first < last && !literal => {
            (&rest[..first], &rest[last + 1..])
        }
        _ => (rest, ""),
    };
    let quoted = head.len() != rest.len();
    let mut out = String::new();
    let mut last_digit = false;
    let mut keep_digits = false;
    for c in head
        .chars()
        .chain(std::iter::once('\0'))
        .chain(tail.chars())
    {
        if c == '\0' {
            if quoted {
                out.push_str("'..'");
            }
            last_digit = false;
        } else if c.is_ascii_digit() {
            if !last_digit {
                keep_digits = out.ends_with("(error ");
                if !keep_digits {
                    out.push('#');
                }
            }
            if keep_digits {
                out.push(c);
            }
            last_digit = true;
        } else {
            out.push(c);
            last_digit = false;
        }
    }
    out
}

/// Shapes that report a room, object or mobile vnum the script computed
/// (`%at% %num%`, `%load% obj %present3%`) and the harness did not create: a
/// gap of the mock world, not of the VM.
fn is_world_gap(shape: &str) -> bool {
    shape.contains("location not found")
        || shape.ends_with("invalid location")
        || shape.contains("bad object vnum")
        || shape.contains("bad mob vnum")
        || shape.contains("No room with vnum")
}

fn top<K: Clone + Ord>(counts: &BTreeMap<K, usize>, n: usize) -> Vec<(K, usize)> {
    let mut rows: Vec<(K, usize)> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(n);
    rows
}

#[test]
fn every_corpus_trigger_runs_to_completion() {
    let Some(records) = corpus::load_corpus() else {
        corpus::skip_message("every_corpus_trigger_runs_to_completion");
        return;
    };
    assert_eq!(records.len(), EXPECTED_RECORDS);

    // A worker thread so one runaway script fails the test rather than
    // hanging it.
    let (tx, rx) = mpsc::channel::<Result<Outcome, (i32, String)>>();
    let work = records.clone();
    let protos = proto_table(&records);
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            for (index, record) in work.iter().enumerate() {
                let result = catch_unwind(AssertUnwindSafe(|| run_record(record, &protos, index)))
                    .map_err(|panic| {
                        let message = panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                            .unwrap_or_default();
                        (record.vnum, message)
                    });
                if tx.send(result).is_err() {
                    return;
                }
            }
        })
        .expect("spawn worker");

    let mut outcomes = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for record in &records {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(outcome)) => {
                for problem in &outcome.problems {
                    failures.push(format!("trigger {}: {problem}", outcome.vnum));
                }
                outcomes.push(outcome);
            }
            Ok(Err((vnum, message))) => {
                failures.push(format!("trigger {vnum} panicked: {message}"))
            }
            Err(_) => {
                failures.push(format!(
                    "trigger {} did not terminate within 60s",
                    record.vnum
                ));
                break;
            }
        }
    }

    print_summary(&outcomes, &failures);
    assert!(
        failures.is_empty(),
        "{} failures: {failures:#?}",
        failures.len()
    );
}

fn print_summary(outcomes: &[Outcome], failures: &[String]) {
    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    let mut shape_vnums: BTreeMap<String, BTreeSet<i32>> = BTreeMap::new();
    let mut shape_sample: BTreeMap<String, String> = BTreeMap::new();
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let mut words: BTreeMap<String, usize> = BTreeMap::new();
    let mut word_vnums: BTreeMap<String, BTreeSet<i32>> = BTreeMap::new();
    let mut total_logs = 0;
    let mut total_commands = 0;
    let mut resumes = 0;
    for outcome in outcomes {
        resumes += outcome.resumes;
        for message in &outcome.logs {
            total_logs += 1;
            let shape = log_shape(message);
            *shapes.entry(shape.clone()).or_default() += 1;
            shape_sample
                .entry(shape.clone())
                .or_insert_with(|| message.clone());
            shape_vnums.entry(shape).or_default().insert(outcome.vnum);
        }
        for key in &outcome.unsupported {
            *unsupported.entry(key.clone()).or_default() += 1;
        }
        for line in &outcome.commands {
            total_commands += 1;
            let word = line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            word_vnums
                .entry(word.clone())
                .or_default()
                .insert(outcome.vnum);
            *words.entry(word).or_default() += 1;
        }
    }

    println!("=== corpus smoke summary ===");
    println!(
        "records run: {}  resumed waits: {resumes}  failures: {}",
        outcomes.len(),
        failures.len()
    );
    println!(
        "script_log messages: {total_logs} in {} shapes",
        shapes.len()
    );
    let mut gaps: BTreeMap<String, usize> = BTreeMap::new();
    let mut real: BTreeMap<String, usize> = BTreeMap::new();
    for (shape, count) in &shapes {
        let into = if is_world_gap(shape) {
            &mut gaps
        } else {
            &mut real
        };
        into.insert(shape.clone(), *count);
    }
    for (shape, count) in top(&real, 100) {
        println!("  {count:5}  {shape}  [e.g. {}]", shape_sample[&shape]);
    }
    println!(
        "world gaps (rooms, objects and mobs a script computes that the harness does not create):"
    );
    for (shape, count) in top(&gaps, 100) {
        println!("  {count:5}  {shape}  [e.g. {}]", shape_sample[&shape]);
    }
    println!("vnums behind the 20 most frequent log shapes:");
    for (shape, count) in top(&real, 20) {
        let vnums: Vec<String> = shape_vnums[&shape].iter().map(|v| v.to_string()).collect();
        println!(
            "  {count:5}  {shape}\n         vnums: {}\n         e.g. {}",
            vnums.join(" "),
            shape_sample[&shape]
        );
    }
    println!("BaseHost::unsupported keys: {} distinct", unsupported.len());
    for (key, count) in top(&unsupported, usize::MAX) {
        println!("  {count:5}  {key}");
    }
    println!(
        "mud_command first words: {total_commands} commands, {} distinct",
        words.len()
    );
    for (word, count) in top(&words, usize::MAX) {
        let vnums: Vec<String> = word_vnums[&word].iter().map(|v| v.to_string()).collect();
        println!("  {count:5}  {word}  [vnums {}]", vnums.join(" "));
    }
    for failure in failures {
        println!("FAILURE {failure}");
    }
}
