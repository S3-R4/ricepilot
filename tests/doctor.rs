//! `ricepilot doctor`, end to end: a healthy machine, every problem class it
//! reports, the machine-wide hazards, and — the gate — that it changes
//! nothing. Every run here takes the `(dev, ino, mtime_ns)` of every path in
//! the whole fixture (home, state, profiles, the rice trees) before and
//! after, and fails if one moved or one appeared.
//!
//! Read-only *by construction* is proved separately and textually, at the
//! bottom: `src/doctor.rs` and `src/doctor/` may name only a listed set of
//! pure items, and `src/ops/look.rs` only reads and the two read-only
//! subprocesses (D57).

// Builds and breaks fixture trees with the standard library, and reads the
// sources it checks; the guards it protects only ever scan src/.
#![allow(clippy::disallowed_methods)]

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use common::{identities, paths_after, redact, source, switching, Fixture};
use ricepilot::error::ExitCode;
use ricepilot::ops::read;

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// `common::undate`, plus the `-<n>` `journal::unique_id` appends when two
/// operations share a second (D40): whether they do is a race against the
/// clock, not something these snapshots review.
fn undate(s: &str) -> String {
    let s = common::undate(s);
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("<TS>-") {
        out.push_str(&rest[..i + 4]);
        let tail = &rest[i + 5..];
        let digits = tail.chars().take_while(char::is_ascii_digit).count();
        rest = if digits > 0 {
            &tail[digits..]
        } else {
            &rest[i + 4..]
        };
    }
    out.push_str(rest);
    out
}

/// What differs between machines and is not what the report is about: the
/// fixture's location, timestamps, what `/bin/sh` says (bash here, dash on
/// CI), and inode numbers.
fn normalise(s: &str, f: &Fixture) -> String {
    let s = undate(&redact(s, f));
    let mut out = String::new();
    let mut after_sh = false;
    for line in s.split_inclusive('\n') {
        let t = line.trim_start();
        if after_sh {
            out.push_str("    <what sh said>\n");
            after_sh = false;
        } else if t.starts_with("now       dev ") || t.starts_with("recorded  dev ") {
            let word = t.split_whitespace().next().unwrap();
            out.push_str(&format!("    {word:<9} dev <dev> ino <ino>\n"));
        } else {
            out.push_str(line);
        }
        if t.starts_with("`sh -n` rejects it:") {
            after_sh = true;
        }
    }
    out
}

fn run_with(f: &Fixture, args: &[&str], answer: Option<&str>) -> Run {
    let mut cmd = common::ricepilot(f);
    cmd.args(args);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(a) = answer {
        stdin.write_all(format!("{a}\n").as_bytes()).unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    Run {
        stdout: normalise(&String::from_utf8_lossy(&out.stdout), f),
        stderr: normalise(&String::from_utf8_lossy(&out.stderr), f),
        code: out.status.code().unwrap(),
    }
}

fn run(f: &Fixture, args: &[&str]) -> Run {
    run_with(f, args, None)
}

/// `ricepilot doctor`, asserting it changed nothing anywhere in the fixture.
fn doctor(f: &Fixture) -> Run {
    let before = identities(f);
    let r = run(f, &["doctor"]);
    let after = identities(f);
    assert_eq!(
        before, after,
        "doctor changed the fixture (a path moved, appeared or went away)"
    );
    r
}

/// Two profiles, `old` and `new`, each with a Lua Hyprland config (so no
/// switch in these tests reaches `Hyprland --verify-config`, which the
/// sandbox refuses), switched from `old` to `new` through the real command:
/// a ledger, generations 0000 and 0001, a recorded manifest, a retired
/// journal, an attic and a rescue script, all as ricepilot left them.
fn machine(case: &str) -> switching::Machine {
    let m = switching::build(&format!("doctor_{case}"));
    for root in [&m.old_root, &m.new_root] {
        std::fs::write(root.join("hypr/hyprland.lua"), "-- a lua config\n").unwrap();
    }
    let s = run(&m.f, &["switch", "new", "--commit"]);
    assert_eq!(s.code, 0, "{}", s.stderr);
    m
}

fn unhealthy(r: &Run) {
    assert_eq!(
        r.code,
        ExitCode::Unhealthy as i32,
        "{}{}",
        r.stdout,
        r.stderr
    );
    assert!(r.stderr.is_empty(), "{}", r.stderr);
}

fn rm(p: &Path) {
    let m = std::fs::symlink_metadata(p).unwrap();
    if m.is_dir() {
        std::fs::remove_dir_all(p).unwrap();
    } else {
        std::fs::remove_file(p).unwrap();
    }
}

fn manifest_of(m: &switching::Machine, profile: &str) -> PathBuf {
    m.f.path(&format!(
        ".local/share/ricepilot/profiles/{profile}/profile.toml"
    ))
}

// ---------------------------------------------------------------------------
// A healthy machine
// ---------------------------------------------------------------------------

/// Everything healthy folds into the one `ok:` line; the notes say what was
/// not checked and why; the exit status is 0.
#[test]
fn a_healthy_machine_is_one_line_and_exits_zero() {
    let m = machine("healthy");
    let r = doctor(&m.f);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    insta::assert_snapshot!(r.stdout);
}

/// Before `init`: nothing registered, nothing owned, nothing to report.
#[test]
fn a_machine_ricepilot_has_never_touched() {
    let f = Fixture::new_in("m5doc", "untouched");
    let r = doctor(&f);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    insta::assert_snapshot!(r.stdout);
}

/// doctor takes no lock, so it answers while another ricepilot holds it —
/// which is when someone is most likely to run it.
#[test]
fn doctor_runs_beside_a_held_lock() {
    let m = machine("locked");
    let _held = ricepilot::ops::lock::acquire(&m.f.path(".run/ricepilot.lock")).unwrap();
    let r = doctor(&m.f);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    // And the same guarded command does refuse, so the lock really is held.
    assert_eq!(run(&m.f, &["recover"]).code, ExitCode::Locked as i32);
}

// ---------------------------------------------------------------------------
// Owned links
// ---------------------------------------------------------------------------

#[test]
fn an_owned_link_an_installer_turned_into_a_directory() {
    let m = machine("real_dir");
    rm(&m.foot);
    m.f.file(".config/foot/foot.ini", "font=monospace\n");
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn an_owned_link_that_points_elsewhere() {
    let m = machine("retargeted");
    // Somewhere no generation had it: the generic advice, not D81's.
    m.f.link(".config/foot", &m.new_root.join("hypr"));
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

/// After `rescue.sh` the restored links are not in the ledger. doctor used
/// to advise `switch <the profile rescued from>`, which undoes the rescue.
/// Where the generation before had a link, or had nothing, it now advises
/// what RECOVERY.md step 5 says: set the link aside and roll back (D81).
/// Following that advice to the letter leaves a healthy machine on the
/// generation the script restored.
#[test]
fn after_rescue_sh_doctor_advises_the_rollback_and_it_works() {
    let m = machine("after-rescue");
    let script = m.f.state().join("rescue.sh");
    let out = common::sh(&script).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(m.live(), m.all_old(), "rescue.sh restored generation 0000");

    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
    assert!(
        !r.stdout.contains("ricepilot switch new --commit"),
        "doctor advised undoing the rescue:\n{}",
        r.stdout
    );

    // Do exactly what it printed, in order: the moves, then the rollback.
    let run_lines: Vec<String> = r
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("mv -nT ") || l.starts_with("ricepilot rollback --commit"))
        .map(str::to_string)
        .collect();
    for line in run_lines.iter().filter(|l| l.starts_with("mv ")) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let from = PathBuf::from(parts[2].replace("<HOME>", &m.f.home.display().to_string()));
        let to = PathBuf::from(parts[3].replace("<HOME>", &m.f.home.display().to_string()));
        assert!(read::lstat_or_absent(&to).unwrap().is_none());
        std::fs::rename(&from, &to).unwrap();
    }
    assert!(run_lines
        .iter()
        .any(|l| l.starts_with("ricepilot rollback --commit")));
    let back = run(&m.f, &["rollback", "--commit"]);
    assert_eq!(back.code, 0, "{}\n{}", back.stdout, back.stderr);
    assert_eq!(m.live(), m.all_old());

    let again = doctor(&m.f);
    assert_eq!(again.code, 0, "{}", again.stdout);
}

/// The same after a switch that *retired* a destination: `rescue.sh` puts
/// the older generation's link back at a path the ledger no longer has a
/// row for, and every rollback refuses it as foreign. doctor names it too,
/// so the printed way back is complete (D81). The acceptance run found the
/// gap: caelestia -> bare retires four links.
#[test]
fn after_rescue_sh_a_retired_link_it_restored_is_named_too() {
    let m = machine("after-rescue-retired");
    // new -> old retires btop.
    let s = run(&m.f, &["switch", "old", "--commit"]);
    assert_eq!(s.code, 0, "{}", s.stderr);
    assert_eq!(m.live(), m.all_old());
    let out = common::sh(&m.f.state().join("rescue.sh")).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(m.live(), m.all_new(), "rescue.sh restored generation 0001");

    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
    assert!(r.stdout.contains("<HOME>/.config/btop"), "{}", r.stdout);
    assert!(
        !r.stdout.contains("ricepilot switch old --commit"),
        "{}",
        r.stdout
    );

    for line in r
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("mv -nT "))
    {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let home = m.f.home.display().to_string();
        std::fs::rename(
            parts[2].replace("<HOME>", &home),
            parts[3].replace("<HOME>", &home),
        )
        .unwrap();
    }
    let back = run(&m.f, &["rollback", "--commit"]);
    assert_eq!(back.code, 0, "{}\n{}", back.stdout, back.stderr);
    assert_eq!(m.live(), m.all_new());
    let again = doctor(&m.f);
    assert_eq!(again.code, 0, "{}", again.stdout);
}

/// A `.set-aside` left by an earlier round (RECOVERY.md step 5 makes one per
/// path) is not overwritten by the next: the printed name skips past it, and
/// the move is `mv -nT` in case the name is taken again by paste time (D70).
#[test]
fn a_taken_set_aside_name_is_skipped_and_the_move_never_replaces() {
    let m = machine("taken-aside");
    m.f.link(".config/foot", &m.old_root.join("foot"));
    m.f.link(".config/foot.set-aside", Path::new("/an/earlier/round"));
    let r = doctor(&m.f);
    unhealthy(&r);
    let line = r
        .stdout
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("mv "))
        .expect("a move is printed");
    assert_eq!(
        line,
        "mv -nT <HOME>/.config/foot <HOME>/.config/foot.set-aside-2"
    );
}

#[test]
fn an_owned_link_replaced_by_a_lookalike() {
    let m = machine("lookalike");
    let target = std::fs::read_link(&m.foot).unwrap();
    m.f.link(".config/foot", &target);
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn an_owned_link_that_is_gone() {
    let m = machine("gone");
    rm(&m.btop);
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

// ---------------------------------------------------------------------------
// ricepilot's records
// ---------------------------------------------------------------------------

/// A journal left at `current.toml` — here, a retired one put back, which is
/// exactly the file an interrupted switch leaves.
#[test]
fn an_interrupted_operation_is_the_first_problem() {
    let m = machine("journal");
    put_back_a_journal(&m.f);
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

fn put_back_a_journal(f: &Fixture) {
    let dir = f.state().join("journal");
    let done = read::list_dir(&dir)
        .unwrap()
        .into_iter()
        .find(|n| n.to_string_lossy().starts_with("done-"))
        .unwrap();
    std::fs::copy(dir.join(done), dir.join("current.toml")).unwrap();
}

/// D51: the manifest no longer declares a path the ledger says the profile
/// owns, so the next switch would retire it.
#[test]
fn a_manifest_edited_out_of_step_with_the_ledger() {
    let m = machine("desync");
    let path = manifest_of(&m, "new");
    let text = std::fs::read_to_string(&path).unwrap();
    let cut = text
        .find("\n[[path]]\ndest       = \"~/.config/btop\"")
        .unwrap();
    std::fs::write(&path, &text[..cut + 1]).unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!("desync_path_missing", r.stdout);

    // The path still declared, but from another source than the link's.
    let m = machine("desync_src");
    let path = manifest_of(&m, "new");
    let text = std::fs::read_to_string(&path).unwrap();
    let edited = text.replacen(
        "dest       = \"~/.config/foot\"\nsrc        = \"foot\"",
        "dest       = \"~/.config/foot\"\nsrc        = \"btop\"",
        1,
    );
    assert_ne!(edited, text);
    std::fs::write(&path, edited).unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!("desync_src_differs", r.stdout);
}

/// A profile whose manifest does not parse, and a ledger that does not: each
/// is a problem of its own, and neither stops the rest of the report.
#[test]
fn a_manifest_and_a_ledger_that_do_not_parse() {
    let m = machine("unparsable");
    std::fs::write(manifest_of(&m, "old"), "name = \"old\"\nkind = [\n").unwrap();
    std::fs::write(m.f.state().join("ledger.toml"), "[[entry]]\ndest = 3\n").unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn a_profile_tree_that_has_drifted_from_its_record() {
    let m = machine("drift");
    std::fs::write(m.new_root.join("foot/marker"), "edited by hand\n").unwrap();
    std::fs::write(m.new_root.join("foot/extra.ini"), "added\n").unwrap();
    // Volatile, so not drift.
    std::fs::write(m.new_root.join("foot/noise.log"), "rewritten\n").unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn a_rescue_script_that_is_missing_stale_or_broken() {
    let m = machine("rescue_missing");
    rm(&m.f.state().join("rescue.sh"));
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!("rescue_missing", r.stdout);

    let m = machine("rescue_stale");
    std::fs::write(
        m.f.state().join("rescue.sh"),
        "#!/bin/sh\n# ricepilot rescue script — restores generation 0007.\necho hello\n",
    )
    .unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!("rescue_stale", r.stdout);

    let m = machine("rescue_broken");
    let path = m.f.state().join("rescue.sh");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}if then fi\n")).unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    assert!(r.stdout.contains("`sh -n` rejects it:"), "{}", r.stdout);
    insta::assert_snapshot!("rescue_broken", r.stdout);
}

/// The live profile's requires, through the real pacman. Skipped where
/// there is none.
#[test]
fn the_live_profile_requires_a_package_that_is_not_installed() {
    let found = ricepilot::ops::exec::locate(ricepilot::ops::exec::Allowed::PacmanQuery)
        .unwrap()
        .is_some();
    if !found {
        eprintln!("SKIPPED the_live_profile_requires_a_package_that_is_not_installed: no pacman");
        return;
    }
    let m = machine("requires");
    m.require("new", &["ricepilot-no-such-package-f3a1"]);
    m.require("old", &["ricepilot-no-such-package-f3a1"]);
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

fn hypr_profile(m: &switching::Machine, name: &str, dialect: &str) -> PathBuf {
    let root = m.f.dir(&format!("rice/{name}/hypr"));
    m.f.profile(
        name,
        &format!(
            "name = \"{name}\"\nroot = \"{}\"\nhypr_dialect = \"{dialect}\"\n\n[[path]]\ndest       = \
             \"~/.config/hypr\"\nsrc        = \"hypr\"\nkind       = \"dir-link\"\nactivation = \
             \"relogin\"\n",
            root.parent().unwrap().display()
        ),
    );
    root
}

/// A profile that links `~/.config/hypr` to a tree with no entry file: the
/// first Hyprland to start on it writes a default config into the profile.
/// Beside it, the two Hyprland configs doctor says it did not check: a
/// `hyprland.conf` (checked by `plan`, not by doctor, which builds no
/// scratch copy) and a `hyprland.lua` under a manifest that claims `conf`.
#[test]
fn hypr_trees_with_no_entry_file_or_one_doctor_cannot_check() {
    let m = machine("no_entry");
    let conf = hypr_profile(&m, "withconf", "conf");
    std::fs::write(conf.join("hyprland.conf"), "exec-once = touch /nowhere\n").unwrap();
    let lua = hypr_profile(&m, "luaconf", "conf");
    std::fs::write(lua.join("hyprland.lua"), "-- lua\n").unwrap();
    let bare = hypr_profile(&m, "bare", "conf");
    std::fs::write(bare.join("monitors.conf"), "monitor=,preferred,auto,1\n").unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

// ---------------------------------------------------------------------------
// What M4 made possible: adopt
// ---------------------------------------------------------------------------

/// A profile with its own payload and a real `~/.config/hypr`, adopted
/// through the real command.
fn adopted(case: &str) -> Fixture {
    let f = Fixture::new_in("m5doc", case);
    f.profile("mine", "name = \"mine\"\nvolatile = []\ngenerated = []\n");
    f.file(".config/hypr/hyprland.lua", "-- a lua config\n");
    f.file(".config/hypr/monitors.conf", "monitor=,preferred,auto,1\n");
    let r = run_with(
        &f,
        &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
        Some("y"),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    f
}

/// D49: adopt, then rollback — the path is empty and the user's own
/// directory is in the attic. doctor is where they find that out.
#[test]
fn adopt_then_rollback_leaves_the_path_empty_and_the_directory_in_the_attic() {
    let f = adopted("adopt_rollback");
    let r = run(&f, &["rollback", "--commit"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(read::lstat(&f.path(".config/hypr")).unwrap().is_none());

    let r = doctor(&f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

/// D51: the `[[path]]` adopt appended, edited out by hand.
#[test]
fn an_adopted_path_edited_out_of_the_manifest() {
    let f = adopted("adopt_edited");
    let path = f.path(".local/share/ricepilot/profiles/mine/profile.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let cut = text.find("\n# added by `ricepilot adopt`.").unwrap();
    std::fs::write(&path, &text[..cut + 1]).unwrap();

    let r = doctor(&f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}

// ---------------------------------------------------------------------------
// Several at once, and the machine
// ---------------------------------------------------------------------------

/// The shape a real bad day has: an installer ran, a switch was interrupted,
/// the rescue script is damaged and a profile was edited. The journal comes
/// first, and nothing is touched.
#[test]
fn several_problems_at_once() {
    let m = machine("several");
    rm(&m.foot);
    m.f.file(".config/foot/foot.ini", "font=monospace\n");
    put_back_a_journal(&m.f);
    std::fs::write(m.f.state().join("rescue.sh"), "if then fi\n").unwrap();
    std::fs::write(m.new_root.join("hypr/marker"), "edited\n").unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    let first = r.stdout.lines().nth(2).unwrap_or("");
    assert!(first.ends_with("journal/current.toml"), "{}", r.stdout);
    insta::assert_snapshot!(r.stdout);
}

fn in_process(m: &switching::Machine, sys: &Path) -> (ricepilot::doctor::Report, String) {
    let at = ricepilot::doctor::Where {
        paths: ricepilot::cli::paths::Paths::rooted_at(m.f.home.clone()),
        system: Some(sys.to_path_buf()),
    };
    let before = identities(&m.f);
    let report = ricepilot::doctor::diagnose(&ricepilot::ops::look::Live, &at);
    assert_eq!(before, identities(&m.f), "doctor changed the fixture");
    let text = normalise(&report.text(), &m.f);
    (report, text)
}

/// The machine-wide hazards, with a fixture standing in for `/`: caelestia's
/// theme daemons in `/proc`, snapper with only a root config, the user's
/// `hypr-session` writing into `~/.config/hypr`, a rice rooted where
/// caelestia-cli migrates from, an attic grown past a gibibyte (a sparse
/// file: nothing reads its content) and verify-config copies piling up.
/// None of it is a problem, so the status is 0 — but every line prints the
/// command, and none is run.
#[test]
fn the_machine_hazards_print_commands_and_run_none() {
    let m = machine("hazards");
    let sys = m.f.dir("sys");
    m.f.file("sys/proc/1/cmdline", "/sbin/init\0splash\0");
    m.f.file(
        "sys/proc/4242/cmdline",
        "/usr/bin/python3\0/usr/bin/caelestia\0shell\0-d\0",
    );
    m.f.file("sys/proc/4243/cmdline", "caelestia\0resizer\0--daemon\0");
    m.f.file("sys/proc/4244/cmdline", "caelestia\0shell\0");
    m.f.file("sys/proc/self/cmdline", "caelestia\0shell\0-d\0");
    m.f.file(
        "sys/etc/snapper/configs/root",
        "# subvolume to snapshot\nSUBVOLUME=\"/\"\nFSTYPE=\"btrfs\"\n",
    );
    m.f.file(
        ".local/bin/hypr-session",
        "#!/bin/sh\n# start a session\n  export SESSION_DIR=\"$HOME/.config/hypr/sessions\"\nmkdir \
         -p \"$SESSION_DIR\"\n",
    );
    m.f.file(".local/share/caelestia/fish/config.fish", "set -g x 1\n");
    m.f.profile(
        "caelestia",
        "name = \"caelestia\"\nroot = \"~/.local/share/caelestia\"\n\n[[path]]\ndest       = \
         \"~/.config/fish\"\nsrc        = \"fish\"\nkind       = \"dir-link\"\nactivation = \
         \"relogin\"\n",
    );
    for i in 0..21 {
        m.f.file(
            &format!(".local/state/ricepilot/verify/20260101T0000{i:02}Z/root/hypr/hyprland.conf"),
            "general {}\n",
        );
    }
    let big =
        m.f.dir(".local/state/ricepilot/attic/20260102T000000Z/home");
    std::fs::File::create(big.join("sparse.img"))
        .unwrap()
        .set_len(3 << 30)
        .unwrap();

    let (report, text) = in_process(&m, &sys);
    assert_eq!(report.exit_code(), ExitCode::Ok, "{text}");
    assert!(
        text.contains("    snapper -c home create-config /home\n"),
        "{text}"
    );
    insta::assert_snapshot!(text);
}

/// The same checks on a machine where they pass: they fold into `ok:`.
#[test]
fn the_machine_checks_when_they_pass() {
    let m = machine("machine_ok");
    let sys = m.f.dir("sys");
    m.f.file("sys/proc/1/cmdline", "/sbin/init\0");
    m.f.file(
        "sys/etc/snapper/configs/home",
        "SUBVOLUME=\"/home\"\nFSTYPE=\"btrfs\"\n",
    );
    let (report, text) = in_process(&m, &sys);
    assert_eq!(report.exit_code(), ExitCode::Ok, "{text}");
    insta::assert_snapshot!(text);
}

/// snapper's configs are often readable by root alone. One doctor cannot
/// read might be the one for /home, so it says "may not", and how to find
/// out.
#[test]
fn a_snapper_config_doctor_cannot_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let m = machine("snapper_unread");
    let sys = m.f.dir("sys");
    m.f.dir("sys/proc");
    let home =
        m.f.file("sys/etc/snapper/configs/home", "SUBVOLUME=\"/home\"\n");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&home).is_ok() {
        eprintln!(
            "SKIPPED a_snapper_config_doctor_cannot_read: this user can read a mode-000 file"
        );
        return;
    }
    let (report, text) = in_process(&m, &sys);
    // Put the mode back, so the next run of the harness can rebuild the tree.
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(report.exit_code(), ExitCode::Ok, "{text}");
    insta::assert_snapshot!(text);
}

// ---------------------------------------------------------------------------
// Read-only by construction
// ---------------------------------------------------------------------------

fn doctor_sources() -> Vec<(String, String)> {
    let mut files = vec!["src/doctor.rs".to_string()];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor");
    let mut more: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| format!("src/doctor/{}", e.unwrap().file_name().to_string_lossy()))
        .collect();
    more.sort();
    files.extend(more);
    files
        .into_iter()
        .map(|f| {
            let text = source(&f);
            (f, text)
        })
        .collect()
}

/// **The construction.** `doctor` reaches the machine only through the
/// `&dyn Look` it is given. Every crate item it names is one of these, and
/// each is pure — a type, a constant, a path computation, a parser of text
/// already read — or reads only through a `Look` it is passed
/// (`verify::build_via`, `requires::missing_via`,
/// `rescue::Binaries::locate_via`). `ops::look::Live` is named once, by
/// `run`, to hand to `diagnose`. Nothing on this list writes, locks,
/// journals, spawns anything but through `Look`, or builds the
/// verify-config sandbox.
const DOCTOR_MAY_NAME: &[&str] = &[
    "crate::Error",
    "crate::Result",
    "crate::cli::Output",
    "crate::cli::paths::Paths",
    "crate::error::ExitCode",
    "crate::generations::Generation",
    "crate::generations::current_path",
    "crate::generations::dir",
    "crate::generations::parse",
    "crate::generations::parse_current",
    "crate::generations::path",
    "crate::hyprverify::CONF_ENTRY",
    "crate::hyprverify::LUA_ENTRY",
    "crate::hyprverify::hypr_dest",
    "crate::journal::parse",
    "crate::ledger::Ledger",
    "crate::ledger::Ledger::default",
    "crate::ledger::parse",
    "crate::manifest::Activation",
    "crate::manifest::Activation::Relogin",
    "crate::manifest::Kind",
    "crate::manifest::Kind::DirLink",
    "crate::manifest::Manifest",
    "crate::manifest::expand_home",
    "crate::manifest::parse",
    "crate::ops::look::Kind",
    "crate::ops::look::Live",
    "crate::ops::look::Look",
    "crate::plan::Target",
    "crate::requires::missing_via",
    "crate::rescue::Binaries::locate_via",
    "crate::rescue::parked_rel",
    "crate::rescue::path",
    "crate::rescue::printable",
    "crate::rescue::rescue_attic",
    "crate::rescue::script",
    "crate::shellword::command_lines",
    "crate::shellword::quoted",
    "crate::shellword::written_out",
    "crate::verify::build_via",
    "crate::verify::compare",
    "crate::verify::manifest_path",
    "crate::verify::parse",
];

/// Words that must not appear anywhere in doctor's sources, prose included
/// (the same rule as the CI greps, D3): each names a way to change the
/// machine, or a read that goes around `Look`.
const DOCTOR_MUST_NOT_SPELL: &[&str] = &[
    "mutate",
    "ops::lock",
    "lock::",
    "write_atomic",
    "journal::write",
    "mark_done",
    "::save",
    "regenerate",
    "SandboxedConfig",
    "sandbox::",
    "hyprverify::check",
    "exec::",
    "Call::",
    "ops::read",
    "std::process",
    "std::env",
    "set_var",
    ".record(",
    ".observe(",
    "ricepilot::",
];

#[test]
fn doctor_names_nothing_that_could_change_the_machine() {
    let mut hits = Vec::new();
    for (file, text) in doctor_sources() {
        for p in paths_after(&text, "crate::") {
            if !DOCTOR_MAY_NAME.contains(&p.as_str()) {
                hits.push(format!("{file}: names `{p}`, which is not on the list"));
            }
        }
        for p in paths_after(&text, "super::") {
            if !["super::Finding", "super::Report"].contains(&p.as_str()) {
                hits.push(format!("{file}: names `{p}`"));
            }
        }
        for word in DOCTOR_MUST_NOT_SPELL {
            for (n, line) in text.lines().enumerate() {
                if line.contains(word) {
                    hits.push(format!(
                        "{file}:{}: spells `{word}`: {}",
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "doctor is read-only by construction (D57): it names only pure items and reaches the \
         machine through `Look`\n{}",
        hits.join("\n")
    );
}

/// And `Look`'s one implementation reads, and runs only `pacman -Q` and
/// `sh -n`.
#[test]
fn look_only_reads() {
    let text = source("src/ops/look.rs");
    let may: &[&str] = &[
        "super::exec::Allowed::PacmanQuery",
        "super::exec::Call::PacmanQuery",
        "super::exec::Call::ShSyntaxCheck",
        "super::exec::Ran",
        "super::exec::locate",
        "super::exec::run",
        "super::read::Kind",
        "super::read::Meta",
        "super::read::list_dir",
        "super::read::lstat",
        "super::read::lstat_or_absent",
        "super::read::read_into",
        "super::read::readlink",
        "super::read::resolves",
        "super::read::size_of",
        "super::read::slurp",
    ];
    let mut hits = Vec::new();
    for p in paths_after(&text, "super::") {
        if !may.contains(&p.as_str()) {
            hits.push(format!("names `{p}`"));
        }
    }
    for p in paths_after(&text, "crate::") {
        if p != "crate::Result" {
            hits.push(format!("names `{p}`"));
        }
    }
    for word in [
        "mutate",
        "lock::",
        "UwsmStop",
        "Hyprctl",
        "HyprlandVerifyConfig",
        "GitStatus",
        "Sandboxed",
        "std::fs",
        "rustix",
        "std::process",
    ] {
        if text.contains(word) {
            hits.push(format!("spells `{word}`"));
        }
    }
    assert!(hits.is_empty(), "src/ops/look.rs:\n{}", hits.join("\n"));
}

/// The guard above can fail: a planted write, a grouped import and a
/// forbidden word are each caught.
#[test]
fn the_doctor_guard_bites() {
    let planted = "use crate::ops::{read, x};\nfn f() { crate::ledger::save(p, l); }\n// mutate\n";
    let names = paths_after(planted, "crate::");
    assert!(names.contains(&"crate::ops::{".to_string()), "{names:?}");
    assert!(
        names.contains(&"crate::ledger::save".to_string()),
        "{names:?}"
    );
    assert!(names.iter().all(|n| !DOCTOR_MAY_NAME.contains(&n.as_str())));
    assert!(DOCTOR_MUST_NOT_SPELL.iter().any(|w| planted.contains(w)));
}

/// A removal `gc` began and did not finish (D62): what is left of the entry
/// sits in `state/gc/`, and doctor says so first-class, with the command
/// that lists it and finishes it once its name is typed again.
#[test]
fn a_gc_that_did_not_finish_is_a_problem() {
    let m = machine("gc_interrupted");
    let attic = m.f.state().join("attic");
    let name = ricepilot::ops::read::list_dir(&attic).unwrap().remove(0);
    let gc = m.f.dir(".local/state/ricepilot/gc");
    std::fs::rename(
        attic.join(&name),
        gc.join(format!("attic-{}", name.to_string_lossy())),
    )
    .unwrap();
    let r = doctor(&m.f);
    unhealthy(&r);
    insta::assert_snapshot!(r.stdout);
}
