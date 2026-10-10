//! `TbaModule` — the TBA pack's code half, and the resource it installs.
//!
//! `packs/tba/pack.toml` declares `[code] module = "tba"`, so a build that
//! cannot link this module refuses the pack (`PackError::MissingModule`) rather
//! than silently loading a world whose `tba.*` sections nobody understands.
//!
//! Registration is split the way every MIB pack does it:
//! - [`MibModule::register_sections`] claims the `tba.*` namespace,
//! - [`MibModule::build`] adds [`TbaPlugin`] at app-construction time,
//! - [`MibModule::install`] turns the loaded YAML into the [`TbaContent`]
//!   resource, after load and validation and before the world is live.
//!
//! The gameplay systems that consume [`TbaContent`] (zone resets, aggro, Circle
//! combat) land in later phases; [`TbaPlugin`] is the seam they attach to.

use std::collections::BTreeMap;

use mib_content::prelude::{MibModule, PackAsset, SectionRegistry, section_defs};
use mib_core::prelude::*;
use mib_persist::prelude::DocKind;
use mib_stdlib::prelude::{
    CharacterCreated, ItemId, NpcId, PlayerDefeated, SaveRegistry, attach_pending_holders,
    handle_player_death, maintain_room_occupants, route_commands,
};
use smol_str::SmolStr;

use crate::combat::{TbaCombatant, TbaGold, TbaProgress, TbaRng, TbaRound, TbaRoundClock};
use crate::content::{
    ShopDef, ShopsSection, SpellDef, SpellsSection, TbaClassDef, TbaClassesSection, TbaItemDef,
    TbaItemsSection, TbaNpcStats, TbaNpcStatsSection, ZoneDef, ZonesSection,
};
use crate::equipment::TbaEquipmentSave;
use crate::zones::ZoneTracker;

/// Module id; matches `[code] module = "tba"` in `packs/tba/pack.toml`.
pub const MODULE_NAME: &str = "tba";

/// The parsed `tba.*` content, installed once per boot from the loaded pack set.
///
/// It is a clone of the section bodies rather than a borrow because
/// [`section_defs`] returns references tied to the `PackAsset` list, while
/// systems need a resource that outlives the boot call. The cloned maps are the
/// lookup tables the later systems consult; a later phase can replace the clone
/// with an index if the content grows large enough to matter.
#[derive(Resource, Debug, Default, Clone)]
pub struct TbaContent {
    /// `tba.zones` — keyed by zone vnum.
    pub zones: BTreeMap<u32, ZoneDef>,
    /// `tba.npc_stats` — keyed by `stdlib.npcs` id.
    pub npc_stats: BTreeMap<NpcId, TbaNpcStats>,
    /// `tba.items` — keyed by `stdlib.items` id.
    pub items: BTreeMap<ItemId, TbaItemDef>,
    /// `tba.classes` — keyed by class id.
    pub classes: BTreeMap<SmolStr, TbaClassDef>,
    /// `tba.spells` — keyed by the snake_case spell id.
    pub spells: BTreeMap<SmolStr, SpellDef>,
    /// `tba.shops` — keyed by the shopkeeper `stdlib.npcs` id (TBA-E17).
    pub shops: BTreeMap<NpcId, ShopDef>,
}

/// The TBA game module.
#[derive(Debug, Default, Clone, Copy)]
pub struct TbaModule;

impl MibModule for TbaModule {
    fn name(&self) -> &'static str {
        MODULE_NAME
    }

    fn register_sections(&self, registry: &mut SectionRegistry) {
        crate::content::register(registry);
    }

    fn build(&self, app: &mut App) {
        app.add_plugins(TbaPlugin);
    }

    fn install(&self, world: &mut World, packs: &[PackAsset]) {
        let zones = section_defs::<ZonesSection>(packs)
            .map(|defs| defs.zones.clone())
            .unwrap_or_default();
        let npc_stats = section_defs::<TbaNpcStatsSection>(packs)
            .map(|defs| defs.npc_stats.clone())
            .unwrap_or_default();
        let items = section_defs::<TbaItemsSection>(packs)
            .map(|defs| defs.items.clone())
            .unwrap_or_default();
        let classes = section_defs::<TbaClassesSection>(packs)
            .map(|defs| defs.classes.clone())
            .unwrap_or_default();
        let spells = section_defs::<SpellsSection>(packs)
            .map(|defs| defs.spells.clone())
            .unwrap_or_default();
        let shops = section_defs::<ShopsSection>(packs)
            .map(|defs| defs.shops.clone())
            .unwrap_or_default();

        // The reset tracker is derived state, seeded from the same content so
        // it exists before the first `FixedUpdate`.
        world.insert_resource(ZoneTracker::from_zones(&zones));
        world.insert_resource(TbaContent {
            zones,
            npc_stats,
            items,
            classes,
            spells,
            shops,
        });
    }
}

/// TBA gameplay systems.
///
/// This is the plugin [`TbaModule::build`] adds and the single seam the
/// gameplay attaches to:
///
/// - **persistence** — [`TbaProgress`] (class/level/XP) and [`TbaCombatant`]
///   (current vitals) register with the stdlib [`SaveRegistry`] under
///   [`DocKind::Player`], so a character survives a restart (doc §7.2).
/// - **the round runner** — a dedicated [`TbaRound`] schedule run every
///   [`ROUND_SECONDS`](crate::combat::ROUND_SECONDS) on the frame clock, so
///   combat never forces the engine's `tick_hz` (doc §4.3).
///
/// The zone-reset interpreter (TBA-E07) runs on `FixedUpdate`; room-local
/// aggro (TBA-E05) attaches here in a later phase.
#[derive(Debug, Default, Clone, Copy)]
pub struct TbaPlugin;

impl Plugin for TbaPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TbaRng>();
        app.init_resource::<TbaRoundClock>();
        app.init_schedule(TbaRound);

        // Each component owner owns its schema (the classic-rpg idiom): the
        // player-scoped progression and vitals live in the same registry the
        // stdlib restore pass resolves.
        app.init_resource::<SaveRegistry>();
        {
            let mut registry = app.world_mut().resource_mut::<SaveRegistry>();
            registry.register::<TbaProgress>("TbaProgress", DocKind::Player);
            registry.register::<TbaCombatant>("TbaCombatant", DocKind::Player);
            registry.register::<TbaGold>("TbaGold", DocKind::Player);
            registry.register::<TbaEquipmentSave>("TbaEquipment", DocKind::Player);
        }

        // Register the messages the gameplay reads so a bare `TbaPlugin`
        // composition (tests) never starves; `add_message` is idempotent.
        app.add_message::<CharacterCreated>();
        app.add_message::<PlayerDefeated>();
        app.add_message::<CommandReceived>();
        app.add_message::<OutputEvent>();

        // Player progression and the genre command seam, after the stdlib
        // router that emits `CharacterCreated` and no-ops `attack`/`flee`.
        // Equipment commands join the same chain; the affect recompute and
        // the wielded-weapon sync follow them so an equip/unequip takes effect
        // in the frame it is issued.
        app.add_systems(
            Update,
            (
                crate::combat::player_combatant_system,
                crate::combat::handle_combat_commands,
                crate::spells::handle_cast_command,
                crate::shops::handle_shop_commands,
                crate::equipment::handle_equipment_commands,
                crate::equipment::recompute_equipment_affects,
                crate::equipment::sync_wielded_weapon,
            )
                .chain()
                .in_set(BridgeSet::Game)
                .after(route_commands),
        );

        // TBA-E09: the genre death rules (XP loss + gold to a carried pile)
        // must be in place before the stdlib corpse/respawn pass, which sweeps
        // every carried item (including the gold pile just spawned) into the
        // corpse. `handle_player_death` is addressable, so the edge is an
        // explicit `.before`; it is added by `StdlibPlugin` in the same
        // `BridgeSet::Game` set.
        app.add_systems(
            Update,
            crate::combat::apply_death_penalty
                .in_set(BridgeSet::Game)
                .before(handle_player_death),
        );

        // Equipment relink: once the stdlib has applied the player bundle and
        // linked carried items, rebuild the runtime worn map from the saved
        // `SaveId`s. Gated on `Without<TbaEquipment>` so it runs once per login
        // and never clobbers live equipment.
        app.add_systems(
            Update,
            crate::equipment::restore_equipment_from_save
                .in_set(BridgeSet::Game)
                .after(attach_pending_holders),
        );

        // Combat rounds on their own clock, after this frame's commands have
        // been routed; the fixed tick is left untouched. Buffs age with the
        // round before damage resolves.
        app.add_systems(
            TbaRound,
            (crate::spells::tick_buffs, crate::combat::resolve_rounds).chain(),
        );
        app.add_systems(Update, crate::combat::run_due_round.after(BridgeSet::Game));

        // World simulation on the fixed tick: reset zones (spawn NPCs), give
        // each NPC its vitals, then run room-local aggro. Ordered after the
        // stdlib spatial index so `RoomOccupants` is current.
        app.add_systems(
            FixedUpdate,
            (
                crate::zones::zone_reset_system,
                crate::combat::npc_combatant_system,
                crate::combat::auto_engage_system,
                crate::combat::helper_system,
                crate::combat::pursuit_system,
            )
                .chain()
                .after(maintain_room_occupants),
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn tba_module_registers_only_the_tba_sections() {
        let module = TbaModule;
        assert_eq!(module.name(), "tba");
        let registry = mib_content::prelude::registry_for(&[&module]);
        let mut ids: Vec<&str> = registry.ids().collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![
                "tba.classes",
                "tba.items",
                "tba.npc_stats",
                "tba.shops",
                "tba.spells",
                "tba.zones"
            ],
            "only tba.* sections: {ids:?}"
        );
    }

    #[test]
    fn tba_content_defaults_to_empty() {
        let content = TbaContent::default();
        assert!(content.zones.is_empty());
        assert!(content.npc_stats.is_empty());
        assert!(content.items.is_empty());
        assert!(content.classes.is_empty());
        assert!(content.spells.is_empty());
        assert!(content.shops.is_empty());
    }

    #[test]
    fn tba_plugin_registers_player_progression_and_vitals() {
        let mut app = App::new();
        app.add_plugins(TbaPlugin);
        let registry = app.world().resource::<SaveRegistry>();
        let registered = registry.registered();
        assert!(registered.contains(&"TbaProgress"), "{registered:?}");
        assert!(registered.contains(&"TbaCombatant"), "{registered:?}");
        assert!(registered.contains(&"TbaGold"), "{registered:?}");
        assert!(registered.contains(&"TbaEquipment"), "{registered:?}");
        assert!(
            registry.kinds().contains(&DocKind::Player),
            "player-scoped docs must be writable"
        );
    }

    #[test]
    fn tba_update_graph_resolves_against_stdlib() {
        // The equipment systems order themselves after the stdlib router and
        // the pending-holder link pass, and the death penalty orders itself
        // before the stdlib corpse pass; initializing the real Update graph
        // proves those cross-crate system references resolve in the
        // composition the game actually boots.
        let mut app = App::new();
        app.add_plugins(mib_stdlib::prelude::StdlibPlugin);
        app.add_plugins(TbaPlugin);
        let mut schedule = app
            .world_mut()
            .resource_mut::<Schedules>()
            .remove(Update)
            .expect("Update schedule is present");
        schedule
            .initialize(app.world_mut())
            .expect("the TBA Update graph must resolve");
        app.world_mut()
            .resource_mut::<Schedules>()
            .reinsert(schedule);
    }
}
