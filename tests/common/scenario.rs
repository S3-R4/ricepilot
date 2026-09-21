//! One switch, built on a real fixture tree, shared by the journal, crash-
//! injection and recovery suites.
//!
//! Every one of those asks the same question from a different angle — "after
//! this, is the machine fully old or fully new?" — so they must all be asking
//! it about the same machine.

#![allow(dead_code)]

use std::path::PathBuf;

use ricepilot::journal::Journal;
use ricepilot::observe::{observe, LedgerEntry, Observed, Ownership};
use ricepilot::ops::mutate::{ApplyContext, ExchangeMode};
use ricepilot::ops::read;
use ricepilot::plan::{plan, Op, Plan, PlanContext, Target};

use super::Fixture;

pub struct Scenario {
    pub f: Fixture,
    pub old_root: PathBuf,
    pub new_root: PathBuf,
    pub attic: PathBuf,
    pub targets: Vec<Target>,
    pub observed: Vec<Observed>,
    pub ops: Vec<Op>,
    pub journal: Journal,
    pub mode: ExchangeMode,
}

pub const ID: &str = "20260921T101112Z";

/// Two owned links pointing at the `old` profile, plus one destination that is
/// absent — so a single scenario covers both the exchange path (shape 1) and
/// the direct-create path (shape 5, D11), and a crash between them is a state
/// recovery has to handle.
pub fn build(case: &str, mode: ExchangeMode) -> Scenario {
    let f = Fixture::new_in("m2", case);
    let old_root = f.dir("rice/old");
    let new_root = f.dir("rice/new");
    for leaf in ["hypr", "foot", "btop"] {
        f.dir(&format!("rice/old/{leaf}"));
        f.dir(&format!("rice/new/{leaf}"));
        f.file(&format!("rice/old/{leaf}/marker"), "old\n");
        f.file(&format!("rice/new/{leaf}/marker"), "new\n");
    }
    f.dir(".config");
    let hypr = f.link(".config/hypr", &old_root.join("hypr"));
    let foot = f.link(".config/foot", &old_root.join("foot"));
    let btop = f.path(".config/btop");

    let ownership = Ownership {
        profile_roots: vec![old_root.clone(), new_root.clone()],
        entries: [&hypr, &foot]
            .iter()
            .map(|d| {
                let m = read::lstat(d).unwrap().unwrap();
                LedgerEntry {
                    dest: d.to_path_buf(),
                    target: read::readlink(d).unwrap(),
                    dev: m.dev,
                    ino: m.ino,
                }
            })
            .collect(),
    };

    let targets = vec![
        Target {
            dest: hypr.clone(),
            src: new_root.join("hypr"),
        },
        Target {
            dest: foot.clone(),
            src: new_root.join("foot"),
        },
        Target {
            dest: btop.clone(),
            src: new_root.join("btop"),
        },
    ];
    let dests: Vec<PathBuf> = targets.iter().map(|t| t.dest.clone()).collect();
    let observed = observe(&dests, &ownership).unwrap();

    let attic = f.attic(ID);
    let attic_dev = read::dev_of_nearest_existing_ancestor(&attic).unwrap();
    let ctx = PlanContext::new(f.home.clone(), attic.clone(), attic_dev);
    let ops = match plan(&observed, &targets, &ctx) {
        Plan::Apply { ops } => ops,
        other => panic!("the scenario must produce an applicable plan, got {other:?}"),
    };

    let journal = Journal::from_plan(ID, "new", attic.clone(), mode, &ops, &observed).unwrap();

    Scenario {
        f,
        old_root,
        new_root,
        attic,
        targets,
        observed,
        ops,
        journal,
        mode,
    }
}

impl Scenario {
    pub fn apply_ctx(&self) -> ApplyContext {
        ApplyContext {
            mode: self.mode,
            attic: self.attic.clone(),
        }
    }

    pub fn journal_path(&self) -> PathBuf {
        self.f.state().join("journal").join("current.toml")
    }

    /// What each destination points at now, in declaration order. `None` means
    /// nothing is there; `Some(None)` means something is there that is not a
    /// symlink.
    pub fn live(&self) -> Vec<Option<Option<PathBuf>>> {
        self.targets
            .iter()
            .map(|t| ricepilot::ops::mutate::link_target(&t.dest).unwrap())
            .collect()
    }

    /// The fully-old topology: two links into `old`, and nothing at `btop`.
    pub fn all_old(&self) -> Vec<Option<Option<PathBuf>>> {
        vec![
            Some(Some(self.old_root.join("hypr"))),
            Some(Some(self.old_root.join("foot"))),
            None,
        ]
    }

    /// The fully-new topology: every destination into `new`.
    pub fn all_new(&self) -> Vec<Option<Option<PathBuf>>> {
        self.targets
            .iter()
            .map(|t| Some(Some(t.src.clone())))
            .collect()
    }
}
