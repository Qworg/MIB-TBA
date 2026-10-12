//! DG script triggers (issue #15, `docs/design/triggers.md`).
//!
//! The pack owns the corpus, the parser, and a bounded interpreter:
//!
//! - [`content`] — the `tba.triggers` section, one record per essential DG
//!   script, keyed by a stable [`content::TriggerId`].
//! - [`ast`] — the Phase-1 parser: the bounded DG subset into an AST, with
//!   every unsupported line recorded as a named marker.
//! - [`context`] — the `%…%` substitution resolver and its plain-data
//!   [`context::TriggerContext`].
//! - [`eval`] — the Phase-2 interpreter (`if`/`switch`/`while`, conditions,
//!   text commands, step cap).
//! - [`index`] — [`index::TriggerIndex`] / [`index::TbaTriggers`], rebuilt at
//!   build/reload.
//! - [`events`] — the events that fire: speech, command, greet/entry, act.
//! - [`effects`] — Phase 3 persisted [`effects::TbaTriggerVars`] and the
//!   exclusive drain for `%load%`/`%purge%` and variable writes; Phase 4 adds
//!   the movement/gate requests (`%teleport%`/`%door%`/`%force%`/`%at%`/
//!   `%zoneecho%`) to the same drain.
//! - [`timers`] — Phase 4 [`timers::TriggerTimers`]: the `wait` queue keyed on
//!   the engine clock, with the resume system.
//! - [`quests`] — the `stdlib.quests` bridge (native kills -> `QuestEvent`).
//!
//! Phase 5 owns combat. Every construct outside the Phase-4 subset is
//! parsed-but-deferred and reported by name, never executed.

pub mod ast;
pub mod content;
pub mod context;
pub mod effects;
pub mod eval;
pub mod events;
pub mod index;
pub mod quests;
pub mod timers;
