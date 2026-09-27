//! `ops::exec`: every allowlist entry, against something real where that is
//! possible without touching the live session, and excused by name where it
//! is not (`SAFETY.md` R7).
//!
//! A test whose binary is not installed skips **with a message** rather than
//! failing: CI has no `pacman` and no `hyprctl`, and a green run there must
//! not be read as those entries having been exercised. Nothing a subprocess
//! itself prints is snapshotted — `/bin/sh` is bash here and dash on CI.

// The harness builds its git fixture with the ordinary standard library and
// its own `git`; the guards only ever scan src/.
#![allow(clippy::disallowed_methods)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Fixture;
use ricepilot::ops::exec::{self, Allowed, Call, HyprctlQuery};
use ricepilot::Error;

/// Whether the tests that talk to the live session, or run the real
/// `Hyprland`, were asked for. They never run by default: `hyprctl` reaches
/// the running compositor, and `Hyprland` is the compositor.
pub fn live_tests(test: &str) -> bool {
    let on = std::env::var_os("RICEPILOT_LIVE_TESTS").is_some_and(|v| v == "1");
    if !on {
        eprintln!(
            "SKIPPED {test}: talks to the live session; set RICEPILOT_LIVE_TESTS=1 to run it"
        );
    }
    on
}

/// Where `what` is, or `None` after saying out loud that the test is skipped.
fn installed(what: Allowed, test: &str) -> Option<PathBuf> {
    let found = exec::locate(what).unwrap();
    if found.is_none() {
        eprintln!(
            "SKIPPED {test}: `{}` is not installed in {} on this machine",
            what.program(),
            exec::BIN_DIRS.join(", ")
        );
    }
    found
}

/// The complete list, with what covers each entry. The match is exhaustive,
/// so adding an entry to `Allowed` without deciding how it is tested does not
/// compile.
#[test]
fn every_allowlist_entry_is_exercised_or_excused() {
    for what in Allowed::ALL {
        let how = match what {
            Allowed::ShSyntaxCheck => "exercised: sh_n_* (real /bin/sh)",
            Allowed::PacmanQuery => "exercised: pacman_* (real pacman, skipped if absent)",
            Allowed::Hyprctl => {
                "exercised: hyprctl_version_* only under RICEPILOT_LIVE_TESTS=1 (it talks to the \
                 running compositor)"
            }
            Allowed::GitStatus => "exercised: git_status_* (fixture repo under target/fixtures)",
            Allowed::HyprlandVerifyConfig => {
                "exercised against a fake stand-in (src/ops/exec/sandbox.rs unit tests); the \
                 real binary only under RICEPILOT_LIVE_TESTS=1 (tests/verify_config_live.rs)"
            }
            Allowed::UwsmStop => {
                "NOT exercised: ends the session; argv and gate asserted, never run"
            }
        };
        let how = if what.reaches_the_session() {
            format!("{how}; refused by ops::exec inside the test sandbox (D56)")
        } else {
            how.to_string()
        };
        let at = exec::locate(what)
            .unwrap()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not installed".into());
        eprintln!("{:<22} {:<28} {how}", format!("{what:?}"), at);
    }
}

// ---- sh -n ----

#[test]
fn sh_n_accepts_a_script_that_parses() {
    let ran = exec::run(Call::ShSyntaxCheck {
        script: "echo 'fine'\n",
    })
    .unwrap();
    assert!(ran.success(), "{ran:?}");
    assert_eq!(ran.program, Path::new(exec::SH));
    assert!(!ran.truncated);
}

#[test]
fn sh_n_reports_a_script_that_does_not_parse() {
    let ran = exec::run(Call::ShSyntaxCheck {
        script: "if true; then\n",
    })
    .unwrap();
    // Not snapshotted: the wording is bash's here and dash's on CI.
    assert!(!ran.success());
    assert!(!ran.stderr.trim().is_empty());
}

/// `-n` parses and never executes: a script that would write a marker leaves
/// no marker.
#[test]
fn sh_n_never_runs_the_script() {
    let f = Fixture::new_in("m5", "exec_sh_never_runs");
    let marker = f.path("marker");
    let script = format!("echo ran > '{}'\n", marker.display());
    let ran = exec::run(Call::ShSyntaxCheck { script: &script }).unwrap();
    assert!(ran.success());
    assert!(std::fs::symlink_metadata(&marker).is_err());
}

// ---- pacman -Q ----

#[test]
fn pacman_query_finds_pacman_itself() {
    let Some(at) = installed(Allowed::PacmanQuery, "pacman_query_finds_pacman_itself") else {
        return;
    };
    let ran = exec::run(Call::PacmanQuery { package: "pacman" }).unwrap();
    assert_eq!(ran.program, at);
    assert!(ran.success(), "{ran:?}");
    assert!(ran.stdout.starts_with("pacman "), "{}", ran.stdout);
}

#[test]
fn pacman_query_does_not_find_a_package_that_does_not_exist() {
    if installed(
        Allowed::PacmanQuery,
        "pacman_query_does_not_find_a_package_that_does_not_exist",
    )
    .is_none()
    {
        return;
    }
    let ran = exec::run(Call::PacmanQuery {
        package: "ricepilot-no-such-package-f3a1",
    })
    .unwrap();
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(ran.stdout.is_empty());
}

/// Refused before anything is located or spawned, so this holds on a machine
/// with no pacman at all.
#[test]
fn pacman_query_refuses_anything_that_is_not_a_package_name() {
    for bad in [
        "",
        "-Syu",
        "--config=/x",
        ".hidden",
        "two words",
        "a/b",
        "hypr>=1",
    ] {
        match exec::run(Call::PacmanQuery { package: bad }) {
            Err(Error::Refused { rule, .. }) => assert_eq!(rule, "R3", "{bad:?}"),
            other => panic!("{bad:?} was not refused: {other:?}"),
        }
    }
}

// ---- hyprctl ----

/// Read-only: `hyprctl version` asks the running compositor what it is. Needs
/// an instance to ask, so it skips outside a Hyprland session.
#[test]
fn hyprctl_version_asks_the_running_compositor() {
    if !live_tests("hyprctl_version_asks_the_running_compositor") {
        return;
    }
    let Some(at) = installed(
        Allowed::Hyprctl,
        "hyprctl_version_asks_the_running_compositor",
    ) else {
        return;
    };
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        eprintln!(
            "SKIPPED hyprctl_version_asks_the_running_compositor: no Hyprland instance \
             (HYPRLAND_INSTANCE_SIGNATURE is not set)"
        );
        return;
    }
    let ran = exec::run(Call::Hyprctl(HyprctlQuery::Version)).unwrap();
    assert_eq!(ran.program, at);
    // `hyprctl` exits 0 even when it reached nothing, so the text is the
    // evidence, not the status.
    assert!(ran.stdout.starts_with("Hyprland "), "{ran:?}");
}

// ---- git status ----

/// The harness's own git, with an empty environment so it never reads the
/// user's git config either (R1).
fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("/usr/bin/git")
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .current_dir(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn index_identity(repo: &Path) -> (u64, u64, i64) {
    use std::os::unix::fs::MetadataExt as _;
    let m = std::fs::symlink_metadata(repo.join(".git/index")).unwrap();
    (m.dev(), m.ino(), m.mtime_nsec() + m.mtime() * 1_000_000_000)
}

#[test]
fn git_status_reports_the_fixture_repo_and_writes_nothing_into_it() {
    if installed(
        Allowed::GitStatus,
        "git_status_reports_the_fixture_repo_and_writes_nothing_into_it",
    )
    .is_none()
    {
        return;
    }
    let f = Fixture::new_in("m5", "exec_git_status");
    let repo = f.dir("repo");
    git(&repo, &["init", "-q"]);
    f.file("repo/tracked", "one\n");
    git(&repo, &["add", "tracked"]);
    // Modified after staging, so a status that refreshed the index would have
    // something to write.
    std::thread::sleep(std::time::Duration::from_millis(20));
    f.file("repo/tracked", "two\n");
    f.file("repo/untracked/deep", "x\n");
    let before = index_identity(&repo);

    let ran = exec::run(Call::GitStatus { repo: &repo }).unwrap();

    assert!(ran.success(), "{ran:?}");
    assert_eq!(ran.stdout, "AM tracked\n?? untracked/deep\n");
    assert_eq!(
        index_identity(&repo),
        before,
        "git status rewrote the index"
    );

    // And the control: a plain `git status` on the same repository does
    // rewrite it, which is why the flag is there at all.
    git(&repo, &["status", "--porcelain"]);
    assert_ne!(
        index_identity(&repo),
        before,
        "plain git status left the index alone, so this test no longer proves anything"
    );
}

/// The fixture tree lives inside ricepilot's own checkout. A directory that is
/// not a repository must be answered as one, not reported as the repository
/// above it.
#[test]
fn git_status_on_a_directory_that_is_not_a_repo_does_not_find_the_one_above() {
    if installed(
        Allowed::GitStatus,
        "git_status_on_a_directory_that_is_not_a_repo_does_not_find_the_one_above",
    )
    .is_none()
    {
        return;
    }
    let f = Fixture::new_in("m5", "exec_git_not_a_repo");
    let dir = f.dir("plain");
    let ran = exec::run(Call::GitStatus { repo: &dir }).unwrap();
    assert!(!ran.success(), "{ran:?}");
    assert!(ran.stdout.is_empty(), "{}", ran.stdout);
}

#[test]
fn git_status_refuses_a_path_that_is_relative_or_walks_up() {
    for bad in ["relative/repo", "/home/u/../../etc"] {
        match exec::run(Call::GitStatus {
            repo: Path::new(bad),
        }) {
            Err(Error::Refused { rule, .. }) => assert_eq!(rule, "R3", "{bad}"),
            other => panic!("{bad} was not refused: {other:?}"),
        }
    }
}

// ---- uwsm stop and Hyprland --verify-config: built, never run ----

/// What `--relogin` will run, asserted without running it.
#[test]
fn uwsm_stop_is_built_by_absolute_path_and_never_run() {
    assert_eq!(Allowed::UwsmStop.program(), "uwsm");
    assert_eq!(exec::UWSM_STOP_ARGV, ["stop"]);
    if let Some(at) = exec::locate(Allowed::UwsmStop).unwrap() {
        assert!(at.is_absolute());
        assert!(
            exec::BIN_DIRS.iter().any(|d| at.starts_with(d)),
            "{}",
            at.display()
        );
    } else {
        eprintln!("NOTE uwsm is not installed here; only the argv was asserted");
    }
}

/// Every line in `src/` outside doc comments that mentions `needle`.
fn source_lines_mentioning(needle: &str) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    let mut hits = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("///") || code.starts_with("//!") || code.starts_with("//") {
                continue;
            }
            if line.contains(needle) {
                let rel = file.strip_prefix(&src).unwrap().display().to_string();
                hits.push(format!("{rel}:{}: {}", n + 1, code));
            }
        }
    }
    hits
}

/// The gate on the session-ending call is that its token cannot be made. The
/// compile-fail doctest on `Relogin` proves no code outside `ops::exec` can
/// build one; this proves `ops::exec` itself does not, and that nothing names
/// the call. `--relogin` is the task that changes both, deliberately.
#[test]
fn nothing_in_the_crate_can_reach_uwsm_stop_yet() {
    let built: Vec<String> = source_lines_mentioning("_sealed")
        .into_iter()
        .filter(|l| !l.ends_with("_sealed: (),"))
        .collect();
    assert!(
        built.is_empty(),
        "a Relogin token is constructed: {built:#?}"
    );

    let named: Vec<String> = source_lines_mentioning("UwsmStop(")
        .into_iter()
        .filter(|l| !l.starts_with("ops/exec.rs:"))
        .collect();
    assert!(named.is_empty(), "Call::UwsmStop is named: {named:#?}");
}

/// Likewise: the only place a `SandboxedConfig` is made is its constructor
/// in the sandbox module, and the call never gets the compositor's instance
/// signature (D55).
#[test]
fn verify_config_is_only_ever_pointed_at_a_sandbox() {
    let built: Vec<String> = source_lines_mentioning("SandboxedConfig {")
        .into_iter()
        .filter(|l| !l.contains("pub struct SandboxedConfig {"))
        .filter(|l| !l.contains("impl SandboxedConfig {"))
        .collect();
    assert_eq!(
        built.len(),
        1,
        "a SandboxedConfig is constructed somewhere other than its one constructor: {built:#?}"
    );
    assert!(built[0].starts_with("ops/exec/sandbox.rs:"), "{built:#?}");
    let called: Vec<String> = source_lines_mentioning("Call::HyprlandVerifyConfig(")
        .into_iter()
        .filter(|l| !l.starts_with("ops/exec.rs:"))
        .collect();
    assert!(
        called.iter().all(|l| l.starts_with("hyprverify.rs:")),
        "verify-config is run from somewhere other than hyprverify: {called:#?}"
    );
    assert!(Allowed::HyprlandVerifyConfig.passes().is_empty());
    assert!(!Allowed::HyprlandVerifyConfig
        .passes()
        .contains(&"HYPRLAND_INSTANCE_SIGNATURE"));
}

// ---- the refusals ricepilot itself words ----

#[test]
fn exec_refusals() {
    let mut s = String::new();
    let mut line = |what: &str, e: Error| {
        s.push_str(&format!("{what}\n  exit {}  {e}\n\n", e.exit_code() as u8));
    };
    line(
        "a requires entry that could be read as an option",
        exec::run(Call::PacmanQuery { package: "-Syu" }).unwrap_err(),
    );
    line(
        "git status on a relative path",
        exec::run(Call::GitStatus {
            repo: Path::new("relative/repo"),
        })
        .unwrap_err(),
    );
    for what in Allowed::ALL {
        if what != Allowed::ShSyntaxCheck {
            line(
                &format!("{what:?} not installed"),
                exec::not_installed(what),
            );
        }
    }
    // Inside the test sandbox (D56), with and without the live opt-in.
    for live in [false, true] {
        for what in Allowed::ALL {
            let refused = exec::sandbox_refuses(what, |name| match name {
                exec::SANDBOX_VAR => Some("/repo/target/fixtures".into()),
                exec::LIVE_TESTS_VAR if live => Some("1".into()),
                _ => None,
            });
            if let Some(e) = refused {
                let opt_in = if live { ", RICEPILOT_LIVE_TESTS=1" } else { "" };
                line(&format!("{what:?} in the sandbox{opt_in}"), e);
            }
        }
    }
    insta::assert_snapshot!(s);
}
