//! Every refusal M4 added below the command layer, snapshotted as one block.
//!
//! The command-level ones live beside their commands
//! (`cli_capture`, `cli_adopt`, `cli_init`), because what matters about
//! those is the whole transcript a user sees. These are the ones a user
//! meets through a command but whose wording belongs to the copier and the
//! journal, and gathering them together is how you notice that two of them
//! have drifted into contradicting each other.

mod common;

use std::fmt::Write as _;
use std::path::Path;

use common::Fixture;

fn line(f: &Fixture, what: &str, e: ricepilot::Error) -> String {
    format!(
        "{what}\n  exit {}  {}\n\n",
        e.exit_code() as u8,
        uninode(&common::redact(&e.to_string(), f))
    )
}

/// Inode numbers are the filesystem's, not ricepilot's, and differ on every
/// run. The word before them is what this snapshot is reviewing.
fn uninode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("inode ") {
        out.push_str(&rest[..i + 6]);
        rest = &rest[i + 6..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        out.push_str("<n>");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn every_refusal_m4_added() {
    let f = Fixture::new_in("m4", "refusals");
    let mut s = String::new();

    // --- the copier ---
    let src = f.dir("rice/src");
    f.file("rice/src/a.conf", "a\n");
    let occupied = f.dir("rice/occupied");
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "copying onto something that is already there",
            ricepilot::ops::mutate::copy_tree(&src, &occupied).unwrap_err()
        )
    );
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "copying a tree into itself",
            ricepilot::ops::mutate::copy_tree(&src, &src.join("inner")).unwrap_err()
        )
    );
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "copying something that is not there",
            ricepilot::ops::mutate::copy_tree(&f.path("rice/nothing"), &f.path("rice/dst"))
                .unwrap_err()
        )
    );
    std::os::unix::net::UnixListener::bind(f.path("rice/src/app.sock")).unwrap();
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "copying a tree with a socket in it",
            ricepilot::ops::mutate::copy_tree(&src, &f.path("rice/dst2")).unwrap_err()
        )
    );

    // --- recovering an interrupted adopt ---
    let dest = f.dir(".config/hypr");
    let target = f.dir(".local/share/ricepilot/profiles/mine/hypr");
    let meta = ricepilot::ops::read::lstat(&dest).unwrap().unwrap();
    let record = ricepilot::journal::Adopt {
        dest: dest.clone(),
        dir_dev: meta.dev,
        // An inode this directory certainly does not have: the state a
        // recovery meets when something replaced the directory in the window.
        dir_ino: meta.ino ^ 0xffff,
        staged: ricepilot::plan::adopt_staged_name(&dest),
        new_target: target,
        attic_rel: Path::new("hypr").to_path_buf(),
    };
    let j = ricepilot::journal::Journal::for_adopt(
        "20260921T000000Z",
        "mine",
        f.state().join("attic/20260921T000000Z"),
        ricepilot::ops::mutate::ExchangeMode::Renameat2,
        record,
        &[],
    );
    let _ = write!(
        s,
        "{}",
        line(
            &f,
            "recovering an adopt whose directory was replaced underneath it",
            ricepilot::journal::plan_recovery(&j).unwrap_err()
        )
    );

    insta::assert_snapshot!(s);
}
