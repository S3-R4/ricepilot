//! The copier, against real trees.
//!
//! Everything M4 does rests on this: `capture` copies a live directory into a
//! profile, `adopt` copies one before it replaces it with a link, and `init`
//! takes a baseline copy of the whole rice. If the copy is not faithful, all
//! three quietly produce a profile that is not the tree the user thought they
//! captured — and the first evidence of it would be a session that comes up
//! wrong after a switch.
//!
//! So these assert the properties by reading them back, never by the absence
//! of an error.

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use common::Fixture;
use ricepilot::ops::{mutate, read};

fn mode_of(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

/// A tree with the shapes a real config directory has: nested directories, a
/// mode-600 secret, an executable, a relative symlink and an absolute one.
fn tree(case: &str) -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new_in("m4", case);
    let src = f.dir("rice/src");
    f.file("rice/src/config.toml", "key = 1\n");
    f.file("rice/src/secret.env", "TOKEN=hunter2\n");
    f.file("rice/src/run.sh", "#!/bin/sh\necho hi\n");
    f.dir("rice/src/nested/deeper");
    f.file("rice/src/nested/deeper/leaf", "leaf\n");
    std::fs::set_permissions(
        f.path("rice/src/secret.env"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::set_permissions(
        f.path("rice/src/run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::set_permissions(
        f.path("rice/src/nested"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    f.link("rice/src/relative.link", Path::new("config.toml"));
    f.link("rice/src/absolute.link", &f.path("outside/target.css"));
    (f, src)
}

#[test]
fn a_copy_preserves_mode_600_rather_than_widening_it() {
    let (f, src) = tree("copy_modes");
    let dst = f.path("rice/dst");
    mutate::copy_tree(&src, &dst).unwrap();

    assert_eq!(mode_of(&dst.join("secret.env")), 0o600);
    assert_eq!(mode_of(&dst.join("run.sh")), 0o755);
    assert_eq!(mode_of(&dst.join("nested")), 0o750);
    assert_eq!(
        mode_of(&dst.join("config.toml")),
        mode_of(&src.join("config.toml"))
    );
}

#[test]
fn a_symlink_is_copied_as_a_symlink_and_never_followed() {
    let (f, src) = tree("copy_links");
    let dst = f.path("rice/dst");
    mutate::copy_tree(&src, &dst).unwrap();

    let rel = dst.join("relative.link");
    assert_eq!(
        read::lstat(&rel).unwrap().unwrap().kind,
        read::Kind::Symlink
    );
    assert_eq!(read::readlink(&rel).unwrap(), Path::new("config.toml"));

    // The absolute one dangles — its target was never created — and it is
    // still copied as the link it is rather than being resolved or skipped.
    let abs = dst.join("absolute.link");
    assert_eq!(
        read::lstat(&abs).unwrap().unwrap().kind,
        read::Kind::Symlink
    );
    assert_eq!(read::readlink(&abs).unwrap(), f.path("outside/target.css"));
    assert!(!read::resolves(&abs).unwrap());
}

#[test]
fn content_and_times_come_across_and_the_source_is_untouched() {
    let (f, src) = tree("copy_content");
    let dst = f.path("rice/dst");

    let before: Vec<(u64, u64, i64)> = ["config.toml", "nested/deeper/leaf"]
        .iter()
        .map(|r| {
            let m = read::lstat(&src.join(r)).unwrap().unwrap();
            (m.dev, m.ino, m.mtime_ns)
        })
        .collect();

    mutate::copy_tree(&src, &dst).unwrap();

    assert_eq!(read::slurp(&dst.join("config.toml")).unwrap(), "key = 1\n");
    assert_eq!(
        read::slurp(&dst.join("nested/deeper/leaf")).unwrap(),
        "leaf\n"
    );
    for r in ["config.toml", "nested/deeper/leaf", "nested"] {
        assert_eq!(
            read::lstat(&src.join(r)).unwrap().unwrap().mtime_ns,
            read::lstat(&dst.join(r)).unwrap().unwrap().mtime_ns,
            "{r}: modification time was not preserved"
        );
    }

    // A copy reads the source; it must not have written to it.
    let after: Vec<(u64, u64, i64)> = ["config.toml", "nested/deeper/leaf"]
        .iter()
        .map(|r| {
            let m = read::lstat(&src.join(r)).unwrap().unwrap();
            (m.dev, m.ino, m.mtime_ns)
        })
        .collect();
    assert_eq!(before, after);
}

/// The copy is a *different* tree with the same content: the inodes must not
/// be shared. `FICLONE` shares extents, not inodes, so this holds on btrfs
/// too — and it is what makes a profile independent of the live directory it
/// was captured from.
#[test]
fn the_copy_is_a_separate_tree() {
    let (f, src) = tree("copy_identity");
    let dst = f.path("rice/dst");
    mutate::copy_tree(&src, &dst).unwrap();

    let a = read::lstat(&src.join("config.toml")).unwrap().unwrap();
    let b = read::lstat(&dst.join("config.toml")).unwrap().unwrap();
    assert_ne!((a.dev, a.ino), (b.dev, b.ino));
}

/// A blake3 manifest of the source and of the copy must be the same manifest,
/// modulo the root. This is the check `capture` makes before it says the copy
/// succeeded, so it is worth proving the copier can pass it.
#[test]
fn source_and_copy_hash_identically() {
    let (f, src) = tree("copy_hashes");
    let dst = f.path("rice/dst");
    mutate::copy_tree(&src, &dst).unwrap();

    let a = ricepilot::verify::build("p", &src, &[], "now").unwrap();
    let b = ricepilot::verify::build("p", &dst, &[], "now").unwrap();
    assert_eq!(ricepilot::verify::compare(&a, &b), Vec::new());
}

#[test]
fn copying_onto_something_that_exists_is_refused_rather_than_replacing_it() {
    let (f, src) = tree("copy_occupied");
    let dst = f.dir("rice/dst");
    f.file("rice/dst/precious", "do not lose me\n");

    let e = mutate::copy_tree(&src, &dst).unwrap_err();
    assert_eq!(e.exit_code(), ricepilot::error::ExitCode::Refused);
    // And the thing that was there is still there, whole.
    assert_eq!(
        read::slurp(&dst.join("precious")).unwrap(),
        "do not lose me\n"
    );
}

#[test]
fn copying_a_tree_into_itself_is_refused() {
    let (_f, src) = tree("copy_recursive");
    let e = mutate::copy_tree(&src, &src.join("inner")).unwrap_err();
    assert_eq!(e.exit_code(), ricepilot::error::ExitCode::Refused);
}

#[test]
fn a_socket_is_refused_by_name_rather_than_silently_skipped() {
    let (f, src) = tree("copy_socket");
    let sock = src.join("app.sock");
    std::os::unix::net::UnixListener::bind(&sock).unwrap();

    let dst = f.path("rice/dst");
    let e = mutate::copy_tree(&src, &dst).unwrap_err();
    assert!(
        e.to_string().contains("app.sock"),
        "the refusal must name the path: {e}"
    );
    // R4: a refusal has zero side effects. The source is walked for anything
    // uncopyable before the first byte is written, so a tree with a socket in
    // the middle of it does not leave two thirds of a copy behind — and there
    // is no delete to tidy one away with.
    assert_eq!(
        read::lstat_or_absent(&dst).unwrap(),
        None,
        "the refusal left a partial copy behind"
    );
}

/// The stats are what `capture` and `init` report, so they have to be true.
#[test]
fn the_report_counts_what_was_copied() {
    let (f, src) = tree("copy_stats");
    let stats = mutate::copy_tree(&src, &f.path("rice/dst")).unwrap();

    // src, nested, nested/deeper
    assert_eq!(stats.dirs, 3);
    // config.toml, secret.env, run.sh, nested/deeper/leaf
    assert_eq!(stats.files, 4);
    assert_eq!(stats.links, 2);
    assert_eq!(
        stats.bytes,
        [
            "key = 1\n",
            "TOKEN=hunter2\n",
            "#!/bin/sh\necho hi\n",
            "leaf\n"
        ]
        .iter()
        .map(|s| s.len() as u64)
        .sum::<u64>()
    );
    assert!(stats.cloned <= stats.files);
}
