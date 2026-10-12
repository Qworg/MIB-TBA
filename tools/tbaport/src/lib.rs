//! `tbaport` — a deterministic tbaMUD → MIB content converter.
//!
//! This is a **build-time tool**, independent of the running server. It reads
//! the tbaMUD flat-file world (`lib/world/{wld,mob,obj,zon,shp,trg,qst}`),
//! maps the records onto the real `mib-stdlib` and `mib-tba` content types,
//! serialises them to YAML, and writes a machine-readable drop report naming
//! everything that was deferred or could not be represented.
//!
//! Design notes live in `docs/05-converter.md`; the authoritative grammar is
//! the C loader in the tbaMUD checkout (`src/db.c`, `src/structs.h`).
//!
//! ```no_run
//! use tbaport::{ConvertOptions, convert};
//!
//! let options = ConvertOptions {
//!     source_root: "/path/to/tbamud".into(),
//!     only: Some(30),
//!     start_room: Some(3001),
//! };
//! let conversion = convert(&options)?;
//! println!("{} rooms", conversion.report.totals.rooms);
//! # Ok::<(), tbaport::ConvertError>(())
//! ```

pub mod cli;
pub mod emit;
pub mod error;
pub mod ids;
pub mod mapping;
pub mod report;
pub mod source;
pub mod triage;

pub use emit::{OutputBundle, render, write_bundle};
pub use error::{ConvertError, Result};
pub use mapping::{Conversion, ConvertOptions, build_triage, convert};
pub use report::{DEFAULT_START_ROOM, DropReport};
pub use triage::TriageReport;
