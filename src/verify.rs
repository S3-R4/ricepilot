//! blake3 manifest over a profile: content hash for regular files,
//! target-string hash for symlinks (never followed), plus mode, uid, gid,
//! `mtime_ns` and file type. Volatile globs are excluded.
//!
//! Implemented in M3.

pub fn verify(_profile: &str) -> crate::Result<()> {
    todo!("M3")
}
