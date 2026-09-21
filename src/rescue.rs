//! Regenerates `state/rescue.sh` after every successful switch: a fully
//! unrolled POSIX `sh` script with absolute binary paths, no loops, no
//! variables, no ricepilot. It restores the previous generation using only
//! `ln -sT tmp && mv -T`, is checked with `sh -n` before being written, and is
//! written atomically as a real file (never a symlink) so it survives a
//! profile switch that goes wrong.
//!
//! It must run from a TTY with no D-Bus, no hyprctl, no fish, no quickshell.
//!
//! Implemented in M3.

pub fn regenerate() -> crate::Result<()> {
    todo!("M3")
}
