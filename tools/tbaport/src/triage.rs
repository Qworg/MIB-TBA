//! The standalone DG trigger triage (`reports/trigger-triage.json`).
//!
//! This is the machine-readable companion to `docs/design/triggers.md`. It
//! classifies every record in `lib/world/trg/*.trg` by attach, expanded
//! trigger type, interactivity bucket and script size, then marks the
//! **essential interactivity set** that Phase 2 (TBA-E20) must re-author.
//!
//! The mechanical classification (attach, flags, types, bucket, line counts)
//! lives on [`crate::source::deferred::TriggerRecord`] and is identical to the
//! per-zone `triggers` array in [`crate::report::DropReport`]. This module adds
//! the corpus roll-ups and the curated ranking.
//!
//! Determinism: records are emitted sorted by `(zone, vnum)`, buckets/counts
//! are `BTreeMap`s, and the essential set is ranked by a fixed score with
//! `(zone, vnum)` tie-breaks, so two runs over the same source are byte-equal.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::source::deferred::TriggerBucket;

/// Triage schema version, bumped on an incompatible shape change.
pub const TRIAGE_SCHEMA: u32 = 1;

/// Zones that make up the Phase-2 demo spine.
///
/// The first group is every zone reachable on foot from the start room
/// (`tba.3001`, the Temple of Midgaard) in the converted world's exit graph
/// (BFS, see `docs/design/triggers.md` §3). The second group is the newbie /
/// tutorial hubs and the one converted quest area, which the start region
/// links to but does not border by a walkable exit.
pub const DEMO_ZONES: &[u32] = &[
    // Reachable from room 3001 in the converted exit graph.
    9, 15, 25, 26, 30, 31, 32, 33, 35, 36, 40, 41, 50, 51, 52, 53, 54, 55, 56, 60, 61, 62, 63, 64,
    65, 70, 71, 72, 73, 79, 120, 150, 186,
    // Newbie / tutorial / quest hubs reached via portals or scripts.
    0, 1, 5, 28, 74, 234,
];

/// Whether a zone is part of the demo spine.
pub fn is_demo_zone(zone: u32) -> bool {
    DEMO_ZONES.contains(&zone)
}

/// The whole-corpus triage.
#[derive(Debug, Clone, Serialize)]
pub struct TriageReport {
    pub schema: u32,
    pub tool: String,
    pub source: String,
    pub corpus: Corpus,
    pub essential: Essential,
    pub records: Vec<TriageRecord>,
}

/// Corpus roll-ups.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Corpus {
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

/// The curated essential interactivity set.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Essential {
    pub count: usize,
    pub zones: Vec<u32>,
    pub demo_zones: Vec<u32>,
    pub by_attach: BTreeMap<String, usize>,
    pub by_bucket: BTreeMap<String, usize>,
}

/// One classified trigger.
#[derive(Debug, Clone, Serialize)]
pub struct TriageRecord {
    pub vnum: i32,
    pub zone: u32,
    pub source_file: String,
    pub attach: String,
    pub name: String,
    pub flags: String,
    pub types: Vec<String>,
    pub narg: i32,
    pub bucket: String,
    pub lines: usize,
    pub raw_lines: usize,
    /// Whether this record is in the curated Phase-2 essential set.
    pub essential: bool,
    /// `0` quest, `1` gatekeeper, `2` other interactive; `3` non-essential.
    pub tier: u8,
    pub score: i64,
    /// 1-based rank among essentials, `null` otherwise.
    pub rank: Option<usize>,
    /// Owner ids from the `.wld`/`.mob`/`.obj` `T` attachments.
    pub owners: Vec<String>,
}

/// Tier of an essential bucket.
pub fn tier(bucket: TriggerBucket) -> u8 {
    match bucket {
        TriggerBucket::QuestObjective => 0,
        TriggerBucket::GatekeeperTransport => 1,
        _ => 2,
    }
}

/// The interactivity types that make a mob response essential.
const ESSENTIAL_MOB_TYPES: &[&str] = &[
    "Speech",
    "Command",
    "Act",
    "Greet",
    "Greet-All",
    "Receive",
    "Bribe",
    "Cast",
    "Door",
    "Entry",
    "Leave",
];

/// Whether a trigger joins the curated essential set.
pub fn is_essential(bucket: TriggerBucket, attach: &str, types: &[String]) -> bool {
    match bucket {
        TriggerBucket::QuestObjective | TriggerBucket::GatekeeperTransport => true,
        TriggerBucket::InteractiveResponse => {
            attach == "mob"
                && types
                    .iter()
                    .any(|t| ESSENTIAL_MOB_TYPES.contains(&t.as_str()))
        }
        _ => false,
    }
}

/// The fixed demo-impact score used to rank essentials. Larger is more
/// demo-visible. Every term is a constant so the ranking is reproducible.
pub fn score(
    bucket: TriggerBucket,
    zone: u32,
    owner_zones: &[u32],
    types: &[String],
    lines: usize,
    name: &str,
) -> i64 {
    let mut score: i64 = match bucket {
        TriggerBucket::QuestObjective => 1000,
        TriggerBucket::GatekeeperTransport => 600,
        TriggerBucket::InteractiveResponse => 400,
        TriggerBucket::CombatBehaviour => 100,
        TriggerBucket::AmbienceFlavour => 0,
    };
    if zone == 30 || owner_zones.contains(&30) {
        score += 200;
    }
    if zone == 0 || owner_zones.contains(&0) {
        score += 120;
    }
    score += (lines.min(40) as i64) * 5;
    for name in types {
        score += match name.as_str() {
            "Greet" | "Speech" | "Receive" | "Command" | "Act" => 40,
            "Bribe" | "Cast" | "Door" | "Entry" | "Leave" => 20,
            _ => 0,
        };
    }
    let lower = name.to_ascii_lowercase();
    for (keyword, weight) in [
        ("questmaster", 300),
        ("tutorial", 250),
        ("newbie", 200),
        ("guide", 150),
        ("guild", 120),
        ("guard", 80),
        ("portal", 150),
        ("quest", 200),
    ] {
        if lower.contains(keyword) {
            score += weight;
        }
    }
    score
}

/// Zone number of an owner vnum (`vnum / 100`, Circle's zone banding).
pub fn vnum_zone(vnum: i32) -> u32 {
    if vnum < 0 { 0 } else { (vnum / 100) as u32 }
}
