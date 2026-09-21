//! **The only module in the crate that touches the outside world.**
//!
//! `scripts/check-ops-boundary.sh` fails CI if a filesystem or subprocess
//! entry point appears anywhere under `src/` outside this directory. Every
//! other module asks `ops` for a named operation instead. The point is that an
//! auditor can read one directory and know the complete set of things
//! ricepilot can do to a machine.
//!
//! Split so the read side can be audited separately from the mutating side:
//!
//! * [`read`]   — `*at()` lookups, `O_PATH|O_NOFOLLOW`, `statfs`. No writes.
//! * [`mutate`] — the closed set of mutators. No delete primitive lives here
//!   either; deletion exists only in [`crate::gc`].
//! * [`exec`]   — the closed allowlist of subprocesses.
//! * [`lock`]   — `flock(LOCK_EX|LOCK_NB)` held for the process lifetime.
//!
//! `read`, `mutate` and `lock` landed in M2; `exec` is M5.

pub mod exec;
pub mod lock;
pub mod mutate;
pub mod read;
