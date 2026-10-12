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
    /// Phase-1/2 parse coverage for the emitted essential scripts: which
    /// constructs parsed, which were named-dropped, and which substitutions
    /// are unknown. Written once for the whole world.
    pub trigger_coverage: TriggerCoverage,
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
    /// Essential DG scripts emitted into `tba.triggers` (issue #15).
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

/// Phase-1/2 parse coverage for the emitted essential scripts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TriggerCoverage {
    /// Emitted essential records.
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
