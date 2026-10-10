//! Native CircleMUD/TBA combat: progression, the round runner, aggro (later),
//! and round resolution.
//!
//! TBA does **not** use `classic-rpg`'s 3×3 tactical grid. Combat is classic
//! Diku: a flat list of combatants, each with a `target`, resolved on a
//! dedicated 3-second [`TbaRound`] schedule (doc §4).

pub mod ai;
pub mod commands;
pub mod death;
pub mod init;
pub mod model;
pub mod progression;
pub mod rewards;
pub mod round;
pub mod systems;

pub use ai::{
    TbaAttackerMemory, TbaSpawnHome, auto_engage_system, engages, helper_system, pursuit_system,
};
pub use commands::handle_combat_commands;
pub use death::{
    GOLD_ITEM_DESCRIPTION, GOLD_ITEM_ID, GOLD_ITEM_NAME, apply_death_penalty, credit_gold_pickups,
};
pub use init::{
    TbaNpcBase, default_class, fallback_combatant, npc_combatant_from_stats, npc_combatant_system,
    player_combatant_system, resolve_class_key,
};
pub use model::{
    EVIL_ALIGNMENT, GOOD_ALIGNMENT, TbaAlignment, TbaCombatant, TbaGold, TbaProgress, WieldedWeapon,
};
pub use progression::{
    BASE_MOVE, MOVE_PER_LEVEL, NAKED_ARMOR_CLASS, TbaRng, derive_vitals, level_for_xp,
    xp_threshold, xp_to_advance,
};
pub use rewards::{
    KillReward, NpcDefeated, alignment_after_kill, alignment_for, award_kill_rewards, grant_kill,
    reward_for,
};
pub use round::{ROUND_SECONDS, TbaRound, TbaRoundClock, run_due_round};
pub use systems::{resolve_rounds, weapon_dice};
