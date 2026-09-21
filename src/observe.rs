//! Read reality. Never trusts a documented layout: every live path is
//! `lstat`ed through a dirfd. All syscalls are delegated to [`crate::ops`].

use std::path::{Path, PathBuf};

use crate::ops::read::{self, Kind};
use crate::{Error, Result};

/// The five shapes a destination path can have. This enumeration is total:
/// the switch decision table in `docs/DESIGN.md` §5 has a row for each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// A symlink whose target is lexically inside a registered profile root
    /// *and* whose ledger entry matches path, target and `(dev, ino)`.
    OwnedLink {
        target: PathBuf,
    },
    /// A symlink we did not create, or one whose ledger entry disagrees.
    /// Includes dangling links.
    ForeignLink {
        target: PathBuf,
        dangling: bool,
    },
    RealDir,
    RealFile,
    Absent,
}

impl Shape {
    /// The word used in output and in refusal messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            Shape::OwnedLink { .. } => "owned link",
            Shape::ForeignLink { dangling: true, .. } => "dangling foreign link",
            Shape::ForeignLink { .. } => "foreign link",
            Shape::RealDir => "real directory",
            Shape::RealFile => "real file",
            Shape::Absent => "absent",
        }
    }
}

/// One observed destination, with the facts the planner needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub dest: PathBuf,
    pub shape: Shape,
    /// `st_dev` of the destination's parent directory. A rename that would
    /// cross `st_dev` fails with `EXDEV` and is refused in pre-flight.
    pub parent_dev: u64,
    /// `statfs` `f_type` of the parent, recorded for the mountpoint check.
    pub parent_fs_type: i64,
    pub is_mountpoint: bool,
}

/// One row of `state/ledger.toml`, as far as observation is concerned.
///
/// The ledger file itself is [`crate::ledger`]'s business (M3); observation
/// takes the rows as data so that classification can be tested without a
/// state directory, and so `observe` has no opinion about where they came
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub dest: PathBuf,
    pub target: PathBuf,
    pub dev: u64,
    pub ino: u64,
}

/// The ownership oracle: the two independent sources of truth the predicate
/// in `SAFETY.md` consults besides the `lstat` itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ownership {
    /// Roots of every *registered* profile. A link pointing outside all of
    /// them is foreign no matter what the ledger says.
    pub profile_roots: Vec<PathBuf>,
    pub entries: Vec<LedgerEntry>,
}

impl Ownership {
    /// All three facts, conjoined. Any one of them failing makes the path
    /// unowned, and an unowned path is refused rather than acted on.
    fn owns(&self, dest: &Path, target: &Path, dev: u64, ino: u64) -> bool {
        // Fact 2: the target is *lexically* inside a registered root. Lexical
        // on purpose — resolving it would mean following symlinks.
        let inside = self
            .profile_roots
            .iter()
            .any(|root| target.starts_with(root));
        // Fact 3: a ledger row agrees about the path, the target string and
        // the identity of the link inode.
        let recorded = self
            .entries
            .iter()
            .any(|e| e.dest == dest && e.target == target && e.dev == dev && e.ino == ino);
        inside && recorded
    }
}

/// Classify one destination. Read-only.
pub fn observe_one(dest: &Path, own: &Ownership) -> Result<Observed> {
    let parent = dest.parent().ok_or_else(|| Error::Refused {
        rule: "R1",
        path: dest.to_path_buf(),
        why: "destination has no parent directory".into(),
    })?;
    let (parent_dev, parent_fs_type) = read::dev_and_fs_type(parent)?;

    let meta = read::lstat(dest)?;
    let (shape, is_mountpoint) = match meta {
        None => (Shape::Absent, false),
        Some(m) => match m.kind {
            // Fact 1 of the ownership predicate: it is a symlink.
            Kind::Symlink => {
                let target = read::readlink(dest)?;
                let shape = if own.owns(dest, &target, m.dev, m.ino) {
                    Shape::OwnedLink { target }
                } else {
                    Shape::ForeignLink {
                        dangling: !read::resolves(dest)?,
                        target,
                    }
                };
                (shape, false)
            }
            // A directory whose `st_dev` differs from its parent's is a
            // mount point: renaming it would cross a filesystem boundary.
            Kind::Dir => (Shape::RealDir, m.dev != parent_dev),
            // Anything that is neither a directory nor a symlink is a
            // non-directory the switch will decline to touch. Sockets and
            // FIFOs land here with regular files; the outcome is the same
            // refusal, and inventing a sixth shape for them would add a row
            // to the decision table that says exactly what row four says.
            Kind::File | Kind::Other => (Shape::RealFile, false),
        },
    };

    Ok(Observed {
        dest: dest.to_path_buf(),
        shape,
        parent_dev,
        parent_fs_type,
        is_mountpoint,
    })
}

/// Observe every destination named by the target manifest.
///
/// This is the whole of phase A's observation: it completes before anything
/// is decided, so the planner sees one consistent picture rather than
/// re-reading the filesystem between decisions (`SAFETY.md` R4).
pub fn observe(dests: &[PathBuf], own: &Ownership) -> Result<Vec<Observed>> {
    dests.iter().map(|d| observe_one(d, own)).collect()
}
