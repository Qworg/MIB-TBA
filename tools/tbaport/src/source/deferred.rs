//! Deferred section reader: `.trg`.
//!
//! DG scripts are **not** emitted yet (doc §10). Each record is still scanned
//! so the drop report names its vnum and attachment point, which is what a
//! later phase needs to rebuild them. The scan is faithful to the source
//! grammar so it cannot mis-count records:
//!
//! - `.trg` — `parse_trigger` (`dg_db_scripts.c:30`): `#vnum`, name, attach
//!   line (`<attach_type> <flags> <narg>`), arglist, command list.
//!
//! Quests (`.qst`) used to be scanned here; they are now parsed and emitted as
//! `stdlib.quests` by [`crate::source::quest`]. `.shp` used to be scanned here
//! too; shops are now parsed into real content by [`crate::source::shop`].
//!
//! [`DeferredRecord`] is still the shared record type for the report entries.

use crate::error::Result;
use crate::source::tokens::Source;

/// A deferred record: enough metadata to rebuild the section later.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeferredRecord {
    /// Record vnum (trigger vnum, shopkeeper mob vnum, or quest vnum).
    pub vnum: i32,
    /// What the record attaches to: `npc`, `mob`, `object`, `world`.
    pub attach: String,
    /// Free-form detail (trigger name, quest name, questmaster).
    pub detail: String,
}

/// Scan a `.trg` file.
pub fn scan_triggers(src: &mut Source) -> Result<Vec<DeferredRecord>> {
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
        let attach_type: i32 = attach_line
            .split_whitespace()
            .next()
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        let attach = match attach_type {
            0 => "mob",
            1 => "object",
            2 => "world",
            _ => "unknown",
        };
        // Consume the arg list and the script body so the next record's `#`
        // is read at the right level.
        let _arglist = src.read_string("trigger arglist")?;
        let _body = src.read_string("trigger body")?;
        records.push(DeferredRecord {
            vnum,
            attach: attach.to_owned(),
            detail: name,
        });
    }
    records.sort();
    Ok(records)
}
