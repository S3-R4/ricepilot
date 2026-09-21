//! The closed set of mutators. Nothing else in the crate may change the
//! filesystem, and **nothing here removes anything**: displaced objects are
//! renamed into the attic (`SAFETY.md` R2).
//!
//! `scripts/check-no-delete.sh` scans this directory too, which shapes two of
//! the implementations below: [`write_atomic`] opens its temp file
//! `O_CREAT | O_EXCL` and never truncates, and [`rename_to_attic`] picks a
//! free name rather than replacing whatever an earlier run left there.
//!
//! Every path is addressed through a dirfd obtained by
//! [`super::read::parent_dirfd`], so the `O_PATH | O_NOFOLLOW` component walk
//! that protects observation protects mutation as well (D9): a symlinked
//! intermediate component is refused here for the same reason it is there.

use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags, RenameFlags};

use super::read::{self, io, Kind};
use crate::plan::Op;
use crate::{Error, Result};

/// How [`exchange`] swaps two entries.
///
/// Taken as an argument rather than read from a global so a test can exercise
/// the fallback on a kernel that supports `renameat2` (the M2 gate requires
/// both paths to be covered, and "whichever one this machine happens to do"
/// would leave the other untested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeMode {
    /// `renameat2(RENAME_EXCHANGE)`: one atomic step, no window.
    Renameat2,
    /// Three same-directory renames, for a kernel or filesystem without
    /// `RENAME_EXCHANGE`. There is a window in which the destination does not
    /// exist; `recover` is what closes it.
    Fallback,
}

impl ExchangeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExchangeMode::Renameat2 => "renameat2",
            ExchangeMode::Fallback => "fallback",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "renameat2" => Some(ExchangeMode::Renameat2),
            "fallback" => Some(ExchangeMode::Fallback),
            _ => None,
        }
    }
}

/// Probe `RENAME_EXCHANGE` once, against a directory ricepilot owns.
///
/// The probe uses two **real** symlinks rather than two names that do not
/// exist. A nonexistent-name probe is a false positive: the kernel resolves
/// the names before the filesystem ever sees the flag, so an unsupporting
/// filesystem answers `ENOENT` — indistinguishable from "the flag was fine,
/// the names were not". Exchanging a name with itself is a false positive for
/// the same reason; the VFS short-circuits that case before dispatch.
///
/// The two probe links are left in place and reused on the next run. They live
/// in ricepilot's own state directory, they are two symlinks pointing at
/// nothing in particular, and creating them is cheaper than the alternative of
/// having no honest answer to this question.
pub fn probe_exchange(state_dir: &Path) -> Result<ExchangeMode> {
    let a = state_dir.join(".rp-probe-a");
    let b = state_dir.join(".rp-probe-b");
    make_dirs(state_dir)?;
    ensure_symlink(&a, Path::new(".rp-probe-a-target"))?;
    ensure_symlink(&b, Path::new(".rp-probe-b-target"))?;

    let (dirfd, a_name) = read::parent_dirfd(&a)?;
    let b_name = b.file_name().unwrap_or(OsStr::new(".rp-probe-b"));
    match rustix::fs::renameat_with(
        &dirfd,
        a_name.as_os_str(),
        &dirfd,
        b_name,
        RenameFlags::EXCHANGE,
    ) {
        Ok(()) => Ok(ExchangeMode::Renameat2),
        Err(rustix::io::Errno::NOSYS)
        | Err(rustix::io::Errno::INVAL)
        | Err(rustix::io::Errno::OPNOTSUPP) => Ok(ExchangeMode::Fallback),
        Err(e) => Err(io(
            format!("probing RENAME_EXCHANGE in {}", state_dir.display()),
            e,
        )),
    }
}

/// `mkdirat` each missing component, walking with the same `O_NOFOLLOW`
/// discipline as [`super::read`]. An existing component that is a symlink is a
/// refusal, not something to traverse.
pub fn make_dirs(path: &Path) -> Result<()> {
    let (parents, last) = read::split_absolute(path)?;
    let mut walked: Vec<&OsStr> = Vec::new();
    let mut so_far = PathBuf::from("/");

    for name in parents.iter().chain(std::iter::once(&last)) {
        so_far.push(name);
        walked.push(name);
        let dirfd = read::walk(&walked[..walked.len() - 1])?;
        match rustix::fs::mkdirat(&dirfd, *name, Mode::from_bits_truncate(0o700)) {
            Ok(()) => {}
            // Already there is the normal case; that it is a *directory* is
            // then checked by the next `walk`, which refuses a symlink.
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(io(format!("mkdir {}", so_far.display()), e)),
        }
    }
    Ok(())
}

/// `symlinkat`. Fails if anything already occupies `link_path` — a symlink is
/// created or it is not, and clobbering is not one of the options.
pub fn create_symlink(link_path: &Path, target: &Path) -> Result<()> {
    let (dirfd, name) = read::parent_dirfd(link_path)?;
    rustix::fs::symlinkat(target, &dirfd, name.as_os_str()).map_err(|e| {
        io(
            format!(
                "creating symlink {} -> {}",
                link_path.display(),
                target.display()
            ),
            e,
        )
    })
}

/// Create the staged link at the sibling temp name the planner chose, so the
/// later [`exchange`] is a same-directory — and therefore same-`st_dev` —
/// rename.
///
/// A leftover staged link from a crashed run makes this fail with `EEXIST`
/// rather than being silently reused. That is the correct outcome: the stale
/// link is evidence of an interrupted switch, and `recover` is what deals with
/// it (by moving it to the attic), not a mutator that quietly overwrites it.
pub fn create_symlink_tmp(link_path: &Path, target: &Path) -> Result<()> {
    create_symlink(link_path, target)
}

/// Create the link only if nothing is there. Used by [`probe_exchange`] for
/// its own two links, which are reused across runs.
fn ensure_symlink(link_path: &Path, target: &Path) -> Result<()> {
    if read::lstat(link_path)?.is_some() {
        return Ok(());
    }
    create_symlink(link_path, target)
}

/// Swap two entries in the same directory.
///
/// Postcondition, identically for both modes: whatever was at `a` is at `b`
/// and whatever was at `b` is at `a`. The fallback reaches that postcondition
/// rather than a convenient approximation of it, because the plan printed by
/// dry-run names the ops that follow — and they are written against this
/// postcondition (`SAFETY.md` R4).
pub fn exchange(mode: ExchangeMode, a: &Path, b: &Path) -> Result<()> {
    match mode {
        ExchangeMode::Renameat2 => exchange_atomic(a, b),
        ExchangeMode::Fallback => exchange_fallback(a, b),
    }
}

fn exchange_atomic(a: &Path, b: &Path) -> Result<()> {
    let (a_dirfd, a_name) = read::parent_dirfd(a)?;
    let (b_dirfd, b_name) = read::parent_dirfd(b)?;
    rustix::fs::renameat_with(
        &a_dirfd,
        a_name.as_os_str(),
        &b_dirfd,
        b_name.as_os_str(),
        RenameFlags::EXCHANGE,
    )
    .map_err(|e| io(format!("exchanging {} <-> {}", a.display(), b.display()), e))
}

/// The name the fallback parks one of the two entries at while it shuffles.
/// A sibling, so every rename stays inside one directory.
pub fn fallback_slot(b: &Path) -> PathBuf {
    let file = b
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    match b.parent() {
        Some(p) => p.join(format!("{file}.rp-swap")),
        None => PathBuf::from(format!("{file}.rp-swap")),
    }
}

/// Three renames, all within one directory:
///
/// 1. `b` → `b.rp-swap`   (frees `b`)
/// 2. `a` → `b`           (**the window**: nothing is at `a`)
/// 3. `b.rp-swap` → `a`
///
/// A crash in the window leaves one of a small, enumerable set of states, and
/// `recover` identifies which by reading the link targets rather than by
/// trusting a step counter.
fn exchange_fallback(a: &Path, b: &Path) -> Result<()> {
    let swap = fallback_slot(b);
    rename_within(b, &swap)?;
    rename_within(a, b)?;
    rename_within(&swap, a)
}

/// `renameat`, refusing to replace an existing entry.
///
/// `RENAME_NOREPLACE` is not assumed available — the same portability question
/// [`probe_exchange`] answers applies to it — so the check is an `lstat` first.
/// It races in principle; in practice both names are ricepilot's own staging
/// names inside a directory guarded by the process lock, and the alternative
/// (a plain rename) silently destroys whatever it lands on.
pub fn rename_within(from: &Path, to: &Path) -> Result<()> {
    if read::lstat(to)?.is_some() {
        return Err(Error::Refused {
            rule: "R2",
            path: to.to_path_buf(),
            why: format!(
                "something already occupies this path; refusing to replace it by renaming {} \
                 over it",
                from.display()
            ),
        });
    }
    rename_over(from, to)
}

/// `renameat` with no existence check. Only for the two cases where replacing
/// the target is the whole point and the target is ricepilot's own: completing
/// a fallback exchange, and [`write_atomic`]'s final step.
fn rename_over(from: &Path, to: &Path) -> Result<()> {
    let (from_dirfd, from_name) = read::parent_dirfd(from)?;
    let (to_dirfd, to_name) = read::parent_dirfd(to)?;
    rustix::fs::renameat(
        &from_dirfd,
        from_name.as_os_str(),
        &to_dirfd,
        to_name.as_os_str(),
    )
    .map_err(|e| {
        io(
            format!("renaming {} -> {}", from.display(), to.display()),
            e,
        )
    })
}

/// Move a displaced object into `attic/<rel>`. The only way anything leaves
/// its original path, and it is a rename: the object still exists afterwards,
/// at a path the user is told about.
///
/// If `attic/<rel>` is taken — a second `recover` over the same attic, say —
/// a numeric suffix is appended. Nothing in the attic is ever replaced,
/// because the attic's entire purpose is to be the place where the thing you
/// did not mean to lose is still sitting.
pub fn rename_to_attic(from: &Path, attic: &Path, rel: &Path) -> Result<PathBuf> {
    let base = attic.join(rel);
    let parent = base.parent().ok_or_else(|| Error::Refused {
        rule: "R2",
        path: base.clone(),
        why: "attic destination has no parent directory".into(),
    })?;
    make_dirs(parent)?;

    let mut candidate = base.clone();
    let mut n = 1u32;
    while read::lstat(&candidate)?.is_some() {
        let file = base
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        candidate = parent.join(format!("{file}.{n}"));
        n += 1;
        if n > 9999 {
            return Err(Error::Refused {
                rule: "R2",
                path: base,
                why: "no free name in the attic after 9999 attempts".into(),
            });
        }
    }
    rename_over(from, &candidate)?;
    Ok(candidate)
}

/// `fsync` a directory, so a rename into it is durable before the next step.
///
/// The directory is reopened `O_RDONLY | O_DIRECTORY | O_NOFOLLOW` rather than
/// reusing [`super::read`]'s walk result: that walk hands back an `O_PATH`
/// descriptor, which names a file without being open on it, and `fsync` of one
/// is `EBADF`. The walk is still what gets us there safely — only the final
/// open differs.
pub fn fsync_dir(dir: &Path) -> Result<()> {
    let (dirfd, name) = read::parent_dirfd(dir)?;
    let fd = rustix::fs::openat(
        &dirfd,
        name.as_os_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {} to fsync it", dir.display()), e))?;
    rustix::fs::fsync(&fd).map_err(|e| io(format!("fsync {}", dir.display()), e))
}

/// temp file + `fsync` + rename + parent directory `fsync`.
///
/// The only way ricepilot writes a byte, and it only ever writes inside its
/// own state directory. The temp is opened `O_CREAT | O_EXCL` and never
/// truncated: a partially written file is never something this function can
/// produce, and the no-delete rule means it is never something it can tidy
/// away either, so it must not create one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: "cannot write atomically to a path with no parent directory".into(),
    })?;
    make_dirs(parent)?;

    let (dirfd, name) = read::parent_dirfd(path)?;
    let stem = name.to_string_lossy().into_owned();

    let mut tmp_name = OsString::new();
    let mut fd = None;
    for n in 0..1000u32 {
        let candidate = OsString::from(format!(".{stem}.rp-w-{n}"));
        match rustix::fs::openat(
            &dirfd,
            candidate.as_os_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        ) {
            Ok(f) => {
                tmp_name = candidate;
                fd = Some(f);
                break;
            }
            Err(rustix::io::Errno::EXIST) => continue,
            Err(e) => return Err(io(format!("creating a temp file beside {stem}"), e)),
        }
    }
    let fd = fd.ok_or_else(|| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: "no free temp name beside this file after 1000 attempts".into(),
    })?;

    let mut file = std::fs::File::from(fd);
    file.write_all(bytes).map_err(|source| Error::Io {
        context: format!("writing {}", path.display()),
        source,
    })?;
    file.sync_all().map_err(|source| Error::Io {
        context: format!("fsync {}", path.display()),
        source,
    })?;
    drop(file);

    rustix::fs::renameat(&dirfd, tmp_name.as_os_str(), &dirfd, name.as_os_str())
        .map_err(|e| io(format!("renaming temp file into {}", path.display()), e))?;
    fsync_dir(parent)
}

/// Whether `path` is a symlink, and what it points at. `None` if nothing is
/// there; `Some(None)` if something is there but is not a symlink.
pub fn link_target(path: &Path) -> Result<Option<Option<PathBuf>>> {
    match read::lstat(path)? {
        None => Ok(None),
        Some(m) if m.kind == Kind::Symlink => Ok(Some(Some(read::readlink(path)?))),
        Some(_) => Ok(Some(None)),
    }
}

/// What [`apply`] needs that is not in the op itself.
#[derive(Debug, Clone)]
pub struct ApplyContext {
    pub mode: ExchangeMode,
    /// `state/attic/<ts>/`.
    pub attic: PathBuf,
}

/// Execute a plan's ops in order.
///
/// `after_step` is called with the index of each op once it has completed. It
/// exists so the crash-injection harness can end the process after step *k*
/// for every *k* (the M2 gate); in ordinary use it is a closure that returns
/// `Ok(())`.
///
/// There is no cleanup on error and no unwinding rollback, deliberately: an
/// error here leaves exactly the state a crash at the same point would, so the
/// recovery path is the *only* recovery path and is therefore the tested one.
pub fn apply(
    ops: &[Op],
    ctx: &ApplyContext,
    after_step: &mut dyn FnMut(usize) -> Result<()>,
) -> Result<()> {
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::CreateTempLink { link_path, target } => create_symlink_tmp(link_path, target)?,
            Op::CreateLink { link_path, target } => create_symlink(link_path, target)?,
            Op::Exchange { dest, staged } => exchange(ctx.mode, dest, staged)?,
            Op::RenameToAttic { from, attic_rel } => {
                rename_to_attic(from, &ctx.attic, attic_rel)?;
            }
            Op::FsyncDir { dir } => fsync_dir(dir)?,
        }
        after_step(i)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The copier
// ---------------------------------------------------------------------------

/// What one [`copy_tree`] moved across, for the report the user sees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyStats {
    pub dirs: usize,
    pub files: usize,
    pub links: usize,
    pub bytes: u64,
    /// Files the kernel shared rather than duplicated (`FICLONE`). On btrfs
    /// this is every file and the copy costs no space; elsewhere it is zero.
    pub cloned: usize,
}

/// Copy `from` to `to`, preserving what a config tree needs preserved.
///
/// * **Symlinks stay symlinks.** A link is recreated with the same target
///   string, byte for byte, and is never followed — the same rule
///   [`super::read`] applies to every open, for the same reason: following one
///   would copy somewhere else's tree into the profile.
/// * **Mode, owner and times are preserved.** Mode is set explicitly after
///   creation, because the `umask` applies to the creating call and a
///   mode-600 file that came back 644 would be a secret this tool widened.
///   Owner is best-effort: an unprivileged process can only keep the owner it
///   already has, and failing the whole copy over that would make `capture`
///   unusable for exactly nothing.
/// * **Nothing is ever replaced and nothing is ever shortened.** Every
///   directory is `mkdirat`ed fresh and every file is opened
///   `O_CREAT | O_EXCL`, so a half-finished earlier attempt is a refusal
///   naming the path rather than something this function overwrites — which
///   matters doubly here, because there is no delete to undo it with (R2).
/// * **Anything that is not a directory, a regular file or a symlink is a
///   refusal**, naming the path, and the whole source is walked for one
///   *before* the first byte is written. A socket or a fifo in a config tree
///   is a thing a running program owns, and a copy of one is not the same
///   object; saying so is better than producing a profile that silently is
///   not the tree it claims to be. The walk happens first because R4 says a
///   refusal has zero side effects, and "we refused, and also left two
///   thirds of a tree behind" is not zero.
///
/// Data is moved with `FICLONE` where the filesystem supports it — on btrfs
/// that makes a 7 GB baseline instant and free — and by a 64K read/write loop
/// where it does not (D43).
pub fn copy_tree(from: &Path, to: &Path) -> Result<CopyStats> {
    if to.starts_with(from) {
        return Err(Error::Refused {
            rule: "R5",
            path: to.to_path_buf(),
            why: format!(
                "is inside {}, so copying one into the other would never finish",
                from.display()
            ),
        });
    }
    if read::lstat_or_absent(to)?.is_some() {
        return Err(Error::Refused {
            rule: "R2",
            path: to.to_path_buf(),
            why: "something is already here. ricepilot copies into a name nothing occupies, so \
                  that an interrupted earlier attempt is something you can look at rather than \
                  something this overwrote"
                .into(),
        });
    }
    let parent = to.parent().ok_or_else(|| Error::Refused {
        rule: "R5",
        path: to.to_path_buf(),
        why: "copy destination has no parent directory".into(),
    })?;
    make_dirs(parent)?;
    refuse_unsupported(from)?;

    let mut stats = CopyStats::default();
    copy_one(from, to, &mut stats)?;
    fsync_dir(parent)?;
    Ok(stats)
}

/// Walk the source and refuse anything [`copy_one`] could not copy, before
/// anything has been created. Stat-only, so it costs a walk and no data.
fn refuse_unsupported(from: &Path) -> Result<()> {
    let meta = read::lstat(from)?.ok_or_else(|| Error::Refused {
        rule: "R5",
        path: from.to_path_buf(),
        why: "there is nothing here to copy".into(),
    })?;
    match meta.kind {
        Kind::File | Kind::Symlink => Ok(()),
        Kind::Dir => {
            for child in read::list_dir(from)? {
                refuse_unsupported(&from.join(child))?;
            }
            Ok(())
        }
        Kind::Other => Err(unsupported(from)),
    }
}

fn unsupported(path: &Path) -> Error {
    Error::Refused {
        rule: "R5",
        path: path.to_path_buf(),
        why: "is neither a directory, a regular file nor a symlink. ricepilot will not pretend \
              a copy of a socket or a fifo is the same object; declare it volatile or move it \
              aside"
            .into(),
    }
}

/// One entry, whatever kind it is. `to` must not exist.
fn copy_one(from: &Path, to: &Path, stats: &mut CopyStats) -> Result<()> {
    let (dirfd, name) = read::parent_dirfd(from)?;
    let st = rustix::fs::statat(
        &dirfd,
        name.as_os_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| io(format!("stat {}", from.display()), e))?;
    let meta = read::meta_of(&st);
    let times = times_of(&st);

    match meta.kind {
        Kind::Dir => {
            copy_dir(from, to, &meta, &times, stats)?;
            stats.dirs += 1;
        }
        Kind::File => {
            copy_file(from, to, &meta, &times, stats)?;
            stats.files += 1;
        }
        Kind::Symlink => {
            copy_symlink(from, to, &meta, &times)?;
            stats.links += 1;
        }
        // Unreachable in practice: `refuse_unsupported` walked the whole
        // source before this started. Kept because the enum is total and a
        // panic here would be a worse answer than the refusal.
        Kind::Other => return Err(unsupported(from)),
    }
    Ok(())
}

fn copy_dir(
    from: &Path,
    to: &Path,
    meta: &read::Meta,
    times: &rustix::fs::Timestamps,
    stats: &mut CopyStats,
) -> Result<()> {
    let (dirfd, name) = read::parent_dirfd(to)?;
    rustix::fs::mkdirat(&dirfd, name.as_os_str(), Mode::from_bits_truncate(0o700))
        .map_err(|e| io(format!("mkdir {}", to.display()), e))?;

    for child in read::list_dir(from)? {
        copy_one(&from.join(&child), &to.join(&child), stats)?;
    }

    // Mode, owner and times are applied *after* the children, because adding
    // an entry to a directory moves its mtime and a read-only directory
    // cannot be written into.
    let fd = rustix::fs::openat(
        &dirfd,
        name.as_os_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {} to set its metadata", to.display()), e))?;
    rustix::fs::fchmod(&fd, Mode::from_bits_truncate(meta.mode))
        .map_err(|e| io(format!("setting the mode of {}", to.display()), e))?;
    keep_owner(&fd, meta);
    rustix::fs::futimens(&fd, times)
        .map_err(|e| io(format!("setting the times of {}", to.display()), e))?;
    rustix::fs::fsync(&fd).map_err(|e| io(format!("fsync {}", to.display()), e))
}

fn copy_file(
    from: &Path,
    to: &Path,
    meta: &read::Meta,
    times: &rustix::fs::Timestamps,
    stats: &mut CopyStats,
) -> Result<()> {
    let (from_dirfd, from_name) = read::parent_dirfd(from)?;
    let src = rustix::fs::openat(
        &from_dirfd,
        from_name.as_os_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io(format!("opening {}", from.display()), e))?;

    let (to_dirfd, to_name) = read::parent_dirfd(to)?;
    let dst = rustix::fs::openat(
        &to_dirfd,
        to_name.as_os_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
        Mode::from_bits_truncate(meta.mode),
    )
    .map_err(|e| io(format!("creating {}", to.display()), e))?;

    // One ioctl for the whole file where the filesystem can share extents,
    // and a plain loop where it cannot. Any error at all falls back: the
    // reasons it can fail (different filesystem, no support, not a regular
    // file) are all reasons to copy the bytes instead, and none of them has
    // written anything to the destination.
    match rustix::fs::ioctl_ficlone(&dst, &src) {
        Ok(()) => {
            stats.cloned += 1;
            stats.bytes += size_of_file(&src, from)?;
        }
        Err(_) => stats.bytes += stream(&src, &dst, from, to)?,
    }

    rustix::fs::fchmod(&dst, Mode::from_bits_truncate(meta.mode))
        .map_err(|e| io(format!("setting the mode of {}", to.display()), e))?;
    keep_owner(&dst, meta);
    rustix::fs::futimens(&dst, times)
        .map_err(|e| io(format!("setting the times of {}", to.display()), e))
}

fn copy_symlink(
    from: &Path,
    to: &Path,
    meta: &read::Meta,
    times: &rustix::fs::Timestamps,
) -> Result<()> {
    let target = read::readlink(from)?;
    create_symlink(to, &target)?;

    let (dirfd, name) = read::parent_dirfd(to)?;
    // A link has no fd of its own to act on: `O_PATH|O_NOFOLLOW` opens it
    // without opening what it points at, but `fchmod`/`futimens` on an
    // `O_PATH` descriptor are `EBADF`. The `*at` forms with
    // `AT_SYMLINK_NOFOLLOW` are the ones that act on the link itself.
    //
    // Its mode is not set: Linux has no `lchmod`, and a symlink's own
    // permission bits are unused by the kernel anyway.
    let (uid, gid) = owner_ids(meta);
    let _ = rustix::fs::chownat(
        &dirfd,
        name.as_os_str(),
        uid,
        gid,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    );
    rustix::fs::utimensat(
        &dirfd,
        name.as_os_str(),
        times,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| io(format!("setting the times of {}", to.display()), e))
}

fn owner_ids(meta: &read::Meta) -> (Option<rustix::fs::Uid>, Option<rustix::fs::Gid>) {
    (
        Some(rustix::fs::Uid::from_raw(meta.uid)),
        Some(rustix::fs::Gid::from_raw(meta.gid)),
    )
}

/// Keep the source's owner where the kernel allows it.
///
/// An unprivileged process may not give a file away, so this fails with
/// `EPERM` for anything the user does not already own — which, for a copy of
/// the user's own config tree, is nothing. Failing the copy over it would
/// mean `capture` could not run without root, which is the opposite of what
/// this tool is for. The mismatch is visible: `verify` records uid and gid,
/// and reports a difference in either.
fn keep_owner<Fd: rustix::fd::AsFd>(fd: Fd, meta: &read::Meta) {
    let (uid, gid) = owner_ids(meta);
    let _ = rustix::fs::fchown(fd, uid, gid);
}

fn times_of(st: &rustix::fs::Stat) -> rustix::fs::Timestamps {
    rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: st.st_atime as _,
            tv_nsec: st.st_atime_nsec as _,
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: st.st_mtime as _,
            tv_nsec: st.st_mtime_nsec as _,
        },
    }
}

#[allow(clippy::unnecessary_cast)]
fn size_of_file<Fd: rustix::fd::AsFd>(fd: Fd, what: &Path) -> Result<u64> {
    let st = rustix::fs::fstat(fd).map_err(|e| io(format!("stat {}", what.display()), e))?;
    Ok(st.st_size as u64)
}

/// The fallback: 64K at a time, so a large file costs bounded memory.
fn stream<SrcFd: rustix::fd::AsFd, DstFd: rustix::fd::AsFd>(
    src: SrcFd,
    dst: DstFd,
    from: &Path,
    to: &Path,
) -> Result<u64> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = rustix::io::read(&src, &mut buf[..])
            .map_err(|e| io(format!("reading {}", from.display()), e))?;
        if n == 0 {
            return Ok(total);
        }
        let mut written = 0;
        while written < n {
            written += rustix::io::write(&dst, &buf[written..n])
                .map_err(|e| io(format!("writing {}", to.display()), e))?;
        }
        total += n as u64;
    }
}
