//! Read-only syscalls. Opens are always `O_PATH | O_NOFOLLOW` so a hostile
//! symlink on the path cannot be traversed; directories are addressed by
//! dirfd and `*at()` throughout.
//!
//! Nothing here changes anything. The mutating side lives in
//! [`crate::ops::mutate`] so the two can be audited separately.

use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, Dir, Mode, OFlags};

use crate::{Error, Result};

/// The kinds of thing a destination can be. Deliberately coarse: the planner
/// only ever distinguishes directory, regular file, symlink and "something
/// else we will not touch".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    Other,
}

/// The facts the planner needs about one inode, all from a single
/// `fstatat(AT_SYMLINK_NOFOLLOW)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub dev: u64,
    pub ino: u64,
    /// Permission bits only (`st_mode & 0o7777`).
    pub mode: u32,
}

// `st_dev`/`st_ino`/`f_type` are `c_ulong`/`c_long` on some targets and fixed
// width on others, so these casts are a no-op on x86-64 and load-bearing
// elsewhere.
#[allow(clippy::unnecessary_cast)]
fn meta_of(st: &rustix::fs::Stat) -> Meta {
    let mode = st.st_mode as u32;
    let kind = match mode & 0o170000 {
        0o040000 => Kind::Dir,
        0o100000 => Kind::File,
        0o120000 => Kind::Symlink,
        _ => Kind::Other,
    };
    Meta {
        kind,
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
        mode: mode & 0o7777,
    }
}

/// Shared by [`super::mutate`] so both sides of the boundary report a syscall
/// failure the same way.
pub(super) fn io(context: impl Into<String>, e: rustix::io::Errno) -> Error {
    Error::Io {
        context: context.into(),
        source: std::io::Error::from_raw_os_error(e.raw_os_error()),
    }
}

/// Split an absolute path into its parent's normal components and its final
/// component. Refuses anything relative or containing `.`/`..`: a path with a
/// `..` in it cannot be walked safely component-by-component, because the
/// meaning of `..` depends on what the previous component resolved to.
pub(super) fn split_absolute(path: &Path) -> Result<(Vec<&OsStr>, &OsStr)> {
    let mut parts: Vec<&OsStr> = Vec::new();
    let mut saw_root = false;
    for c in path.components() {
        match c {
            Component::RootDir => saw_root = true,
            Component::Normal(n) => parts.push(n),
            _ => {
                return Err(Error::Refused {
                    rule: "R1",
                    path: path.to_path_buf(),
                    why: "path must be absolute and free of `.` and `..`".into(),
                })
            }
        }
    }
    let last = parts
        .pop()
        .filter(|_| saw_root)
        .ok_or_else(|| Error::Refused {
            rule: "R1",
            path: path.to_path_buf(),
            why: "path must be absolute and name a final component".into(),
        })?;
    Ok((parts, last))
}

/// Walk `components` from `/`, opening each with `O_PATH | O_NOFOLLOW`.
///
/// A symlinked intermediate component is a refusal rather than something we
/// quietly follow: if `~/.config` is a link, every conclusion we would draw
/// about what lives under it is a conclusion about somewhere else.
pub(super) fn walk(components: &[&OsStr]) -> Result<OwnedFd> {
    let mut fd = rustix::fs::open(
        "/",
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io("opening /", e))?;

    let mut so_far = PathBuf::from("/");
    for name in components {
        so_far.push(name);
        let next = rustix::fs::openat(
            &fd,
            *name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| io(format!("opening {}", so_far.display()), e))?;

        let st =
            rustix::fs::fstat(&next).map_err(|e| io(format!("stat {}", so_far.display()), e))?;
        match meta_of(&st).kind {
            Kind::Dir => fd = next,
            Kind::Symlink => {
                return Err(Error::Refused {
                    rule: "R3",
                    path: so_far,
                    why: "intermediate path component is a symlink; refusing to traverse it".into(),
                })
            }
            _ => {
                return Err(Error::Refused {
                    rule: "R3",
                    path: so_far,
                    why: "intermediate path component is not a directory".into(),
                })
            }
        }
    }
    Ok(fd)
}

/// A dirfd on `path`'s parent directory, opened without following any symlink.
pub fn parent_dirfd(path: &Path) -> Result<(OwnedFd, OsString)> {
    let (parents, last) = split_absolute(path)?;
    let fd = walk(&parents)?;
    Ok((fd, last.to_os_string()))
}

/// `fstatat(AT_SYMLINK_NOFOLLOW)`. Never follows the final component.
/// `Ok(None)` means the path does not exist (shape 5, absent).
pub fn lstat(path: &Path) -> Result<Option<Meta>> {
    let (dirfd, name) = parent_dirfd(path)?;
    match rustix::fs::statat(&dirfd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => Ok(Some(meta_of(&st))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(e) => Err(io(format!("lstat {}", path.display()), e)),
    }
}

/// Whether the final component resolves when symlinks *are* followed. Used
/// only to label a symlink as dangling; we never open the resolved target.
pub fn resolves(path: &Path) -> Result<bool> {
    let (dirfd, name) = parent_dirfd(path)?;
    match rustix::fs::statat(&dirfd, name.as_os_str(), AtFlags::empty()) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) | Err(rustix::io::Errno::LOOP) => Ok(false),
        Err(e) => Err(io(format!("stat {}", path.display()), e)),
    }
}

/// `readlinkat`. Returns the raw target string, not a canonicalised path.
pub fn readlink(path: &Path) -> Result<PathBuf> {
    let (dirfd, name) = parent_dirfd(path)?;
    let target = rustix::fs::readlinkat(&dirfd, name.as_os_str(), Vec::new())
        .map_err(|e| io(format!("readlink {}", path.display()), e))?;
    Ok(PathBuf::from(
        OsStr::from_bytes(target.as_bytes()).to_os_string(),
    ))
}

/// `st_dev` and `statfs` `f_type` of a directory, for the `EXDEV` pre-flight
/// and the mountpoint check.
#[allow(clippy::unnecessary_cast)]
pub fn dev_and_fs_type(dir: &Path) -> Result<(u64, i64)> {
    let (parents, last) = split_absolute(dir)?;
    let mut all = parents;
    all.push(last);
    let fd = walk(&all)?;
    let st = rustix::fs::fstat(&fd).map_err(|e| io(format!("stat {}", dir.display()), e))?;
    let sfs = rustix::fs::fstatfs(&fd).map_err(|e| io(format!("statfs {}", dir.display()), e))?;
    Ok((st.st_dev as u64, sfs.f_type as i64))
}

/// Read a regular file whole, without following a symlink at the final
/// component. Named `slurp` rather than the obvious thing because the ops
/// boundary grep (`SAFETY.md` R2) bans that name everywhere else.
pub fn slurp(path: &Path) -> Result<String> {
    let (dirfd, name) = parent_dirfd(path)?;
    let fd = rustix::fs::openat(
        &dirfd,
        name.as_os_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {}", path.display()), e))?;

    let mut buf = String::new();
    std::fs::File::from(fd)
        .read_to_string(&mut buf)
        .map_err(|source| Error::Io {
            context: format!("reading {}", path.display()),
            source,
        })?;
    Ok(buf)
}

/// Entry names in a directory, sorted, excluding `.` and `..`. The directory
/// itself is opened `O_NOFOLLOW`, so a symlink in its place is an error.
pub fn list_dir(path: &Path) -> Result<Vec<OsString>> {
    let (dirfd, name) = parent_dirfd(path)?;
    let fd = rustix::fs::openat(
        &dirfd,
        name.as_os_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {}", path.display()), e))?;

    let dir = Dir::new(fd).map_err(|e| io(format!("reading {}", path.display()), e))?;
    let mut names = Vec::new();
    for entry in dir {
        let entry = entry.map_err(|e| io(format!("reading {}", path.display()), e))?;
        let raw = entry.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        names.push(OsStr::from_bytes(raw).to_os_string());
    }
    names.sort();
    Ok(names)
}

/// `st_dev` of the nearest existing ancestor of `path`.
///
/// Used to answer "which filesystem will the attic be on?" before the attic
/// directory exists. A directory created later inherits the device of the
/// directory it is created in, so the answer is the same one a later `statfs`
/// would give.
pub fn dev_of_nearest_existing_ancestor(path: &Path) -> Result<u64> {
    let mut cur = path;
    loop {
        match lstat(cur) {
            Ok(Some(m)) if m.kind == Kind::Dir => return Ok(m.dev),
            Ok(_) | Err(_) => {}
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    Err(Error::Refused {
        rule: "R1",
        path: path.to_path_buf(),
        why: "no existing ancestor directory to take a device number from".into(),
    })
}
