//! The read side of the boundary as a value: a trait whose every method
//! reads, and one implementation, [`Live`], whose every method is a single
//! call into `ops::read` or one of the two read-only subprocesses.
//!
//! It exists so a caller can be handed *reading and nothing else*. A function
//! that takes `&dyn Look` and names no other effectful item can learn about
//! the machine only through these methods, and none of them can change the
//! machine: there is no method here that creates, renames, writes, locks or
//! starts anything but `pacman -Q` and `sh -n`. `doctor` is written against
//! it (D57), and so are the pieces of `verify`, `requires` and `rescue` that
//! `doctor` reuses, so that there is one tree walk, one requires-check and one
//! rescue-script generator rather than a read-only copy of each.
//!
//! What this module may name is checked textually by `tests/doctor.rs`: only
//! `read`'s functions and types, and `exec`'s `locate`, `run` and the two
//! read-only calls.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use super::exec::Ran;
pub use super::read::Kind;
pub use super::read::Meta;
use crate::Result;

/// Everything a read-only caller may ask of the machine.
pub trait Look {
    /// `fstatat(AT_SYMLINK_NOFOLLOW)`; `None` when nothing is there.
    fn lstat(&self, path: &Path) -> Result<Option<Meta>>;
    /// [`Look::lstat`], with a missing *ancestor* also answering `None`.
    fn lstat_or_absent(&self, path: &Path) -> Result<Option<Meta>>;
    /// The raw target string of a symlink.
    fn readlink(&self, path: &Path) -> Result<PathBuf>;
    /// Whether the final component resolves with symlinks followed. Nothing
    /// is opened at the resolved end.
    fn resolves(&self, path: &Path) -> Result<bool>;
    /// Entry names of a directory, sorted.
    fn list_dir(&self, path: &Path) -> Result<Vec<OsString>>;
    /// A regular file's content as UTF-8.
    fn slurp(&self, path: &Path) -> Result<String>;
    /// A regular file's bytes, in chunks.
    fn read_into(&self, path: &Path, sink: &mut dyn FnMut(&[u8])) -> Result<()>;
    /// `st_size`, without following a final symlink.
    fn size_of(&self, path: &Path) -> Result<u64>;
    /// Whether `pacman` is installed where `ops::exec` looks for it.
    fn pacman_found(&self) -> Result<bool>;
    /// `pacman -Q -- <package>`. A non-zero exit is an answer, not an error.
    fn pacman_query(&self, package: &str) -> Result<Ran>;
    /// `sh -n`, the script on stdin. A non-zero exit is an answer.
    fn sh_syntax(&self, script: &str) -> Result<Ran>;
}

/// The machine as it is. A unit value: holding one grants nothing that
/// [`Look`]'s methods do not.
#[derive(Debug, Clone, Copy, Default)]
pub struct Live;

impl Look for Live {
    fn lstat(&self, path: &Path) -> Result<Option<Meta>> {
        super::read::lstat(path)
    }
    fn lstat_or_absent(&self, path: &Path) -> Result<Option<Meta>> {
        super::read::lstat_or_absent(path)
    }
    fn readlink(&self, path: &Path) -> Result<PathBuf> {
        super::read::readlink(path)
    }
    fn resolves(&self, path: &Path) -> Result<bool> {
        super::read::resolves(path)
    }
    fn list_dir(&self, path: &Path) -> Result<Vec<OsString>> {
        super::read::list_dir(path)
    }
    fn slurp(&self, path: &Path) -> Result<String> {
        super::read::slurp(path)
    }
    fn read_into(&self, path: &Path, sink: &mut dyn FnMut(&[u8])) -> Result<()> {
        super::read::read_into(path, sink)
    }
    fn size_of(&self, path: &Path) -> Result<u64> {
        super::read::size_of(path)
    }
    fn pacman_found(&self) -> Result<bool> {
        Ok(super::exec::locate(super::exec::Allowed::PacmanQuery)?.is_some())
    }
    fn pacman_query(&self, package: &str) -> Result<Ran> {
        super::exec::run(super::exec::Call::PacmanQuery { package })
    }
    fn sh_syntax(&self, script: &str) -> Result<Ran> {
        super::exec::run(super::exec::Call::ShSyntaxCheck { script })
    }
}
