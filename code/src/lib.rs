//! `mib-tba` — the Builder Academy (CircleMUD / tbaMUD) game pack.
//!
//! This is the code half of `packs/tba/`, beside the `pack.toml` and
//! `content/` it belongs to, so the directory is one movable unit (D12/D13 —
//! the same shape as `packs/classic-rpg/` and `packs/stdlib/`).
//!
//! ## Layering
//!
//! TBA is an **independent genre**, not a layer on `classic-rpg`:
//!
//! ```text
//! mib-core ── mib-stdlib ─┬─ mib-classic-rpg   (3x3 tactical grid)
//!                         └─ mib-tba           (native Circle round combat)
//! ```
//!
//! `packs/tba/pack.toml` therefore declares `depends_on = ["stdlib"]` and
//! `game = "tba"`, and ships no `rpg.*` sections. World content (rooms, NPC
//! prototypes, item prototypes) lives in the `stdlib.*` sections; the Diku
//! domain rules live in the `tba.*` sections owned by [`content`].
//!
//! ## What this phase installs
//!
//! The pack skeleton and its four data models: [`ZonesSection`]
//! (`tba.zones`), [`TbaNpcStatsSection`] (`tba.npc_stats`),
//! [`TbaItemsSection`] (`tba.items`) and [`TbaClassesSection`]
//! (`tba.classes`), installed as the [`TbaContent`] resource. The zone-reset
//! interpreter, room-local aggro and Circle combat round attach to
//! [`TbaPlugin`] in later phases.

pub mod content;
pub mod module;

pub mod prelude {
    pub use crate::content::{
        AffectDef, AffectLocation, Attribute, DoorReset, NpcBehaviour, Position, ResetMode,
        TbaClassDef, TbaClassesFile, TbaClassesSection, TbaItemDef, TbaItemKind, TbaItemsFile,
        TbaItemsSection, TbaNpcStats, TbaNpcStatsFile, TbaNpcStatsSection, WearSlot, ZoneCommand,
        ZoneDef, ZonesFile, ZonesSection, register,
    };
    pub use crate::module::{MODULE_NAME, TbaContent, TbaModule, TbaPlugin};
}
