//! Per-path confirmation for `init` and `adopt` (`SAFETY.md` R6).
//!
//! These two commands change what gets touched on a real machine, so a human
//! says yes to each path or nothing happens to it. The interesting question
//! is not how to ask — it is how a test can drive the asking without the
//! asking being *skipped*.
//!
//! A `--yes` flag would be the obvious answer and is the wrong one. A flag a
//! test can pass is a flag a user can pass, R6 exists precisely so that a
//! human sees what is about to be touched, and a confirmation bypassed in
//! every test is one whose first real execution happens on the user's
//! machine.
//!
//! So there is no bypass. The confirmation always runs; only the *widget*
//! differs. When stdin is a terminal the question is an [`inquire`] prompt.
//! When it is not — a pipe, which is what a test gives it — the same question
//! text is printed and a line is read back. Both paths share
//! [`question_text`] and both parse the same answers, so what a test
//! exercises is the real decision and the real wording; what it does not
//! exercise is the terminal widget's key handling, which belongs to
//! `inquire` (D47).
//!
//! The default is **no**. End of input, an empty line, or anything that is
//! not a yes means the path is not touched.

use std::io::{BufRead as _, IsTerminal as _, Write as _};

/// The question, rendered identically whichever way it is asked.
pub fn question_text(what: &str) -> String {
    format!("{what} [y/N]")
}

/// Ask, and return whether the answer was yes.
///
/// Errors from the prompt itself (a closed terminal, an interrupted read)
/// are **not** propagated as failures: they mean no answer was given, and no
/// answer means no.
pub fn ask(what: &str) -> bool {
    if std::io::stdin().is_terminal() {
        ask_interactively(what)
    } else {
        ask_on_a_pipe(what)
    }
}

/// The terminal path. Not covered by the test suite — driving a TUI widget
/// would be testing `inquire` rather than ricepilot — which is why it is as
/// thin as it can be: one call, defaulting to no, and the same question text
/// the tested path uses (R7: an untested path is named as one).
fn ask_interactively(what: &str) -> bool {
    inquire::Confirm::new(what)
        .with_default(false)
        .prompt()
        .unwrap_or(false)
}

/// The non-terminal path: print the question, read one line.
fn ask_on_a_pipe(what: &str) -> bool {
    let mut out = std::io::stdout();
    let _ = write!(out, "{} ", question_text(what));
    let _ = out.flush();

    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    let answer = match read {
        Ok(0) | Err(_) => String::new(),
        Ok(_) => line.trim().to_ascii_lowercase(),
    };
    let yes = answer == "y" || answer == "yes";
    // Echo what was taken as the answer. The person reading a transcript of a
    // command that moved their configuration should be able to see what it
    // was told, not just what it did.
    let _ = writeln!(out, "{}", if yes { "yes" } else { "no" });
    yes
}
