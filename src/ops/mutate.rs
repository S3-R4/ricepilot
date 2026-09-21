//! The closed set of mutators. Nothing else in the crate may change the
//! filesystem, and nothing here removes anything.
//!
//! Implemented in M2.

use std::path::Path;

/// Create a symlink at a sibling temp name in the destination's own directory,
/// so the later exchange is a same-directory, same-`st_dev` rename.
pub fn create_symlink_tmp(_dir: &Path, _target: &Path) -> crate::Result<std::path::PathBuf> {
    todo!("M2")
}

/// `renameat2(RENAME_EXCHANGE)`. Probed once at startup; where unsupported,
/// falls back to rename-to-attic-then-rename (both paths tested, M2 gate).
pub fn exchange(_a: &Path, _b: &Path) -> crate::Result<()> {
    todo!("M2")
}

/// Move a displaced object into `state/attic/<ts>/`. The only way anything
/// leaves its original path. Requires same `st_dev` (pre-flight guarantees it).
pub fn rename_to_attic(_from: &Path, _attic: &Path) -> crate::Result<()> {
    todo!("M2")
}

/// `fsync` a directory so a rename is durable before the next step.
pub fn fsync_dir(_dir: &Path) -> crate::Result<()> {
    todo!("M2")
}

/// temp file + fsync + rename + parent dir fsync. The only way ricepilot
/// writes a byte, and it only ever writes into its own state directory.
pub fn write_atomic(_path: &Path, _bytes: &[u8]) -> crate::Result<()> {
    todo!("M2")
}
