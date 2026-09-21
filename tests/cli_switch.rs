//! `ricepilot switch`, end to end, against real fixture trees.
//!
//! Every invocation is the real binary under `env_clear()` with only the
//! fixture variables set, so no test can reach the real `$HOME` (R1).

mod common;

use std::process::Command;

use common::{redact, switching, Fixture};
use ricepilot::error::ExitCode;
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

/// Replace every `YYYYMMDDTHHMMSSZ` timestamp with `<TS>`, so snapshots do not
/// depend on when they were taken. Only the timestamp: the paths around it are
/// exactly what the user is told, and redacting those would hide the thing
/// worth reviewing.
fn undate(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let is_ts = |i: usize| {
        i + 16 <= b.len()
            && b[i..i + 8].iter().all(char::is_ascii_digit)
            && b[i + 8] == 'T'
            && b[i + 9..i + 15].iter().all(char::is_ascii_digit)
            && b[i + 15] == 'Z'
    };
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if is_ts(i) {
            out.push_str("<TS>");
            i += 16;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

#[test]
fn a_dry_run_prints_the_plan_and_changes_nothing() {
    let m = switching::build("switch_dry");
    let before = m.live();

    let r = run(&m.f, &["switch", "new"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(undate(&r.stdout));

    assert_eq!(m.live(), before, "R4: a dry run has no effects");
    assert!(
        ricepilot::generations::current(&m.f.state())
            .unwrap()
            .is_none(),
        "and writes no generation"
    );
    assert!(
        read::lstat_or_absent(&ricepilot::rescue::path(&m.f.state()))
            .unwrap()
            .is_none(),
        "and no rescue script"
    );
    // Not even the two probe links, which are a mutation of ricepilot's own
    // state directory (D39).
    assert!(read::lstat_or_absent(&m.f.state().join(".rp-probe-a"))
        .unwrap()
        .is_none());
}

#[test]
fn commit_switches_every_destination_and_records_the_generation() {
    let m = switching::build("switch_commit");

    let r = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(undate(&r.stdout));

    assert_eq!(m.live(), m.all_new());

    // Generation 0000 is the state before the first switch, so that "rollback
    // re-applies NNNN-1" has no special case (D35).
    let state = m.f.state();
    assert_eq!(ricepilot::generations::current(&state).unwrap(), Some(1));
    let g0 = ricepilot::generations::load(&state, 0).unwrap();
    assert_eq!(g0.profile, ricepilot::generations::PRE_EXISTING);
    assert_eq!(g0.targets().len(), 2, "hypr and foot; btop was absent");
    assert_eq!(g0.absent(), vec![m.btop.clone()]);

    let g1 = ricepilot::generations::load(&state, 1).unwrap();
    assert_eq!(g1.profile, "new");
    assert_eq!(g1.targets().len(), 3);

    // The ledger now says ricepilot owns all three, on behalf of `new`.
    let ledger = ricepilot::ledger::load(&state.join("ledger.toml")).unwrap();
    assert_eq!(ledger.entries.len(), 3);
    assert!(ledger.entries.iter().all(|e| e.profile == "new"));

    // A rescue script, a tree manifest, and a retired journal.
    assert!(read::lstat(&ricepilot::rescue::path(&state))
        .unwrap()
        .is_some());
    assert!(
        ricepilot::verify::load(&ricepilot::verify::manifest_path(&state, "new"))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        ricepilot::journal::read_current(&state.join("journal").join("current.toml")).unwrap(),
        None,
        "the journal is retired once everything else succeeded"
    );
    assert!(
        read::list_dir(&state.join("journal"))
            .unwrap()
            .iter()
            .any(|n| n.to_string_lossy().starts_with("done-")),
        "retired by renaming, not by removal (D25)"
    );
}

/// The displaced old links are in the attic and still readable. Nothing about
/// a switch removes anything.
#[test]
fn the_displaced_links_are_in_the_attic() {
    let m = switching::build("switch_attic");
    run(&m.f, &["switch", "new", "--commit"]);

    let attic = m.f.state().join("attic");
    let ts = read::list_dir(&attic).unwrap();
    assert_eq!(ts.len(), 1, "one attic directory per switch");
    let dir = attic.join(&ts[0]);

    let parked = dir.join(m.hypr.strip_prefix("/").unwrap());
    assert_eq!(read::readlink(&parked).unwrap(), m.old_root.join("hypr"));
}

/// `verify` against the profile just switched to is clean, because the switch
/// recorded the manifest it is compared against.
#[test]
fn verify_is_clean_immediately_after_a_switch() {
    let m = switching::build("switch_then_verify");
    run(&m.f, &["switch", "new", "--commit"]);

    let r = run(&m.f, &["verify", "new"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(r.stdout.contains("clean:"), "{}", r.stdout);

    // And a one-byte edit in the profile is drift. The volatile `.log` file
    // beside it is not.
    m.f.file("rice/new/hypr/marker", "edited\n");
    m.f.file("rice/new/hypr/noise.log", "much more volatile\n");
    let r = run(&m.f, &["verify", "new"]);
    assert_eq!(r.code, ExitCode::Drift as i32);
    assert!(r.stdout.contains("hypr/marker"), "{}", r.stdout);
    assert!(!r.stdout.contains("noise.log"), "{}", r.stdout);
}

/// Switching twice in a row is a no-op the second time: reality already
/// matches, so there is no new generation and nothing in the attic.
#[test]
fn switching_to_the_profile_already_live_does_nothing() {
    let m = switching::build("switch_again");
    run(&m.f, &["switch", "new", "--commit"]);

    let r = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(undate(&r.stdout));
    assert_eq!(
        ricepilot::generations::current(&m.f.state()).unwrap(),
        Some(1),
        "a no-op switch writes no generation: rollback must not step through \
         states the machine was never in"
    );
}

/// A foreign link at a managed destination declines the whole switch, with
/// every reason, non-zero, and with nothing touched (R4, R5).
#[test]
fn a_foreign_destination_declines_the_whole_switch() {
    let m = switching::build("switch_declines");
    let elsewhere = m.f.dir("rice/other/foot");
    m.f.link(".config/foot", &elsewhere);
    let before = m.live();

    let r = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(r.code, ExitCode::Refused as i32);
    insta::assert_snapshot!(undate(&r.stdout));

    assert_eq!(m.live(), before);
    assert!(
        ricepilot::generations::current(&m.f.state())
            .unwrap()
            .is_none(),
        "a decline has zero side effects"
    );
}

/// An unfinished switch blocks a new one. Planning against a machine that is
/// half way between two profiles would produce a correct plan for a machine
/// that does not exist, and applying it would bury what `recover` needs.
#[test]
fn a_switch_refuses_while_another_is_in_flight() {
    let m = switching::build("switch_in_flight");
    run(&m.f, &["switch", "new", "--commit"]);

    // Put a journal back, as a crash would have left it.
    let done = read::list_dir(&m.f.state().join("journal"))
        .unwrap()
        .into_iter()
        .find(|n| n.to_string_lossy().starts_with("done-"))
        .unwrap();
    ricepilot::ops::mutate::rename_within(
        &m.f.state().join("journal").join(&done),
        &m.f.state().join("journal").join("current.toml"),
    )
    .unwrap();

    let r = run(&m.f, &["switch", "old", "--commit"]);
    assert_eq!(r.code, ExitCode::Refused as i32);
    insta::assert_snapshot!(r.stderr);
    assert_eq!(m.live(), m.all_new(), "nothing was touched");
}

#[test]
fn a_second_ricepilot_exits_locked_rather_than_switching() {
    let m = switching::build("switch_locked");
    let held = ricepilot::ops::lock::acquire(&m.f.path(".run/ricepilot.lock")).unwrap();

    let r = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(r.code, ExitCode::Locked as i32);
    assert_eq!(m.live(), m.all_old());

    drop(held);
}

/// A profile whose payload is not there would link every managed destination
/// at nothing. A dangling `~/.config/hypr` is a compositor with no config at
/// the next login — and, without this refusal, it would look exactly like a
/// successful switch.
#[test]
fn a_switch_onto_a_missing_profile_tree_is_refused() {
    let m = switching::build("switch_missing_src");
    m.f.clear("rice/new/hypr");
    let before = m.live();

    let r = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(r.code, ExitCode::Refused as i32);
    insta::assert_snapshot!(undate(&r.stdout));

    assert_eq!(m.live(), before, "and nothing was touched");
}

/// The same check refuses a source that exists and is not a directory —
/// including a symlink to one. `lstat` does not follow it and neither does
/// ricepilot: pointing a managed destination at a link whose target it has
/// never looked at is the chain of trust the ownership predicate refuses.
#[test]
fn a_switch_onto_a_source_that_is_not_a_directory_is_refused() {
    let m = switching::build("switch_src_not_dir");
    m.f.clear("rice/new/foot");
    m.f.file("rice/new/foot", "this is a file\n");

    let r = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(r.code, ExitCode::Refused as i32);
    assert!(r.stdout.contains("is not a directory"), "{}", r.stdout);
    assert_eq!(m.live(), m.all_old());
}

/// `plan` sees it too, so the dry run and the switch agree — a plan that said
/// it would work and a switch that refused would be the worst of both.
#[test]
fn plan_reports_the_same_missing_source() {
    let m = switching::build("switch_plan_missing_src");
    m.f.clear("rice/new/hypr");

    let r = run(&m.f, &["plan", "new"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("does not exist"), "{}", r.stdout);
}
