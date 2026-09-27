//! Per-path confirmation for `init` and `adopt`, the logout `--relogin`
//! offers, and the name `gc` has typed back before it removes anything
//! (`SAFETY.md` R6).
//!
//! These change what gets touched on a real machine, so a human says yes to
//! each path — or to ending the session — or nothing happens. The interesting question
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

/// A yes, as a value: proof that the question was asked and answered yes.
///
/// Made in one place, [`affirmed`], and nowhere else — the field is private
/// — so a function that demands one cannot be reached without the question
/// having been put to a human. `--relogin` is the one that demands it: the
/// `uwsm stop` token's constructor takes a `Yes` (D58). `init` and `adopt`
/// branch on [`ask`]'s `bool`, which is the same question and the same
/// answer, without the type.
#[derive(Debug)]
pub struct Yes {
    _said: (),
}

/// [`ask`], with the yes as a [`Yes`].
pub fn affirmed(what: &str) -> Option<Yes> {
    if ask(what) {
        Some(Yes { _said: () })
    } else {
        None
    }
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

/// The operator typed a name back, and it was exactly the name asked for.
///
/// Made in one place, [`typed_back`], and nowhere else — the field is
/// private. `gc` removes nothing without one naming the entry (D61): a `y`
/// can be typed without reading the line above it, and a name like
/// `20260927T101500Z` cannot be typed without looking at which entry it is.
#[derive(Debug)]
pub struct Named {
    name: String,
}

impl Named {
    /// The name that was typed, which is the name that was asked for.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The prompt [`typed_back`] shows, identical whichever way it is asked.
pub fn name_prompt(name: &str) -> String {
    format!("type {name} to remove it, or anything else to keep it:")
}

/// Print `what`, ask for `name` to be typed back, and return the proof if it
/// was — exactly, apart from surrounding whitespace, and case-sensitively.
///
/// There is no default that removes: end of input, an empty line, a read
/// error and any other text all return `None`, and so does a closed
/// terminal. The same two widgets as [`ask`], for the same reason (D47).
pub fn typed_back(what: &str, name: &str) -> Option<Named> {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{what}");
    let _ = out.flush();
    let typed = if std::io::stdin().is_terminal() {
        type_interactively(name)
    } else {
        type_on_a_pipe(name)
    };
    typed.filter(|t| t.trim() == name).map(|_| Named {
        name: name.to_string(),
    })
}

/// The terminal path for [`typed_back`]. Untested for the reason
/// [`ask_interactively`] is, and as thin: one call.
fn type_interactively(name: &str) -> Option<String> {
    inquire::Text::new(&name_prompt(name)).prompt().ok()
}

/// The non-terminal path for [`typed_back`]: print the prompt, read one line,
/// echo what was read so a transcript shows what the command was told.
fn type_on_a_pipe(name: &str) -> Option<String> {
    let mut out = std::io::stdout();
    let _ = write!(out, "{} ", name_prompt(name));
    let _ = out.flush();

    let mut line = String::new();
    let typed = match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    };
    let _ = writeln!(
        out,
        "{}",
        match typed.as_deref() {
            None => "(no answer)",
            Some("") => "(nothing typed)",
            Some(t) => t,
        }
    );
    typed
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
