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

pub mod clock;
pub mod combat;
pub mod content;
pub mod equipment;
pub mod items;
pub mod module;
pub mod party;
pub mod shops;
pub mod socials;
pub mod spells;
pub mod zones;

pub mod prelude {
    pub use crate::clock::{
        MONTH_NAMES, WEEKDAYS, apply_night_darkness, handle_time_command, ordinal_suffix,
        time_report,
    };
    pub use crate::combat::{
        BASE_ATTRIBUTE, BASE_MOVE, EVIL_ALIGNMENT, GOLD_ITEM_DESCRIPTION, GOLD_ITEM_ID,
        GOLD_ITEM_NAME, GOOD_ALIGNMENT, KillReward, MOVE_PER_LEVEL, NAKED_ARMOR_CLASS, NpcDefeated,
        ROUND_SECONDS, SaveType, TbaAlignment, TbaAttackDice, TbaAttackerMemory, TbaAttributes,
        TbaCombatant, TbaGold, TbaNpcBase, TbaProgress, TbaRng, TbaRound, TbaRoundClock, TbaSaves,
        TbaSpawnHome, WieldedWeapon, alignment_after_kill, alignment_for, apply_death_penalty,
        auto_engage_system, award_kill_rewards, combat_dice, credit_gold_pickups, default_class,
        derive_vitals, engages, fallback_combatant, grant_kill, handle_combat_commands,
        helper_system, level_for_xp, mag_savingthrow, npc_combatant_from_stats,
        npc_combatant_system, player_combatant_system, pursuit_system, resolve_class_key,
        resolve_rounds, reward_for, run_due_round, saving_throws, weapon_dice, xp_threshold,
        xp_to_advance,
    };
    pub use crate::content::{
        AffectDef, AffectLocation, Attribute, DoorReset, ItemAffectFlag, ItemExtraFlag, LevelBonus,
        NpcBehaviour, Position, ResetMode, SaveEffect, SpellDef, SpellKind, SpellTarget,
        SpellsFile, SpellsSection, TbaClassDef, TbaClassesFile, TbaClassesSection, TbaItemDef,
        TbaItemKind, TbaItemsFile, TbaItemsSection, TbaNpcStats, TbaNpcStatsFile,
        TbaNpcStatsSection, WearSlot, ZoneCommand, ZoneCommandKind, ZoneDef, ZonesFile,
        ZonesSection, register,
    };
    pub use crate::content::{ShopDef, ShopHours, ShopTrade, ShopsFile, ShopsSection};
    pub use crate::content::{SocialDef, SocialsFile, SocialsSection};
    pub use crate::equipment::{
        TbaEquipment, TbaEquipmentSave, choose_slot, handle_equipment_commands,
        recompute_combatant, recompute_combatant_full, recompute_equipment_affects,
        recompute_npc_equipment_affects, relink_slots, restore_equipment_from_save,
        sync_wielded_weapon,
    };
    pub use crate::items::{
        MUD_HOUR_SECONDS, TbaItemTimer, TbaObjectClock, attach_item_timers, tick_item_timers,
    };
    pub use crate::module::{MODULE_NAME, TbaContent, TbaModule, TbaPlugin};
    pub use crate::party::{
        LeaveOutcome, PartyCommand, TbaFollowing, TbaParties, TbaParty, cleanup_parties_on_logout,
        follow_system, handle_party_commands, parse_party_line,
    };
    pub use crate::shops::{ShopCommand, handle_shop_commands, parse_shop_line};
    pub use crate::socials::{
        SocialCommand, SocialNames, gate_reason, handle_social_commands, parse_social_line,
        substitute,
    };
    pub use crate::spells::{
        ActiveBuff, DEFAULT_BUFF_ROUNDS, TbaBuffs, handle_cast_command, mana_cost, tick_buffs,
    };
    pub use crate::zones::{ZoneState, ZoneTracker, zone_reset_system};
}
