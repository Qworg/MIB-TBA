//! The DG script runtime: `dg-vm` hosted on the ECS world.
//!
//! `dg-vm` is an engine-agnostic port of tbaMUD's DG Scripts interpreter. It
//! reaches the game only through the [`dg_vm::DgHost`] trait and runs a script
//! when the game calls one of its dispatch functions (`dg_vm::dispatch`). This
//! module is the game's side of that contract:
//!
//! - [`host`] — [`host::EcsHost`], which borrows the `World` for the length of
//!   one dispatch and implements the nine host traits against it, one trait
//!   per file.
//! - [`mobcmd`] — the game's command interpreter as the VM's
//!   `mud_command`/`mudcommand` escape hatch.
//! - [`ids`] — the mapping between ECS entities and the VM's [`dg_vm::Uid`].
//! - [`script`] — per-entity script state and the trigger prototypes built
//!   from `tba.triggers`.
//! - [`waits`] — the pulse clock and the queue of scripts suspended in `wait`.
//! - [`vars`] — [`vars::TbaTriggerVars`], the persisted quest variables.
//! - [`intake`] — the systems that observe gameplay and call the dispatch
//!   functions, grouped by the events they own: commands, movement, items,
//!   combat and the world.
//!
//! The VM's run state lives in the [`DgVm`] resource. [`register`] is called
//! by [`TbaPlugin`](crate::module::TbaPlugin).
//!
//! # Design
//!
//! **Ids.** DG names every entity by a script id (`dg_vm::Uid`) whose range
//! says what it is. [`ids::DgId`] holds it and [`ids::DgIds`] maps it back to
//! the entity. A room's id is `ROOM_ID_BASE + Room.num`; a player's is
//! `Player.account_id` when that lies in `1..MOB_ID_BASE`, otherwise a counter
//! from the top of the player range downwards; NPCs and items count up from
//! their base. [`ids::ensure_ids`] gives every room, NPC, item and player an
//! id; [`EcsHost::entity`](host::EcsHost::entity) validates that the entity is
//! still alive, and the index forgets the dead at the next `ensure_ids`.
//!
//! **List order.** The C walks linked lists whose order scripts can observe.
//! [`ids::DgOrder`] is stamped from a counter whenever an entity is created or
//! its placement (`Location`, `CarriedBy`, `ContainedBy`) changes: the
//! [`ids::stamp_placement_order`] system does it for changes made by the rest
//! of the game and the host stamps directly when it moves something.
//! [`ids::DgBirth`] is the creation stamp, which `character_list` and
//! `object_list` use because the C never reorders them. The world-graph
//! queries sort by these: room people newest first (`char_to_room` prepends),
//! room contents oldest first (`obj_to_room` appends), carrying and container
//! contents newest first (`obj_to_char` and `obj_to_obj` prepend).
//!
//! **Script state.** [`script::DgScript`] holds an entity's triggers and
//! globals, [`script::DgMemory`] a mob's memory. [`script::DgProtos`] is built
//! at install from `tba.triggers`; [`script::attach_proto_scripts`] gives every
//! room, NPC and item that has not been through it (marker
//! [`script::DgScriptInit`]) its triggers in attachment order. A character a
//! script extracts gets [`script::DgDead`] and stays in the world, answering
//! `is_dead`, until the outermost [`with_host`] call ends.
//!
//! **Entry point.** All trigger activity goes through [`with_host`]: it ensures
//! ids, gives players their scripts, attaches prototype triggers and fires the
//! `load` trigger of each new mob and object, runs the caller's closure with
//! an [`host::EcsHost`], and, when the outermost call ends with the VM idle,
//! sweeps the dead and mirrors player variables ([`vars`]). A nested call
//! (from inside the closure) does none of that. Every entity seen for the
//! first time gets its `load` trigger, which is broader than the C: the C
//! fires it only from the zone reset, the script and wizard `load` commands and
//! spell creation, so an item restored from a save or bought in a shop also
//! runs its `load` trigger here.
//!
//! **Dispatch from `Update`.** The placement stamps of moves the game makes are
//! applied by a `FixedUpdate` system, so a dispatch run from `Update` straight
//! after a move must call [`host::EcsHost::stamp_order`] on the mover first, or
//! it sees the order from before the move.
//!
//! **Pulses.** [`waits::dg_pulse_system`] advances a pulse counter (a tenth of
//! a second) from the fixed clock and runs the C's script heartbeat once per
//! tick: due `wait`s, the random triggers every `PULSE_DG_SCRIPT` pulses, and
//! the time triggers when the mud hour changes. Several pulses in one tick run
//! the random sweep at most once.
//!
//! **Player variables.** A player's globals live in their `DgScript` and are
//! mirrored into the persisted [`vars::TbaTriggerVars::remote`]; a global that
//! becomes truthy is a quest flag ([`vars`]).
//!
//! **Gods.** The pack has no immortal levels, so the god rules of
//! `valid_dg_target` are moot: every NPC and every playing player is a valid
//! target.

pub mod host;
pub mod ids;
pub mod intake;
pub mod mobcmd;
pub mod script;
pub mod vars;
pub mod waits;

use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};

use dg_vm::dispatch::{load_mtrigger, load_otrigger};
use dg_vm::types::VmState;
use mib_core::prelude::*;
use mib_stdlib::prelude::{CarriedBy, ContainedBy, Npc, Player};

use crate::items::tick_item_timers;
use crate::zones::zone_reset_system;
use host::EcsHost;
use ids::{DgId, ensure_ids};
use script::{DgDead, DgProtos, attach_proto_scripts};
use waits::{DgPulse, DgWaits, dg_pulse_system};

/// The interpreter state the C keeps in globals and statics, shared across
/// nested script runs for the life of the world (`BaseHost::vm_state`).
#[derive(Resource, Debug, Default)]
pub struct DgVm {
    pub state: VmState,
    /// How many [`with_host`] calls are open.
    pub host_depth: u32,
}

/// The most recent lines kept in [`DgLog`].
const LOG_LINES: usize = 256;

/// The most distinct keys [`DgLog`] counts; later ones count under `other`.
const LOG_KEYS: usize = 1024;

/// What scripts reported: a bounded ring of recent lines and a count per key.
#[derive(Resource, Debug, Default)]
pub struct DgLog {
    lines: VecDeque<String>,
    counts: HashMap<String, u64>,
}

impl DgLog {
    /// Record a line and count it under `key`.
    pub fn record(&mut self, key: &str, line: String) {
        if self.lines.len() >= LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        let key = if self.counts.len() >= LOG_KEYS && !self.counts.contains_key(key) {
            "other"
        } else {
            key
        };
        let count = self.counts.entry(key.to_owned()).or_insert(0);
        *count = count.saturating_add(1);
    }

    /// The recent lines, oldest first.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// How many times `key` was reported. Script errors count under
    /// `script_log`, anything the host does not model under its own key.
    pub fn count(&self, key: &str) -> u64 {
        self.counts.get(key).copied().unwrap_or(0)
    }
}

/// Install the runtime's resources and systems.
pub fn register(app: &mut App) {
    register_runtime(app);
    intake::register(app);
}

/// The runtime without intake: resources, the pulse clock and the order
/// stamps. Systems that observe gameplay are [`intake`]'s.
pub fn register_runtime(app: &mut App) {
    app.init_resource::<DgVm>();
    app.init_resource::<DgLog>();
    app.init_resource::<ids::DgIds>();
    app.init_resource::<DgProtos>();
    app.init_resource::<DgPulse>();
    app.init_resource::<DgWaits>();
    app.add_message::<mib_stdlib::prelude::QuestEvent>();
    // After the zone reset, the item timers and the rest of the pack's fixed
    // chain, so what they created this tick is complete (combatant state
    // included) when its triggers first run; before the save set, so a variable a
    // script changed is persisted with the tick that changed it.
    app.add_systems(
        FixedUpdate,
        (ids::stamp_placement_order, dg_pulse_system)
            .chain()
            .after(zone_reset_system)
            .after(tick_item_timers)
            .after(mib_stdlib::prelude::maintain_room_occupants)
            .after(crate::party::cleanup_parties_on_logout)
            .before(mib_stdlib::prelude::SaveSet),
    );
}

/// Run `f` against the world as a DG host.
///
/// Before `f`: ids are ensured, players get their scripts, every new room, NPC
/// and item gets its prototype triggers, and each new mob and object has its
/// `load` trigger fired (the C fires it from the zone reset and the load
/// commands only; here every entity seen for the first time gets it). After the outermost call, once the VM is idle:
/// characters scripts extracted are swept and player variables are mirrored
/// into their persisted form. A nested call does only the id pass.
pub fn with_host<R>(world: &mut World, f: impl FnOnce(&mut EcsHost<'_>) -> R) -> R {
    world.init_resource::<DgVm>();
    let outermost = {
        let mut vm = world.resource_mut::<DgVm>();
        vm.host_depth += 1;
        vm.host_depth == 1
    };
    ensure_ids(world);
    let fresh = if outermost {
        vars::seed_player_scripts(world);
        attach_proto_scripts(world)
    } else {
        Vec::new()
    };

    let result = {
        let mut host = EcsHost { world };
        for entity in fresh {
            fire_load_trigger(&mut host, entity);
        }
        f(&mut host)
    };

    let idle = {
        let mut vm = world.resource_mut::<DgVm>();
        vm.host_depth = vm.host_depth.saturating_sub(1);
        vm.host_depth == 0 && vm.state.depth == 0
    };
    if idle {
        sweep_dead(world);
        vars::sync_player_vars(world);
    }
    result
}

/// `load_mtrigger` / `load_otrigger` for an entity that was just initialised.
fn fire_load_trigger(host: &mut EcsHost<'_>, entity: Entity) {
    let Some(uid) = host.uid(entity) else {
        return;
    };
    if host.world.get::<Npc>(entity).is_some() {
        load_mtrigger(host, uid);
    } else {
        load_otrigger(host, uid);
    }
}

/// Despawn the characters scripts extracted, with everything they carry or
/// wear. The C defers `extract_char` to the end of the pulse so a running
/// script can still see the corpse; this is that end. A player is never
/// despawned here (leaving the game is the session's business): only the
/// marker is removed.
fn sweep_dead(world: &mut World) {
    let mut dead_query = world.query_filtered::<Entity, With<DgDead>>();
    let dead: Vec<Entity> = dead_query.iter(world).collect();
    if dead.is_empty() {
        return;
    }
    let mut doomed: Vec<Entity> = Vec::new();
    for entity in dead {
        if world.get::<Player>(entity).is_some() {
            world.entity_mut(entity).remove::<DgDead>();
        } else {
            doomed.push(entity);
        }
    }
    if doomed.is_empty() {
        return;
    }

    // Everything held by (or inside something held by) a doomed character.
    let mut children: HashMap<Entity, Vec<Entity>> = HashMap::new();
    let mut carried = world.query::<(Entity, &CarriedBy)>();
    for (item, holder) in carried.iter(world) {
        children.entry(holder.0).or_default().push(item);
    }
    let mut contained = world.query::<(Entity, &ContainedBy)>();
    for (item, container) in contained.iter(world) {
        children.entry(container.0).or_default().push(item);
    }
    let mut seen: HashSet<Entity> = doomed.iter().copied().collect();
    let mut queue: VecDeque<Entity> = doomed.iter().copied().collect();
    while let Some(entity) = queue.pop_front() {
        for child in children.get(&entity).into_iter().flatten() {
            if seen.insert(*child) {
                doomed.push(*child);
                queue.push_back(*child);
            }
        }
    }

    let uids: Vec<_> = doomed
        .iter()
        .filter_map(|entity| world.get::<DgId>(*entity).map(|id| id.0))
        .collect();
    for entity in doomed {
        world.despawn(entity);
    }
    if let Some(mut waits) = world.get_resource_mut::<DgWaits>() {
        for uid in uids {
            waits.cancel_owner(uid);
        }
    }
}
