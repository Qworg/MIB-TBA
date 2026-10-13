//! Static analysis over the whole trigger corpus.
//!
//! The expected numbers were counted independently of `analyze`: a line scan
//! of the `.trg` bodies for the first word of each line.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::corpus::{self, EXPECTED_RECORDS, Record};
use dg_vm::analyze::{Analysis, Corpus, LineClass, StructuralErrorKind, analyze};
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
    // Found with one stack of `if` / `while` / `switch` openers per body, a
    // closer pairing with the innermost opener of its kind, in a line scan
    // written apart from `analyze`. Balancing the counts of each kind alone
    // finds 13 of these; trigger 1106 (`switch` where an `if` was meant) and
    // 1133 (a `while` and an `if` closed in the wrong order) need the nesting.
    let vnums: Vec<i32> = corpus.structural.keys().copied().collect();
    assert_eq!(
        vnums,
        vec![
            1106, 1133, 1292, 1324, 1338, 1385, 1939, 1959, 2737, 4601, 8315, 13005, 21101, 34302,
            55513
        ]
    );
    assert_eq!(corpus.totals.broken_scripts, 15);
    let at = |vnum: i32| -> Vec<(usize, StructuralErrorKind)> {
        corpus.structural[&vnum]
            .iter()
            .map(|e| (e.line, e.kind))
            .collect()
    };
    use StructuralErrorKind::{
        DoneWithoutLoop, ElseWithoutIf, EndWithoutIf, IfWithoutEnd, SwitchWithoutDone,
        WhileWithoutDone,
    };
    assert_eq!(at(1338), [(1, IfWithoutEnd)]);
    // The `end` at the bottom closes the inner block; the `if` at 18 is open.
    assert_eq!(at(1324), [(18, IfWithoutEnd)]);
    assert_eq!(at(1385), [(1, IfWithoutEnd), (3, IfWithoutEnd)]);
    assert_eq!(
        at(1133),
        [(9, IfWithoutEnd), (105, ElseWithoutIf), (107, EndWithoutIf)]
    );
    assert_eq!(at(1292), [(2, WhileWithoutDone)]);
    assert_eq!(at(34302), [(3, SwitchWithoutDone)]);
    assert_eq!(at(21101), [(4, SwitchWithoutDone), (29, EndWithoutIf)]);
    assert_eq!(at(1959), [(11, DoneWithoutLoop)]);
    assert_eq!(at(13005), [(184, DoneWithoutLoop)]);
    assert_eq!(at(4601), [(8, ElseWithoutIf), (10, EndWithoutIf)]);
    assert_eq!(at(8315), [(7, EndWithoutIf)]);
}

/// Counts from a line scan of the `.trg` bodies written apart from `analyze`
/// (first word of each line; the tables of `interpreter.c` and the
/// substitution rules of `var_subst` ported separately), and the names of
/// the tables from the lists the modules export.
#[test]
fn corpus_commands_and_references_match_independent_counts() {
    let Some(records) = corpus::load_corpus() else {
        corpus::skip_message("corpus_commands_and_references_match_independent_counts");
        return;
    };
    let totals = analyse_corpus(&records).totals;

    for (class, expected) in [
        (LineClass::Control, 8085),
        (LineClass::Statement, 8247),
        (LineClass::ScriptCommand, 4985),
        (LineClass::UnknownCommand, 45),
        (LineClass::MudCommand, 2681),
        (LineClass::DynamicCommand, 71),
        (LineClass::OverLength, 0),
    ] {
        assert_eq!(
            totals.classes.get(&class).copied().unwrap_or(0),
            expected,
            "{class:?}"
        );
    }
    for (name, expected) in [
        ("break", 1195),
        ("case", 1509),
        ("default", 137),
        ("done", 286),
    ] {
        assert_eq!(count(&totals.controls, name), expected, "{name}");
    }
    for (name, expected) in [("else", 723), ("end", 1745)] {
        assert_eq!(count(&totals.controls, name), expected, "{name}");
    }
    for (name, expected) in [
        ("return", 360),
        ("rdelete", 135),
        ("nop", 106),
        ("unset", 38),
        ("extract", 18),
        ("context", 13),
        ("makeuid", 2),
    ] {
        assert_eq!(count(&totals.statements, name), expected, "{name}");
    }

    // Script commands, the command variables expanded: `%send%` and `msend`
    // are both msend for a mob, and so on per owner type.
    for (name, expected) in [
        ("msend", 279),
        ("mecho", 430),
        ("mload", 379),
        ("osend", 637),
        ("oechoaround", 226),
        ("wecho", 621),
        ("wdoor", 290),
        ("wteleport", 87),
        ("osetval", 5),
        ("otimer", 9),
    ] {
        assert_eq!(count(&totals.script_commands, name), expected, "{name}");
    }
    assert_eq!(totals.script_commands.values().sum::<usize>(), 4985);

    // Mob lines for the game. A word that does not start with a letter is
    // the one character the interpreter takes.
    assert_eq!(totals.mud_commands.values().sum::<usize>(), 2681);
    assert_eq!(totals.mud_commands.len(), 139);
    assert_eq!(count(&totals.mud_commands, "say"), 1436);
    assert_eq!(count(&totals.mud_commands, "'"), 3);
    assert!(
        totals
            .mud_commands
            .keys()
            .all(|w| w.starts_with(|c: char| c.is_ascii_alphabetic()) || w.chars().count() == 1)
    );

    assert_eq!(totals.unknown_object_commands.values().sum::<usize>(), 16);
    assert_eq!(totals.unknown_world_commands.values().sum::<usize>(), 29);
    assert_eq!(count(&totals.unknown_object_commands, "load"), 3);
    assert_eq!(count(&totals.unknown_world_commands, "tobacco,"), 2);
    // Room trigger 332's first line escapes its percent signs.
    assert_eq!(count(&totals.unknown_world_commands, "%send%"), 1);

    // References: every `%...%` of a command line or a condition, nested
    // ones included, and one field per step of each chain.
    assert_eq!(totals.references, 17437);
    assert_eq!(totals.field_references, 5545);
    assert_eq!(totals.char_fields.values().sum::<usize>(), 4393);
    assert_eq!(totals.obj_fields.values().sum::<usize>(), 3356);
    assert_eq!(totals.room_fields.values().sum::<usize>(), 2730);
    assert_eq!(totals.text_functions.values().sum::<usize>(), 443);
    assert_eq!(totals.unknown_fields.values().sum::<usize>(), 83);
    assert_eq!(totals.unknown_fields.len(), 34);
    assert_eq!(totals.global_reads, 0);
    assert_eq!(totals.heads_unknown.values().sum::<usize>(), 284);
    assert_eq!(totals.heads_unknown.len(), 166);
    // `eval people %room.people%` makes `%people.name%` a character field.
    assert!(count(&totals.char_fields, "himher") >= 20);
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
        // A table may answer a name only for some subfields.
        for sub in ["", "1", "on", "off", "abc", "0"] {
            let mut cx = Cx {
                host: &mut host,
                owner: mob,
                attach: AttachType::Mob,
                trig: &mut trig,
            };
            assert_eq!(char_field(&mut cx, mob, name, sub), None, "{name}");
            assert_eq!(char_field(&mut cx, pc, name, sub), None, "{name}");
            assert_eq!(obj_field(&mut cx, item, name, sub), None, "{name}");
            assert_eq!(room_field(&mut cx, room, name, sub), None, "{name}");
        }
    }
}
