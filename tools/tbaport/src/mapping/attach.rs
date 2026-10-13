//! DG trigger attachments: which triggers each mob, object and room carries.
//!
//! The C builds an entity's trigger list in two steps, and both are ported
//! here:
//!
//! - **Prototype `T` lines.** `dg_read_trigger` (`dg_db_scripts.c:132`) and
//!   `dg_obj_trigger` (`:222`) append one `proto_script` node per `T <vnum>`
//!   line, in file order. A line naming a trigger that does not exist is
//!   logged and skipped; a trigger named twice is attached twice. Nothing
//!   compares the trigger's `attach_type` with the owner's kind: the type
//!   letters are simply read through the owner's table (`add_trigger`,
//!   `dg_scripts.c:946`, ORs the trigger's bits into the owner's script
//!   types), so a mismatched attachment is kept and reported.
//! - **Zone-reset `T` commands.** `reset_zone` (`db.c:3025`) attaches a fresh
//!   trigger copy to the mob or object the script just loaded (`tmob`/`tobj`)
//!   or to the named room. The converter has no instances at conversion time,
//!   so it folds each such command into the *prototype* list of its target,
//!   after the target's own `T` lines and only when the trigger is not
//!   already there. That is a deliberate deviation: the C attaches per
//!   instance, on every reset, so a prototype that also gets the trigger from
//!   a reset in some zones would, in the C, carry it only on the instances
//!   that zone loads.
//!
//! `V` reset commands (a script variable on the same targets) have no schema
//! home yet; each is reported with everything needed to apply it later.

use std::collections::BTreeSet;

use mib_tba::triggers::content::TriggerId;

use super::{AttachCandidate, ZoneSource, push_dangling, push_unmapped};
use crate::emit::OutputBundle;
use crate::ids;
use crate::report::{TriggerVariableDrop, ZoneReport};
use crate::source::zone::{RawCommand, RawZone};

/// `attach_type` of a trigger record, by the owner kinds that match it
/// (`MOB_TRIGGER` 0, `OBJ_TRIGGER` 1, `WLD_TRIGGER` 2; `dg_scripts.h:20`).
fn owner_attach_type(owner: AttachCandidate) -> i32 {
    match owner {
        AttachCandidate::Npc(_) => 0,
        AttachCandidate::Item(_) => 1,
        AttachCandidate::Room(_) => 2,
    }
}

/// The report spelling of an owner kind (`npc`, `item`, `room`).
pub(super) fn owner_kind(owner: &str) -> &str {
    owner.split_once(':').map_or("", |(kind, _)| kind)
}

/// Which converted entities exist, for the zone-reset target checks.
pub(super) struct Entities<'a> {
    pub rooms: &'a BTreeSet<mib_stdlib::content::RoomId>,
    pub npcs: &'a BTreeSet<mib_stdlib::content::NpcId>,
    pub items: &'a BTreeSet<mib_stdlib::content::ItemId>,
}

impl Entities<'_> {
    fn contains(&self, owner: AttachCandidate) -> bool {
        match owner {
            AttachCandidate::Npc(vnum) => self.npcs.contains(&ids::npc_id(vnum)),
            AttachCandidate::Item(vnum) => self.items.contains(&ids::item_id(vnum)),
            AttachCandidate::Room(vnum) => self.rooms.contains(&ids::room_id(vnum)),
        }
    }
}

/// Counts of what the zone-reset fold did, for the report.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct FoldStats {
    /// `T` commands that appended a new link.
    pub folded: usize,
    /// `T` commands whose trigger the target already listed.
    pub already_present: usize,
}

/// A `T` or `V` reset command and the entity the C would act on.
pub(super) struct ResetScript<'a> {
    /// Index into the zone's command list (the report's `command N`).
    pub index: usize,
    pub command: &'a RawCommand,
    /// `None` when the C finds nothing to act on: no preceding load of the
    /// requested kind, or an unknown target type.
    pub target: Option<AttachCandidate>,
}

/// Walk a zone's reset program and resolve the target of every `T` and `V`.
///
/// The C keeps `tmob` and `tobj` (`reset_zone`, `db.c:2866`): `M` sets `tmob`
/// and clears `tobj`; `O`, `G`, `E` and `P` set `tobj` and clear `tmob`;
/// `D` and `R` clear both; `T` and `V` leave them alone. A world target
/// (`arg1 == 2`) is the room in `arg3`. The walk is static, so it assumes
/// every load succeeds (the C skips a load that would exceed its `max`).
pub(super) fn reset_scripts(zone: &RawZone) -> Vec<ResetScript<'_>> {
    let mut scripts = Vec::new();
    let mut mob: Option<i32> = None;
    let mut obj: Option<i32> = None;
    for (index, command) in zone.commands.iter().enumerate() {
        match command.code {
            'M' => {
                mob = Some(command.arg1);
                obj = None;
            }
            'O' | 'G' | 'E' | 'P' => {
                obj = Some(command.arg1);
                mob = None;
            }
            'D' | 'R' => {
                mob = None;
                obj = None;
            }
            'T' | 'V' => {
                let target = match command.arg1 {
                    0 => mob.map(AttachCandidate::Npc),
                    1 => obj.map(AttachCandidate::Item),
                    2 => Some(AttachCandidate::Room(command.arg3)),
                    _ => None,
                };
                scripts.push(ResetScript {
                    index,
                    command,
                    target,
                });
            }
            _ => {}
        }
    }
    scripts
}

/// Append `trigger` to `owner`'s list, reporting an `attach_type` mismatch.
///
/// `kind` is the trigger's `attach_type`. The link is kept either way: the C
/// attaches regardless, and the report makes the mismatch visible.
fn push_link(
    bundle: &mut OutputBundle,
    zr: &mut ZoneReport,
    owner: AttachCandidate,
    trigger: i32,
    kind: i32,
) {
    let list = bundle
        .triggers
        .attachments
        .entry(owner.attach())
        .or_default();
    if kind != owner_attach_type(owner) {
        // The list position keeps a repeated link a distinct report entry.
        push_unmapped(
            zr,
            &owner.label(),
            "trigger_attach_type_mismatch",
            &format!("trigger {trigger}"),
            &format!(
                "the trigger is authored for attach_type {kind} but is listed by {} (position \
                 {}); the attachment is kept and its type letters are read through the owner's \
                 table, as the C does",
                owner.label(),
                list.len() + 1
            ),
        );
    }
    list.push(TriggerId::from_vnum(trigger));
}

/// Attach the `T` lines of every mob, object and room in one zone.
///
/// `kinds` maps every loaded trigger vnum to its `attach_type`; a `T` line
/// naming anything else is skipped, as `dg_read_trigger` skips it, and
/// reported.
pub(super) fn attach_prototype_lines(
    source: &ZoneSource,
    kinds: &std::collections::BTreeMap<i32, i32>,
    bundle: &mut OutputBundle,
    zr: &mut ZoneReport,
) {
    let owners = source
        .mobs
        .iter()
        .map(|mob| (AttachCandidate::Npc(mob.vnum), &mob.trigger_vnums))
        .chain(
            source
                .objs
                .iter()
                .map(|obj| (AttachCandidate::Item(obj.vnum), &obj.trigger_vnums)),
        )
        .chain(
            source
                .rooms
                .iter()
                .map(|room| (AttachCandidate::Room(room.vnum), &room.trigger_vnums)),
        );
    for (owner, lines) in owners {
        for trigger in lines {
            match kinds.get(trigger) {
                Some(kind) => push_link(bundle, zr, owner, *trigger, *kind),
                None => push_dangling(
                    zr,
                    "dangling_trigger_ref",
                    &format!("{} T line", owner.label()),
                    &format!("trigger {trigger}"),
                ),
            }
        }
    }
}

/// Fold one zone's reset `T` commands into the attachments and report its
/// `V` commands.
///
/// `renum_zone_table` (`db.c:1624`) disables a `T` whose trigger or room
/// argument names nothing (a mob or object `T` still needs a real room in
/// `arg3`), so those are dropped and reported rather than attached.
pub(super) fn fold_reset_scripts(
    source: &ZoneSource,
    kinds: &std::collections::BTreeMap<i32, i32>,
    entities: &Entities<'_>,
    bundle: &mut OutputBundle,
    zr: &mut ZoneReport,
) -> FoldStats {
    let mut stats = FoldStats::default();
    for ResetScript {
        index,
        command,
        target,
    } in reset_scripts(&source.zone)
    {
        let from = format!("zone {} command {index} ({})", source.number, command.code);
        if command.code == 'V' {
            zr.trigger_variables
                .push(variable_drop(source.number, index, command, target));
            continue;
        }

        let Some(kind) = kinds.get(&command.arg2).copied() else {
            push_dangling(
                zr,
                "dangling_zone_trigger",
                &from,
                &format!("trigger {}", command.arg2),
            );
            continue;
        };
        if !entities.rooms.contains(&ids::room_id(command.arg3)) {
            push_dangling(
                zr,
                "dangling_zone_trigger_room",
                &from,
                ids::room_id(command.arg3).as_ref(),
            );
            continue;
        }
        let Some(owner) = target else {
            push_unmapped(
                zr,
                &from,
                "zone_trigger_no_target",
                &format!("trigger {}", command.arg2),
                "no preceding load of the requested kind in this reset script; the C attaches \
                 nothing",
            );
            continue;
        };
        if !entities.contains(owner) {
            push_dangling(zr, "dangling_zone_trigger_target", &from, &owner.label());
            continue;
        }

        let id = TriggerId::from_vnum(command.arg2);
        let present = bundle
            .triggers
            .attachments
            .get(&owner.attach())
            .is_some_and(|list| list.contains(&id));
        if present {
            stats.already_present += 1;
        } else {
            push_link(bundle, zr, owner, command.arg2, kind);
            stats.folded += 1;
        }
    }
    stats
}

/// The report entry for one `V` reset command.
///
/// `reset_zone` (`db.c:3052`) passes `arg3` as the variable's context for a
/// mob or object target and `arg2` for a room target, so that is the
/// `context` recorded; the raw `arg2`/`arg3` are not otherwise kept.
fn variable_drop(
    zone: u32,
    index: usize,
    command: &RawCommand,
    target: Option<AttachCandidate>,
) -> TriggerVariableDrop {
    TriggerVariableDrop {
        kind: "zone_trigger_variable".to_owned(),
        zone,
        command: index,
        line: command.line,
        if_flag: command.if_flag,
        attach: match command.arg1 {
            0 => "mob",
            1 => "object",
            2 => "world",
            _ => "unknown",
        }
        .to_owned(),
        target: target.map(AttachCandidate::label),
        context: if command.arg1 == 2 {
            command.arg2
        } else {
            command.arg3
        },
        name: command.sarg1.clone(),
        value: command.sarg2.clone(),
    }
}
