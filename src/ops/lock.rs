//! `flock(LOCK_EX|LOCK_NB)` on `$XDG_RUNTIME_DIR/ricepilot.lock`, held for the
//! whole process lifetime.
//!
//! Non-blocking on purpose. If a second ricepilot queued, it would wait behind
//! a switch that may be half-finished and then start its own pre-flight from
//! an observation taken before the first one's phase B — which is the one
//! circumstance in which two correct plans compose into a broken machine. It
//! exits `Locked` (5) instead, and the exit code says which of the two
//! "nothing happened" outcomes this was.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, Mode, OFlags};

use super::read::io;
use crate::{Error, Result};

/// The held lock. Dropping it closes the descriptor, which is what releases
/// the `flock` — so the lock's lifetime is the value's lifetime, and a caller
/// that wants it for the process lifetime keeps the value alive.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
    /// Held so the descriptor — and with it the lock — outlives the call that
    /// took it. Read only by [`Lock::is_at_its_path`].
    fd: OwnedFd,
}

impl Lock {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file this lock is held on is still the file at its path.
    ///
    /// `flock` attaches to an inode, not a name. While this process holds
    /// the lock no other ricepilot can hold it *on this inode* — but if the
    /// file at the path was replaced (renamed over, or moved away and made
    /// again), the next ricepilot opens the new file and locks that, and both
    /// believe they are alone. So "no other ricepilot holds the lock" is
    /// proved by holding it **and** by the path still naming the inode it is
    /// held on: one `fstat` of the descriptor, one `lstat` of the path, the
    /// `(dev, ino)` of each compared (D58). Read-only.
    #[allow(clippy::unnecessary_cast)]
    pub fn is_at_its_path(&self) -> Result<bool> {
        let held = rustix::fs::fstat(&self.fd).map_err(|e| {
            io(
                format!("stat of the lock held on {}", self.path.display()),
                e,
            )
        })?;
        let now = super::read::lstat(&self.path)?;
        Ok(now.is_some_and(|m| {
            m.kind == super::read::Kind::File
                && (m.dev, m.ino) == (held.st_dev as u64, held.st_ino as u64)
        }))
    }
}

/// Take the lock, or return [`Error::Locked`] if another ricepilot holds it.
///
/// The lock file is opened, never truncated: its *contents* are irrelevant —
/// `flock` attaches to the open file description, not to anything in the file
/// — and truncating it would be a destructive operation performed for no
/// reason (`SAFETY.md` R2).
pub fn acquire(path: &Path) -> Result<Lock> {
    let parent = path.parent().ok_or_else(|| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: "lock path has no parent directory".into(),
    })?;
    super::mutate::make_dirs(parent)?;

    let (dirfd, name) = super::read::parent_dirfd(path)?;
    let fd = rustix::fs::openat(
        &dirfd,
        name.as_os_str(),
        OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|e| io(format!("opening lock file {}", path.display()), e))?;

    match rustix::fs::flock(&fd, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Lock {
            path: path.to_path_buf(),
            fd,
        }),
        Err(rustix::io::Errno::WOULDBLOCK) => Err(Error::Locked {
            path: path.to_path_buf(),
        }),
        Err(e) => Err(io(format!("locking {}", path.display()), e)),
    }
}
