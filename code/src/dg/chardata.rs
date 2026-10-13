//! Character state the pack does not model, kept for DG scripts.
//!
//! Scripts read and write facts about a character that the pack's own
//! components have no field for: a title, practices, quest points, hunger and
//! thirst, per-character skill percentages, an NPC's purse, act flags set on
//! the fly and the affects a script applied. They live here, each in the
//! smallest component that holds them. Nothing in the pack's own systems reads
//! these components; they exist so a script's write is visible to the next
//! script's read.
//!
//! The components derive serde so they can be registered with the stdlib
//! `SaveRegistry` under `DocKind::Player`; registration is `module.rs`'s and
//! needs this module re-exported from `dg::host`. Until it is done they last
//! for the life of the entity. (A character's permanent stat adjustments are
//! not here: they are pinned entries of `TbaBuffs`, which is saved.)

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use mib_core::prelude::*;
use serde::{Deserialize, Serialize};

use crate::content::AffectDef;

/// `GET_COND` for a fresh player when not hungry or thirsty (`init_char`:
/// `GET_COND(ch, HUNGER) = 24`). The pack models no hunger, so a player is
/// always full.
pub const FULL_CONDITION: i32 = 24;

/// The player-special record (`player_specials->saved`) scripts touch, plus
/// the title (`ch->player.title`).
///
/// A player gets one on a script's first write; until then every field reads
/// as a freshly created character's. Persisted with the player once registered.
#[derive(Debug, Clone, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaCharExtras {
    /// `GET_TITLE`; `None` is the C's NULL pointer.
    #[serde(default)]
    pub title: Option<String>,
    /// `GET_PRACTICES` (`spells_to_learn`).
    #[serde(default)]
    pub practices: i32,
    /// `GET_QUESTPOINTS`.
    #[serde(default)]
    pub quest_points: i32,
    /// `GET_COND(ch, HUNGER)`.
    #[serde(default = "full_condition")]
    pub hunger: i32,
    /// `GET_COND(ch, THIRST)`.
    #[serde(default = "full_condition")]
    pub thirst: i32,
    /// `GET_COND(ch, DRUNK)`.
    #[serde(default)]
    pub drunk: i32,
    /// `SET_SKILL` percentages a script wrote, by `SKILL_*` / `SPELL_*`
    /// number. They win over the class grant.
    #[serde(default)]
    pub skills: BTreeMap<i32, i32>,
}

fn full_condition() -> i32 {
    FULL_CONDITION
}

impl Default for TbaCharExtras {
    fn default() -> Self {
        Self {
            title: None,
            practices: 0,
            quest_points: 0,
            hunger: FULL_CONDITION,
            thirst: FULL_CONDITION,
            drunk: 0,
            skills: BTreeMap::new(),
        }
    }
}

/// The one `dummy_mob` player-special record every NPC shares
/// (`CHECK_PLAYER_SPECIAL`): a player-only attribute read or written through
/// an NPC lands here, so it is seen through every other NPC. All zero at the
/// start, as the C's static record is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Resource)]
pub struct TbaDummyMob {
    pub practices: i32,
    pub quest_points: i32,
    pub hunger: i32,
    pub thirst: i32,
    pub drunk: i32,
    pub skills: BTreeMap<i32, i32>,
}

/// An NPC's gold and experience, which the pack keeps only on the prototype
/// (`tba.npc_stats`). Inserted on the first script write, from the prototype's
/// values. The kill reward still reads the prototype (logged as
/// `char_npc_purse_reward` on the first change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaNpcPurse {
    /// `GET_GOLD` of the mob.
    pub gold: i32,
    /// `GET_EXP` of the mob.
    pub exp: i32,
}

/// Act-array bits a script set or cleared on a character, relative to what the
/// prototype says (`MOB_FLAGS` for an NPC, `PLR_FLAGS` for a player).
#[derive(Debug, Clone, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaActFlags {
    /// Bits forced on.
    #[serde(default)]
    pub set: BTreeSet<u32>,
    /// Bits forced off.
    #[serde(default)]
    pub cleared: BTreeSet<u32>,
}

/// One affect a script applied with `dg_affect` (or `affect_to_char`).
///
/// A numeric affect also has a pinned copy in the character's `TbaBuffs`,
/// which is what makes it count and which runs out by itself when its rounds
/// do; the `numeric` field is that copy's affect, found again by equality
/// among the pinned buffs. A bit-only affect lives here alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DgAffect {
    /// `af.spell`: `SPELL_DG_AFFECT` for a script's affect.
    pub spell: i32,
    /// `af.location`, an `APPLY_*` number (`0` for a bit affect).
    pub location: i32,
    /// `af.modifier`.
    pub modifier: i8,
    /// The `AFF_*` bit, for a bit affect.
    pub bit: Option<u32>,
    /// The pulse (`DgPulse`) after which the affect has worn off; `None` is
    /// permanent (`duration == -1`). A pulse count since boot: it has to become
    /// a remaining duration before it is saved.
    pub expires: Option<i64>,
    /// The `TbaBuffs` entry that makes a numeric affect count.
    pub numeric: Option<AffectDef>,
    /// Whether this marker created the pack component its bit stands for
    /// (true sight, sneaking, hiding). A component that was already there
    /// (from a cast or the character's own skill) is left alone when the
    /// marker goes.
    #[serde(default)]
    pub owns_bit: bool,
}

/// The affects scripts applied to a character, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaDgAffects(pub Vec<DgAffect>);

/// Reads of facts the pack does not model, counted until the next mutable
/// call can report them (`BaseHost::unsupported` needs `&mut`). The counts are
/// per key with the detail line to log; interior mutability lets a `&self`
/// read bump them. A report is one log line per key carrying the count.
#[derive(Debug, Default, Resource)]
pub struct TbaUnmodelledReads(Mutex<BTreeMap<&'static str, (&'static str, u64)>>);

impl TbaUnmodelledReads {
    /// Count one read under `key`.
    pub fn note(&self, key: &'static str, detail: &'static str) {
        if let Ok(mut counts) = self.0.lock() {
            let entry = counts.entry(key).or_insert((detail, 0));
            entry.1 = entry.1.saturating_add(1);
        }
    }

    /// Take the counts: `(key, detail, count)`.
    pub fn take(&self) -> Vec<(&'static str, &'static str, u64)> {
        let Ok(mut counts) = self.0.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *counts)
            .into_iter()
            .map(|(key, (detail, count))| (key, detail, count))
            .collect()
    }
}
