//! The blake3 manifest: what it records, what it excludes, and what it
//! notices.
//!
//! The M3 gate asks for two properties — volatile paths are ignored, and a
//! one-byte change anywhere else is flagged — and both are asserted here
//! against a real tree rather than against a hand-built manifest value.

mod common;

use common::Fixture;
use ricepilot::verify::{self, Difference, EntryKind};

const WHEN: &str = "20260921T101112Z";

fn volatile() -> Vec<String> {
    vec![
        "**/fish_variables".into(),
        "shell.json".into(),
        "**/*.log".into(),
        "btop/themes".into(),
    ]
}

/// A small profile tree with one file under each volatile rule and several
/// that no rule covers.
fn tree(case: &str) -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new_in("m3", case);
    let root = f.dir("rice/one");
    f.file(
        "rice/one/hypr/hyprland.conf",
        "bind = SUPER, Q, killactive\n",
    );
    f.file("rice/one/foot/foot.ini", "font=monospace:size=11\n");
    f.file(
        "rice/one/fish/fish_variables",
        "SETUVAR fish_color:brblack\n",
    );
    f.file("rice/one/shell.json", "{\"scheme\":\"mocha\"}\n");
    f.file("rice/one/hypr/hyprland.log", "started\n");
    f.file("rice/one/btop/themes/caelestia.theme", "generated\n");
    f.file("rice/one/btop/btop.conf", "color_theme = caelestia\n");
    f.link("rice/one/hypr/scheme", &f.path("rice/one/foot"));
    (f, root)
}

fn build(root: &std::path::Path) -> verify::TreeManifest {
    verify::build("one", root, &volatile(), WHEN).unwrap()
}

fn paths(m: &verify::TreeManifest) -> Vec<&str> {
    m.entries.iter().map(|e| e.path.as_str()).collect()
}

/// The gate's first half: nothing a volatile glob covers appears in the
/// manifest, and a whole directory named by a glob takes its subtree with it.
#[test]
fn volatile_paths_are_excluded_and_everything_else_is_recorded() {
    let (_f, root) = tree("verify_excludes");
    let m = build(&root);
    let got = paths(&m);

    for excluded in [
        "fish/fish_variables",
        "shell.json",
        "hypr/hyprland.log",
        "btop/themes",
        "btop/themes/caelestia.theme",
    ] {
        assert!(
            !got.contains(&excluded),
            "{excluded} is volatile and must not be hashed; manifest has {got:?}"
        );
    }
    for recorded in ["hypr/hyprland.conf", "foot/foot.ini", "btop/btop.conf"] {
        assert!(
            got.contains(&recorded),
            "{recorded} is missing from {got:?}"
        );
    }
}

/// The gate's second half. One byte, in a file no glob covers.
#[test]
fn a_one_byte_change_is_flagged() {
    let (f, root) = tree("verify_one_byte");
    let before = build(&root);
    f.file("rice/one/foot/foot.ini", "font=monospace:size=12\n");
    let diffs = verify::compare(&before, &build(&root));

    assert_eq!(
        diffs,
        vec![Difference::ContentChanged {
            path: "foot/foot.ini".into(),
            kind: EntryKind::File,
        }]
    );
}

/// The same edit inside a volatile path is not drift, because that path was
/// never hashed. This is the property that keeps `verify` usable at all on a
/// rice whose applications write into it continuously.
#[test]
fn a_change_inside_a_volatile_path_is_not_drift() {
    let (f, root) = tree("verify_volatile_change");
    let before = build(&root);
    f.file("rice/one/fish/fish_variables", "SETUVAR fish_color:red\n");
    f.file("rice/one/btop/themes/caelestia.theme", "regenerated\n");
    f.file("rice/one/hypr/hyprland.log", "started\nstopped\n");

    assert_eq!(verify::compare(&before, &build(&root)), vec![]);
}

/// A symlink is hashed by its target *string*. Editing what it points at is
/// invisible to the link's own row — following it would make the hash a
/// statement about a different tree — and re-pointing it is flagged.
#[test]
fn a_symlink_is_hashed_by_its_target_string_and_never_followed() {
    let (f, root) = tree("verify_symlink");
    let before = build(&root);

    let row = before
        .entries
        .iter()
        .find(|e| e.path == "hypr/scheme")
        .expect("the link is recorded");
    assert_eq!(row.kind, EntryKind::Symlink);
    assert!(
        !paths(&before).iter().any(|p| p.starts_with("hypr/scheme/")),
        "a link to a directory is one row, not a subtree"
    );

    f.link("rice/one/hypr/scheme", &f.path("rice/one/btop"));
    let diffs = verify::compare(&before, &build(&root));
    assert_eq!(
        diffs,
        vec![Difference::ContentChanged {
            path: "hypr/scheme".into(),
            kind: EntryKind::Symlink,
        }]
    );
}

#[test]
fn an_added_path_and_a_departed_one_are_both_reported() {
    let (f, root) = tree("verify_added_gone");
    let before = build(&root);
    f.file("rice/one/foot/extra.ini", "new\n");
    f.clear("rice/one/btop/btop.conf");

    let diffs = verify::compare(&before, &build(&root));
    assert!(diffs.contains(&Difference::Added {
        path: "foot/extra.ini".into(),
        kind: EntryKind::File,
    }));
    assert!(diffs.contains(&Difference::Removed {
        path: "btop/btop.conf".into(),
        kind: EntryKind::File,
    }));
}

/// A file rewritten with exactly what was already in it is not drift. It is
/// reported, separately and in plain words, because R7 means saying what was
/// observed — but it is not folded in among the real changes.
#[test]
fn a_rewrite_with_identical_bytes_is_reported_as_touched_not_as_drift() {
    let (f, root) = tree("verify_touched");
    let before = build(&root);
    std::thread::sleep(std::time::Duration::from_millis(10));
    f.file("rice/one/foot/foot.ini", "font=monospace:size=11\n");

    let diffs = verify::compare(&before, &build(&root));
    assert_eq!(
        diffs,
        vec![Difference::Touched {
            path: "foot/foot.ini".into()
        }]
    );
    assert!(!diffs[0].is_substantive());
}

/// A regular file replaced by a directory of the same name is a change of kind
/// and is reported as one, rather than as a content change whose hash happens
/// to be missing.
#[test]
fn a_path_that_changes_kind_says_so() {
    let (f, root) = tree("verify_kind");
    let before = build(&root);
    f.clear("rice/one/btop/btop.conf");
    f.dir("rice/one/btop/btop.conf");

    assert_eq!(
        verify::compare(&before, &build(&root)),
        vec![Difference::KindChanged {
            path: "btop/btop.conf".into(),
            was: EntryKind::File,
            now: EntryKind::Dir,
        }]
    );
}

#[test]
fn a_mode_change_alone_is_reported_with_both_modes() {
    let (f, root) = tree("verify_mode");
    let before = build(&root);
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(
        f.path("rice/one/foot/foot.ini"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    let diffs = verify::compare(&before, &build(&root));
    assert!(
        matches!(&diffs[0], Difference::MetadataChanged { path, detail }
            if path == "foot/foot.ini" && detail.contains("0600")),
        "got {diffs:?}"
    );
}

#[test]
fn a_manifest_round_trips_through_the_file() {
    let (f, root) = tree("verify_roundtrip");
    let m = build(&root);
    let path = verify::manifest_path(&f.state(), "one");
    verify::save(&path, &m).unwrap();
    assert_eq!(verify::load(&path).unwrap(), Some(m));
}

#[test]
fn nothing_recorded_yet_is_none_rather_than_an_empty_manifest() {
    let f = Fixture::new_in("m3", "verify_unrecorded");
    assert_eq!(
        verify::load(&verify::manifest_path(&f.state(), "one")).unwrap(),
        None
    );
}

/// The glob subset, asserted directly. These are the cases the manifest
/// examples in `docs/DESIGN.md` §3 rely on.
#[test]
fn the_glob_matcher_covers_the_subset_the_manifest_uses() {
    for (pattern, path, expected) in [
        ("**/fish_variables", "fish/fish_variables", true),
        ("**/fish_variables", "fish_variables", true),
        ("**/fish_variables", "a/b/c/fish_variables", true),
        ("**/fish_variables", "fish/fish_variables.bak", false),
        ("shell.json", "shell.json", true),
        ("shell.json", "nested/shell.json", false),
        ("**/*.log", "hypr/hyprland.log", true),
        ("**/*.log", "hypr/hyprland.conf", false),
        ("btop/themes", "btop/themes", true),
        ("btop/themes", "btop/themes/deep/one.theme", true),
        ("btop/themes", "btop/themesong", false),
        ("hypr/*.conf", "hypr/hyprland.conf", true),
        ("hypr/*.conf", "hypr/sub/hyprland.conf", false),
        ("?.conf", "a.conf", true),
        ("?.conf", "ab.conf", false),
        ("*", "anything", true),
        ("*", "nested/anything", true),
    ] {
        assert_eq!(
            verify::glob_matches(pattern, path),
            expected,
            "glob_matches({pattern:?}, {path:?})"
        );
    }
}

/// A pattern built to make a backtracking matcher work hard must cost time,
/// not stack. This would blow up a naive recursive implementation.
#[test]
fn a_pathological_pattern_terminates() {
    let seg = "a".repeat(64);
    assert!(!verify::glob_matches("*a*a*a*a*a*b", &seg));
}
