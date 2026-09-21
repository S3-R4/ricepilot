//! Read a tree and report what is in it that a person should decide about
//! before ricepilot copies or links it.
//!
//! This is what `capture` prints before it copies and what `init` asks about.
//! It answers three questions the five shapes do not, because the five shapes
//! are about a *destination* and these are about the contents of a tree:
//!
//! * **Which symlinks inside it are absolute?** The target machine has
//!   `zen/userChrome.css -> /home/…/caelestia/zen/userChrome.css`. An
//!   absolute link keeps pointing at the original tree after the copy, so a
//!   captured profile is not self-contained and a switch does not fully
//!   switch. ricepilot copies it faithfully — a link is copied as a link
//!   ([SAFETY.md](../docs/SAFETY.md) R3) — and says so, rather than
//!   rewriting a user's link behind their back.
//! * **Which files only their owner can read?** A mode-600 file in a config
//!   tree is usually a token or a key. Those are exactly the files an
//!   application rewrites and exactly the ones whose content should not be
//!   hashed into a manifest that gets read out on a broken machine, so they
//!   are *proposed* as `volatile` for the user to confirm.
//! * **Is there anything here that cannot be copied at all?** A socket or a
//!   fifo. Reported here so the user hears it while nothing has happened yet,
//!   rather than from the copier's refusal.
//!
//! Read-only, through [`crate::ops::read`], and it never follows a symlink.

use std::path::{Path, PathBuf};

use crate::ops::read::{self, Kind};
use crate::Result;

/// A symlink inside the surveyed tree whose target is absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsoluteLink {
    /// Relative to the surveyed root, with `/` separators.
    pub rel: String,
    pub target: PathBuf,
    /// Whether the target points back inside the tree it lives in. One that
    /// does is merely brittle; one that does not is a dependency on a
    /// directory the profile does not contain.
    pub inside: bool,
}

/// A regular file no one but its owner can read (`mode & 0o077 == 0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateFile {
    pub rel: String,
    pub mode: u32,
}

/// What one tree turned out to contain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Survey {
    pub root: PathBuf,
    pub dirs: usize,
    pub files: usize,
    pub links: usize,
    pub bytes: u64,
    /// Absolute symlinks, in path order.
    pub absolute_links: Vec<AbsoluteLink>,
    /// Owner-only-readable regular files, in path order.
    pub private: Vec<PrivateFile>,
    /// Paths that are neither a directory, a regular file nor a symlink, in
    /// path order. [`crate::ops::mutate::copy_tree`] refuses these.
    pub uncopyable: Vec<String>,
}

impl Survey {
    /// Every path the survey has something to say about. Used by `init` to
    /// decide whether to print the "nothing here needs a decision" line.
    pub fn is_quiet(&self) -> bool {
        self.absolute_links.is_empty() && self.private.is_empty() && self.uncopyable.is_empty()
    }
}

/// Walk `root`, without following a symlink anywhere.
pub fn survey(root: &Path) -> Result<Survey> {
    let mut s = Survey {
        root: root.to_path_buf(),
        ..Survey::default()
    };
    walk(root, root, Path::new(""), &mut s)?;
    s.absolute_links.sort_by(|a, b| a.rel.cmp(&b.rel));
    s.private.sort_by(|a, b| a.rel.cmp(&b.rel));
    s.uncopyable.sort();
    Ok(s)
}

fn walk(root: &Path, here: &Path, rel: &Path, s: &mut Survey) -> Result<()> {
    s.dirs += 1;
    for name in read::list_dir(here)? {
        let child_rel = rel.join(&name);
        let rel_str = child_rel.to_string_lossy().into_owned();
        let path = here.join(&name);
        let Some(meta) = read::lstat(&path)? else {
            // It went away between the listing and the stat. It is not there
            // now, and reporting it as present would be a claim about a
            // moment that has passed.
            continue;
        };
        match meta.kind {
            Kind::Dir => walk(root, &path, &child_rel, s)?,
            Kind::File => {
                s.files += 1;
                s.bytes += read::size_of(&path)?;
                if meta.mode & 0o077 == 0 {
                    s.private.push(PrivateFile {
                        rel: rel_str,
                        mode: meta.mode,
                    });
                }
            }
            Kind::Symlink => {
                s.links += 1;
                let target = read::readlink(&path)?;
                if target.is_absolute() {
                    s.absolute_links.push(AbsoluteLink {
                        inside: target.starts_with(root),
                        rel: rel_str,
                        target,
                    });
                }
            }
            Kind::Other => s.uncopyable.push(rel_str),
        }
    }
    Ok(())
}

/// Every entry of `dir` that is a symlink pointing lexically inside `root`.
///
/// This is how `init` finds the rice's existing directory links without
/// trusting a documented layout: `~/.config` on the target machine has 128
/// entries of which about twenty are rice, the documented VSCodium file links
/// do not exist because the installer raced, and the only way to know which
/// is which is to `lstat` all of them.
///
/// Lexical, never resolved — the same reason fact 2 of the ownership
/// predicate is lexical.
pub fn links_into(dir: &Path, root: &Path) -> Result<Vec<LiveLink>> {
    let mut out = Vec::new();
    for name in read::list_dir(dir)? {
        let path = dir.join(&name);
        let Some(meta) = read::lstat(&path)? else {
            continue;
        };
        if meta.kind != Kind::Symlink {
            continue;
        }
        let target = read::readlink(&path)?;
        if !target.starts_with(root) {
            continue;
        }
        let points_at_dir = matches!(read::lstat(&target)?, Some(m) if m.kind == Kind::Dir);
        out.push(LiveLink {
            dest: path,
            target,
            points_at_dir,
        });
    }
    out.sort_by(|a, b| a.dest.cmp(&b.dest));
    Ok(out)
}

/// One live symlink that already points into the rice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLink {
    pub dest: PathBuf,
    pub target: PathBuf,
    /// Whether the target is a real directory *right now*. A link that points
    /// at nothing, or at a file, is reported and not adopted: v1 manages
    /// directory links only, and a dangling one is a fact about the rice the
    /// user should hear rather than something to register as healthy.
    pub points_at_dir: bool,
}
