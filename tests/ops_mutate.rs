//! The mutating primitives, against real fixture trees under
//! `target/fixtures/` (D5: `/tmp` is a tmpfs with a different `st_dev`, and
//! this whole module is about what `rename(2)` does).
//!
//! Both `ExchangeMode`s are exercised everywhere an exchange appears. The
//! fallback is not allowed to go untested just because this kernel supports
//! `renameat2` — the machine that needs the fallback is by definition not the
//! machine the tests ran on.

mod common;

use std::path::Path;

use common::Fixture;
use ricepilot::ops::mutate::{self, ExchangeMode};
use ricepilot::ops::read;

const BOTH: [ExchangeMode; 2] = [ExchangeMode::Renameat2, ExchangeMode::Fallback];

fn target_of(p: &Path) -> std::path::PathBuf {
    read::readlink(p).unwrap()
}

#[test]
fn probe_answers_and_leaves_its_two_links_behind() {
    let f = Fixture::new_in("m2", "probe");
    let state = f.state();

    let first = mutate::probe_exchange(&state).unwrap();
    // Re-probing reuses the same two links rather than accumulating more.
    let second = mutate::probe_exchange(&state).unwrap();
    assert_eq!(first, second);

    for name in [".rp-probe-a", ".rp-probe-b"] {
        assert!(
            read::lstat(&state.join(name)).unwrap().is_some(),
            "probe link {name} should still be there for the next run"
        );
    }
}

#[test]
fn make_dirs_creates_a_chain_and_is_idempotent() {
    let f = Fixture::new_in("m2", "mkdirs");
    let deep = f.path("a/b/c/d");
    mutate::make_dirs(&deep).unwrap();
    mutate::make_dirs(&deep).unwrap();
    assert_eq!(read::lstat(&deep).unwrap().unwrap().kind, read::Kind::Dir);
}

#[test]
fn make_dirs_refuses_to_traverse_a_symlinked_component() {
    let f = Fixture::new_in("m2", "mkdirs_link");
    let real = f.dir("real");
    f.link("via", &real);

    // D9: the component walk refuses a symlinked intermediate rather than
    // following it. Creating `via/x` would really create `real/x`.
    let err = mutate::make_dirs(&f.path("via/x")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("intermediate path component is a symlink"),
        "{msg}"
    );
    assert!(
        read::lstat(&real.join("x")).unwrap().is_none(),
        "nothing should have been created through the link"
    );
}

#[test]
fn a_staged_link_is_never_silently_reused() {
    let f = Fixture::new_in("m2", "staged_exists");
    let root = f.dir("rice/new");
    let staged = f.path(".config/hypr.rp-tmp-0");
    f.dir(".config");

    mutate::create_symlink_tmp(&staged, &root).unwrap();
    // A leftover from a crashed run is evidence, not a free slot.
    let err = mutate::create_symlink_tmp(&staged, &root).unwrap_err();
    assert!(err.to_string().contains("creating symlink"), "{err}");
    assert_eq!(target_of(&staged), root);
}

#[test]
fn exchange_swaps_both_ways_in_both_modes() {
    for mode in BOTH {
        let f = Fixture::new_in("m2", &format!("exchange_{}", mode.as_str()));
        let old = f.dir("rice/old/hypr");
        let new = f.dir("rice/new/hypr");
        f.dir(".config");
        let dest = f.link(".config/hypr", &old);
        let staged = f.path(".config/hypr.rp-tmp-0");
        f.clear(".config/hypr.rp-tmp-0");
        f.clear(".config/hypr.rp-tmp-0.rp-swap");
        mutate::create_symlink_tmp(&staged, &new).unwrap();

        mutate::exchange(mode, &dest, &staged).unwrap();

        // The postcondition is identical for both modes: that is the whole
        // reason the fallback is three renames rather than two.
        assert_eq!(target_of(&dest), new, "{mode:?}");
        assert_eq!(target_of(&staged), old, "{mode:?}");
        assert!(
            read::lstat(&mutate::fallback_slot(&staged))
                .unwrap()
                .is_none(),
            "{mode:?}: the fallback's scratch name must not survive"
        );
    }
}

#[test]
fn an_exchange_never_touches_either_profile_tree() {
    for mode in BOTH {
        let case = format!("exchange_inodes_{}", mode.as_str());
        let f = Fixture::new_in("m2", &case);
        let old = f.dir("rice/old/hypr");
        let new = f.dir("rice/new/hypr");
        f.file("rice/old/hypr/hyprland.conf", "old\n");
        f.file("rice/new/hypr/hyprland.conf", "new\n");
        f.dir(".config");
        let dest = f.link(".config/hypr", &old);
        let staged = f.path(".config/hypr.rp-tmp-0");
        f.clear(".config/hypr.rp-tmp-0");
        f.clear(".config/hypr.rp-tmp-0.rp-swap");
        mutate::create_symlink_tmp(&staged, &new).unwrap();

        let before = [
            f.ident("rice/old/hypr"),
            f.ident("rice/new/hypr"),
            f.ident("rice/old/hypr/hyprland.conf"),
            f.ident("rice/new/hypr/hyprland.conf"),
        ];
        let mtimes = [read::mtime_ns(&old).unwrap(), read::mtime_ns(&new).unwrap()];

        mutate::exchange(mode, &dest, &staged).unwrap();

        let after = [
            f.ident("rice/old/hypr"),
            f.ident("rice/new/hypr"),
            f.ident("rice/old/hypr/hyprland.conf"),
            f.ident("rice/new/hypr/hyprland.conf"),
        ];
        assert_eq!(
            before, after,
            "{mode:?}: a switch re-points links, it does not touch targets"
        );
        assert_eq!(
            mtimes,
            [read::mtime_ns(&old).unwrap(), read::mtime_ns(&new).unwrap()],
            "{mode:?}"
        );
    }
}

#[test]
fn the_attic_never_replaces_what_is_already_in_it() {
    let f = Fixture::new_in("m2", "attic_collision");
    let attic = f.attic("20260921T000000Z");
    let old = f.dir("rice/old/hypr");
    f.dir(".config");

    let rel = Path::new("home/u/.config/hypr");

    let a = f.link(".config/hypr", &old);
    let landed_a = mutate::rename_to_attic(&a, &attic, rel).unwrap();
    assert_eq!(landed_a, attic.join(rel));

    let b = f.link(".config/hypr", &f.dir("rice/other/hypr"));
    let landed_b = mutate::rename_to_attic(&b, &attic, rel).unwrap();

    assert_ne!(landed_a, landed_b, "the second must not land on the first");
    assert_eq!(target_of(&landed_a), old, "the first must still be intact");
    assert!(read::lstat(&a).unwrap().is_none(), "it really moved");
}

#[test]
fn rename_within_refuses_to_replace_an_occupied_path() {
    let f = Fixture::new_in("m2", "rename_occupied");
    let from = f.link("from", &f.dir("rice/x"));
    let to = f.link("to", &f.dir("rice/y"));

    let err = mutate::rename_within(&from, &to).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("refusing to replace it"), "{msg}");
    assert!(read::lstat(&from).unwrap().is_some(), "zero side effects");
}

#[test]
fn write_atomic_replaces_content_and_leaves_no_temp_behind() {
    let f = Fixture::new_in("m2", "write_atomic");
    let path = f.path(".local/state/ricepilot/journal/current.toml");

    mutate::write_atomic(&path, b"first\n").unwrap();
    assert_eq!(read::slurp(&path).unwrap(), "first\n");

    mutate::write_atomic(&path, b"second\n").unwrap();
    assert_eq!(read::slurp(&path).unwrap(), "second\n");

    let names: Vec<String> = read::list_dir(path.parent().unwrap())
        .unwrap()
        .into_iter()
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["current.toml".to_string()]);
}

/// `Meta` carries everything one `fstatat` can answer, so a caller that needs
/// the mode, the ownership and the mtime of a path gets them from a single
/// syscall rather than from three that could describe three different files.
#[test]
fn meta_carries_mode_ownership_and_mtime_from_one_stat() {
    let f = Fixture::new_in("m2", "meta_fields");
    let file = f.file("rice/one/marker", "content\n");
    let m = read::lstat(&file).unwrap().unwrap();

    assert_eq!(m.kind, read::Kind::File);
    assert_eq!(m.uid, rustix_uid());
    assert_ne!(m.mtime_ns, 0);
    assert_eq!(read::mtime_ns(&file).unwrap(), m.mtime_ns);

    // A symlink is stat'ed as itself, never as what it points at.
    let link = f.link(".config/marker", &file);
    let lm = read::lstat(&link).unwrap().unwrap();
    assert_eq!(lm.kind, read::Kind::Symlink);
    assert_ne!(lm.ino, m.ino);
}

/// Taken through the same interface the crate uses, so the assertion above is
/// about ricepilot's view of the file rather than about the test's.
fn rustix_uid() -> u32 {
    std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap())
}

/// Asking for the mtime of something that is not there is a refusal naming the
/// path, not a zero that a comparison would silently accept.
#[test]
fn an_absent_path_has_no_modification_time() {
    let f = Fixture::new_in("m2", "meta_absent");
    let err = read::mtime_ns(&f.path(".config/nothing")).unwrap_err();
    assert_eq!(err.exit_code(), ricepilot::error::ExitCode::Refused);
}
