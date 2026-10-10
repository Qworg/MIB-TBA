//! The machine-readable drop report.
//!
//! Written to `--report` (default `target/tbaport/drop-report.json`). Every
//! deliberately dropped or deferred source feature is named here with enough
//! metadata to rebuild it later; nothing is dropped silently.
//!
//! Vectors are sorted before writing so two runs over the same source produce
//! byte-identical JSON.

use std::path::Path;

use serde::Serialize;

use crate::error::Result;

/// Report schema version. Bumped when the shape changes incompatibly.
pub const REPORT_SCHEMA: u32 = 1;

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
        }
        self.zones.sort_by_key(|zone| zone.number);
        self.totals = Totals {
            zones: self.zones.len(),
            rooms: self.zones.iter().map(|z| z.rooms).sum(),
            npcs: self.zones.iter().map(|z| z.npcs).sum(),
            items: self.zones.iter().map(|z| z.items).sum(),
            zone_commands: self.zones.iter().map(|z| z.commands).sum(),
            deferred: self.zones.iter().map(|z| z.deferred.len()).sum(),
            dangling: self.zones.iter().map(|z| z.dangling.len()).sum(),
            unmapped: self.zones.iter().map(|z| z.unmapped.len()).sum(),
        };
    }

    /// Write the report as pretty JSON.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| crate::error::ConvertError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, format!("{json}\n")).map_err(|source| crate::error::ConvertError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}
