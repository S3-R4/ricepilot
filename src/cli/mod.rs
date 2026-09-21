//! `clap` surface. Every mutating subcommand is dry-run by default and
//! requires `--commit` (`SAFETY.md` R4). Every user-visible string is
//! snapshot-tested.
//!
//! Implemented in M1 (read-only commands) and M5 (the rest).

pub mod paths;
pub mod render;
pub mod switch;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::{Error, Result};

#[derive(Debug, Parser)]
#[command(name = "ricepilot", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Register the live rice by reference and take a baseline copy.
    Init {
        #[arg(long)]
        commit: bool,
    },
    /// Show the current generation, owned links and any drift.
    Status,
    /// Read-only health report; prints commands, never runs them.
    Doctor,
    /// List registered profiles.
    List,
    /// Show one profile's manifest as ricepilot understands it.
    Show { profile: String },
    /// Copy a live directory into a new profile. Does not activate it.
    Capture {
        profile: String,
        #[arg(long)]
        commit: bool,
    },
    /// Convert a real user directory into a profile-owned dir-link.
    Adopt {
        path: std::path::PathBuf,
        #[arg(long)]
        into: String,
        #[arg(long)]
        commit: bool,
    },
    /// Print the plan for switching to a profile. Never mutates.
    Plan { profile: String },
    /// Switch profiles. Dry-run unless `--commit`.
    Switch {
        profile: String,
        #[arg(long)]
        commit: bool,
        /// Run `uwsm stop` after a y/N confirmation.
        #[arg(long)]
        relogin: bool,
        /// Treat volatile-path drift as an error instead of a report.
        #[arg(long)]
        strict: bool,
    },
    /// Re-apply the previous generation through the ordinary switch path.
    Rollback {
        #[arg(long)]
        commit: bool,
    },
    /// Finish or undo an interrupted switch by observing reality.
    Recover {
        #[arg(long)]
        commit: bool,
    },
    /// Print the path to the standalone TTY-safe rescue script.
    Rescue,
    /// Check a profile against its blake3 manifest.
    Verify { profile: String },
    /// Diff a profile against the live filesystem.
    Diff { profile: String },
    /// Itemise attic directories and remove one after typed confirmation.
    Gc {
        #[arg(long)]
        commit: bool,
    },
}

/// What a command produced: its whole output, and the status it exits with.
///
/// The two are separate because a command can succeed at its job and still
/// have something to report that a script must be able to notice. `verify`
/// is the case: it ran correctly, it printed what it found, and what it found
/// was drift. Folding that into an `Err` would put the itemised report on
/// stderr and reduce it to one line; folding it into exit 0 would make a
/// check that a script cannot act on (D34).
pub struct Output {
    pub text: String,
    pub code: crate::error::ExitCode,
}

impl From<String> for Output {
    fn from(text: String) -> Self {
        Output {
            text,
            code: crate::error::ExitCode::Ok,
        }
    }
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(out) => {
            print!("{}", out.text);
            ExitCode::from(out.code as u8)
        }
        Err(e) => {
            eprintln!("ricepilot: {e}");
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

/// Dispatch. Commands return their whole output rather than printing as they
/// go, so a command that fails half way through cannot have already printed
/// half an answer.
pub fn run(command: Command) -> Result<Output> {
    let paths = paths::Paths::from_env()?;
    match command {
        Command::List => cmd_list(&paths).map(Output::from),
        Command::Show { profile } => cmd_show(&paths, &profile).map(Output::from),
        Command::Status => cmd_status(&paths).map(Output::from),
        Command::Plan { profile } => cmd_plan(&paths, &profile).map(Output::from),
        Command::Recover { commit } => cmd_recover(&paths, commit).map(Output::from),
        Command::Verify { profile } => cmd_verify(&paths, &profile),
        Command::Rescue => cmd_rescue(&paths).map(Output::from),
        Command::Rollback { commit } => cmd_rollback(&paths, commit),
        Command::Switch {
            profile,
            commit,
            relogin,
            strict,
        } => cmd_switch(&paths, &profile, commit, relogin, strict),

        // Mutating commands and the remaining read-only ones arrive in later
        // milestones. Saying so and exiting non-zero is the honest answer;
        // a stub that silently did nothing would be worse than an error.
        other => Err(Error::NotPossible {
            anchor: "not-yet-implemented",
            why: format!(
                "`{}` is not implemented yet; M1 ships the read-only commands \
                 plan, status, list and show, M2 adds recover, and M3 adds switch, \
                 rollback, verify and rescue",
                subcommand_name(&other)
            ),
        }),
    }
}

fn subcommand_name(c: &Command) -> &'static str {
    match c {
        Command::Init { .. } => "init",
        Command::Status => "status",
        Command::Doctor => "doctor",
        Command::List => "list",
        Command::Show { .. } => "show",
        Command::Capture { .. } => "capture",
        Command::Adopt { .. } => "adopt",
        Command::Plan { .. } => "plan",
        Command::Switch { .. } => "switch",
        Command::Rollback { .. } => "rollback",
        Command::Recover { .. } => "recover",
        Command::Rescue => "rescue",
        Command::Verify { .. } => "verify",
        Command::Diff { .. } => "diff",
        Command::Gc { .. } => "gc",
    }
}

/// `ricepilot recover`.
///
/// The lock is taken even for the dry run. Reading a half-finished switch
/// while another ricepilot is in the middle of making it would produce a
/// report about a machine that no longer exists by the time it is printed, and
/// a dry run whose answer is stale is worse than one that declines.
fn cmd_recover(paths: &paths::Paths, commit: bool) -> Result<String> {
    let _lock = crate::ops::lock::acquire(&paths.lock_path()?)?;

    let journal_path = paths.journal_path();
    let Some(journal) = crate::journal::read_current(&journal_path)? else {
        return Ok(render::NOTHING_TO_RECOVER.to_string());
    };

    let recovery = crate::journal::plan_recovery(&journal)?;
    if !commit {
        return Ok(render::recover(&recovery, false));
    }

    crate::journal::execute(&recovery, &journal_path, journal.mode()?, &journal.attic)?;
    Ok(render::recover(&recovery, true))
}

fn cmd_list(paths: &paths::Paths) -> Result<String> {
    let profiles = paths::load_all(paths)?;
    Ok(render::list(&profiles, &paths.home))
}

fn cmd_show(paths: &paths::Paths, name: &str) -> Result<String> {
    let profile = paths::load(paths, name)?;
    let root = profile.root(&paths.home);
    Ok(render::show(
        &profile.name,
        &profile.manifest,
        &root,
        &paths.home,
    ))
}

fn cmd_status(paths: &paths::Paths) -> Result<String> {
    let profiles = paths::load_all(paths)?;
    let ledger = crate::ledger::load(&paths.ledger_path())?;
    let generation = match crate::generations::current(&paths.state)? {
        Some(id) => Some(crate::generations::load(&paths.state, id)?),
        None => None,
    };
    Ok(render::status(
        paths,
        &profiles,
        &ledger,
        generation.as_ref(),
    ))
}

fn cmd_plan(paths: &paths::Paths, name: &str) -> Result<String> {
    let profile = paths::load(paths, name)?;
    let targets = profile.manifest.targets(&profile.dir, &paths.home);
    let dests: Vec<std::path::PathBuf> = targets.iter().map(|t| t.dest.clone()).collect();

    // Every registered profile's root counts, not just this one's: a link
    // pointing into the profile we are switching *away* from is still one of
    // ours, and must not be misread as foreign.
    let all = paths::load_all(paths)?;
    let ledger = crate::ledger::load(&paths.ledger_path())?;
    let ownership = ledger.ownership(all.iter().map(|p| p.root(&paths.home)).collect());

    let observed = crate::observe::observe(&dests, &ownership)?;
    let attic = paths.attic_dir();
    let attic_dev = crate::ops::read::dev_of_nearest_existing_ancestor(&attic)?;
    let ctx = crate::plan::PlanContext::new(paths.home.clone(), attic, attic_dev);
    let plan = crate::plan::plan(&observed, &targets, &ctx);

    Ok(render::plan(&profile.name, &observed, &plan))
}

/// `ricepilot verify <profile>`.
///
/// Compares the profile tree against the blake3 manifest recorded when
/// ricepilot last switched to it. It reports what it observed and never what
/// the profile manifest claims should be true (`SAFETY.md` R7).
fn cmd_verify(paths: &paths::Paths, name: &str) -> Result<Output> {
    let profile = paths::load(paths, name)?;
    let root = profile.root(&paths.home);
    let recorded_path = crate::verify::manifest_path(&paths.state, name);

    let Some(recorded) = crate::verify::load(&recorded_path)? else {
        return Err(Error::Refused {
            rule: "R7",
            path: recorded_path,
            why: format!(
                "no manifest has been recorded for `{name}` yet, so there is nothing to compare \
                 against. one is recorded each time ricepilot switches to a profile"
            ),
        });
    };

    let now = crate::verify::build(
        name,
        &root,
        &profile.manifest.volatile,
        crate::journal::timestamp_id(std::time::SystemTime::now()),
    )?;
    let diffs = crate::verify::compare(&recorded, &now);
    let drifted = diffs.iter().any(|d| d.is_substantive());

    Ok(Output {
        text: render::verify(name, &root, &recorded, &diffs),
        code: if drifted {
            crate::error::ExitCode::Drift
        } else {
            crate::error::ExitCode::Ok
        },
    })
}

/// `ricepilot rescue` — where the standalone script is, and how to run it.
///
/// It prints a path and nothing else happens. The script is regenerated by
/// `switch`, not by this command: a rescue script written on demand would be
/// written by the ricepilot on a machine that may already be in the state the
/// script exists to get out of.
fn cmd_rescue(paths: &paths::Paths) -> Result<String> {
    let p = crate::rescue::path(&paths.state);
    if crate::ops::read::lstat_or_absent(&p)?.is_none() {
        return Err(Error::Refused {
            rule: "R7",
            path: p,
            why: "there is no rescue script yet. one is written after each successful switch, \
                  and until a switch has happened there is no previous generation for it to \
                  restore"
                .into(),
        });
    }
    Ok(render::rescue(&p))
}

/// `ricepilot switch <profile> [--commit]`.
///
/// The target state comes from the profile manifest; the destinations to
/// retire come from the ledger — every path ricepilot owns that this profile
/// does not claim (D36). Everything after that is [`switch::run`], which
/// `rollback` shares.
fn cmd_switch(
    paths: &paths::Paths,
    name: &str,
    commit: bool,
    relogin: bool,
    strict: bool,
) -> Result<Output> {
    // A flag that is accepted and quietly ignored is worse than one that is
    // refused: the user asked for something and was told nothing.
    if relogin {
        return Err(Error::NotPossible {
            anchor: "not-yet-implemented",
            why: "`--relogin` runs `uwsm stop`, which needs the subprocess allowlist in \
                  `ops::exec`; that is M5. the switch itself works — run it without the flag \
                  and log out yourself"
                .into(),
        });
    }
    if strict {
        return Err(Error::NotPossible {
            anchor: "not-yet-implemented",
            why: "`--strict` turns volatile-path drift into a refusal, and drift reporting at \
                  switch time is M5. `ricepilot verify` compares a profile against its \
                  recorded manifest today"
                .into(),
        });
    }

    let profile = paths::load(paths, name)?;
    let root = profile.root(&paths.home);
    let targets = profile.manifest.targets(&profile.dir, &paths.home);

    // Anything ricepilot owns that this profile does not claim.
    let ledger = crate::ledger::load(&paths.ledger_path())?;
    let retire: Vec<std::path::PathBuf> = ledger
        .owned_dests()
        .into_iter()
        .filter(|d| !targets.iter().any(|t| &t.dest == d))
        .collect();

    let req = switch::Request {
        kind: switch::Kind::Switch,
        label: format!("to profile `{name}`"),
        profile: name.to_string(),
        targets,
        retire,
        manifest_of: Some((root, profile.manifest.volatile.clone())),
    };
    switch::run(paths, &req, commit)
}

/// `ricepilot rollback [--commit]`.
///
/// Re-applies generation `NNNN-1` through [`switch::run`] — the same lock, the
/// same pre-flight, the same journal, the same recovery story. The only thing
/// that differs from a `switch` is where the target state comes from, which is
/// why it is an argument rather than a second implementation.
fn cmd_rollback(paths: &paths::Paths, commit: bool) -> Result<Output> {
    let state = &paths.state;
    let Some(current) = crate::generations::current(state)? else {
        return Err(Error::Refused {
            rule: "R7",
            path: crate::generations::current_path(state),
            why: "there is no generation to roll back from: ricepilot has not switched anything \
                  on this machine"
                .into(),
        });
    };
    if current == 0 {
        return Err(Error::Refused {
            rule: "R7",
            path: crate::generations::path(state, 0),
            why: "generation 0000 is the state ricepilot found before it switched anything. \
                  there is nothing before it to go back to"
                .into(),
        });
    }

    let previous = current - 1;
    let to = crate::generations::load(state, previous)?;
    let targets = to.targets();

    // Everything ricepilot owns that generation NNNN-1 did not have a link at.
    // These are displaced into the attic rather than removed (D36) — including
    // a link the forward switch created at a destination that was absent,
    // which is the case D21 left open.
    let ledger = crate::ledger::load(&paths.ledger_path())?;
    let retire: Vec<std::path::PathBuf> = ledger
        .owned_dests()
        .into_iter()
        .filter(|d| !targets.iter().any(|t| &t.dest == d))
        .collect();

    // Generation 0000 belongs to no profile ricepilot registered, so there is
    // no single tree to hash. Recording a manifest of "wherever those links
    // happen to point" would be a manifest of nothing in particular.
    let manifest_of = paths::load(paths, &to.profile)
        .ok()
        .map(|p| (p.root(&paths.home), p.manifest.volatile.clone()));

    let req = switch::Request {
        kind: switch::Kind::Rollback,
        label: format!("to generation {previous:04} (`{}`)", to.profile),
        profile: to.profile.clone(),
        targets,
        retire,
        manifest_of,
    };
    switch::run(paths, &req, commit)
}
