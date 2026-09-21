//! `state/ledger.toml`: which live paths ricepilot owns.
//!
//! Ownership is the conjunction of three independent facts (`docs/DESIGN.md`):
//! the path is `S_IFLNK` under `O_PATH|O_NOFOLLOW`; its `readlinkat` target is
//! lexically inside a registered profile root; and a ledger entry matches the
//! path, the target *and* the `(dev, ino)`. Anything else is unowned → refuse.
//!
//! Implemented in M3.

pub fn load() -> crate::Result<()> {
    todo!("M3")
}
