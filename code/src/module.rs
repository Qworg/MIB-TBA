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
    CharacterCreated, CommandSet, ItemId, NpcId, PlayerDefeated, QuestEvent, SaveRegistry, SaveSet,
    attach_pending_holders, handle_player_death, maintain_room_occupants, route_commands,
};
use smol_str::SmolStr;

use crate::combat::{
    NpcDefeated, TbaAlignment, TbaCombatant, TbaGold, TbaProgress, TbaRng, TbaRound, TbaRoundClock,
};
use crate::content::{
    ShopDef, ShopsSection, SocialDef, SocialsSection, SpellDef, SpellsSection, TbaClassDef,
    TbaClassesSection, TbaItemDef, TbaItemsSection, TbaNpcStats, TbaNpcStatsSection, ZoneDef,
    ZonesSection,
};
use crate::dg::script::DgProtos;
use crate::dg::vars::TbaTriggerVars;
use crate::equipment::TbaEquipmentSave;
use crate::items::TbaObjectClock;
use crate::party::TbaParties;
use crate::triggers::content::{TriggerDef, TriggerId, TriggersSection};
use crate::triggers::quests::trigger_quest_bridge_system;
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
    /// `tba.classes.skill_levels` — class -> skill -> minimum level
    /// (`init_spell_levels()`, `tbamud/src/class.c:1652-1664`).
    pub skill_levels: BTreeMap<SmolStr, BTreeMap<SmolStr, u32>>,
    /// `tba.spells` — keyed by the snake_case spell id.
    pub spells: BTreeMap<SmolStr, SpellDef>,
    /// `tba.shops` — keyed by the shopkeeper `stdlib.npcs` id (TBA-E17).
    pub shops: BTreeMap<NpcId, ShopDef>,
    /// `tba.socials` — keyed by the social word (issue #17).
    pub socials: BTreeMap<SmolStr, SocialDef>,
    /// `tba.triggers` — keyed by [`TriggerId`] (issue #15).
    pub triggers: BTreeMap<TriggerId, TriggerDef>,
    /// `tba.triggers.attachments` — each owner's ordered trigger list, keyed
    /// `npc:<id>`, `item:<id>` or `room:<id>`.
    pub trigger_attachments: BTreeMap<String, Vec<TriggerId>>,
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
        let skill_levels = section_defs::<TbaClassesSection>(packs)
            .map(|defs| defs.skill_levels.clone())
            .unwrap_or_default();
        let spells = section_defs::<SpellsSection>(packs)
            .map(|defs| defs.spells.clone())
            .unwrap_or_default();
        let shops = section_defs::<ShopsSection>(packs)
            .map(|defs| defs.shops.clone())
            .unwrap_or_default();
        let socials = section_defs::<SocialsSection>(packs)
            .map(|defs| defs.socials.clone())
            .unwrap_or_default();
        let triggers = section_defs::<TriggersSection>(packs)
            .map(|defs| defs.triggers.clone())
            .unwrap_or_default();
        let trigger_attachments = section_defs::<TriggersSection>(packs)
            .map(|defs| defs.attachments.clone())
            .unwrap_or_default();

        // The DG trigger prototypes are parsed once here; a record that cannot
        // load is counted and skipped, never fatal.
        world.insert_resource(DgProtos::build(&triggers, &trigger_attachments));

        // The reset tracker is derived state, seeded from the same content so
        // it exists before the first `FixedUpdate`.
        world.insert_resource(ZoneTracker::from_zones(&zones));
        world.insert_resource(TbaContent {
            zones,
            npc_stats,
            items,
            classes,
            skill_levels,
            spells,
            shops,
            socials,
            triggers,
            trigger_attachments,
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
        app.init_resource::<TbaObjectClock>();
        app.init_resource::<TbaParties>();
        app.init_resource::<crate::spec::TbaSpecRng>();
        app.init_resource::<crate::mail::MailCompose>();
        app.init_resource::<crate::boards::BoardCompose>();
        app.init_schedule(TbaRound);
        crate::dg::register(app);

        // Each component owner owns its schema (the classic-rpg idiom): the
        // player-scoped progression and vitals live in the same registry the
        // stdlib restore pass resolves.
        app.init_resource::<SaveRegistry>();
        {
            let mut registry = app.world_mut().resource_mut::<SaveRegistry>();
            registry.register::<TbaProgress>("TbaProgress", DocKind::Player);
            registry.register::<TbaCombatant>("TbaCombatant", DocKind::Player);
            registry.register::<TbaAlignment>("TbaAlignment", DocKind::Player);
            registry.register::<TbaGold>("TbaGold", DocKind::Player);
            registry.register::<TbaEquipmentSave>("TbaEquipment", DocKind::Player);
            registry.register::<crate::spells::TbaBuffs>("TbaBuffs", DocKind::Player);
            // Quest variables (`remote`/`global`) survive reconnect in the
            // player's character bundle.
            registry.register::<TbaTriggerVars>("TbaTriggerVars", DocKind::Player);
            // The character facts a script reads and writes (title, practices,
            // quest points, conditions, skill percentages), the act-array bits
            // it set or cleared, and the affects it applied are part of the
            // player's character bundle.
            registry
                .register::<crate::dg::chardata::TbaCharExtras>("TbaCharExtras", DocKind::Player);
            registry.register::<crate::dg::chardata::TbaActFlags>("TbaActFlags", DocKind::Player);
            registry.register::<crate::dg::chardata::TbaDgAffects>("TbaDgAffects", DocKind::Player);
            // A script's write to an NPC's gold or experience is saved with the
            // NPC's own document. Every persistent NPC carries
            // `SaveKind(DocKind::Npc)`, so the changed purse survives a
            // restart and its kill still pays what it now holds.
            registry.register::<crate::dg::chardata::TbaNpcPurse>("TbaNpcPurse", DocKind::Npc);
            // Item timers are item-scoped; `DocKind::Item` is already writable
            // because the stdlib registers `Item` under it.
            crate::items::register(&mut registry);
            // A script's per-instance object changes (values, cost, rent,
            // weight, applies, alias) are saved with the item too.
            registry.register::<crate::dg::host::DgObjOverride>("DgObjOverride", DocKind::Item);
        }

        // Register the messages the gameplay reads so a bare `TbaPlugin`
        // composition (tests) never starves; `add_message` is idempotent.
        app.add_message::<CharacterCreated>();
        app.add_message::<PlayerDefeated>();
        app.add_message::<NpcDefeated>();
        // Every hit-point reduction emits a `TbaDamage`, one seam for
        // anything that reacts to a blow.
        app.add_message::<crate::combat::TbaDamage>();
        app.add_message::<CommandReceived>();
        app.add_message::<OutputEvent>();
        // The quest bridge emits stdlib quest events.
        app.add_message::<QuestEvent>();

        // The `who` filter is the pack's only remaining visibility surface;
        // `look`/`Scene`/GMCP are filtered by the engine perception seam. The
        // resource is normally inserted by `BridgePlugin`; init here so a
        // `StdlibPlugin` + `TbaPlugin` composition (tests) also has it.
        app.init_resource::<HandledCommands>();
        app.add_systems(
            Update,
            crate::visibility::intercept_who.in_set(CommandSet::PreRoute),
        );
        // Castle guards that refuse a direction (`block_way`) run after the DG
        // pre-commit refusals, so a trigger still wins.
        app.add_systems(
            Update,
            crate::spec::intercept_block_way
                .in_set(CommandSet::PreRoute)
                .after(crate::dg::intake::DgIntake::PreCommit),
        );
        // A mail compose body is consumed before the DG command gate so a
        // body line is never offered to a trigger or the router.
        app.add_systems(
            Update,
            (
                crate::mail::compose_mail_input,
                crate::boards::compose_board_input,
            )
                .in_set(CommandSet::PreRoute)
                .before(crate::dg::intake::DgIntake::CommandGate),
        );
        app.add_systems(
            Update,
            crate::boards::intercept_board_look.in_set(CommandSet::PreRoute),
        );
        app.add_systems(
            Update,
            (
                crate::mail::ensure_mailboxes,
                crate::mail::seed_player_mail,
                crate::mail::handle_mail_commands,
            )
                .chain()
                .in_set(BridgeSet::Game)
                .after(route_commands),
        );
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
                crate::skills::handle_skill_commands,
                crate::spells::handle_cast_command,
                crate::shops::handle_shop_commands,
                crate::boards::handle_board_commands,
                crate::socials::handle_social_commands,
                crate::clock::handle_time_command,
                crate::party::handle_party_commands,
                crate::equipment::handle_equipment_commands,
                crate::equipment::recompute_equipment_affects,
                crate::equipment::recompute_npc_equipment_affects,
                crate::equipment::sync_wielded_weapon,
                // Drive the engine perception seam from the pack's private
                // stealth state (`TbaHidden`/`TbaSneaking`) and true-sight
                // grants (`TbaTrueSight`) after every handler that mutates
                // them, so the carriers are current by the end of the frame.
                crate::stealth::sync_concealment,
                crate::stealth::sync_perception,
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

        // Gold pickups: a `tba_gold` pile a player carries becomes purse gold
        // and the pile is consumed. It must run after `route_commands` (so a
        // `get` this frame is visible) and after `handle_player_death` (so the
        // death penalty's own carried pile has already been swept into the
        // corpse — crediting it back would undo the penalty).
        app.add_systems(
            Update,
            crate::combat::credit_gold_pickups
                .in_set(BridgeSet::Game)
                .after(route_commands)
                .after(handle_player_death),
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
            (
                crate::spells::tick_buffs,
                crate::stealth::tick_true_sight,
                crate::combat::resolve_rounds,
            )
                .chain(),
        );
        app.add_systems(Update, crate::combat::run_due_round.after(BridgeSet::Game));

        // Kill rewards: read every `NpcDefeated` produced this frame — melee
        // kills from the round schedule above, spell kills from the command
        // chain — and grant XP/gold plus any level-up. This runs after
        // `run_due_round` so a melee kill is paid in the same frame.
        app.add_systems(
            Update,
            crate::combat::award_kill_rewards.after(crate::combat::run_due_round),
        );

        // Translate each native kill into a stdlib
        // `QuestEvent::Killed`, after the reward pass read the same stream.
        app.add_systems(
            Update,
            trigger_quest_bridge_system.after(crate::combat::award_kill_rewards),
        );

        // World simulation on the fixed tick: reset zones (spawn NPCs), attach
        // and age object timers, give each NPC its vitals, then run room-local
        // aggro. Ordered after the stdlib spatial index so `RoomOccupants` is
        // current, and before the save pipeline so a timer that changed this
        // tick is persisted with the tick that changed it.
        app.add_systems(
            FixedUpdate,
            (
                crate::zones::zone_reset_system,
                crate::items::attach_item_timers,
                crate::items::tick_item_timers,
                crate::clock::apply_night_darkness,
                crate::combat::npc_combatant_system,
                crate::spec::attach_spec_procs,
                crate::combat::auto_engage_system,
                crate::combat::helper_system,
                crate::combat::pursuit_system,
                crate::party::follow_system,
                crate::spec::spec_proc_system,
                crate::party::cleanup_parties_on_logout,
            )
                .chain()
                .after(maintain_room_occupants)
                .before(SaveSet),
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
                "tba.socials",
                "tba.spells",
                "tba.triggers",
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
        assert!(content.triggers.is_empty());
        assert!(content.trigger_attachments.is_empty());
    }

    #[test]
    fn tba_plugin_registers_player_progression_and_vitals() {
        let mut app = App::new();
        app.add_plugins(TbaPlugin);
        let registry = app.world().resource::<SaveRegistry>();
        let registered = registry.registered();
        assert!(registered.contains(&"TbaProgress"), "{registered:?}");
        assert!(registered.contains(&"TbaCombatant"), "{registered:?}");
        assert!(registered.contains(&"TbaAlignment"), "{registered:?}");
        assert!(registered.contains(&"TbaGold"), "{registered:?}");
        assert!(registered.contains(&"TbaEquipment"), "{registered:?}");
        assert!(registered.contains(&"TbaBuffs"), "{registered:?}");
        assert!(registered.contains(&"TbaItemTimer"), "{registered:?}");
        assert!(registered.contains(&"DgObjOverride"), "{registered:?}");
        assert!(registered.contains(&"TbaTriggerVars"), "{registered:?}");
        assert!(registered.contains(&"TbaNpcPurse"), "{registered:?}");
        assert!(registered.contains(&"TbaCharExtras"), "{registered:?}");
        assert!(registered.contains(&"TbaActFlags"), "{registered:?}");
        assert!(registered.contains(&"TbaDgAffects"), "{registered:?}");
        assert!(
            registry.kinds().contains(&DocKind::Player),
            "player-scoped docs must be writable"
        );
        assert!(
            registry.kinds().contains(&DocKind::Npc),
            "the NPC purse must be writable"
        );
    }

    #[test]
    fn tba_plugin_installs_the_party_resource() {
        let mut app = App::new();
        app.add_plugins(TbaPlugin);
        assert!(
            app.world().get_resource::<TbaParties>().is_some(),
            "the party resource is installed by the plugin"
        );
        assert!(
            app.world().get_resource::<crate::dg::DgVm>().is_some(),
            "the script runtime state is installed by the plugin"
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
