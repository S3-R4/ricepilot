//! `state/ledger.toml`: which live paths ricepilot owns.
//!
//! Ownership is the conjunction of three independent facts (`docs/SAFETY.md`):
//! the path is `S_IFLNK` under `O_PATH|O_NOFOLLOW`; its `readlinkat` target is
//! lexically inside a registered profile root; and a ledger entry matches the
//! path, the target *and* the `(dev, ino)`. Anything else is unowned → refuse.
//!
//! This module supplies the third fact and nothing else. [`crate::observe`]
//! takes the rows as data, so classification stays testable without a state
//! directory and has no opinion about where the rows came from.
//!
//! The `(dev, ino)` is what makes a row evidence rather than a claim. A ledger
//! that recorded only the path and the target would still agree with itself
//! after an installer removed our link and put an identical-looking one of its
//! own in its place — and "identical-looking" is precisely the case where
//! ricepilot must not act.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::observe;
use crate::ops::{mutate, read};
use crate::{Error, Result};

/// One owned live path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub dest: PathBuf,
    /// The link's target *string*, exactly as `readlinkat` returned it. Not
    /// resolved: resolving it would mean following a symlink, and the whole
    /// predicate exists to avoid trusting one.
    pub target: PathBuf,
    /// Identity of the link inode itself, from a `lstat` that did not follow
    /// the final component.
    pub dev: u64,
    pub ino: u64,
    /// Which profile put it there. Not part of the ownership predicate —
    /// it is what `status` and the retirement decision read.
    pub profile: String,
}

impl Entry {
    /// Record a live path as owned, reading its identity off the disk.
    ///
    /// Refuses anything that is not a symlink: the ledger is a record of links
    /// ricepilot created, and a row describing a real directory would assert
    /// an ownership that fact 1 of the predicate can never confirm.
    pub fn of(dest: &Path, profile: &str) -> Result<Self> {
        let meta = read::lstat(dest)?.ok_or_else(|| Error::Refused {
            rule: "R4",
            path: dest.to_path_buf(),
            why: "cannot record an owned path that does not exist".into(),
        })?;
        if meta.kind != read::Kind::Symlink {
            return Err(Error::Refused {
                rule: "R4",
                path: dest.to_path_buf(),
                why: format!(
                    "cannot record a {} as an owned link; the ledger records links ricepilot \
                     created",
                    match meta.kind {
                        read::Kind::Dir => "real directory",
                        read::Kind::File => "regular file",
                        _ => "non-symlink",
                    }
                ),
            });
        }
        Ok(Entry {
            dest: dest.to_path_buf(),
            target: read::readlink(dest)?,
            dev: meta.dev,
            ino: meta.ino,
            profile: profile.to_string(),
        })
    }
}

/// The whole file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    #[serde(default, rename = "entry")]
    pub entries: Vec<Entry>,
}

impl Ledger {
    /// The rows [`crate::observe`] needs, with the bookkeeping fields dropped.
    pub fn observe_entries(&self) -> Vec<observe::LedgerEntry> {
        self.entries
            .iter()
            .map(|e| observe::LedgerEntry {
                dest: e.dest.clone(),
                target: e.target.clone(),
                dev: e.dev,
                ino: e.ino,
            })
            .collect()
    }

    /// The complete ownership oracle: the ledger's rows plus the roots of
    /// every registered profile.
    ///
    /// Every registered root counts, not only the one being switched to: a
    /// link pointing into the profile we are switching *away* from is still
    /// one of ours, and reading it as foreign would refuse the very switch it
    /// is the starting point of.
    pub fn ownership(&self, profile_roots: Vec<PathBuf>) -> observe::Ownership {
        observe::Ownership {
            profile_roots,
            entries: self.observe_entries(),
        }
    }

    /// Every destination ricepilot currently owns, in file order.
    pub fn owned_dests(&self) -> Vec<PathBuf> {
        self.entries.iter().map(|e| e.dest.clone()).collect()
    }

    /// Replace the rows for `dests` with freshly observed ones, leaving every
    /// other row alone.
    ///
    /// Phase C's POST record. It re-`lstat`s rather than trusting what the
    /// switch believes it just did (`SAFETY.md` R7): the ledger's job is to
    /// say what is true of the filesystem, and the one moment it could be
    /// wrong is the moment right after something changed it.
    pub fn record(&mut self, dests: &[PathBuf], profile: &str) -> Result<()> {
        for dest in dests {
            let entry = Entry::of(dest, profile)?;
            match self.entries.iter_mut().find(|e| &e.dest == dest) {
                Some(slot) => *slot = entry,
                None => self.entries.push(entry),
            }
        }
        self.entries.sort_by(|a, b| a.dest.cmp(&b.dest));
        Ok(())
    }

    /// Drop the rows for paths ricepilot no longer owns — because they were
    /// displaced into the attic, not because anything was removed.
    ///
    /// A row for a path that is no longer a link would be an ownership claim
    /// nothing on the disk supports, which is the one thing a ledger must
    /// never contain.
    pub fn forget(&mut self, dests: &[PathBuf]) {
        self.entries.retain(|e| !dests.contains(&e.dest));
    }
}

/// Read `state/ledger.toml`. An absent file is an empty ledger, which is the
/// correct description of a machine where `init` has not run: ricepilot owns
/// nothing, so every live path is unowned and every switch onto one is
/// refused.
pub fn load(path: &Path) -> Result<Ledger> {
    if read::lstat_or_absent(path)?.is_none() {
        return Ok(Ledger::default());
    }
    let text = read::slurp(path)?;
    toml::from_str(&text).map_err(|e| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!(
            "the ledger does not parse, so ricepilot cannot tell which live paths it owns: {}",
            e.message()
        ),
    })
}

/// Write `state/ledger.toml` atomically, as a real file.
pub fn save(path: &Path, ledger: &Ledger) -> Result<()> {
    let text = toml::to_string_pretty(ledger).map_err(|e| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!("could not serialise the ledger: {e}"),
    })?;
    mutate::write_atomic(path, text.as_bytes())
}
