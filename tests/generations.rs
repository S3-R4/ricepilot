//! Generations, and the `current` pointer that has to survive a bad switch.

mod common;

use common::Fixture;
use ricepilot::generations::{self, GenEntry, Generation};

const WHEN: &str = "20260921T101112Z";

#[test]
fn a_generation_records_the_topology_it_observes() {
    let f = Fixture::new_in("m3", "gen_observe");
    let root = f.dir("rice/new");
    f.dir("rice/new/hypr");
    let hypr = f.link(".config/hypr", &root.join("hypr"));
    let btop = f.path(".config/btop");

    let g = Generation::observe(1, "new", WHEN, &[hypr.clone(), btop.clone()]).unwrap();

    assert_eq!(
        g.entries,
        vec![
            GenEntry {
                dest: btop.clone(),
                target: None
            },
            GenEntry {
                dest: hypr.clone(),
                target: Some(root.join("hypr"))
            },
        ]
    );
    assert_eq!(g.targets().len(), 1, "only the destination holding a link");
    assert_eq!(g.absent(), vec![btop]);
    assert_eq!(g.dests().len(), 2, "an absence is recorded, not omitted");
}

/// Observed, never assumed. A destination holding a real directory is recorded
/// as holding no link, because that is what is true of it.
#[test]
fn a_destination_that_is_not_a_link_records_no_target() {
    let f = Fixture::new_in("m3", "gen_realdir");
    let dir = f.dir(".config/hypr");
    let g = Generation::observe(1, "new", WHEN, std::slice::from_ref(&dir)).unwrap();
    assert_eq!(
        g.entries,
        vec![GenEntry {
            dest: dir,
            target: None
        }]
    );
}

#[test]
fn a_generation_round_trips_through_its_file() {
    let f = Fixture::new_in("m3", "gen_roundtrip");
    let root = f.dir("rice/new");
    let hypr = f.link(".config/hypr", &root.join("hypr"));
    let g = Generation::observe(7, "new", WHEN, &[hypr]).unwrap();

    generations::save(&f.state(), &g).unwrap();
    assert_eq!(generations::load(&f.state(), 7).unwrap(), g);
    assert!(
        generations::path(&f.state(), 7).ends_with("0007.toml"),
        "ids are zero-padded so a listing is in switch order"
    );
}

/// The pointer is a real file. This is the one assertion in the suite that is
/// about the *kind* of the file rather than its contents, and it is the point
/// of the whole design: a switch that goes wrong is a switch that did something
/// unintended to a symlink, so the thing that says how to get back must not be
/// one.
#[test]
fn the_current_pointer_is_a_real_file() {
    let f = Fixture::new_in("m3", "gen_current_file");
    generations::set_current(&f.state(), 3).unwrap();

    let meta = ricepilot::ops::read::lstat(&generations::current_path(&f.state()))
        .unwrap()
        .unwrap();
    assert_eq!(meta.kind, ricepilot::ops::read::Kind::File);
    assert_eq!(generations::current(&f.state()).unwrap(), Some(3));
}

/// And if something replaces it with a symlink, that is refused rather than
/// followed — the failure mode this file exists to avoid must not be reachable
/// by pointing the file at itself.
#[test]
fn a_symlinked_current_pointer_is_refused() {
    let f = Fixture::new_in("m3", "gen_current_link");
    f.dir(".local/state/ricepilot/generations");
    let elsewhere = f.file("rice/decoy", "0009\n");
    f.link(".local/state/ricepilot/generations/current", &elsewhere);

    let err = generations::current(&f.state()).unwrap_err();
    assert_eq!(err.exit_code(), ricepilot::error::ExitCode::Refused);
    assert!(err.to_string().contains("regular file"));
}

#[test]
fn a_machine_that_has_never_switched_has_no_current_generation() {
    let f = Fixture::new_in("m3", "gen_none");
    assert_eq!(generations::current(&f.state()).unwrap(), None);
    assert_eq!(generations::next_id(&f.state()).unwrap(), 0);
}

#[test]
fn the_next_id_follows_the_pointer_rather_than_the_directory() {
    let f = Fixture::new_in("m3", "gen_next");
    generations::set_current(&f.state(), 4).unwrap();
    assert_eq!(generations::next_id(&f.state()).unwrap(), 5);
}

#[test]
fn a_pointer_that_is_not_a_number_is_refused_with_what_it_said() {
    let f = Fixture::new_in("m3", "gen_garbage");
    f.file(
        ".local/state/ricepilot/generations/current",
        "not a number\n",
    );
    let err = generations::current(&f.state()).unwrap_err();
    assert_eq!(err.exit_code(), ricepilot::error::ExitCode::Refused);
    assert!(err.to_string().contains("not a number"));
}

#[test]
fn asking_for_a_generation_that_was_never_written_is_refused() {
    let f = Fixture::new_in("m3", "gen_missing");
    let err = generations::load(&f.state(), 2).unwrap_err();
    assert_eq!(err.exit_code(), ricepilot::error::ExitCode::Refused);
    assert!(err.to_string().contains("0002"));
}
