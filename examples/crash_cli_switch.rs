//! `switch --commit` that really dies in the middle of itself.
//!
//! `examples/crash_switch` crashes `ops::apply`. That proves the *ops layer*
//! is recoverable from any intermediate state, which is what M2's gate asked
//! for. It does not prove that the command a user types is, because `switch`
//! does several things around the apply — it writes the journal, it writes
//! generation 0000, it records the ledger — and a crash between any two of
//! them is a state `recover` has to resolve.
//!
//! So this helper builds the same fixture the M3 command suites use, runs the
//! real `cli::switch::run_with`, and calls `std::process::abort()` after step
//! *k*. Not `panic!`: a panic unwinds, runs destructors and flushes, and a
//! crash does none of those.
//!
//! It lives in `examples/` for D26's reason: a helper whose purpose is to
//! abort a switch half way through belongs neither inside the boundary the
//! guard scripts protect nor in anything `cargo install` would put on a
//! machine.

#[path = "../tests/common/mod.rs"]
mod common;

use common::switching;
use ricepilot::cli::switch::{Kind, Request};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, case, k] = <[String; 3]>::try_from(args)
        .unwrap_or_else(|a| panic!("usage: crash_cli_switch <case> <k>, got {a:?}"));
    let k: usize = k.parse().expect("k must be a number");

    let m = switching::build(&case);
    // The same environment the test suite gives the real binary, so this
    // process resolves its state directory and its lock exactly as `ricepilot
    // switch` would (R1: nothing outside the fixture is reachable).
    for (k, v) in m.f.env() {
        std::env::set_var(k, v);
    }
    let paths = ricepilot::cli::paths::Paths::from_env().expect("fixture paths");

    // The same request `cmd_switch` builds, so what crashes is the command and
    // not an approximation of it.
    let profile = ricepilot::cli::paths::load(&paths, "new").expect("profile `new`");
    let targets = profile.manifest.targets(&profile.dir, &paths.home);
    let ledger = ricepilot::ledger::load(&paths.ledger_path()).expect("ledger");
    let retire: Vec<std::path::PathBuf> = ledger
        .owned_dests()
        .into_iter()
        .filter(|d| !targets.iter().any(|t| &t.dest == d))
        .collect();

    let req = Request {
        kind: Kind::Switch,
        label: "to profile `new`".into(),
        profile: "new".into(),
        targets,
        retire,
        manifest_of: Some((profile.root(&paths.home), profile.manifest.volatile.clone())),
    };

    let _ = ricepilot::cli::switch::run_with(&paths, &req, true, &mut |i| {
        if i == k {
            std::process::abort();
        }
        Ok(())
    });

    // Reaching here means k was past the end of the plan. Abort anyway, so the
    // caller's "it was supposed to abort" assertion stays meaningful.
    std::process::abort();
}
