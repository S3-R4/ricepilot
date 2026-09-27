//! `switch --strict` (D59): what a switch otherwise reports and goes ahead
//! past — a profile that is not what ricepilot recorded, one it could not
//! compare, a Hyprland config the sandboxed verify-config did not check — is
//! refused instead, with nothing touched.
//!
//! Every invocation is the real binary inside the test sandbox (D56).

mod common;

use std::path::PathBuf;

use common::{redact, switching, undate_ids, Fixture};
use ricepilot::error::ExitCode;
use ricepilot::plan::Refusal;

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(f: &Fixture, args: &[&str]) -> Run {
    let mut cmd = common::ricepilot(f);
    cmd.args(args);
    let out = cmd.output().unwrap();
    Run {
        stdout: undate_ids(&redact(&String::from_utf8_lossy(&out.stdout), f)),
        stderr: undate_ids(&redact(&String::from_utf8_lossy(&out.stderr), f)),
        code: out.status.code().unwrap(),
    }
}

fn ok(r: &Run) {
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
}

/// A machine on `old` with a manifest recorded for both profiles: switched
/// to `new` and back, which is how a profile gets one.
fn recorded(case: &str) -> switching::Machine {
    let m = switching::build(case);
    ok(&run(&m.f, &["switch", "new", "--commit"]));
    ok(&run(&m.f, &["switch", "old", "--commit"]));
    assert_eq!(m.live(), m.all_old());
    m
}

/// Just the drift block, and the verdict under it, out of a switch's output.
fn drift_and_verdict(out: &str) -> String {
    let from = out.find("profile drift:").expect("no drift block");
    let to = out[from..]
        .find("applying")
        .or_else(|| out[from..].find("would apply"))
        .or_else(|| out[from..].find("declined."))
        .map(|i| from + i)
        .unwrap();
    let verdict: String = out[to..].lines().next().unwrap().to_string();
    let refusals: Vec<&str> = out.lines().filter(|l| l.starts_with("  [R")).collect();
    format!("{}{verdict}\n{}", &out[from..to], refusals.join("\n"))
}

#[test]
fn without_strict_drift_is_reported_and_the_switch_goes_ahead() {
    let m = recorded("strict_off");
    m.f.file("rice/new/hypr/marker", "edited by hand\n");
    m.f.file("rice/new/foot/extra.ini", "new file\n");

    let r = run(&m.f, &["switch", "new", "--commit"]);
    ok(&r);
    assert_eq!(m.live(), m.all_new(), "reported, not refused");
    insta::assert_snapshot!(drift_and_verdict(&r.stdout));
}

#[test]
fn strict_refuses_a_profile_that_has_drifted_and_touches_nothing() {
    let m = recorded("strict_drift");
    m.f.file("rice/new/hypr/marker", "edited by hand\n");
    m.f.file("rice/new/foot/extra.ini", "new file\n");
    let attic_before = std::fs::read_dir(m.f.state().join("attic"))
        .unwrap()
        .count();

    let r = run(&m.f, &["switch", "new", "--commit", "--strict"]);
    assert_eq!(r.code, ExitCode::Refused as i32, "{}", r.stderr);
    assert_eq!(m.live(), m.all_old(), "R4: a refusal has no side effects");
    assert_eq!(
        ricepilot::generations::current(&m.f.state()).unwrap(),
        Some(2)
    );
    assert_eq!(
        std::fs::read_dir(m.f.state().join("attic"))
            .unwrap()
            .count(),
        attic_before
    );
    insta::assert_snapshot!(r.stdout);
}

/// A volatile file rewritten, and a file rewritten with the content it
/// already had: neither is drift, so `--strict` goes ahead — and says what
/// it did not compare.
#[test]
fn strict_goes_ahead_past_volatile_paths_and_identical_rewrites() {
    let m = recorded("strict_volatile");
    m.f.file("rice/new/hypr/noise.log", "an app wrote this\n");
    std::thread::sleep(std::time::Duration::from_millis(20));
    m.f.file("rice/new/foot/marker", "new\n");

    let r = run(&m.f, &["switch", "new", "--commit", "--strict"]);
    ok(&r);
    assert_eq!(m.live(), m.all_new());
    insta::assert_snapshot!(drift_and_verdict(&r.stdout));
}

/// Nothing recorded yet: nothing to compare with, and `--strict` does not
/// switch to a profile it could not compare.
#[test]
fn strict_refuses_a_profile_with_nothing_recorded() {
    let m = switching::build("strict_unrecorded");
    let r = run(&m.f, &["switch", "new", "--strict"]);
    assert_eq!(r.code, ExitCode::Refused as i32, "{}", r.stderr);
    assert_eq!(m.live(), m.all_old());
    insta::assert_snapshot!(drift_and_verdict(&r.stdout));

    // Without the flag, the same switch has nothing to say about drift.
    let r = run(&m.f, &["switch", "new"]);
    ok(&r);
    assert!(!r.stdout.contains("profile drift:"), "{}", r.stdout);
}

/// A comparison that could not read what it needed is said out loud even
/// without `--strict` — and the switch goes ahead, as it did before the
/// comparison existed; phase C records a fresh manifest over the broken one.
#[test]
fn a_recorded_manifest_that_does_not_parse_is_reported_not_refused() {
    let m = recorded("strict_broken_record");
    m.f.file(
        ".local/state/ricepilot/manifests/new.toml",
        "this is not a manifest\n",
    );
    let r = run(&m.f, &["switch", "new", "--commit"]);
    ok(&r);
    assert_eq!(m.live(), m.all_new());
    insta::assert_snapshot!(drift_and_verdict(&r.stdout));
    assert!(
        ricepilot::verify::load(&m.f.state().join("manifests/new.toml"))
            .unwrap()
            .is_some()
    );
}

/// A `hyprland.lua` is never run (NOT-POSSIBLE.md#verify-lua-config), so it
/// is "NOT checked": reported and gone ahead past without `--strict`,
/// refused with it.
#[test]
fn strict_refuses_a_hyprland_config_it_did_not_check() {
    let m = switching::build("strict_lua");
    m.f.file("rice/new/hypr/hyprland.lua", "os.execute('touch ran')\n");
    ok(&run(&m.f, &["switch", "new", "--commit"]));
    ok(&run(&m.f, &["switch", "old", "--commit"]));

    let r = run(&m.f, &["switch", "new"]);
    ok(&r);
    assert!(r.stdout.contains("was NOT checked"), "{}", r.stdout);

    let r = run(&m.f, &["switch", "new", "--strict"]);
    assert_eq!(r.code, ExitCode::Refused as i32, "{}", r.stderr);
    assert_eq!(m.live(), m.all_old());
    let from = r.stdout.find("hypr config:").unwrap();
    insta::assert_snapshot!(&r.stdout[from..]);
}

/// A rollback goes through the same phase A, so it reports drift in the
/// profile it returns to — and is never strict: the way back is not made
/// harder to take.
#[test]
fn rollback_reports_drift_and_goes_ahead() {
    let m = recorded("strict_rollback");
    ok(&run(&m.f, &["switch", "new", "--commit"]));
    m.f.file("rice/old/hypr/marker", "edited by hand\n");

    let r = run(&m.f, &["rollback", "--commit"]);
    ok(&r);
    assert_eq!(m.live(), m.all_old());
    insta::assert_snapshot!(drift_and_verdict(&r.stdout));
}

/// The three refusals `--strict` adds, including the ones no fixture here
/// reaches: a machine without Hyprland, and a recorded manifest that does
/// not parse.
#[test]
fn every_refusal_strict_adds() {
    let changed: Vec<String> = (1..=7)
        .map(|n| format!("changed      hypr/conf.d/{n:02}.conf"))
        .collect();
    let m = PathBuf::from("/home/u/.local/state/ricepilot/manifests/caelestia.toml");
    let refusals = vec![
        Refusal::ProfileDrifted {
            profile: "caelestia".into(),
            manifest: m.clone(),
            changed: changed[..2].to_vec(),
        },
        Refusal::ProfileDrifted {
            profile: "caelestia".into(),
            manifest: m.clone(),
            changed,
        },
        Refusal::ProfileNotCompared {
            profile: "caelestia".into(),
            manifest: m.clone(),
            why: "nothing has been recorded for it yet".into(),
        },
        Refusal::ProfileNotCompared {
            profile: "caelestia".into(),
            manifest: m,
            why: "the recorded manifest could not be read: <why>".into(),
        },
        Refusal::VerifyConfigNotChecked {
            file: PathBuf::from("/home/u/.local/share/caelestia/hypr/hyprland.conf"),
            why: "`Hyprland` is not installed in /usr/bin, /usr/local/bin".into(),
        },
        Refusal::VerifyConfigNotChecked {
            file: PathBuf::from("/home/u/.local/share/caelestia/hypr/hyprland.lua"),
            why: "a Lua config is a program, which ricepilot does not run; see \
                  NOT-POSSIBLE.md#verify-lua-config"
                .into(),
        },
    ];
    for r in &refusals {
        assert_eq!(r.rule(), "R4");
    }
    insta::assert_snapshot!(ricepilot::cli::render::refusal_list(&refusals));
}
