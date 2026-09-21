//! `switch` and `rollback`, which are one code path (`docs/DESIGN.md` §6).
//!
//! `rollback` re-applies generation `NNNN-1` through *this* function. It is
//! not a second, less-tested implementation of the same idea: the riskiest
//! command in the tool is the one that must not have a code path of its own.
//! The only things that differ are where the target state comes from and the
//! wording, and both are arguments.
//!
//! Phase A decides and mutates nothing. Phase B is the exchanges. Phase C
//! settles: attic, fsyncs, generation, ledger, tree manifest, `rescue.sh`,
//! and only then is the journal retired (D25).

use std::path::PathBuf;

use crate::error::ExitCode;
use crate::generations::{self, Generation};
use crate::journal::{self, Journal};
use crate::ledger;
use crate::observe;
use crate::ops::{lock, mutate, read};
use crate::plan::{self, Plan, Target};
use crate::rescue;
use crate::verify;
use crate::{Error, Result};

use super::paths::Paths;
use super::{render, Output};

/// Which command is running. It changes nothing about what happens; it changes
/// what the user is told, and telling a user "switched to profile `caelestia`"
/// when they typed `rollback` is how a person loses track of which way round
/// their machine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Switch,
    Rollback,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Switch => "switch",
            Kind::Rollback => "rollback",
        }
    }
}

/// Everything the switch machinery needs that differs between `switch` and
/// `rollback`.
pub struct Request {
    pub kind: Kind,
    /// What the destination state is called in the output.
    pub label: String,
    /// The profile name recorded in the ledger and the generation.
    pub profile: String,
    pub targets: Vec<Target>,
    /// Destinations ricepilot owns that this target state drops (D36).
    pub retire: Vec<PathBuf>,
    /// The profile tree to record a blake3 manifest of, and the globs to
    /// exclude. `None` when there is no single tree to hash — rolling back to
    /// generation `0000`, whose links may point anywhere.
    pub manifest_of: Option<(PathBuf, Vec<String>)>,
}

impl Request {
    /// Every destination phase A must observe: the ones being switched and
    /// the ones being retired, with no duplicates.
    fn dests(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.targets.iter().map(|t| t.dest.clone()).collect();
        for d in &self.retire {
            if !out.contains(d) {
                out.push(d.clone());
            }
        }
        out
    }
}

/// Phase A, B and C.
pub fn run(paths: &Paths, req: &Request, commit: bool) -> Result<Output> {
    run_with(paths, req, commit, &mut |_| Ok(()))
}

/// [`run`], with a hook called after each op of phase B/C has completed.
///
/// It exists for one reason: the crash-injection harness ends the process
/// after step *k* for every *k*, against **this** function rather than against
/// `mutate::apply`, so that what is proven recoverable is the command a user
/// actually runs and not a subset of it. In ordinary use the hook is a closure
/// that returns `Ok(())`. Same shape, and same reason, as `mutate::apply`'s
/// (D26).
pub fn run_with(
    paths: &Paths,
    req: &Request,
    commit: bool,
    after_step: &mut dyn FnMut(usize) -> Result<()>,
) -> Result<Output> {
    // ---- Phase A: decide. Nothing below mutates until the commit gate. ----

    let _lock = lock::acquire(&paths.lock_path()?)?;

    // An in-flight journal means a previous switch did not finish. Planning a
    // new one against a half-switched machine would produce a correct plan for
    // a machine that does not exist, and applying it would bury the evidence
    // `recover` needs.
    if journal::read_current(&paths.journal_path())?.is_some() {
        return Err(Error::Refused {
            rule: "R4",
            path: paths.journal_path(),
            why: "a switch is already in flight and did not finish. run `ricepilot recover` \
                  first; until then ricepilot will not plan against a machine that is half \
                  way between two profiles"
                .into(),
        });
    }

    let profiles = super::paths::load_all(paths)?;
    let ledger_file = paths.ledger_path();
    let mut ledger = ledger::load(&ledger_file)?;
    let ownership = ledger.ownership(profiles.iter().map(|p| p.root(&paths.home)).collect());

    let dests = req.dests();
    let observed = observe::observe(&dests, &ownership)?;

    // Unique, not merely time-based: a rollback immediately after a switch is
    // the ordinary case, and two switches in the same second must not share an
    // id (D40).
    let id = journal::unique_id(
        &paths.state,
        &journal::timestamp_id(std::time::SystemTime::now()),
    )?;
    let attic = paths.attic_dir().join(&id);
    let attic_dev = read::dev_of_nearest_existing_ancestor(&paths.attic_dir())?;
    let ctx = plan::PlanContext::new(paths.home.clone(), attic.clone(), attic_dev)
        .retiring(req.retire.clone());
    let plan = plan::plan(&observed, &req.targets, &ctx);

    let header = render::switch_header(req, &observed, &plan, commit);

    // A decline is the complete list of reasons and a non-zero exit, with zero
    // side effects (R4). It is not an `Err` because the itemised list is the
    // answer, and an error message is one line.
    if let Plan::Decline { .. } = &plan {
        return Ok(Output {
            text: header,
            code: ExitCode::Refused,
        });
    }
    let Plan::Apply { ops } = &plan else {
        // NoOp: reality already matches. Nothing to journal, nothing to
        // record, and no new generation — a generation per no-op switch would
        // make `rollback` step through states the machine was never in.
        return Ok(Output {
            text: header,
            code: ExitCode::Ok,
        });
    };

    if !commit {
        return Ok(Output {
            text: header + render::UNCOMMITTED,
            code: ExitCode::Ok,
        });
    }

    // ---- The commit gate is behind us. From here on things change. ----

    // Probed once, here rather than at startup: the probe creates two symlinks
    // in ricepilot's own state directory, and a dry run must have no effects
    // at all (D39).
    mutate::make_dirs(&paths.state)?;
    let mode = mutate::probe_exchange(&paths.state)?;
    mutate::make_dirs(&attic)?;

    // The generation before this one, taken while it is still true. On a
    // machine that has never switched this is generation 0000 (D35).
    let previous = generations::current(&paths.state)?;
    let new_id = match previous {
        Some(n) => n + 1,
        None => {
            let pre = Generation::observe(0, generations::PRE_EXISTING, &id, &dests)?;
            generations::save(&paths.state, &pre)?;
            1
        }
    };

    // R4: the journal is written and fsync'd — the file *and* its directory —
    // before the first effect. Before the staging links too, so a crash
    // between here and the exchanges leaves a record of what the staged names
    // are; without one, an orphaned staged link would be invisible to
    // `recover` and would block the next switch (D38).
    let j = Journal::from_plan(&id, &req.profile, attic.clone(), mode, ops, &observed)?;
    journal::write(&paths.journal_path(), &j)?;

    // ---- Phase B and C: the plan, exactly as it was printed. ----
    let apply_ctx = mutate::ApplyContext {
        mode,
        attic: attic.clone(),
    };
    mutate::apply(ops, &apply_ctx, after_step)?;

    // Generation NNNN, from the POST observation. Read off the disk, never
    // assumed from what the plan said it would do (R7).
    let after = Generation::observe(new_id, &req.profile, &id, &dests)?;
    generations::save(&paths.state, &after)?;
    generations::set_current(&paths.state, new_id)?;

    // The ledger: the switched destinations are ours, the retired ones are not
    // any more. A row for a path we no longer own would be an ownership claim
    // nothing on the disk supports.
    let switched: Vec<PathBuf> = req.targets.iter().map(|t| t.dest.clone()).collect();
    ledger.record(&switched, &req.profile)?;
    ledger.forget(&req.retire);
    ledger::save(&ledger_file, &ledger)?;

    let mut recorded_manifest = None;
    if let Some((root, volatile)) = &req.manifest_of {
        let m = verify::build(&req.profile, root, volatile, &id)?;
        let p = verify::manifest_path(&paths.state, &req.profile);
        verify::save(&p, &m)?;
        recorded_manifest = Some(p);
    }

    // `rescue.sh` restores the generation *before* this one — which is the
    // thing someone who cannot log in wants.
    let back_to = generations::load(&paths.state, new_id - 1)?;
    let script = rescue::regenerate(&paths.state, &back_to)?;

    // Retiring the journal is the last act (D25). While it is in place the
    // machine can be recovered again, and that must stay true until every
    // step above has succeeded.
    journal::mark_done(&paths.journal_path(), &id)?;

    Ok(Output {
        text: header
            + &render::switch_done(
                req,
                new_id,
                new_id - 1,
                &attic,
                &script,
                recorded_manifest.as_deref(),
                &req.retire,
            ),
        code: ExitCode::Ok,
    })
}
