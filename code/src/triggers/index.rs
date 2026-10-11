//! `TriggerIndex` and the parsed-trigger resource (issue #15, Phase 1/2).
//!
//! The index maps an attachment key (`npc:tba_npc_3`, `item:tba_item_16`,
//! `room:tba.22`) and an event type (`speech`, `command`, `greet`…) to the
//! trigger ids that should fire. It is rebuilt at world build/reload from
//! [`TbaContent::triggers`](crate::module::TbaContent) exactly like
//! `RoomIndex`; the runtime never re-parses a script mid-tick.

use std::collections::BTreeMap;

use mib_core::prelude::Resource;

use crate::triggers::ast::{ParsedProgram, parse_script};
use crate::triggers::content::{TriggerDef, TriggerId};
use crate::triggers::context::NamedDrop;

/// Attachment key + event type -> trigger ids, in stable id order.
#[derive(Debug, Clone, Default)]
pub struct TriggerIndex {
    by_attach: BTreeMap<String, BTreeMap<String, Vec<TriggerId>>>,
}

impl TriggerIndex {
    /// Build the index from the trigger definitions.
    pub fn build(defs: &BTreeMap<TriggerId, TriggerDef>) -> Self {
        let mut index = TriggerIndex::default();
        for (id, def) in defs {
            if def.attach.trim().is_empty() {
                // An unattached trigger never fires; the converter recorded the
                // dangling attachment in the drop report.
                continue;
            }
            for ty in &def.types {
                let ty = ty.trim().to_ascii_lowercase();
                if ty.is_empty() {
                    continue;
                }
                let ids = index
                    .by_attach
                    .entry(def.attach.clone())
                    .or_default()
                    .entry(ty)
                    .or_default();
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        for types in index.by_attach.values_mut() {
            for ids in types.values_mut() {
                ids.sort();
                ids.dedup();
            }
        }
        index
    }

    /// The triggers attached to `attach` for event type `ty`.
    pub fn lookup(&self, attach: &str, ty: &str) -> &[TriggerId] {
        self.by_attach
            .get(attach)
            .and_then(|types| types.get(ty))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The number of distinct attachment keys.
    pub fn attach_count(&self) -> usize {
        self.by_attach.len()
    }

    /// Whether the index holds no triggers.
    pub fn is_empty(&self) -> bool {
        self.by_attach.is_empty()
    }
}

/// The parsed trigger corpus: the definitions, their ASTs, the index, and the
/// parse-time named drops.
#[derive(Debug, Clone, Default, Resource)]
pub struct TbaTriggers {
    /// The section definitions keyed by id.
    pub defs: BTreeMap<TriggerId, TriggerDef>,
    /// One parsed program per definition.
    pub programs: BTreeMap<TriggerId, ParsedProgram>,
    /// Attachment/type lookup.
    pub index: TriggerIndex,
    /// Scripts whose parse failed structurally (a named drop each).
    pub parse_drops: Vec<NamedDrop>,
}

impl TbaTriggers {
    /// Parse every definition and build the index.
    pub fn build(defs: BTreeMap<TriggerId, TriggerDef>) -> Self {
        let mut programs = BTreeMap::new();
        let mut parse_drops = Vec::new();
        for (id, def) in &defs {
            let program = parse_script(&def.script);
            if let Some(error) = &program.error {
                parse_drops.push(NamedDrop::new(
                    "trigger_parse_error",
                    format!("{id}: {error}"),
                ));
            }
            for (kind, count) in &program.unsupported {
                parse_drops.push(NamedDrop::new(
                    format!("unsupported:{kind}"),
                    format!("{id}: {count} line(s)"),
                ));
            }
            programs.insert(id.clone(), program);
        }
        let index = TriggerIndex::build(&defs);
        Self {
            defs,
            programs,
            index,
            parse_drops,
        }
    }

    /// The parsed program for a trigger id.
    pub fn program(&self, id: &TriggerId) -> Option<&ParsedProgram> {
        self.programs.get(id)
    }

    /// The definition for a trigger id.
    pub fn def(&self, id: &TriggerId) -> Option<&TriggerDef> {
        self.defs.get(id)
    }

    /// Whether there are no triggers.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn def(vnum: i32, attach: &str, types: &[&str]) -> TriggerDef {
        TriggerDef {
            vnum,
            name: format!("trigger {vnum}"),
            attach: attach.to_owned(),
            types: types.iter().map(|t| (*t).to_owned()).collect(),
            narg: 100,
            arglist: String::new(),
            script: "say hello\n".to_owned(),
        }
    }

    #[test]
    fn index_groups_by_attach_and_type() {
        let mut defs = BTreeMap::new();
        defs.insert(TriggerId::from_vnum(1), def(1, "npc:tba_npc_5", &["greet"]));
        defs.insert(
            TriggerId::from_vnum(2),
            def(2, "npc:tba_npc_5", &["greet", "speech"]),
        );
        defs.insert(TriggerId::from_vnum(3), def(3, "room:tba.3001", &["enter"]));
        defs.insert(TriggerId::from_vnum(4), def(4, "", &["greet"]));
        let triggers = TbaTriggers::build(defs);
        assert_eq!(triggers.index.attach_count(), 2);
        assert_eq!(
            triggers.index.lookup("npc:tba_npc_5", "greet"),
            &[TriggerId::from_vnum(1), TriggerId::from_vnum(2)]
        );
        assert_eq!(
            triggers.index.lookup("npc:tba_npc_5", "speech"),
            &[TriggerId::from_vnum(2)]
        );
        assert!(triggers.index.lookup("npc:tba_npc_5", "command").is_empty());
        assert!(triggers.index.lookup("npc:tba_npc_9", "greet").is_empty());
        // The unattached trigger is parsed but indexed nowhere.
        assert!(triggers.program(&TriggerId::from_vnum(4)).is_some());
        assert_eq!(triggers.index.lookup("", "greet"), &[] as &[TriggerId]);
    }
}
