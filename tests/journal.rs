//! The write-ahead journal: what it records, what it refuses to record, and
//! that it is durable before anything has happened.

mod common;

use common::{scenario, Fixture};
use ricepilot::journal::{self, Journal};
use ricepilot::observe::{Observed, Shape};
use ricepilot::ops::mutate::ExchangeMode;
use ricepilot::ops::read;
use ricepilot::plan::Op;

#[test]
fn the_record_is_the_before_and_after_of_every_destination() {
    let s = scenario::build("journal_shape", ExchangeMode::Renameat2);
    let j = &s.journal;

    assert_eq!(j.id, scenario::ID);
    assert_eq!(j.exchange_mode, "renameat2");
    assert_eq!(j.entries.len(), 3);

    let hypr = &j.entries[0];
    assert_eq!(
        hypr.old_target.as_deref(),
        Some(s.old_root.join("hypr").as_path())
    );
    assert_eq!(hypr.new_target, s.new_root.join("hypr"));
    assert!(hypr.staged.is_some(), "an exchanged destination is staged");
    assert!(
        hypr.attic_rel.is_some(),
        "its old link is destined for the attic"
    );

    // The absent destination (D11) has no staged link and nothing to displace:
    // `symlinkat` is already atomic, so there is no exchange to record.
    let btop = j
        .entries
        .iter()
        .find(|e| e.dest.ends_with("btop"))
        .expect("the created destination is journalled too");
    assert_eq!(btop.staged, None);
    assert_eq!(btop.old_target, None);
    assert_eq!(btop.attic_rel, None);

    // The printed plan is carried verbatim, for whoever is reading this after
    // the machine did not come back up.
    assert_eq!(
        j.ops,
        s.ops.iter().map(|o| o.to_string()).collect::<Vec<_>>()
    );
}

#[test]
fn a_journal_survives_a_round_trip_through_the_disk() {
    let s = scenario::build("journal_roundtrip", ExchangeMode::Fallback);
    let path = s.journal_path();

    journal::write(&path, &s.journal).unwrap();
    let back = journal::read_current(&path).unwrap().unwrap();

    assert_eq!(back, s.journal);
    assert_eq!(back.mode().unwrap(), ExchangeMode::Fallback);
}

#[test]
fn there_is_no_journal_until_one_is_written() {
    let f = Fixture::new_in("m2", "journal_absent");
    let path = f.state().join("journal").join("current.toml");
    assert_eq!(journal::read_current(&path).unwrap(), None);
}

#[test]
fn a_finished_journal_is_renamed_not_taken_away() {
    let s = scenario::build("journal_done", ExchangeMode::Renameat2);
    let path = s.journal_path();
    journal::write(&path, &s.journal).unwrap();

    let done = journal::mark_done(&path, &s.journal.id).unwrap();

    assert_eq!(
        done.file_name().unwrap(),
        format!("done-{}.toml", scenario::ID).as_str()
    );
    assert!(
        read::lstat(&done).unwrap().is_some(),
        "the evidence is still there"
    );
    assert_eq!(
        journal::read_current(&path).unwrap(),
        None,
        "and `current` is free for the next switch"
    );
}

#[test]
fn a_destination_whose_two_states_look_identical_is_refused() {
    // Recovery decides where a destination is by reading its link target. If
    // old and new were the same string there would be no reading of the
    // filesystem that could tell the two apart, and "fully old or fully new"
    // would stop being a checkable claim. The planner never emits this; the
    // journal refuses it rather than trusting the planner.
    let dest = std::path::PathBuf::from("/home/u/.config/hypr");
    let same = std::path::PathBuf::from("/home/u/rice/hypr");
    let staged = std::path::PathBuf::from("/home/u/.config/hypr.rp-tmp-0");

    let ops = vec![
        Op::CreateTempLink {
            link_path: staged.clone(),
            target: same.clone(),
        },
        Op::Exchange {
            dest: dest.clone(),
            staged,
        },
    ];
    let observed = vec![Observed {
        dest: dest.clone(),
        shape: Shape::OwnedLink {
            target: same.clone(),
        },
        parent_dev: 1,
        parent_fs_type: 0,
        is_mountpoint: false,
    }];

    let err = Journal::from_plan(
        "id",
        "p",
        "/home/u/.local/state/ricepilot/attic/id",
        ExchangeMode::Renameat2,
        &ops,
        &observed,
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("could not tell which side of the switch"),
        "{err}"
    );
}

#[test]
fn a_plan_touching_an_unswitchable_shape_is_refused() {
    // Every other shape is already a refusal in `plan`, so a plan carrying one
    // is a planner bug. It resolves to a decline here rather than being
    // written down as if it were a normal switch.
    let dest = std::path::PathBuf::from("/home/u/.config/hypr");
    let ops = vec![Op::CreateLink {
        link_path: dest.clone(),
        target: "/home/u/rice/new/hypr".into(),
    }];
    let observed = vec![Observed {
        dest: dest.clone(),
        shape: Shape::RealDir,
        parent_dev: 1,
        parent_fs_type: 0,
        is_mountpoint: false,
    }];

    // A created destination carries no old target, so this one goes through;
    // the guard bites on the exchange path, where the old target is read from
    // the observation.
    let j = Journal::from_plan(
        "id",
        "p",
        "/attic",
        ExchangeMode::Renameat2,
        &ops,
        &observed,
    );
    assert!(j.is_ok(), "a create reads nothing from the observation");

    let staged = std::path::PathBuf::from("/home/u/.config/hypr.rp-tmp-0");
    let ops = vec![
        Op::CreateTempLink {
            link_path: staged.clone(),
            target: "/home/u/rice/new/hypr".into(),
        },
        Op::Exchange {
            dest: dest.clone(),
            staged,
        },
    ];
    let err = Journal::from_plan(
        "id",
        "p",
        "/attic",
        ExchangeMode::Renameat2,
        &ops,
        &observed,
    )
    .unwrap_err();
    assert!(err.to_string().contains("never switchable"), "{err}");
}

#[test]
fn timestamps_are_readable_because_gc_makes_you_type_them_back() {
    use std::time::{Duration, UNIX_EPOCH};
    assert_eq!(journal::timestamp_id(UNIX_EPOCH), "19700101T000000Z");
    assert_eq!(
        journal::timestamp_id(UNIX_EPOCH + Duration::from_secs(1_758_412_800)),
        "20250921T000000Z"
    );
    // A leap day, because the calendar arithmetic is written out by hand.
    assert_eq!(
        journal::timestamp_id(UNIX_EPOCH + Duration::from_secs(1_709_164_800)),
        "20240229T000000Z"
    );
}
