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

/// The facts about one inode, all from a single
/// `fstatat(AT_SYMLINK_NOFOLLOW)`.
///
/// The planner needs only `kind`, `dev`, `ino` and `mode`. [`crate::verify`]
/// needs the ownership and the mtime too, and taking them from the same
/// `statat` the caller already made is both cheaper and — more to the point —
/// atomic: a hash and a mode read a moment apart can describe two different
/// files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub dev: u64,
    pub ino: u64,
    /// Permission bits only (`st_mode & 0o7777`).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Whole nanoseconds since the epoch, so a manifest comparison is an
    /// integer comparison rather than a float one.
    pub mtime_ns: i64,
}

// `st_dev`/`st_ino`/`f_type` are `c_ulong`/`c_long` on some targets and fixed
// width on others, so these casts are a no-op on x86-64 and load-bearing
// elsewhere.
#[allow(clippy::unnecessary_cast)]
pub(super) fn meta_of(st: &rustix::fs::Stat) -> Meta {
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
        uid: st.st_uid as u32,
        gid: st.st_gid as u32,
        mtime_ns: st.st_mtime as i64 * 1_000_000_000 + st.st_mtime_nsec as i64,
    }
}

/// The mtime of whatever is at `path`, without following a final symlink.
///
/// A thin wrapper over [`lstat`], kept as a named helper because the tests
/// that assert a pre-existing target was never touched read better for it,
/// and because it lives on the *read* side: it was in `mutate` only because
/// M2 needed it there as a proof obligation, and `verify` is what made that
/// untidiness visible.
pub fn mtime_ns(path: &Path) -> Result<i64> {
    lstat(path)?
        .map(|m| m.mtime_ns)
        .ok_or_else(|| Error::Refused {
            rule: "R1",
            path: path.to_path_buf(),
            why: "nothing is here to take a modification time from".into(),
        })
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

/// [`lstat`], but a missing *ancestor* directory also answers "absent".
///
/// `lstat` reports a missing intermediate component as an error, which is the
/// right answer when the caller believes the directory is there. It is the
/// wrong answer for "is there an in-flight journal?" on a machine where
/// `state/journal/` has never been created: that is the ordinary case, not a
/// fault.
///
/// A *symlinked* component still refuses (D9). Only absence is softened.
pub fn lstat_or_absent(path: &Path) -> Result<Option<Meta>> {
    match lstat(path) {
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        other => other,
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

/// The size in bytes of whatever is at `path`, from one `fstatat` and
/// without following a final symlink.
///
/// Kept off [`Meta`] because only the survey wants it: `Meta` is the set of
/// facts the planner and `verify` both need, and a field that one caller
/// reads is a field every test has to construct.
#[allow(clippy::unnecessary_cast)]
pub fn size_of(path: &Path) -> Result<u64> {
    let (dirfd, name) = parent_dirfd(path)?;
    let st = rustix::fs::statat(&dirfd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|e| io(format!("stat {}", path.display()), e))?;
    Ok(st.st_size as u64)
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

/// Open a regular file for reading, and nothing else: not through a symlink
/// at the final component, and not a fifo, a socket, a device or a
/// directory.
///
/// A fifo planted where ricepilot expects a file would otherwise stop
/// `doctor`, `diff` and `status` dead: an `O_RDONLY` open of a fifo waits
/// for a writer, for ever. So the thing is looked at before it is opened —
/// an open of a device can itself do something, and a fifo is refused
/// without ever being opened — and opened `O_NONBLOCK` anyway, so a fifo
/// swapped in between the look and the open cannot hang the open either;
/// then the descriptor's own `fstat` must say "regular file". (`O_NONBLOCK`
/// changes nothing about reading a regular file.)
pub(super) fn open_regular(path: &Path) -> Result<OwnedFd> {
    let (dirfd, name) = parent_dirfd(path)?;
    // Absence and the like are left for the open to report, in the words it
    // always has: callers match on `NotFound` from it.
    if let Ok(st) = rustix::fs::statat(&dirfd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        if meta_of(&st).kind == Kind::Other {
            return Err(not_regular(path, &st));
        }
    }
    let fd = rustix::fs::openat(
        &dirfd,
        name.as_os_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {}", path.display()), e))?;
    let st = rustix::fs::fstat(&fd).map_err(|e| io(format!("stat {}", path.display()), e))?;
    if meta_of(&st).kind != Kind::File {
        return Err(not_regular(path, &st));
    }
    Ok(fd)
}

// `st_mode` is `u32` here and not everywhere, as in [`meta_of`].
#[allow(clippy::unnecessary_cast)]
fn not_regular(path: &Path, st: &rustix::fs::Stat) -> Error {
    let what = match st.st_mode as u32 & 0o170000 {
        0o010000 => "a fifo",
        0o140000 => "a socket",
        0o020000 => "a character device",
        0o060000 => "a block device",
        0o040000 => "a directory",
        0o120000 => "a symlink",
        _ => "not a regular file",
    };
    Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!(
            "expected a regular file here, and this is {what}. ricepilot reads only regular \
             files: opening a fifo would wait for a writer for ever"
        ),
    }
}

/// Read a regular file whole, without following a symlink at the final
/// component. Named `slurp` rather than the obvious thing because the ops
/// boundary grep (`SAFETY.md` R2) bans that name everywhere else.
pub fn slurp(path: &Path) -> Result<String> {
    let fd = open_regular(path)?;

    let mut buf = String::new();
    std::fs::File::from(fd)
        .read_to_string(&mut buf)
        .map_err(|source| Error::Io {
            context: format!("reading {}", path.display()),
            source,
        })?;
    Ok(buf)
}

/// Feed a regular file's bytes to `sink` in chunks, without following a
/// symlink at the final component.
///
/// Chunked rather than returning a `Vec<u8>` so [`crate::verify`] can hash a
/// tree of any size in bounded memory, and so the bytes of a user's config
/// never accumulate anywhere ricepilot could accidentally write them out.
pub fn read_into(path: &Path, sink: &mut dyn FnMut(&[u8])) -> Result<()> {
    let fd = open_regular(path)?;

    let mut file = std::fs::File::from(fd);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|source| Error::Io {
            context: format!("reading {}", path.display()),
            source,
        })?;
        if n == 0 {
            return Ok(());
        }
        sink(&buf[..n]);
    }
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

// ---------------------------------------------------------------------------
// Open directories: a walk the filesystem cannot redirect half way
// ---------------------------------------------------------------------------

/// A directory held open `O_RDONLY | O_DIRECTORY | O_NOFOLLOW`, reached from
/// `/` without following a symlink at any component (D9), with every name
/// below it looked up relative to the descriptor.
///
/// What [`lstat`] and [`list_dir`] cannot promise and this can: that the
/// second step of a walk is taken in the directory the first step found.
/// Those walk the whole path again from `/` on every call, so a directory
/// renamed, or replaced by a symlink, between two calls sends the second one
/// somewhere else. `gc` walks what it is about to take away through these,
/// so every step is made inside the directory the previous step opened and
/// checked (D62).
///
/// Nothing here changes anything. [`DirFd::as_fd`] hands the descriptor to
/// the one caller that acts on the names in it, `crate::gc`, and the
/// ops-boundary check confines what that caller may do with it to one
/// syscall in one file (D63).
#[derive(Debug)]
pub struct DirFd {
    fd: OwnedFd,
    path: PathBuf,
}

/// One entry as a single `fstatat(AT_SYMLINK_NOFOLLOW)` saw it, plus the
/// mount it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub meta: Meta,
    /// `st_size`. For a symlink, the length of its target string.
    pub size: u64,
    /// `statx`'s `stx_mnt_id`, or `None` when the kernel does not report one
    /// (before Linux 5.8). Two entries with the same `st_dev` can still be on
    /// different mounts — a bind mount of a directory on the same filesystem
    /// is one — and this is what tells them apart.
    pub mount: Option<u64>,
}

impl DirFd {
    /// Open `path`, which must be a directory and not a symlink.
    pub fn open(path: &Path) -> Result<Self> {
        let (dirfd, name) = parent_dirfd(path)?;
        Ok(DirFd {
            fd: open_directory(&dirfd, name.as_os_str(), path)?,
            path: path.to_path_buf(),
        })
    }

    /// Open the directory `name` inside this one. A symlink at `name` is an
    /// error (`O_NOFOLLOW`), never something followed.
    pub fn open_child(&self, name: &OsStr) -> Result<Self> {
        let path = self.path.join(name);
        Ok(DirFd {
            fd: open_directory(&self.fd, name, &path)?,
            path,
        })
    }

    /// Where this was opened, for messages. Not used to reach anything.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This directory itself, from its descriptor.
    #[allow(clippy::unnecessary_cast)]
    pub fn found(&self) -> Result<Found> {
        let st = rustix::fs::fstat(&self.fd)
            .map_err(|e| io(format!("stat {}", self.path.display()), e))?;
        let mount = mount_id(&self.fd, OsStr::new(""), AtFlags::EMPTY_PATH, &self.path)?;
        Ok(Found {
            meta: meta_of(&st),
            size: st.st_size as u64,
            mount,
        })
    }

    /// The entry `name` in this directory, without following it if it is a
    /// symlink and without triggering an automount. `None` if it is gone.
    #[allow(clippy::unnecessary_cast)]
    pub fn child(&self, name: &OsStr) -> Result<Option<Found>> {
        let path = self.path.join(name);
        let st = match rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io(format!("lstat {}", path.display()), e)),
        };
        let mount = mount_id(
            &self.fd,
            name,
            AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
            &path,
        )?;
        Ok(Some(Found {
            meta: meta_of(&st),
            size: st.st_size as u64,
            mount,
        }))
    }

    /// The entry names in this directory, sorted, without `.` and `..`.
    pub fn names(&self) -> Result<Vec<OsString>> {
        let dir = Dir::read_from(&self.fd)
            .map_err(|e| io(format!("reading {}", self.path.display()), e))?;
        let mut names = Vec::new();
        for entry in dir {
            let entry = entry.map_err(|e| io(format!("reading {}", self.path.display()), e))?;
            let raw = entry.file_name().to_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            names.push(OsStr::from_bytes(raw).to_os_string());
        }
        names.sort();
        Ok(names)
    }

    /// The descriptor, for the one caller that acts on names relative to it
    /// (`crate::gc`, D62). Borrowed: it closes when this value is dropped.
    pub fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd as _;
        self.fd.as_fd()
    }
}

fn open_directory<Fd: rustix::fd::AsFd>(dirfd: Fd, name: &OsStr, path: &Path) -> Result<OwnedFd> {
    rustix::fs::openat(
        dirfd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {}", path.display()), e))
}

/// `statx`'s mount id for `name` in `dirfd`, or `None` where the kernel has
/// no `statx` or does not fill the field in.
fn mount_id<Fd: rustix::fd::AsFd>(
    dirfd: Fd,
    name: &OsStr,
    flags: AtFlags,
    path: &Path,
) -> Result<Option<u64>> {
    use rustix::fs::StatxFlags;
    match rustix::fs::statx(dirfd, name, flags, StatxFlags::MNT_ID) {
        Ok(stx) if StatxFlags::from_bits_retain(stx.stx_mask).contains(StatxFlags::MNT_ID) => {
            Ok(Some(stx.stx_mnt_id))
        }
        Ok(_) | Err(rustix::io::Errno::NOSYS) => Ok(None),
        Err(e) => Err(io(format!("statx {}", path.display()), e)),
    }
}
