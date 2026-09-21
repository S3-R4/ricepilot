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
