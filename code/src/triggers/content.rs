//! `tba.triggers` — the DG script corpus the converter emits for the essential
//! interactivity set (issue #15, `docs/design/triggers.md` §6.3).
//!
//! This is the DG record plus its attachment, **not** a re-authoring: the
//! converter writes the source attach target (`npc:`/`item:`/`room:`), the
//! lowercased expanded type letters, the `narg`, the DG `arglist`, and the
//! script body verbatim (colour-stripped). The runtime parses `script` once at
//! world build/reload into an AST ([`crate::triggers::ast`]).
//!
//! The section keys by a stable [`TriggerId`] (`tba_trig_<vnum>`) so a script
//! survives a re-conversion even if the zone file moves.

use std::collections::BTreeMap;

use mib_content::prelude::{PackSection, SectionRegistry, ValidationCtx};
use mib_stdlib::prelude::{ItemId, ItemsSection, NpcId, NpcsSection, RoomId, RoomsSection};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Define a string-backed id newtype. Mirrors the stdlib `id_type!` macro; the
/// macro is private to `mib-stdlib`, so the pack defines its own.
macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub SmolStr);

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(SmolStr::new(value))
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(SmolStr::new(value))
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

id_type!(TriggerId);

impl TriggerId {
    /// The converter's stable id for a trigger vnum (`tba_trig_<vnum>`).
    pub fn from_vnum(vnum: i32) -> Self {
        Self(SmolStr::new(format!("tba_trig_{vnum}")))
    }
}

/// One DG trigger record.
///
/// `attach` names the source entity: `npc:<id>`, `item:<id>`, or `room:<id>`.
/// It is empty for a trigger the converter could not attach to any emitted
/// entity (a named dangling drop, not a silent one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerDef {
    /// The DG trigger vnum (`#<vnum>` in the `.trg` record).
    pub vnum: i32,
    /// The trigger name (the `name~` field).
    pub name: String,
    /// The resolved attachment (`npc:tba_npc_3` / `item:tba_item_16` /
    /// `room:tba.22`), or empty when unattached.
    #[serde(default)]
    pub attach: String,
    /// The lowercased expanded type letters (`["speech"]`, `["greet-all"]`).
    #[serde(default)]
    pub types: Vec<String>,
    /// The DG `narg` field.
    #[serde(default)]
    pub narg: i32,
    /// The DG `arglist` (match text for speech/command).
    #[serde(default)]
    pub arglist: String,
    /// The script body, verbatim and colour-stripped.
    pub script: String,
}

impl TriggerDef {
    /// The three-part split of [`TriggerDef::attach`], or `None` when it is
    /// empty/malformed.
    pub fn attach_target(&self) -> Option<AttachTarget<'_>> {
        AttachTarget::parse(&self.attach)
    }
}

/// A parsed `attach` reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachTarget<'a> {
    /// `npc:<id>`.
    Npc(&'a str),
    /// `item:<id>`.
    Item(&'a str),
    /// `room:<id>`.
    Room(&'a str),
}

impl<'a> AttachTarget<'a> {
    /// Parse `<prefix>:<id>`. An unknown prefix is `None`.
    pub fn parse(attach: &'a str) -> Option<Self> {
        let (prefix, id) = attach.split_once(':')?;
        if id.is_empty() {
            return None;
        }
        match prefix {
            "npc" => Some(AttachTarget::Npc(id)),
            "item" => Some(AttachTarget::Item(id)),
            "room" => Some(AttachTarget::Room(id)),
            _ => None,
        }
    }
}

/// `tba.triggers` — keyed by [`TriggerId`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggersFile {
    pub triggers: BTreeMap<TriggerId, TriggerDef>,
}

/// `tba.triggers`.
pub struct TriggersSection;

impl PackSection for TriggersSection {
    const ID: &'static str = "tba.triggers";
    type Defs = TriggersFile;

    fn validate(defs: &Self::Defs, ctx: &mut ValidationCtx<'_>) {
        let npcs = npc_ids(ctx);
        let items = item_ids(ctx);
        let rooms = room_ids(ctx);

        for (id, trigger) in &defs.triggers {
            let field = format!("triggers.{id}");
            if trigger.name.trim().is_empty() {
                ctx.error_at(format!("{field}.name"), "trigger name must not be empty");
            }
            if trigger.script.trim().is_empty() {
                ctx.error_at(
                    format!("{field}.script"),
                    "trigger script must not be empty",
                );
            }
            for (index, ty) in trigger.types.iter().enumerate() {
                if ty.trim().is_empty() || ty != &ty.to_ascii_lowercase() {
                    ctx.error_at(
                        format!("{field}.types[{index}]"),
                        format!("trigger type `{ty}` must be a non-empty lowercase name"),
                    );
                }
            }
            if trigger.attach.trim().is_empty() {
                // An unattached trigger is legal content (the converter records
                // it as a named dangling drop); it simply never fires.
                continue;
            }
            match trigger.attach_target() {
                Some(AttachTarget::Npc(npc)) => {
                    require_npc(ctx, &format!("{field}.attach"), &NpcId::from(npc), &npcs)
                }
                Some(AttachTarget::Item(item)) => {
                    require_item(ctx, &format!("{field}.attach"), &ItemId::from(item), &items)
                }
                Some(AttachTarget::Room(room)) => {
                    require_room(ctx, &format!("{field}.attach"), &RoomId::from(room), &rooms)
                }
                None => ctx.error_at(
                    format!("{field}.attach"),
                    format!(
                        "attach `{}` must be `npc:<id>`, `item:<id>`, or `room:<id>`",
                        trigger.attach
                    ),
                ),
            }
        }
    }
}

/// Register `tba.triggers`. Called from `crate::content::register`.
pub fn register(registry: &mut SectionRegistry) {
    registry.register::<TriggersSection>();
}

fn npc_ids(ctx: &ValidationCtx<'_>) -> BTreeMap<NpcId, ()> {
    ctx.section::<NpcsSection>()
        .map(|npcs| npcs.npcs.keys().cloned().map(|id| (id, ())).collect())
        .unwrap_or_default()
}

fn item_ids(ctx: &ValidationCtx<'_>) -> BTreeMap<ItemId, ()> {
    ctx.section::<ItemsSection>()
        .map(|items| items.items.keys().cloned().map(|id| (id, ())).collect())
        .unwrap_or_default()
}

fn room_ids(ctx: &ValidationCtx<'_>) -> BTreeMap<RoomId, ()> {
    ctx.section::<RoomsSection>()
        .map(|rooms| rooms.rooms.keys().cloned().map(|id| (id, ())).collect())
        .unwrap_or_default()
}

fn require_npc(ctx: &mut ValidationCtx<'_>, field: &str, id: &NpcId, npcs: &BTreeMap<NpcId, ()>) {
    if !npcs.contains_key(id) {
        ctx.error_at(field.to_owned(), format!("unknown npc `{id}`"));
    }
}

fn require_item(
    ctx: &mut ValidationCtx<'_>,
    field: &str,
    id: &ItemId,
    items: &BTreeMap<ItemId, ()>,
) {
    if !items.contains_key(id) {
        ctx.error_at(field.to_owned(), format!("unknown item `{id}`"));
    }
}

fn require_room(
    ctx: &mut ValidationCtx<'_>,
    field: &str,
    id: &RoomId,
    rooms: &BTreeMap<RoomId, ()>,
) {
    if !rooms.contains_key(id) {
        ctx.error_at(field.to_owned(), format!("unknown room `{id}`"));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn trigger_id_is_the_stable_tba_prefix() {
        assert_eq!(TriggerId::from_vnum(139).as_ref(), "tba_trig_139");
    }

    #[test]
    fn attach_target_parses_the_three_families() {
        assert_eq!(
            AttachTarget::parse("npc:tba_npc_3"),
            Some(AttachTarget::Npc("tba_npc_3"))
        );
        assert_eq!(
            AttachTarget::parse("item:tba_item_16"),
            Some(AttachTarget::Item("tba_item_16"))
        );
        assert_eq!(
            AttachTarget::parse("room:tba.22"),
            Some(AttachTarget::Room("tba.22"))
        );
        assert_eq!(AttachTarget::parse(""), None);
        assert_eq!(AttachTarget::parse("mob:tba_npc_3"), None);
        assert_eq!(AttachTarget::parse("npc:"), None);
    }
}
