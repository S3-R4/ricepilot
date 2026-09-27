//! **The one file in ricepilot that removes anything.** It is also the one
//! exemption from the ops boundary: `scripts/check-ops-boundary.sh` lets it,
//! and only it, spell `rustix::fs::unlinkat` and `rustix::fs::AtFlags`, and
//! nothing else from that module (D63). Every other fs call it makes — every
//! open, every stat, every rename — is an `ops` function.
//!
//! What it will take apart, and how (D62):
//!
//! * **Only a tombstone.** An entry is first renamed, whole, to
//!   `state/gc/<area>-<name>/` by [`bury`] — a rename, which the `ops`
//!   mutators already do — and [`erase`] only ever runs inside `state/gc/`.
//!   A crash part way through therefore leaves a half-removed directory
//!   whose location says what it is, never a half-removed attic entry that
//!   looks intact to `doctor` or to a human reading the attic.
//! * **Only what the operator was shown.** [`erase`] is given the listing
//!   the entry was itemised with. Before each name is taken away it is looked
//!   up again and must be the same `(dev, ino)`, kind and mount as its line
//!   in that listing; before a directory is entered, the descriptor opened
//!   on it must be that inode. The first mismatch stops the removal. An
//!   object that appeared after the listing is never looked for — so the
//!   directory holding it is not empty, its removal fails, and that stops it
//!   too.
//! * **Relative to a directory descriptor, never by path.** Each directory
//!   is opened `O_RDONLY | O_DIRECTORY | O_NOFOLLOW` from the descriptor of
//!   the one above it (`ops::read::DirFd`), and each name is removed with
//!   `unlinkat` on that descriptor. A path swapped for a symlink after the
//!   walk began cannot redirect it: there is no path lookup to redirect.
//! * **A symlink is removed as a link.** `unlinkat` without `AT_REMOVEDIR`
//!   removes the directory entry it is given and does not follow it, so a
//!   displaced link in the attic goes and whatever it points at stays. A
//!   symlink is never opened as a directory — `O_NOFOLLOW` refuses — so
//!   nothing is ever walked through one.
//! * **Never across a mount.** The listing only contains objects on the
//!   state directory's filesystem and mount (`st_dev` *and* `statx` mount
//!   id; `gc` keeps an entry otherwise), and each object is re-checked
//!   against its line — including its mount id — just before it goes.
//! * **Depth first.** A directory is emptied, then removed with
//!   `AT_REMOVEDIR`, which the kernel refuses for anything not empty.
//!
//! There is no journal: the tombstone is the record. A `state/gc/<name>/` is
//! something an operator typed the name of, gc is the only thing that puts
//! anything there, and `ricepilot gc` lists what is left in one as an
//! interrupted removal that still needs its name typed again.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::ops::mutate;
use crate::ops::read::{DirFd, Found, Kind};
use crate::{Error, Result};

use super::{Obj, Tally};

/// An entry that has been moved to `state/gc/` to be removed there, and the
/// identity it must still have.
#[derive(Debug)]
pub struct Tombstone {
    gc: PathBuf,
    name: OsString,
    root: Found,
}

impl Tombstone {
    /// An entry already in `state/gc/` — an interrupted removal.
    pub fn already(gc: &Path, name: &str, root: Found) -> Self {
        Tombstone {
            gc: gc.to_path_buf(),
            name: OsString::from(name),
            root,
        }
    }

    pub fn path(&self) -> PathBuf {
        self.gc.join(&self.name)
    }
}

/// Move the entry at `from` to `gc/<name>`, whole, with one rename, and make
/// that durable. Refuses if anything is already at `gc/<name>`: two removals
/// never share a tombstone.
pub fn bury(from: &Path, gc: &Path, name: &str, root: Found) -> Result<Tombstone> {
    mutate::make_dirs(gc)?;
    let to = gc.join(name);
    mutate::rename_within(from, &to)?;
    mutate::fsync_dir(gc)?;
    if let Some(parent) = from.parent() {
        mutate::fsync_dir(parent)?;
    }
    Ok(Tombstone {
        gc: gc.to_path_buf(),
        name: OsString::from(name),
        root,
    })
}

/// Take a tombstone apart, depth first, removing only what `listing` names,
/// each object re-checked just before it goes. `listing[0]` is the entry's
/// own directory; the rest are relative to it, parents before children.
pub fn erase(t: &Tombstone, listing: &[Obj]) -> Result<Tally> {
    let gc = DirFd::open(&t.gc)?;
    let Some(top) = gc.child(&t.name)? else {
        return Err(changed(&t.path(), "is not there"));
    };
    if !same(&top, &t.root) || listing.first().map(|o| &o.found) != Some(&t.root) {
        return Err(changed(&t.path(), "is not the directory that was listed"));
    }
    let dir = gc.open_child(&t.name)?;
    if !same(&dir.found()?, &t.root) {
        return Err(changed(&t.path(), "is not the directory that was listed"));
    }

    // Each directory's listed children, by the directory's `rel`.
    let mut children: std::collections::BTreeMap<&Path, Vec<&Obj>> = Default::default();
    for o in listing.iter().skip(1) {
        let parent = o.rel.parent().unwrap_or(Path::new(""));
        children.entry(parent).or_default().push(o);
    }

    let mut done = Tally::default();
    empty(&dir, Path::new(""), &children, &mut done)?;
    drop(dir);
    unlink(&gc, &t.name, Kind::Dir)?;
    done.dirs += 1;
    mutate::fsync_dir(&t.gc)?;
    Ok(done)
}

fn empty(
    dir: &DirFd,
    rel: &Path,
    children: &std::collections::BTreeMap<&Path, Vec<&Obj>>,
    done: &mut Tally,
) -> Result<()> {
    for obj in children.get(rel).map(Vec::as_slice).unwrap_or_default() {
        let Some(name) = obj.rel.file_name() else {
            return Err(changed(
                dir.path(),
                "has an entry with no name in its listing",
            ));
        };
        let path = dir.path().join(name);
        let Some(now) = dir.child(name)? else {
            return Err(changed(&path, "is gone"));
        };
        if !same(&now, &obj.found) {
            return Err(changed(&path, "is not what was listed"));
        }
        let kind = obj.found.meta.kind;
        if kind == Kind::Dir {
            let sub = dir.open_child(name)?;
            if !same(&sub.found()?, &obj.found) {
                return Err(changed(&path, "is not the directory that was listed"));
            }
            empty(&sub, &obj.rel, children, done)?;
            drop(sub);
        }
        unlink(dir, name, kind)?;
        match kind {
            Kind::Dir => done.dirs += 1,
            Kind::Symlink => done.links += 1,
            Kind::File => done.files += 1,
            Kind::Other => done.other += 1,
        }
        if kind != Kind::Dir {
            done.bytes += obj.found.size;
        }
    }
    Ok(())
}

/// The same object: kind, `(dev, ino)` and mount. Not the mtime or size of
/// a directory, which change as it is emptied; every field was compared in
/// full before the entry was buried (`gc::collect`).
fn same(now: &Found, listed: &Found) -> bool {
    now.meta.kind == listed.meta.kind
        && now.meta.dev == listed.meta.dev
        && now.meta.ino == listed.meta.ino
        && now.mount.is_some()
        && now.mount == listed.mount
}

fn changed(path: &Path, what: &str) -> Error {
    Error::Refused {
        rule: "R2",
        path: path.to_path_buf(),
        why: format!("{what}; gc removes only what it listed, so it stopped here"),
    }
}

// ---------------------------------------------------------------------------
// The delete primitive
// ---------------------------------------------------------------------------

/// Remove the entry `name` from the directory `dir` holds open: the one call
/// in ricepilot that destroys anything.
///
/// Why this is the whole of it, and why each argument is what it is:
///
/// * `unlinkat(dirfd, name, flags)` acts on one directory entry, found by
///   `name` in the directory `dirfd` is open on. There is no path, so there
///   is no component for a concurrent rename or a planted symlink to
///   redirect; `dirfd` was opened `O_NOFOLLOW` from its parent's descriptor,
///   and `erase` checked its `(dev, ino)` against the listing before it got
///   here.
/// * Without `AT_REMOVEDIR` it removes a file, socket, fifo or **symlink
///   itself** — it never follows the link, so the target of a displaced link
///   is untouched. It refuses a directory (`EISDIR`).
/// * With `AT_REMOVEDIR` it removes a directory only if it is empty
///   (`ENOTEMPTY` otherwise), and refuses anything that is not a directory
///   (`ENOTDIR`). Nothing recursive exists in this crate: emptying is
///   `erase`'s walk, one listed name at a time.
/// * `kind` is the listed kind, just re-checked against what is there. A
///   name swapped for something else of the *same* kind between that check
///   and this call is the window that remains; it is inside ricepilot's own
///   state directory, under the lock, and needs someone writing there by hand.
///
/// `clippy.toml` disallows this function crate-wide, with `unlink` and
/// `rmdir` and the `std` removal functions, and this is the one `allow`: on
/// this function and nothing wider, so any other call site anywhere —
/// including elsewhere in `src/gc/` — fails the lint (D63).
#[allow(clippy::disallowed_methods)]
fn unlink(dir: &DirFd, name: &OsStr, kind: Kind) -> Result<()> {
    let flags = if kind == Kind::Dir {
        rustix::fs::AtFlags::REMOVEDIR
    } else {
        rustix::fs::AtFlags::empty()
    };
    rustix::fs::unlinkat(dir.as_fd(), name, flags).map_err(|e| Error::Io {
        context: format!("removing {}", dir.path().join(name).display()),
        source: std::io::Error::from_raw_os_error(e.raw_os_error()),
    })
}
