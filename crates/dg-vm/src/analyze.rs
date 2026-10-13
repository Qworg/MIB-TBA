//! Static analysis of a script body: which constructs it uses and which of
//! them the VM does not implement. The converter publishes this as the corpus
//! coverage report.
//!
//! [`analyze`] classifies every line of a body the way
//! [`crate::driver::script_driver`] treats it, without running anything:
//!
//! - a comment (`*`) or a blank line;
//! - a control line (`if`, `elseif`, `else`, `end`, `while`, `done`,
//!   `switch`, `case`, `break`, and the `default` label that `find_case`
//!   looks for), tested in the driver's order ([`CONTROL_KEYWORDS`]);
//! - one of the driver's own statements ([`STATEMENT_KEYWORDS`]), or
//!   `dg_cast` / `dg_affect`;
//! - otherwise a line for the owner's command interpreter. A mob owner's
//!   interpreter is [`MOB_SCRIPT_COMMANDS`], matched on the whole word, and
//!   anything it does not know goes to the game's command interpreter (a mud
//!   command). An object or room owner's interpreter is [`OBJ_CMD_INFO`] /
//!   [`WLD_CMD_INFO`], matched on a prefix of the entry as the C does, and
//!   anything else is an unknown command that the C logs.
//!
//! The driver substitutes `%…%` before it tests statements and commands, so a
//! line led by a command variable (`%send% %actor% hi`) is classified by the
//! command it expands to for the owner's type ([`COMMAND_VARIABLES`]). A line
//! whose first word cannot be known without running the script is a dynamic
//! command.
//!
//! Every `%var.field(subfield)%` reference is scanned with the rules of
//! `var_subst` (`%%` is a literal percent sign, a `(…)` subfield may hold
//! further references, a dotted chain names one field per dot). Field names
//! are classified against the text functions and the character, object and
//! room tables; a name in none of them is "unknown or global": it may still be
//! a script global on that entity.
//!
//! Control-flow errors are those the driver's scans would hit at run time: an
//! `if` whose `find_end` runs off the script, a `while` without a `done`, and
//! an `else`, `end` or `done` with nothing to close.
//!
//! # Simplifications
//!
//! - The scan decides statically what the C decides with data. A dotted chain
//!   is taken to continue (the C stops extending it when a step substitutes to
//!   nothing); the paren depth of a line is carried across its references as
//!   in the C, but the subfield buffer is not.
//! - A line of [`MAX_INPUT_LENGTH`] bytes or more is refused by `var_subst` at
//!   run time; it is counted ([`Analysis::over_length_lines`]) and its
//!   references are not scanned.
//! - `case` labels are compared raw by `find_case`, never substituted, so they
//!   hold no references. `default` reached by falling through a `case` is
//!   dispatched as a command by the driver; it is counted as the label it
//!   usually is.
//! - A variable is "assigned" when any line of the script assigns it, in any
//!   order.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::commands::mob::MOB_SCRIPT_COMMANDS;
use crate::commands::obj::OBJ_CMD_INFO;
use crate::commands::strncmp_lookup;
use crate::commands::wld::WLD_CMD_INFO;
use crate::cstr::{
    any_one_arg, half_chop, one_argument, skip_spaces, starts_with_ci, two_arguments,
};
use crate::dispatch::EVENT_VARIABLES;
use crate::driver::{CONTROL_KEYWORDS, STATEMENT_KEYWORDS, find_end, scan_long, scan_until};
use crate::fields_char;
use crate::fields_obj;
use crate::fields_room;
use crate::mock::MockHost;
use crate::script::{Line, Program};
use crate::subst::{COMMAND_VARIABLES, SPECIAL_VARIABLES};
use crate::text::TEXT_FUNCTIONS;
use crate::types::{AttachType, MAX_INPUT_LENGTH};

/// What a line is, as `script_driver` treats it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LineClass {
    /// `*` comment.
    Comment,
    /// Only whitespace.
    Blank,
    /// `if`, `elseif`, `else`, `end`, `while`, `done`, `switch`, `case`,
    /// `break` or the `default` label.
    Control,
    /// A statement of the driver ([`STATEMENT_KEYWORDS`]).
    Statement,
    /// `dg_cast` or `dg_affect`.
    Magic,
    /// A command of the owner's script command table.
    ScriptCommand,
    /// An object or room owner's line that its command table does not know.
    UnknownCommand,
    /// A mob owner's line that is not a script command: the game's command
    /// interpreter runs it.
    MudCommand,
    /// A line whose first word is a variable that cannot be resolved without
    /// running the script.
    DynamicCommand,
}

impl LineClass {
    /// Every class, in report order.
    pub const ALL: [LineClass; 9] = [
        LineClass::Comment,
        LineClass::Blank,
        LineClass::Control,
        LineClass::Statement,
        LineClass::Magic,
        LineClass::ScriptCommand,
        LineClass::UnknownCommand,
        LineClass::MudCommand,
        LineClass::DynamicCommand,
    ];

    /// The class's name in the report.
    pub fn name(self) -> &'static str {
        match self {
            LineClass::Comment => "comment",
            LineClass::Blank => "blank",
            LineClass::Control => "control",
            LineClass::Statement => "statement",
            LineClass::Magic => "magic",
            LineClass::ScriptCommand => "script_command",
            LineClass::UnknownCommand => "unknown_command",
            LineClass::MudCommand => "mud_command",
            LineClass::DynamicCommand => "dynamic_command",
        }
    }
}

/// A control-flow error the driver's scans hit at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StructuralErrorKind {
    /// `find_end` finds no `end` for this `if`. `error` is the number in the
    /// driver's log line (`(error 1)` is an `if` on the last line, `(error 2)`
    /// a scan that runs off the script).
    IfWithoutEnd { error: u8 },
    /// No `done` closes this `while`: the driver's `find_done` answers with
    /// the last line (or, in a nest, past it, which is the logged error).
    WhileWithoutDone,
    /// No `done` closes this `switch`.
    SwitchWithoutDone,
    /// An `else` or `elseif` outside any `if` block.
    ElseWithoutIf,
    /// An `end` outside any `if` block.
    EndWithoutIf,
    /// A `done` outside any `while` or `switch` block.
    DoneWithoutLoop,
}

impl StructuralErrorKind {
    /// The error in words, as the report names it.
    pub fn describe(self) -> String {
        match self {
            StructuralErrorKind::IfWithoutEnd { error } => {
                format!("'if' without 'end' (error {error})")
            }
            StructuralErrorKind::WhileWithoutDone => "'while' without 'done'".to_owned(),
            StructuralErrorKind::SwitchWithoutDone => "'switch' without 'done'".to_owned(),
            StructuralErrorKind::ElseWithoutIf => "'else' without 'if'".to_owned(),
            StructuralErrorKind::EndWithoutIf => "'end' without 'if'".to_owned(),
            StructuralErrorKind::DoneWithoutLoop => "'done' without 'while'".to_owned(),
        }
    }
}

/// One control-flow error and the line it is found at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StructuralError {
    /// 1-based, among the lines [`Program::parse`] keeps.
    pub line: usize,
    pub kind: StructuralErrorKind,
}

/// What a script (or, merged, a corpus) uses. Every map is keyed by lowercase
/// names; counts are occurrences.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Scripts analysed (1 for [`analyze`]).
    pub scripts: usize,
    /// Lines, as [`Program::parse`] splits them. Equals the sum of
    /// [`Analysis::classes`].
    pub lines: usize,
    /// Lines by class; a class with no lines is absent.
    pub classes: BTreeMap<LineClass, usize>,
    /// Control lines by keyword (`if`, `elseif`, …, `default`).
    pub controls: BTreeMap<String, usize>,
    /// Driver statements by name, `dg_cast` and `dg_affect` included.
    pub statements: BTreeMap<String, usize>,
    /// `wait` lines by form: `pulses`, `seconds`, `ticks`, `until`,
    /// `dynamic` (the amount is a variable) and `invalid` (the driver logs
    /// and leaves the trigger stuck).
    pub wait_forms: BTreeMap<String, usize>,
    /// Script commands by name (`mecho`, `oload`, `wteleport`, …), including
    /// those a command variable expands to.
    pub script_commands: BTreeMap<String, usize>,
    /// Object-owned lines no object command accepts, by first word.
    pub unknown_object_commands: BTreeMap<String, usize>,
    /// Room-owned lines no world command accepts, by first word.
    pub unknown_world_commands: BTreeMap<String, usize>,
    /// Mob-owned lines that go to the game's command interpreter, by first
    /// word.
    pub mud_commands: BTreeMap<String, usize>,
    /// Lines of [`MAX_INPUT_LENGTH`] bytes or more, which `var_subst`
    /// refuses.
    pub over_length_lines: usize,
    /// Control-flow errors, in line order within each script.
    pub structural: Vec<StructuralError>,
    /// Scripts with at least one control-flow error.
    pub broken_scripts: usize,
    /// `%…%` references (nested ones included).
    pub references: usize,
    /// References whose name has whitespace in it, so it can be no variable:
    /// a percent sign in prose such as `50% of`, which `var_subst` replaces
    /// with nothing.
    pub text_percents: usize,
    /// Field names in references (each step of a dotted chain counts).
    pub field_references: usize,
    /// Text-function references by function name.
    pub text_functions: BTreeMap<String, usize>,
    /// References to a name of the character table, by name. A name that
    /// several tables know is counted in each.
    pub char_fields: BTreeMap<String, usize>,
    /// References to a name of the object table.
    pub obj_fields: BTreeMap<String, usize>,
    /// References to a name of the room table.
    pub room_fields: BTreeMap<String, usize>,
    /// References to a name no table knows ("unknown or global": it may be a
    /// script global of the entity).
    pub unknown_fields: BTreeMap<String, usize>,
    /// Variable heads the VM provides: event variables, the special
    /// variables and the command variables.
    pub heads_provided: BTreeMap<String, usize>,
    /// Variable heads the script assigns itself.
    pub heads_assigned: BTreeMap<String, usize>,
    /// Variable heads that are read but neither provided nor assigned.
    pub heads_unknown: BTreeMap<String, usize>,
}

impl Analysis {
    /// Add `other`'s counts to these: the corpus total is the merge of every
    /// script's analysis.
    pub fn merge(&mut self, other: &Analysis) {
        self.scripts += other.scripts;
        self.lines += other.lines;
        self.over_length_lines += other.over_length_lines;
        self.broken_scripts += other.broken_scripts;
        self.references += other.references;
        self.text_percents += other.text_percents;
        self.field_references += other.field_references;
        self.structural.extend_from_slice(&other.structural);
        merge_counts(&mut self.classes, &other.classes);
        for (mine, theirs) in [
            (&mut self.controls, &other.controls),
            (&mut self.statements, &other.statements),
            (&mut self.wait_forms, &other.wait_forms),
            (&mut self.script_commands, &other.script_commands),
            (
                &mut self.unknown_object_commands,
                &other.unknown_object_commands,
            ),
            (
                &mut self.unknown_world_commands,
                &other.unknown_world_commands,
            ),
            (&mut self.mud_commands, &other.mud_commands),
            (&mut self.text_functions, &other.text_functions),
            (&mut self.char_fields, &other.char_fields),
            (&mut self.obj_fields, &other.obj_fields),
            (&mut self.room_fields, &other.room_fields),
            (&mut self.unknown_fields, &other.unknown_fields),
            (&mut self.heads_provided, &other.heads_provided),
            (&mut self.heads_assigned, &other.heads_assigned),
            (&mut self.heads_unknown, &other.heads_unknown),
        ] {
            merge_counts(mine, theirs);
        }
    }
}

fn merge_counts<K: Ord + Clone>(into: &mut BTreeMap<K, usize>, from: &BTreeMap<K, usize>) {
    for (key, count) in from {
        *into.entry(key.clone()).or_default() += count;
    }
}

fn bump<K: Ord>(map: &mut BTreeMap<K, usize>, key: K) {
    *map.entry(key).or_default() += 1;
}

/// Corpus totals: the merged [`Analysis`] and the scripts (by vnum) behind
/// the findings that name them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Corpus {
    /// The merge of every script added.
    pub totals: Analysis,
    /// The control-flow errors of each broken script, by vnum.
    pub structural: BTreeMap<i32, Vec<StructuralError>>,
    /// Each unknown object command word and the scripts that use it.
    pub unknown_object_vnums: BTreeMap<String, BTreeSet<i32>>,
    /// Each unknown world command word and the scripts that use it.
    pub unknown_world_vnums: BTreeMap<String, BTreeSet<i32>>,
}

impl Corpus {
    /// Add the analysis of script `vnum`.
    pub fn add(&mut self, vnum: i32, analysis: &Analysis) {
        self.totals.merge(analysis);
        if !analysis.structural.is_empty() {
            self.structural
                .entry(vnum)
                .or_default()
                .extend_from_slice(&analysis.structural);
        }
        for (words, vnums) in [
            (
                &analysis.unknown_object_commands,
                &mut self.unknown_object_vnums,
            ),
            (
                &analysis.unknown_world_commands,
                &mut self.unknown_world_vnums,
            ),
        ] {
            for word in words.keys() {
                vnums.entry(word.clone()).or_default().insert(vnum);
            }
        }
    }
}

/// Analyse one script body owned by an entity of type `attach`.
pub fn analyze(body: &str, attach: AttachType) -> Analysis {
    let program = Program::parse(body);
    let lines: &[Line] = &program.lines;
    let assigned = assigned_variables(lines);
    let mut out = Analysis {
        scripts: 1,
        lines: lines.len(),
        ..Analysis::default()
    };
    for line in lines {
        let body = line.body();
        let (class, substituted) = classify(&mut out, body, attach, &assigned);
        bump(&mut out.classes, class);
        if let Some(text) = substituted {
            if text.len() >= MAX_INPUT_LENGTH {
                out.over_length_lines += 1;
            } else {
                note_references(&mut out, text, &assigned);
            }
        }
    }
    out.structural = structure(lines);
    out.broken_scripts = usize::from(!out.structural.is_empty());
    out
}

/// The keyword of `CONTROL_KEYWORDS` that starts `body`, as the driver tests
/// them, trimmed; `default` is the label `find_case` looks for.
fn control_keyword(body: &str) -> Option<&'static str> {
    CONTROL_KEYWORDS
        .iter()
        .find(|keyword| starts_with_ci(body, keyword))
        .map(|keyword| keyword.trim_end())
        .or_else(|| starts_with_ci(body, "default").then_some("default"))
}

/// Classify one line, record what names it, and return the text the driver
/// hands to `var_subst` (none for a line it does not substitute).
fn classify<'a>(
    out: &mut Analysis,
    body: &'a str,
    attach: AttachType,
    assigned: &BTreeSet<String>,
) -> (LineClass, Option<&'a str>) {
    if body.starts_with('*') {
        return (LineClass::Comment, None);
    }
    if body.is_empty() {
        return (LineClass::Blank, None);
    }
    if let Some(keyword) = control_keyword(body) {
        bump(&mut out.controls, keyword.to_owned());
        // The condition of these is substituted; the other keywords carry
        // none, and `case` labels are compared as written.
        let substituted = match keyword {
            "if" | "elseif" | "while" | "switch" => {
                Some(body.get(keyword.len() + 1..).unwrap_or(""))
            }
            _ => None,
        };
        return (LineClass::Control, substituted);
    }

    // The statements are tested on the substituted line; a keyword that
    // matches the line as written matches it after substitution too, since
    // the text before the first `%` does not change.
    if !body.starts_with('%')
        && let Some(keyword) = STATEMENT_KEYWORDS
            .iter()
            .find(|keyword| starts_with_ci(body, keyword))
    {
        let name = keyword.trim_end();
        bump(&mut out.statements, name.to_owned());
        if name == "wait" {
            bump(&mut out.wait_forms, wait_form(body).to_owned());
        }
        let class = if name == "dg_cast" || name == "dg_affect" {
            LineClass::Magic
        } else {
            LineClass::Statement
        };
        return (class, Some(body));
    }

    let Some(word) = command_word(body, attach, assigned) else {
        return (LineClass::DynamicCommand, Some(body));
    };
    let class = match attach {
        AttachType::Mob => {
            if MOB_SCRIPT_COMMANDS
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&word))
            {
                bump(&mut out.script_commands, word);
                LineClass::ScriptCommand
            } else {
                bump(&mut out.mud_commands, word);
                LineClass::MudCommand
            }
        }
        AttachType::Obj => table_command(
            out,
            OBJ_CMD_INFO
                .iter()
                .map(|(name, command)| (*name, command.is_some())),
            word,
            true,
        ),
        AttachType::Wld => table_command(
            out,
            WLD_CMD_INFO
                .iter()
                .map(|(name, command)| (*name, command.is_some())),
            word,
            false,
        ),
    };
    (class, Some(body))
}

/// Look `word` up in an object or world command table as its interpreter
/// does: the first entry `word` is a prefix of. An entry with no function
/// (`RESERVED`) is unknown.
fn table_command<'a>(
    out: &mut Analysis,
    table: impl Iterator<Item = (&'a str, bool)>,
    word: String,
    object: bool,
) -> LineClass {
    let table: Vec<(&str, bool)> = table.collect();
    let names: Vec<&str> = table.iter().map(|(name, _)| *name).collect();
    let found = strncmp_lookup(&names, &word)
        .and_then(|index| table.get(index))
        .filter(|(_, has_function)| *has_function);
    match found {
        Some((name, _)) => {
            bump(&mut out.script_commands, name.trim_end().to_owned());
            LineClass::ScriptCommand
        }
        None => {
            let unknown = if object {
                &mut out.unknown_object_commands
            } else {
                &mut out.unknown_world_commands
            };
            bump(unknown, word);
            LineClass::UnknownCommand
        }
    }
}

/// The first word of a command line as the interpreter will see it, lowercase;
/// `None` when it depends on a variable's value. A leading command variable
/// that the script does not assign expands to a command of the owner's type.
fn command_word(body: &str, attach: AttachType, assigned: &BTreeSet<String>) -> Option<String> {
    if let Some(rest) = body.strip_prefix('%') {
        let name = rest.split('%').next().filter(|_| rest.contains('%'))?;
        let lower = name.to_ascii_lowercase();
        if assigned.contains(&lower) {
            return None;
        }
        let (_, words) = COMMAND_VARIABLES.iter().find(|(var, _)| *var == lower)?;
        let word = words.get(attach.index())?;
        return Some(word.trim_end().to_owned());
    }
    let (word, _) = any_one_arg(body);
    (!word.contains('%')).then_some(word)
}

/// The form of a `wait` line, from the checks of `process_wait`.
fn wait_form(body: &str) -> &'static str {
    let (_, rest) = any_one_arg(body);
    let arg = skip_spaces(rest);
    if arg.is_empty() {
        return "invalid";
    }
    if starts_with_ci(arg, "until ") {
        return match scan_until(arg) {
            (Some(_), _) => "until",
            _ => "invalid",
        };
    }
    if arg.starts_with('%') {
        return "dynamic";
    }
    match scan_long(arg) {
        None => "invalid",
        Some((_, tail)) => match tail
            .trim_start_matches(crate::cstr::is_space)
            .chars()
            .next()
        {
            Some('t') => "ticks",
            Some('s') => "seconds",
            _ => "pulses",
        },
    }
}

/// The names the script assigns: the variable operand of `set`, `eval`,
/// `global`, `makeuid`, `extract`, `dg_letter` and `remote`, parsed as the
/// driver parses them. An operand that is itself a reference is skipped.
fn assigned_variables(lines: &[Line]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in lines {
        let body = line.body();
        let Some(keyword) = STATEMENT_KEYWORDS
            .iter()
            .find(|keyword| starts_with_ci(body, keyword))
        else {
            continue;
        };
        let name = match keyword.trim_end() {
            "set" => two_arguments(body).1,
            "eval" => {
                let (_, rest) = one_argument(body);
                one_argument(rest).0
            }
            "global" | "remote" => any_one_arg(any_one_arg(body).1).0,
            "makeuid" | "extract" | "dg_letter" => half_chop(half_chop(body).1).0,
            _ => continue,
        };
        if !name.is_empty() && !name.contains('%') {
            names.insert(name);
        }
    }
    names
}

/// One `%var.field(subfield)%` as it stands in the text: the lowercased
/// variable name and the field name of each step of the dotted chain.
struct Reference {
    head: String,
    fields: Vec<String>,
}

/// Every reference in `text`, nested ones in subfields included.
fn scan_references(text: &str) -> Vec<Reference> {
    let mut refs = Vec::new();
    let mut pending = vec![text.to_owned()];
    while let Some(next) = pending.pop() {
        scan_level(&next, &mut refs, &mut pending);
    }
    refs
}

/// One `var_subst` pass over `text`: the references at this level, with the
/// text of each non-empty subfield queued in `inner` for a pass of its own.
fn scan_level(text: &str, refs: &mut Vec<Reference>, inner: &mut Vec<String>) {
    // A C string ends at its first NUL.
    let text = text.split('\0').next().unwrap_or("");
    let bytes = text.as_bytes();
    let at = |index: usize| bytes.get(index).copied().unwrap_or(0);
    let slice = |from: usize, to: usize| text.get(from..to).unwrap_or("").to_ascii_lowercase();
    let mut p = 0usize;
    let mut paren_count: i32 = 0;
    while at(p) != 0 {
        // copy until we find the first %
        while at(p) != 0 && at(p) != b'%' {
            p += 1;
        }
        // double %
        if at(p) != 0 {
            p += 1;
            if at(p) == b'%' {
                p += 1;
                continue;
            }
        }
        if at(p) == 0 {
            break;
        }

        // search until end of var or beginning of field
        let var_start = p;
        while at(p) != 0 && at(p) != b'%' && at(p) != b'.' {
            p += 1;
        }
        let head = slice(var_start, p);
        let mut fields = Vec::new();
        let mut subfield: Vec<u8> = Vec::new();
        if at(p) == b'.' {
            p += 1;
            let mut field_start = p;
            // Where the field's name ends: its first `.`, `(` or `)`.
            let mut field_end: Option<usize> = None;
            let mut dots = 0;
            while at(p) != 0 && (at(p) != b'%' || paren_count > 0 || dots > 0) {
                if dots > 0 {
                    // The chain goes on: the step before it is complete and
                    // this character starts the next.
                    fields.push(slice(field_start, field_end.unwrap_or(p)));
                    field_start = p;
                    field_end = None;
                    dots = 0;
                } else if at(p) == b'(' {
                    field_end.get_or_insert(p);
                    paren_count += 1;
                } else if at(p) == b')' {
                    field_end.get_or_insert(p);
                    paren_count -= 1;
                } else if paren_count > 0 {
                    subfield.push(at(p));
                } else if at(p) == b'.' {
                    field_end.get_or_insert(p);
                    dots += 1;
                }
                p += 1;
            }
            fields.push(slice(field_start, field_end.unwrap_or(p)));
        }
        // the closing %
        p += 1;

        if !subfield.is_empty() {
            inner.push(String::from_utf8_lossy(&subfield).into_owned());
        }
        fields.retain(|field| !field.is_empty());
        refs.push(Reference { head, fields });
    }
}

/// Count the references of `text` into `out`.
fn note_references(out: &mut Analysis, text: &str, assigned: &BTreeSet<String>) {
    for reference in scan_references(text) {
        out.references += 1;
        let head = reference.head;
        if head.is_empty() || head.contains(crate::cstr::is_space) {
            out.text_percents += 1;
            continue;
        }
        let special = SPECIAL_VARIABLES.contains(&head.as_str());
        if special
            || EVENT_VARIABLES.contains(&head.as_str())
            || COMMAND_VARIABLES.iter().any(|(name, _)| *name == head)
        {
            bump(&mut out.heads_provided, head.clone());
        } else if assigned.contains(&head) {
            bump(&mut out.heads_assigned, head.clone());
        } else {
            bump(&mut out.heads_unknown, head.clone());
        }
        // The first field of `%random.char%`, `%time.hour%`, `%people.3001%`
        // and the like is the variable's own argument, not an entity field.
        let skip = usize::from(special && head != "self");
        for field in reference.fields.into_iter().skip(skip) {
            note_field(out, field);
        }
    }
}

/// Count a field name under every table that knows it, or as unknown.
fn note_field(out: &mut Analysis, field: String) {
    out.field_references += 1;
    let mut known = false;
    for (names, counts) in [
        (TEXT_FUNCTIONS, &mut out.text_functions),
        (fields_char::FIELD_NAMES, &mut out.char_fields),
        (fields_obj::FIELD_NAMES, &mut out.obj_fields),
        (fields_room::FIELD_NAMES, &mut out.room_fields),
    ] {
        if names.contains(&field.as_str()) {
            bump(counts, field.clone());
            known = true;
        }
    }
    if !known {
        bump(&mut out.unknown_fields, field);
    }
}

/// How many `if` and `while` lines get the driver's own scan to name their
/// error: a script made only of openers would otherwise cost a scan to the end
/// of the script each. The rest are reported with the scan's general answer.
const SCAN_LIMIT: usize = 64;

/// The control-flow errors of a script, in line order.
fn structure(lines: &[Line]) -> Vec<StructuralError> {
    let mut errors = Vec::new();
    // Openers still waiting for their closer, as line indexes.
    let mut open_ifs: Vec<usize> = Vec::new();
    let mut open_loops: Vec<usize> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let body = line.body();
        let at_line = index + 1;
        match control_keyword(body) {
            Some("if") => open_ifs.push(index),
            Some("elseif" | "else") if open_ifs.is_empty() => errors.push(StructuralError {
                line: at_line,
                kind: StructuralErrorKind::ElseWithoutIf,
            }),
            Some("end") => {
                if open_ifs.pop().is_none() {
                    errors.push(StructuralError {
                        line: at_line,
                        kind: StructuralErrorKind::EndWithoutIf,
                    });
                }
            }
            Some("while" | "switch") => open_loops.push(index),
            Some("done") => {
                if open_loops.pop().is_none() {
                    errors.push(StructuralError {
                        line: at_line,
                        kind: StructuralErrorKind::DoneWithoutLoop,
                    });
                }
            }
            _ => {}
        }
    }

    // What is left open has no closer; the driver's scans name the error.
    let mut host = MockHost::new();
    for (checked, &index) in open_ifs.iter().enumerate() {
        let error = if checked < SCAN_LIMIT {
            host.logs.clear();
            find_end(&mut host, 0, lines, index);
            host.logs.first().and_then(|log| error_number(log))
        } else {
            None
        };
        errors.push(StructuralError {
            line: index + 1,
            kind: StructuralErrorKind::IfWithoutEnd {
                error: error.unwrap_or(2),
            },
        });
    }
    // `find_done` answers with the last line when a loop has no `done`, so the
    // driver runs such a loop without a log line; the script is still broken.
    for &index in &open_loops {
        let is_while = lines
            .get(index)
            .is_some_and(|line| starts_with_ci(line.body(), "while "));
        errors.push(StructuralError {
            line: index + 1,
            kind: if is_while {
                StructuralErrorKind::WhileWithoutDone
            } else {
                StructuralErrorKind::SwitchWithoutDone
            },
        });
    }
    errors.sort();
    errors
}

/// The `N` of a driver log line ending `(error N)`.
fn error_number(log: &str) -> Option<u8> {
    let tail = log.rsplit_once("(error ")?.1;
    tail.trim_end_matches(')').parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn mob(body: &str) -> Analysis {
        analyze(body, AttachType::Mob)
    }

    fn class_count(analysis: &Analysis, class: LineClass) -> usize {
        analysis.classes.get(&class).copied().unwrap_or(0)
    }

    fn count(map: &BTreeMap<String, usize>, key: &str) -> usize {
        map.get(key).copied().unwrap_or(0)
    }

    #[test]
    fn every_line_lands_in_exactly_one_class() {
        let a = mob("* note\n   \nif %x%\nset a 1\nmecho hi\nsay hi\n%y% go\ndg_cast 1 x\nend");
        assert_eq!(a.lines, 9);
        assert_eq!(a.classes.values().sum::<usize>(), a.lines);
        for (class, want) in [
            (LineClass::Comment, 1),
            (LineClass::Blank, 1),
            (LineClass::Control, 2),
            (LineClass::Statement, 1),
            (LineClass::ScriptCommand, 1),
            (LineClass::MudCommand, 1),
            (LineClass::DynamicCommand, 1),
            (LineClass::Magic, 1),
        ] {
            assert_eq!(class_count(&a, class), want, "{}", class.name());
        }
    }

    #[test]
    fn control_lines_follow_the_drivers_prefix_tests() {
        let a = mob(
            "if 1\nelseif 2\nelse\nend\nwhile 0\nswitch 1\ncase 1\ndefault\nbreak\ndone\nIF 1\nend",
        );
        for (word, want) in [
            ("if", 2),
            ("elseif", 1),
            ("else", 1),
            ("end", 2),
            ("while", 1),
            ("switch", 1),
            ("case", 1),
            ("default", 1),
            ("break", 1),
            ("done", 1),
        ] {
            assert_eq!(count(&a.controls, word), want, "{word}");
        }
        // The driver tests prefixes: `elsewhere` is an else and `endless` an
        // end, but `if` without its space is a command.
        let a = mob("elsewhere\nendless\nif\nwhile");
        assert_eq!(count(&a.controls, "else"), 1);
        assert_eq!(count(&a.controls, "end"), 1);
        assert_eq!(class_count(&a, LineClass::MudCommand), 2);
        assert_eq!(count(&a.mud_commands, "if"), 1);
    }

    #[test]
    fn statements_are_named_and_magic_is_apart() {
        let a = mob(
            "eval a 1\nnop x\nextract b 1 w\ndg_letter c 1 w\nmakeuid d mob 1\nhalt\n\
             dg_cast 1 x\ndg_affect x y z 1\nglobal a\ncontext 1\nremote a %x%\nrdelete a 1\n\
             return 0\nset e 1\nunset e\nwait 1\nattach 1 x\ndetach 1 x\nHALTED",
        );
        for name in [
            "eval",
            "nop",
            "extract",
            "dg_letter",
            "makeuid",
            "dg_cast",
            "dg_affect",
            "global",
            "context",
            "remote",
            "rdelete",
            "return",
            "set",
            "unset",
            "wait",
            "attach",
            "detach",
        ] {
            assert_eq!(count(&a.statements, name), 1, "{name}");
        }
        // `halt` matches by prefix.
        assert_eq!(count(&a.statements, "halt"), 2);
        assert_eq!(class_count(&a, LineClass::Magic), 2);
        assert_eq!(class_count(&a, LineClass::Statement), 17);
        // A keyword needs its space: a bare `set` is a command.
        let a = mob("set\nwait");
        assert_eq!(class_count(&a, LineClass::MudCommand), 2);
    }

    #[test]
    fn wait_forms() {
        let a = mob(
            "wait 5\nwait 2 s\nwait 1 t\nwait until 14:30\nwait until 1430\nwait %n%\nwait\n\
             wait soon\nwait until noon",
        );
        for (form, want) in [
            ("pulses", 1),
            ("seconds", 1),
            ("ticks", 1),
            ("until", 2),
            ("dynamic", 1),
            ("invalid", 2),
        ] {
            assert_eq!(count(&a.wait_forms, form), want, "{form}");
        }
    }

    #[test]
    fn mob_commands_split_into_script_commands_and_mud_commands() {
        let a = mob("mecho hi\nMLOAD obj 1\nsay hello\nmech x\ngrin\nSay again");
        assert_eq!(count(&a.script_commands, "mecho"), 1);
        assert_eq!(count(&a.script_commands, "mload"), 1);
        // The mob table matches whole words only.
        assert_eq!(count(&a.mud_commands, "mech"), 1);
        assert_eq!(count(&a.mud_commands, "say"), 2);
        assert_eq!(count(&a.mud_commands, "grin"), 1);
        assert_eq!(class_count(&a, LineClass::ScriptCommand), 2);
        assert_eq!(class_count(&a, LineClass::MudCommand), 4);
    }

    #[test]
    fn object_and_room_tables_match_prefixes_and_count_unknowns() {
        let a = analyze(
            "oecho a\noechoaround x\noe y\nmecho no\nosetval 1 2\noset 1 2",
            AttachType::Obj,
        );
        assert_eq!(count(&a.script_commands, "oecho"), 2);
        assert_eq!(count(&a.script_commands, "oechoaround"), 1);
        assert_eq!(count(&a.script_commands, "osetval"), 2);
        assert_eq!(count(&a.unknown_object_commands, "mecho"), 1);
        assert_eq!(class_count(&a, LineClass::UnknownCommand), 1);
        assert!(a.unknown_world_commands.is_empty());

        let a = analyze(
            "wecho a\nwlog x\nwl y\nsay hi\nwlogs z\nreserved q",
            AttachType::Wld,
        );
        assert_eq!(count(&a.script_commands, "wecho"), 1);
        // `wlog` in full is the log command; `wl` selects `wload`.
        assert_eq!(count(&a.script_commands, "wlog"), 1);
        assert_eq!(count(&a.script_commands, "wload"), 1);
        assert_eq!(count(&a.unknown_world_commands, "say"), 1);
        assert_eq!(count(&a.unknown_world_commands, "wlogs"), 1);
        // `RESERVED` has no function.
        assert_eq!(count(&a.unknown_world_commands, "reserved"), 1);
        assert!(a.unknown_object_commands.is_empty());
    }

    #[test]
    fn command_variables_expand_by_owner_type() {
        let body = "%send% %actor% hi\n%echo% x\n%transform% 1\n%move% a";
        let a = mob(body);
        assert_eq!(count(&a.script_commands, "msend"), 1);
        // `%echo%` and a mob's `%move%` are both mecho.
        assert_eq!(count(&a.script_commands, "mecho"), 2);
        assert_eq!(count(&a.script_commands, "mtransform"), 1);
        let a = analyze(body, AttachType::Obj);
        assert_eq!(count(&a.script_commands, "osend"), 1);
        assert_eq!(count(&a.script_commands, "omove"), 1);
        let a = analyze(body, AttachType::Wld);
        assert_eq!(count(&a.script_commands, "wsend"), 1);
        // There is no wtransform: a room's `%transform%` is wecho.
        assert_eq!(count(&a.script_commands, "wecho"), 2);
        assert_eq!(count(&a.script_commands, "wmove"), 1);
        assert_eq!(class_count(&a, LineClass::ScriptCommand), 4);
    }

    #[test]
    fn dynamic_commands_are_those_a_run_would_have_to_resolve() {
        let a =
            mob("%actor.name% waves\n%cmd% x\nsa%x% y\n%unterminated\n%%\nset send 1\n%send% x");
        // `%cmd%` is an event variable, not a command variable; `%%` leads
        // with an escape; the last line's variable is the script's own.
        assert_eq!(class_count(&a, LineClass::DynamicCommand), 6);
        assert_eq!(class_count(&a, LineClass::Statement), 1);
    }

    #[test]
    fn references_are_scanned_with_the_substitution_rules() {
        let a = mob("say %actor.name% 100%% sure %self.hitp% %actor.fighting.name% %%actor%%");
        assert_eq!(a.references, 3);
        assert_eq!(count(&a.heads_provided, "actor"), 2);
        assert_eq!(count(&a.heads_provided, "self"), 1);
        assert_eq!(count(&a.char_fields, "name"), 2);
        assert_eq!(count(&a.char_fields, "hitp"), 1);
        assert_eq!(count(&a.char_fields, "fighting"), 1);
        assert_eq!(a.field_references, 4);
    }

    #[test]
    fn references_nest_inside_subfields() {
        let a = mob("say %actor.inventory(%random.5%).name% %arg.contains(%self.name%)%");
        assert_eq!(a.references, 4);
        assert_eq!(count(&a.heads_provided, "random"), 1);
        assert_eq!(count(&a.heads_provided, "self"), 1);
        assert_eq!(count(&a.heads_provided, "arg"), 1);
        assert_eq!(count(&a.text_functions, "contains"), 1);
        assert_eq!(count(&a.char_fields, "inventory"), 1);
        // `%random.5%`'s field is its argument, not a table name.
        assert!(a.unknown_fields.is_empty(), "{:?}", a.unknown_fields);
    }

    #[test]
    fn field_names_are_classified_by_table() {
        let a = mob(
            "say %arg.strlen% %actor.hitp% %object.cost% %self.north% %actor.gadget% %x.GADGET%",
        );
        assert_eq!(count(&a.text_functions, "strlen"), 1);
        assert_eq!(count(&a.char_fields, "hitp"), 1);
        assert_eq!(count(&a.obj_fields, "cost"), 1);
        assert_eq!(count(&a.room_fields, "north"), 1);
        // Names are compared without regard to case.
        assert_eq!(count(&a.unknown_fields, "gadget"), 2);
        // A name several tables know is counted in each.
        let a = mob("say %actor.name% %actor.vnum%");
        for map in [&a.char_fields, &a.obj_fields, &a.room_fields] {
            assert_eq!(count(map, "name"), 1);
            assert_eq!(count(map, "vnum"), 1);
        }
    }

    #[test]
    fn variable_heads_are_provided_assigned_or_unknown() {
        let a = mob(
            "set mine 1\neval calc 2\nmakeuid uid mob 1\nextract w 1 a b\ndg_letter l 1 x\n\
             global mine\nremote far %actor.id%\n\
             say %mine% %calc% %uid% %w% %l% %far% %stranger% %actor% %send% %time.hour% %Mine%",
        );
        for name in ["mine", "calc", "uid", "w", "l", "far"] {
            let n = if name == "mine" { 2 } else { 1 };
            assert_eq!(count(&a.heads_assigned, name), n, "{name}");
        }
        assert_eq!(count(&a.heads_unknown, "stranger"), 1);
        assert_eq!(a.heads_unknown.len(), 1);
        assert_eq!(count(&a.heads_provided, "actor"), 2);
        for name in ["send", "time"] {
            assert_eq!(count(&a.heads_provided, name), 1, "{name}");
        }
        // `time.hour` names no field; `actor.id` names one.
        assert_eq!(a.field_references, 1);
    }

    #[test]
    fn conditions_are_scanned_but_case_labels_are_not() {
        let a = mob("if %actor.level% > 5\nswitch %arg%\ncase %other%\nbreak\ndone\nend");
        assert_eq!(a.references, 2);
        assert!(a.heads_unknown.is_empty());
        assert_eq!(count(&a.char_fields, "level"), 1);
        assert_eq!(count(&a.heads_provided, "arg"), 1);
    }

    #[test]
    fn percent_signs_in_prose_are_not_variables() {
        let a = mob("say there is a 50% chance of rain, 20% of snow");
        assert_eq!(a.text_percents, 1);
        assert!(a.heads_unknown.is_empty());
    }

    #[test]
    fn over_long_lines_are_counted_and_not_scanned() {
        let long = format!("say {} %actor.name%", "x".repeat(MAX_INPUT_LENGTH));
        let a = mob(&long);
        assert_eq!(a.over_length_lines, 1);
        assert_eq!(a.references, 0);
        assert_eq!(a.lines, 1);
    }

    fn kinds(analysis: &Analysis) -> Vec<(usize, StructuralErrorKind)> {
        analysis
            .structural
            .iter()
            .map(|e| (e.line, e.kind))
            .collect()
    }

    #[test]
    fn well_formed_scripts_have_no_structural_errors() {
        let a = mob(
            "if 1\nif 2\nsay a\nelse\nsay b\nend\nelseif 3\nsay c\nend\n\
             while 0\nswitch 1\ncase 1\nbreak\ndone\ndone",
        );
        assert!(a.structural.is_empty(), "{:?}", a.structural);
        assert_eq!(a.broken_scripts, 0);
    }

    #[test]
    fn an_if_without_an_end_names_the_drivers_error() {
        // Error 2: the scan runs off the script.
        let a = mob("if 1\nsay a\nsay b");
        assert_eq!(
            kinds(&a),
            vec![(1, StructuralErrorKind::IfWithoutEnd { error: 2 })]
        );
        assert_eq!(a.broken_scripts, 1);
        // Error 1: the `if` is the last line.
        let a = mob("say a\nif 1");
        assert_eq!(
            kinds(&a),
            vec![(2, StructuralErrorKind::IfWithoutEnd { error: 1 })]
        );
        // The inner `if` has its end; the outer one does not.
        let a = mob("if 1\nif 2\nend\nsay x");
        assert_eq!(
            kinds(&a),
            vec![(1, StructuralErrorKind::IfWithoutEnd { error: 2 })]
        );
    }

    #[test]
    fn loops_and_stray_keywords_are_named() {
        // `find_done` takes the last line for the `done` of a lone block and
        // the driver goes on without a log; the script is broken all the same.
        let a = mob("while 1\nsay a");
        assert_eq!(kinds(&a), vec![(1, StructuralErrorKind::WhileWithoutDone)]);
        let a = mob("switch 1\ncase 1\nsay a");
        assert_eq!(kinds(&a), vec![(1, StructuralErrorKind::SwitchWithoutDone)]);
        let a = mob("while 1\nwhile 2\nsay a\ndone");
        assert_eq!(kinds(&a), vec![(1, StructuralErrorKind::WhileWithoutDone)]);
        let a = mob("switch 1\nswitch 2\ncase 1\ndone");
        assert_eq!(kinds(&a), vec![(1, StructuralErrorKind::SwitchWithoutDone)]);
        let a = mob("else\nsay a\nend\ndone\nelseif 1");
        assert_eq!(
            kinds(&a),
            vec![
                (1, StructuralErrorKind::ElseWithoutIf),
                (3, StructuralErrorKind::EndWithoutIf),
                (4, StructuralErrorKind::DoneWithoutLoop),
                (5, StructuralErrorKind::ElseWithoutIf),
            ]
        );
        assert_eq!(
            StructuralErrorKind::IfWithoutEnd { error: 5 }.describe(),
            "'if' without 'end' (error 5)"
        );
    }

    #[test]
    fn the_scan_limit_bounds_the_work_on_a_script_of_openers() {
        let body = "if 1\n".repeat(1000);
        let a = mob(&body);
        assert_eq!(a.structural.len(), 1000);
        assert_eq!(a.broken_scripts, 1);
        let body = "while 1\n".repeat(1000);
        assert!(mob(&body).structural.len() >= 999);
    }

    #[test]
    fn merged_analyses_add_up() {
        let mut total = mob("say a\nif 1\nmecho %actor.name%");
        total.merge(&analyze("oecho x\n* c\nfoo", AttachType::Obj));
        assert_eq!(total.scripts, 2);
        assert_eq!(total.lines, 6);
        assert_eq!(total.classes.values().sum::<usize>(), 6);
        assert_eq!(class_count(&total, LineClass::Control), 1);
        assert_eq!(class_count(&total, LineClass::ScriptCommand), 2);
        assert_eq!(count(&total.char_fields, "name"), 1);
        assert_eq!(total.broken_scripts, 1);

        let mut corpus = Corpus::default();
        corpus.add(7, &analyze("foo\nbar", AttachType::Wld));
        corpus.add(3, &analyze("foo\nif 1", AttachType::Wld));
        assert_eq!(corpus.totals.scripts, 2);
        assert_eq!(corpus.unknown_world_vnums["foo"], BTreeSet::from([3, 7]));
        assert_eq!(corpus.unknown_world_vnums["bar"], BTreeSet::from([7]));
        assert_eq!(
            corpus.structural.keys().copied().collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn hostile_text_does_not_panic() {
        let samples = [
            "",
            "%",
            "%%%",
            "%.",
            "%a.",
            "%a.b.",
            "%a.b.%",
            "%a.(((",
            "%a.b(c",
            "%a.b)))%x.y(%z%",
            "%é.ü(ß).ö%",
            "if \u{e9}\nwhile \u{e9}\n\u{a0}done",
            "say %a.b(%c.d(%e.f(%g%)%)%)%",
            "\u{0}x\n\ry\r\n\t \u{b}",
            "wait until 99:99:99\nwait -",
            "wait \u{e9}",
            "%send",
            "% send%",
        ];
        for sample in samples {
            for attach in [AttachType::Mob, AttachType::Obj, AttachType::Wld] {
                let a = analyze(sample, attach);
                assert_eq!(a.classes.values().sum::<usize>(), a.lines, "{sample:?}");
            }
        }
        // Every prefix of a script, cut anywhere a character allows.
        let script =
            "if %actor.name(%é%)% == ü\n  %send% %actor% 100%%\nelse\nwait until 12:00\nend";
        for end in 0..=script.len() {
            if let Some(prefix) = script.get(..end) {
                let a = mob(prefix);
                assert_eq!(a.classes.values().sum::<usize>(), a.lines);
            }
        }
    }
}
