//! Read-only health report. Never fixes anything; prints the exact command a
//! human would run, including the ones ricepilot is forbidden to run itself
//! (`snapper -c home create-config /home`, the `hypr-session` `SESSION_DIR`
//! patch).
//!
//! Implemented in M5.

pub fn run() -> crate::Result<()> {
    todo!("M5")
}
