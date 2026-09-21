//! `ricepilot adopt`, end to end — the five-shape table's `adopt` column,
//! the confirmation, and what happens to the user's directory.
//!
//! The confirmation is driven by writing to the command's stdin, not by a
//! flag that skips it. There is no flag that skips it (D47): what these
//! tests exercise is the question a user is really asked and the answer
//! ricepilot really parses.

mod common;

use std::io::Write as _;
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

/// Run the binary with `answer` on stdin. `None` means stdin is an empty
/// pipe — the "the user pressed ctrl-D" case, which must count as no.
fn run(f: &Fixture, args: &[&str], answer: Option<&str>) -> Run {
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
    if let Some(a) = answer {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{a}\n").as_bytes())
            .unwrap();
    } else {
        drop(child.stdin.take());
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

/// A machine with one of each shape at a managed-looking destination, plus a
/// profile with its own payload to adopt into.
struct Machine {
    f: Fixture,
    profile: PathBuf,
}

fn machine(case: &str) -> Machine {
    let f = Fixture::new_in("m4ad", case);
    // A profile with its own payload — not by-reference, since adopt copies
    // into it.
    let profile = f.profile("mine", "name = \"mine\"\nvolatile = []\ngenerated = []\n");

    // Shape 3: a real directory, with a nested file, a mode-600 file and a
    // symlink in it — the thing adopt exists for.
    f.dir(".config/hypr/scripts");
    f.file(".config/hypr/hyprland.conf", "monitor=,preferred,auto,1\n");
    f.file(".config/hypr/scripts/configs.fish", "#!/usr/bin/fish\n");
    f.file(".config/hypr/secret.token", "swordfish\n");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            f.path(".config/hypr/secret.token"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    f.link(".config/hypr/self.conf", Path::new("hyprland.conf"));

    // Shape 2: a foreign symlink, pointing at a directory that is nobody's
    // profile.
    f.dir("elsewhere/foot");
    f.link(".config/foot", &f.path("elsewhere/foot"));
    // Shape 2 again, dangling.
    f.link(".config/btop", &f.path("elsewhere/gone"));
    // Shape 4.
    f.file(".config/starship.toml", "format = ''\n");
    // Shape 5.
    f.clear(".config/waybar");

    Machine { f, profile }
}

#[test]
fn a_dry_run_shows_the_whole_thing_and_changes_nothing() {
    let m = machine("dry");
    let r = run(&m.f, &["adopt", "~/.config/hypr", "--into", "mine"], None);
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!("adopt_dry_run", r.stdout);

    // R4: zero side effects. Still a real directory, nothing in the profile,
    // no journal, no probe links.
    assert_eq!(
        read::lstat(&m.f.path(".config/hypr"))
            .unwrap()
            .unwrap()
            .kind,
        read::Kind::Dir
    );
    assert_eq!(
        read::lstat_or_absent(&m.profile.join("hypr")).unwrap(),
        None
    );
    assert_eq!(
        read::lstat_or_absent(&m.f.state().join("journal/current.toml")).unwrap(),
        None
    );
    assert_eq!(
        read::lstat_or_absent(&m.f.state().join(".rp-probe-a")).unwrap(),
        None
    );
}

/// The gate's "nothing is ever deleted": after `adopt --commit` the
/// displaced directory is in the attic *with its contents intact*, and that
/// is asserted by reading them back.
#[test]
fn a_committed_adopt_links_the_path_and_the_directory_survives_in_the_attic() {
    let m = machine("commit");
    let before = m.f.ident(".config/hypr");

    let r = run(
        &m.f,
        &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
        Some("y"),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    insta::assert_snapshot!("adopt_commit", r.stdout);

    // The destination is now a link into the profile.
    let dest = m.f.path(".config/hypr");
    assert_eq!(
        read::lstat(&dest).unwrap().unwrap().kind,
        read::Kind::Symlink
    );
    assert_eq!(read::readlink(&dest).unwrap(), m.profile.join("hypr"));

    // The copy is faithful: content, the 600 mode, and the symlink as a
    // symlink.
    let copy = m.profile.join("hypr");
    assert_eq!(
        read::slurp(&copy.join("hyprland.conf")).unwrap(),
        "monitor=,preferred,auto,1\n"
    );
    assert_eq!(
        read::lstat(&copy.join("secret.token"))
            .unwrap()
            .unwrap()
            .mode,
        0o600
    );
    assert_eq!(
        read::readlink(&copy.join("self.conf")).unwrap(),
        Path::new("hyprland.conf")
    );

    // And the original directory is in the attic — the same inode it always
    // was, with its contents readable.
    let attic = find_in_attic(&m.f, "hypr");
    let moved = read::lstat(&attic).unwrap().unwrap();
    assert_eq!((moved.dev, moved.ino), before, "it was moved, not re-made");
    assert_eq!(
        read::slurp(&attic.join("hyprland.conf")).unwrap(),
        "monitor=,preferred,auto,1\n"
    );
    assert_eq!(
        read::slurp(&attic.join("scripts/configs.fish")).unwrap(),
        "#!/usr/bin/fish\n"
    );

    // The ledger owns it, so `switch` can now act on it.
    let led = ricepilot::ledger::load(&m.f.state().join("ledger.toml")).unwrap();
    assert_eq!(led.owned_dests(), vec![dest]);

    // The journal was retired, not left behind.
    assert_eq!(
        read::lstat_or_absent(&m.f.state().join("journal/current.toml")).unwrap(),
        None
    );
}

/// The manifest and the ledger must agree after an adopt. If the manifest
/// did not gain the path, the very next `switch` into the same profile
/// would find a ledger entry the target state does not include and *retire*
/// the link adopt had just created — displacing it into the attic and
/// leaving the destination empty (D51).
#[test]
fn after_an_adopt_the_profile_claims_the_path_and_a_switch_is_a_no_op() {
    let m = machine("declares");
    assert_eq!(
        run(
            &m.f,
            &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
            Some("y")
        )
        .code,
        0
    );

    let shown = run(&m.f, &["show", "mine"], None);
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    insta::assert_snapshot!("adopt_then_show", shown.stdout);

    // And the switch that would have retired it has nothing to do instead.
    let planned = run(&m.f, &["plan", "mine"], None);
    assert_eq!(planned.code, 0, "{}", planned.stderr);
    assert!(
        planned.stdout.contains("owned link"),
        "the adopted link must read as owned:\n{}",
        planned.stdout
    );
    assert!(
        planned.stdout.contains("nothing to do"),
        "and there must be nothing to switch:\n{}",
        planned.stdout
    );

    let switched = run(&m.f, &["switch", "mine", "--commit"], None);
    assert_eq!(switched.code, 0, "{}", switched.stderr);
    assert_eq!(
        read::readlink(&m.f.path(".config/hypr")).unwrap(),
        m.profile.join("hypr"),
        "the switch retired the link adopt had just made"
    );
}

/// Where the displaced directory landed. The attic directory is named after
/// the operation's id, which is a timestamp, so the test finds it rather
/// than predicting it.
fn find_in_attic(f: &Fixture, leaf: &str) -> PathBuf {
    let attic = f.state().join("attic");
    for id in read::list_dir(&attic).unwrap() {
        let candidate = attic.join(id).join(
            f.path(".config")
                .strip_prefix("/")
                .unwrap()
                .join(leaf)
                .as_path(),
        );
        if read::lstat_or_absent(&candidate).unwrap().is_some() {
            return candidate;
        }
    }
    panic!("nothing named {leaf} in {}", attic.display());
}

/// R6 in its strongest form: the confirmation is not a formality. Answering
/// anything but yes — including not answering at all — leaves the machine
/// exactly as it was.
#[test]
fn an_unconfirmed_adopt_touches_nothing() {
    for (case, answer) in [("said_no", Some("n")), ("said_nothing", None)] {
        let m = machine(case);
        let before = m.f.ident(".config/hypr");

        let r = run(
            &m.f,
            &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
            answer,
        );
        assert_eq!(
            r.code,
            ricepilot::error::ExitCode::Refused as i32,
            "{case}: {}",
            r.stdout
        );
        if case == "said_no" {
            insta::assert_snapshot!("adopt_declined", format!("{}{}", r.stdout, r.stderr));
        }

        assert_eq!(
            m.f.ident(".config/hypr"),
            before,
            "{case}: the directory moved"
        );
        assert_eq!(
            read::lstat(&m.f.path(".config/hypr"))
                .unwrap()
                .unwrap()
                .kind,
            read::Kind::Dir
        );
        assert_eq!(
            read::lstat_or_absent(&m.profile.join("hypr")).unwrap(),
            None,
            "{case}: it copied before asking"
        );
        assert_eq!(
            read::lstat_or_absent(&m.f.state().join("journal/current.toml")).unwrap(),
            None
        );
    }
}

/// The `adopt` column of `docs/DESIGN.md` §5, one row at a time — including
/// the two the gate names explicitly: a foreign symlink and a dangling one.
#[test]
fn the_five_shapes_under_adopt() {
    let m = machine("shapes");
    let mut s = String::new();

    // Shape 1 needs a link ricepilot owns, so: adopt one first, then adopt
    // it again.
    assert_eq!(
        run(
            &m.f,
            &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
            Some("y")
        )
        .code,
        0
    );

    for (shape, path) in [
        ("1  owned link", "~/.config/hypr"),
        ("2  foreign link", "~/.config/foot"),
        ("2  dangling foreign link", "~/.config/btop"),
        ("4  real file", "~/.config/starship.toml"),
        ("5  absent", "~/.config/waybar"),
    ] {
        let r = run(&m.f, &["adopt", path, "--into", "mine"], None);
        s.push_str(&format!(
            "--- shape {shape}\nexit {}\n{}\n",
            r.code, r.stdout
        ));
        if !r.stderr.is_empty() {
            s.push_str(&format!("stderr: {}", r.stderr));
        }
    }
    insta::assert_snapshot!("adopt_five_shapes", s);
}

/// Shape 5 with the profile already holding the source: there is nothing to
/// copy and nothing to displace, so the link is created directly.
#[test]
fn an_absent_destination_is_linked_when_the_profile_already_has_the_source() {
    let m = machine("absent_with_source");
    m.f.dir(".local/share/ricepilot/profiles/mine/waybar");
    m.f.file(".local/share/ricepilot/profiles/mine/waybar/config", "{}\n");

    let r = run(
        &m.f,
        &["adopt", "~/.config/waybar", "--into", "mine", "--commit"],
        Some("y"),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let dest = m.f.path(".config/waybar");
    assert_eq!(
        read::lstat(&dest).unwrap().unwrap().kind,
        read::Kind::Symlink
    );
    assert_eq!(read::readlink(&dest).unwrap(), m.profile.join("waybar"));
}

#[test]
fn every_adopt_refusal() {
    let m = machine("refusals");
    // A by-reference profile: its root is somebody else's directory.
    let clone = m.f.dir("rice/caelestia");
    m.f.dir("rice/caelestia/hypr");
    m.f.profile(
        "byref",
        &format!("name = \"byref\"\nroot = \"{}\"\n", clone.display()),
    );
    // A real directory whose name the profile already holds.
    m.f.dir(".config/dup");
    m.f.file(".config/dup/a.conf", "live\n");
    m.f.dir(".local/share/ricepilot/profiles/mine/dup");

    let mut s = String::new();
    for (what, args) in [
        (
            "into a by-reference profile",
            vec!["adopt", "~/.config/hypr", "--into", "byref", "--commit"],
        ),
        (
            "into a profile that does not exist",
            vec!["adopt", "~/.config/hypr", "--into", "nope", "--commit"],
        ),
        (
            "a denylisted destination",
            vec!["adopt", "~/.ssh", "--into", "mine", "--commit"],
        ),
        (
            "a relative path",
            vec!["adopt", "config/hypr", "--into", "mine", "--commit"],
        ),
        (
            "a path that is not there",
            vec!["adopt", "~/.config/nothing", "--into", "mine", "--commit"],
        ),
        (
            "a name the profile already holds",
            vec!["adopt", "~/.config/dup", "--into", "mine", "--commit"],
        ),
    ] {
        let r = run(&m.f, &args, Some("y"));
        s.push_str(&format!(
            "{what}\n  exit {}  {}\n",
            r.code,
            if r.stderr.is_empty() {
                r.stdout.lines().last().unwrap_or("").to_string()
            } else {
                r.stderr.trim().to_string()
            }
        ));
    }
    insta::assert_snapshot!("adopt_refusals", s);
}
