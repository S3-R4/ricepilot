//! The requires-check: `pacman -Q` filling `PlanContext::missing_requires`.
//!
//! The planner's refusal and its `paru -S --needed …` line were snapshotted in
//! M1 against a hand-filled context (`plan_snapshots.rs`). What is new is the
//! filling, so these tests ask a real pacman — and skip, saying so, where
//! there is none (CI). The two refusals ricepilot words itself when pacman is
//! absent or gives no answer are snapshotted directly, because this machine
//! has a pacman and cannot reach them any other way.

mod common;

use common::{redact, switching, Fixture};
use ricepilot::error::ExitCode;
use ricepilot::ops::exec::{self, Allowed};

/// A name no repository will ever carry.
const BOGUS: &str = "ricepilot-no-such-package-f3a1";

fn pacman(test: &str) -> bool {
    let found = exec::locate(Allowed::PacmanQuery).unwrap().is_some();
    if !found {
        eprintln!(
            "SKIPPED {test}: `pacman` is not installed in {} on this machine",
            exec::BIN_DIRS.join(", ")
        );
    }
    found
}

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
        stdout: redact(&String::from_utf8_lossy(&out.stdout), f),
        stderr: redact(&String::from_utf8_lossy(&out.stderr), f),
        code: out.status.code().unwrap(),
    }
}

/// `common::undate`, and the same-second `-<n>` suffix with it (D40).
fn undate(s: &str) -> String {
    let s = common::undate(s);
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("<TS>-") {
        out.push_str(&rest[..i + 4]);
        let tail = &rest[i + 5..];
        let digits = tail.chars().take_while(char::is_ascii_digit).count();
        rest = if digits > 0 {
            &tail[digits..]
        } else {
            &rest[i + 4..]
        };
    }
    out.push_str(rest);
    out
}

// ---- the check itself ----

#[test]
fn no_requires_asks_nothing_and_needs_no_pacman() {
    assert!(ricepilot::requires::missing(&[]).unwrap().is_empty());
}

#[test]
fn an_installed_package_is_not_missing_and_a_bogus_one_is() {
    if !pacman("an_installed_package_is_not_missing_and_a_bogus_one_is") {
        return;
    }
    let asked = vec!["pacman".to_string(), BOGUS.to_string(), BOGUS.to_string()];
    assert_eq!(ricepilot::requires::missing(&asked).unwrap(), [BOGUS]);
}

/// `pacman -Q sh` answers `bash …`: bash provides sh. A check that read names
/// back out of pacman's output would call this missing; the exit status says
/// it is satisfied, which is the question (D53).
#[test]
fn a_requirement_satisfied_by_a_provider_is_not_missing() {
    if !pacman("a_requirement_satisfied_by_a_provider_is_not_missing") {
        return;
    }
    assert!(ricepilot::requires::missing(&["sh".to_string()])
        .unwrap()
        .is_empty());
}

// ---- through the commands ----

#[test]
fn plan_declines_a_profile_whose_requires_are_not_installed() {
    if !pacman("plan_declines_a_profile_whose_requires_are_not_installed") {
        return;
    }
    let m = switching::build("requires_plan");
    m.require("new", &["pacman", BOGUS]);

    let r = run(&m.f, &["plan", "new"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    // Only the bogus one: `pacman` is installed, and `paru -S --needed` of a
    // package that is already there would be noise in the one line that is
    // meant to be copied.
    insta::assert_snapshot!(undate(&r.stdout));
}

#[test]
fn switch_commit_refuses_and_changes_nothing() {
    if !pacman("switch_commit_refuses_and_changes_nothing") {
        return;
    }
    let m = switching::build("requires_switch");
    m.require("new", &[BOGUS]);

    let r = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(r.code, ExitCode::Refused as i32, "{}", r.stderr);
    assert!(r.stdout.contains(&format!("paru -S --needed {BOGUS}")));
    assert_eq!(m.live(), m.all_old(), "a refused switch touched a link");
    assert!(ricepilot::generations::current(&m.f.state())
        .unwrap()
        .is_none());
}

/// Going back into a profile is a switch into it (D53): a rollback whose
/// target now requires something missing is declined, and the machine stays
/// on the generation it is on.
#[test]
fn rollback_into_a_profile_whose_requires_are_missing_is_declined() {
    if !pacman("rollback_into_a_profile_whose_requires_are_missing_is_declined") {
        return;
    }
    let m = switching::build("requires_rollback");
    assert_eq!(run(&m.f, &["switch", "new", "--commit"]).code, 0);
    assert_eq!(run(&m.f, &["switch", "old", "--commit"]).code, 0);
    let on = ricepilot::generations::current(&m.f.state()).unwrap();
    m.require("new", &[BOGUS]);

    let r = run(&m.f, &["rollback", "--commit"]);
    assert_eq!(r.code, ExitCode::Refused as i32, "{}", r.stderr);
    insta::assert_snapshot!(undate(&r.stdout));
    assert_eq!(m.live(), m.all_old());
    assert_eq!(ricepilot::generations::current(&m.f.state()).unwrap(), on);
}

// ---- the refusals only a machine without a working pacman reaches ----

#[test]
fn requires_refusals() {
    let mut s = String::new();
    let mut line = |what: &str, e: ricepilot::Error| {
        s.push_str(&format!("{what}\n  exit {}  {e}\n\n", e.exit_code() as u8));
    };
    line(
        "a profile with requires, on a machine with no pacman",
        ricepilot::requires::uncheckable(&["hyprland".into(), "foot".into()]),
    );
    line(
        "pacman ran and did not say whether the package is installed",
        ricepilot::requires::unanswered("hyprland", Some(3)),
    );
    line(
        "pacman was ended by a signal",
        ricepilot::requires::unanswered("hyprland", None),
    );
    insta::assert_snapshot!(s);
}
