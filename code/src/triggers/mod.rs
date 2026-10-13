//! DG script triggers (issue #15, `docs/design/triggers.md`).
//!
//! The pack owns the corpus and the kill bridge; the interpreter is
//! [`dg_vm`], hosted by [`crate::dg`].
//!
//! - [`content`] — the `tba.triggers` section, one record per DG script,
//!   keyed by a stable [`content::TriggerId`], plus each owner's ordered
//!   attachment list.
//! - [`quests`] — the `stdlib.quests` bridge (native kills -> `QuestEvent`).

pub mod content;
pub mod quests;
