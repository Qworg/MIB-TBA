//! The machine-readable drop report.
//!
//! Written to `--report` (default `target/tbaport/drop-report.json`). Every
//! deliberately dropped or deferred source feature is named here with enough
//! metadata to rebuild it later; nothing is dropped silently.
//!
//! Vectors are sorted before writing so two runs over the same source produce
//! byte-identical JSON.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::error::Result;

/// Report schema version. Bumped when the shape changes incompatibly.
pub const REPORT_SCHEMA: u32 = 2;

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
    /// Which kind of entity the command targets: `mob`, `object`, `world`.
    pub attach: String,
    /// The target the reset script resolves to (`npc tba_npc_3`, …), or
    /// `None` when the C would find none.
    pub target: Option<String>,
    /// The variable's context, as the C passes it to `add_var`.
    pub context: i32,
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
    /// Zone-reset `T` commands that appended a new link to a prototype.
    pub zone_reset_folded: usize,
    /// Zone-reset `T` commands whose trigger the target already listed.
    pub zone_reset_already_present: usize,
    /// Emitted triggers no owner lists.
    pub unattached_triggers: usize,
    /// The vnums of those triggers, ascending.
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

/// Parser coverage for every emitted script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TriggerCoverage {
    /// Emitted records.
    pub records: usize,
    /// Records whose script parsed without a structural error.
    pub parsed: usize,
    /// Records whose script had a structural error (a named drop each).
    pub parse_failures: usize,
    /// Records containing at least one unsupported construct.
    pub records_with_unsupported: usize,
    /// AST node kind -> occurrences.
    pub construct_counts: BTreeMap<String, usize>,
    /// Unsupported construct kind -> occurrences.
    pub unsupported_counts: BTreeMap<String, usize>,
    /// Unknown `%…%` token -> occurrences.
    pub unknown_tokens: BTreeMap<String, usize>,
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
        use std::fmt::Write as _;

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
        let _ = writeln!(out, "\n## DG trigger parse coverage (#15)\n");
        let _ = writeln!(out, "- emitted records: {}", coverage.records);
        let _ = writeln!(out, "- parsed: {}", coverage.parsed);
        let _ = writeln!(out, "- parse failures: {}", coverage.parse_failures);
        let _ = writeln!(
            out,
            "- records with unsupported constructs: {}",
            coverage.records_with_unsupported
        );
        let _ = writeln!(out, "\n### Emitted constructs\n");
        let _ = writeln!(out, "| construct | count |");
        let _ = writeln!(out, "|---|---:|");
        for (kind, count) in &coverage.construct_counts {
            let _ = writeln!(out, "| `{kind}` | {count} |");
        }
        let _ = writeln!(out, "\n### Unsupported constructs (named drops)\n");
        let _ = writeln!(out, "| construct | count |");
        let _ = writeln!(out, "|---|---:|");
        for (kind, count) in &coverage.unsupported_counts {
            let _ = writeln!(out, "| `{kind}` | {count} |");
        }

        let _ = writeln!(out, "\n## Global permanent drops\n");
        for drop in &self.global_drops {
            let _ = writeln!(out, "- `{}` / `{}`: {}", drop.kind, drop.name, drop.reason);
        }
        out
    }
}
