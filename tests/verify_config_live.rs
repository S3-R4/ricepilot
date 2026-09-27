//! The real `Hyprland --verify-config`, on a fixture, through the sandbox.
//!
//! **Opt-in only**: skipped unless `RICEPILOT_LIVE_TESTS=1`, and skipped if
//! `Hyprland` is not installed. Hyprland is the compositor; this is the one
//! test that runs it, and it never runs by default (D55). Everything it
//! touches is under `target/fixtures/`; the real `~/.config/hypr` is never
//! named.
//!
//! The fixture's exec lines each write a marker file, and the assertion is
//! that none appears — the same proof the unit tests make against a fake
//! stand-in, made once more against the real parser.

mod common;

use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use common::Fixture;
use ricepilot::ops::exec::{self, Allowed};

#[test]
fn real_verify_config_runs_no_exec_line() {
    let test = "real_verify_config_runs_no_exec_line";
    if std::env::var_os("RICEPILOT_LIVE_TESTS").is_none_or(|v| v != "1") {
        eprintln!("SKIPPED {test}: runs the real Hyprland; set RICEPILOT_LIVE_TESTS=1 to run it");
        return;
    }
    if exec::locate(Allowed::HyprlandVerifyConfig)
        .unwrap()
        .is_none()
    {
        eprintln!("SKIPPED {test}: Hyprland is not installed");
        return;
    }

    let f = Fixture::new_in("m5", "verify_live");
    let markers = f.dir("markers");
    let root = f.dir("rice/new");
    let m = markers.display();
    f.file(
        "rice/new/hypr/hyprland.conf",
        &format!(
            "$hypr = ~/.config/hypr\nexec-once = : > {m}/exec-once\nexec = : > {m}/exec\n\
             execr = : > {m}/execr\nexecr-once=: > {m}/execr-once\nsource = $hypr/conf.d/*.conf\n\
             general {{\n    border_size = 2\n}}\n"
        ),
    );
    f.file(
        "rice/new/hypr/conf.d/a.conf",
        &format!("exec-once = : > {m}/sourced\n"),
    );
    f.profile(
        "new",
        &format!(
            "name = \"new\"\nroot = \"{}\"\n\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \
             \"hypr\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
            root.display()
        ),
    );

    let mut cmd = Command::new(cargo_bin("ricepilot"));
    cmd.args(["plan", "new"]).env_clear();
    for (k, v) in f.env() {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("parsed in a sandboxed"), "{stdout}");

    // Hyprland's second pass is where exec lines fork. Give a stray one time.
    std::thread::sleep(std::time::Duration::from_secs(1));
    let left = ricepilot::ops::read::list_dir(&markers).unwrap();
    assert!(left.is_empty(), "an exec line ran: {left:?}");
}
