//! `state/rescue.sh` — rung 3 of the recovery ladder.
//!
//! The M3 gate says: executed by a real `/bin/sh` in a fixture, it restores the
//! previous generation. So it is executed by a real `/bin/sh`. Asserting on the
//! script's *text* would prove that ricepilot writes what ricepilot expects,
//! which is not the question anyone has at a TTY.

mod common;

use std::path::{Path, PathBuf};

use common::Fixture;
use ricepilot::generations::Generation;
use ricepilot::ops::read;
use ricepilot::rescue;

const WHEN: &str = "20260921T101112Z";

struct World {
    f: Fixture,
    old: PathBuf,
    new: PathBuf,
    hypr: PathBuf,
    btop: PathBuf,
    /// The topology before the switch: `hypr` into `old`, nothing at `btop`.
    gen0: Generation,
}

/// Build the machine as it was, record generation 0000 from it, then switch it
/// by hand to where a successful `switch` would have left it.
fn switched(case: &str) -> World {
    let f = Fixture::new_in("m3", case);
    let old = f.dir("rice/old");
    let new = f.dir("rice/new");
    for leaf in ["hypr", "btop"] {
        f.dir(&format!("rice/old/{leaf}"));
        f.dir(&format!("rice/new/{leaf}"));
        f.file(&format!("rice/old/{leaf}/marker"), "old\n");
        f.file(&format!("rice/new/{leaf}/marker"), "new\n");
    }
    f.dir(".config");
    let hypr = f.link(".config/hypr", &old.join("hypr"));
    let btop = f.path(".config/btop");
    f.clear(".config/btop");

    let gen0 = Generation::observe(0, "old", WHEN, &[hypr.clone(), btop.clone()]).unwrap();

    // Where the switch left the machine.
    f.link(".config/hypr", &new.join("hypr"));
    f.link(".config/btop", &new.join("btop"));

    World {
        f,
        old,
        new,
        hypr,
        btop,
        gen0,
    }
}

fn run_script(path: &Path) -> String {
    let out = common::sh(path).output().unwrap();
    assert!(
        out.status.success(),
        "the rescue script exited {:?}:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The gate. A real `/bin/sh`, a real tree, and the previous generation's
/// topology afterwards.
#[test]
fn a_real_sh_running_the_script_restores_the_previous_generation() {
    let w = switched("rescue_restores");
    let script = rescue::regenerate(&w.f.state(), &w.gen0).unwrap();

    let output = run_script(&script);

    assert_eq!(
        read::readlink(&w.hypr).unwrap(),
        w.old.join("hypr"),
        "the switched link is back on the previous profile"
    );
    assert_eq!(
        read::lstat(&w.btop).unwrap(),
        None,
        "and the destination the previous generation did not have is empty again"
    );

    // Empty, but nothing was removed: the link is in the rescue attic.
    let parked = rescue::rescue_attic(&w.f.state(), 0).join(w.btop.strip_prefix("/").unwrap());
    assert_eq!(read::readlink(&parked).unwrap(), w.new.join("btop"));

    assert!(
        output.contains("ok      "),
        "it reports what it did: {output}"
    );
}

/// Running it twice must not make things worse. The second pass finds `hypr`
/// already restored and re-links it to the same place; `btop` is already gone
/// and its line fails loudly rather than silently doing something else.
#[test]
fn running_the_script_a_second_time_leaves_the_same_topology() {
    let w = switched("rescue_twice");
    let script = rescue::regenerate(&w.f.state(), &w.gen0).unwrap();

    run_script(&script);
    let output = run_script(&script);

    assert_eq!(read::readlink(&w.hypr).unwrap(), w.old.join("hypr"));
    assert_eq!(read::lstat(&w.btop).unwrap(), None);
    assert!(
        output.contains("FAILED"),
        "a step that cannot be repeated says so rather than pretending: {output}"
    );
}

/// One destination it cannot restore must not cost the others. A real
/// directory in the way is the case that actually happens — an installer put
/// it there.
#[test]
fn a_destination_it_cannot_restore_does_not_stop_the_rest() {
    let w = switched("rescue_partial");
    let script = rescue::regenerate(&w.f.state(), &w.gen0).unwrap();

    // `mv -T` will not replace a non-empty directory.
    w.f.clear(".config/hypr");
    w.f.dir(".config/hypr");
    w.f.file(".config/hypr/installer-wrote-this", "x\n");

    let output = run_script(&script);

    assert!(output.contains("FAILED"), "{output}");
    assert_eq!(
        read::lstat(&w.btop).unwrap(),
        None,
        "the other destination was still restored"
    );
    assert!(
        read::lstat(&w.f.path(".config/hypr/installer-wrote-this"))
            .unwrap()
            .is_some(),
        "and what the installer wrote is untouched"
    );
}

/// The script's whole premise is that it needs nothing. Asserting the absence
/// of the things it must not contain is the one text assertion worth making.
#[test]
fn the_script_uses_only_four_constructs_and_three_commands() {
    let w = switched("rescue_shape");
    let script = rescue::regenerate(&w.f.state(), &w.gen0).unwrap();
    let text = read::slurp(&script).unwrap();
    assert!(text.starts_with("#!/bin/sh\n"));

    // Comments are prose and may say "ricepilot" or "PATH". The constraint is
    // on what the shell will actually execute.
    let code: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    for forbidden in ["for ", "while ", "until ", "$", "`", "rm "] {
        assert!(
            !code.contains(forbidden),
            "the rescue script must not execute {forbidden:?}:\n{code}"
        );
    }

    // Stronger than a blocklist: every construct it uses is one of four, and
    // every command it runs is one of three absolute paths. Grepping the text
    // for "ricepilot" would prove nothing — a fixture path contains the word.
    for line in code.lines() {
        let first = line.split_whitespace().next().unwrap();
        assert!(
            matches!(first, "if" | "else" | "fi" | "echo"),
            "the script uses a construct other than its four: {line}"
        );
        for command in line.split("&&") {
            let head = command
                .trim()
                .trim_start_matches("if ")
                .split_whitespace()
                .next()
                .unwrap();
            assert!(
                matches!(
                    head,
                    "/usr/bin/ln" | "/usr/bin/mv" | "/usr/bin/mkdir" | "else" | "fi" | "echo"
                ),
                "the script runs {head:?}, which is not one of its three commands: {line}"
            );
        }
    }
    // Every command it names is an absolute path that exists.
    for line in text.lines().filter(|l| l.starts_with("if ")) {
        let bin = line.split_whitespace().nth(1).unwrap();
        assert!(bin.starts_with('/'), "not an absolute path: {line}");
        assert!(
            read::lstat(Path::new(bin)).unwrap().is_some(),
            "names a binary that is not there: {bin}"
        );
    }
}

/// It is a real file, for the same reason the `current` pointer is: a switch
/// that goes wrong is a switch that did something unintended to a symlink.
#[test]
fn the_script_is_written_as_a_real_file() {
    let w = switched("rescue_real_file");
    let script = rescue::regenerate(&w.f.state(), &w.gen0).unwrap();
    assert_eq!(
        read::lstat(&script).unwrap().unwrap().kind,
        read::Kind::File
    );
    assert_eq!(script, rescue::path(&w.f.state()));
}

/// A path containing a single quote is the classic way to turn a generated
/// script into arbitrary commands. It is quoted, and `sh` still restores it.
#[test]
fn a_path_with_a_quote_in_it_is_quoted_not_executed() {
    let f = Fixture::new_in("m3", "rescue_quoting");
    let old = f.dir("rice/o'ld");
    f.dir("rice/o'ld/hypr");
    f.dir(".config");
    let dest = f.link(".config/it's", &old.join("hypr"));
    let g = Generation::observe(0, "old", WHEN, std::slice::from_ref(&dest)).unwrap();

    let elsewhere = f.dir("rice/other");
    f.link(".config/it's", &elsewhere);

    let script = rescue::regenerate(&f.state(), &g).unwrap();
    run_script(&script);

    assert_eq!(read::readlink(&dest).unwrap(), old.join("hypr"));
}

/// `sh -n` runs before the file is written, so a script that does not parse is
/// never on the disk pretending to be a way out.
#[test]
fn a_script_that_does_not_parse_is_refused_and_never_written() {
    let err = ricepilot::ops::exec::sh_syntax_check("if true; then\n").unwrap_err();
    assert_eq!(err.exit_code(), ricepilot::error::ExitCode::Refused);
    assert!(err.to_string().contains("did not parse"));

    ricepilot::ops::exec::sh_syntax_check("echo 'fine'\n").unwrap();
}

/// A generation that managed nothing still produces a script that parses and
/// runs, rather than an empty file or a refusal.
#[test]
fn a_generation_with_no_destinations_still_produces_a_runnable_script() {
    let f = Fixture::new_in("m3", "rescue_empty");
    let g = Generation::observe(0, "old", WHEN, &[]).unwrap();
    let script = rescue::regenerate(&f.state(), &g).unwrap();
    assert!(run_script(&script).contains("managed no destinations"));
}

/// A path carrying `\n<command> #` is the way to inject into a generated
/// script's `#` comments: the newline ends the comment and the next line is
/// run. Manifests refuse one (D68), but the script also names paths read
/// back from the live filesystem — here a pre-existing link and its target,
/// recorded by generation 0000 — so the script escapes them itself.
///
/// Executed by a real `/bin/sh`: no line it prints is the injected command's
/// output, no marker file appears, and the link with the newline in its name
/// is still restored, since the command acting on it is given the real path.
#[test]
fn a_newline_in_a_path_from_the_live_filesystem_cannot_inject_a_command() {
    let f = Fixture::new_in("m3", "rescue_newline");
    let payload = "\necho INJECTED-FROM-RESCUE\n: > INJECTED-MARKER #";
    let old = f.dir(&format!("rice/old{payload}"));
    f.dir(&format!("rice/old{payload}/hypr"));
    f.dir(".config");
    let dest = f.link(&format!(".config/x{payload}"), &old.join("hypr"));
    // A destination generation 0000 had nothing at: it is displaced into the
    // rescue attic, whose comment names the attic path too.
    let gone = f.path(&format!(".config/gone{payload}"));
    let g = Generation::observe(
        0,
        format!("old{payload}"),
        WHEN,
        &[dest.clone(), gone.clone()],
    )
    .unwrap();

    let elsewhere = f.dir("rice/other");
    f.link(&format!(".config/x{payload}"), &elsewhere);
    f.link(&format!(".config/gone{payload}"), &elsewhere);

    let script = rescue::regenerate(&f.state(), &g).unwrap();
    let text = read::slurp(&script).unwrap();

    // Parsed as a shell would: every comment line is one line, with the
    // control characters written out rather than emitted.
    for line in text.lines().filter(|l| l.starts_with('#')) {
        assert!(!line.chars().any(char::is_control), "{line:?}");
    }
    assert!(
        text.contains(r"# profile: old\necho INJECTED-FROM-RESCUE\n: > INJECTED-MARKER #"),
        "{text}"
    );
    // Tokenised as `sh` does it: outside single quotes, `#` at the start of
    // a word runs to the end of the line and `\` escapes one character. What
    // is left — the code `sh` runs, quotes' contents aside — never contains
    // the payload.
    let code = unquoted_code(&text);
    assert!(!code.contains("INJECTED"), "{code}");

    let mut cmd = common::sh(&script);
    let cwd = f.dir("cwd");
    cmd.current_dir(&cwd);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.lines().any(|l| l == "INJECTED-FROM-RESCUE"),
        "the payload ran:\n{stdout}"
    );
    assert!(read::lstat_or_absent(&cwd.join("INJECTED-MARKER"))
        .unwrap()
        .is_none());
    assert_eq!(read::readlink(&dest).unwrap(), old.join("hypr"));
    assert!(read::lstat_or_absent(&gone).unwrap().is_none());

    insta::assert_snapshot!(common::redact(&text, &f));
}

/// The parts of a script `sh` would read as code rather than as a comment or
/// the inside of single quotes. Enough of the POSIX tokeniser for what the
/// rescue script uses: single quotes, `\` outside them, `#` comments.
fn unquoted_code(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    let mut word_start = true;
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                }
                out.push_str("''");
                word_start = false;
            }
            '\\' => {
                chars.next();
                word_start = false;
            }
            '#' if word_start => {
                for q in chars.by_ref() {
                    if q == '\n' {
                        out.push('\n');
                        break;
                    }
                }
                word_start = true;
            }
            _ => {
                out.push(c);
                word_start = c.is_whitespace() || ";&|()".contains(c);
            }
        }
    }
    out
}
