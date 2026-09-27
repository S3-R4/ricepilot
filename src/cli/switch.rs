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
use crate::ops::look::{Live, Look};
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
    /// The packages the target profile declares in `requires`, checked with
    /// `pacman -Q` in phase A. Empty when there is nothing to check — which
    /// includes rolling back to generation `0000`, which belongs to no
    /// profile (D53).
    pub requires: Vec<String>,
    /// The profile tree to record a blake3 manifest of, and the globs to
    /// exclude. `None` when there is no single tree to hash — rolling back to
    /// generation `0000`, whose links may point anywhere. Phase A also
    /// compares this tree with the manifest recorded last time, and reports
    /// what differs (D59).
    pub manifest_of: Option<(PathBuf, Vec<String>)>,
    /// `--strict`: refuse, rather than report and go ahead past, profile
    /// drift, a profile that could not be compared, and a Hyprland config
    /// that was not checked (D59). `switch` only; a rollback is never strict.
    pub strict: bool,
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

/// How a `switch` or `rollback` ended, beside what it printed.
///
/// `--relogin` reads this and nothing else to decide whether a logout may be
/// offered at all: only [`Ended::Completed`] can lead to one (D58).
pub struct Outcome {
    pub output: Output,
    pub ended: Ended,
}

pub enum Ended {
    /// The pre-flight declined; nothing was touched.
    Declined,
    /// Every destination already matched; nothing was done.
    NothingToDo,
    /// The plan was printed and `--commit` was not given.
    DryRun,
    /// Phase C finished and the journal was retired. Boxed: it carries the
    /// whole record of what was done, and the other endings carry nothing.
    Completed(Box<Completed>),
}

/// Proof that a switch or rollback ran to the end of phase C **in this
/// process**: the exchanges happened, the generation and the ledger were
/// recorded, `rescue.sh` was regenerated and the journal was retired — in
/// that order, with nothing left to do.
///
/// It is made in one place, the last line of [`run_with`], and its fields are
/// private, so holding one means exactly that. It also still holds the
/// process lock the switch took, so nothing can start another operation for
/// as long as it lives: `--relogin` keeps it until `uwsm stop` has been run
/// or declined (D58).
pub struct Completed {
    lock: lock::Lock,
    paths: Paths,
    id: String,
    generation: u32,
    back_to: Generation,
    targets: Vec<Target>,
    retired: Vec<PathBuf>,
    rescue: PathBuf,
    journal_done: PathBuf,
}

impl Completed {
    /// The held process lock.
    pub fn lock(&self) -> &lock::Lock {
        &self.lock
    }
    pub fn paths(&self) -> &Paths {
        &self.paths
    }
    /// The switch id: `journal/done-<id>.toml`, `attic/<id>/`.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// The generation this switch recorded and made current.
    pub fn generation(&self) -> u32 {
        self.generation
    }
    /// The generation before it, which `rescue.sh` now restores.
    pub fn back_to(&self) -> &Generation {
        &self.back_to
    }
    /// The destinations the plan linked, and where to.
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }
    /// The destinations the plan retired into the attic, now absent.
    pub fn retired(&self) -> &[PathBuf] {
        &self.retired
    }
    pub fn rescue(&self) -> &std::path::Path {
        &self.rescue
    }
    /// Where the retired journal went.
    pub fn journal_done(&self) -> &std::path::Path {
        &self.journal_done
    }
}

/// Read what is at each target's `src`, so the planner can refuse a link that
/// would dangle without doing any IO itself.
///
/// This is the one pre-flight that is about the *profile* rather than about
/// the destination, and it is the difference between a switch that fails and a
/// switch that succeeds into a session with no configuration.
pub fn source_facts(targets: &[Target]) -> Result<Vec<plan::SourceFact>> {
    source_facts_via(&Live, targets)
}

/// [`source_facts`], reading through `look` — which is how `diff`, read-only
/// by construction (D60), learns what it tells the user about each source.
pub fn source_facts_via(look: &dyn Look, targets: &[Target]) -> Result<Vec<plan::SourceFact>> {
    let mut out = Vec::new();
    for t in targets {
        if out.iter().any(|f: &plan::SourceFact| f.src == t.src) {
            continue;
        }
        let state = match look.lstat_or_absent(&t.src)? {
            None => plan::SourceState::Missing,
            Some(m) if m.kind == read::Kind::Dir => plan::SourceState::Dir,
            // A symlink to a directory is deliberately *not* a directory here.
            // `lstat` does not follow it, and neither does ricepilot: pointing
            // a managed destination at a link whose target it has never looked
            // at is exactly the chain of trust the ownership predicate exists
            // to refuse.
            Some(_) => plan::SourceState::NotADir,
        };
        out.push(plan::SourceFact {
            src: t.src.clone(),
            state,
        });
    }
    Ok(out)
}

/// The profile tree a switch is about to link, against the manifest
/// ricepilot recorded the last time it switched to it: the walk and the
/// comparison `verify` uses, `volatile` excluded (D59). Read-only.
///
/// What cannot be compared — nothing recorded yet, a recorded manifest or a
/// tree that cannot be read — is a fact to report, not a failure: without
/// `--strict` the switch goes ahead exactly as it did before this check
/// existed (and says so when a read failed).
///
/// The comparison itself is [`verify::against_record_via`], which `diff`
/// makes too (D60); this is what a switch makes of its answer.
pub fn drift(
    state: &std::path::Path,
    profile: &str,
    root: &std::path::Path,
    volatile: &[String],
) -> plan::Drift {
    let manifest = verify::manifest_path(state, profile);
    let not_compared = |why: String, read_failed: bool| plan::Drift::NotCompared {
        profile: profile.to_string(),
        manifest: manifest.clone(),
        why,
        read_failed,
    };
    match verify::against_record_via(&Live, state, profile, root, volatile) {
        verify::AgainstRecord::Unrecorded => {
            not_compared("nothing has been recorded for it yet".into(), false)
        }
        verify::AgainstRecord::Unreadable { why } => not_compared(why, true),
        verify::AgainstRecord::Compared {
            recorded, diffs, ..
        } => plan::Drift::Compared {
            profile: profile.to_string(),
            manifest: manifest.clone(),
            recorded: recorded.created,
            changed: diffs
                .iter()
                .filter(|d| d.is_substantive())
                .map(|d| d.to_string())
                .collect(),
            touched: diffs.iter().filter(|d| !d.is_substantive()).count(),
            volatile: volatile.to_vec(),
        },
    }
}

/// Phase A, B and C.
pub fn run(paths: &Paths, req: &Request, commit: bool) -> Result<Outcome> {
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
) -> Result<Outcome> {
    // ---- Phase A: decide. Nothing below mutates until the commit gate. ----

    // Held until the returned `Completed` is dropped, or to the end of this
    // function for any other outcome.
    let lock = lock::acquire(&paths.lock_path()?)?;

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
    // Run in a dry run too, so the plan printed is the one `--commit` acts on.
    // Its one effect is the scratch copy under `state/verify/<id>/`, in
    // ricepilot's own state and never the user's config (D55).
    let hypr = crate::hyprverify::check(&paths.home, &paths.state, &id, &req.targets, &req.retire)?;
    let drift = req
        .manifest_of
        .as_ref()
        .map(|(root, volatile)| drift(&paths.state, &req.profile, root, volatile));
    let ctx = plan::PlanContext::new(paths.home.clone(), attic.clone(), attic_dev)
        .with_sources(source_facts(&req.targets)?)
        .missing_requires(crate::requires::missing(&req.requires)?)
        .verify_failed(hypr.as_ref().and_then(|o| o.failure()))
        .retiring(req.retire.clone())
        .strictly(
            req.strict,
            drift.clone(),
            hypr.as_ref().and_then(|o| o.not_checked()),
        );
    let plan = plan::plan(&observed, &req.targets, &ctx);

    let header =
        render::switch_header(req, &observed, hypr.as_ref(), drift.as_ref(), &plan, commit);

    // A decline is the complete list of reasons and a non-zero exit, with zero
    // side effects (R4). It is not an `Err` because the itemised list is the
    // answer, and an error message is one line.
    if let Plan::Decline { .. } = &plan {
        return Ok(Outcome {
            output: Output {
                text: header,
                code: ExitCode::Refused,
            },
            ended: Ended::Declined,
        });
    }
    let Plan::Apply { ops } = &plan else {
        // NoOp: reality already matches. Nothing to journal, nothing to
        // record, and no new generation — a generation per no-op switch would
        // make `rollback` step through states the machine was never in.
        return Ok(Outcome {
            output: Output {
                text: header,
                code: ExitCode::Ok,
            },
            ended: Ended::NothingToDo,
        });
    };

    if !commit {
        return Ok(Outcome {
            output: Output {
                text: header + &render::uncommitted(hypr.as_ref()),
                code: ExitCode::Ok,
            },
            ended: Ended::DryRun,
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
    let journal_done = journal::mark_done(&paths.journal_path(), &id)?;

    let output = Output {
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
    };
    Ok(Outcome {
        output,
        ended: Ended::Completed(Box::new(Completed {
            lock,
            paths: paths.clone(),
            id,
            generation: new_id,
            back_to,
            targets: req.targets.clone(),
            retired: req.retire.clone(),
            rescue: script,
            journal_done,
        })),
    })
}
