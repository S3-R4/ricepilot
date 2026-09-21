//! Subprocess invocation. Lives inside the `ops` boundary for the same reason
//! the filesystem calls do: every escape from this process into the outside
//! world should be visible in one directory.
//!
//! The allowlist is closed and each entry is read-only or user-initiated:
//! `sh -n` (parse a script, never run it), `pacman -Q` (query, never install),
//! `hyprctl` (query / `submap reset`), `Hyprland --verify-config` (sandboxed
//! scratch copy only), `uwsm stop` (only behind `--relogin` and a y/N),
//! `git status` (read-only, reporting).
//!
//! `sudo`, `caelestia`, `install.fish`, `paru -S`, `rm`, `git stash|checkout|
//! clean|reset` and anything writing under `/etc` are refused here, not left
//! to caller discipline (`SAFETY.md` R3).
//!
//! `sh -n` landed in M3 because [`crate::rescue`] must not write a script it
//! has not had parsed. The rest is M5.

use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::{Error, Result};

/// A subprocess ricepilot is permitted to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    ShSyntaxCheck,
    PacmanQuery,
    Hyprctl,
    HyprlandVerifyConfig,
    UwsmStop,
    GitStatus,
}

/// Where `sh -n` is. `/bin/sh` is the one path POSIX itself names, and the
/// rescue script is the thing that has to work when nothing else does, so it
/// is not looked up through `PATH`.
pub const SH: &str = "/bin/sh";

/// Parse `script` with `sh -n` without running a line of it.
///
/// The script is fed on **stdin** rather than written to a temp file. A temp
/// file would have to be tidied away afterwards, and tidying away is a
/// removal, which exists nowhere outside `src/gc/` (R2). Stdin has no such
/// problem and `sh -n` reads it happily.
///
/// `-n` means "read commands and check for syntax errors, but do not execute".
/// It is the only mode this function will ever invoke `sh` in.
pub fn sh_syntax_check(script: &str) -> Result<()> {
    let mut child = Command::new(SH)
        .arg("-n")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::Io {
            context: format!("running {SH} -n to check a generated script"),
            source,
        })?;

    child
        .stdin
        .take()
        .ok_or_else(|| Error::Io {
            context: format!("{SH} -n accepted no standard input"),
            source: std::io::Error::other("no stdin pipe"),
        })?
        .write_all(script.as_bytes())
        .map_err(|source| Error::Io {
            context: format!("feeding a generated script to {SH} -n"),
            source,
        })?;

    let out = child.wait_with_output().map_err(|source| Error::Io {
        context: format!("waiting for {SH} -n"),
        source,
    })?;

    if out.status.success() {
        return Ok(());
    }
    Err(Error::Refused {
        rule: "R4",
        path: std::path::PathBuf::from(SH),
        why: format!(
            "the generated rescue script did not parse, so it was not written: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    })
}

pub fn run(_what: Allowed, _args: &[&str]) -> crate::Result<String> {
    todo!("M5")
}
