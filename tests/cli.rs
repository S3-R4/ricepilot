//! The read-only commands, end to end, against real fixture trees.
//!
//! Every invocation runs with `RICEPILOT_HOME` / `RICEPILOT_DATA_DIR` /
//! `RICEPILOT_STATE_DIR` pointing inside `target/fixtures/`, so no test can
//! reach the real `$HOME` (`SAFETY.md` R1).

mod common;

use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use common::{redact, Fixture};

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(f: &Fixture, args: &[&str]) -> Run {
    let mut cmd = Command::new(cargo_bin("ricepilot"));
    cmd.args(args);
    // A cleared environment plus only the fixture variables: if the binary
    // ever reached for the real HOME, there would not be one to find.
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

fn caelestia_manifest(root: &Path) -> String {
    format!(
        r#"name = "caelestia"
root = "{}"
requires = ["hyprland", "foot"]
hypr_dialect = "conf"
volatile = ["**/fish_variables", "shell.json"]
generated = ["hypr/scheme/current.conf"]

[[path]]
dest       = "~/.config/hypr"
src        = "hypr"
kind       = "dir-link"
activation = "relogin"

[[path]]
dest       = "~/.config/foot"
src        = "foot"
kind       = "dir-link"
activation = "relogin"

[[path]]
dest       = "~/.config/fuzzel"
src        = "fuzzel"
kind       = "generated"
activation = "never"
"#,
        root.display()
    )
}

/// A fixture with one by-reference profile whose payload really exists.
fn with_caelestia(case: &str) -> Fixture {
    let f = Fixture::new(case);
    let root = f.dir("rice/caelestia");
    f.dir("rice/caelestia/hypr");
    f.dir("rice/caelestia/foot");
    f.profile("caelestia", &caelestia_manifest(&root));
    f
}

#[test]
fn list_with_no_profiles() {
    let f = Fixture::new("cli_list_empty");
    let r = run(&f, &["list"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn list_a_by_reference_profile() {
    let f = with_caelestia("cli_list");
    let r = run(&f, &["list"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn show_a_profile() {
    let f = with_caelestia("cli_show");
    let r = run(&f, &["show", "caelestia"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn show_a_profile_that_does_not_exist() {
    let f = Fixture::new("cli_show_missing");
    let r = run(&f, &["show", "nope"]);
    assert_eq!(r.code, ricepilot::error::ExitCode::Refused as i32);
    insta::assert_snapshot!(r.stderr);
}

#[test]
fn a_profile_name_cannot_escape_the_profiles_directory() {
    let f = Fixture::new("cli_show_escape");
    let r = run(&f, &["show", "../../../etc"]);
    assert_eq!(r.code, ricepilot::error::ExitCode::Refused as i32);
    insta::assert_snapshot!(r.stderr);
}

#[test]
fn status() {
    let f = with_caelestia("cli_status");
    let r = run(&f, &["status"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

/// The pre-`init` case on a machine whose rice already exists: the links are
/// there, but nothing is registered, so every one of them is correctly
/// reported as foreign and the switch is declined.
#[test]
fn plan_against_existing_unregistered_links() {
    let f = with_caelestia("cli_plan_foreign");
    let root = f.path("rice/caelestia");
    f.clear(".config/hypr");
    f.clear(".config/foot");
    f.link(".config/hypr", &root.join("hypr"));
    f.link(".config/foot", &root.join("foot"));

    let r = run(&f, &["plan", "caelestia"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

/// The same tree once the ledger records the links: all three ownership facts
/// hold, so they are owned links rather than foreign ones, and the plan is a
/// no-op because they already point where the profile says. This is the branch
/// M1 could not reach and the reason its explanatory note is now gone.
#[test]
fn plan_with_a_ledger_sees_its_own_links() {
    let f = with_caelestia("cli_plan_owned");
    let root = f.path("rice/caelestia");
    f.clear(".config/hypr");
    f.clear(".config/foot");
    let hypr = f.link(".config/hypr", &root.join("hypr"));
    let foot = f.link(".config/foot", &root.join("foot"));

    let mut l = ricepilot::ledger::Ledger::default();
    l.record(&[hypr, foot], "caelestia").unwrap();
    ricepilot::ledger::save(&f.state().join("ledger.toml"), &l).unwrap();

    let r = run(&f, &["plan", "caelestia"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);

    let s = run(&f, &["status"]);
    assert_eq!(s.code, 0, "{}", s.stderr);
    insta::assert_snapshot!("status_with_owned_paths", s.stdout);
}

/// A ledger row whose `(dev, ino)` no longer matches the link at that path is
/// the installer-swapped-it case, and it is refused rather than acted on.
#[test]
fn plan_refuses_a_link_the_ledger_no_longer_identifies() {
    let f = with_caelestia("cli_plan_stale_ledger");
    let root = f.path("rice/caelestia");
    f.clear(".config/hypr");
    f.clear(".config/foot");
    let hypr = f.link(".config/hypr", &root.join("hypr"));
    let foot = f.link(".config/foot", &root.join("foot"));

    let mut l = ricepilot::ledger::Ledger::default();
    l.record(&[hypr, foot.clone()], "caelestia").unwrap();
    ricepilot::ledger::save(&f.state().join("ledger.toml"), &l).unwrap();

    // Same path, same target string, different inode.
    f.link(".config/foot", &root.join("foot"));

    let r = run(&f, &["plan", "caelestia"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stdout.contains("foreign link"),
        "an identical-looking replacement must not be treated as owned:\n{}",
        r.stdout
    );
    let _ = foot;
}

/// Nothing at either destination: the switch is a pair of plain link
/// creations, with nothing to displace.
#[test]
fn plan_onto_absent_destinations() {
    let f = with_caelestia("cli_plan_absent");
    f.clear(".config/hypr");
    f.clear(".config/foot");

    let r = run(&f, &["plan", "caelestia"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

/// A real directory at a managed destination — what a rice installer leaves
/// behind. Refused, with the reason.
#[test]
fn plan_against_a_real_directory() {
    let f = with_caelestia("cli_plan_realdir");
    f.clear(".config/hypr");
    f.clear(".config/foot");
    f.dir(".config/hypr");
    f.dir(".config/foot");

    let r = run(&f, &["plan", "caelestia"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

/// A command whose milestone has not landed exits `NotPossible` and names the
/// milestone. It is deliberately not a silent no-op: `--commit` returning 0
/// without doing anything is the most dangerous thing a half-built version of
/// this tool could do.
#[test]
fn a_command_from_a_later_milestone_is_not_implemented_and_says_so() {
    let f = with_caelestia("cli_init");
    let r = run(&f, &["init", "--commit"]);
    assert_eq!(r.code, ricepilot::error::ExitCode::NotPossible as i32);
    insta::assert_snapshot!(r.stderr);
}

/// `--relogin` and `--strict` are M5. Accepting a flag and quietly ignoring it
/// is worse than refusing it: the user asked for something and was told
/// nothing. Both refuse, and the switch itself does not happen.
#[test]
fn a_flag_from_a_later_milestone_refuses_rather_than_being_ignored() {
    let f = with_caelestia("cli_switch_flags");
    for flag in ["--relogin", "--strict"] {
        let r = run(&f, &["switch", "caelestia", "--commit", flag]);
        assert_eq!(
            r.code,
            ricepilot::error::ExitCode::NotPossible as i32,
            "{flag}: {}",
            r.stderr
        );
        insta::assert_snapshot!(format!("switch{}", flag.replace('-', "_")), r.stderr);
    }
    // And nothing happened.
    assert!(ricepilot::generations::current(&f.state())
        .unwrap()
        .is_none());
}

/// M1 ships no mutating command at all, so no fixture destination may change.
/// This asserts the negative directly rather than trusting the dispatch table.
#[test]
fn no_read_only_command_changes_anything() {
    let f = with_caelestia("cli_readonly");
    f.clear(".config/hypr");
    let dest = f.dir(".config/hypr");
    let before = std::fs::symlink_metadata(&dest).unwrap();

    for args in [
        vec!["list"],
        vec!["status"],
        vec!["show", "caelestia"],
        vec!["plan", "caelestia"],
    ] {
        run(&f, &args);
    }

    let after = std::fs::symlink_metadata(&dest).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert!(
        std::fs::read_dir(f.path(".config")).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("rp-tmp")),
        "a read-only command left a staged link behind"
    );
}

use std::os::unix::fs::MetadataExt as _;

/// A profile with a small real payload of its own, so `verify` has a tree to
/// hash. `volatile` covers one file in it.
fn with_payload(case: &str) -> Fixture {
    let f = Fixture::new(case);
    let dir = f.profile(
        "bare",
        r#"name = "bare"
volatile = ["**/*.log"]

[[path]]
dest       = "~/.config/hypr"
src        = "hypr"
kind       = "dir-link"
activation = "relogin"
"#,
    );
    let _ = dir;
    f.file(
        ".local/share/ricepilot/profiles/bare/hypr/hyprland.conf",
        "bind = SUPER, Q\n",
    );
    f.file(
        ".local/share/ricepilot/profiles/bare/hypr/hypr.log",
        "noise\n",
    );
    f
}

fn record_manifest(f: &Fixture) {
    let root = f.path(".local/share/ricepilot/profiles/bare");
    let m = ricepilot::verify::build("bare", &root, &["**/*.log".to_string()], "20260921T101112Z")
        .unwrap();
    ricepilot::verify::save(&ricepilot::verify::manifest_path(&f.state(), "bare"), &m).unwrap();
}

/// Nothing has been recorded, so there is nothing to compare against. Saying
/// so is the honest answer; comparing against an empty manifest would report
/// the whole tree as newly added.
#[test]
fn verify_before_anything_is_recorded_refuses() {
    let f = with_payload("cli_verify_unrecorded");
    let r = run(&f, &["verify", "bare"]);
    assert_eq!(r.code, ricepilot::error::ExitCode::Refused as i32);
    insta::assert_snapshot!(r.stderr);
}

#[test]
fn verify_on_an_unchanged_profile_is_clean_and_exits_zero() {
    let f = with_payload("cli_verify_clean");
    record_manifest(&f);
    let r = run(&f, &["verify", "bare"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}

/// One byte in a hashed file, one rewrite inside a volatile one. Only the
/// first is drift, and the exit code says so without the report having to be
/// parsed.
#[test]
fn verify_reports_drift_and_exits_with_the_drift_code() {
    let f = with_payload("cli_verify_drift");
    record_manifest(&f);
    f.file(
        ".local/share/ricepilot/profiles/bare/hypr/hyprland.conf",
        "bind = SUPER, W\n",
    );
    f.file(
        ".local/share/ricepilot/profiles/bare/hypr/hypr.log",
        "much more noise\n",
    );

    let r = run(&f, &["verify", "bare"]);
    assert_eq!(r.code, ricepilot::error::ExitCode::Drift as i32);
    insta::assert_snapshot!(r.stdout);
}

/// Before any switch there is no previous generation, so there is no script.
/// Saying so beats printing a path to a file that is not there.
#[test]
fn rescue_before_any_switch_refuses_and_says_why() {
    let f = with_caelestia("cli_rescue_none");
    let r = run(&f, &["rescue"]);
    assert_eq!(r.code, ricepilot::error::ExitCode::Refused as i32);
    insta::assert_snapshot!(r.stderr);
}

/// Once one exists, the command prints where it is and the exact line to type.
#[test]
fn rescue_prints_the_path_and_the_command() {
    let f = with_caelestia("cli_rescue");
    let g = ricepilot::generations::Generation::observe(
        0,
        "caelestia",
        "20260921T101112Z",
        &[f.path(".config/hypr")],
    )
    .unwrap();
    ricepilot::rescue::regenerate(&f.state(), &g).unwrap();

    let r = run(&f, &["rescue"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(r.stdout);
}
