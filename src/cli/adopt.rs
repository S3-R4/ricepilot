//! `ricepilot adopt <path> --into <profile>`
//!
//! The riskiest command in the tool: the only one that turns a real user
//! directory into a symlink. It is treated accordingly — one path at a time,
//! confirmed by a human, hash-verified before anything moves, journalled
//! before the first live effect, and the displaced directory is **moved** to
//! the attic rather than removed.
//!
//! The order is what makes it recoverable:
//!
//! 1. Observe, plan (`plan::plan_adopt`, pure), print, and stop unless
//!    `--commit`.
//! 2. Ask. No answer, or any answer but yes, means nothing happens.
//! 3. Copy the directory into the profile and compare the copy against the
//!    original with a blake3 manifest. This is before the journal, and
//!    deliberately: it is a write into ricepilot's own data directory rather
//!    than a mutation of the live machine, and the link the journal describes
//!    cannot be journalled against a source that does not exist yet (D48).
//! 4. Add the path to the profile's manifest, so the ledger row this
//!    creates and the manifest a later `switch` reads agree about who owns
//!    it. Without it, the very next `switch <profile>` would see a ledger
//!    entry the target state does not include and *retire* the link adopt
//!    just made (D51).
//! 5. Write and fsync the journal — the file and its directory — naming the
//!    directory's `(dev, ino)`, the staging name and the attic slot.
//! 6. Stage the link, exchange it with the directory, move the directory to
//!    the attic, fsync.
//! 7. Record the ledger row and the generation, regenerate `rescue.sh`, and
//!    retire the journal last (D25).
//!
//! A crash anywhere from step 5 on is resolved by `recover`, which reads the
//! filesystem and decides forward or abandon — including the window in which
//! the user's real directory is sitting at a staging name.

use std::path::PathBuf;

use crate::error::ExitCode;
use crate::generations::{self, Generation};
use crate::journal::{self, Journal};
use crate::ops::{lock, mutate, read};
use crate::plan::{self, Plan, Target};
use crate::{ledger, observe, rescue, survey, verify, Error, Result};

use super::paths::Paths;
use super::{confirm, render, Output};

/// Everything phase A worked out, without changing anything.
pub struct Adoption {
    /// The live path being adopted.
    pub dest: PathBuf,
    pub profile: String,
    /// The name the copy takes inside the profile.
    pub leaf: String,
    /// `<profile>/<leaf>`, which the new link will point at.
    pub new_target: PathBuf,
    pub observed: observe::Observed,
    /// The `(dev, ino)` of the directory being adopted, as **phase A** saw
    /// it. This is what the journal records and what recovery matches
    /// against (D46), and it is re-checked immediately before the journal is
    /// written: if an installer replaced the directory in between, the thing
    /// about to be displaced is not the thing that was confirmed.
    pub dir_id: Option<(u64, u64)>,
    /// What is in the directory, when there is one to copy.
    pub survey: Option<survey::Survey>,
    pub plan: Plan,
    /// Whether step 3 has anything to do. False when the destination is
    /// absent and the profile already holds the source.
    pub copies: bool,
    pub attic: PathBuf,
}

/// Phase A. Read-only.
pub fn plan_it(paths: &Paths, path: &std::path::Path, into: &str, id: &str) -> Result<Adoption> {
    let profile = super::paths::load(paths, into)?;

    // R3: a by-reference profile's root is a directory ricepilot does not
    // own and never writes to — typically the user's live rice clone. Copying
    // into one would be ricepilot writing into somebody else's git worktree.
    if profile.manifest.root.is_some() {
        return Err(Error::Refused {
            rule: "R3",
            path: profile.root(&paths.home),
            why: format!(
                "profile `{into}` is by-reference: its root is a directory ricepilot does not \
                 own and never writes to. adopt into a profile with its own payload, or \
                 `ricepilot capture` the directory into a new one"
            ),
        });
    }

    let dest = crate::manifest::expand_home(path, &paths.home);
    if !dest.is_absolute() {
        return Err(Error::Refused {
            rule: "R1",
            path: dest,
            why: "must be an absolute path or start with `~`".into(),
        });
    }
    let leaf = dest
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .ok_or_else(|| Error::Refused {
            rule: "R4",
            path: dest.clone(),
            why: "has no final path component to name it by inside the profile".into(),
        })?;
    let new_target = profile.dir.join(&leaf);

    // Every registered profile's root counts towards ownership, not just the
    // one being adopted into — the same reason `plan` loads them all.
    let all = super::paths::load_all(paths)?;
    let led = ledger::load(&paths.ledger_path())?;
    let ownership = led.ownership(all.iter().map(|p| p.root(&paths.home)).collect());
    let observed = observe::observe_one(&dest, &ownership)?;

    let target = Target {
        dest: dest.clone(),
        src: new_target.clone(),
    };
    let attic = paths.attic_dir().join(id);
    let attic_dev = read::dev_of_nearest_existing_ancestor(&paths.attic_dir())?;
    let ctx = plan::PlanContext::new(paths.home.clone(), attic.clone(), attic_dev)
        .with_sources(super::switch::source_facts(std::slice::from_ref(&target))?);
    let plan = plan::plan_adopt(&observed, &target, &ctx);

    let copies = observed.shape == observe::Shape::RealDir;
    let survey = if copies {
        Some(survey::survey(&dest)?)
    } else {
        None
    };
    let dir_id = read::lstat(&dest)?
        .filter(|m| m.kind == read::Kind::Dir)
        .map(|m| (m.dev, m.ino));

    Ok(Adoption {
        dest,
        profile: into.to_string(),
        leaf,
        new_target,
        observed,
        dir_id,
        survey,
        plan,
        copies,
        attic,
    })
}

/// `ricepilot adopt`. Dry-run unless `commit`.
pub fn run(paths: &Paths, path: &std::path::Path, into: &str, commit: bool) -> Result<Output> {
    run_with(paths, path, into, commit, &mut |_| Ok(()))
}

/// [`run`], with a hook called after each op has completed.
///
/// The crash-injection harness ends the process after step *k* for every *k*
/// against **this** function rather than against `mutate::apply`, so what is
/// proven recoverable is the command a user runs and not a subset of it —
/// the same shape and the same reason as `switch::run_with` (D26).
pub fn run_with(
    paths: &Paths,
    path: &std::path::Path,
    into: &str,
    commit: bool,
    after_step: &mut dyn FnMut(usize) -> Result<()>,
) -> Result<Output> {
    let _lock = lock::acquire(&paths.lock_path()?)?;

    if journal::read_current(&paths.journal_path())?.is_some() {
        return Err(Error::Refused {
            rule: "R4",
            path: paths.journal_path(),
            why: "an operation is already in flight and did not finish. run `ricepilot recover` \
                  first; until then ricepilot will not plan against a machine that is half way \
                  through something else"
                .into(),
        });
    }

    let id = journal::unique_id(
        &paths.state,
        &journal::timestamp_id(std::time::SystemTime::now()),
    )?;
    let a = plan_it(paths, path, into, &id)?;
    let header = render::adopt_header(&a, commit);

    let ops = match &a.plan {
        Plan::Decline { .. } => {
            return Ok(Output {
                text: header,
                code: ExitCode::Refused,
            })
        }
        Plan::NoOp => {
            return Ok(Output {
                text: header,
                code: ExitCode::Ok,
            })
        }
        Plan::Apply { ops } => ops.clone(),
    };

    if !commit {
        return Ok(Output {
            text: header + render::ADOPT_UNCOMMITTED,
            code: ExitCode::Ok,
        });
    }

    // ---- R6. A human says yes to this path, or nothing happens to it. ----
    print!("{header}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    if !confirm::ask(&render::adopt_question(&a)) {
        return Err(Error::Refused {
            rule: "R6",
            path: a.dest.clone(),
            why: "not confirmed, so nothing was touched. adopt is the one command that turns a \
                  real directory into a link, and it does that only for a path you say yes to"
                .into(),
        });
    }

    // ---- Step 3: the copy, and the proof that it is one. ----
    let mut stats = None;
    if a.copies {
        let s = mutate::copy_tree(&a.dest, &a.new_target)?;
        let before = verify::build(&a.profile, &a.dest, &[], &id)?;
        let after = verify::build(&a.profile, &a.new_target, &[], &id)?;
        let diffs = verify::compare(&before, &after);
        if !diffs.is_empty() {
            return Err(Error::Refused {
                rule: "R7",
                path: a.new_target.clone(),
                why: format!(
                    "the copy does not hash the same as {} ({}). nothing on the live machine \
                     has been touched: the directory is still a directory, and the copy is \
                     still where it was written, because ricepilot removes nothing",
                    a.dest.display(),
                    diffs
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            });
        }
        stats = Some(s);
    }

    // ---- Step 4: the manifest. Before the journal, because a crash here
    // leaves a manifest declaring a destination that is still a real
    // directory — which `plan` reads as shape 3 and refuses, honestly. A
    // crash the other way round would leave a ledger row and a link that no
    // manifest claims, and the next `switch` would retire it (D51).
    let manifest_path = paths.manifest_path(&a.profile);
    let updated = declare_path(&read::slurp(&manifest_path)?, &a, &paths.home)?;
    mutate::write_atomic(&manifest_path, updated.as_bytes())?;

    // ---- Steps 5 onward. From here a crash is `recover`'s problem. ----
    mutate::make_dirs(&paths.state)?;
    let mode = mutate::probe_exchange(&paths.state)?;
    mutate::make_dirs(&a.attic)?;

    // Generation 0000 — the topology before ricepilot changed anything — is
    // written while it is still true, exactly as `switch` does (D35).
    let previous = generations::current(&paths.state)?;
    let mut known: Vec<PathBuf> = ledger::load(&paths.ledger_path())?.owned_dests();
    if !known.contains(&a.dest) {
        known.push(a.dest.clone());
    }
    let new_id = match previous {
        Some(n) => n + 1,
        None => {
            let pre = Generation::observe(0, generations::PRE_EXISTING, &id, &known)?;
            generations::save(&paths.state, &pre)?;
            1
        }
    };

    let (dir_dev, dir_ino) = a.still_the_same_directory()?;
    let record = journal::Adopt {
        dest: a.dest.clone(),
        dir_dev,
        dir_ino,
        staged: plan::adopt_staged_name(&a.dest),
        new_target: a.new_target.clone(),
        attic_rel: plan::adopt_attic_rel(&a.dest),
    };
    let j = Journal::for_adopt(&id, &a.profile, a.attic.clone(), mode, record, &ops);
    journal::write(&paths.journal_path(), &j)?;

    let ctx = mutate::ApplyContext {
        mode,
        attic: a.attic.clone(),
    };
    mutate::apply(&ops, &ctx, after_step)?;

    // The POST record, read off the disk rather than assumed (R7).
    let after = Generation::observe(new_id, &a.profile, &id, &known)?;
    generations::save(&paths.state, &after)?;
    generations::set_current(&paths.state, new_id)?;

    let mut led = ledger::load(&paths.ledger_path())?;
    led.record(std::slice::from_ref(&a.dest), &a.profile)?;
    ledger::save(&paths.ledger_path(), &led)?;

    let back_to = generations::load(&paths.state, new_id - 1)?;
    let script = rescue::regenerate(&paths.state, &back_to)?;

    journal::mark_done(&paths.journal_path(), &id)?;

    Ok(Output {
        text: render::adopt_done(&a, stats.as_ref(), new_id, &script),
        code: ExitCode::Ok,
    })
}

/// Append a `[[path]]` block for this adoption to a profile's manifest.
///
/// Appended as text rather than re-serialised from the parsed value: the
/// file belongs to the user, who may have edited it, added comments or
/// ordered it to taste, and round-tripping it through a serialiser would
/// quietly throw all of that away. A new `[[path]]` at the end of the file
/// is always valid — anything trailing already belongs to the last table.
///
/// The result is parsed before it is returned, so a manifest ricepilot could
/// not read is never one it writes. That is also what catches a destination
/// the manifest already declares: `manifest::validate` refuses a duplicate
/// `dest`, and its message names it.
fn declare_path(existing: &str, a: &Adoption, home: &std::path::Path) -> Result<String> {
    let mut s = existing.to_string();
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(&format!(
        "\n# added by `ricepilot adopt`.\n[[path]]\ndest       = \"{}\"\nsrc        = \
         \"{}\"\nkind       = \"dir-link\"\nactivation = \"relogin\"\n",
        super::capture::tildify(&a.dest, home),
        a.leaf
    ));
    crate::manifest::parse(&s)?;
    Ok(s)
}

impl Adoption {
    /// The identity phase A observed, re-checked now.
    ///
    /// Between the plan and this point the user answered a question and a
    /// tree was copied, which on a machine with a rice installer on it is
    /// long enough for `~/.config/hypr` to have become a different
    /// directory. Journalling the old identity would make recovery refuse a
    /// state it caused; journalling the new one would displace something
    /// nobody confirmed. So it refuses, before the journal and before any
    /// live effect.
    ///
    /// For a destination that was *absent* there is no directory and nothing
    /// to match: the adopt is a single `symlinkat` with no window (D11), and
    /// zero is recorded.
    fn still_the_same_directory(&self) -> Result<(u64, u64)> {
        let Some(was) = self.dir_id else {
            return Ok((0, 0));
        };
        let now = read::lstat(&self.dest)?
            .filter(|m| m.kind == read::Kind::Dir)
            .map(|m| (m.dev, m.ino));
        if now == Some(was) {
            return Ok(was);
        }
        Err(Error::Refused {
            rule: "R4",
            path: self.dest.clone(),
            why: format!(
                "is no longer the directory ricepilot looked at a moment ago (inode {} then, \
                 {} now). something replaced it while this command was running, and ricepilot \
                 will not displace a directory nobody confirmed. nothing on the live machine \
                 has been touched; the copy already made is at {}",
                was.1,
                match now {
                    Some((_, ino)) => ino.to_string(),
                    None => "nothing there".to_string(),
                },
                self.new_target.display()
            ),
        })
    }
}
