//! Per-player persisted DG variables.

use std::collections::BTreeMap;

use mib_core::prelude::*;
use serde::{Deserialize, Serialize};

/// Per-player persisted DG variables.
///
/// Registered under `DocKind::Player`, so quest variables survive a reconnect
/// and a restart. `remote` is the DG per-player scope (the quest state in the
/// corpus); `global` is stored on the setting player.
#[derive(Debug, Clone, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaTriggerVars {
    /// The player's `remote` variables, keyed lowercased.
    pub remote: BTreeMap<String, String>,
    /// The `global` variables visible to this player.
    pub global: BTreeMap<String, String>,
}
