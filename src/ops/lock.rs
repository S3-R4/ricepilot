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

use rustix::fs::{AtFlags, FlockOperation, Mode, OFlags};

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
///
/// And it is opened only as what it should be (D74): a regular file owned by
/// the user running ricepilot. The runtime directory is the user's own, but
/// `RICEPILOT_RUNTIME_DIR` can point anywhere, and a symlink planted at the
/// lock's name would have had this `O_CREAT` open — or create — a file
/// wherever it pointed. So a symlink is not followed (`O_NOFOLLOW`), a fifo,
/// socket, device or directory is refused before it is opened (an open of a
/// device can do something by itself) and again from the descriptor's
/// `fstat`, which is also where the owner is checked. `O_NONBLOCK`, so that a
/// fifo swapped in between the look and the open cannot hang the open.
pub fn acquire(path: &Path) -> Result<Lock> {
    let parent = path.parent().ok_or_else(|| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: "lock path has no parent directory".into(),
    })?;
    super::mutate::make_dirs(parent)?;

    let (dirfd, name) = super::read::parent_dirfd(path)?;
    if let Ok(st) = rustix::fs::statat(&dirfd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        refuse_unless_ours(path, &st)?;
    }
    let fd = rustix::fs::openat(
        &dirfd,
        name.as_os_str(),
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|e| match e {
        // What `O_NOFOLLOW` answers for a symlink swapped in after the look.
        rustix::io::Errno::LOOP => not_a_lock_file(path, "a symlink".into()),
        e => io(format!("opening lock file {}", path.display()), e),
    })?;
    let st = rustix::fs::fstat(&fd)
        .map_err(|e| io(format!("stat of lock file {}", path.display()), e))?;
    refuse_unless_ours(path, &st)?;

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

/// The lock file must be a regular file owned by this user (D74).
fn refuse_unless_ours(path: &Path, st: &rustix::fs::Stat) -> Result<()> {
    let m = super::read::meta_of(st);
    if m.kind != super::read::Kind::File {
        return Err(not_a_lock_file(path, super::read::kind_name(st).into()));
    }
    let me = rustix::process::geteuid().as_raw();
    if m.uid != me {
        return Err(not_a_lock_file(
            path,
            format!("a file owned by uid {}, not by you (uid {me})", m.uid),
        ));
    }
    Ok(())
}

fn not_a_lock_file(path: &Path, what: String) -> Error {
    Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!(
            "the lock file must be a regular file you own, and this is {what}. it was not \
             followed or locked: anything else could have been planted to make ricepilot \
             open, create or wait on some other file. nothing was changed. if no ricepilot \
             is running, move it aside and run the command again (D74)"
        ),
    }
}
