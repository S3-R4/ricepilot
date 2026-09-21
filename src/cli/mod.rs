//! `clap` surface. Every mutating subcommand is dry-run by default and
//! requires `--commit` (`SAFETY.md` R4). Every user-visible string is
//! snapshot-tested.
//!
//! Implemented in M1 (read-only commands) and M5 (the rest).

pub mod paths;
pub mod render;

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

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(out) => {
            print!("{out}");
            ExitCode::from(crate::error::ExitCode::Ok as u8)
        }
        Err(e) => {
            eprintln!("ricepilot: {e}");
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

/// Dispatch. Commands return their whole output as a `String` rather than
/// printing as they go, so a command that fails half way through cannot have
/// already printed half an answer.
pub fn run(command: Command) -> Result<String> {
    let paths = paths::Paths::from_env()?;
    match command {
        Command::List => cmd_list(&paths),
        Command::Show { profile } => cmd_show(&paths, &profile),
        Command::Status => cmd_status(&paths),
        Command::Plan { profile } => cmd_plan(&paths, &profile),
        Command::Recover { commit } => cmd_recover(&paths, commit),

        // Mutating commands and the remaining read-only ones arrive in later
        // milestones. Saying so and exiting non-zero is the honest answer;
        // a stub that silently did nothing would be worse than an error.
        other => Err(Error::NotPossible {
            anchor: "not-yet-implemented",
            why: format!(
                "`{}` is not implemented yet; M1 ships the read-only commands \
                 plan, status, list and show, and M2 adds recover",
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
    let ledger_present = crate::ops::read::lstat(&paths.ledger_path())?.is_some();
    Ok(render::status(paths, &profiles, ledger_present))
}

fn cmd_plan(paths: &paths::Paths, name: &str) -> Result<String> {
    let profile = paths::load(paths, name)?;
    let targets = profile.manifest.targets(&profile.dir, &paths.home);
    let dests: Vec<std::path::PathBuf> = targets.iter().map(|t| t.dest.clone()).collect();

    // Every registered profile's root counts, not just this one's: a link
    // pointing into the profile we are switching *away* from is still one of
    // ours, and must not be misread as foreign.
    let all = paths::load_all(paths)?;
    let ownership = crate::observe::Ownership {
        profile_roots: all.iter().map(|p| p.root(&paths.home)).collect(),
        // M3 fills this from state/ledger.toml. Until then the predicate's
        // third fact can never hold, which is why NO_LEDGER_NOTE is printed.
        entries: Vec::new(),
    };

    let observed = crate::observe::observe(&dests, &ownership)?;
    let attic = paths.attic_dir();
    let attic_dev = crate::ops::read::dev_of_nearest_existing_ancestor(&attic)?;
    let ctx = crate::plan::PlanContext::new(paths.home.clone(), attic, attic_dev);
    let plan = crate::plan::plan(&observed, &targets, &ctx);

    let mut out = render::plan(&profile.name, &observed, &plan);

    // The note explains why a link that looks right is called foreign. It is
    // only printed when that actually happened, so it never contradicts a
    // plan that has no foreign link in it.
    let ledger_present = crate::ops::read::lstat(&paths.ledger_path())?.is_some();
    let any_foreign = observed
        .iter()
        .any(|o| matches!(o.shape, crate::observe::Shape::ForeignLink { .. }));
    if !ledger_present && any_foreign {
        out.push_str(render::NO_LEDGER_NOTE);
    }
    Ok(out)
}
