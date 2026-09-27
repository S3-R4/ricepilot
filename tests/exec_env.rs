//! A child of `ops::exec` sees none of ricepilot's environment and is found by
//! absolute path, never through `PATH` (D52).
//!
//! Its own test binary, with one test, because it sets variables on the test
//! process: in a binary with other tests running on other threads that would
//! race them.

#![allow(clippy::disallowed_methods)]

mod common;

use std::os::unix::fs::PermissionsExt as _;

use common::Fixture;
use ricepilot::ops::exec::{self, Allowed, Call};

#[test]
fn a_hostile_environment_reaches_no_child() {
    let f = Fixture::new_in("m5", "exec_env");
    let marker = f.path("impostor-ran");

    // A `PATH` whose first directory holds impostors for every allowlisted
    // name. If any of them runs, it leaves a marker.
    let bin = f.dir("impostors");
    for what in Allowed::ALL {
        let p = f.file(
            &format!("impostors/{}", what.program()),
            &format!(
                "#!/bin/sh\necho {} >> '{}'\n",
                what.program(),
                marker.display()
            ),
        );
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:/usr/bin", bin.display());

    // Variables git would obey if it saw them, pointing somewhere wrong.
    let elsewhere = f.dir("elsewhere");
    std::env::set_var("PATH", &path);
    std::env::set_var("GIT_DIR", &elsewhere);
    std::env::set_var("GIT_WORK_TREE", &elsewhere);
    std::env::set_var("GIT_CONFIG_PARAMETERS", "'core.fsmonitor'='/bin/false'");
    std::env::set_var("LD_PRELOAD", "/nonexistent/libimpostor.so");

    let ran = exec::run(Call::ShSyntaxCheck {
        script: "echo fine\n",
    })
    .unwrap();
    assert!(ran.success(), "{ran:?}");
    assert_eq!(ran.stderr, "", "LD_PRELOAD reached sh");

    if exec::locate(Allowed::PacmanQuery).unwrap().is_some() {
        let ran = exec::run(Call::PacmanQuery { package: "pacman" }).unwrap();
        assert!(ran.success(), "{ran:?}");
        assert!(ran.program.starts_with("/usr"), "{}", ran.program.display());
    } else {
        eprintln!(
            "SKIPPED the pacman half of a_hostile_environment_reaches_no_child: not installed"
        );
    }

    if exec::locate(Allowed::GitStatus).unwrap().is_some() {
        let repo = f.dir("repo");
        let st = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(st.success());
        f.file("repo/here", "x\n");
        let ran = exec::run(Call::GitStatus { repo: &repo }).unwrap();
        // The repository named, not the one GIT_DIR points at.
        assert!(ran.success(), "{ran:?}");
        assert_eq!(ran.stdout, "?? here\n");
    } else {
        eprintln!("SKIPPED the git half of a_hostile_environment_reaches_no_child: not installed");
    }

    assert!(
        std::fs::symlink_metadata(&marker).is_err(),
        "an impostor on PATH was run: {}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
}
