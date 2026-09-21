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

/// A destination this switch stops owning: it is displaced into the attic and
/// nothing is put back in its place (D36).
///
/// Recorded separately from [`Entry`] because it has no "new target". Its two
/// states are "the link is still here" and "the link is in the attic", and
/// recovery tells them apart the same way it tells any other pair apart — by
/// reading what is at the path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retire {
    pub dest: PathBuf,
    pub old_target: PathBuf,
    pub attic_rel: PathBuf,
}

/// A **real directory** being replaced by a link to a copy of itself: the
/// one thing `adopt` does that no other command does (D46).
///
/// [`Entry`] cannot represent this. It models a destination by its link
/// target, and a real directory has none — `slot_of` answers
/// `Foreign("not a symlink")` and `side_of` turns that into a refusal, so an
/// adopt journalled as an `Entry` would be unrecoverable by construction:
/// `recover` would refuse the very state `adopt` exists to pass through.
///
/// So the pre-state is identified by `(dev, ino)` instead. That satisfies
/// D24 — the two states are still told apart by reading the filesystem — and
/// it is *stronger* evidence than a target string: a directory an installer
/// removed and recreated between the journal and the crash has a different
/// inode, and recovery then refuses rather than moving someone else's
/// directory into the attic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adopt {
    pub dest: PathBuf,
    /// Identity of the real directory, as the pre-flight observed it.
    pub dir_dev: u64,
    pub dir_ino: u64,
    /// The sibling name the new link is staged at, so the exchange is a
    /// same-directory rename.
    pub staged: PathBuf,
    /// The copy inside the profile that the new link points at.
    pub new_target: PathBuf,
    /// Where the displaced real directory lands inside the attic. It is
    /// **moved**, never removed: this is the user's actual configuration.
    pub attic_rel: PathBuf,
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
    /// Destinations this switch stops owning. `#[serde(default)]` so a journal
    /// written before retirement existed still parses — an in-flight switch
    /// from an older ricepilot must stay recoverable by a newer one.
    #[serde(default)]
    pub retire: Vec<Retire>,
    /// Real directories this operation is turning into links. `adopt` writes
    /// exactly one; `switch` never writes any. `#[serde(default)]` for the
    /// same reason `retire` has one: a journal written by an older ricepilot
    /// must stay recoverable by a newer one.
    #[serde(default)]
    pub adopt: Vec<Adopt>,
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
        let mut retire: Vec<Retire> = Vec::new();

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
                    match entries
                        .iter_mut()
                        .find(|e| e.staged.as_deref() == Some(from.as_path()))
                    {
                        // The displaced old link, sitting at the staging name
                        // the exchange left it at.
                        Some(entry) => entry.attic_rel = Some(attic_rel.clone()),
                        // Otherwise this is a retirement: a destination the
                        // target state no longer includes, moved out of the
                        // way with nothing put back.
                        None => retire.push(Retire {
                            dest: from.clone(),
                            old_target: retired_target_of(from, observed)?,
                            attic_rel: attic_rel.clone(),
                        }),
                    }
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
            retire,
            adopt: Vec::new(),
            ops: ops.iter().map(|o| o.to_string()).collect(),
        })
    }

    /// The record of one `adopt`: no [`Entry`] and no [`Retire`], one
    /// [`Adopt`].
    ///
    /// Built directly rather than derived from the ops, unlike
    /// [`Journal::from_plan`]. The ops are still what gets executed and still
    /// what is printed, but the fact recovery turns on — the identity of the
    /// directory being displaced — is not in any op, and deriving the record
    /// from the plan would mean inventing an op to carry it.
    pub fn for_adopt(
        id: impl Into<String>,
        profile: impl Into<String>,
        attic: impl Into<PathBuf>,
        mode: ExchangeMode,
        adopt: Adopt,
        ops: &[Op],
    ) -> Self {
        Journal {
            id: id.into(),
            profile: profile.into(),
            attic: attic.into(),
            exchange_mode: mode.as_str().to_string(),
            entries: Vec::new(),
            retire: Vec::new(),
            adopt: vec![adopt],
            ops: ops.iter().map(|o| o.to_string()).collect(),
        }
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

/// The link a retirement displaces. It must be an owned link: `plan` refuses
/// to retire anything else, so a plan carrying one is a planner bug rather
/// than a state to be journalled.
fn retired_target_of(dest: &Path, observed: &[Observed]) -> Result<PathBuf> {
    let obs = observed
        .iter()
        .find(|o| o.dest == dest)
        .ok_or_else(|| Error::Refused {
            rule: "R4",
            path: dest.to_path_buf(),
            why: "plan retires a destination phase A did not observe".into(),
        })?;
    match &obs.shape {
        Shape::OwnedLink { target } => Ok(target.clone()),
        other => Err(Error::Refused {
            rule: "R4",
            path: dest.to_path_buf(),
            why: format!(
                "plan retires a destination observed as a {}, which ricepilot does not own",
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

/// A [`timestamp_id`] that is not already taken, in either the journal
/// directory or the attic.
///
/// Two switches in the same second would otherwise share an id, and the second
/// one's `done-<id>.toml` would collide with the first's — `rename_within`
/// refuses to replace an existing file (correctly), so the switch would fail
/// *after* having applied every one of its effects. A test found this by
/// rolling back immediately after a switch, which is exactly what a user does
/// when a rice turns out to be wrong.
///
/// The suffix keeps the journal and the attic directory named after each
/// other, which is what makes them findable from one another (D40).
pub fn unique_id(state: &Path, base: &str) -> Result<String> {
    let taken = |id: &str| -> Result<bool> {
        let journal = state.join("journal").join(format!("done-{id}.toml"));
        let attic = state.join("attic").join(id);
        Ok(read::lstat_or_absent(&journal)?.is_some() || read::lstat_or_absent(&attic)?.is_some())
    };
    if !taken(base)? {
        return Ok(base.to_string());
    }
    for n in 1..1000u32 {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(Error::Refused {
        rule: "R4",
        path: state.join("journal"),
        why: format!("no free switch id beside {base} after 1000 attempts"),
    })
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

// ---------------------------------------------------------------------------
// Recovery
// ---------------------------------------------------------------------------

/// Which side of the switch one destination is on, decided by reading the
/// filesystem rather than by consulting a step counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The link still points where it did before the switch — or, for a
    /// destination that was absent, still does not exist.
    Old,
    /// The link points at the new profile.
    New,
    /// Neither: the fallback exchange's window, in which the destination does
    /// not exist and the two links are parked at sibling names.
    InFlight,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Old => "old",
            Side::New => "new",
            Side::InFlight => "mid-exchange",
        }
    }
}

/// Which way the whole switch will be driven. There is no third option: R5
/// forbids leaving it mixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// At least one destination is already new, so finishing is the only move
    /// that does not undo something that has already taken effect.
    Forward,
    /// Nothing was exchanged before the crash. The switch never started, and
    /// the staged links go to the attic.
    Backward,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Forward => "forward",
            Direction::Backward => "backward",
        }
    }
}

/// A step recovery would take. A closed set, like [`Op`], and executed only by
/// [`mutate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    StageLink { path: PathBuf, target: PathBuf },
    CreateLink { dest: PathBuf, target: PathBuf },
    Exchange { dest: PathBuf, staged: PathBuf },
    Rename { from: PathBuf, to: PathBuf },
    ToAttic { from: PathBuf, rel: PathBuf },
    FsyncDir { dir: PathBuf },
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::StageLink { path, target } => {
                write!(f, "stage link   {} -> {}", path.display(), target.display())
            }
            Action::CreateLink { dest, target } => {
                write!(f, "create link  {} -> {}", dest.display(), target.display())
            }
            Action::Exchange { dest, staged } => {
                write!(
                    f,
                    "exchange     {} <-> {}",
                    dest.display(),
                    staged.display()
                )
            }
            Action::Rename { from, to } => {
                write!(f, "rename       {} -> {}", from.display(), to.display())
            }
            Action::ToAttic { from, rel } => {
                write!(
                    f,
                    "to attic     {} -> <attic>/{}",
                    from.display(),
                    rel.display()
                )
            }
            Action::FsyncDir { dir } => write!(f, "fsync dir    {}", dir.display()),
        }
    }
}

/// One destination, as recovery found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub dest: PathBuf,
    pub side: Side,
}

/// The complete recovery decision. Produced without mutating anything, so
/// `recover` can print it and stop (R4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovery {
    pub id: String,
    pub profile: String,
    /// Whether the interrupted operation was an `adopt`. It changes nothing
    /// about what recovery does and everything about what it says: an adopt
    /// displaces a real directory, and telling someone their *switch* was
    /// interrupted would send them looking for one that never happened.
    pub adopting: bool,
    pub direction: Direction,
    pub statuses: Vec<Status>,
    pub actions: Vec<Action>,
}

/// What a slot — a destination or one of its two staging names — holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Absent,
    /// A symlink pointing at the pre-switch target.
    Old,
    /// A symlink pointing at the post-switch target.
    New,
    /// Anything else. Never acted on.
    Foreign(String),
}

fn slot_of(path: &Path, old: Option<&Path>, new: &Path) -> Result<Slot> {
    Ok(match mutate::link_target(path)? {
        None => Slot::Absent,
        Some(None) => Slot::Foreign("not a symlink".into()),
        Some(Some(t)) if Some(t.as_path()) == old => Slot::Old,
        Some(Some(t)) if t == new => Slot::New,
        Some(Some(t)) => Slot::Foreign(format!("a symlink to {}", t.display())),
    })
}

fn foreign(path: &Path, detail: &str) -> Error {
    Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!(
            "an interrupted switch left this destination as {detail}, which is neither the \
             profile it was on nor the one it was moving to. ricepilot will not guess what \
             happened to it"
        ),
    }
}

/// The three names one destination's links can be sitting at mid-switch.
struct Slots {
    dest: Slot,
    staged: Slot,
    swap: Slot,
}

fn read_slots(e: &Entry) -> Result<Slots> {
    let old = e.old_target.as_deref();
    let dest = slot_of(&e.dest, old, &e.new_target)?;
    let (staged, swap) = match &e.staged {
        Some(s) => (
            slot_of(s, old, &e.new_target)?,
            slot_of(&mutate::fallback_slot(s), old, &e.new_target)?,
        ),
        None => (Slot::Absent, Slot::Absent),
    };
    Ok(Slots { dest, staged, swap })
}

fn side_of(e: &Entry, s: &Slots) -> Result<Side> {
    match &s.dest {
        Slot::New => Ok(Side::New),
        Slot::Old => Ok(Side::Old),
        // A destination that was absent before the switch is still "old" while
        // it is absent, and there is nothing mid-exchange about it: `CreateLink`
        // is a single atomic `symlinkat` with no window (D11).
        Slot::Absent if e.old_target.is_none() => Ok(Side::Old),
        // Absent with an old target recorded means the fallback's window.
        Slot::Absent => Ok(Side::InFlight),
        Slot::Foreign(detail) => Err(foreign(&e.dest, detail)),
    }
}

/// Which side of the switch a retirement is on. "New" is the destination being
/// empty: that is what this switch was moving it towards.
fn retire_side(r: &Retire) -> Result<Side> {
    match mutate::link_target(&r.dest)? {
        None => Ok(Side::New),
        Some(Some(t)) if t == r.old_target => Ok(Side::Old),
        Some(Some(t)) => Err(foreign(&r.dest, &format!("a symlink to {}", t.display()))),
        Some(None) => Err(foreign(&r.dest, "not a symlink")),
    }
}

/// What one slot holds, as far as an [`Adopt`] is concerned.
///
/// The parallel of [`Slot`], and separate from it for the reason D46 gives:
/// the pre-state here is a directory identified by its inode, not a link
/// identified by its target, and one enum covering both would make every
/// `Slot` match arm answer a question it does not have the facts for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirSlot {
    Absent,
    /// The very directory the journal recorded, by `(dev, ino)`.
    TheDir,
    /// The new link, pointing at the copy inside the profile.
    NewLink,
    /// Anything else. Never acted on.
    Foreign(String),
}

fn dir_slot_of(path: &Path, a: &Adopt) -> Result<DirSlot> {
    Ok(match read::lstat(path)? {
        None => DirSlot::Absent,
        Some(m) if m.kind == read::Kind::Dir => {
            if (m.dev, m.ino) == (a.dir_dev, a.dir_ino) {
                DirSlot::TheDir
            } else {
                // Same path, different inode: something replaced the
                // directory between the journal and now. Moving *this* one
                // into the attic would displace something ricepilot never
                // looked at.
                DirSlot::Foreign(format!(
                    "a different directory (inode {}, not the inode {} that was confirmed)",
                    m.ino, a.dir_ino
                ))
            }
        }
        Some(m) if m.kind == read::Kind::Symlink => {
            let t = read::readlink(path)?;
            if t == a.new_target {
                DirSlot::NewLink
            } else {
                DirSlot::Foreign(format!("a symlink to {}", t.display()))
            }
        }
        Some(_) => DirSlot::Foreign("neither a directory nor a symlink".into()),
    })
}

/// The three names one adopted destination can be sitting at mid-operation.
struct DirSlots {
    dest: DirSlot,
    staged: DirSlot,
    swap: DirSlot,
}

fn slots_of_adopt(a: &Adopt) -> Result<DirSlots> {
    Ok(DirSlots {
        dest: dir_slot_of(&a.dest, a)?,
        staged: dir_slot_of(&a.staged, a)?,
        swap: dir_slot_of(&mutate::fallback_slot(&a.staged), a)?,
    })
}

/// Which side of the adopt this destination is on.
/// An adopt's own version of [`foreign`]. The switch wording — "neither the
/// profile it was on nor the one it was moving to" — describes a pair of
/// link targets, and an adopt's two states are a directory and a link.
fn foreign_adopt(path: &Path, detail: &str) -> Error {
    Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!(
            "an interrupted adopt left this path as {detail}, which is neither the directory \
             that was adopted nor the link it was becoming. something else changed it while \
             ricepilot was not looking, and ricepilot will not guess what"
        ),
    }
}

fn adopt_side(a: &Adopt, s: &DirSlots) -> Result<Side> {
    match &s.dest {
        DirSlot::NewLink => Ok(Side::New),
        DirSlot::TheDir => Ok(Side::Old),
        // The fallback exchange's window: the directory is parked at the
        // staging name and the destination itself is empty. Nothing else can
        // make an adopted destination absent — the atomic path never does,
        // and ricepilot has no delete.
        DirSlot::Absent => Ok(Side::InFlight),
        DirSlot::Foreign(detail) => Err(foreign_adopt(&a.dest, detail)),
    }
}

/// What to do about one adopted destination, given where its three slots are.
///
/// Every forward arm ends with the displaced **real directory** going to the
/// attic. That is the user's actual configuration, so it is moved and never
/// removed (R2), and the report says where it went.
fn actions_for_adopt(a: &Adopt, s: &DirSlots, dir: Direction) -> Result<Vec<Action>> {
    let swap = mutate::fallback_slot(&a.staged);
    let to_attic = Action::ToAttic {
        from: a.staged.clone(),
        rel: a.attic_rel.clone(),
    };
    Ok(match dir {
        Direction::Forward => match (&s.dest, &s.staged, &s.swap) {
            // Done but for the displaced directory, which the exchange left
            // sitting at the staging name.
            (DirSlot::NewLink, DirSlot::TheDir, DirSlot::Absent) => vec![to_attic],
            // Fully done.
            (DirSlot::NewLink, DirSlot::Absent, DirSlot::Absent) => Vec::new(),
            // Still a real directory and nothing staged: the crash beat the
            // staging step.
            (DirSlot::TheDir, DirSlot::Absent, DirSlot::Absent) => vec![
                Action::StageLink {
                    path: a.staged.clone(),
                    target: a.new_target.clone(),
                },
                Action::Exchange {
                    dest: a.dest.clone(),
                    staged: a.staged.clone(),
                },
                to_attic,
            ],
            // Staged, not yet exchanged.
            (DirSlot::TheDir, DirSlot::NewLink, DirSlot::Absent) => vec![
                Action::Exchange {
                    dest: a.dest.clone(),
                    staged: a.staged.clone(),
                },
                to_attic,
            ],
            // The fallback after its first rename: the link is at the scratch
            // name and the directory is still live. Resume from step two.
            (DirSlot::TheDir, DirSlot::Absent, DirSlot::NewLink) => vec![
                Action::Rename {
                    from: a.dest.clone(),
                    to: a.staged.clone(),
                },
                Action::Rename {
                    from: swap,
                    to: a.dest.clone(),
                },
                to_attic,
            ],
            // The fallback's window: the directory is parked at the staging
            // name and the destination is empty. Finish step three.
            (DirSlot::Absent, DirSlot::TheDir, DirSlot::NewLink) => vec![
                Action::Rename {
                    from: swap,
                    to: a.dest.clone(),
                },
                to_attic,
            ],
            (dest, st, sw) => return Err(unexpected_adopt(a, dest, st, sw)),
        },

        // Backward: no exchange took effect anywhere, so the user's directory
        // is untouched and only the staged link has to go. The **copy inside
        // the profile stays** — it is a copy, it harms nothing, and removing
        // it is a removal (R2). The report names it.
        Direction::Backward => match (&s.dest, &s.staged, &s.swap) {
            (DirSlot::TheDir, DirSlot::Absent, DirSlot::Absent) => Vec::new(),
            (DirSlot::TheDir, DirSlot::NewLink, DirSlot::Absent) => vec![Action::ToAttic {
                from: a.staged.clone(),
                rel: staged_link_rel(a),
            }],
            (DirSlot::TheDir, DirSlot::Absent, DirSlot::NewLink) => vec![Action::ToAttic {
                from: swap,
                rel: staged_link_rel(a),
            }],
            (dest, st, sw) => return Err(unexpected_adopt(a, dest, st, sw)),
        },
    })
}

/// Where an abandoned staged *link* lands, kept distinct from where the
/// displaced *directory* would have landed so the attic says which is which.
fn staged_link_rel(a: &Adopt) -> PathBuf {
    let name = a
        .attic_rel
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    match a.attic_rel.parent() {
        Some(p) => p.join(format!("{name}.staged")),
        None => PathBuf::from(format!("{name}.staged")),
    }
}

fn unexpected_adopt(a: &Adopt, dest: &DirSlot, staged: &DirSlot, swap: &DirSlot) -> Error {
    Error::Refused {
        rule: "R5",
        path: a.dest.clone(),
        why: format!(
            "an interrupted adopt left a combination ricepilot cannot account for \
             (destination: {dest:?}, staged: {staged:?}, scratch: {swap:?}). It will not \
             guess; nothing has been changed"
        ),
    }
}

/// Where a staged link that is being abandoned lands in the attic. Distinct
/// from the displaced old link's slot so the attic says which is which.
fn staged_attic_rel(e: &Entry) -> PathBuf {
    let base = e
        .attic_rel
        .clone()
        .unwrap_or_else(|| e.dest.strip_prefix("/").unwrap_or(&e.dest).to_path_buf());
    let name = base
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    match base.parent() {
        Some(p) => p.join(format!("{name}.staged")),
        None => PathBuf::from(format!("{name}.staged")),
    }
}

/// Decide what to do about one destination, given where it actually is.
///
/// Nothing here consults a step number. Each arm is chosen by what the three
/// slots hold, which is why replaying a journal twice is the same as replaying
/// it once: the second run finds the destination already on the side it was
/// driven to and emits no action for it.
fn actions_for(e: &Entry, s: &Slots, dir: Direction) -> Result<Vec<Action>> {
    let mut out = Vec::new();
    let staged = e.staged.clone();

    match dir {
        Direction::Forward => match (&s.dest, &s.staged, &s.swap) {
            // Already new. All that can remain is the displaced old link,
            // sitting at one of the staging names.
            (Slot::New, _, _) => {
                if let Some(st) = &staged {
                    if s.staged == Slot::Old {
                        out.push(Action::ToAttic {
                            from: st.clone(),
                            rel: e.attic_rel.clone().unwrap_or_else(|| staged_attic_rel(e)),
                        });
                    }
                    if s.swap == Slot::Old {
                        out.push(Action::ToAttic {
                            from: mutate::fallback_slot(st),
                            rel: e.attic_rel.clone().unwrap_or_else(|| staged_attic_rel(e)),
                        });
                    }
                }
            }

            // The destination was absent before the switch and still is: the
            // single `symlinkat` never ran.
            (Slot::Absent, _, _) if e.old_target.is_none() => out.push(Action::CreateLink {
                dest: e.dest.clone(),
                target: e.new_target.clone(),
            }),

            // The fallback's window: the old link is parked at `staged`, the
            // new one at `swap`. Finish step three, then displace the old one.
            (Slot::Absent, Slot::Old, Slot::New) => {
                let st = staged.clone().expect("window implies a staged name");
                out.push(Action::Rename {
                    from: mutate::fallback_slot(&st),
                    to: e.dest.clone(),
                });
                out.push(displace(e, st));
            }

            // Still old, with the new link already staged: the ordinary
            // pre-exchange state. After the exchange the staged name holds the
            // old link, which is what goes to the attic.
            (Slot::Old, Slot::New, Slot::Absent) => {
                let st = staged.clone().expect("a staged slot implies a staged name");
                out.push(Action::Exchange {
                    dest: e.dest.clone(),
                    staged: st.clone(),
                });
                out.push(displace(e, st));
            }

            // Still old, but the fallback had already moved the new link to
            // its scratch name. Resume from its step two.
            (Slot::Old, Slot::Absent, Slot::New) => {
                let st = staged.clone().expect("a swap slot implies a staged name");
                out.push(Action::Rename {
                    from: e.dest.clone(),
                    to: st.clone(),
                });
                out.push(Action::Rename {
                    from: mutate::fallback_slot(&st),
                    to: e.dest.clone(),
                });
                out.push(displace(e, st));
            }

            // Still old and nothing staged at all: the crash beat the staging
            // step. Stage it now and exchange. This is the case a step-driven
            // replay gets wrong, because the recorded step that created the
            // link is the one that did not happen.
            (Slot::Old, Slot::Absent, Slot::Absent) => {
                let st = staged.clone().ok_or_else(|| Error::Refused {
                    rule: "R4",
                    path: e.dest.clone(),
                    why: "journal records an exchange with no staging name".into(),
                })?;
                out.push(Action::StageLink {
                    path: st.clone(),
                    target: e.new_target.clone(),
                });
                out.push(Action::Exchange {
                    dest: e.dest.clone(),
                    staged: st.clone(),
                });
                out.push(displace(e, st));
            }

            (dest, st, sw) => return Err(unexpected(e, dest, st, sw)),
        },

        Direction::Backward => match (&s.dest, &s.staged, &s.swap) {
            // Nothing was exchanged, so the destination is untouched. Only the
            // staged links need to go, and they go to the attic like anything
            // else ricepilot displaces.
            (Slot::Old, _, _) | (Slot::Absent, _, _) => {
                if let Some(st) = &staged {
                    if s.staged == Slot::New {
                        out.push(Action::ToAttic {
                            from: st.clone(),
                            rel: staged_attic_rel(e),
                        });
                    }
                    if s.swap == Slot::New {
                        out.push(Action::ToAttic {
                            from: mutate::fallback_slot(st),
                            rel: staged_attic_rel(e),
                        });
                    }
                }
            }
            (dest, st, sw) => return Err(unexpected(e, dest, st, sw)),
        },
    }
    Ok(out)
}

/// The displaced old link's move into the attic. Every forward arm that drives
/// a destination across ends with one: the exchange leaves the old link at the
/// staging name, and leaving it there would mean a `.rp-tmp-0` symlink sitting
/// in the user's `~/.config` forever.
fn displace(e: &Entry, staged: PathBuf) -> Action {
    Action::ToAttic {
        from: staged,
        rel: e.attic_rel.clone().unwrap_or_else(|| staged_attic_rel(e)),
    }
}

fn unexpected(e: &Entry, dest: &Slot, staged: &Slot, swap: &Slot) -> Error {
    Error::Refused {
        rule: "R5",
        path: e.dest.clone(),
        why: format!(
            "an interrupted switch left a combination ricepilot cannot account for \
             (destination: {dest:?}, staged: {staged:?}, scratch: {swap:?}). It will not \
             guess; nothing has been changed"
        ),
    }
}

/// Work out what it would take to finish — or abandon — an interrupted switch.
///
/// Read-only. The caller prints this and stops unless `--commit` was given
/// (R4), and passes the very same value to [`execute`] if it was.
pub fn plan_recovery(j: &Journal) -> Result<Recovery> {
    let mut slots = Vec::new();
    let mut statuses = Vec::new();
    for e in &j.entries {
        let s = read_slots(e)?;
        statuses.push(Status {
            dest: e.dest.clone(),
            side: side_of(e, &s)?,
        });
        slots.push(s);
    }

    // A retirement has only two states, and they are as distinguishable as
    // any other pair: the link is still at the destination, or it is in the
    // attic and the destination is empty.
    let mut retire_sides = Vec::new();
    for r in &j.retire {
        let side = retire_side(r)?;
        retire_sides.push(side);
        statuses.push(Status {
            dest: r.dest.clone(),
            side,
        });
    }

    let mut adopt_slots = Vec::new();
    for a in &j.adopt {
        let s = slots_of_adopt(a)?;
        statuses.push(Status {
            dest: a.dest.clone(),
            side: adopt_side(a, &s)?,
        });
        adopt_slots.push(s);
    }

    // Once a single exchange has taken effect, going back means undoing
    // something that is already true of the machine, using the same window
    // that just failed. Going forward finishes what is already most of the
    // way done. Before the first exchange there is nothing to finish, and the
    // switch is abandoned instead.
    let direction = if statuses
        .iter()
        .any(|s| s.side == Side::New || s.side == Side::InFlight)
    {
        Direction::Forward
    } else {
        Direction::Backward
    };

    let mut actions = Vec::new();
    for (e, s) in j.entries.iter().zip(&slots) {
        actions.extend(actions_for(e, s, direction)?);
    }
    for (a, s) in j.adopt.iter().zip(&adopt_slots) {
        actions.extend(actions_for_adopt(a, s, direction)?);
    }

    // Retirements happen in phase C, after every exchange. Going forward
    // finishes the ones that have not happened; going backward means no
    // exchange took effect, so no retirement did either and the destination
    // is untouched — there is nothing to undo and nothing to do.
    for (r, side) in j.retire.iter().zip(&retire_sides) {
        if direction == Direction::Forward && *side == Side::Old {
            actions.push(Action::ToAttic {
                from: r.dest.clone(),
                rel: r.attic_rel.clone(),
            });
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    for dest in j
        .entries
        .iter()
        .map(|e| &e.dest)
        .chain(j.retire.iter().map(|r| &r.dest))
        .chain(j.adopt.iter().map(|a| &a.dest))
    {
        if let Some(p) = dest.parent() {
            if !dirs.contains(&p.to_path_buf()) {
                dirs.push(p.to_path_buf());
            }
        }
    }
    for dir in dirs {
        actions.push(Action::FsyncDir { dir });
    }
    actions.push(Action::FsyncDir {
        dir: j.attic.clone(),
    });

    Ok(Recovery {
        id: j.id.clone(),
        profile: j.profile.clone(),
        adopting: !j.adopt.is_empty(),
        direction,
        statuses,
        actions,
    })
}

/// Carry out a [`Recovery`]. Separate from [`plan_recovery`] so the printed
/// decision and the executed one are the same value (R4), exactly as
/// [`crate::plan`] and [`mutate::apply`] are.
pub fn execute(r: &Recovery, journal_path: &Path, mode: ExchangeMode, attic: &Path) -> Result<()> {
    for action in &r.actions {
        match action {
            Action::StageLink { path, target } => mutate::create_symlink_tmp(path, target)?,
            Action::CreateLink { dest, target } => mutate::create_symlink(dest, target)?,
            Action::Exchange { dest, staged } => mutate::exchange(mode, dest, staged)?,
            Action::Rename { from, to } => mutate::rename_within(from, to)?,
            Action::ToAttic { from, rel } => {
                mutate::rename_to_attic(from, attic, rel)?;
            }
            Action::FsyncDir { dir } => mutate::fsync_dir(dir)?,
        }
    }
    // Retiring the journal is deliberately not one of the actions: while the
    // journal is in place another `recover` can be run, and that must stay
    // true until every action above has succeeded. A failure half way through
    // recovery leaves a machine that can be recovered again.
    mark_done(journal_path, &r.id)?;
    Ok(())
}
