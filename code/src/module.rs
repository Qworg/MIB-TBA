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
use mib_stdlib::prelude::{ItemId, NpcId};
use smol_str::SmolStr;

use crate::content::{
    TbaClassDef, TbaClassesSection, TbaItemDef, TbaItemsSection, TbaNpcStats, TbaNpcStatsSection,
    ZoneDef, ZonesSection,
};

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
        world.insert_resource(TbaContent {
            zones: section_defs::<ZonesSection>(packs)
                .map(|defs| defs.zones.clone())
                .unwrap_or_default(),
            npc_stats: section_defs::<TbaNpcStatsSection>(packs)
                .map(|defs| defs.npc_stats.clone())
                .unwrap_or_default(),
            items: section_defs::<TbaItemsSection>(packs)
                .map(|defs| defs.items.clone())
                .unwrap_or_default(),
            classes: section_defs::<TbaClassesSection>(packs)
                .map(|defs| defs.classes.clone())
                .unwrap_or_default(),
        });
    }
}

/// TBA gameplay systems.
///
/// Empty by design in this phase: it is the plugin `TbaModule::build` adds, so
/// later phases have a single place to attach the zone-reset interpreter
/// (TBA-E07), room-local aggro (TBA-E05/E31) and the Circle combat round
/// (TBA-E31) without re-touching the module wiring.
#[derive(Debug, Default, Clone, Copy)]
pub struct TbaPlugin;

impl Plugin for TbaPlugin {
    fn build(&self, _app: &mut App) {}
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
            vec!["tba.classes", "tba.items", "tba.npc_stats", "tba.zones"],
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
    }
}
