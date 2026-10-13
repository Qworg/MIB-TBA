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
use dg_vm::host::{BaseHost, WorldHost};
use dg_vm::mock::MockHost;
use dg_vm::types::{AttachType, ScriptMemory, TrigProto, Uid};

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

// A corpus whose index lists a file that is not there is reported as an error
// (the scenarios then skip) rather than panicking.
#[test]
fn a_missing_file_of_the_index_is_an_error_not_a_panic() {
    let dir = std::env::temp_dir().join(format!("dg-vm-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index"), "0.trg\n30.trg\n$\n").unwrap();
    std::fs::write(dir.join("0.trg"), "#7\nn~\n0 d 1\n~\nb~\n$~\n").unwrap();
    let result = corpus::read_corpus_from(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
    let error = result.unwrap_err();
    assert!(error.contains("30.trg"), "{error}");
    assert!(
        corpus::read_corpus_from(&dir)
            .unwrap_err()
            .contains("not found")
    );
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
    /// Words the script addresses a loaded mob or object by (`%force% mist
    /// kill ...`).
    addressed: BTreeSet<String>,
    /// Whether the script loads a mob (which then answers to `addressed`).
    loads_mobs: bool,
    /// Names of characters a script addresses (`%at% rumble ...`).
    people: BTreeSet<String>,
    /// The people the script reaches with `%at%`: they stand in rooms of
    /// their own. Every other addressed character stands with the owner.
    remote: BTreeSet<String>,
    /// Names a script purges (`%purge% spectre`): the owner answers to them
    /// when the script loads nothing else.
    purged: BTreeSet<String>,
    /// What `arg` needs after the player's name when a script hands the whole
    /// of it to a command that takes more than a name (`%force% %arg%`).
    arg_tail: &'static str,
}

/// What a first run of a trigger showed its world to lack, learned from the
/// `script_log` lines of that run: the next run's world has it.
#[derive(Default, Clone, PartialEq, Eq)]
struct Needs {
    rooms: BTreeSet<i32>,
    people: BTreeSet<String>,
    /// People a script reaches with `%at%`, who stand elsewhere.
    remote: BTreeSet<String>,
    /// The `arg` the command triggers receive is a room number (the script
    /// teleports to it).
    arg_is_room: bool,
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
                    needs.remote.insert(name.to_owned());
                }
            }
        }
        // `findmob.vnum(ovnum): No room with vnum 21102`.
        if let Some(at) = message.find("No room with vnum ")
            && let Ok(room) = message[at + 18..].trim().parse::<i32>()
        {
            needs.rooms.insert(room);
        }
        // `mteleport target is an invalid room`, `oteleport ...`: the script
        // teleports to what the command was typed with.
        if message.ends_with("teleport target is an invalid room") {
            needs.arg_is_room = true;
        }
        // `msend: victim (Rumble) does not exist`: a character the script
        // addresses by name.
        if let Some(at) = message.find(": victim (")
            && let Some(name) = message[at + 10..].strip_suffix(") does not exist")
        {
            needs.people.insert(name.to_owned());
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
                // Room 0 is the void, where a variable holding 0 reads its
                // globals: scripts that compute 0 do not mean that room.
                && number > 0
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

/// The `arg` the command triggers are given: the name of a player in the room
/// other than the actor, which is what most scripts do with it (send to it,
/// force it, read its fields), unless a run learned that it is a room.
const EVENT_ARG: &str = "Watcher";

/// The room number `arg` is when the script teleports to it.
const EVENT_ARG_ROOM: &str = "3002";

/// `spello(...)` in `spell_parser.c` with the numbers of `spells.h`: the spell
/// table a script's `dg_cast` looks its spell up in.
const SPELLS: &[(i32, &str)] = &[
    (1, "armor"),
    (2, "teleport"),
    (3, "bless"),
    (4, "blindness"),
    (5, "burning hands"),
    (6, "call lightning"),
    (7, "charm person"),
    (8, "chill touch"),
    (9, "clone"),
    (10, "color spray"),
    (11, "control weather"),
    (12, "create food"),
    (13, "create water"),
    (14, "cure blind"),
    (15, "cure critic"),
    (16, "cure light"),
    (17, "curse"),
    (18, "detect alignment"),
    (19, "detect invisibility"),
    (20, "detect magic"),
    (21, "detect poison"),
    (22, "dispel evil"),
    (23, "earthquake"),
    (24, "enchant weapon"),
    (25, "energy drain"),
    (26, "fireball"),
    (27, "harm"),
    (28, "heal"),
    (29, "invisibility"),
    (30, "lightning bolt"),
    (31, "locate object"),
    (32, "magic missile"),
    (33, "poison"),
    (34, "protection from evil"),
    (35, "remove curse"),
    (36, "sanctuary"),
    (37, "shocking grasp"),
    (38, "sleep"),
    (39, "strength"),
    (40, "summon"),
    (42, "word of recall"),
    (43, "remove poison"),
    (44, "sense life"),
    (45, "animate dead"),
    (46, "dispel good"),
    (47, "group armor"),
    (48, "group heal"),
    (50, "infravision"),
    (51, "waterwalk"),
    (52, "identify"),
    (53, "fly"),
    (54, "darkness"),
    (298, "Script-inflicted"),
];

/// `SPELL_MAGIC_MISSILE`, the spell the cast triggers are offered.
const SPELL_MAGIC_MISSILE: i32 = 32;

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
fn scan_command(words: &[&str], refs: &mut Refs) {
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
                "load"
                    | "at"
                    | "teleport"
                    | "door"
                    | "transform"
                    | "goto"
                    | "force"
                    | "damage"
                    | "send"
                    | "purge"
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
                    refs.loads_mobs = true;
                }
            }
        }
        // do_wat: `at <room> <command>`; the rest of the line is a command.
        "at" => {
            if let Some(room) = words.get(1).and_then(|w| number(w)) {
                refs.rooms.insert(room);
            }
            if words.len() > 2 {
                scan_command(&words[2..], refs);
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
        // `force <victim> <command>`, `damage <victim> <n>`,
        // `send <victim> <text>`: the first word names who it acts on.
        "force" | "damage" | "send" => {
            if let Some(word) = words.get(1)
                && !word.starts_with('%')
                && number(word).is_none()
            {
                refs.addressed.insert((*word).to_owned());
            }
        }
        // `purge <name>`: the owner (or something it holds) answers to it.
        "purge" => {
            if let Some(word) = words.get(1)
                && !word.starts_with('%')
                && number(word).is_none()
            {
                refs.purged.insert((*word).to_owned());
            }
        }
        _ => {}
    }
    // `%teleport% %arg%` and `%force% %arg%` take the player's name and then
    // a room or a command.
    if let ([_, "%arg%"], "teleport" | "force") = (words, command) {
        refs.arg_tail = if command == "teleport" {
            " 3002"
        } else {
            " look"
        };
    }
}

fn referenced_vnums(record: &Record) -> Refs {
    let mut refs = Refs::default();
    for line in record.body.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        scan_command(&words, &mut refs);
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
    // Without a mob to load, the characters a script addresses by name are
    // players standing somewhere.
    if !refs.loads_mobs {
        refs.people.extend(refs.addressed.iter().cloned());
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
            refs.remote.insert((*target).to_owned());
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
    // `%teleport% %actor% %self.vnum%` names a room by the owner's own number.
    refs.rooms.extend([3000, 3031]);
    refs.rooms.extend(needs.rooms.iter().copied());
    refs.people.extend(needs.people.iter().cloned());
    refs.remote.extend(needs.remote.iter().cloned());
    refs.people.extend(needs.remote.iter().cloned());
    // A character the script reaches with `%at%` stands in a room of its own;
    // the others stand with the owner, where `mforce` and `msend` look.
    let people: Vec<(String, i32)> = refs
        .people
        .iter()
        .filter(|name| refs.remote.contains(*name))
        .zip(98_000..)
        .map(|(name, vnum)| (name.clone(), vnum))
        .collect();
    refs.rooms.extend(people.iter().map(|(_, vnum)| *vnum));
    // A loaded mob or object answers to the words the script addresses.
    let addressed: Vec<&str> = refs.addressed.iter().map(String::as_str).collect();
    let loaded = |what: &str| {
        std::iter::once(what)
            .chain(addressed.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    };
    for vnum in &refs.mobs {
        fx.host
            .globals
            .texts
            .insert(format!("mobproto:{vnum}"), loaded("a loaded mob"));
    }
    for vnum in &refs.objs {
        fx.host
            .globals
            .texts
            .insert(format!("objproto:{vnum}"), loaded("a loaded object"));
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
    for (number, name) in SPELLS {
        // TAR_CHAR_ROOM | TAR_FIGHT_VICT, MAG_DAMAGE
        fx.host.add_spell(name, *number, (1 << 1) | (1 << 4), 1);
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
    for name in refs
        .people
        .iter()
        .filter(|name| !refs.remote.contains(*name))
    {
        let person = fx.host.add_player(name);
        common::world::dress_player(&mut fx.host, person, 10);
        fx.host.char_to_room(person, fx.room);
    }
    let watcher = fx.host.add_player(EVENT_ARG);
    common::world::dress_player(&mut fx.host, watcher, 5);
    fx.host.char_to_room(watcher, fx.room);
    let attach = AttachType::from_index(record.attach_type).unwrap_or(AttachType::Mob);
    let bits = dg_vm::types::trig::asciiflag_conv(&record.flags);
    let owner = match attach {
        AttachType::Mob => {
            let mob = fx.host.add_mob(3000, "the owner");
            let alias = if refs.loads_mobs {
                "owner".to_owned()
            } else {
                std::iter::once("owner")
                    .chain(refs.purged.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            fx.host.set_text(mob, "alias", &alias);
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
            match object_place(bits, record.narg, &record.body, index) {
                Place::Room => fx.host.obj_to_room(obj, fx.room),
                Place::Inventory => fx.host.obj_to_char(obj, fx.player),
                Place::Worn => fx.host.equip_char(fx.player, obj, 5),
            }
            obj
        }
        AttachType::Wld => fx.room,
    };
    let id = fx.host.attach(owner, &proto).expect("owner exists");
    // `time` triggers fire in the hour their narg names.
    if bits & (1 << 19) != 0 {
        fx.host.time.hours = record.narg;
    }
    // perform_give / perform_drop run the receive and drop triggers before
    // the object leaves the actor's inventory (`obj->carried_by != actor`).
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
        arg: if needs.arg_is_room {
            EVENT_ARG_ROOM.to_owned()
        } else {
            format!("{EVENT_ARG}{}", refs.arg_tail)
        },
    };
    let vars = event_vars(record, attach, &cast);
    BuiltWorld {
        host: fx.host,
        owner,
        id,
        attach,
        bits,
        player: fx.player,
        guard: fx.guard,
        object,
        target: fx.inventory[0],
        room: fx.room,
        arg: cast.arg,
        vars,
    }
}

/// Where an object owner lies, stands or hangs for the event its trigger
/// types listen to: `get` fires on an object in the room, `drop`, `give`,
/// `wear` and `consume` on one the actor carries, `remove` on one worn, and a
/// command trigger where its narg says. A script that reads `self.worn_by`
/// is worn and one that reads `self.carried_by` is carried; the rest rotate
/// through the three.
fn object_place(bits: u32, narg: i32, body: &str, index: usize) -> Place {
    let has = |bit: u32| bits & (1 << bit) != 0;
    if has(2) {
        if narg & dg_vm::types::OCMD_EQUIP != 0 {
            return Place::Worn;
        }
        if narg & dg_vm::types::OCMD_INVEN != 0 {
            return Place::Inventory;
        }
        return Place::Room;
    }
    if has(6) || has(16) {
        Place::Room
    } else if has(7) || has(8) || has(9) || has(18) {
        Place::Inventory
    } else if has(11) || body.contains("self.worn_by") {
        Place::Worn
    } else if body.contains("self.carried_by") {
        Place::Inventory
    } else {
        match index % 3 {
            0 => Place::Room,
            1 => Place::Inventory,
            _ => Place::Worn,
        }
    }
}

enum Place {
    Room,
    Inventory,
    Worn,
}

/// A world with one trigger attached, ready to fire.
struct BuiltWorld {
    host: MockHost,
    owner: Uid,
    id: u64,
    attach: AttachType,
    bits: u32,
    player: Uid,
    guard: Uid,
    /// The object of the event (`object` of the receive and drop triggers).
    object: Uid,
    /// The object the act triggers get as `target`.
    target: Uid,
    room: Uid,
    arg: String,
    /// The event variables, for a trigger no dispatch function can fire.
    vars: Vec<(String, String)>,
}

/// A value `RunState::loops` never has after a run: `TRIG_NEW` resets it, so
/// finding it still there means no dispatch function ran the trigger.
const NOT_RUN: i32 = 123_456;

/// Fire the trigger through the dispatch function of the first event its type
/// letters name, with the command or speech its argument list matches and a
/// winning roll. `false` when the dispatcher declined to run it.
fn fire_by_dispatch(world: &mut BuiltWorld, record: &Record) -> bool {
    use dg_vm::dispatch as d;
    let mark = |world: &mut BuiltWorld, value: i32| {
        if let Some(trigger) = world
            .host
            .script_mut(world.owner)
            .and_then(|script| script.trigger_mut(world.id))
        {
            trigger.run.loops = value;
        }
    };
    mark(world, NOT_RUN);
    let host = &mut world.host;
    let (owner, actor, room) = (world.owner, world.player, world.room);
    let command = first_word(&record.arglist, "look");
    // A speech trigger with narg 0 matches a phrase (`is_substring`), one
    // with narg 1 a word.
    let speech = if record.narg == 0 && !record.arglist.trim().is_empty() && record.arglist != "*" {
        record.arglist.trim().to_owned()
    } else {
        first_word(&record.arglist, "hello")
    };
    let narg = record.narg.max(1);
    let spell = SPELL_MAGIC_MISSILE;
    let first = (1..20).find(|bit| world.bits & (1 << bit) != 0);
    // A percent roll wins when it is the lowest the die gives.
    host.rolls.extend([1; 4]);
    match (world.attach, first) {
        (AttachType::Mob, Some(1)) => d::random_mtrigger(host, owner),
        (AttachType::Mob, Some(2)) => {
            d::command_mtrigger(host, actor, &command, &world.arg);
        }
        (AttachType::Mob, Some(3)) => d::speech_mtrigger(host, actor, &speech),
        (AttachType::Mob, Some(4)) => d::act_mtrigger(
            host,
            owner,
            &format!("Tester says something about {}\r\n", record.arglist),
            Some(actor),
            Some(world.guard),
            Some(world.object),
            Some(world.target),
            None,
        ),
        (AttachType::Mob, Some(5)) => {
            d::death_mtrigger(host, owner, Some(actor));
        }
        (AttachType::Mob, Some(6 | 7)) => {
            d::greet_mtrigger(host, actor, 0);
        }
        (AttachType::Mob, Some(8)) => {
            d::entry_mtrigger(host, owner);
        }
        (AttachType::Mob, Some(9)) => {
            d::receive_mtrigger(host, owner, actor, world.object);
        }
        (AttachType::Mob, Some(10)) => {
            host.set_link(owner, "fighting", Some(actor));
            d::fight_mtrigger(host, owner);
        }
        (AttachType::Mob, Some(11)) => {
            // Hit points at or below the trigger's percentage of the maximum.
            let max = host.num(owner, "max_hit");
            host.set_num(owner, "hit", i64::from(narg - 1) * max / 100);
            host.set_link(owner, "fighting", Some(actor));
            d::hitprcnt_mtrigger(host, owner);
        }
        (AttachType::Mob, Some(12)) => d::bribe_mtrigger(host, owner, actor, narg.max(100)),
        (AttachType::Mob, Some(13)) => d::load_mtrigger(host, owner),
        (AttachType::Mob, Some(14)) => {
            host.set_script_mem(
                owner,
                vec![ScriptMemory {
                    id: actor,
                    cmd: None,
                }],
            );
            d::greet_memory_mtrigger(host, actor);
        }
        (AttachType::Mob, Some(15)) => {
            d::cast_mtrigger(host, actor, Some(owner), spell);
        }
        (AttachType::Mob, Some(16)) => {
            d::leave_mtrigger(host, actor, 0);
        }
        (AttachType::Mob, Some(17)) => {
            d::door_mtrigger(host, actor, 0, 0);
        }
        (AttachType::Mob, Some(19)) => d::time_mtrigger(host, owner),
        (AttachType::Obj, Some(1)) => d::random_otrigger(host, owner),
        (AttachType::Obj, Some(2)) => {
            d::command_otrigger(host, actor, &command, &world.arg);
        }
        (AttachType::Obj, Some(5)) => d::timer_otrigger(host, owner),
        (AttachType::Obj, Some(6)) => {
            d::get_otrigger(host, owner, actor);
        }
        (AttachType::Obj, Some(7)) => {
            d::drop_otrigger(host, owner, actor);
        }
        (AttachType::Obj, Some(8)) => {
            d::give_otrigger(host, owner, actor, world.guard);
        }
        (AttachType::Obj, Some(9)) => {
            d::wear_otrigger(host, owner, actor, 5);
        }
        (AttachType::Obj, Some(11)) => {
            d::remove_otrigger(host, owner, actor);
        }
        (AttachType::Obj, Some(13)) => d::load_otrigger(host, owner),
        (AttachType::Obj, Some(15)) => {
            d::cast_otrigger(host, actor, Some(owner), spell);
        }
        (AttachType::Obj, Some(16)) => {
            d::leave_otrigger(host, room, actor, 0);
        }
        (AttachType::Obj, Some(18)) => {
            d::consume_otrigger(host, owner, actor, dg_vm::types::OCMD_EAT);
        }
        (AttachType::Obj, Some(19)) => d::time_otrigger(host, owner),
        (AttachType::Wld, Some(1)) => d::random_wtrigger(host, owner),
        (AttachType::Wld, Some(2)) => {
            d::command_wtrigger(host, actor, &command, &world.arg);
        }
        (AttachType::Wld, Some(3)) => d::speech_wtrigger(host, actor, &speech),
        (AttachType::Wld, Some(5)) => d::reset_wtrigger(host, owner),
        (AttachType::Wld, Some(6)) => {
            d::enter_wtrigger(host, owner, actor, 0);
        }
        (AttachType::Wld, Some(7)) => {
            d::drop_wtrigger(host, world.object, actor);
        }
        (AttachType::Wld, Some(15)) => {
            d::cast_wtrigger(host, actor, Some(world.guard), None, spell);
        }
        (AttachType::Wld, Some(16)) => {
            d::leave_wtrigger(host, owner, actor, 0);
        }
        (AttachType::Wld, Some(17)) => {
            d::door_wtrigger(host, actor, 0, 0);
        }
        (AttachType::Wld, Some(18)) => {
            d::login_wtrigger(host, owner, actor);
        }
        (AttachType::Wld, Some(19)) => d::time_wtrigger(host, owner),
        _ => return false,
    }
    host.rolls.clear();
    let ran = host
        .script(owner)
        .and_then(|script| script.trigger(world.id))
        .is_none_or(|trigger| trigger.run.loops != NOT_RUN);
    if !ran {
        mark(world, 0);
    }
    ran
}

/// First whitespace-delimited word of `text`, or `fallback` when the text is
/// empty or a wildcard.
fn first_word(text: &str, fallback: &str) -> String {
    match text.split_whitespace().next() {
        Some(word) if word != "*" => word.to_owned(),
        _ => fallback.to_owned(),
    }
}

/// What one record's run left behind.
struct Outcome {
    vnum: i32,
    logs: Vec<String>,
    unsupported: Vec<String>,
    commands: Vec<String>,
    resumes: usize,
    problems: Vec<String>,
    /// Whether a dispatch function ran the trigger (otherwise it was fired by
    /// hand).
    dispatched: bool,
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
    // What no dispatch function can fire is run by hand with the event's
    // variables set.
    let dispatched = fire_by_dispatch(&mut world, record);
    if !dispatched {
        let vars: Vec<(&str, String)> = world
            .vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        set_event_vars(&mut world.host, world.owner, world.id, &vars);
        script_driver(&mut world.host, world.owner, world.id, Mode::New);
    }
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
        dispatched,
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
    // The argument a C format prints in trailing parentheses (`(arg == %s)`,
    // `(loading mob vnum %d to room %s)`) is the script's text; the driver's
    // `(error N)` is part of the message.
    let collapsed;
    if let Some(at) = rest.rfind(" (")
        && rest.ends_with(')')
        && !rest[at..].starts_with(" (error ")
        && !literal
    {
        collapsed = format!("{} (..)", &rest[..at]);
        rest = &collapsed;
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
    // The names the C formats print in parentheses (`victim (%s) does not
    // exist`, `location not found (%s)`) are the script's text, not part of
    // the message.
    for marker in ["victim (", "location not found ("] {
        if let Some(at) = out.find(marker) {
            let start = at + marker.len();
            if let Some(end) = out[start..].find(')') {
                out.replace_range(start..start + end, "..");
            }
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

/// Triggers whose log lines come from something the mock world cannot give
/// them, with the cause: these are listed apart from the script and VM
/// messages.
const HARNESS_LIMITS: &[(i32, &str)] = &[
    (
        1911,
        "purges a corpse that nothing in the mock world creates",
    ),
    (
        2750,
        "`attach %arg%` needs a trigger vnum and a target id in the player's words",
    ),
];

fn harness_limit(vnum: i32) -> Option<&'static str> {
    HARNESS_LIMITS
        .iter()
        .find(|(limited, _)| *limited == vnum)
        .map(|(_, cause)| *cause)
}

fn top<K: Clone + Ord>(counts: &BTreeMap<K, usize>, n: usize) -> Vec<(K, usize)> {
    let mut rows: Vec<(K, usize)> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(n);
    rows
}

#[test]
fn log_shapes_keep_distinct_c_messages_apart() {
    // Different formats of script_driver stay different, as do the error
    // numbers of one format.
    let distinct = [
        "Trigger VNum 4601 has 'else' without 'if'.",
        "Trigger VNum 4601 has 'end' without 'if'.",
        "Trigger VNum 1385 has 'if' without 'end'. (error 2)",
        "Trigger VNum 1338 has 'if' without 'end'. (error 5)",
        "Mob (a, VNum 3000):: mteleport: victim (visconti) does not exist",
        "Mob (a, VNum 3000):: mteleport target is an invalid room",
        "Room 3001 :: wat: location not found (2301)",
        "Room 3001 :: wat: invalid location",
    ];
    let shapes: BTreeSet<String> = distinct.iter().map(|m| log_shape(m)).collect();
    assert_eq!(shapes.len(), distinct.len(), "{shapes:?}");

    // One format with different arguments is one shape, whichever way the C
    // spells the prefix.
    let same = [
        "Trigger: a, VNum 12, type: 1. unknown object field: 'inventory'",
        "Trigger: b c, VNum 3. unknown object field: 'x'y'",
    ];
    assert_eq!(log_shape(same[0]), log_shape(same[1]));
    let same = [
        "Mob (a, VNum 3000):: msend: victim (sword) does not exist",
        "Mob (a, VNum 3000):: msend: victim (Rumble) does not exist",
    ];
    assert_eq!(log_shape(same[0]), log_shape(same[1]));
    // The text in trailing parentheses is the script's, with or without an
    // apostrophe in the format or the text.
    let same = [
        "Mob (a, VNum 3000):: mdoor: invalid door target (arg == north)",
        "Mob (a, VNum 3000):: mdoor: invalid door target (arg == 2 o'clock)",
    ];
    assert_eq!(log_shape(same[0]), log_shape(same[1]));
    let same = [
        "Mob (a, VNum 3000):: mload: room target vnum doesn't exist (loading mob vnum 5 to room x)",
        "Mob (a, VNum 3000):: mload: room target vnum doesn't exist (loading mob vnum 77 to room it's)",
    ];
    assert_eq!(log_shape(same[0]), log_shape(same[1]));
    assert_ne!(
        log_shape("Mob (a, VNum 3000):: mdoor: invalid door target (arg == x)"),
        log_shape("Mob (a, VNum 3000):: mdoor: invalid door direction (arg == x)"),
    );
    let same = [
        "Trigger: x, Vnum 1926, type 0. Trying to access Global var list of void. Apparently this has not been set up!",
        "Trigger: y z, Vnum 7, type 0. Trying to access Global var list of void. Apparently this has not been set up!",
    ];
    assert_eq!(log_shape(same[0]), log_shape(same[1]));
}

#[test]
fn objects_are_placed_where_their_event_happens() {
    let bits = |letters: &str| dg_vm::types::trig::asciiflag_conv(letters);
    // get_otrigger: the object lies in the room; drop, give, wear and consume
    // are offered an object the actor carries; remove one the actor wears.
    assert!(matches!(object_place(bits("g"), 100, "", 1), Place::Room));
    for letters in ["h", "i", "j", "s"] {
        assert!(matches!(
            object_place(bits(letters), 100, "", 0),
            Place::Inventory
        ));
    }
    assert!(matches!(object_place(bits("l"), 100, "", 0), Place::Worn));
    // cmd_otrig: narg 1 worn, 2 carried, 4 in the room.
    assert!(matches!(object_place(bits("c"), 1, "", 0), Place::Worn));
    assert!(matches!(
        object_place(bits("c"), 2, "", 0),
        Place::Inventory
    ));
    assert!(matches!(object_place(bits("c"), 4, "", 0), Place::Room));
    // A timer or random object is where its script reads its holder from.
    let timer = bits("f");
    assert!(matches!(
        object_place(timer, 5, "eval actor %self.worn_by%", 0),
        Place::Worn
    ));
    assert!(matches!(
        object_place(timer, 5, "set actor %self.carried_by%", 0),
        Place::Inventory
    ));
}

#[test]
fn a_run_teaches_the_next_one_what_the_script_addresses() {
    let mut needs = Needs::default();
    let logs = vec![
        "Obj (a, VNum 3031):: oteleport target is an invalid room".to_owned(),
        "Mob (a, VNum 3000):: msend: victim (Rumble) does not exist".to_owned(),
    ];
    assert!(harvest(&logs, &mut needs));
    assert!(needs.arg_is_room);
    assert!(needs.people.contains("Rumble"));
    assert!(!harvest(&logs, &mut needs));
}

#[test]
fn constants_a_script_computes_are_found() {
    let body = "eval num %random.99% + 2300\nset zone 211\nmgoto %zone%02\n\
                eval present %random.27%\neval present2 %present% * 1000\n\
                eval present3 %present2% + %random.4%\n* 777\nset zero 0\n";
    let found = candidate_numbers(body);
    // The first roll is 1.
    for number in [2300, 2301, 211, 21102, 1000, 1001] {
        assert!(found.contains(&number), "{number} in {found:?}");
    }
    assert!(!found.contains(&777), "a comment is not read");
    assert!(!found.contains(&0), "0 is the void");
    assert_eq!(evaluate_constants("2 + 3 * 4 - 8 / 2"), Some(10));
    assert_eq!(evaluate_constants("1 / 0"), None);
    assert_eq!(evaluate_constants("x + 1"), None);
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
    let mut limited: BTreeMap<(i32, &str), usize> = BTreeMap::new();
    let mut total_commands = 0;
    let mut resumes = 0;
    for outcome in outcomes {
        resumes += outcome.resumes;
        for message in &outcome.logs {
            if let Some(cause) = harness_limit(outcome.vnum) {
                *limited.entry((outcome.vnum, cause)).or_default() += 1;
                continue;
            }
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
    let dispatched = outcomes.iter().filter(|o| o.dispatched).count();
    println!(
        "fired through dispatch: {dispatched}  by hand (no dispatch function ran them): {}",
        outcomes.len() - dispatched
    );
    println!(
        "script_log messages: {total_logs} in {} shapes",
        shapes.len()
    );
    if !limited.is_empty() {
        println!("harness limits (messages the mock world causes, by trigger):");
        for ((vnum, cause), count) in &limited {
            println!("  {count:5}  trigger {vnum}: {cause}");
        }
    }
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
    if !gaps.is_empty() {
        println!(
            "harness gaps (rooms, objects and mobs a script computes that the harness does not create):"
        );
        for (shape, count) in top(&gaps, 100) {
            println!("  {count:5}  {shape}  [e.g. {}]", shape_sample[&shape]);
        }
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
