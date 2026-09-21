//! **The only module permitted to contain a delete primitive** (`SAFETY.md`
//! R2). `scripts/check-no-delete.sh` fails CI if `unlink`, `rmdir`,
//! `remove_dir_all`, `remove_file`, `remove_dir`, `rmtree` or an `rm`
//! invocation appears under `src/` outside this directory.
//!
//! Even here, `gc` never runs implicitly: it itemises exactly what it would
//! remove, and requires the operator to type the attic directory's name back.
//!
//! Implemented in M5.

use std::path::PathBuf;

/// One attic generation offered for removal, with its size and age.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub dir: PathBuf,
    pub bytes: u64,
    pub age_days: u64,
}

/// Enumerate removable attic directories. Read-only.
pub fn candidates() -> crate::Result<Vec<Candidate>> {
    todo!("M5")
}

/// Remove one attic directory after the operator has typed its name back.
/// `confirmed_name` must equal the directory's file name or this refuses.
pub fn collect(_candidate: &Candidate, _confirmed_name: &str) -> crate::Result<()> {
    todo!("M5")
}
