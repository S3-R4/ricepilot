//! Every refusal message M3 added, snapshotted as one block.
//!
//! These are the sentences a user meets on a machine that is already going
//! wrong, so the wording is part of the deliverable rather than an
//! implementation detail. Gathering them in one snapshot means a change to any
//! of them shows up as a diff of all of them, side by side, which is how you
//! notice that two messages have drifted into contradicting each other.

mod common;

use std::fmt::Write as _;

use common::Fixture;

/// Render one refusal the way the binary does, with the fixture path redacted.
///
/// A message that quotes `sh`'s own diagnostic has that part replaced: the
/// wording differs between dash and bash, and what is being reviewed here is
/// ricepilot's sentence, not the shell's.
fn line(f: &Fixture, what: &str, e: ricepilot::Error) -> String {
    let text = common::redact(&e.to_string(), f);
    let text = match text.split_once("was not written: ") {
        Some((head, tail)) => {
            let rest = tail.rfind(" (").map(|i| &tail[i..]).unwrap_or("");
            format!("{head}was not written: <what sh said>{rest}")
        }
        None => text,
    };
    format!("{what}\n  exit {}  {text}\n\n", e.exit_code() as u8)
}

#[test]
fn every_refusal_m3_added() {
    let f = Fixture::new_in("m3", "refusals");
    let state = f.state();
    let mut s = String::new();

    // --- ledger ---
    let dir = f.dir(".config/hypr");
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "recording a real directory as an owned link",
            ricepilot::ledger::Entry::of(&dir, "one").unwrap_err()
        )
    );
    let _ = write!(
        &mut s,
        "{}",
        line(
            &f,
            "recording a path that is not there",
            ricepilot::ledger::Entry::of(&f.path(".config/nothing"), "one").unwrap_err()
        )
    );
    f.file(".local/state/ricepilot/ledger.toml", "not toml {{{\n");
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "a ledger that does not parse",
            ricepilot::ledger::load(&state.join("ledger.toml")).unwrap_err()
        )
    );

    // --- generations ---
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "asking for a generation that was never written",
            ricepilot::generations::load(&state, 4).unwrap_err()
        )
    );
    f.file(
        ".local/state/ricepilot/generations/current",
        "not a number\n",
    );
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "a current-generation pointer that is not a number",
            ricepilot::generations::current(&state).unwrap_err()
        )
    );
    f.clear(".local/state/ricepilot/generations/current");
    let decoy = f.file("rice/decoy", "0009\n");
    f.link(".local/state/ricepilot/generations/current", &decoy);
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "a current-generation pointer that is a symlink",
            ricepilot::generations::current(&state).unwrap_err()
        )
    );

    // --- verify ---
    f.file(
        ".local/state/ricepilot/manifests/one.toml",
        "still not toml {{{\n",
    );
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "a recorded tree manifest that does not parse",
            ricepilot::verify::load(&ricepilot::verify::manifest_path(&state, "one")).unwrap_err()
        )
    );

    // --- rescue ---
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "a generated script that does not parse",
            ricepilot::ops::exec::sh_syntax_check("if true; then\n").unwrap_err()
        )
    );

    insta::assert_snapshot!(s);
}
