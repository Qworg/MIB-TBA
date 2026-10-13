//! Static analysis over the whole trigger corpus.
//!
//! The expected numbers were counted independently of `analyze`: a line scan
//! of the `.trg` bodies for the first word of each line.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::corpus::{self, EXPECTED_RECORDS, Record};
use dg_vm::analyze::{Analysis, Corpus, LineClass, analyze};
use dg_vm::fields_char::char_field;
use dg_vm::fields_obj::obj_field;
use dg_vm::fields_room::room_field;
use dg_vm::host::Cx;
use dg_vm::mock::MockHost;
use dg_vm::script::Program;
use dg_vm::text::text_processed;
use dg_vm::types::{AttachType, TrigInstance};

fn attach(kind: i32) -> AttachType {
    match kind {
        0 => AttachType::Mob,
        1 => AttachType::Obj,
        _ => AttachType::Wld,
    }
}

fn analyse_corpus(records: &[Record]) -> Corpus {
    let mut corpus = Corpus::default();
    for r in records {
        corpus.add(r.vnum, &analyze(&r.body, attach(r.attach_type)));
    }
    corpus
}

fn count(map: &std::collections::BTreeMap<String, usize>, key: &str) -> usize {
    map.get(key).copied().unwrap_or(0)
}

#[test]
fn corpus_totals_match_independent_counts() {
    let Some(records) = corpus::load_corpus() else {
        corpus::skip_message("corpus_totals_match_independent_counts");
        return;
    };
    assert_eq!(records.len(), EXPECTED_RECORDS);
    let totals = analyse_corpus(&records).totals;
    assert_eq!(totals.scripts, EXPECTED_RECORDS);

    // Non-empty lines, split on either terminator, summed over the bodies.
    assert_eq!(totals.lines, 26308);
    assert_eq!(totals.classes.values().sum::<usize>(), totals.lines);
    assert_eq!(totals.classes[&LineClass::Comment], 1799);

    for (name, expected) in [
        ("if", 1749),
        ("elseif", 454),
        ("while", 126),
        ("switch", 161),
    ] {
        assert_eq!(count(&totals.controls, name), expected, "{name}");
    }
    for (name, expected) in [
        ("wait", 1933),
        ("eval", 1073),
        ("set", 3845),
        ("remote", 178),
        ("halt", 168),
        ("global", 338),
        ("detach", 26),
        ("attach", 14),
        ("dg_cast", 336),
        ("dg_affect", 59),
    ] {
        assert_eq!(count(&totals.statements, name), expected, "{name}");
    }
    assert_eq!(totals.classes[&LineClass::Magic], 336 + 59);
}

#[test]
fn every_line_of_every_record_lands_in_exactly_one_class() {
    let Some(records) = corpus::load_corpus() else {
        corpus::skip_message("every_line_of_every_record_lands_in_exactly_one_class");
        return;
    };
    for r in &records {
        let a: Analysis = analyze(&r.body, attach(r.attach_type));
        let lines = Program::parse(&r.body).lines.len();
        assert_eq!(a.lines, lines, "record {}", r.vnum);
        assert_eq!(
            a.classes.values().sum::<usize>(),
            lines,
            "record {}",
            r.vnum
        );
        // The per-name maps hold exactly the lines of their class.
        let named = |classes: &[LineClass]| {
            classes
                .iter()
                .map(|c| a.classes.get(c).copied().unwrap_or(0))
                .sum::<usize>()
        };
        assert_eq!(
            a.controls.values().sum::<usize>(),
            named(&[LineClass::Control])
        );
        assert_eq!(
            a.statements.values().sum::<usize>(),
            named(&[LineClass::Statement, LineClass::Magic])
        );
        assert_eq!(
            a.unknown_object_commands.values().sum::<usize>()
                + a.unknown_world_commands.values().sum::<usize>(),
            named(&[LineClass::UnknownCommand])
        );
        assert_eq!(
            a.mud_commands.values().sum::<usize>(),
            named(&[LineClass::MudCommand])
        );
        assert_eq!(
            a.script_commands.values().sum::<usize>(),
            named(&[LineClass::ScriptCommand])
        );
    }
}

#[test]
fn the_broken_scripts_are_named_with_their_errors() {
    let Some(records) = corpus::load_corpus() else {
        corpus::skip_message("the_broken_scripts_are_named_with_their_errors");
        return;
    };
    let corpus = analyse_corpus(&records);
    // Found by balancing `if`/`end` and `while`+`switch`/`done` per body with
    // a separate line scan.
    let vnums: Vec<i32> = corpus.structural.keys().copied().collect();
    assert_eq!(
        vnums,
        vec![
            1292, 1324, 1338, 1385, 1939, 1959, 2737, 4601, 8315, 13005, 21101, 34302, 55513
        ]
    );
    assert_eq!(corpus.totals.broken_scripts, 13);
    assert!(
        corpus
            .structural
            .values()
            .flatten()
            .all(|e| !e.kind.describe().is_empty())
    );
}

/// A field name that no table lists must be refused by every table.
#[test]
fn names_outside_the_lists_are_unknown_to_the_tables() {
    let Some(records) = corpus::load_corpus() else {
        corpus::skip_message("names_outside_the_lists_are_unknown_to_the_tables");
        return;
    };
    let unknown = analyse_corpus(&records).totals.unknown_fields;
    assert!(!unknown.is_empty());

    let mut host = MockHost::new();
    let room = host.add_room(3001, "Temple");
    let mob = host.add_mob(3000, "the guard");
    let pc = host.add_player("Bob");
    let item = host.add_obj(3020, "a sword");
    host.char_to_room(mob, room);
    host.char_to_room(pc, room);
    let proto = host.add_proto(1, AttachType::Mob, "g", 100, "", "nop");
    let mut trig = TrigInstance::new(1, proto);
    for name in unknown.keys() {
        assert!(text_processed(&host, name, "", "abc").is_none(), "{name}");
        let mut cx = Cx {
            host: &mut host,
            owner: mob,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        assert_eq!(char_field(&mut cx, mob, name, ""), None, "{name}");
        assert_eq!(char_field(&mut cx, pc, name, ""), None, "{name}");
        assert_eq!(obj_field(&mut cx, item, name, ""), None, "{name}");
        assert_eq!(room_field(&mut cx, room, name, ""), None, "{name}");
    }
}
