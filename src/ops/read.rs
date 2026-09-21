//! Read-only syscalls. Opens are always `O_PATH | O_NOFOLLOW` so a hostile
//! symlink on the path cannot be traversed; directories are addressed by
//! dirfd and `*at()` throughout.
//!
//! Implemented in M2.

use std::path::Path;

/// `fstatat(AT_SYMLINK_NOFOLLOW)`. Never follows the final component.
pub fn lstat(_path: &Path) -> crate::Result<()> {
    todo!("M2")
}

/// `readlinkat`. Returns the raw target string, not a canonicalised path.
pub fn readlink(_path: &Path) -> crate::Result<std::path::PathBuf> {
    todo!("M2")
}
