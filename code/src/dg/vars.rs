//! Per-player persisted DG variables and the quest bridge.
//!
//! DG saves a player's script globals with the character. Here a player's
//! globals live in [`DgScript`] like everyone else's, and are mirrored into
//! the persisted [`TbaTriggerVars`]:
//!
//! - when a player first gets a script it is seeded from [`TbaTriggerVars`]
//!   (the earlier runtime wrote two maps; both are loaded, `remote` winning on
//!   a clash, in context 0);
//! - when a with-host call ends, a player whose context-0 globals differ from
//!   what was last mirrored has them written to [`TbaTriggerVars::remote`] and
//!   [`TbaTriggerVars::global`] is emptied, so `remote` is the only map
//!   written from then on;
//! - a global that was absent or falsy before and is truthy now (non-empty and
//!   not starting with `0`) is a quest flag: a [`QuestEvent::FlagSet`] is
//!   written for the `stdlib.quests` machinery, as a truthy `remote` write
//!   was before.

use std::collections::BTreeMap;

use bevy::ecs::change_detection::Tick;
use dg_vm::Script;
use mib_core::prelude::*;
use mib_stdlib::prelude::{PendingBundleRestore, Player, QuestEvent};
use serde::{Deserialize, Serialize};

use super::script::DgScript;

/// Per-player persisted DG variables.
///
/// Registered under `DocKind::Player`, so quest variables survive a reconnect
/// and a restart. `remote` is the DG per-player scope (the quest state in the
/// corpus) and the only map written; `global` is read once, to seed a player's
/// script from a save made by the earlier runtime, and is emptied at the first
/// write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaTriggerVars {
    /// The player's `remote` variables, keyed lowercased.
    pub remote: BTreeMap<String, String>,
    /// The `global` variables visible to this player.
    pub global: BTreeMap<String, String>,
}

/// The context-0 globals last mirrored into [`TbaTriggerVars`] (or loaded from
/// it), the baseline a later change is measured against.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct DgVarSync(pub BTreeMap<String, String>);

/// A quest flag's truth: non-empty and not starting with `0`.
fn truthy(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('0')
}

/// The persisted variables as a script's context-0 globals: `global` first,
/// then `remote` over it.
fn seeded_vars(vars: Option<&TbaTriggerVars>) -> BTreeMap<String, String> {
    let Some(vars) = vars else {
        return BTreeMap::new();
    };
    let mut merged = BTreeMap::new();
    for (name, value) in vars.global.iter().chain(vars.remote.iter()) {
        merged.insert(name.to_ascii_lowercase(), value.clone());
    }
    merged
}

/// A script's context-0 globals by lowercased name; the first entry for a
/// name (the head of the list, the one lookups find) wins.
fn context_zero_globals(script: &Script) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for var in script
        .global_vars
        .vars
        .iter()
        .filter(|var| var.context == 0)
    {
        map.entry(var.name.to_ascii_lowercase())
            .or_insert_with(|| var.value.clone());
    }
    map
}

/// Give a player a script seeded from [`TbaTriggerVars`], if it has none.
pub fn seed_player_script(world: &mut World, entity: Entity) {
    if world.get_entity(entity).is_err() || world.get::<DgScript>(entity).is_some() {
        return;
    }
    let seeded = seeded_vars(world.get::<TbaTriggerVars>(entity));
    let mut script = Script::default();
    let mut baseline = BTreeMap::new();
    for (name, value) in seeded {
        if script.global_vars.add_var(&name, &value, 0) {
            baseline.insert(name, value);
        }
    }
    world
        .entity_mut(entity)
        .insert((DgScript(script), DgVarSync(baseline)));
}

/// Give every player without a script one. A player whose saved character has
/// not been restored yet waits for the restore, so the seed sees its variables.
pub fn seed_player_scripts(world: &mut World) {
    let mut players = world.query_filtered::<Entity, (
        With<Player>,
        Without<DgScript>,
        Without<PendingBundleRestore>,
    )>();
    let fresh: Vec<Entity> = players.iter(world).collect();
    for entity in fresh {
        seed_player_script(world, entity);
    }
}

/// Take into the script the persisted variables the script has never seen: a
/// `TbaTriggerVars` restored after the player's script was created. They join
/// both the baseline and the current globals, so they are neither lost at the
/// next write nor reported as new quest flags.
fn adopt_late_vars(
    world: &mut World,
    entity: Entity,
    before: &mut BTreeMap<String, String>,
    current: &mut BTreeMap<String, String>,
) {
    let late: Vec<(String, String)> = seeded_vars(world.get::<TbaTriggerVars>(entity))
        .into_iter()
        .filter(|(name, _)| {
            holdable(name) && !before.contains_key(name) && !current.contains_key(name)
        })
        .collect();
    if late.is_empty() {
        return;
    }
    if let Some(mut script) = world.get_mut::<DgScript>(entity) {
        for (name, value) in late {
            if script.0.global_vars.add_var(&name, &value, 0) {
                before.insert(name.clone(), value.clone());
                current.insert(name, value);
            }
        }
    }
}

/// The tick of the last [`sync_player_vars`] pass: a player whose script and
/// persisted variables have not changed since is skipped.
#[derive(Resource, Debug, Default)]
pub struct DgVarClock(Option<Tick>);

/// Whether a script can hold a variable of this name (`add_var` refuses
/// dotted names).
fn holdable(name: &str) -> bool {
    !name.contains('.')
}

/// Mirror every player's changed globals into [`TbaTriggerVars`] and emit the
/// quest flags that became truthy.
pub fn sync_player_vars(world: &mut World) {
    world.init_resource::<DgVarClock>();
    let last = world.resource::<DgVarClock>().0;
    let now = world.read_change_tick();
    let mut players = world.query_filtered::<(
        Entity,
        Ref<DgScript>,
        Option<Ref<TbaTriggerVars>>,
        Has<DgVarSync>,
    ), With<Player>>();
    let dirty: Vec<Entity> = players
        .iter(world)
        .filter(|(_, script, vars, synced)| {
            let Some(last) = last else {
                return true;
            };
            !*synced
                || script.last_changed().is_newer_than(last, now)
                || vars
                    .as_ref()
                    .is_some_and(|vars| vars.last_changed().is_newer_than(last, now))
        })
        .map(|(entity, ..)| entity)
        .collect();
    for entity in dirty {
        sync_player(world, entity);
    }
    // Changes made from here on are newer than this pass.
    world.increment_change_tick();
    let end = world.read_change_tick();
    world.resource_mut::<DgVarClock>().0 = Some(end);
    world.increment_change_tick();
}

/// Mirror one player's globals.
fn sync_player(world: &mut World, entity: Entity) {
    let Some(script) = world.get::<DgScript>(entity) else {
        return;
    };
    let mut current = context_zero_globals(&script.0);
    let mut before = match world.get::<DgVarSync>(entity) {
        Some(sync) => sync.0.clone(),
        None => {
            world.entity_mut(entity).insert(DgVarSync(current));
            return;
        }
    };
    adopt_late_vars(world, entity, &mut before, &mut current);
    if current == before {
        return;
    }

    if world.get::<TbaTriggerVars>(entity).is_none() {
        world.entity_mut(entity).insert(TbaTriggerVars::default());
    }
    if let Some(mut vars) = world.get_mut::<TbaTriggerVars>(entity) {
        // Saved names a script cannot hold are carried through unchanged.
        let kept: Vec<(String, String)> = vars
            .global
            .iter()
            .chain(vars.remote.iter())
            .filter(|(name, _)| !holdable(name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        vars.remote = current.clone();
        vars.remote.extend(kept);
        vars.global.clear();
    }

    let raised: Vec<&String> = current
        .iter()
        .filter(|(name, value)| truthy(value) && !before.get(*name).is_some_and(|old| truthy(old)))
        .map(|(name, _)| name)
        .collect();
    if let Some(mut events) = world.get_resource_mut::<Messages<QuestEvent>>() {
        for name in raised {
            events.write(QuestEvent::FlagSet {
                player: entity,
                flag: name.as_str().into(),
            });
        }
    }
    world.entity_mut(entity).insert(DgVarSync(current));
}
