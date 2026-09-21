//! `ricepilot capture`, end to end, against real fixture trees.
//!
//! The gate items this suite owns: a capture preserves mode 600 and preserves
//! symlinks as symlinks — asserted by reading the mode and the link target
//! back off the copy, not by the absence of an error — and absolute in-tree
//! symlinks are reported.

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use common::{redact, Fixture};
use ricepilot::ops::read;

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// Whether the kernel shared the extents depends on the filesystem under
/// `target/fixtures/` — btrfs on the target machine, ext4 on CI — so the
/// count is replaced with a fixed word. What is being reviewed here is
/// ricepilot's sentence, not the filesystem's capabilities.
fn normalise_reflinks(out: &str) -> String {
    let mut s = String::new();
    for line in out.split_inclusive('\n') {
        match line.find("reflinked:") {
            Some(i) => {
                s.push_str(&line[..i]);
                s.push_str("reflinked: <depends on the filesystem>\n");
            }
            None => s.push_str(line),
        }
    }
    s
}

fn run(f: &Fixture, args: &[&str]) -> Run {
    let mut cmd = Command::new(cargo_bin("ricepilot"));
    cmd.args(args);
    cmd.env_clear();
    for (k, v) in f.env() {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    Run {
        stdout: normalise_reflinks(&redact(&String::from_utf8_lossy(&out.stdout), f)),
        stderr: redact(&String::from_utf8_lossy(&out.stderr), f),
        code: out.status.code().unwrap(),
    }
}

fn mode_of(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

/// A live `~/.config/hypr` with the shapes that matter: a mode-600 file, a
/// relative symlink, and an absolute one pointing back into the tree — the
/// shape caelestia's `userChrome.css` has on the target machine.
fn live_rice(case: &str) -> Fixture {
    let f = Fixture::new_in("m4cap", case);
    f.dir(".config/hypr/scripts");
    f.file(".config/hypr/hyprland.conf", "monitor=,preferred,auto,1\n");
    f.file(".config/hypr/secret.token", "swordfish\n");
    f.file(".config/hypr/scripts/configs.fish", "#!/usr/bin/fish\n");
    std::fs::set_permissions(
        f.path(".config/hypr/secret.token"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    f.link(".config/hypr/relative.conf", Path::new("hyprland.conf"));
    f.link(
        ".config/hypr/userChrome.css",
        &f.path(".config/hypr/scripts/configs.fish"),
    );
    f
}

#[test]
fn a_dry_run_reports_the_tree_and_writes_nothing() {
    let f = live_rice("dry");
    let r = run(
        &f,
        &[
            "capture",
            "mine",
            "--from",
            &f.path(".config/hypr").display().to_string(),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!("capture_dry_run", r.stdout);

    // R4: a dry run has zero side effects.
    assert_eq!(
        read::lstat_or_absent(&f.path(".local/share/ricepilot/profiles/mine")).unwrap(),
        None
    );
    assert_eq!(
        read::lstat_or_absent(&f.path(".local/share/ricepilot/staging")).unwrap(),
        None
    );
}

#[test]
fn a_capture_preserves_mode_600_and_keeps_symlinks_as_symlinks() {
    let f = live_rice("commit");
    let r = run(
        &f,
        &[
            "capture",
            "mine",
            "--commit",
            "--from",
            &f.path(".config/hypr").display().to_string(),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!("capture_commit", r.stdout);

    let copy = f.path(".local/share/ricepilot/profiles/mine/hypr");
    assert_eq!(mode_of(&copy.join("secret.token")), 0o600);
    assert_eq!(
        read::slurp(&copy.join("secret.token")).unwrap(),
        "swordfish\n"
    );

    let rel = copy.join("relative.conf");
    assert_eq!(
        read::lstat(&rel).unwrap().unwrap().kind,
        read::Kind::Symlink
    );
    assert_eq!(read::readlink(&rel).unwrap(), Path::new("hyprland.conf"));

    let abs = copy.join("userChrome.css");
    assert_eq!(
        read::lstat(&abs).unwrap().unwrap().kind,
        read::Kind::Symlink
    );
    assert_eq!(
        read::readlink(&abs).unwrap(),
        f.path(".config/hypr/scripts/configs.fish"),
        "an absolute link is copied as the link it is, never resolved"
    );

    // Nothing was activated: the live directory is still a real directory and
    // the ledger is still empty.
    assert_eq!(
        read::lstat(&f.path(".config/hypr")).unwrap().unwrap().kind,
        read::Kind::Dir
    );
    assert_eq!(
        read::lstat_or_absent(&f.path(".local/state/ricepilot/ledger.toml")).unwrap(),
        None
    );
}

/// The captured profile is a profile: `show` reads it back and `verify` has a
/// manifest to compare against without waiting for a switch.
#[test]
fn the_captured_profile_is_readable_and_verifiable() {
    let f = live_rice("roundtrip");
    let hypr = f.path(".config/hypr").display().to_string();
    assert_eq!(
        run(&f, &["capture", "mine", "--commit", "--from", &hypr]).code,
        0
    );

    let shown = run(&f, &["show", "mine"]);
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    insta::assert_snapshot!("capture_then_show", shown.stdout);

    let verified = run(&f, &["verify", "mine"]);
    assert_eq!(verified.code, 0, "{}", verified.stderr);
    assert!(
        verified.stdout.contains("clean"),
        "a freshly captured profile must verify clean:\n{}",
        verified.stdout
    );
}

/// The gate's "nothing is ever deleted" in its `capture` form: a second
/// capture under the same name does not write over the first.
#[test]
fn capturing_over_an_existing_profile_is_refused() {
    let f = live_rice("exists");
    let hypr = f.path(".config/hypr").display().to_string();
    assert_eq!(
        run(&f, &["capture", "mine", "--commit", "--from", &hypr]).code,
        0
    );

    let again = run(&f, &["capture", "mine", "--commit", "--from", &hypr]);
    assert_eq!(
        again.code,
        ricepilot::error::ExitCode::Refused as i32,
        "{}",
        again.stdout
    );
    // And the first one is intact.
    assert_eq!(
        read::slurp(&f.path(".local/share/ricepilot/profiles/mine/hypr/hyprland.conf")).unwrap(),
        "monitor=,preferred,auto,1\n"
    );
}

#[test]
fn every_capture_refusal() {
    let f = live_rice("refusals");
    f.file(".config/loose.conf", "not a directory\n");
    f.link(".config/linked", &f.path(".config/hypr"));
    f.dir("elsewhere/hypr");
    let mut s = String::new();

    for (what, args) in [
        ("with no --from", vec!["capture", "mine"]),
        (
            "a source that is not there",
            vec!["capture", "mine", "--from", "~/.config/nothing"],
        ),
        (
            "a source that is a regular file",
            vec!["capture", "mine", "--from", "~/.config/loose.conf"],
        ),
        (
            "a source that is a symlink",
            vec!["capture", "mine", "--from", "~/.config/linked"],
        ),
        (
            "two sources with the same final component",
            vec![
                "capture",
                "mine",
                "--from",
                "~/.config/hypr",
                "--from",
                "~/elsewhere/hypr",
            ],
        ),
        (
            "a source with `..` in it",
            vec!["capture", "mine", "--from", "~/.config/../.config/hypr"],
        ),
        (
            "a profile name that is a path",
            vec!["capture", "../escape", "--from", "~/.config/hypr"],
        ),
        (
            "a relative source",
            vec!["capture", "mine", "--from", "config/hypr"],
        ),
    ] {
        let r = run(&f, &args);
        s.push_str(&format!("{what}\n  exit {}  {}\n", r.code, r.stderr.trim()));
    }
    insta::assert_snapshot!("capture_refusals", s);
}
