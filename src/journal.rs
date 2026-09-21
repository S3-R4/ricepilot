//! The write-ahead journal: what a switch is about to do, written and fsync'd
//! — the file *and* its containing directory — before the first exchange.
//!
//! What it records is deliberately **not** a step log. It is the before-and-
//! after of each destination: where the link pointed, where it will point, and
//! which sibling name the new link was staged at. Replay is state-driven:
//! `recover` reads the live link targets and decides per destination whether
//! that destination is old or new, and never re-runs a recorded step. A step
//! log would invite exactly the bug it looks like it prevents — re-running a
//! rename whose effect is already present, against a filesystem that has since
//! moved on.
//!
//! A finished journal is **renamed** to `done-<id>.toml`, not taken away. The
//! sequence of switches a machine has been through is evidence, and R2 has no
//! exception for evidence.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::observe::{Observed, Shape};
use crate::ops::mutate::{self, ExchangeMode};
use crate::ops::read;
use crate::plan::Op;
use crate::{Error, Result};

/// One destination's before-and-after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub dest: PathBuf,
    /// The sibling name the new link is staged at. `None` for a destination
    /// that was absent: `symlinkat` is already atomic, so there is nothing to
    /// stage and nothing to exchange (D11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged: Option<PathBuf>,
    /// Where the link pointed before. `None` means the destination did not
    /// exist, which is the "old" state for a created link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_target: Option<PathBuf>,
    pub new_target: PathBuf,
    /// Where the displaced old link lands inside the attic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attic_rel: Option<PathBuf>,
}

impl Entry {
    /// A destination whose old and new targets are equal would make the two
    /// states indistinguishable by reading the filesystem, which is the one
    /// thing state-driven recovery cannot survive. The planner never emits
    /// one — shape 1 with a matching target produces no op at all — and
    /// [`Journal::from_plan`] rejects it rather than trusting that.
    fn distinguishable(&self) -> bool {
        self.old_target.as_deref() != Some(self.new_target.as_path())
    }
}

/// The record of one in-flight switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    /// Also the attic directory's name, so the journal and the things it
    /// displaced are findable from each other.
    pub id: String,
    pub profile: String,
    pub attic: PathBuf,
    /// Which [`ExchangeMode`] the switch was started with. Recovery needs it:
    /// the fallback has intermediate states the atomic path cannot produce,
    /// and a machine whose kernel answered differently after a reboot should
    /// not be guessed at.
    pub exchange_mode: String,
    pub entries: Vec<Entry>,
    /// The plan as it was printed, for an auditor. Nothing reads this back —
    /// recovery uses `entries` — but "what did it say it would do" is the
    /// first question anyone asks of a machine that did not come back up.
    pub ops: Vec<String>,
}

impl Journal {
    /// Derive the record from the plan's ops and the observation it was made
    /// against.
    ///
    /// From the ops rather than from the manifest, so the journal describes
    /// the plan that was *printed* (R4) and cannot drift from it: if a future
    /// planner emits a different op sequence, this follows it or fails.
    pub fn from_plan(
        id: impl Into<String>,
        profile: impl Into<String>,
        attic: impl Into<PathBuf>,
        mode: ExchangeMode,
        ops: &[Op],
        observed: &[Observed],
    ) -> Result<Self> {
        let attic = attic.into();
        let mut staged_targets: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut entries: Vec<Entry> = Vec::new();

        for op in ops {
            match op {
                Op::CreateTempLink { link_path, target } => {
                    staged_targets.push((link_path.clone(), target.clone()));
                }
                Op::Exchange { dest, staged } => {
                    let new_target = staged_targets
                        .iter()
                        .find(|(p, _)| p == staged)
                        .map(|(_, t)| t.clone())
                        .ok_or_else(|| Error::Refused {
                            rule: "R4",
                            path: staged.clone(),
                            why: "plan exchanges a staged link it never created".into(),
                        })?;
                    entries.push(Entry {
                        dest: dest.clone(),
                        staged: Some(staged.clone()),
                        old_target: old_target_of(dest, observed)?,
                        new_target,
                        attic_rel: None,
                    });
                }
                Op::CreateLink { link_path, target } => entries.push(Entry {
                    dest: link_path.clone(),
                    staged: None,
                    old_target: None,
                    new_target: target.clone(),
                    attic_rel: None,
                }),
                Op::RenameToAttic { from, attic_rel } => {
                    let entry = entries
                        .iter_mut()
                        .find(|e| e.staged.as_deref() == Some(from.as_path()))
                        .ok_or_else(|| Error::Refused {
                            rule: "R4",
                            path: from.clone(),
                            why: "plan moves something to the attic that no exchange produced"
                                .into(),
                        })?;
                    entry.attic_rel = Some(attic_rel.clone());
                }
                Op::FsyncDir { .. } => {}
            }
        }

        for e in &entries {
            if !e.distinguishable() {
                return Err(Error::Refused {
                    rule: "R5",
                    path: e.dest.clone(),
                    why: "old and new link targets are identical, so recovery could not tell \
                          which side of the switch this destination is on"
                        .into(),
                });
            }
        }

        Ok(Journal {
            id: id.into(),
            profile: profile.into(),
            attic,
            exchange_mode: mode.as_str().to_string(),
            entries,
            ops: ops.iter().map(|o| o.to_string()).collect(),
        })
    }

    pub fn mode(&self) -> Result<ExchangeMode> {
        ExchangeMode::parse(&self.exchange_mode).ok_or_else(|| Error::Refused {
            rule: "R4",
            path: PathBuf::from(&self.id),
            why: format!(
                "journal records exchange_mode = {:?}, which this ricepilot does not know",
                self.exchange_mode
            ),
        })
    }
}

fn old_target_of(dest: &Path, observed: &[Observed]) -> Result<Option<PathBuf>> {
    let obs = observed
        .iter()
        .find(|o| o.dest == dest)
        .ok_or_else(|| Error::Refused {
            rule: "R4",
            path: dest.to_path_buf(),
            why: "plan acts on a destination phase A did not observe".into(),
        })?;
    match &obs.shape {
        Shape::OwnedLink { target } => Ok(Some(target.clone())),
        Shape::Absent => Ok(None),
        // Every other shape is a refusal in `plan`, so a plan carrying one is
        // a bug in the planner rather than a state to be journalled.
        other => Err(Error::Refused {
            rule: "R4",
            path: dest.to_path_buf(),
            why: format!(
                "plan acts on a destination observed as a {}, which is never switchable",
                other.as_str()
            ),
        }),
    }
}

/// Write the record and make it durable — the file and the directory holding
/// it — before the caller's first exchange. [`mutate::write_atomic`] does both
/// fsyncs; this function exists so the ordering requirement has a name that
/// appears at the call site.
pub fn write(path: &Path, journal: &Journal) -> Result<()> {
    let text = toml::to_string_pretty(journal).map_err(|e| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!("could not serialise the journal: {e}"),
    })?;
    mutate::write_atomic(path, text.as_bytes())
}

/// `Ok(None)` when there is no in-flight journal — the ordinary case, and the
/// one `recover` reports as "nothing to recover".
pub fn read_current(path: &Path) -> Result<Option<Journal>> {
    if read::lstat_or_absent(path)?.is_none() {
        return Ok(None);
    }
    let text = read::slurp(path)?;
    let journal: Journal = toml::from_str(&text).map_err(|e| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!("journal does not parse: {}", e.message()),
    })?;
    Ok(Some(journal))
}

/// Retire a finished journal by renaming it to `done-<id>.toml`.
///
/// Renaming rather than taking it away is R2 applied to ricepilot's own
/// bookkeeping: the sequence of switches a machine has been through is the
/// first thing anyone wants when it did not come back up.
pub fn mark_done(path: &Path, id: &str) -> Result<PathBuf> {
    let dir = path.parent().ok_or_else(|| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: "journal path has no parent directory".into(),
    })?;
    let done = dir.join(format!("done-{id}.toml"));
    mutate::rename_within(path, &done)?;
    mutate::fsync_dir(dir)?;
    Ok(done)
}

/// A UTC timestamp id, used for both the journal and the attic directory it
/// names. Readable because `gc` makes the operator type an attic directory's
/// name back before it will touch it, and `1758412800` is not a name anyone
/// can check they typed correctly.
pub fn timestamp_id(at: std::time::SystemTime) -> String {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's `civil_from_days`. Written out rather than pulled in as a
/// dependency: it is fifteen lines, it is exact, and a date library is a lot
/// of surface area to add to a tool whose only use for a date is naming a
/// directory.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
