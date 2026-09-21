//! Subprocess invocation. Lives inside the `ops` boundary for the same reason
//! the filesystem calls do: every escape from this process into the outside
//! world should be visible in one directory.
//!
//! The allowlist is closed and each entry is read-only or user-initiated:
//! `pacman -Q` (query, never install), `hyprctl` (query / `submap reset`),
//! `Hyprland --verify-config` (sandboxed scratch copy only), `uwsm stop`
//! (only behind `--relogin` and a y/N), `git status` (read-only, reporting).
//!
//! `sudo`, `caelestia`, `install.fish`, `paru -S`, `rm`, `git stash|checkout|
//! clean|reset` and anything writing under `/etc` are refused here, not left
//! to caller discipline (`SAFETY.md` R3).
//!
//! Implemented in M5.

/// A subprocess ricepilot is permitted to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    PacmanQuery,
    Hyprctl,
    HyprlandVerifyConfig,
    UwsmStop,
    GitStatus,
}

pub fn run(_what: Allowed, _args: &[&str]) -> crate::Result<String> {
    todo!("M5")
}
