//! Trigger reader: `.trg`.
//!
//! Every record is scanned and *classified* so the drop report carries the
//! triage and the converter can emit the whole corpus into `tba.triggers`. The
//! scan is faithful to the source grammar so it cannot mis-count records:
//!
//! - `.trg` — `parse_trigger` (`dg_db_scripts.c:30`): `#vnum`, name, attach
//!   line (`<attach_type> <flags> <narg>`), arglist, command list.
//!
//! The attach line's `flags` field is a string of letters converted by
//! `asciiflag_conv` (`db.c:1209`): letter `a` is bit 0, `b` bit 1, … The bit
//! names depend on the attach type, from `trig_types[]`/`otrig_types[]`/
//! `wtrig_types[]` (`constants.c:859-931`). See [`expand_types`].
//!
//! Quests (`.qst`) are parsed and emitted as `stdlib.quests` by
//! [`crate::source::quest`]; shops (`.shp`) by [`crate::source::shop`].

use crate::error::Result;
use crate::source::tokens::Source;

/// Trigger type names, indexed by `asciiflag_conv` bit, for `attach_type` 0
/// (mob). `trig_types[]`, `constants.c:859`.
const MOB_TYPES: [&str; 20] = [
    "Global",
    "Random",
    "Command",
    "Speech",
    "Act",
    "Death",
    "Greet",
    "Greet-All",
    "Entry",
    "Receive",
    "Fight",
    "HitPrcnt",
    "Bribe",
    "Load",
    "Memory",
    "Cast",
    "Leave",
    "Door",
    "Damage",
    "Time",
];

/// Object trigger type names (`otrig_types[]`, `constants.c:884`). `UNUSEDn`
/// slots are dropped by [`expand_types`].
const OBJ_TYPES: [&str; 20] = [
    "Global", "Random", "Command", "UNUSED1", "UNUSED2", "Timer", "Get", "Drop", "Give", "Wear",
    "UNUSED3", "Remove", "UNUSED4", "Load", "UNUSED5", "Cast", "Leave", "UNUSED6", "Consume",
    "Time",
];

/// World (room) trigger type names (`wtrig_types[]`, `constants.c:909`).
const WLD_TYPES: [&str; 20] = [
    "Global",
    "Random",
    "Command",
    "Speech",
    "UNUSED1",
    "Zone Reset",
    "Enter",
    "Drop",
    "UNUSED2",
    "UNUSED3",
    "UNUSED4",
    "UNUSED5",
    "UNUSED6",
    "UNUSED7",
    "UNUSED8",
    "Cast",
    "Leave",
    "Door",
    "Login",
    "Time",
];

/// The rough interactivity taxonomy from the TBA-E20 triage (`docs/design/
/// triggers.md`). One primary bucket per trigger, chosen with a fixed
/// precedence so the report is deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TriggerBucket {
    /// Drives a quest/objective: object hand-in, quest variable, quest reward.
    QuestObjective,
    /// Answers player input: speech, commands, acts, greets, receive.
    InteractiveResponse,
    /// Gates or moves the player: doors, entries, leaves, teleports.
    GatekeeperTransport,
    /// NPC combat behaviour: fight, hitprcnt, death, damage, memory.
    CombatBehaviour,
    /// Flavour with no player-visible consequence: random/time/global echoes.
    AmbienceFlavour,
}

impl TriggerBucket {
    /// Stable report spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerBucket::QuestObjective => "quest_objective",
            TriggerBucket::InteractiveResponse => "interactive_response",
            TriggerBucket::GatekeeperTransport => "gatekeeper_transport",
            TriggerBucket::CombatBehaviour => "combat_behaviour",
            TriggerBucket::AmbienceFlavour => "ambience_flavour",
        }
    }
}

/// One classified DG trigger record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TriggerRecord {
    /// Trigger vnum.
    pub vnum: i32,
    /// Trigger name (the `name~` field).
    pub name: String,
    /// What the record attaches to: `mob`, `object`, `world`, or `unknown`.
    pub attach: String,
    /// `attach_type` as loaded: 0 mob, 1 object, 2 world.
    pub attach_type: i32,
    /// Raw letter string from the attach line.
    pub flags: String,
    /// Expanded type names (valid flags only, in flag order).
    pub types: Vec<String>,
    /// The `narg` field (0 when the attach line omits it).
    pub narg: i32,
    /// The DG `arglist` (match text for speech/command).
    pub arglist: String,
    /// The script body, verbatim and colour-stripped.
    pub body: String,
    /// Non-blank lines in the script body.
    pub lines: usize,
    /// All body lines, blank included.
    pub raw_lines: usize,
    /// The rough interactivity bucket.
    pub bucket: TriggerBucket,
}

/// The attach-type-to-bit-name table for `attach_type`.
fn type_table(attach_type: i32) -> &'static [&'static str; 20] {
    match attach_type {
        0 => &MOB_TYPES,
        1 => &OBJ_TYPES,
        2 => &WLD_TYPES,
        _ => &MOB_TYPES,
    }
}

/// `asciiflag_conv` bit expansion (`db.c:1209`), keeping only named types.
///
/// Lowercase `a`..`z` select bits 0..25; anything above the 20 named slots
/// (and the `UNUSEDn` placeholders) is dropped.
pub fn expand_types(attach_type: i32, flags: &str) -> Vec<String> {
    let table = type_table(attach_type);
    let mut out = Vec::new();
    for c in flags.chars() {
        if c.is_ascii_lowercase() {
            let bit = (c as u32 - 'a' as u32) as usize;
            if let Some(name) = table.get(bit)
                && !name.starts_with("UNUSED")
            {
                out.push((*name).to_owned());
            }
        }
    }
    out
}

/// Scan a `.trg` file and classify every record.
pub fn scan_triggers(src: &mut Source) -> Result<Vec<TriggerRecord>> {
    let mut records = Vec::new();
    while let Some(header) = src.next_line() {
        let trimmed = header.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('$') {
            break;
        }
        let vnum = match trimmed
            .strip_prefix('#')
            .and_then(|r| r.trim().parse().ok())
        {
            Some(vnum) => vnum,
            None => continue,
        };
        let name = src.read_string("trigger name")?;
        let attach_line = src
            .next_nonblank()
            .ok_or_else(|| src.parse_error(format!("trigger {vnum}: missing attach line")))?;
        let mut fields = attach_line.split_whitespace();
        let attach_type: i32 = fields.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        let flags = fields.next().unwrap_or("").to_owned();
        let narg: i32 = fields.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        let attach = match attach_type {
            0 => "mob",
            1 => "object",
            2 => "world",
            _ => "unknown",
        };
        // Consume the arg list and the script body so the next record's `#`
        // is read at the right level.
        let arglist = src.read_string("trigger arglist")?;
        let body = src.read_string("trigger body")?;
        let types = expand_types(attach_type, &flags);
        let lines = body.lines().filter(|line| !line.trim().is_empty()).count();
        let raw_lines = body.lines().count();
        let bucket = classify(attach, &types, &name, &body);
        records.push(TriggerRecord {
            vnum,
            name,
            attach: attach.to_owned(),
            attach_type,
            flags,
            types,
            narg,
            arglist,
            body,
            lines,
            raw_lines,
            bucket,
        });
    }
    records.sort();
    Ok(records)
}

/// Player-input trigger types per attach family (`MOB_INTER`/`OBJ_INTER`/
/// `WLD_INTER` in `docs/design/triggers.md`).
fn is_interactive(attach: &str, types: &[String]) -> bool {
    let set: &[&str] = match attach {
        "mob" => &[
            "Speech",
            "Command",
            "Act",
            "Greet",
            "Greet-All",
            "Receive",
            "Bribe",
            "Cast",
        ],
        "object" => &[
            "Command", "Get", "Drop", "Give", "Wear", "Remove", "Consume",
        ],
        "world" => &["Command", "Speech", "Enter", "Drop", "Cast", "Give"],
        _ => &[],
    };
    types.iter().any(|t| set.contains(&t.as_str()))
}

/// Movement-gating trigger types per attach family.
fn is_gate(attach: &str, types: &[String]) -> bool {
    let set: &[&str] = match attach {
        "mob" => &["Door", "Entry", "Leave"],
        "object" => &["Leave"],
        "world" => &["Door", "Enter", "Leave"],
        _ => &[],
    };
    types.iter().any(|t| set.contains(&t.as_str()))
}

/// Combat-behaviour trigger types per attach family.
fn is_combat(attach: &str, types: &[String]) -> bool {
    let set: &[&str] = match attach {
        "mob" => &["Fight", "HitPrcnt", "Death", "Damage", "Memory"],
        "object" => &["Timer"],
        "world" => &["Zone Reset"],
        _ => &[],
    };
    types.iter().any(|t| set.contains(&t.as_str()))
}

/// True when the body names a quest state: `%object.vnum%`/`%obj.vnum%`, a
/// `%actor.varexists(…quest|solved|complet…)` probe, or a `questpoint(s)`.
fn quest_marker(body: &str, name: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    if lower.contains("%object.vnum%") || lower.contains("%obj.vnum%") {
        return true;
    }
    if lower.contains("questpoint") {
        return true;
    }
    if varexists_mentions(&lower, &["quest", "solved", "complet"]) {
        return true;
    }
    let name_lower = name.to_ascii_lowercase();
    let quest_word = [
        "quest", "reward", "complet", "deliver", "fetch", "bring", "slay",
    ]
    .iter()
    .any(|word| name_lower.contains(word));
    quest_word
        && ["%load%", "%purge%", "%send%", "varexists"]
            .iter()
            .any(|needle| lower.contains(needle))
}

/// Find every `%actor.varexists(<inner>)%` and report whether any `<inner>`
/// contains one of `needles`.
fn varexists_mentions(lower: &str, needles: &[&str]) -> bool {
    const OPEN: &str = "%actor.varexists(";
    let mut rest = lower;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find(')') else {
            break;
        };
        let inner = &after[..end];
        if needles.iter().any(|needle| inner.contains(needle)) {
            return true;
        }
        rest = after;
    }
    false
}

/// True when a body line begins with an opening/closing/locking word.
fn line_transport(body: &str) -> bool {
    body.lines().any(|line| {
        let first = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(first.as_str(), "open" | "close" | "lock" | "unlock")
    })
}

/// True when a body token is a combat verb (`kill`, `mkill`, `dg_cast`,
/// `damage`, `hit`, `attack`) — used to separate a fighting `Random`/`Global`
/// trigger from a flavour one.
fn combat_token(body: &str) -> bool {
    const COMBAT: [&str; 6] = ["kill", "mkill", "dg_cast", "damage", "hit", "attack"];
    let lower = body.to_ascii_lowercase();
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| COMBAT.contains(&token))
}

/// Classify one trigger into its primary bucket. The rule order is the
/// precedence and is reproduced in `docs/design/triggers.md`.
pub fn classify(attach: &str, types: &[String], name: &str, body: &str) -> TriggerBucket {
    if quest_marker(body, name) {
        return TriggerBucket::QuestObjective;
    }
    let interactive = is_interactive(attach, types);
    let lower = body.to_ascii_lowercase();
    let transport = lower.contains("%teleport%")
        || lower.contains("%goto%")
        || lower.contains("%door%")
        || line_transport(body);
    if (is_gate(attach, types) || transport) && !interactive {
        return TriggerBucket::GatekeeperTransport;
    }
    if interactive {
        return TriggerBucket::InteractiveResponse;
    }
    if is_combat(attach, types) {
        return TriggerBucket::CombatBehaviour;
    }
    let ambient = types
        .iter()
        .any(|t| matches!(t.as_str(), "Random" | "Global" | "Load" | "Time" | "Login"));
    if ambient {
        return if combat_token(body) {
            TriggerBucket::CombatBehaviour
        } else {
            TriggerBucket::AmbienceFlavour
        };
    }
    TriggerBucket::AmbienceFlavour
}
