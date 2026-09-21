//! Crash injection against `switch --commit` itself.
//!
//! M2's harness aborts inside `ops::apply`, which proves the ops layer is
//! recoverable from every intermediate state. It does not prove the *command*
//! is: `switch` writes the journal, writes generation 0000, applies the plan,
//! writes generation NNNN, updates the ledger, records a manifest, regenerates
//! `rescue.sh` and only then retires the journal. A crash between any two of
//! those is a state a user can really be left in.
//!
//! So: abort after step *k*, for every *k*, then run the real `recover
//! --commit` binary and require the machine to be fully old or fully new.
//! Never mixed (R5).

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::{switching, Fixture};

/// Build the helper once, through the `cargo` that invoked this test — `cargo
/// test` neither builds examples nor exports a `CARGO_BIN_EXE_*` for them
/// (D26).
fn helper() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = Command::new(cargo)
                .args(["build", "--locked", "--example", "crash_cli_switch"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .status()
                .expect("building the crash helper");
            assert!(status.success());
            let mut dir = std::env::current_exe().unwrap();
            dir.pop();
            dir.pop();
            dir.join("examples").join("crash_cli_switch")
        })
        .clone()
}

fn crash(case: &str, k: usize) {
    let out = Command::new(helper())
        .arg(case)
        .arg(k.to_string())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the helper was supposed to abort at step {k}"
    );
}

fn recover(f: &Fixture) -> (i32, String) {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("ricepilot"));
    cmd.args(["recover", "--commit"]);
    cmd.env_clear();
    for (k, v) in f.env() {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The plan for this fixture has nine ops. Aborting after each of them in turn
/// covers every state the command can be interrupted in from the journal
/// onwards.
const STEPS: usize = 9;

#[test]
fn recover_lands_fully_old_or_fully_new_after_a_crash_at_every_step() {
    for k in 0..STEPS {
        let case = format!("crash_cmd_{k}");
        crash(&case, k);
        let m = switching::attach(&case);

        // The journal reached the disk before the state it describes existed.
        // The process that wrote it is gone, so this is the disk's word, not
        // that process's.
        let journal_path = m.f.state().join("journal").join("current.toml");
        assert!(
            ricepilot::journal::read_current(&journal_path)
                .unwrap()
                .is_some(),
            "step {k}: no journal survived the crash"
        );

        let (code, stderr) = recover(&m.f);
        assert_eq!(code, 0, "step {k}: recover failed: {stderr}");

        let live = m.live();
        assert!(
            live == m.all_old() || live == m.all_new(),
            "step {k}: left mixed — {live:?}\nold: {:?}\nnew: {:?}",
            m.all_old(),
            m.all_new()
        );

        // And the journal is retired, so the machine is not perpetually
        // mid-switch.
        assert_eq!(
            ricepilot::journal::read_current(&journal_path).unwrap(),
            None,
            "step {k}: the journal was not retired"
        );
    }
}

/// Recovery is idempotent: running it a second time finds every destination
/// already where the first drove it, and says there is nothing in flight.
#[test]
fn a_second_recover_after_a_crash_has_nothing_to_do() {
    let case = "crash_cmd_twice";
    crash(case, 2);
    let m = switching::attach(case);

    let (code, _) = recover(&m.f);
    assert_eq!(code, 0);
    let after = m.live();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("ricepilot"));
    cmd.args(["recover", "--commit"]);
    cmd.env_clear();
    for (k, v) in m.f.env() {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code().unwrap(), 0);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        ricepilot::cli::render::NOTHING_TO_RECOVER
    );
    assert_eq!(m.live(), after);
}

/// A crash leaves the machine recoverable, and a *new* switch must not be the
/// thing that resolves it. Planning against a half-switched machine would bury
/// what `recover` needs.
#[test]
fn a_switch_after_a_crash_refuses_until_recover_has_run() {
    let case = "crash_cmd_then_switch";
    crash(case, 2);
    let m = switching::attach(case);
    let before = m.live();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("ricepilot"));
    cmd.args(["switch", "old", "--commit"]);
    cmd.env_clear();
    for (k, v) in m.f.env() {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code().unwrap(),
        ricepilot::error::ExitCode::Refused as i32
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("recover"));
    assert_eq!(m.live(), before, "and nothing was touched");
}
