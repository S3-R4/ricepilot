//! `state/generations/NNNN.toml` plus a `current` pointer. `rollback`
//! re-applies generation NNNN-1 through the ordinary switch path — it is not a
//! separate, less-tested code path.
//!
//! Implemented in M3.

pub fn current() -> crate::Result<u32> {
    todo!("M3")
}
