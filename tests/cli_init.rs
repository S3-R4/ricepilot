//! `ricepilot init`, end to end.
//!
//! The fixture is shaped like the target machine: a rice clone with an
//! absolute in-tree symlink and a mode-600 file, directory links in
//! `~/.config` that already point into it, one of them at a **denylisted**
//! destination (caelestia really does link `~/.config/uwsm`), one that
//! dangles, and a user's own directory that has nothing to do with the rice.

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use assert_cmd::cargo::cargo_bin;
use common::{redact, undate, Fixture};
use ricepilot::ops::read;

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

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

/// `answers` is fed on stdin, one line each, in the order the command asks.
fn run(f: &Fixture, args: &[&str], answers: &[&str]) -> Run {
    let mut cmd = Command::new(cargo_bin("ricepilot"));
    cmd.args(args);
    cmd.env_clear();
    for (k, v) in f.env() {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().unwrap();
        for a in answers {
            stdin.write_all(format!("{a}\n").as_bytes()).unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    Run {
        stdout: undate(&normalise_reflinks(&redact(
            &String::from_utf8_lossy(&out.stdout),
            f,
        ))),
        stderr: undate(&redact(&String::from_utf8_lossy(&out.stderr), f)),
        code: out.status.code().unwrap(),
    }
}

struct Machine {
    f: Fixture,
    root: PathBuf,
}

fn machine(case: &str) -> Machine {
    let f = Fixture::new_in("m4init", case);
    let root = f.dir("rice/caelestia");
    for leaf in ["hypr", "foot", "uwsm", "zen"] {
        f.dir(&format!("rice/caelestia/{leaf}"));
    }
    f.file("rice/caelestia/hypr/hyprland.conf", "monitor=\n");
    f.file("rice/caelestia/foot/foot.ini", "font=\n");
    f.file("rice/caelestia/uwsm/env-hyprland", "export X=1\n");
    f.file("rice/caelestia/zen/userChrome.css", "* {}\n");
    f.file("rice/caelestia/.secret", "token\n");
    std::fs::set_permissions(
        f.path("rice/caelestia/.secret"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    // The absolute in-tree link the target machine really has.
    f.link(
        "rice/caelestia/zen/userChrome.link",
        &f.path("rice/caelestia/zen/userChrome.css"),
    );

    f.dir(".config");
    f.link(".config/hypr", &root.join("hypr"));
    f.link(".config/foot", &root.join("foot"));
    // Denylisted: caelestia links this one, and v1 will not manage it.
    f.link(".config/uwsm", &root.join("uwsm"));
    // A link into the rice that resolves to nothing.
    f.link(".config/btop", &root.join("btop"));
    // The user's own directory, and a link somewhere else entirely.
    f.dir(".config/nvim");
    f.dir("elsewhere/mako");
    f.link(".config/mako", &f.path("elsewhere/mako"));

    Machine { f, root }
}

#[test]
fn a_dry_run_reports_everything_and_changes_nothing() {
    let m = machine("dry");
    let r = run(
        &m.f,
        &["init", "--root", &m.root.display().to_string()],
        &[],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!("init_dry_run", r.stdout);

    assert_eq!(
        read::lstat_or_absent(&m.f.path(".local/share/ricepilot/profiles/caelestia")).unwrap(),
        None
    );
    assert_eq!(
        read::lstat_or_absent(&m.f.state().join("baseline")).unwrap(),
        None
    );
    assert_eq!(
        read::lstat_or_absent(&m.f.state().join("ledger.toml")).unwrap(),
        None
    );
}

#[test]
fn a_committed_init_registers_by_reference_and_moves_nothing() {
    let m = machine("commit");
    let before: Vec<(u64, u64)> = [".config/hypr", ".config/foot", ".config/uwsm"]
        .iter()
        .map(|p| m.f.ident(p))
        .collect();

    // hypr: yes. foot: yes. volatile proposal: yes.
    let r = run(
        &m.f,
        &["init", "--root", &m.root.display().to_string(), "--commit"],
        &["y", "y", "y"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!("init_commit", r.stdout);

    // Every live link is the same inode it was: init moves nothing.
    let after: Vec<(u64, u64)> = [".config/hypr", ".config/foot", ".config/uwsm"]
        .iter()
        .map(|p| m.f.ident(p))
        .collect();
    assert_eq!(before, after, "init changed a live link");

    // The profile is by-reference: its root is the clone, and the profile
    // directory holds nothing but the manifest.
    let dir = m.f.path(".local/share/ricepilot/profiles/caelestia");
    assert_eq!(
        read::list_dir(&dir).unwrap(),
        vec![std::ffi::OsString::from("profile.toml")]
    );
    let manifest =
        ricepilot::manifest::parse(&read::slurp(&dir.join("profile.toml")).unwrap()).unwrap();
    assert_eq!(
        manifest.root.as_deref(),
        Some(Path::new("~/rice/caelestia"))
    );
    assert_eq!(manifest.volatile, vec![".secret".to_string()]);
    assert_eq!(manifest.paths.len(), 2, "only the two confirmed links");

    // The ledger owns exactly what was confirmed — not uwsm, which was never
    // offered, and not btop, which points at nothing.
    let led = ricepilot::ledger::load(&m.f.state().join("ledger.toml")).unwrap();
    assert_eq!(
        led.owned_dests(),
        vec![m.f.path(".config/foot"), m.f.path(".config/hypr")]
    );

    // The baseline is a copy: the original is still there, and the copy is a
    // different tree with the same content — the 600 mode and the absolute
    // link included.
    let baseline = m.f.state().join("baseline/caelestia");
    assert_eq!(
        read::slurp(&baseline.join("hypr/hyprland.conf")).unwrap(),
        "monitor=\n"
    );
    assert_eq!(
        read::lstat(&baseline.join(".secret"))
            .unwrap()
            .unwrap()
            .mode,
        0o600
    );
    let link = baseline.join("zen/userChrome.link");
    assert_eq!(
        read::lstat(&link).unwrap().unwrap().kind,
        read::Kind::Symlink
    );
    assert_eq!(
        read::readlink(&link).unwrap(),
        m.f.path("rice/caelestia/zen/userChrome.css"),
        "an absolute link is copied as the link it is"
    );
    assert!(
        read::lstat(&m.root.join("hypr/hyprland.conf"))
            .unwrap()
            .is_some(),
        "the rice clone must still be there: a copy, never a move"
    );
}

/// R6 is not a formality here either: a link the user says no to is not
/// registered, and saying no to everything still registers the profile —
/// which then owns nothing, and `switch` keeps refusing, correctly.
#[test]
fn links_the_user_declines_are_not_registered() {
    let m = machine("declined");
    let r = run(
        &m.f,
        &["init", "--root", &m.root.display().to_string(), "--commit"],
        &["n", "n", "n"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("no links were adopted"), "{}", r.stdout);

    let led = ricepilot::ledger::load(&m.f.state().join("ledger.toml")).unwrap();
    assert!(led.owned_dests().is_empty());
    let dir = m.f.path(".local/share/ricepilot/profiles/caelestia");
    let manifest =
        ricepilot::manifest::parse(&read::slurp(&dir.join("profile.toml")).unwrap()).unwrap();
    assert!(manifest.paths.is_empty());
    assert!(manifest.volatile.is_empty());
}

/// After `init`, the links ricepilot recorded are *owned* — which is the
/// whole point of recording them. `plan` sees shape 1 rather than refusing
/// them as unowned foreign links.
#[test]
fn the_adopted_links_are_owned_afterwards() {
    let m = machine("owned");
    assert_eq!(
        run(
            &m.f,
            &["init", "--root", &m.root.display().to_string(), "--commit"],
            &["y", "y", "n"],
        )
        .code,
        0
    );

    let status = run(&m.f, &["status"], &[]);
    assert_eq!(status.code, 0, "{}", status.stderr);
    insta::assert_snapshot!("init_then_status", status.stdout);

    let planned = run(&m.f, &["plan", "caelestia"], &[]);
    assert_eq!(planned.code, 0, "{}", planned.stderr);
    assert!(
        planned.stdout.contains("owned link"),
        "the links init recorded must read as owned:\n{}",
        planned.stdout
    );
    assert!(
        planned.stdout.contains("nothing to do"),
        "and the rice is already live, so there is nothing to switch:\n{}",
        planned.stdout
    );
}

#[test]
fn every_init_refusal() {
    let m = machine("refusals");
    m.f.file("rice/loose", "not a directory\n");
    let mut s = String::new();

    for (what, args) in [
        ("with no --root", vec!["init"]),
        (
            "a root that is not there",
            vec!["init", "--root", "~/rice/nothing"],
        ),
        (
            "a root that is a regular file",
            vec!["init", "--root", "~/rice/loose"],
        ),
        ("a relative root", vec!["init", "--root", "rice/caelestia"]),
        (
            "a profile name that is a path",
            vec!["init", "--root", "~/rice/caelestia", "--name", "../escape"],
        ),
    ] {
        let r = run(&m.f, &args, &[]);
        s.push_str(&format!("{what}\n  exit {}  {}\n", r.code, r.stderr.trim()));
    }

    // And a second init over the same profile.
    assert_eq!(
        run(
            &m.f,
            &["init", "--root", &m.root.display().to_string(), "--commit"],
            &["n", "n", "n"],
        )
        .code,
        0
    );
    let again = run(
        &m.f,
        &["init", "--root", &m.root.display().to_string(), "--commit"],
        &["n", "n", "n"],
    );
    s.push_str(&format!(
        "registering a profile that already exists\n  exit {}  {}\n",
        again.code,
        again.stderr.trim()
    ));

    insta::assert_snapshot!("init_refusals", s);
}
