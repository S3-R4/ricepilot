//! `ricepilot diff <profile>`: a profile against the live filesystem.
//!
//! For a `dir-link` profile the live filesystem is two things, and this
//! compares both (D60):
//!
//! * **The links.** Each destination the profile's manifest declares is
//!   classified with the ownership predicate a switch acts on
//!   ([`crate::observe::shape_via`]) and set beside what the profile puts
//!   there: a link, recorded in the ledger, to `<root>/<src>`, with a
//!   directory at the far end. Anything else at the destination — another
//!   profile's link, a link ricepilot did not make (even one to the same
//!   place), a real directory, a file, nothing — differs. So does a link
//!   ricepilot owns that the profile does not declare: switching to it would
//!   retire that link into the attic. A real directory where the link should
//!   be is also compared, path by path, with the profile's source for it,
//!   because that is what an installer leaves behind and what the user has
//!   to decide about.
//! * **The content.** The profile's tree, from its root — for a
//!   by-reference profile, the external directory it names, such as the live
//!   caelestia clone — against the blake3 manifest ricepilot recorded the
//!   last time it switched to or captured it, `volatile` excluded: the
//!   comparison a switch reports (D59), made by the same function. What
//!   `volatile` left out is listed on its own, never as drift.
//!
//! **Read-only by construction**, as `doctor` is (D57): everything
//! [`compare`] learns about the machine comes through the `&dyn Look` it is
//! handed, and every other crate item this module names is pure or reads
//! only through that same `Look`. It takes no lock, starts no process and
//! builds no verify-config copy. `tests/diff.rs` checks the source text for
//! this, and checks every run against the whole fixture's
//! `(dev, ino, mtime_ns)`.
//!
//! It exits 6 (`Drift`) when it saw a difference, as `verify` does (D34), 4
//! when the content could not be compared because something could not be
//! read, and 0 otherwise.

mod render;

use std::path::{Path, PathBuf};

use crate::cli::paths::Paths;
use crate::error::ExitCode;
use crate::manifest::Activation;
use crate::manifest::Kind;
use crate::observe::Ownership;
use crate::observe::Shape;
use crate::ops::look::Look;
use crate::plan::SourceState;
use crate::verify::AgainstRecord;
use crate::verify::Difference;
use crate::Error;
use crate::Result;

/// What is at one destination now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// ricepilot's link — all three facts of the ownership predicate hold —
    /// to `target`, recorded for `profile`.
    Owned {
        target: PathBuf,
        profile: String,
    },
    /// A symlink that is not ricepilot's; `recorded` says which fact of the
    /// ownership predicate it fails.
    Foreign {
        target: PathBuf,
        dangling: bool,
        recorded: Recorded,
    },
    /// A real directory. `content` is its comparison with the profile's
    /// source for this destination, when there is a source directory to
    /// compare it with.
    RealDir {
        content: Option<Content>,
    },
    /// A regular file, or anything else that is neither a directory nor a
    /// link.
    RealFile,
    Absent,
}

/// What the ledger says about a link that is not ricepilot's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// Nothing: ricepilot did not make it (fact 3).
    No,
    /// A row for this path that describes another link — another target or
    /// another inode (fact 3).
    Otherwise,
    /// A row describing exactly this link, whose target is inside no
    /// registered profile's root any more (fact 2).
    OutsideEveryProfile,
}

/// A real directory at a destination against the profile's source for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// `verify::compare`'s answer, the source as the "recorded" side: `added`
    /// is in the directory and not in the source.
    Compared {
        diffs: Vec<Difference>,
    },
    Unreadable {
        why: String,
    },
}

/// What the profile puts at one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// A link to `src`; `source` is what is there now.
    Link { src: PathBuf, source: SourceState },
    /// Nothing: the profile does not link this destination — it does not
    /// declare it, or declares it as something never linked — and ricepilot
    /// owns something at it, which a switch to the profile would retire.
    Undeclared,
}

/// One destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub dest: PathBuf,
    pub found: Found,
    pub wanted: Wanted,
}

impl Row {
    /// The live destination is exactly what the profile puts there:
    /// ricepilot's link, to the profile's source, which is a directory.
    pub fn same(&self) -> bool {
        match (&self.found, &self.wanted) {
            (
                Found::Owned { target, .. },
                Wanted::Link {
                    src,
                    source: SourceState::Dir,
                },
            ) => target == src,
            _ => false,
        }
    }
}

/// A `[[path]]` the profile declares and ricepilot never links: `generated`
/// and `volatile` classifications, and a `dir-link` with
/// `activation = "never"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeverLinked {
    pub dest: PathBuf,
    pub kind: &'static str,
    pub activation: &'static str,
}

/// The whole comparison. [`Report::text`] is what is printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub profile: String,
    pub root: PathBuf,
    pub by_reference: bool,
    /// `journal/current.toml`, when there is one: an operation is running or
    /// did not finish, and the links may be half way between two profiles.
    pub in_flight: Option<PathBuf>,
    pub rows: Vec<Row>,
    pub never_linked: Vec<NeverLinked>,
    /// Where the recorded manifest is, or would be.
    pub manifest: PathBuf,
    pub content: AgainstRecord,
    /// The profile's `volatile` globs, as declared now.
    pub volatile: Vec<String>,
}

impl Report {
    /// Destinations that are not what the profile puts there.
    pub fn links_differing(&self) -> usize {
        self.rows.iter().filter(|r| !r.same()).count()
    }

    /// Paths of the tree that differ from the record, `touched` ones not
    /// counted (D34).
    pub fn paths_differing(&self) -> usize {
        match &self.content {
            AgainstRecord::Compared { diffs, .. } => {
                diffs.iter().filter(|d| d.is_substantive()).count()
            }
            _ => 0,
        }
    }

    /// 4 when the content could not be compared because something could not
    /// be read: the answer is incomplete, and 0 or 6 would each claim more
    /// than was seen. Otherwise 6 when anything differs, and 0 when nothing
    /// does — which, when nothing has been recorded yet, is said to be about
    /// the links alone (D60).
    pub fn exit_code(&self) -> ExitCode {
        if let AgainstRecord::Unreadable { .. } = self.content {
            return ExitCode::Failed;
        }
        if self.links_differing() + self.paths_differing() > 0 {
            ExitCode::Drift
        } else {
            ExitCode::Ok
        }
    }

    pub fn text(&self) -> String {
        render::text(self)
    }
}

/// `ricepilot diff <profile>`, against the machine.
pub fn run(paths: &Paths, name: &str) -> Result<crate::cli::Output> {
    let report = compare(&crate::ops::look::Live, paths, name)?;
    Ok(crate::cli::Output {
        text: report.text(),
        code: report.exit_code(),
    })
}

/// Compare profile `name` with what `look` finds.
///
/// Refuses what `plan` refuses before it looks at a destination: a profile
/// that is not registered or whose manifest is not valid, another registered
/// profile whose manifest is not valid (its root is fact 2 of the ownership
/// predicate for every link), and a ledger that does not parse (fact 3). A
/// destination that cannot be classified is refused too, as `plan` refuses
/// it. The content comparison never refuses; it says what it could not read.
pub fn compare(look: &dyn Look, paths: &Paths, name: &str) -> Result<Report> {
    let profile = crate::cli::paths::load_via(look, paths, name)?;
    let all = crate::cli::paths::load_all_via(look, paths)?;
    let ledger = crate::ledger::load_via(look, &paths.ledger_path())?;
    let own = ledger.ownership(all.iter().map(|p| p.root(&paths.home)).collect());

    let root = profile.root(&paths.home);
    let volatile = profile.manifest.volatile.clone();
    let targets = profile.manifest.targets(&profile.dir, &paths.home);
    let sources = crate::cli::switch::source_facts_via(look, &targets)?;

    let journal = paths.journal_path();
    let in_flight = look.lstat_or_absent(&journal)?.map(|_| journal);

    let mut rows = Vec::new();
    for t in &targets {
        let source = sources
            .iter()
            .find(|f| f.src == t.src)
            .map(|f| f.state)
            .unwrap_or(SourceState::Missing);
        // The source's path inside the profile, which is what `volatile`
        // globs are written against.
        let anchor = t.src.strip_prefix(&root).unwrap_or(&t.src);
        let against = (source == SourceState::Dir).then_some(Against {
            src: &t.src,
            anchor,
            volatile: &volatile,
        });
        rows.push(Row {
            dest: t.dest.clone(),
            found: found(look, &t.dest, &own, &ledger, against)?,
            wanted: Wanted::Link {
                src: t.src.clone(),
                source,
            },
        });
    }

    // What ricepilot owns that this profile does not declare. Nothing there
    // now is no difference — a switch would leave nothing there too — and a
    // ledger row describing nothing is `doctor`'s to report.
    for e in &ledger.entries {
        if targets.iter().any(|t| t.dest == e.dest) {
            continue;
        }
        let found = found(look, &e.dest, &own, &ledger, None)?;
        if found != Found::Absent {
            rows.push(Row {
                dest: e.dest.clone(),
                found,
                wanted: Wanted::Undeclared,
            });
        }
    }

    let never_linked = profile
        .manifest
        .paths
        .iter()
        .filter(|p| !(p.kind == Kind::DirLink && p.activation == Activation::Relogin))
        .map(|p| NeverLinked {
            dest: crate::manifest::expand_home(&p.dest, &paths.home),
            kind: p.kind.as_str(),
            activation: p.activation.as_str(),
        })
        .collect();

    let content = crate::verify::against_record_via(look, &paths.state, name, &root, &volatile);

    Ok(Report {
        profile: profile.name.clone(),
        by_reference: profile.manifest.root.is_some(),
        root,
        in_flight,
        rows,
        never_linked,
        manifest: crate::verify::manifest_path(&paths.state, name),
        content,
        volatile,
    })
}

/// The profile's source for a destination, for comparing a real directory
/// found there with it.
#[derive(Clone, Copy)]
struct Against<'a> {
    src: &'a Path,
    anchor: &'a Path,
    volatile: &'a [String],
}

fn found(
    look: &dyn Look,
    dest: &Path,
    own: &Ownership,
    ledger: &crate::ledger::Ledger,
    against: Option<Against<'_>>,
) -> Result<Found> {
    // A missing parent is an empty destination, not a fault: `~/.config`
    // may not exist on a machine nothing has been linked on.
    let meta = look.lstat_or_absent(dest)?;
    let row = ledger.entries.iter().find(|e| e.dest == dest);
    Ok(match crate::observe::shape_via(look, dest, meta, own)? {
        Shape::OwnedLink { target } => Found::Owned {
            target,
            profile: row.map(|e| e.profile.clone()).unwrap_or_default(),
        },
        Shape::ForeignLink { target, dangling } => {
            let recorded = match (row, meta) {
                (None, _) => Recorded::No,
                (Some(e), Some(m)) if e.target == target && (e.dev, e.ino) == (m.dev, m.ino) => {
                    Recorded::OutsideEveryProfile
                }
                (Some(_), _) => Recorded::Otherwise,
            };
            Found::Foreign {
                target,
                dangling,
                recorded,
            }
        }
        Shape::RealDir => Found::RealDir {
            content: against.map(|a| real_dir_content(look, dest, a)),
        },
        Shape::RealFile => Found::RealFile,
        Shape::Absent => Found::Absent,
    })
}

/// A real directory at a destination against the profile's source for it:
/// the same walk and comparison as the content check, both sides anchored
/// where the source sits in the profile so `volatile` means the same thing
/// on each.
fn real_dir_content(look: &dyn Look, dest: &Path, a: Against<'_>) -> Content {
    let walk = |root: &Path| {
        crate::verify::build_within_via(look, "", root, a.anchor, a.volatile, "").map(|(m, _)| m)
    };
    match (walk(a.src), walk(dest)) {
        (Ok(src), Ok(live)) => Content::Compared {
            diffs: crate::verify::compare(&src, &live),
        },
        (Err(e), _) | (_, Err(e)) => Content::Unreadable { why: said(&e) },
    }
}

/// The message of an error, without the `refusing:` framing that suits a
/// command declining and not a line in a report.
fn said(e: &Error) -> String {
    match e {
        Error::Refused { why, path, .. } => format!("{why} ({})", path.display()),
        other => other.to_string(),
    }
}
