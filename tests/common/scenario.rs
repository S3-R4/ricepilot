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
    build_with(case, mode, true)
}

/// Without the absent destination, every row is shape 1 and the switch is a
/// pure permutation of link targets — which is the only shape for which an
/// exact inverse exists without a delete primitive (D21).
pub fn build_links_only(case: &str, mode: ExchangeMode) -> Scenario {
    build_with(case, mode, false)
}

/// Re-describe a scenario whose tree already exists, without disturbing it.
/// The journal comes from the disk, because after a crash the disk is the only
/// place it is.
pub fn attach(case: &str, mode: ExchangeMode) -> Scenario {
    let f = Fixture::attach("m2", case);
    let old_root = f.path("rice/old");
    let new_root = f.path("rice/new");
    let attic = f.state().join("attic").join(ID);
    let targets: Vec<Target> = ["hypr", "foot", "btop"]
        .iter()
        .map(|leaf| Target {
            dest: f.path(&format!(".config/{leaf}")),
            src: new_root.join(leaf),
        })
        .collect();
    let journal = ricepilot::journal::read_current(&f.state().join("journal").join("current.toml"))
        .unwrap()
        .expect("the crashed process wrote its journal before its first effect");
    Scenario {
        f,
        old_root,
        new_root,
        attic,
        targets,
        observed: Vec::new(),
        ops: Vec::new(),
        journal,
        mode,
    }
}

pub fn build_with(case: &str, mode: ExchangeMode, include_absent: bool) -> Scenario {
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

    let mut targets = vec![
        Target {
            dest: hypr.clone(),
            src: new_root.join("hypr"),
        },
        Target {
            dest: foot.clone(),
            src: new_root.join("foot"),
        },
    ];
    if include_absent {
        targets.push(Target {
            dest: btop.clone(),
            src: new_root.join("btop"),
        });
    }
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

    /// The fully-old topology: every exchanged destination back into `old`,
    /// and nothing at a destination that started absent.
    pub fn all_old(&self) -> Vec<Option<Option<PathBuf>>> {
        self.targets
            .iter()
            .map(|t| {
                let leaf = t.dest.file_name().unwrap();
                if t.dest.ends_with("btop") {
                    None
                } else {
                    Some(Some(self.old_root.join(leaf)))
                }
            })
            .collect()
    }

    /// The ownership oracle for the links as they stand right now, as a real
    /// ledger would describe them after the switch that put them there.
    pub fn ownership_now(&self) -> Ownership {
        Ownership {
            profile_roots: vec![self.old_root.clone(), self.new_root.clone()],
            entries: self
                .targets
                .iter()
                .filter_map(|t| {
                    let m = read::lstat(&t.dest).ok()??;
                    Some(LedgerEntry {
                        dest: t.dest.clone(),
                        target: read::readlink(&t.dest).ok()?,
                        dev: m.dev,
                        ino: m.ino,
                    })
                })
                .collect(),
        }
    }

    /// Plan the switch back: same destinations, sources under `old` again.
    pub fn inverse_ops(&self, attic: &std::path::Path) -> Vec<Op> {
        let targets: Vec<Target> = self
            .targets
            .iter()
            .map(|t| Target {
                dest: t.dest.clone(),
                src: self.old_root.join(t.dest.file_name().unwrap()),
            })
            .collect();
        let dests: Vec<PathBuf> = targets.iter().map(|t| t.dest.clone()).collect();
        let observed = observe(&dests, &self.ownership_now()).unwrap();
        let attic_dev = read::dev_of_nearest_existing_ancestor(attic).unwrap();
        let ctx = PlanContext::new(self.f.home.clone(), attic.to_path_buf(), attic_dev);
        match plan(&observed, &targets, &ctx) {
            Plan::Apply { ops } => ops,
            other => panic!("the inverse must be applicable, got {other:?}"),
        }
    }

    /// The fully-new topology: every destination into `new`.
    pub fn all_new(&self) -> Vec<Option<Option<PathBuf>>> {
        self.targets
            .iter()
            .map(|t| Some(Some(t.src.clone())))
            .collect()
    }
}
