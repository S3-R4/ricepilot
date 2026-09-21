//! `state/generations/NNNN.toml` plus a `current` pointer.
//!
//! One generation records the complete link topology of every destination
//! ricepilot manages, as it stood *after* a switch: for each destination,
//! either the target its link points at, or the fact that nothing is there.
//! Recording the absences matters as much as recording the links — it is what
//! lets `rollback` know that a destination the previous generation did not
//! have is one it must displace rather than leave behind (D35).
//!
//! `current` is a **real file** written with
//! [`crate::ops::mutate::write_atomic`], never a symlink. The whole point of
//! it is to survive a switch that went wrong, and a switch that went wrong is
//! a switch that did something unintended to a symlink.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ops::{mutate, read};
use crate::plan::Target;
use crate::{Error, Result};

/// One destination's state at the end of a switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenEntry {
    pub dest: PathBuf,
    /// `None` means nothing was at this destination — which is a state worth
    /// recording, not an absence of information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
}

/// One switch's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub id: u32,
    /// The profile this generation is of. Generation `0000` — the state
    /// before ricepilot ever switched anything — uses [`PRE_EXISTING`].
    pub profile: String,
    pub created: String,
    #[serde(default, rename = "entry")]
    pub entries: Vec<GenEntry>,
}

/// The `profile` of generation `0000`: the topology as ricepilot found it,
/// belonging to no profile ricepilot put there.
pub const PRE_EXISTING: &str = "(the state before the first switch)";

impl Generation {
    /// The destinations this generation has a link at, as planner targets.
    pub fn targets(&self) -> Vec<Target> {
        self.entries
            .iter()
            .filter_map(|e| {
                e.target.as_ref().map(|t| Target {
                    dest: e.dest.clone(),
                    src: t.clone(),
                })
            })
            .collect()
    }

    /// Every destination named, whether or not it holds a link.
    pub fn dests(&self) -> Vec<PathBuf> {
        self.entries.iter().map(|e| e.dest.clone()).collect()
    }

    /// The destinations this generation records as holding nothing.
    pub fn absent(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|e| e.target.is_none())
            .map(|e| e.dest.clone())
            .collect()
    }

    /// Read the live topology of `dests` off the disk.
    ///
    /// Observed, never assumed (`SAFETY.md` R7): a generation that recorded
    /// what a switch *meant* to do would be a record of an intention, and the
    /// thing `rollback` and `rescue.sh` need is a record of a fact.
    pub fn observe(
        id: u32,
        profile: impl Into<String>,
        created: impl Into<String>,
        dests: &[PathBuf],
    ) -> Result<Self> {
        let mut entries = Vec::new();
        for dest in dests {
            let target = match mutate::link_target(dest)? {
                Some(Some(t)) => Some(t),
                // Nothing there, or something there that is not a link. Both
                // are "this generation has no link at this path"; the second
                // is also a thing `switch` would have refused, so it cannot
                // arise from a switch ricepilot completed.
                _ => None,
            };
            entries.push(GenEntry {
                dest: dest.clone(),
                target,
            });
        }
        entries.sort_by(|a, b| a.dest.cmp(&b.dest));
        Ok(Generation {
            id,
            profile: profile.into(),
            created: created.into(),
            entries,
        })
    }
}

pub fn dir(state: &Path) -> PathBuf {
    state.join("generations")
}

/// `NNNN.toml`, zero-padded so the directory listing is in switch order.
pub fn path(state: &Path, id: u32) -> PathBuf {
    dir(state).join(format!("{id:04}.toml"))
}

/// The pointer at the active generation. A real file (see the module note).
pub fn current_path(state: &Path) -> PathBuf {
    dir(state).join("current")
}

/// `Ok(None)` on a machine that has never switched.
pub fn current(state: &Path) -> Result<Option<u32>> {
    let p = current_path(state);
    if read::lstat_or_absent(&p)?.is_none() {
        return Ok(None);
    }
    let meta = read::lstat(&p)?.ok_or_else(|| Error::Refused {
        rule: "R4",
        path: p.clone(),
        why: "the current-generation pointer went away while it was being read".into(),
    })?;
    // A symlink here would mean the one file meant to survive a bad switch was
    // itself the kind of thing a bad switch damages.
    if meta.kind != read::Kind::File {
        return Err(Error::Refused {
            rule: "R4",
            path: p,
            why: "the current-generation pointer is not a regular file. ricepilot writes it as \
                  one precisely so that a switch which went wrong cannot have taken it with them"
                .into(),
        });
    }
    let text = read::slurp(&p)?;
    text.trim()
        .parse::<u32>()
        .map(Some)
        .map_err(|_| Error::Refused {
            rule: "R4",
            path: current_path(state),
            why: format!(
                "the current-generation pointer reads {:?}, which is not a generation number",
                text.trim()
            ),
        })
}

/// Point `current` at `id`. Atomic: the pointer names a generation that is
/// already fully written, or it names the previous one, and never anything in
/// between.
pub fn set_current(state: &Path, id: u32) -> Result<()> {
    mutate::write_atomic(&current_path(state), format!("{id:04}\n").as_bytes())
}

pub fn save(state: &Path, g: &Generation) -> Result<()> {
    let p = path(state, g.id);
    let text = toml::to_string_pretty(g).map_err(|e| Error::Refused {
        rule: "R4",
        path: p.clone(),
        why: format!("could not serialise generation {}: {e}", g.id),
    })?;
    mutate::write_atomic(&p, text.as_bytes())
}

pub fn load(state: &Path, id: u32) -> Result<Generation> {
    let p = path(state, id);
    if read::lstat_or_absent(&p)?.is_none() {
        return Err(Error::Refused {
            rule: "R4",
            path: p,
            why: format!("there is no generation {id:04} to read"),
        });
    }
    let text = read::slurp(&p)?;
    toml::from_str(&text).map_err(|e| Error::Refused {
        rule: "R4",
        path: p,
        why: format!("generation {id:04} does not parse: {}", e.message()),
    })
}

/// The id the next switch will write.
pub fn next_id(state: &Path) -> Result<u32> {
    Ok(match current(state)? {
        Some(n) => n + 1,
        None => 0,
    })
}
