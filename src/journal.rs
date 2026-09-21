//! Write-ahead journal. Written and fsync'd (file *and* containing directory)
//! before the first exchange, so a crash at any point is recoverable.
//!
//! Replay is **state-driven, not step-driven**: `recover` observes reality and
//! decides, per destination, whether it is old or new — it never blindly
//! re-runs recorded steps. The M2 gate kills the process after step k for
//! every k and asserts the result is fully old or fully new, never mixed.
//!
//! Implemented in M2.

pub fn write(_plan: &crate::plan::Plan) -> crate::Result<()> {
    todo!("M2")
}

pub fn replay() -> crate::Result<()> {
    todo!("M2")
}
