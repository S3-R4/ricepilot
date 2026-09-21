//! `adopt --commit` that really dies in the middle of itself.
//!
//! The same pattern as `examples/crash_cli_switch`, pointed at the command
//! that is genuinely dangerous. `adopt` has a window `switch` does not: the
//! user's **real directory** is sitting at a staging name, and `recover` has
//! to resolve that to fully-old or fully-new without a delete and without
//! guessing.
//!
//! `std::process::abort()`, not `panic!`: a panic unwinds, runs destructors
//! and flushes, and a crash does none of those.
//!
//! The confirmation is not bypassed here either (D47). This process reads
//! its answer from stdin exactly as the binary does, and the test feeds it
//! one — so what crashes is the whole command, prompt included.

#[path = "../tests/common/mod.rs"]
mod common;

use common::adopting;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, case, k] = <[String; 3]>::try_from(args)
        .unwrap_or_else(|a| panic!("usage: crash_cli_adopt <case> <k>, got {a:?}"));
    let k: usize = k.parse().expect("k must be a number");

    let m = adopting::build(&case);
    // Before `Paths`, not after: `Paths::rooted_at` reads these variables,
    // and a process that built its paths first would fail on `lock_path()`
    // before doing anything interesting.
    for (key, value) in m.f.env() {
        std::env::set_var(key, value);
    }
    let paths = ricepilot::cli::paths::Paths::from_env().expect("fixture paths");

    let _ = ricepilot::cli::adopt::run_with(&paths, &m.dest, "mine", true, &mut |i| {
        if i == k {
            std::process::abort();
        }
        Ok(())
    });

    // Reaching here means k was past the end of the plan. Abort anyway, so
    // the caller's "it was supposed to abort" assertion stays meaningful.
    std::process::abort();
}
