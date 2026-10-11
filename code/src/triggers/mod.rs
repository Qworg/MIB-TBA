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
//! - [`events`] — the Four events that fire this pass: speech, command,
//!   greet/entry, act.
//!
//! Phase 3 owns variables and the `stdlib.quests` bridge; Phase 4 owns
//! timers/movement; Phase 5 owns combat. Every construct outside the Phase-2
//! subset is parsed-but-deferred and reported by name, never executed.

pub mod ast;
pub mod content;
pub mod context;
pub mod eval;
pub mod events;
pub mod index;
