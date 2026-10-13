//! The machine-readable drop report.
//!
//! Written to `--report` (default `target/tbaport/drop-report.json`). Every
//! deliberately dropped or deferred source feature is named here with enough
//! metadata to rebuild it later; nothing is dropped silently.
//!
//! Vectors are sorted before writing so two runs over the same source produce
//! byte-identical JSON.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use dg_vm::analyze::Corpus;
use serde::Serialize;

use crate::error::Result;

/// Report schema version. Bumped when the shape changes incompatibly.
pub const REPORT_SCHEMA: u32 = 3;

/// The vnum Circle spawns mortals in by default (`CONFIG_MORTAL_START`).
pub const DEFAULT_START_ROOM: u32 = 3001;

/// Top-level drop report.
#[derive(Debug, Clone, Serialize)]
pub struct DropReport {
    pub schema: u32,
    pub tool: String,
    pub source: String,
    pub out: String,
    /// `full` or `only`.
    pub mode: String,
    pub only: Option<u32>,
    /// The vnum that was tagged `start_room`, or `None` when the requested
    /// room was absent from the converted world (a clear, machine-readable
    /// record of the no-op).
    pub start_room: Option<u32>,
    /// Whole-world features with no schema home, recorded once rather than
    /// once per record (for example room coordinates, which tbaMUD never
    /// stores). Sorted by `(kind, name)`.
    pub global_drops: Vec<GlobalDrop>,
    /// The DG trigger-corpus triage (issue #15). Every `.trg` record is
    /// classified by attach, expanded type, interactivity bucket and script
    /// size so the Phase-2 re-authoring scope is regenerable rather than
    /// hand-counted. Summary plus the per-record list.
    pub trigger_triage: TriggerTriage,
    /// Phase-1 parser coverage for every emitted script: which
    /// constructs parsed, which were named-dropped, and which substitutions
    /// are unknown. Written once for the whole world.
    pub trigger_coverage: TriggerCoverage,
    /// What each mob, object and room carries (issue #27): owner and link
    /// counts, unattached triggers, `attach_type` mismatches and the `V`
    /// reset commands that are still dropped.
    pub trigger_attachments: TriggerAttachments,
    pub totals: Totals,
    pub zones: Vec<ZoneReport>,
}

/// Aggregate counts across every selected zone.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub zones: usize,
    pub rooms: usize,
    pub npcs: usize,
    pub items: usize,
    pub zone_commands: usize,
    pub deferred: usize,
    pub dangling: usize,
    pub unmapped: usize,
    pub global_drops: usize,
    /// Every `unmapped` entry folded by `kind`, so the surviving drops are
    /// visible by reason without a second pass over the zone list.
    pub unmapped_by_kind: BTreeMap<String, usize>,
    /// Every `dangling` entry folded by `kind`.
    pub dangling_by_kind: BTreeMap<String, usize>,
    /// Every `deferred` entry folded by `section`.
    pub deferred_by_section: BTreeMap<String, usize>,
    /// DG scripts emitted into `tba.triggers` (every `.trg` record).
    pub triggers_converted: usize,
}

/// A source feature that is a permanent, named drop: it has no schema home and
/// is not deferred to a later conversion phase. Recorded once for the whole
/// world rather than once per record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct GlobalDrop {
    pub kind: String,
    pub name: String,
    pub reason: String,
}

/// Per-zone counts and per-record findings.
#[derive(Debug, Clone, Serialize)]
pub struct ZoneReport {
    pub number: u32,
    pub name: String,
    pub bot: i32,
    pub top: i32,
    pub area: String,
    pub rooms: usize,
    pub npcs: usize,
    pub items: usize,
    pub commands: usize,
    pub source_files: SourceFiles,
    pub deferred: Vec<DeferredEntry>,
    pub dangling: Vec<DanglingRef>,
    pub unmapped: Vec<UnmappedFlag>,
    /// One entry per reset command, preserving the source `if_flag`.
    pub command_flags: Vec<CommandFlag>,
    /// The classified `.trg` records that live in this zone file.
    pub triggers: Vec<TriggerTriageRecord>,
    /// The `V` reset commands in this zone: a script variable set on the mob,
    /// object or room the reset script targets. Named drops (the schema has
    /// no per-instance variables yet); each carries what is needed to apply
    /// it later.
    pub trigger_variables: Vec<TriggerVariableDrop>,
    /// Every zone-reset `T` command that ran statically and was folded into
    /// a prototype attachment list (or found already there).
    pub zone_trigger_links: Vec<ZoneTriggerLink>,
}

/// One zone-reset `T` command folded into the prototype attachments.
///
/// The C attaches a fresh copy per instance on every reset (`reset_zone`,
/// `db.c:3024`); the converter has no instances, so the link lands on the
/// prototype list instead (see `mapping::attach`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ZoneTriggerLink {
    pub zone: u32,
    /// Index into the zone's command list.
    pub command: usize,
    /// Source line of the command.
    pub line: usize,
    pub if_flag: i32,
    /// The owner the command targets (`npc tba_npc_3`, …).
    pub owner: String,
    /// The trigger vnum.
    pub trigger: i32,
    /// `folded` (a new link was appended) or `already_present` (the owner's
    /// list already held the trigger, so the C's second copy is not kept).
    pub outcome: String,
}

/// One `V` zone-reset command, dropped by name (`reset_zone`, `db.c:3052`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TriggerVariableDrop {
    /// Always `zone_trigger_variable`.
    pub kind: String,
    pub zone: u32,
    /// Index into the zone's command list.
    pub command: usize,
    /// Source line of the command.
    pub line: usize,
    pub if_flag: i32,
    /// What `reset_zone` does with the command: `runs`, `skipped` (the
    /// `if_flag`/`last_cmd` gate) or `disabled` (`renum_zone_table` found a
    /// missing vnum). Only a `runs` command with a `target` can be applied.
    pub fate: String,
    /// Which kind of entity the command targets: `mob`, `object`, `world`.
    pub attach: String,
    /// The target the reset script resolves to (`npc tba_npc_3`, …), or
    /// `None` when the C would find none or the command never runs (see `fate`).
    pub target: Option<String>,
    /// The context field as written in the zone file (`arg2`, the builder's
    /// "Global's context" prompt). For a room target the C passes it to
    /// `add_var` (`db.c:3075`).
    pub context: i32,
    /// The room field as written in the zone file (`arg3`, a vnum). For a mob
    /// or object target the C passes the room's *rnum* (a `world[]` index,
    /// after `renum_zone_table` rewrites `arg3`) as the variable's context
    /// instead of `context` (`db.c:3057`); for a room target it names the
    /// room.
    pub room: i32,
    pub name: String,
    pub value: String,
}

/// The attachment roll-up for `tba.triggers.attachments` (issue #27).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TriggerAttachments {
    /// Entities with at least one trigger.
    pub owners: usize,
    /// Total trigger links across all owners (duplicates counted).
    pub links: usize,
    /// Links by owner kind (`npc`, `item`, `room`).
    pub links_by_owner_kind: BTreeMap<String, usize>,
    /// Zone-reset `T` commands that appended a new link to a prototype
    /// (itemised in each zone's `zone_trigger_links`).
    pub zone_reset_folded: usize,
    /// Zone-reset `T` commands whose trigger the target already listed
    /// (also itemised there).
    pub zone_reset_already_present: usize,
    /// Emitted triggers no owner lists.
    pub unattached_triggers: usize,
    /// The vnums of those triggers, ascending numerically.
    pub unattached: Vec<i32>,
    /// Links whose trigger `attach_type` differs from the owner's kind; each
    /// is also an `unmapped` entry of kind `trigger_attach_type_mismatch`.
    pub attach_type_mismatches: usize,
    /// `V` reset commands, each itemised in its zone's `trigger_variables`.
    pub variable_commands: usize,
}

/// The whole-corpus DG trigger triage (issue #15).
#[derive(Debug, Clone, Default, Serialize)]
pub struct TriggerTriage {
    pub records: usize,
    /// All body lines, blank included.
    pub script_lines: usize,
    /// Non-blank body lines.
    pub nonblank_script_lines: usize,
    pub zones_with_triggers: usize,
    pub by_attach: BTreeMap<String, usize>,
    pub by_type: BTreeMap<String, usize>,
    pub by_bucket: BTreeMap<String, usize>,
    pub by_zone: BTreeMap<u32, usize>,
}

/// What `dg-vm` states about the emitted scripts: the totals of
/// `dg_vm::analyze` over every record, classified as the VM's driver treats
/// each line. Every map is keyed by lowercase names and sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TriggerCoverage {
    /// Emitted records.
    pub records: usize,
    /// Script lines, as `dg_vm::script::Program::parse` splits them; the sum
    /// of [`TriggerCoverage::lines_by_class`].
    pub script_lines: usize,
    /// The record's `attach_type` (`mob`, `obj`, `wld`) -> records.
    pub by_attach_type: BTreeMap<String, usize>,
    /// Lines by class: `comment`, `blank`, `control`, `statement`, `magic`,
    /// `script_command`, `unknown_command`, `mud_command`, `dynamic_command`.
    pub lines_by_class: BTreeMap<String, usize>,
    /// Control lines by keyword (`if`, `end`, `case`, `default`, ...).
    pub controls: BTreeMap<String, usize>,
    /// The driver's own statements by name (`set`, `eval`, `wait`,
    /// `dg_cast`, ...).
    pub statements: BTreeMap<String, usize>,
    /// `wait` lines by form (`pulses`, `seconds`, `ticks`, `until`,
    /// `dynamic`, `invalid`).
    pub wait_forms: BTreeMap<String, usize>,
    /// Script commands of the owner's command table by name, the commands the
    /// command variables (`%send%`) expand to included.
    pub script_commands: BTreeMap<String, usize>,
    /// Object-owned lines no object command accepts (the C logs "Unknown
    /// object cmd"), by first word.
    pub unknown_object_commands: BTreeMap<String, UnknownCommand>,
    /// Room-owned lines no world command accepts (the C logs "Unknown world
    /// cmd"), by first word.
    pub unknown_world_commands: BTreeMap<String, UnknownCommand>,
    /// Mob-owned lines that go to the game's command interpreter, by first
    /// word, split by whether the game supports the word.
    pub mud_commands: MudCommandCoverage,
    /// Lines whose first word is a variable that cannot be resolved without
    /// running the script (also in `lines_by_class`).
    pub dynamic_commands: usize,
    /// Lines of `MAX_INPUT_LENGTH` (512) bytes or more, which the VM's
    /// substitution refuses: the driver runs nothing for them.
    pub over_length_lines: usize,
    /// Control-flow errors the driver's scans hit.
    pub structural_errors: StructuralErrors,
    /// Substitution references by what their field names.
    pub field_references: FieldReferences,
    /// Variable heads read in a script that the VM does not provide and the
    /// script never assigns, with their reference counts.
    pub unknown_variable_heads: BTreeMap<String, usize>,
    /// Variable heads the VM provides (event, special and command
    /// variables) -> references.
    pub provided_variable_heads: BTreeMap<String, usize>,
    /// References to variables some line of the script assigns.
    pub assigned_variable_references: usize,
}

impl TriggerCoverage {
    /// The report's coverage from the analysis totals of `records` scripts,
    /// of which `by_attach_type` counts each owner type. `supported_mob_words`
    /// is the game's list of mob command words ([`split_mud_commands`]).
    pub fn from_corpus(
        corpus: &Corpus,
        by_attach_type: BTreeMap<String, usize>,
        supported_mob_words: &[&str],
    ) -> Self {
        let totals = &corpus.totals;
        let unknown =
            |counts: &BTreeMap<String, usize>,
             vnums: &BTreeMap<String, std::collections::BTreeSet<i32>>| {
                counts
                    .iter()
                    .map(|(word, count)| {
                        let vnums = vnums
                            .get(word)
                            .map(|set| set.iter().copied().collect())
                            .unwrap_or_default();
                        (
                            word.clone(),
                            UnknownCommand {
                                count: *count,
                                vnums,
                            },
                        )
                    })
                    .collect()
            };
        let structural_errors = StructuralErrors {
            count: corpus.structural.values().map(Vec::len).sum(),
            scripts: corpus.structural.len(),
            by_vnum: corpus
                .structural
                .iter()
                .map(|(vnum, errors)| {
                    let lines = errors
                        .iter()
                        .map(|e| format!("line {}: {}", e.line, e.kind.describe()))
                        .collect();
                    (*vnum, lines)
                })
                .collect(),
        };
        Self {
            records: totals.scripts,
            script_lines: totals.lines,
            by_attach_type,
            lines_by_class: totals
                .classes
                .iter()
                .map(|(class, count)| (class.name().to_owned(), *count))
                .collect(),
            controls: totals.controls.clone(),
            statements: totals.statements.clone(),
            wait_forms: totals.wait_forms.clone(),
            script_commands: totals.script_commands.clone(),
            unknown_object_commands: unknown(
                &totals.unknown_object_commands,
                &corpus.unknown_object_vnums,
            ),
            unknown_world_commands: unknown(
                &totals.unknown_world_commands,
                &corpus.unknown_world_vnums,
            ),
            mud_commands: split_mud_commands(&totals.mud_commands, supported_mob_words),
            dynamic_commands: totals
                .classes
                .get(&dg_vm::analyze::LineClass::DynamicCommand)
                .copied()
                .unwrap_or(0),
            over_length_lines: totals.over_length_lines,
            structural_errors,
            field_references: FieldReferences {
                total: totals.field_references,
                text_functions: totals.text_functions.clone(),
                character: totals.char_fields.clone(),
                object: totals.obj_fields.clone(),
                room: totals.room_fields.clone(),
                unknown_or_global: totals.unknown_fields.clone(),
            },
            unknown_variable_heads: totals.heads_unknown.clone(),
            provided_variable_heads: totals.heads_provided.clone(),
            assigned_variable_references: totals.heads_assigned.values().sum(),
        }
    }
}

/// An unknown object or world command word: its uses and the scripts using it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UnknownCommand {
    pub count: usize,
    /// Sorted trigger vnums.
    pub vnums: Vec<i32>,
}

/// The mob-owned lines handed to the game's command interpreter.
///
/// The game's list of supported mob command words is wired in separately
/// ([`split_mud_commands`]); until then it is empty, so every word is
/// unsupported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MudCommandCoverage {
    /// Lines in all.
    pub lines: usize,
    /// Words the game supports -> lines.
    pub supported: BTreeMap<String, usize>,
    /// Words it does not -> lines.
    pub unsupported: BTreeMap<String, usize>,
}

/// Split mud-command words (first word -> lines) into those in `supported`
/// and the rest. A word is supported when it equals an entry ASCII
/// case-insensitively; the mob command interpreter's abbreviations are the
/// caller's to expand into the list.
pub fn split_mud_commands(
    mud_commands: &BTreeMap<String, usize>,
    supported: &[&str],
) -> MudCommandCoverage {
    let mut out = MudCommandCoverage::default();
    for (word, count) in mud_commands {
        out.lines += count;
        let known = supported.iter().any(|name| name.eq_ignore_ascii_case(word));
        let side = if known {
            &mut out.supported
        } else {
            &mut out.unsupported
        };
        *side.entry(word.clone()).or_default() += count;
    }
    out
}

/// Control-flow errors the driver's scans would hit at run time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StructuralErrors {
    /// Errors in all.
    pub count: usize,
    /// Scripts with at least one.
    pub scripts: usize,
    /// Trigger vnum -> its errors, each `line N: <error>`.
    pub by_vnum: BTreeMap<i32, Vec<String>>,
}

/// `%var.field%` references by the table their field name belongs to. A name
/// several tables know is counted in each, so the maps do not sum to
/// `total`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FieldReferences {
    /// Field names in references, each step of a dotted chain counting once.
    pub total: usize,
    /// String functions (`strlen`, `car`, ...).
    pub text_functions: BTreeMap<String, usize>,
    /// Fields of the character table.
    pub character: BTreeMap<String, usize>,
    /// Fields of the object table.
    pub object: BTreeMap<String, usize>,
    /// Fields of the room table.
    pub room: BTreeMap<String, usize>,
    /// Names in none of those tables; each may still be a script global on
    /// its entity.
    pub unknown_or_global: BTreeMap<String, usize>,
}

/// One classified `.trg` record in the report.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TriggerTriageRecord {
    pub vnum: i32,
    pub name: String,
    /// `mob`, `object`, or `world`.
    pub attach: String,
    /// Raw attach-line letter string.
    pub flags: String,
    /// Expanded type names (valid flags only).
    pub types: Vec<String>,
    pub narg: i32,
    /// The rough interactivity bucket (`docs/design/triggers.md`).
    pub bucket: String,
    /// Non-blank body lines.
    pub lines: usize,
    /// All body lines, blank included.
    pub raw_lines: usize,
}

/// Which source files were present (and therefore scanned).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SourceFiles {
    pub wld: bool,
    pub mob: bool,
    pub obj: bool,
    pub zon: bool,
    pub shp: bool,
    pub trg: bool,
    pub qst: bool,
}

/// A deferred section record (shops/triggers/quests).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DeferredEntry {
    pub section: String,
    pub vnum: i32,
    pub attach: String,
    pub detail: String,
    pub reason: String,
}

/// A reference to an id that resolves to nothing in the converted output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DanglingRef {
    pub kind: String,
    pub from: String,
    pub target: String,
}

/// A source flag/feature with no target field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct UnmappedFlag {
    pub object: String,
    pub kind: String,
    pub name: String,
    pub detail: String,
}

/// The source `if_flag` of one reset command.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CommandFlag {
    pub index: usize,
    pub code: String,
    pub if_flag: i32,
}

impl DropReport {
    /// Sort every collection and recompute totals.
    pub fn finalise(&mut self) {
        for zone in &mut self.zones {
            zone.deferred.sort();
            zone.deferred.dedup();
            zone.dangling.sort();
            zone.dangling.dedup();
            zone.unmapped.sort();
            zone.unmapped.dedup();
            zone.triggers.sort();
            zone.triggers.dedup();
            zone.trigger_variables.sort();
            zone.trigger_variables.dedup();
            zone.zone_trigger_links.sort();
            zone.zone_trigger_links.dedup();
        }
        self.zones.sort_by_key(|zone| zone.number);
        self.global_drops.sort();
        self.global_drops.dedup();

        let mut unmapped_by_kind: BTreeMap<String, usize> = BTreeMap::new();
        let mut dangling_by_kind: BTreeMap<String, usize> = BTreeMap::new();
        let mut deferred_by_section: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_attach: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_bucket: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_zone: BTreeMap<u32, usize> = BTreeMap::new();
        let mut script_lines = 0usize;
        let mut nonblank_script_lines = 0usize;
        for zone in &self.zones {
            for entry in &zone.unmapped {
                *unmapped_by_kind.entry(entry.kind.clone()).or_default() += 1;
            }
            for entry in &zone.dangling {
                *dangling_by_kind.entry(entry.kind.clone()).or_default() += 1;
            }
            for entry in &zone.deferred {
                *deferred_by_section
                    .entry(entry.section.clone())
                    .or_default() += 1;
            }
            for trigger in &zone.triggers {
                *by_attach.entry(trigger.attach.clone()).or_default() += 1;
                *by_bucket.entry(trigger.bucket.clone()).or_default() += 1;
                *by_zone.entry(zone.number).or_default() += 1;
                for name in &trigger.types {
                    *by_type.entry(name.clone()).or_default() += 1;
                }
                script_lines += trigger.raw_lines;
                nonblank_script_lines += trigger.lines;
            }
        }

        self.trigger_triage = TriggerTriage {
            records: self.zones.iter().map(|z| z.triggers.len()).sum(),
            script_lines,
            nonblank_script_lines,
            zones_with_triggers: by_zone.len(),
            by_attach,
            by_type,
            by_bucket,
            by_zone,
        };

        self.trigger_attachments.attach_type_mismatches = unmapped_by_kind
            .get("trigger_attach_type_mismatch")
            .copied()
            .unwrap_or(0);
        let links = self.zones.iter().flat_map(|zone| &zone.zone_trigger_links);
        let (mut folded, mut present) = (0, 0);
        for link in links {
            if link.outcome == "folded" {
                folded += 1;
            } else {
                present += 1;
            }
        }
        self.trigger_attachments.zone_reset_folded = folded;
        self.trigger_attachments.zone_reset_already_present = present;
        self.trigger_attachments.variable_commands = self
            .zones
            .iter()
            .map(|zone| zone.trigger_variables.len())
            .sum();

        self.totals = Totals {
            zones: self.zones.len(),
            rooms: self.zones.iter().map(|z| z.rooms).sum(),
            npcs: self.zones.iter().map(|z| z.npcs).sum(),
            items: self.zones.iter().map(|z| z.items).sum(),
            zone_commands: self.zones.iter().map(|z| z.commands).sum(),
            deferred: self.zones.iter().map(|z| z.deferred.len()).sum(),
            dangling: self.zones.iter().map(|z| z.dangling.len()).sum(),
            unmapped: self.zones.iter().map(|z| z.unmapped.len()).sum(),
            global_drops: self.global_drops.len(),
            unmapped_by_kind,
            dangling_by_kind,
            deferred_by_section,
            triggers_converted: self.trigger_coverage.records,
        };
    }

    /// Write the report as pretty JSON plus a human-readable summary.
    ///
    /// The summary sits beside the JSON as `<stem>.summary.md`
    /// (`reports/drop-report.summary.md` for `reports/drop-report.json`)
    /// so the same command regenerates both and a reviewer can read the
    /// totals without loading the multi-megabyte JSON.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| crate::error::ConvertError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, format!("{json}\n")).map_err(|source| {
            crate::error::ConvertError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;

        let summary_path = path.with_extension("summary.md");
        std::fs::write(&summary_path, self.summary()).map_err(|source| {
            crate::error::ConvertError::Io {
                path: summary_path,
                source,
            }
        })?;
        Ok(())
    }

    /// A deterministic Markdown summary of the report's totals, grouped by
    /// drop reason.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        let totals = &self.totals;
        let _ = writeln!(out, "# tbaport drop report summary\n");
        let _ = writeln!(out, "- tool: `{}`", self.tool);
        let _ = writeln!(out, "- mode: `{}`", self.mode);
        let _ = writeln!(out, "- zones: {}", totals.zones);
        let _ = writeln!(out, "- rooms: {}", totals.rooms);
        let _ = writeln!(out, "- npcs: {}", totals.npcs);
        let _ = writeln!(out, "- items: {}", totals.items);
        let _ = writeln!(out, "- zone commands: {}", totals.zone_commands);
        let _ = writeln!(out, "- deferred: {}", totals.deferred);
        let _ = writeln!(out, "- dangling: {}", totals.dangling);
        let _ = writeln!(out, "- unmapped: {}", totals.unmapped);
        let _ = writeln!(out, "- global permanent drops: {}", totals.global_drops);
        let _ = writeln!(out, "- DG triggers emitted: {}", totals.triggers_converted);
        let attachments = &self.trigger_attachments;
        let _ = writeln!(
            out,
            "- DG trigger owners: {} ({} links)",
            attachments.owners, attachments.links
        );

        let _ = writeln!(out, "\n## Unmapped drops by reason\n");
        let _ = writeln!(out, "| reason (`kind`) | count |");
        let _ = writeln!(out, "|---|---:|");
        for (kind, count) in &totals.unmapped_by_kind {
            let _ = writeln!(out, "| `{kind}` | {count} |");
        }

        let _ = writeln!(out, "\n## Dangling references by reason\n");
        let _ = writeln!(out, "| reason (`kind`) | count |");
        let _ = writeln!(out, "|---|---:|");
        for (kind, count) in &totals.dangling_by_kind {
            let _ = writeln!(out, "| `{kind}` | {count} |");
        }

        let _ = writeln!(out, "\n## Deferred records by section\n");
        let _ = writeln!(out, "| section | count |");
        let _ = writeln!(out, "|---|---:|");
        for (section, count) in &totals.deferred_by_section {
            let _ = writeln!(out, "| `{section}` | {count} |");
        }

        let triage = &self.trigger_triage;
        let _ = writeln!(out, "\n## DG trigger triage (#15)\n");
        let _ = writeln!(out, "- records: {}", triage.records);
        let _ = writeln!(
            out,
            "- script lines (non-blank): {}",
            triage.nonblank_script_lines
        );
        let _ = writeln!(out, "- zones with triggers: {}", triage.zones_with_triggers);
        let _ = writeln!(out, "\n### Triggers by attach\n");
        let _ = writeln!(out, "| attach | count |");
        let _ = writeln!(out, "|---|---:|");
        for (attach, count) in &triage.by_attach {
            let _ = writeln!(out, "| `{attach}` | {count} |");
        }
        let _ = writeln!(out, "\n### Triggers by interactivity bucket\n");
        let _ = writeln!(out, "| bucket | count |");
        let _ = writeln!(out, "|---|---:|");
        for (bucket, count) in &triage.by_bucket {
            let _ = writeln!(out, "| `{bucket}` | {count} |");
        }
        let _ = writeln!(out, "\n### Triggers by expanded type\n");
        let _ = writeln!(out, "| type | count |");
        let _ = writeln!(out, "|---|---:|");
        for (name, count) in &triage.by_type {
            let _ = writeln!(out, "| `{name}` | {count} |");
        }

        let _ = writeln!(out, "\n## DG trigger attachments (#27)\n");
        let _ = writeln!(out, "- owners: {}", attachments.owners);
        let _ = writeln!(out, "- links: {}", attachments.links);
        for (kind, count) in &attachments.links_by_owner_kind {
            let _ = writeln!(out, "  - `{kind}`: {count}");
        }
        let _ = writeln!(
            out,
            "- zone-reset `T` folded into prototypes: {} ({} already listed); deviation: \
             the C attaches per instance at each reset",
            attachments.zone_reset_folded, attachments.zone_reset_already_present
        );
        let _ = writeln!(
            out,
            "- unattached triggers: {}",
            attachments.unattached_triggers
        );
        let _ = writeln!(
            out,
            "- attach_type mismatches (kept, named drops): {}",
            attachments.attach_type_mismatches
        );
        let _ = writeln!(
            out,
            "- `V` reset commands (named drops): {}",
            attachments.variable_commands
        );

        let coverage = &self.trigger_coverage;
        let _ = writeln!(out, "\n## DG trigger corpus (#15)\n");
        let _ = writeln!(
            out,
            "Counts describe the emitted scripts (colour codes stripped), so they can \
             differ from a count over the raw `.trg` bodies.\n"
        );
        let _ = writeln!(out, "- emitted records: {}", coverage.records);
        let _ = writeln!(out, "- script lines: {}", coverage.script_lines);
        let _ = writeln!(
            out,
            "- lines over the substitution limit: {}",
            coverage.over_length_lines
        );
        let _ = writeln!(out, "\n### Records by attach type\n");
        count_table(&mut out, "attach type", &coverage.by_attach_type, None);
        let _ = writeln!(out, "\n### Lines by class\n");
        count_table(&mut out, "class", &coverage.lines_by_class, None);
        let _ = writeln!(out, "\n### Control lines\n");
        count_table(&mut out, "keyword", &coverage.controls, None);
        let _ = writeln!(out, "\n### Statements\n");
        count_table(&mut out, "statement", &coverage.statements, None);
        let _ = writeln!(out, "\n### `wait` forms\n");
        count_table(&mut out, "form", &coverage.wait_forms, None);
        let _ = writeln!(
            out,
            "\n### Script commands (top {TOP_N} of {})\n",
            coverage.script_commands.len()
        );
        count_table(&mut out, "command", &coverage.script_commands, Some(TOP_N));

        let errors = &coverage.structural_errors;
        let _ = writeln!(out, "\n### Structural errors\n");
        let _ = writeln!(
            out,
            "{} errors in {} scripts.\n",
            errors.count, errors.scripts
        );
        let _ = writeln!(out, "| trigger | errors |");
        let _ = writeln!(out, "|---:|---|");
        for (vnum, lines) in &errors.by_vnum {
            let _ = writeln!(out, "| {vnum} | {} |", lines.join("; "));
        }

        for (title, unknown) in [
            ("Unknown object commands", &coverage.unknown_object_commands),
            ("Unknown world commands", &coverage.unknown_world_commands),
        ] {
            let _ = writeln!(
                out,
                "\n### {title} (top {} of {} words)\n",
                unknown.len().min(TOP_N),
                unknown.len()
            );
            let _ = writeln!(out, "| word | lines | triggers |");
            let _ = writeln!(out, "|---|---:|---|");
            for (word, use_) in top_n(unknown.iter().map(|(w, u)| (w, u.count)), TOP_N) {
                let vnums = unknown.get(word).map_or(&[][..], |u| u.vnums.as_slice());
                let _ = writeln!(
                    out,
                    "| {} | {} | {} |",
                    code_cell(word),
                    use_,
                    vnum_list(vnums)
                );
            }
        }

        let mud = &coverage.mud_commands;
        let _ = writeln!(
            out,
            "\n### Mob commands for the game ({} lines)\n",
            mud.lines
        );
        let _ = writeln!(
            out,
            "Supported words: {}; unsupported words: {} (top {} below).\n",
            mud.supported.len(),
            mud.unsupported.len(),
            mud.unsupported.len().min(TOP_N)
        );
        count_table(&mut out, "unsupported word", &mud.unsupported, Some(TOP_N));

        let refs = &coverage.field_references;
        let _ = writeln!(
            out,
            "\n### Substitution fields ({} references)\n",
            refs.total
        );
        let _ = writeln!(out, "| table | distinct names | references |");
        let _ = writeln!(out, "|---|---:|---:|");
        for (table, names) in [
            ("text function", &refs.text_functions),
            ("character", &refs.character),
            ("object", &refs.object),
            ("room", &refs.room),
            ("unknown or global", &refs.unknown_or_global),
        ] {
            let _ = writeln!(
                out,
                "| {table} | {} | {} |",
                names.len(),
                names.values().sum::<usize>()
            );
        }
        let _ = writeln!(
            out,
            "\nUnknown or global names (top {} of {}):\n",
            refs.unknown_or_global.len().min(TOP_N),
            refs.unknown_or_global.len()
        );
        count_table(&mut out, "name", &refs.unknown_or_global, Some(TOP_N));
        let _ = writeln!(
            out,
            "\n### Variables read but never provided or assigned (top {} of {} names)\n",
            coverage.unknown_variable_heads.len().min(TOP_N),
            coverage.unknown_variable_heads.len()
        );
        count_table(
            &mut out,
            "variable",
            &coverage.unknown_variable_heads,
            Some(TOP_N),
        );

        let _ = writeln!(out, "\n## Global permanent drops\n");
        for drop in &self.global_drops {
            let _ = writeln!(out, "- `{}` / `{}`: {}", drop.kind, drop.name, drop.reason);
        }
        out
    }
}

/// How many rows a long map gets in the summary.
const TOP_N: usize = 15;

/// The `limit` largest entries: count descending, name ascending.
fn top_n<'a>(
    entries: impl Iterator<Item = (&'a String, usize)>,
    limit: usize,
) -> Vec<(&'a String, usize)> {
    let mut rows: Vec<(&String, usize)> = entries.collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    rows.truncate(limit);
    rows
}

/// A two-column markdown table of `counts`: all rows, or the `limit` largest.
fn count_table(
    out: &mut String,
    heading: &str,
    counts: &BTreeMap<String, usize>,
    limit: Option<usize>,
) {
    let _ = writeln!(out, "| {heading} | count |");
    let _ = writeln!(out, "|---|---:|");
    let rows = match limit {
        Some(limit) => top_n(counts.iter().map(|(k, v)| (k, *v)), limit),
        None => counts.iter().map(|(k, v)| (k, *v)).collect(),
    };
    for (name, count) in rows {
        let _ = writeln!(out, "| {} | {count} |", code_cell(name));
    }
}

/// `text` as a code span that is safe in a markdown table cell: a backtick
/// would end the span and a `|` the cell.
fn code_cell(text: &str) -> String {
    let text = text.replace('|', "\\|").replace('\n', " ");
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

/// Up to eight vnums, then how many more.
fn vnum_list(vnums: &[i32]) -> String {
    let shown: Vec<String> = vnums.iter().take(8).map(i32::to_string).collect();
    match vnums.len().checked_sub(8) {
        Some(more) if more > 0 => format!("{} (+{more})", shown.join(", ")),
        _ => shown.join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
        entries.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    #[test]
    fn mud_commands_split_by_the_supported_words() {
        let words = counts(&[("say", 5), ("Smile", 2), ("zap", 1)]);
        let none = split_mud_commands(&words, &[]);
        assert_eq!(none.lines, 8);
        assert!(none.supported.is_empty());
        assert_eq!(none.unsupported, words);

        let split = split_mud_commands(&words, &["say", "smile"]);
        assert_eq!(split.lines, 8);
        assert_eq!(split.supported, counts(&[("say", 5), ("Smile", 2)]));
        assert_eq!(split.unsupported, counts(&[("zap", 1)]));
    }

    #[test]
    fn table_cells_survive_pipes_and_backticks() {
        assert_eq!(code_cell("say"), "`say`");
        assert_eq!(code_cell("a|b"), "`a\\|b`");
        assert_eq!(code_cell("it`s"), "`` it`s ``");
        let mut out = String::new();
        count_table(&mut out, "word", &counts(&[("x|y", 2)]), None);
        assert!(out.contains("| `x\\|y` | 2 |"), "{out}");
    }

    #[test]
    fn long_lists_are_cut_to_the_largest_entries() {
        let map = counts(&[("a", 1), ("b", 3), ("c", 3), ("d", 2)]);
        let top = top_n(map.iter().map(|(k, v)| (k, *v)), 3);
        let names: Vec<&str> = top.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["b", "c", "d"]);
        assert_eq!(vnum_list(&[1, 2]), "1, 2");
        assert_eq!(
            vnum_list(&(1..=10).collect::<Vec<_>>()),
            "1, 2, 3, 4, 5, 6, 7, 8 (+2)"
        );
        assert_eq!(vnum_list(&[]), "");
    }
}
