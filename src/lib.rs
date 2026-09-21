//! ricepilot — switch a Linux desktop between complete rice profiles by
//! atomically re-pointing directory symlinks.
//!
//! # Structural invariants (enforced by CI, see `scripts/`)
//!
//! * **No delete primitive exists outside [`gc`].** Displaced objects are
//!   renamed into the attic, never removed. (`SAFETY.md` R2)
//! * **No filesystem syscall is issued outside [`ops`].** Every other module
//!   receives data or calls a named `ops` helper. (`SAFETY.md` R2)
//! * [`plan`] is pure: `(Observed, Target) -> Plan` with zero IO.
#![forbid(unsafe_code)]

pub mod error;

pub mod cli;
pub mod doctor;
pub mod gc;
pub mod generations;
pub mod journal;
pub mod ledger;
pub mod manifest;
pub mod observe;
pub mod ops;
pub mod plan;
pub mod rescue;
pub mod survey;
pub mod verify;

pub use error::{Error, Result};
