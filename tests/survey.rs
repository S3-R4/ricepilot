//! The tree survey: absolute symlinks, owner-only files, uncopyable paths,
//! and the live links `init` finds without trusting a documented layout.

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use common::Fixture;
use ricepilot::survey;

fn tree(case: &str) -> Fixture {
    let f = Fixture::new_in("m4survey", case);
    f.dir("rice/zen");
    f.file("rice/hyprland.conf", "monitor=\n");
    f.file("rice/zen/userChrome.css", "* {}\n");
    f.file("rice/token", "secret\n");
    std::fs::set_permissions(f.path("rice/token"), std::fs::Permissions::from_mode(0o600)).unwrap();
    f
}

#[test]
fn an_absolute_link_is_reported_and_says_whether_it_leaves_the_tree() {
    let f = tree("absolute");
    // The shape the target machine really has.
    f.link("rice/inside.css", &f.path("rice/zen/userChrome.css"));
    f.link("rice/outside.css", &f.path("elsewhere/other.css"));
    f.link("rice/relative.conf", Path::new("hyprland.conf"));

    let s = survey::survey(&f.path("rice")).unwrap();
    let names: Vec<(&str, bool)> = s
        .absolute_links
        .iter()
        .map(|l| (l.rel.as_str(), l.inside))
        .collect();
    assert_eq!(names, vec![("inside.css", true), ("outside.css", false)]);
    assert_eq!(
        s.absolute_links[0].target,
        f.path("rice/zen/userChrome.css")
    );
    // A relative link is not reported: it travels with the tree.
    assert_eq!(s.links, 3);
}

#[test]
fn owner_only_files_are_reported_with_their_mode() {
    let f = tree("private");
    let s = survey::survey(&f.path("rice")).unwrap();
    assert_eq!(s.private.len(), 1);
    assert_eq!(s.private[0].rel, "token");
    assert_eq!(s.private[0].mode, 0o600);
}

#[test]
fn counts_cover_the_whole_tree_and_the_root_directory() {
    let f = tree("counts");
    let s = survey::survey(&f.path("rice")).unwrap();
    // rice, rice/zen
    assert_eq!(s.dirs, 2);
    assert_eq!(s.files, 3);
    assert_eq!(
        s.bytes,
        "monitor=\n".len() as u64 + "* {}\n".len() as u64 + "secret\n".len() as u64
    );
    assert!(
        !s.is_quiet(),
        "a mode-600 file is something to decide about"
    );
}

#[test]
fn a_socket_is_reported_as_uncopyable_rather_than_counted_as_a_file() {
    let f = tree("socket");
    std::os::unix::net::UnixListener::bind(f.path("rice/app.sock")).unwrap();
    let s = survey::survey(&f.path("rice")).unwrap();
    assert_eq!(s.uncopyable, vec!["app.sock".to_string()]);
    assert_eq!(s.files, 3);
}

/// `init` finds the rice's links by `lstat`ing every entry of the config
/// directory, never by trusting a documented list — the target machine's
/// documented VSCodium file links do not exist because the installer raced.
#[test]
fn live_links_into_a_root_are_found_and_everything_else_is_left_alone() {
    let f = Fixture::new_in("m4survey", "live_links");
    let root = f.dir("rice");
    f.dir("rice/hypr");
    f.dir("rice/foot");
    f.dir("elsewhere/other");

    f.dir(".config");
    f.link(".config/hypr", &root.join("hypr"));
    f.link(".config/foot", &root.join("foot"));
    // A link into somewhere else entirely: not the rice, not adopted.
    f.link(".config/mako", &f.path("elsewhere/other"));
    // A real directory: a user's own config, not a rice link.
    f.dir(".config/nvim");
    // A link into the rice that points at nothing.
    f.link(".config/btop", &root.join("btop"));

    let found = survey::links_into(&f.path(".config"), &root).unwrap();
    let names: Vec<(String, bool)> = found
        .iter()
        .map(|l| {
            (
                l.dest.file_name().unwrap().to_string_lossy().into_owned(),
                l.points_at_dir,
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("btop".to_string(), false),
            ("foot".to_string(), true),
            ("hypr".to_string(), true),
        ]
    );
}
