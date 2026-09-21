//! A switch that really dies in the middle of itself.
//!
//! The in-process crash injection in `tests/crash_injection.rs` proves that
//! recovery handles every intermediate state. It cannot prove that the journal
//! was on the disk before the state it describes existed, because the process
//! that wrote it is the process doing the asserting.
//!
//! This helper closes that gap: it builds the fixture, writes and fsyncs the
//! journal, applies the plan, and calls `std::process::abort()` after step *k*.
//! Nothing it held in memory survives. The test then recovers from what is
//! actually on the disk.
//!
//! It lives in `examples/` rather than `src/bin/` on purpose: the guard scripts
//! scan `src/`, and a helper that could abort a switch has no business being
//! inside the boundary they protect — nor inside anything `cargo install`
//! would put on a machine.

#[path = "../tests/common/mod.rs"]
mod common;

use common::scenario;
use ricepilot::journal;
use ricepilot::ops::mutate::{self, ExchangeMode};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, case, mode, k] = <[String; 4]>::try_from(args).unwrap_or_else(|a| {
        panic!("usage: crash_switch <case> <renameat2|fallback> <k>, got {a:?}")
    });
    let mode = ExchangeMode::parse(&mode).expect("unknown exchange mode");
    let k: usize = k.parse().expect("k must be a number");

    let s = scenario::build(&case, mode);

    // R4: the journal is written and made durable — the file and the directory
    // holding it — before the first effect.
    journal::write(&s.journal_path(), &s.journal).expect("journal");

    let _ = mutate::apply(&s.ops, &s.apply_ctx(), &mut |i| {
        if i == k {
            // Not a panic: a panic unwinds, runs destructors and flushes. A
            // crash does none of those, and the point of this helper is to be
            // a crash.
            std::process::abort();
        }
        Ok(())
    });

    // Reaching here means k was past the end of the plan. Abort anyway, so the
    // caller's "it was supposed to abort" assertion stays meaningful.
    std::process::abort();
}
