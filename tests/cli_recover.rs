//! `ricepilot recover`, end to end, against a machine that really crashed.
//!
//! The in-flight state is produced by `examples/crash_switch` aborting mid
//! switch, not by a fixture hand-built to look like one. A snapshot of what
//! the command says about a hand-built state is a snapshot of the fixture.

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::{redact, scenario, Fixture};
use ricepilot::ops::mutate::{self, ExchangeMode};
use ricepilot::ops::read;

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(f: &Fixture, args: &[&str]) -> Run {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("ricepilot"));
    cmd.args(args);
    cmd.env_clear();
    for (k, v) in f.env() {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    Run {
        stdout: redact(&String::from_utf8_lossy(&out.stdout), f),
        stderr: redact(&String::from_utf8_lossy(&out.stderr), f),
        code: out.status.code().unwrap(),
    }
}

/// Build the crash helper once, then crash a switch after step `k`.
fn crashed(case: &str, mode: ExchangeMode, k: usize) -> Fixture {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let helper = BUILT
        .get_or_init(|| {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = Command::new(cargo)
                .args(["build", "--locked", "--example", "crash_switch"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .status()
                .expect("building the crash helper");
            assert!(status.success());
            let mut dir = std::env::current_exe().unwrap();
            dir.pop();
            dir.pop();
            dir.join("examples").join("crash_switch")
        })
        .clone();

    let out = Command::new(helper)
        .arg(case)
        .arg(mode.as_str())
        .arg(k.to_string())
        .output()
        .unwrap();
    assert!(!out.status.success(), "the helper was supposed to abort");
    Fixture::attach("m2", case)
}

#[test]
fn nothing_to_recover_on_a_healthy_machine() {
    let f = Fixture::new_in("m2", "cli_recover_clean");
    let r = run(&f, &["recover"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn recover_is_dry_run_by_default() {
    // Crash after the first exchange, so there is real work to finish.
    let f = crashed("cli_recover_forward", ExchangeMode::Renameat2, 2);
    let s = scenario::attach("cli_recover_forward", ExchangeMode::Renameat2);
    let before = s.live();

    let r = run(&f, &["recover"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);

    // R4: the dry run changed nothing at all.
    assert_eq!(s.live(), before);
    assert!(ricepilot::journal::read_current(&s.journal_path())
        .unwrap()
        .is_some());
}

#[test]
fn recover_commit_finishes_the_switch() {
    let f = crashed("cli_recover_commit", ExchangeMode::Renameat2, 2);
    let s = scenario::attach("cli_recover_commit", ExchangeMode::Renameat2);

    let r = run(&f, &["recover", "--commit"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);

    assert_eq!(s.live(), s.all_new());
    assert_eq!(
        ricepilot::journal::read_current(&s.journal_path()).unwrap(),
        None
    );

    // A second run has nothing to say, because the journal was retired.
    let again = run(&f, &["recover"]);
    assert_eq!(again.stdout, ricepilot::cli::render::NOTHING_TO_RECOVER);
}

#[test]
fn recover_commit_abandons_a_switch_that_never_started() {
    // Crash after the first staging link: no exchange happened.
    let f = crashed("cli_recover_backward", ExchangeMode::Fallback, 0);
    let s = scenario::attach("cli_recover_backward", ExchangeMode::Fallback);

    let r = run(&f, &["recover", "--commit"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);

    assert_eq!(s.live(), s.all_old());
}

#[test]
fn a_destination_nobody_can_account_for_is_refused() {
    let f = crashed("cli_recover_foreign", ExchangeMode::Renameat2, 2);
    let s = scenario::attach("cli_recover_foreign", ExchangeMode::Renameat2);

    // Something re-pointed a managed destination while ricepilot was down.
    let dest = &s.journal.entries[1].dest;
    let elsewhere = f.dir("rice/other/foot");
    mutate::rename_to_attic(dest, &s.attic, &PathBuf::from("moved-by-test")).unwrap();
    mutate::create_symlink(dest, &elsewhere).unwrap();

    let r = run(&f, &["recover", "--commit"]);
    assert_eq!(r.code, 2, "a refusal exits Refused");
    insta::assert_snapshot!(r.stderr);

    // Zero side effects.
    assert_eq!(read::readlink(dest).unwrap(), elsewhere);
    assert!(ricepilot::journal::read_current(&s.journal_path())
        .unwrap()
        .is_some());
}

#[test]
fn a_second_ricepilot_exits_locked_rather_than_queueing() {
    let f = crashed("cli_recover_locked", ExchangeMode::Renameat2, 2);
    let held = ricepilot::ops::lock::acquire(&f.path(".run/ricepilot.lock")).unwrap();

    let r = run(&f, &["recover"]);
    assert_eq!(r.code, 5, "Locked");
    insta::assert_snapshot!(r.stderr);

    drop(held);
}
