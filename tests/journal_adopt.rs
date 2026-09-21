//! Recovery of an interrupted `adopt`, state by state.
//!
//! `adopt` passes through a state no `switch` ever produces: a managed
//! destination that is a **real directory**. `journal::Entry` cannot describe
//! it — it models a destination by its link target, and `side_of` refuses
//! anything that is not a symlink — so `adopt` has a record type of its own
//! whose pre-state is a `(dev, ino)` (D46).
//!
//! Each test below puts the filesystem into one of the states a crash can
//! leave and asserts what recovery decides, without running it. The crash
//! harness in `tests/crash_adopt_command.rs` then does the same thing for
//! real, by aborting the command.

mod common;

use std::path::{Path, PathBuf};

use common::Fixture;
use ricepilot::journal::{self, Action, Adopt, Direction, Journal, Side};
use ricepilot::ops::mutate::ExchangeMode;

struct Case {
    f: Fixture,
    j: Journal,
    dest: PathBuf,
    staged: PathBuf,
    swap: PathBuf,
    target: PathBuf,
}

/// A live `~/.config/hypr` directory, a copy of it inside a profile, and the
/// journal `adopt` would have written before it touched anything.
fn case(name: &str) -> Case {
    let f = Fixture::new_in("m4adopt", name);
    let dest = f.dir(".config/hypr");
    f.file(".config/hypr/hyprland.conf", "live\n");
    let target = f.dir(".local/share/ricepilot/profiles/mine/hypr");
    f.file(
        ".local/share/ricepilot/profiles/mine/hypr/hyprland.conf",
        "live\n",
    );

    let meta = ricepilot::ops::read::lstat(&dest).unwrap().unwrap();
    let staged = f.path(".config/hypr.rp-tmp-0");
    let swap = ricepilot::ops::mutate::fallback_slot(&staged);
    let j = Journal::for_adopt(
        "20260921T000000Z",
        "mine",
        f.state().join("attic").join("20260921T000000Z"),
        ExchangeMode::Renameat2,
        Adopt {
            dest: dest.clone(),
            dir_dev: meta.dev,
            dir_ino: meta.ino,
            staged: staged.clone(),
            new_target: target.clone(),
            attic_rel: Path::new("adopted").join("hypr"),
        },
        &[],
    );
    Case {
        f,
        j,
        dest,
        staged,
        swap,
        target,
    }
}

fn recovery(c: &Case) -> journal::Recovery {
    journal::plan_recovery(&c.j).unwrap()
}

/// Nothing happened yet: the directory is live and nothing is staged. There
/// is nothing to finish, so the adopt is abandoned and the user's directory
/// is not touched.
#[test]
fn before_anything_started_the_adopt_is_abandoned() {
    let c = case("not_started");
    let r = recovery(&c);
    assert_eq!(r.direction, Direction::Backward);
    assert_eq!(r.statuses[0].side, Side::Old);
    assert!(
        r.actions
            .iter()
            .all(|a| matches!(a, Action::FsyncDir { .. })),
        "nothing but fsyncs: {:?}",
        r.actions
    );
}

/// The link was staged and the exchange had not happened. Still nothing that
/// took effect, so the staged link goes to the attic and the directory stays.
#[test]
fn a_staged_link_with_no_exchange_is_abandoned_into_the_attic() {
    let c = case("staged_only");
    ricepilot::ops::mutate::create_symlink(&c.staged, &c.target).unwrap();

    let r = recovery(&c);
    assert_eq!(r.direction, Direction::Backward);
    assert_eq!(r.statuses[0].side, Side::Old);
    assert_eq!(
        r.actions[0],
        Action::ToAttic {
            from: c.staged.clone(),
            rel: Path::new("adopted").join("hypr.staged"),
        },
        "the abandoned link lands somewhere the attic distinguishes from the directory"
    );
}

/// The exchange happened: the destination is the new link and the user's
/// directory is sitting at the staging name. Forward is the only direction,
/// and the directory is *moved* to the attic, never removed.
#[test]
fn after_the_exchange_the_displaced_directory_goes_to_the_attic() {
    let c = case("exchanged");
    ricepilot::ops::mutate::create_symlink(&c.staged, &c.target).unwrap();
    ricepilot::ops::mutate::exchange(ExchangeMode::Renameat2, &c.dest, &c.staged).unwrap();

    let r = recovery(&c);
    assert_eq!(r.direction, Direction::Forward);
    assert_eq!(r.statuses[0].side, Side::New);
    assert_eq!(
        r.actions[0],
        Action::ToAttic {
            from: c.staged.clone(),
            rel: Path::new("adopted").join("hypr"),
        }
    );
}

/// Fully done. A second `recover` finds nothing left to do, which is what
/// makes recovery idempotent.
#[test]
fn a_finished_adopt_has_nothing_left_to_do() {
    let c = case("finished");
    // The destination in its final shape: the link in place, the user's
    // directory already in the attic.
    let attic = c.f.state().join("attic").join("20260921T000000Z");
    ricepilot::ops::mutate::make_dirs(&attic).unwrap();
    ricepilot::ops::mutate::rename_to_attic(&c.dest, &attic, Path::new("adopted/hypr")).unwrap();
    ricepilot::ops::mutate::create_symlink(&c.dest, &c.target).unwrap();

    let r = recovery(&c);
    assert_eq!(r.direction, Direction::Forward);
    assert_eq!(r.statuses[0].side, Side::New);
    assert!(
        r.actions
            .iter()
            .all(|a| matches!(a, Action::FsyncDir { .. })),
        "nothing but fsyncs: {:?}",
        r.actions
    );
}

/// The fallback exchange's window: the directory is parked at the staging
/// name, the new link at the scratch name, and the destination is empty.
/// Nothing else can make an adopted destination absent, so this is decidable.
#[test]
fn the_fallback_window_is_finished_forward() {
    let c = case("fallback_window");
    ricepilot::ops::mutate::create_symlink(&c.swap, &c.target).unwrap();
    ricepilot::ops::mutate::rename_within(&c.dest, &c.staged).unwrap();

    let r = recovery(&c);
    assert_eq!(r.direction, Direction::Forward);
    assert_eq!(r.statuses[0].side, Side::InFlight);
    assert_eq!(
        r.actions[0],
        Action::Rename {
            from: c.swap.clone(),
            to: c.dest.clone(),
        }
    );
    assert_eq!(
        r.actions[1],
        Action::ToAttic {
            from: c.staged.clone(),
            rel: Path::new("adopted").join("hypr"),
        }
    );
}

/// The one that matters most: between the journal and the crash, something
/// replaced the directory. Its inode no longer matches, so ricepilot refuses
/// rather than moving a directory it never looked at into the attic.
///
/// This is why the pre-state is a `(dev, ino)` and not a path.
#[test]
fn a_directory_that_was_replaced_is_refused_rather_than_displaced() {
    let c = case("replaced");
    let attic = c.f.state().join("attic").join("replaced-by-hand");
    ricepilot::ops::mutate::make_dirs(&attic).unwrap();
    ricepilot::ops::mutate::rename_to_attic(&c.dest, &attic, Path::new("hypr")).unwrap();
    // An installer puts an identical-looking directory back.
    c.f.dir(".config/hypr");
    c.f.file(".config/hypr/hyprland.conf", "live\n");

    let e = journal::plan_recovery(&c.j).unwrap_err();
    assert_eq!(e.exit_code(), ricepilot::error::ExitCode::Refused);
    let msg = e.to_string();
    assert!(
        msg.contains("a different directory"),
        "the refusal must say what it found: {msg}"
    );
}

/// A journal written before `adopt` existed still parses, and one with an
/// adopt record round-trips. An in-flight operation from an older ricepilot
/// has to stay recoverable by a newer one.
#[test]
fn the_record_round_trips_and_older_journals_still_parse() {
    let c = case("serde");
    let path = c.f.state().join("journal").join("current.toml");
    journal::write(&path, &c.j).unwrap();
    assert_eq!(journal::read_current(&path).unwrap().as_ref(), Some(&c.j));

    // The shape a pre-M4 ricepilot wrote: no `adopt` key at all.
    let older = "id = \"x\"\nprofile = \"p\"\nattic = \"/home/u/attic\"\nexchange_mode = \
                 \"renameat2\"\nentries = []\nops = []\n";
    let parsed: Journal = toml::from_str(older).unwrap();
    assert!(parsed.adopt.is_empty());
}
