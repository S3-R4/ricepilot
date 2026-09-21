//! `clap` surface. Every mutating subcommand is dry-run by default and
//! requires `--commit` (`SAFETY.md` R4). Every user-visible string is
//! snapshot-tested.
//!
//! Implemented in M1 (read-only commands) and M5 (the rest).

use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
    let _cli = Cli::parse();
    eprintln!("ricepilot: not implemented yet (milestone M0: scaffold only)");
    ExitCode::from(3)
}
