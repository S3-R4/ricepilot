//! Read reality. Never trusts a documented layout: every live path is
//! `lstat`ed through a dirfd. All syscalls are delegated to [`crate::ops`].
//!
//! Implemented in M1.

use std::path::PathBuf;

/// The five shapes a destination path can have. This enumeration is total:
/// the switch decision table in `docs/DESIGN.md` has a row for each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// A symlink whose target is lexically inside a registered profile root
    /// *and* whose ledger entry matches path, target and `(dev, ino)`.
    OwnedLink {
        target: PathBuf,
    },
    /// A symlink we did not create, or one whose ledger entry disagrees.
    /// Includes dangling links.
    ForeignLink {
        target: PathBuf,
        dangling: bool,
    },
    RealDir,
    RealFile,
    Absent,
}

/// One observed destination, with the facts the planner needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub dest: PathBuf,
    pub shape: Shape,
    /// `st_dev` of the destination's parent directory. A rename that would
    /// cross `st_dev` fails with `EXDEV` and is refused in pre-flight.
    pub parent_dev: u64,
    /// `statfs` `f_type` of the parent, recorded for the mountpoint check.
    pub parent_fs_type: i64,
    pub is_mountpoint: bool,
}

/// Observe every destination named by the target manifest.
pub fn observe(_dests: &[PathBuf]) -> crate::Result<Vec<Observed>> {
    todo!("M1")
}
