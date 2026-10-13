//! The DG Scripts virtual machine (issue #29).
//!
//! A line-faithful port of tbaMUD's trigger interpreter. The reference is the
//! C source (`tbamud/src/dg_*.c`); every module names the file and function it
//! ports, and a behaviour that differs from the C is a bug unless the module
//! documents the deviation and why.
//!
//! The VM is engine-agnostic. It reaches the game world only through
//! [`host::DgHost`], a composition of small sub-traits of typed, data-level
//! primitives. The game implements them against its ECS; [`mock::MockHost`]
//! implements them over an in-memory property bag for tests.
//!
//! # Model
//!
//! DG is a *text* language: every value is a string, a script is a list of
//! lines, and an entity reference is a string of [`types::UID_CHAR`] followed
//! by a numeric id. The port keeps that model.
//!
//! - [`types`] — ids, variables, the per-entity [`types::Script`] and its
//!   [`types::TrigInstance`]s, trigger-type bits.
//! - [`cstr`] — the C string helpers the interpreter's behaviour depends on
//!   (`atoi`, `str_cmp`, `one_argument`, …).
//! - [`script`] — a script body as classified lines ([`script::Program`]).
//! - [`driver`] — `script_driver`: control flow and the built-in statements.
//! - [`expr`] — `eval_expr`/`eval_op`: the expression evaluator.
//! - [`subst`] — `var_subst`/`find_replacement`: `%…%` substitution.
//! - [`text`] — `text_processed`: string functions on a variable's value.
//! - [`fields_char`], [`fields_obj`], [`fields_room`] — the entity field
//!   tables of `find_replacement`.
//! - [`commands`] — the script command interpreters (`m*`/`o*`/`w*`).
//! - [`magic`] — `dg_cast` and `dg_affect`.
//! - [`dispatch`] — `dg_triggers.c`: which trigger fires for which event.
//! - [`mock`] — the in-memory host.
//!
//! # Call graph
//!
//! ```text
//! dispatch::*_trigger  ──►  driver::script_driver
//!                               │  control lines ──► expr::process_if ──► subst::var_subst
//!                               │  other lines   ──► subst::var_subst, then by first word:
//!                               │       eval/set/global/remote/wait/…   (driver)
//!                               │       dg_cast / dg_affect             (magic)
//!                               │       m*/o*/w* script commands        (commands)
//!                               │       anything else, mobs only        (host.mud_command)
//!                               ▼
//!                       subst::find_replacement ──► text / fields_* ──► host primitives
//! ```
//!
//! A script command can fire further triggers (`mload` fires `load`), so the
//! graph is re-entrant through the host; [`types::MAX_SCRIPT_DEPTH`] bounds it.

pub mod commands;
pub mod cstr;
pub mod dispatch;
pub mod driver;
pub mod expr;
pub mod fields_char;
pub mod fields_obj;
pub mod fields_room;
pub mod host;
pub mod magic;
pub mod mock;
pub mod script;
pub mod subst;
pub mod text;
pub mod types;

pub use host::{BaseHost, Cx, DgHost};
pub use types::{AttachType, Kind, Script, TrigInstance, TrigProto, Uid, Var, VarList};
