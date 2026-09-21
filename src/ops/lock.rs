//! `flock(LOCK_EX|LOCK_NB)` on `$XDG_RUNTIME_DIR/ricepilot.lock`, held for the
//! whole process lifetime. Non-blocking: a second ricepilot exits `Locked`
//! rather than queueing behind a half-finished switch.
//!
//! Implemented in M2.

/// The held lock. Dropping it closes the fd and releases the lock.
#[derive(Debug)]
pub struct Lock {
    _private: (),
}

pub fn acquire() -> crate::Result<Lock> {
    todo!("M2")
}
