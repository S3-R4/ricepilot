//! `ricepilot rollback`, end to end.
//!
//! The M3 gate's round-trip property lives here: switch, roll back, and the
//! link topology is byte-exact while every pre-existing target's inode and
//! mtime is unchanged. M2 proved that at the ops level (D20) because
//! `rollback` did not exist yet; this proves it at the level a user reaches.

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

/// **The gate.** Switch, roll back, and compare.
///
/// Two properties, and the second is the one that matters most: ricepilot must
/// never touch a profile's contents. Every path in both trees keeps its
/// `(dev, ino, mtime_ns)` across a full round trip, which is a stronger claim
/// than "the files still say the same thing" — it says they are the same
/// inodes, never rewritten.
#[test]
fn switch_then_rollback_restores_the_topology_and_touches_no_profile() {
    let m = switching::build("rollback_roundtrip");
    let before_topology = m.live();
    let before_profiles = m.profile_identities();
    assert_eq!(before_topology, m.all_old());

    let s = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(s.code, 0, "{}", s.stderr);
    assert_eq!(m.live(), m.all_new());

    let r = run(&m.f, &["rollback", "--commit"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(undate(&r.stdout));

    assert_eq!(
        m.live(),
        before_topology,
        "the link topology is byte-exact after a round trip"
    );
    assert_eq!(
        m.profile_identities(),
        before_profiles,
        "and not one inode or mtime in either profile tree moved"
    );
}

/// The D21/D36 case, at the command level: `btop` was absent, the switch
/// created a link there, and rolling back cannot remove it. It is displaced
/// into the attic, the destination is empty, and the user is told where the
/// link went rather than left to assume it was deleted.
#[test]
fn a_link_the_switch_created_is_retired_into_the_attic() {
    let m = switching::build("rollback_retire");
    run(&m.f, &["switch", "new", "--commit"]);
    assert!(read::lstat(&m.btop).unwrap().is_some());

    let r = run(&m.f, &["rollback", "--commit"]);
    assert_eq!(r.code, 0, "{}", r.stderr);

    assert_eq!(read::lstat(&m.btop).unwrap(), None, "the path is empty");
    assert!(
        r.stdout.contains("no longer managed"),
        "the user is told, in words: {}",
        r.stdout
    );

    // The link is in the second attic directory — nothing was removed.
    let attic = m.f.state().join("attic");
    let dirs = read::list_dir(&attic).unwrap();
    assert_eq!(dirs.len(), 2, "one attic per switch, and rollback is one");
    let found = dirs.iter().any(|d| {
        let p = attic.join(d).join(m.btop.strip_prefix("/").unwrap());
        read::lstat_or_absent(&p).unwrap().is_some()
    });
    assert!(found, "the retired link is in the attic");

    // And the ledger no longer claims a path ricepilot does not own.
    let ledger = ricepilot::ledger::load(&m.f.state().join("ledger.toml")).unwrap();
    assert!(!ledger.owned_dests().contains(&m.btop));
    assert_eq!(ledger.entries.len(), 2);
}

/// Rollback goes through the switch path, so it journals, generations and
/// recovers like a switch. Generation 0002 is the rolled-back state; 0001 is
/// still there.
#[test]
fn a_rollback_is_a_generation_of_its_own() {
    let m = switching::build("rollback_generation");
    run(&m.f, &["switch", "new", "--commit"]);
    run(&m.f, &["rollback", "--commit"]);

    let state = m.f.state();
    assert_eq!(ricepilot::generations::current(&state).unwrap(), Some(2));
    let g2 = ricepilot::generations::load(&state, 2).unwrap();
    assert_eq!(g2.absent(), vec![m.btop.clone()]);

    // Generation 0000 is the topology ricepilot *found*. Those links point
    // into profile `old`'s root, but ricepilot did not put them there and has
    // no record saying it did, so rolling back to that state is labelled for
    // what it is rather than for what it resembles (D41).
    assert_eq!(g2.profile, ricepilot::generations::PRE_EXISTING);

    // Generation 0001 is untouched: history is kept, not rewound.
    assert_eq!(
        ricepilot::generations::load(&state, 1).unwrap().profile,
        "new"
    );

    // And rolling back again returns to `new`, because 0001 is now NNNN-1.
    run(&m.f, &["rollback", "--commit"]);
    assert_eq!(m.live(), m.all_new());
}

#[test]
fn a_dry_run_rollback_changes_nothing() {
    let m = switching::build("rollback_dry");
    run(&m.f, &["switch", "new", "--commit"]);
    let before = m.live();

    let r = run(&m.f, &["rollback"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!(undate(&r.stdout));

    assert_eq!(m.live(), before);
    assert_eq!(
        ricepilot::generations::current(&m.f.state()).unwrap(),
        Some(1)
    );
}

/// There is nothing to roll back to before the first switch, and saying so is
/// better than rolling back to a generation that does not exist.
#[test]
fn rollback_before_any_switch_refuses() {
    let m = switching::build("rollback_none");
    let r = run(&m.f, &["rollback", "--commit"]);
    assert_eq!(r.code, ExitCode::Refused as i32);
    insta::assert_snapshot!(r.stderr);
    assert_eq!(m.live(), m.all_old());
}

/// `rescue.sh` and `rollback --commit` are the two ways back, and they must
/// agree. After a switch, running the script instead of the command reaches
/// the same topology.
#[test]
fn the_rescue_script_reaches_the_same_state_as_rollback() {
    // Each fixture lives at its own path, so the two topologies are compared
    // with their home prefixes stripped: the question is whether they have the
    // same shape, not whether they are in the same directory.
    let shape = |m: &switching::Machine| -> Vec<Option<Option<String>>> {
        m.live()
            .into_iter()
            .map(|slot| {
                slot.map(|t| {
                    t.map(|p| {
                        p.strip_prefix(&m.f.home)
                            .unwrap_or(&p)
                            .display()
                            .to_string()
                    })
                })
            })
            .collect()
    };

    let by_command = {
        let m = switching::build("rollback_vs_rescue_cmd");
        run(&m.f, &["switch", "new", "--commit"]);
        run(&m.f, &["rollback", "--commit"]);
        shape(&m)
    };

    let m = switching::build("rollback_vs_rescue_sh");
    run(&m.f, &["switch", "new", "--commit"]);
    let out = Command::new("/bin/sh")
        .arg(ricepilot::rescue::path(&m.f.state()))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        shape(&m),
        by_command,
        "the rescue script and `rollback --commit` must agree about where back is"
    );
    assert_eq!(m.live(), m.all_old());
}
