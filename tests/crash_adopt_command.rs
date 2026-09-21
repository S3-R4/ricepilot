//! Crash injection against `adopt --commit`.
//!
//! `adopt` has a window no `switch` has: the user's **real directory** is at
//! a staging name while a symlink is at the path it came from. A crash there
//! is the worst state this tool can leave a machine in, and it is the state
//! `journal::Adopt` exists to make decidable (D46).
//!
//! So: abort after step *k*, for every *k*, then run the real
//! `recover --commit` binary and require the machine to be fully old or
//! fully new. Never mixed (R5), and the user's directory intact either way.
//!
//! One state is not reachable through this harness: "the journal was written
//! and the first op never ran", because the hook fires *after* each op.
//! `tests/journal_adopt.rs` drives that one directly, by building the
//! filesystem into it and asking what recovery decides.

mod common;

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use common::adopting::{self, Side};
use common::Fixture;
use ricepilot::ops::read;

/// Build the helper once, through the `cargo` that invoked this test —
/// `cargo test` neither builds examples nor exports a `CARGO_BIN_EXE_*` for
/// them (D26).
fn helper() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = Command::new(cargo)
                .args(["build", "--locked", "--example", "crash_cli_adopt"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .status()
                .expect("building the crash helper");
            assert!(status.success());
            let mut dir = std::env::current_exe().unwrap();
            dir.pop();
            dir.pop();
            dir.join("examples").join("crash_cli_adopt")
        })
        .clone()
}

/// Run the helper, answering its confirmation, and require it to die.
fn crash(case: &str, k: usize) {
    let mut child = Command::new(helper())
        .arg(case)
        .arg(k.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"y\n").unwrap();
    let out = child.wait_with_output().unwrap();
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

/// The adopt plan for this fixture has five ops: stage, exchange, to attic,
/// fsync the config directory, fsync the attic.
const STEPS: usize = 5;

#[test]
fn recover_lands_fully_old_or_fully_new_after_a_crash_at_every_step() {
    let mut seen = Vec::new();
    for k in 0..STEPS {
        let case = format!("crash_adopt_{k}");
        crash(&case, k);
        let m = adopting::attach(&case);

        // The journal reached the disk before the state it describes
        // existed. The process that wrote it is gone, so this is the disk's
        // word rather than that process's.
        let journal_path = m.f.state().join("journal").join("current.toml");
        let j = ricepilot::journal::read_current(&journal_path)
            .unwrap()
            .unwrap_or_else(|| panic!("step {k}: no journal survived the crash"));
        assert_eq!(j.adopt.len(), 1, "step {k}: the adopt record is missing");

        let (code, stderr) = recover(&m.f);
        assert_eq!(code, 0, "step {k}: recover failed: {stderr}");

        let side = m
            .side()
            .unwrap_or_else(|| panic!("step {k}: left mixed — {}", describe(&m)));
        seen.push(side);

        // Either way, the user's configuration is readable and intact. That
        // is the whole point: nothing is ever deleted.
        let dir = match side {
            Side::Old => m.dest.clone(),
            Side::New => m.in_attic().expect("the displaced directory"),
        };
        assert_eq!(
            read::slurp(&dir.join("hyprland.conf")).unwrap(),
            "monitor=,preferred,auto,1\n",
            "step {k}: the directory's contents did not survive"
        );
        assert_eq!(
            read::slurp(&dir.join("scripts/configs.fish")).unwrap(),
            "#!/usr/bin/fish\n"
        );

        assert_eq!(
            ricepilot::journal::read_current(&journal_path).unwrap(),
            None,
            "step {k}: the journal was not retired"
        );
    }

    // Both outcomes have to actually occur, or this suite would pass just as
    // well against a recovery that always abandoned — or always finished.
    // Crashing before the exchange abandons (D23: nothing has taken effect
    // yet); crashing after it finishes.
    assert!(seen.contains(&Side::Old), "no crash abandoned: {seen:?}");
    assert!(seen.contains(&Side::New), "no crash finished: {seen:?}");
}

/// What the machine looks like, for a failure message that says something.
fn describe(m: &adopting::Machine) -> String {
    format!(
        "dest: {:?}, staged clear: {}, in attic: {:?}",
        ricepilot::ops::mutate::link_target(&m.dest),
        m.staging_is_clear(),
        m.in_attic()
    )
}

/// Recovery is idempotent: a second run finds the destination already where
/// the first drove it and says there is nothing in flight.
#[test]
fn a_second_recover_after_a_crashed_adopt_has_nothing_to_do() {
    let case = "crash_adopt_twice";
    crash(case, 1);
    let m = adopting::attach(case);

    let (code, stderr) = recover(&m.f);
    assert_eq!(code, 0, "{stderr}");
    let after = ricepilot::ops::mutate::link_target(&m.dest).unwrap();

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
    assert_eq!(
        ricepilot::ops::mutate::link_target(&m.dest).unwrap(),
        after,
        "the second recover changed something"
    );
}

/// A crashed adopt leaves the machine recoverable, and a *switch* must not
/// be the thing that resolves it: planning against a machine that is half
/// way through an adopt would bury what `recover` needs.
#[test]
fn a_switch_after_a_crashed_adopt_refuses_until_recover_has_run() {
    let case = "crash_adopt_then_switch";
    crash(case, 1);
    let m = adopting::attach(case);
    let before = ricepilot::ops::mutate::link_target(&m.dest).unwrap();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("ricepilot"));
    cmd.args(["switch", "mine", "--commit"]);
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
    assert_eq!(
        ricepilot::ops::mutate::link_target(&m.dest).unwrap(),
        before,
        "and nothing was touched"
    );
}
