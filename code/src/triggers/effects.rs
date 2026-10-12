//! Phase 3 side effects: persisted variables and the `&mut World` drain.
//!
//! The interpreter ([`crate::triggers::eval`]) is deliberately world-free: it
//! queues [`SpawnRequest`]s, [`PurgeRequest`]s, and [`VarWrite`]s on the
//! [`EvalOutcome`]. The event systems translate those into [`PendingTriggerOps`]
//! with resolved entities; [`trigger_effects_system`] is the one exclusive
//! system that drains them.
//!
//! This is the clean seam the design calls for. `%load%` routes to the same
//! stdlib spawn helpers the zone-reset engine uses ([`spawn_item_on_ground`],
//! [`spawn_item_carried_by_entity`], [`spawn_npc_from_content`]), all of which
//! take `&mut World`. Reaching for `Commands` instead would either defer the
//! spawn past the frame the trigger observed (so `%purge% %object%` in the same
//! record could not find the entity) or require the interpreter to depend on
//! Bevy at all. Queuing keeps evaluation pure and gives the tests a seam that
//! does not need a live schedule.
//!
//! ## Persistence
//!
//! [`TbaTriggerVars`] is registered with the stdlib
//! [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry) under
//! [`DocKind::Player`], so a quest variable survives reconnect. DG `remote`
//! writes its player's map; `global` rides the same component. User-defined
//! arrays are not persisted (they are deferred at the parser).

use std::collections::BTreeMap;

use mib_core::prelude::*;
use mib_stdlib::prelude::{
    ItemId, NpcId, QuestEvent, Room, find_item_def, spawn_item_carried_by_entity,
    spawn_item_on_ground, spawn_npc_from_content,
};
use serde::{Deserialize, Serialize};

use crate::triggers::ast::LoadKind;
use crate::triggers::context::NamedDrop;
use crate::triggers::eval::VarScope;

/// Per-player persisted DG variables (issue #15, Phase 3).
///
/// Registered under `DocKind::Player`, so `remote`/`global` writes survive a
/// reconnect and a restart. `remote` is the DG per-player scope (the quest
/// state in the corpus); `global` is stored on the setting player in this
/// phase. Cross-player `remote` is a named drop, matching the design's
/// "remote to offline players" deferral.
#[derive(Debug, Clone, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaTriggerVars {
    /// The player's `remote` variables, keyed lowercased.
    pub remote: BTreeMap<String, String>,
    /// The `global` variables visible to this player.
    pub global: BTreeMap<String, String>,
}

/// One resolved `%load%` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSpawn {
    pub kind: LoadKind,
    /// The already-substituted vnum text.
    pub vnum: String,
    /// The resolved holder for a carried item (`%actor%`/`%self%`), if any.
    pub victim: Option<Entity>,
    /// The room to spawn into (the firing's room).
    pub room: Option<Entity>,
    /// The unused `[wear]` argument, kept for the report.
    pub wear: Option<String>,
    /// The trigger owner, for drop attribution.
    pub owner: Entity,
}

/// One resolved `%purge%` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingPurge {
    pub target: Entity,
}

/// One resolved persisted variable write (`value: None` deletes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingVarWrite {
    pub owner: Entity,
    pub scope: VarScope,
    pub name: String,
    pub value: Option<String>,
}

/// A quest flag the record satisfied, for the `stdlib.quests` bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFlag {
    pub player: Entity,
    pub flag: String,
}

/// Effects queued by the trigger systems and drained by
/// [`trigger_effects_system`].
#[derive(Debug, Default, Resource)]
pub struct PendingTriggerOps {
    pub spawns: Vec<PendingSpawn>,
    pub purges: Vec<PendingPurge>,
    pub vars: Vec<PendingVarWrite>,
    pub flags: Vec<PendingFlag>,
    /// Runtime named drops (unknown tokens, deferred constructs, unresolved
    /// effect targets) so nothing is silent.
    pub drops: Vec<NamedDrop>,
}

impl PendingTriggerOps {
    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.spawns.is_empty()
            && self.purges.is_empty()
            && self.vars.is_empty()
            && self.flags.is_empty()
            && self.drops.is_empty()
    }
}

/// Accumulated runtime named-drop counts, keyed by kind.
///
/// The parse-time drops live on [`TbaTriggers`](crate::triggers::index::TbaTriggers);
/// this resource is the runtime counterpart, so an unknown token or a deferred
/// `%load%` recipient is counted rather than thrown away.
#[derive(Debug, Default, Resource)]
pub struct TriggerDropLog {
    pub by_kind: BTreeMap<String, usize>,
}

impl TriggerDropLog {
    /// Count one drop.
    pub fn record(&mut self, drop: &NamedDrop) {
        *self.by_kind.entry(drop.kind.clone()).or_default() += 1;
    }

    /// The number of drops of `kind`.
    pub fn count(&self, kind: &str) -> usize {
        self.by_kind.get(kind).copied().unwrap_or(0)
    }
}

/// The exclusive drain. Runs after the trigger intake in both `Update`
/// (`BridgeSet::Game`) and `FixedUpdate`.
///
/// `%load%`/`%purge%` need `&mut World` (the stdlib spawn helpers) and the
/// persisted variable + flag writes need get-or-insert component access, so one
/// exclusive system owns all of them rather than each trigger record reaching
/// for `Commands`.
pub fn trigger_effects_system(world: &mut World) {
    let ops = std::mem::take(&mut *world.resource_mut::<PendingTriggerOps>());
    for spawn in ops.spawns {
        apply_spawn(world, spawn);
    }
    for purge in ops.purges {
        if world.get_entity(purge.target).is_ok() {
            let _ = world.despawn(purge.target);
        }
    }
    for write in ops.vars {
        apply_var_write(world, write);
    }
    for flag in ops.flags {
        if let Some(mut events) = world.get_resource_mut::<Messages<QuestEvent>>() {
            events.write(QuestEvent::FlagSet {
                player: flag.player,
                flag: flag.flag.into(),
            });
        }
    }
    if !ops.drops.is_empty() {
        if world.get_resource::<TriggerDropLog>().is_none() {
            world.insert_resource(TriggerDropLog::default());
        }
        let mut log = world.resource_mut::<TriggerDropLog>();
        for drop in &ops.drops {
            log.record(drop);
        }
    }
}

/// Spawn one `%load%` request through the stdlib helpers.
///
/// DG's `%load%` carries no `max_in_world` argument, so it spawns
/// unconditionally; the zone-reset `max_in_world` convention applies only to
/// reset programs, which already enforce it in
/// [`zone_reset_system`](crate::zones::zone_reset_system).
fn apply_spawn(world: &mut World, spawn: PendingSpawn) {
    let Some(vnum) = parse_vnum(&spawn.vnum) else {
        return;
    };
    match spawn.kind {
        LoadKind::Obj => {
            let item_id = ItemId::from(format!("tba_item_{vnum}"));
            let Some(def) = find_item_def(world, &item_id) else {
                return;
            };
            if let Some(victim) = spawn
                .victim
                .filter(|entity| world.get_entity(*entity).is_ok())
            {
                spawn_item_carried_by_entity(world, victim, &item_id, &def);
            } else if let Some(room) = spawn
                .room
                .filter(|entity| world.get_entity(*entity).is_ok())
            {
                spawn_item_on_ground(world, room, &item_id, &def);
            }
        }
        LoadKind::Mob => {
            let npc_id = NpcId::from(format!("tba_npc_{vnum}"));
            if let Some(room) = spawn
                .room
                .filter(|entity| world.get_entity(*entity).is_ok())
                && let Some(key) = world.get::<Room>(room).map(|room| room.key.clone())
            {
                spawn_npc_from_content(world, &key.as_str().into(), &npc_id);
            }
        }
    }
}

/// Apply one persisted write, creating the component on first use.
fn apply_var_write(world: &mut World, write: PendingVarWrite) {
    if world.get::<TbaTriggerVars>(write.owner).is_none() {
        if world.get_entity(write.owner).is_err() {
            return;
        }
        world
            .entity_mut(write.owner)
            .insert(TbaTriggerVars::default());
    }
    let Some(mut vars) = world.get_mut::<TbaTriggerVars>(write.owner) else {
        return;
    };
    let map = match write.scope {
        VarScope::Remote => &mut vars.remote,
        VarScope::Global => &mut vars.global,
    };
    match write.value {
        Some(value) => {
            map.insert(write.name, value);
        }
        None => {
            map.remove(&write.name);
        }
    }
}

/// Parse a `%load%` vnum after substitution.
fn parse_vnum(text: &str) -> Option<i32> {
    text.trim().parse::<i32>().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parse_vnum_rejects_junk() {
        assert_eq!(parse_vnum(" 1300 "), Some(1300));
        assert_eq!(parse_vnum(""), None);
        assert_eq!(parse_vnum("%self.vnum%"), None);
    }

    #[test]
    fn var_write_creates_the_component_and_deletes() {
        let mut app = App::new();
        app.init_resource::<PendingTriggerOps>();
        app.add_message::<QuestEvent>();
        let player = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<PendingTriggerOps>()
            .vars
            .push(PendingVarWrite {
                owner: player,
                scope: VarScope::Remote,
                name: "solved".to_owned(),
                value: Some("1".to_owned()),
            });
        trigger_effects_system(app.world_mut());
        let vars = app
            .world()
            .get::<TbaTriggerVars>(player)
            .expect("component");
        assert_eq!(vars.remote.get("solved").map(String::as_str), Some("1"));

        app.world_mut()
            .resource_mut::<PendingTriggerOps>()
            .vars
            .push(PendingVarWrite {
                owner: player,
                scope: VarScope::Remote,
                name: "solved".to_owned(),
                value: None,
            });
        trigger_effects_system(app.world_mut());
        let vars = app
            .world()
            .get::<TbaTriggerVars>(player)
            .expect("component");
        assert!(!vars.remote.contains_key("solved"));
    }

    #[test]
    fn flag_write_emits_a_quest_event() {
        let mut app = App::new();
        app.init_resource::<PendingTriggerOps>();
        app.add_message::<QuestEvent>();
        let player = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<PendingTriggerOps>()
            .flags
            .push(PendingFlag {
                player,
                flag: "solved_tutorial_quest_zone_0".to_owned(),
            });
        trigger_effects_system(app.world_mut());
        let events: Vec<QuestEvent> = app
            .world_mut()
            .resource_mut::<Messages<QuestEvent>>()
            .drain()
            .collect();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            QuestEvent::FlagSet { player: p, flag } if *p == player && flag.as_str() == "solved_tutorial_quest_zone_0"
        ));
    }
}
