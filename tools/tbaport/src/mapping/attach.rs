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
//!   that zone loads. Every folded command, and every one skipped because the
//!   target already listed the trigger, is itemised in its zone's report
//!   (`zone_trigger_links`).
//!
//! `V` reset commands (a script variable on the same targets) have no schema
//! home yet; each is reported with everything needed to apply it later.
//!
//! The reset walk follows `renum_zone_table` (`db.c:1623`), which turns a
//! command naming a missing vnum into a disabled `*` at boot, and the
//! `if_flag`/`last_cmd` gate at the top of `reset_zone` (`db.c:2869`).

use std::collections::{BTreeMap, BTreeSet};

use mib_tba::triggers::content::TriggerId;

use super::{AttachCandidate, ZoneSource, push_dangling, push_unmapped};
use crate::emit::OutputBundle;
use crate::ids;
use crate::report::{TriggerVariableDrop, ZoneReport, ZoneTriggerLink};
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

/// What exists in the converted world, for the reset-script checks that
/// `renum_zone_table` makes with `real_mobile`, `real_object`, `real_room`
/// and `real_trigger`.
pub(super) struct Entities<'a> {
    pub rooms: &'a BTreeSet<mib_stdlib::content::RoomId>,
    pub npcs: &'a BTreeSet<mib_stdlib::content::NpcId>,
    pub items: &'a BTreeSet<mib_stdlib::content::ItemId>,
    /// Every loaded trigger vnum with its `attach_type`.
    pub triggers: &'a BTreeMap<i32, i32>,
    /// The zone file each mob, object and room lives in.
    pub zones: BTreeMap<AttachCandidate, u32>,
}

impl Entities<'_> {
    fn room(&self, vnum: i32) -> bool {
        self.rooms.contains(&ids::room_id(vnum))
    }

    fn item(&self, vnum: i32) -> bool {
        self.items.contains(&ids::item_id(vnum))
    }

    /// Whether `renum_zone_table` keeps the command (every vnum argument it
    /// converts resolves) rather than disabling it.
    fn keeps(&self, command: &RawCommand) -> bool {
        let (a1, a2, a3) = (command.arg1, command.arg2, command.arg3);
        match command.code {
            'M' => self.npcs.contains(&ids::npc_id(a1)) && self.room(a3),
            // An `O` with room -1 (`NOWHERE`) is not renumbered.
            'O' => self.item(a1) && (a3 == -1 || self.room(a3)),
            'G' | 'E' => self.item(a1),
            'P' => self.item(a1) && self.item(a3),
            'D' => self.room(a1),
            'R' => self.room(a1) && self.item(a2),
            'T' => self.triggers.contains_key(&a2) && self.room(a3),
            'V' => self.room(a3),
            _ => true,
        }
    }
}

/// What the reset walk finds for a `T` or `V` command.
pub(super) enum ResetFate {
    /// `renum_zone_table` disabled the command (a vnum argument is missing).
    Disabled,
    /// The `if_flag`/`last_cmd` gate skips it, because the command before it
    /// was disabled (or itself skipped).
    Skipped,
    /// It runs; `None` when the C finds nothing to act on (no preceding load
    /// of the requested kind, or an unknown target type).
    Runs(Option<AttachCandidate>),
}

/// A `T` or `V` reset command and what the C does with it.
pub(super) struct ResetScript<'a> {
    /// Index into the zone's command list (the report's `command N`).
    pub index: usize,
    pub command: &'a RawCommand,
    pub fate: ResetFate,
}

/// Walk a zone's reset program and resolve the target of every `T` and `V`.
///
/// The C keeps `tmob` and `tobj` (`reset_zone`, `db.c:2866`): `M` sets `tmob`
/// and clears `tobj`; `O`, `G`, `E` and `P` set `tobj` and clear `tmob`;
/// `D` and `R` clear both; `T` and `V` leave them alone. A world target
/// (`arg1 == 2`) is the room in `arg3`. A command `renum_zone_table` disabled
/// only clears `last_cmd`, leaving `tmob` and `tobj` as they were, and a
/// command with a non-zero `if_flag` is skipped while `last_cmd` is clear.
/// The walk is static, so it assumes every load succeeds (the C skips a load
/// that would exceed its `max`) and so that `last_cmd` is set after every
/// command that runs.
pub(super) fn reset_scripts<'a>(
    zone: &'a RawZone,
    entities: &Entities<'_>,
) -> Vec<ResetScript<'a>> {
    let mut scripts = Vec::new();
    let mut mob: Option<i32> = None;
    let mut obj: Option<i32> = None;
    let mut last_cmd = false;
    for (index, command) in zone.commands.iter().enumerate() {
        let script = matches!(command.code, 'T' | 'V');
        let fate = if command.if_flag != 0 && !last_cmd {
            Some(ResetFate::Skipped)
        } else if !entities.keeps(command) {
            last_cmd = false;
            Some(ResetFate::Disabled)
        } else {
            None
        };
        if let Some(fate) = fate {
            if script {
                scripts.push(ResetScript {
                    index,
                    command,
                    fate,
                });
            }
            continue;
        }
        match command.code {
            'M' => {
                mob = Some(command.arg1);
                obj = None;
                last_cmd = true;
            }
            'O' | 'G' | 'E' | 'P' => {
                obj = Some(command.arg1);
                mob = None;
                last_cmd = true;
            }
            'D' | 'R' => {
                mob = None;
                obj = None;
                last_cmd = true;
            }
            'T' | 'V' => {
                let target = match command.arg1 {
                    0 => mob.map(AttachCandidate::Npc),
                    1 => obj.map(AttachCandidate::Item),
                    2 => Some(AttachCandidate::Room(command.arg3)),
                    _ => None,
                };
                // Only a command that found its target sets `last_cmd`.
                last_cmd |= target.is_some();
                scripts.push(ResetScript {
                    index,
                    command,
                    fate: ResetFate::Runs(target),
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
/// attaches regardless, and the report makes the mismatch visible. `zone` is
/// the zone file the owner lives in; it decides the owner list's shard.
fn push_link(
    bundle: &mut OutputBundle,
    zr: &mut ZoneReport,
    owner: AttachCandidate,
    zone: u32,
    trigger: i32,
    kind: i32,
) {
    bundle.attachment_zones.insert(owner.attach(), zone);
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
    kinds: &BTreeMap<i32, i32>,
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
                Some(kind) => push_link(bundle, zr, owner, source.number, *trigger, *kind),
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
/// A `T` command that `renum_zone_table` disabled is reported as the dangling
/// reference that disabled it rather than attached.
pub(super) fn fold_reset_scripts(
    source: &ZoneSource,
    entities: &Entities<'_>,
    bundle: &mut OutputBundle,
    zr: &mut ZoneReport,
) {
    for ResetScript {
        index,
        command,
        fate,
    } in reset_scripts(&source.zone, entities)
    {
        let from = format!("zone {} command {index} ({})", source.number, command.code);
        let target = match fate {
            ResetFate::Runs(target) => target,
            ResetFate::Skipped | ResetFate::Disabled => None,
        };
        if command.code == 'V' {
            zr.trigger_variables
                .push(variable_drop(source.number, index, command, target));
            continue;
        }

        match fate {
            ResetFate::Skipped => push_unmapped(
                zr,
                &from,
                "zone_trigger_skipped",
                &format!("trigger {}", command.arg2),
                "the command follows a disabled command and has a non-zero if_flag, so \
                 reset_zone skips it",
            ),
            ResetFate::Disabled if !entities.triggers.contains_key(&command.arg2) => push_dangling(
                zr,
                "dangling_zone_trigger",
                &from,
                &format!("trigger {}", command.arg2),
            ),
            ResetFate::Disabled => push_dangling(
                zr,
                "dangling_zone_trigger_room",
                &from,
                ids::room_id(command.arg3).as_ref(),
            ),
            ResetFate::Runs(None) => push_unmapped(
                zr,
                &from,
                "zone_trigger_no_target",
                &format!("trigger {}", command.arg2),
                "no preceding load of the requested kind in this reset script; the C attaches \
                 nothing",
            ),
            ResetFate::Runs(Some(owner)) => {
                let Some(kind) = entities.triggers.get(&command.arg2).copied() else {
                    continue;
                };
                let id = TriggerId::from_vnum(command.arg2);
                let present = bundle
                    .triggers
                    .attachments
                    .get(&owner.attach())
                    .is_some_and(|list| list.contains(&id));
                if !present {
                    // A kept command's target was loaded by an earlier command,
                    // so its zone is known.
                    let zone = entities.zones.get(&owner).copied().unwrap_or(source.number);
                    push_link(bundle, zr, owner, zone, command.arg2, kind);
                }
                zr.zone_trigger_links.push(ZoneTriggerLink {
                    zone: source.number,
                    command: index,
                    line: command.line,
                    if_flag: command.if_flag,
                    owner: owner.label(),
                    trigger: command.arg2,
                    outcome: if present { "already_present" } else { "folded" }.to_owned(),
                });
            }
        }
    }
}

/// The report entry for one `V` reset command.
///
/// `context` and `room` are the command's raw `arg2` and `arg3`; the C passes
/// `arg3` (the room's rnum after `renum_zone_table`) as the variable's context
/// for a mob or object target and `arg2` for a room target (`reset_zone`,
/// `db.c:3057`, `:3064`, `:3075`), which a later change applying the variable
/// can derive from the two fields and `attach`.
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
        context: command.arg2,
        room: command.arg3,
        name: command.sarg1.clone(),
        value: command.sarg2.clone(),
    }
}
