//! `state/ledger.toml` — the third fact of the ownership predicate.
//!
//! The tests that matter here are the ones where the ledger *disagrees* with
//! the disk. A ledger that only ever confirms what a link already looks like
//! adds nothing to the predicate; its whole value is refusing the link that
//! looks right and is not.

mod common;

use std::path::PathBuf;

use common::Fixture;
use ricepilot::ledger::{self, Entry, Ledger};
use ricepilot::observe::{observe_one, Shape};

/// A fixture with one profile root and a link into it, plus a ledger that
/// records the link honestly.
fn owned(case: &str) -> (Fixture, PathBuf, PathBuf, Ledger) {
    let f = Fixture::new_in("m3", case);
    let root = f.dir("rice/one");
    f.dir("rice/one/hypr");
    let dest = f.link(".config/hypr", &root.join("hypr"));
    let mut l = Ledger::default();
    l.record(std::slice::from_ref(&dest), "one").unwrap();
    (f, root, dest, l)
}

#[test]
fn a_recorded_link_satisfies_all_three_facts() {
    let (_f, root, dest, l) = owned("ledger_owned");
    let obs = observe_one(&dest, &l.ownership(vec![root])).unwrap();
    assert_eq!(
        obs.shape,
        Shape::OwnedLink {
            target: dest
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("rice/one/hypr")
        }
    );
}

/// Fact 2 alone is not enough: a link pointing into a registered root that the
/// ledger has never heard of is foreign. This is the pre-`init` case — the
/// links are already there and ricepilot did not create them.
#[test]
fn a_link_into_a_registered_root_with_no_ledger_row_is_foreign() {
    let (_f, root, dest, _l) = owned("ledger_no_row");
    let empty = Ledger::default();
    let obs = observe_one(&dest, &empty.ownership(vec![root])).unwrap();
    assert!(matches!(obs.shape, Shape::ForeignLink { .. }));
}

/// Fact 3's `(dev, ino)` is what makes a row evidence rather than a claim.
/// Replacing the link with an identical-looking one leaves the path and the
/// target string agreeing and the inode not, and that is the case where
/// ricepilot must refuse.
#[test]
fn an_identical_looking_replacement_link_is_foreign() {
    let (f, root, dest, l) = owned("ledger_reinoded");
    let target = ricepilot::ops::read::readlink(&dest).unwrap();
    f.link(".config/hypr", &target);

    let obs = observe_one(&dest, &l.ownership(vec![root])).unwrap();
    assert!(
        matches!(obs.shape, Shape::ForeignLink { .. }),
        "a row whose (dev, ino) no longer matches must not confer ownership"
    );
}

/// Fact 2 is checked independently of the ledger: a row can say whatever it
/// likes about a link pointing outside every registered root.
#[test]
fn a_link_outside_every_registered_root_is_foreign_however_the_ledger_reads() {
    let (f, _root, dest, l) = owned("ledger_outside");
    let elsewhere = f.dir("rice/other");
    assert!(matches!(
        observe_one(&dest, &l.ownership(vec![elsewhere]))
            .unwrap()
            .shape,
        Shape::ForeignLink { .. }
    ));
}

#[test]
fn a_ledger_round_trips_through_the_file() {
    let (f, _root, _dest, l) = owned("ledger_roundtrip");
    let path = f.state().join("ledger.toml");
    ledger::save(&path, &l).unwrap();
    assert_eq!(ledger::load(&path).unwrap(), l);

    // It is a real file, not a symlink: the state directory is the one place
    // a switch that went wrong must still be readable from.
    let meta = ricepilot::ops::read::lstat(&path).unwrap().unwrap();
    assert_eq!(meta.kind, ricepilot::ops::read::Kind::File);
}

#[test]
fn an_absent_ledger_is_an_empty_one_rather_than_an_error() {
    let f = Fixture::new_in("m3", "ledger_absent");
    let l = ledger::load(&f.state().join("nothing-here.toml")).unwrap();
    assert!(l.entries.is_empty());
}

#[test]
fn a_ledger_that_does_not_parse_is_a_refusal_naming_the_file() {
    let f = Fixture::new_in("m3", "ledger_corrupt");
    let path = f.state().join("ledger.toml");
    f.file(
        ".local/state/ricepilot/ledger.toml",
        "this is not toml {{{\n",
    );
    let err = ledger::load(&path).unwrap_err();
    assert_eq!(err.exit_code(), ricepilot::error::ExitCode::Refused);
    assert!(err.to_string().contains("ledger.toml"));
}

/// `record` re-reads the disk rather than trusting what the caller believes it
/// just did, and `forget` drops a row without anything being removed.
#[test]
fn record_replaces_a_row_and_forget_drops_it() {
    let (f, _root, dest, mut l) = owned("ledger_record");
    let other = f.dir("rice/two/hypr");
    f.link(".config/hypr", &other);

    l.record(std::slice::from_ref(&dest), "two").unwrap();
    assert_eq!(l.entries.len(), 1, "a re-record replaces, never appends");
    assert_eq!(l.entries[0].target, other);
    assert_eq!(l.entries[0].profile, "two");

    l.forget(&[dest]);
    assert!(l.entries.is_empty());
}

/// The ledger records links. A row asserting ownership of a real directory
/// could never be confirmed by fact 1, so it is refused at the point it would
/// be written rather than believed and acted on later.
#[test]
fn a_real_directory_cannot_be_recorded_as_owned() {
    let f = Fixture::new_in("m3", "ledger_realdir");
    let dir = f.dir(".config/hypr");
    let err = Entry::of(&dir, "one").unwrap_err();
    assert_eq!(err.exit_code(), ricepilot::error::ExitCode::Refused);
    assert!(err.to_string().contains("real directory"));
}
