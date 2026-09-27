//! A path with a control character in it is never printed inside a command
//! meant to be pasted (D72). A newline in a single-quoted word is one
//! argument to `sh`, but on a terminal it is two lines, and a line pasted or
//! retyped from the screen is two commands; `$'…'` is not POSIX. So the
//! command is left out, and what replaces it says why and shows the path
//! written out.
//!
//! The state directory is the path here: it comes from the environment, not
//! a manifest (which refuses control characters, D68), so it is the kind of
//! path that can really carry one.

// Builds fixture trees with the standard library; the guards it protects
// only ever scan src/.
#![allow(clippy::disallowed_methods)]

mod common;

use std::path::{Path, PathBuf};

use common::{redact, Fixture};

/// A state directory whose name holds a newline followed by what would be a
/// command of its own if the line were pasted.
fn state_with_a_newline(f: &Fixture) -> PathBuf {
    let state = f.path(".local/state/rice\necho INJECTED #");
    std::fs::create_dir_all(&state).unwrap();
    state
}

fn run(f: &Fixture, state: &Path, args: &[&str]) -> (i32, String) {
    let out = common::ricepilot(f)
        .env("RICEPILOT_STATE_DIR", state)
        .args(args)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap(), redact(&text, f))
}

/// No line of `text` is a command naming the injected payload: every line
/// that mentions it is a `#` comment or the parenthesised notice.
fn no_line_carries_the_payload(text: &str) {
    for line in text.lines() {
        let t = line.trim_start();
        assert!(
            !t.starts_with("echo INJECTED"),
            "a line starts with the payload:\n{text}"
        );
    }
}

#[test]
fn doctor_leaves_out_a_command_naming_such_a_path() {
    let f = Fixture::new_in("m5-ctl", "doctor");
    let state = state_with_a_newline(&f);
    // More verify-config copies than doctor lets pass without a hazard, so
    // it prints commands naming the state directory.
    for n in 0..21 {
        std::fs::create_dir_all(state.join("verify").join(format!("20260927T1200{n:02}Z")))
            .unwrap();
    }
    let (code, text) = run(&f, &state, &["doctor"]);
    no_line_carries_the_payload(&text);
    assert!(
        text.contains("# left out: a path in this command"),
        "{text}"
    );
    insta::assert_snapshot!(format!("exit {code}\n{text}"));
}

#[test]
fn rescue_prints_no_sh_line_for_such_a_path() {
    let f = Fixture::new_in("m5-ctl", "rescue");
    let state = state_with_a_newline(&f);
    let (code, text) = run(&f, &state, &["rescue"]);
    no_line_carries_the_payload(&text);
    let refused = format!("exit {code}\n{text}");
    // Only whether a script is there is looked at, not what is in it.
    std::fs::write(state.join("rescue.sh"), "#!/bin/sh\n").unwrap();
    let (code, text) = run(&f, &state, &["rescue"]);
    no_line_carries_the_payload(&text);
    insta::assert_snapshot!(format!("{refused}\n---\nexit {code}\n{text}"));
}

/// The renderers every switch, adopt and `--relogin` print their `sh` line
/// through, directly: a plain path is unchanged, a quoted one quoted, and one
/// with a control character is withheld.
#[test]
fn the_sh_line_every_notice_prints() {
    use ricepilot::cli::render::sh_line;
    let s = [
        sh_line(Path::new("/home/u/.local/state/ricepilot/rescue.sh")),
        sh_line(Path::new("/home/u/my state/rescue.sh")),
        sh_line(Path::new("/home/u/a\necho INJECTED #/rescue.sh")),
        sh_line(Path::new("/home/u/a\u{1b}[2J/rescue.sh")),
    ]
    .join("\n");
    no_line_carries_the_payload(&s);
    insta::assert_snapshot!(s);
}

/// A state directory whose name is not valid UTF-8. Printed, it comes out
/// with `U+FFFD` where its byte was — a command naming it names another
/// path (D76).
fn state_not_utf8(f: &Fixture) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let state = f
        .home
        .join(std::ffi::OsStr::from_bytes(b".local/state/caf\xe9"));
    std::fs::create_dir_all(&state).unwrap();
    state
}

/// No line of `text` is a command naming the lossy state directory: every
/// line mentioning it is a `#` comment or the parenthesised notice.
fn no_command_names_a_lossy_path(text: &str) {
    for line in text.lines().filter(|l| l.contains('\u{FFFD}')) {
        let t = line.trim_start();
        assert!(
            !["sh ", "ls ", "mv ", "ln ", "less ", "$EDITOR "]
                .iter()
                .any(|c| t.starts_with(c)),
            "a line names the lossy path as if it could be pasted: {line}\n{text}"
        );
    }
}

#[test]
fn rescue_prints_no_sh_line_for_a_path_that_is_not_utf8() {
    let f = Fixture::new_in("m5-ctl", "rescue_not_utf8");
    let state = state_not_utf8(&f);
    std::fs::write(state.join("rescue.sh"), "#!/bin/sh\n").unwrap();
    let (code, text) = run(&f, &state, &["rescue"]);
    no_command_names_a_lossy_path(&text);
    assert!(
        text.contains(r"caf\xe9/rescue.sh: its name is not valid UTF-8"),
        "{text}"
    );
    insta::assert_snapshot!(format!("exit {code}\n{text}"));
}

#[test]
fn doctor_leaves_out_a_command_naming_a_path_that_is_not_utf8() {
    let f = Fixture::new_in("m5-ctl", "doctor_not_utf8");
    let state = state_not_utf8(&f);
    for n in 0..21 {
        std::fs::create_dir_all(state.join("verify").join(format!("20260927T1200{n:02}Z")))
            .unwrap();
    }
    let (_, text) = run(&f, &state, &["doctor"]);
    no_command_names_a_lossy_path(&text);
    assert!(
        text.contains("# left out: a path in this command is not valid UTF-8"),
        "{text}"
    );
}

/// The `sh` line renderer, directly, for a path that is not valid UTF-8.
#[test]
fn the_sh_line_for_a_path_that_is_not_utf8() {
    use ricepilot::cli::render::sh_line;
    use std::os::unix::ffi::OsStrExt;
    let p = Path::new(std::ffi::OsStr::from_bytes(
        b"/home/u/.local/state/caf\xe9/rescue.sh",
    ));
    let s = sh_line(p);
    assert!(!s.starts_with("sh "), "{s}");
    assert!(!s.contains('\u{FFFD}'), "{s}");
    insta::assert_snapshot!(s);
}
