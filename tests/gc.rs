//! `ricepilot gc` (D61, D62): the one command that removes anything.
//!
//! Every removal here goes through the binary, with the name typed on its
//! stdin. There is no other way to make one: `gc::collect` takes a
//! `confirm::Named`, which only `confirm::typed_back` makes, and only from
//! what was read back (D47) — so these tests drive the real question, the
//! real comparison and the real refusal.
//!
//! The attics are made the way a user makes them — a real `switch`,
//! `rollback` and `adopt` — so the journals that account for them are real
//! too. Where a test needs a state no command produces (a planted object, a
//! half-removed tombstone), it builds it by hand and says so.

// The harness builds and tampers with fixture trees with the ordinary
// standard library; the guards only ever scan src/.
#![allow(clippy::disallowed_methods)]

mod common;

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use common::{adopting, identities, redact, switching, undate_ids, Fixture};
use ricepilot::ops::read;

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// Replace every size in bytes: a link's size is the length of its target
/// string, and the targets here are absolute paths into the fixture, whose
/// length depends on where the repository is checked out. Sizes are asserted
/// separately, against the fixture itself.
fn unbyte(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(" bytes") {
        let head = &rest[..i];
        let digits = head.len() - head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            out.push_str(&rest[..i + " bytes".len()]);
            rest = &rest[i + " bytes".len()..];
            continue;
        }
        out.push_str(&head[..head.len() - digits]);
        out.push_str("<N> bytes");
        rest = &rest[i + " bytes".len()..];
        // A human-readable size after it, `(1.2 KiB)`.
        if rest.starts_with(" (") {
            if let Some(end) = rest.find(')') {
                rest = &rest[end + 1..];
            }
        }
    }
    out.push_str(rest);
    out.replace("1 byte;", "<N> bytes;")
}

/// Replace every count of directories: an attic entry holds a directory
/// for each component of the displaced path, and the fixture's path is as
/// deep as the checkout. Counted separately, against the fixture.
fn undir(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(" director") {
        let head = &rest[..i];
        let digits = head.len() - head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        out.push_str(&head[..head.len() - digits]);
        if digits > 0 {
            out.push_str("<N>");
        }
        out.push_str(" director");
        rest = &rest[i + " director".len()..];
    }
    out.push_str(rest);
    out.replace("<N> directory,", "<N> directories,")
        .replace("<N> directory;", "<N> directories;")
}

fn tidy(s: &str, f: &Fixture) -> String {
    undir(&unbyte(&undate_ids(&redact(s, f))))
}

/// Run the binary with `answers` on stdin (`None`: stdin closed at once, no
/// answer at all).
fn run(f: &Fixture, args: &[&str], answers: Option<&str>) -> Run {
    let mut cmd = common::ricepilot(f);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(a) = answers {
        // A process that exits before asking closes the pipe; that is fine.
        let _ = stdin.write_all(a.as_bytes());
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    Run {
        stdout: tidy(&String::from_utf8_lossy(&out.stdout), f),
        stderr: tidy(&String::from_utf8_lossy(&out.stderr), f),
        code: out.status.code().unwrap(),
    }
}

fn ok(r: &Run) {
    assert_eq!(r.code, 0, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
}

/// The names in a directory of the state, in order.
fn names(dir: &Path) -> Vec<String> {
    match read::lstat_or_absent(dir).unwrap() {
        None => Vec::new(),
        Some(_) => read::list_dir(dir)
            .unwrap()
            .into_iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect(),
    }
}

/// A machine that has switched `old` → `new` and rolled back: two attic
/// entries, each accounted for by its journal and holding only links — the
/// displaced old links (into `rice/old` and `rice/new`, directories outside
/// the attic with files in them) and the retired `btop` link.
fn switched_and_rolled_back(case: &str) -> switching::Machine {
    let m = switching::build(case);
    ok(&run(&m.f, &["switch", "new", "--commit"], None));
    ok(&run(&m.f, &["rollback", "--commit"], None));
    assert_eq!(names(&m.f.state().join("attic")).len(), 2);
    m
}

/// A verify-config scratch copy as the sandbox leaves one (D55): `root/`
/// with the stripped config, and `run/`, here with the socket a Hyprland
/// run can leave in it.
fn verify_copy(f: &Fixture, name: &str) -> PathBuf {
    let dir = f.state().join("verify").join(name);
    let conf = dir.join("root/home/.config/hypr/hyprland.conf");
    std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
    std::fs::write(&conf, "monitor=,preferred,auto,1\n\n").unwrap();
    std::fs::create_dir_all(dir.join("run/hypr")).unwrap();
    // A fifo, standing in for the socket a Hyprland run leaves: neither a
    // file, a directory nor a link. (A socket's path would be longer than
    // `sun_path` allows, this deep in the fixture tree.)
    rustix::fs::mknodat(
        rustix::fs::CWD,
        dir.join("run/hypr/.socket.sock"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_bits_truncate(0o600),
        0,
    )
    .unwrap();
    dir
}

/// Assert that nothing changed outside `allowed` (and below them): every
/// path that was there is there with the same `(dev, ino, mtime_ns)`, and
/// nothing appeared. The state directory itself is the same inode, and only
/// its mtime may move, since `state/gc/` is made in it.
fn unchanged_outside(
    before: &BTreeMap<PathBuf, (u64, u64, i64)>,
    after: &BTreeMap<PathBuf, (u64, u64, i64)>,
    allowed: &[PathBuf],
) {
    let exempt = |p: &Path| allowed.iter().any(|a| p.starts_with(a));
    for (p, v) in before {
        if p.ends_with(".local/state/ricepilot") {
            let now = after.get(p).expect("the state directory is there");
            assert_eq!((now.0, now.1), (v.0, v.1), "{}", p.display());
        } else if !exempt(p) {
            assert_eq!(after.get(p), Some(v), "{} changed", p.display());
        }
    }
    for p in after.keys() {
        if !exempt(p) {
            assert!(before.contains_key(p), "{} appeared", p.display());
        }
    }
}

// ---------------------------------------------------------------------------
// Itemising
// ---------------------------------------------------------------------------

#[test]
fn a_machine_with_nothing_to_collect_says_so() {
    let f = Fixture::new_in("gc", "nothing");
    let r = run(&f, &["gc"], None);
    ok(&r);
    insta::assert_snapshot!("gc_nothing", r.stdout);
    let r = run(&f, &["gc", "--commit"], None);
    ok(&r);
    insta::assert_snapshot!("gc_nothing_commit", r.stdout);
}

/// The dry run itemises every entry — path, contents, size, reason — and
/// changes nothing at all: the whole fixture's `(dev, ino, mtime_ns)` is the
/// same afterwards.
#[test]
fn a_dry_run_itemises_everything_and_changes_nothing() {
    let m = switched_and_rolled_back("gc_dry");
    verify_copy(&m.f, "20260101T000000Z");
    let before = identities(&m.f);
    let r = run(&m.f, &["gc"], None);
    ok(&r);
    insta::assert_snapshot!("gc_dry_run", r.stdout);
    assert_eq!(identities(&m.f), before);
    assert!(names(&m.f.state().join("gc")).is_empty());
}

/// The sizes, which the snapshots leave out: the bytes of a links-only entry
/// are the lengths of its link targets.
#[test]
fn the_size_is_what_the_entry_holds() {
    let m = switching::build("gc_size");
    ok(&run(&m.f, &["switch", "new", "--commit"], None));
    let attic = m.f.state().join("attic");
    let entry = attic.join(&names(&attic)[0]);
    let paths = m.paths();
    let _lock = ricepilot::ops::lock::acquire(&m.f.path(".run/ricepilot.lock")).unwrap();
    let survey = ricepilot::gc::candidates(&paths).unwrap();
    let item = survey.items.iter().find(|i| i.path == entry).unwrap();
    let expected: u64 = [&m.old_root.join("hypr"), &m.old_root.join("foot")]
        .iter()
        .map(|t| t.as_os_str().len() as u64)
        .sum();
    assert_eq!(item.tally.links, 2);
    assert_eq!(item.tally.bytes, expected);
    // The entry itself, and one directory per component of the displaced
    // links' parent, `/…/home/.config`, which both share.
    let depth = m.hypr.parent().unwrap().components().count() - 1;
    assert_eq!(item.tally.dirs, 1 + depth);
    assert_eq!(item.tally.files + item.tally.other, 0);
    assert!(item.is_candidate(), "{:?}", item.verdict);
}

// ---------------------------------------------------------------------------
// The name, typed back
// ---------------------------------------------------------------------------

/// The right name removes the entry, and only the entry: its links pointed
/// at directories outside the attic, full of files, and every one of those
/// — and everything else in the fixture — keeps its `(dev, ino, mtime_ns)`.
///
/// A second link is planted at the `.1` name `rename_to_attic` would have
/// used, pointing at a directory that is nobody's profile: gc accounts for
/// it as a displaced link, removes the link, and the directory it points at
/// survives untouched.
#[test]
fn the_right_name_removes_the_links_and_nothing_they_point_at() {
    let m = switching::build("gc_right");
    ok(&run(&m.f, &["switch", "new", "--commit"], None));
    let attic = m.f.state().join("attic");
    let name = names(&attic).remove(0);
    let entry = attic.join(&name);

    let precious = m.f.dir("precious/deep");
    m.f.file("precious/deep/data", "irreplaceable\n");
    let displaced = entry.join(m.hypr.strip_prefix("/").unwrap());
    std::os::unix::fs::symlink(m.f.path("precious"), displaced.with_extension("1")).unwrap();
    assert_eq!(
        read::lstat(&displaced.with_extension("1"))
            .unwrap()
            .unwrap()
            .kind,
        read::Kind::Symlink
    );

    let before = identities(&m.f);
    let r = run(&m.f, &["gc", "--commit"], Some(&format!("{name}\n")));
    ok(&r);
    insta::assert_snapshot!("gc_right_name", r.stdout);

    assert!(read::lstat_or_absent(&entry).unwrap().is_none());
    assert!(
        names(&m.f.state().join("gc")).is_empty(),
        "nothing left behind"
    );
    let after = identities(&m.f);
    unchanged_outside(
        &before,
        &after,
        &[entry.clone(), attic.clone(), m.f.state().join("gc")],
    );
    // Spelled out, for the targets the brief names: each directory a
    // removed link pointed at, and what is in it.
    for p in [
        m.old_root.join("hypr"),
        m.old_root.join("hypr/marker"),
        m.old_root.join("foot"),
        precious.clone(),
        m.f.path("precious"),
        m.f.path("precious/deep/data"),
    ] {
        assert_eq!(after.get(&p), before.get(&p), "{}", p.display());
        assert!(after.contains_key(&p), "{} is gone", p.display());
    }
}

/// A wrong name, an empty line and no answer at all each remove nothing —
/// not the entry asked about, and not the next one either.
#[test]
fn a_wrong_name_an_empty_answer_and_no_answer_remove_nothing() {
    let m = switched_and_rolled_back("gc_wrong");
    let attic = m.f.state().join("attic");
    let [first, second]: [String; 2] = names(&attic).try_into().unwrap();
    let before = identities(&m.f);

    // The other entry's name, then the right name in the wrong case.
    let r = run(
        &m.f,
        &["gc", "--commit"],
        Some(&format!("{second}\n{}\n", first.to_lowercase())),
    );
    ok(&r);
    // The lower-cased name is not a timestamp to `undate`; say what it was.
    insta::assert_snapshot!(
        "gc_wrong_names",
        r.stdout
            .replace(&first.to_lowercase(), "<the name, in lower case>")
    );
    assert_eq!(identities(&m.f), before);

    // Empty lines.
    let r = run(&m.f, &["gc", "--commit"], Some("\n\n"));
    ok(&r);
    insta::assert_snapshot!("gc_empty_answers", r.stdout);
    assert_eq!(identities(&m.f), before);

    // End of input before any answer.
    let r = run(&m.f, &["gc", "--commit"], None);
    ok(&r);
    insta::assert_snapshot!("gc_no_answer", r.stdout);
    assert_eq!(identities(&m.f), before);

    // A name with more on the line is not the name.
    let r = run(&m.f, &["gc", "--commit"], Some(&format!("{first} yes\n")));
    ok(&r);
    assert_eq!(identities(&m.f), before);
    assert!(names(&m.f.state().join("gc")).is_empty());
}

/// Each answer is for one entry: the first named, the second not.
#[test]
fn each_answer_is_for_one_entry() {
    let m = switched_and_rolled_back("gc_one_each");
    let attic = m.f.state().join("attic");
    let [first, second]: [String; 2] = names(&attic).try_into().unwrap();
    let r = run(&m.f, &["gc", "--commit"], Some(&format!("{first}\nno\n")));
    ok(&r);
    insta::assert_snapshot!("gc_one_of_two", r.stdout);
    assert_eq!(names(&attic), vec![second]);
}

/// The entry is read again after its name is typed, and must be exactly what
/// was listed. Here a file is added to it while the question is on the
/// screen: nothing is removed, nothing is moved, and the refusal says why.
#[test]
fn an_entry_that_changes_while_it_is_asked_about_is_not_removed() {
    let m = switching::build("gc_changed");
    ok(&run(&m.f, &["switch", "new", "--commit"], None));
    let attic = m.f.state().join("attic");
    let name = names(&attic).remove(0);
    let entry = attic.join(&name);

    let mut cmd = common::ricepilot(&m.f);
    cmd.args(["gc", "--commit"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).ends_with("to keep it: ") {
        let n = stdout.read(&mut buf).unwrap();
        assert!(n > 0, "no question: {}", String::from_utf8_lossy(&seen));
        seen.extend_from_slice(&buf[..n]);
    }

    let displaced = entry.join(m.foot.strip_prefix("/").unwrap());
    std::fs::write(
        displaced.with_extension("late"),
        "added after the listing\n",
    )
    .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(format!("{name}\n").as_bytes()).unwrap();
    drop(stdin);
    stdout.read_to_end(&mut seen).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = tidy(&String::from_utf8_lossy(&seen), &m.f);
    assert_eq!(
        out.status.code(),
        Some(ricepilot::error::ExitCode::Refused as i32)
    );
    let tail = &text[text.find("to keep it: ").unwrap()..];
    insta::assert_snapshot!("gc_changed_while_asked", tail);

    assert!(read::lstat(&entry).unwrap().is_some(), "it was not moved");
    assert!(read::lstat(&displaced).unwrap().is_some());
    assert!(names(&m.f.state().join("gc")).is_empty());
}

// ---------------------------------------------------------------------------
// The user's own directories (D46, D49)
// ---------------------------------------------------------------------------

/// After `adopt`, the attic holds the user's real directory — moved, not
/// copied. It is a candidate only while its destination is ricepilot's link
/// and the profile holds an identical copy; a copy that has changed keeps
/// it, and so does the adopt-then-rollback state, in which it is the way
/// back.
#[test]
fn an_adopted_directory_is_kept_unless_a_profile_holds_it_and_the_link_is_ours() {
    let m = adopting::build("gc_adopted");
    let r = run(
        &m.f,
        &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
        Some("y\n"),
    );
    ok(&r);

    // Held: the link is ricepilot's, the copy identical.
    let r = run(&m.f, &["gc"], None);
    ok(&r);
    insta::assert_snapshot!("gc_adopted_held", r.stdout);

    // The copy changes: the attic now holds the only copy of the old bytes.
    let conf = m.target.join("monitors.conf");
    let original = std::fs::read(&conf).unwrap();
    std::fs::write(&conf, "monitor=,1920x1080,auto,1\n").unwrap();
    let r = run(&m.f, &["gc"], None);
    ok(&r);
    insta::assert_snapshot!("gc_adopted_copy_differs", r.stdout);
    std::fs::write(&conf, original).unwrap();

    // Rolled back: the destination is empty and this is the way back (D49).
    ok(&run(&m.f, &["rollback", "--commit"], None));
    let r = run(&m.f, &["gc"], None);
    ok(&r);
    insta::assert_snapshot!("gc_adopted_after_rollback", r.stdout);

    // And typing its name does not remove it: kept entries are not asked
    // about at all.
    let dir = m.in_attic().unwrap();
    let before = identities(&m.f);
    let r = run(&m.f, &["gc", "--commit"], Some("no\n"));
    ok(&r);
    assert!(read::lstat(&dir).unwrap().is_some());
    unchanged_outside(&before, &identities(&m.f), &[m.f.state().join("attic")]);
}

/// The brief's case in its plainest form: a directory `adopt` displaced that
/// no profile holds a copy of any more — the profile's copy has been moved
/// out by hand. The attic holds the only copy, so the entry is kept, the
/// report says why, and typing its name at `--commit` asks nothing and
/// removes nothing.
#[test]
fn an_adopted_directory_whose_copy_is_gone_is_kept() {
    let m = adopting::build("gc_adopted_gone");
    ok(&run(
        &m.f,
        &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
        Some("y\n"),
    ));
    let dir = m.in_attic().unwrap();
    let name = names(&m.f.state().join("attic")).remove(0);
    std::fs::rename(&m.target, m.f.path("moved-away")).unwrap();

    let r = run(&m.f, &["gc"], None);
    ok(&r);
    insta::assert_snapshot!("gc_adopted_copy_gone", r.stdout);
    assert!(r.stdout.contains("no candidates, 1 kept."), "{}", r.stdout);

    let before = identities(&m.f);
    let r = run(&m.f, &["gc", "--commit"], Some(&format!("{name}\n")));
    ok(&r);
    assert!(!r.stdout.contains("type "), "a kept entry was asked about");
    assert!(read::lstat(&dir).unwrap().is_some());
    assert_eq!(identities(&m.f), before);
}

/// `adopt` puts a real directory at its attic place and nothing else, so a
/// link found there is not something the journal accounts for — its target
/// string is in no record — and it keeps the entry, even with the copy held
/// and the destination ricepilot's link.
#[test]
fn a_link_where_adopt_put_a_directory_keeps_the_entry() {
    let m = adopting::build("gc_adopted_link");
    ok(&run(
        &m.f,
        &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
        Some("y\n"),
    ));
    let dir = m.in_attic().unwrap();
    std::fs::rename(&dir, m.f.path("moved-out")).unwrap();
    std::os::unix::fs::symlink(m.f.path("moved-out"), &dir).unwrap();

    let r = run(&m.f, &["gc"], None);
    ok(&r);
    assert!(r.stdout.contains("no candidates, 1 kept."), "{}", r.stdout);
    assert!(
        r.stdout
            .contains("/.config/hypr, a link no record says ricepilot put there"),
        "{}",
        r.stdout
    );
}

/// With the copy proven, the adopted directory goes when its name is typed —
/// and the profile's copy, and the live link to it, are untouched.
#[test]
fn an_adopted_directory_a_profile_holds_is_removed_when_named() {
    let m = adopting::build("gc_adopted_rm");
    ok(&run(
        &m.f,
        &["adopt", "~/.config/hypr", "--into", "mine", "--commit"],
        Some("y\n"),
    ));
    let dir = m.in_attic().unwrap();
    let attic = m.f.state().join("attic");
    let name = names(&attic).remove(0);
    let before = identities(&m.f);

    let r = run(&m.f, &["gc", "--commit"], Some(&format!("{name}\n")));
    ok(&r);
    insta::assert_snapshot!("gc_adopted_removed", r.stdout);
    assert!(read::lstat_or_absent(&dir).unwrap().is_none());
    unchanged_outside(
        &before,
        &identities(&m.f),
        &[attic.clone(), m.f.state().join("gc")],
    );
    assert_eq!(read::readlink(&m.dest).unwrap(), m.target);
}

// ---------------------------------------------------------------------------
// What keeps an entry
// ---------------------------------------------------------------------------

/// Everything gc cannot account for keeps its entry, and the report says
/// which path and why: a name ricepilot does not give, something that is
/// not a directory, an entry with no journal, an object no journal names, a
/// file where a link was displaced, a live link into an entry, a rescue
/// attic whose generation is gone, a verify copy with something the sandbox
/// does not make, a directory gc could not empty, and an entry whose
/// interrupted removal is still waiting.
#[test]
fn every_reason_to_keep_an_entry_is_given() {
    use std::os::unix::fs::PermissionsExt as _;

    let m = switched_and_rolled_back("gc_kept");
    let state = m.f.state();
    let attic = state.join("attic");
    let [first, second]: [String; 2] = names(&attic).try_into().unwrap();

    // Not ricepilot's name, and not a directory.
    std::fs::create_dir_all(attic.join("my-backup")).unwrap();
    std::fs::write(attic.join("my-backup/notes.txt"), "mine\n").unwrap();
    std::fs::write(attic.join("20250101T000000Z"), "a file\n").unwrap();
    // No journal names it.
    std::fs::create_dir_all(attic.join("20250102T000000Z/home")).unwrap();
    // A rescue attic whose generation does not exist.
    std::fs::create_dir_all(attic.join("rescue-0042")).unwrap();
    // Something no journal names, beside the links the first one does.
    let displaced_hypr = attic.join(&first).join(m.hypr.strip_prefix("/").unwrap());
    std::fs::write(displaced_hypr.with_extension("orig"), "left by hand\n").unwrap();
    // In the second: the retired btop link replaced by a directory, and a
    // live link (at a destination ricepilot knows) pointing into it.
    let retired = attic.join(&second).join(m.btop.strip_prefix("/").unwrap());
    std::fs::remove_file(&retired).unwrap();
    std::fs::create_dir(&retired).unwrap();
    std::fs::set_permissions(&retired, std::fs::Permissions::from_mode(0o555)).unwrap();
    m.f.clear(".config/btop");
    std::os::unix::fs::symlink(&retired, &m.btop).unwrap();
    // A verify copy holding something the sandbox never makes.
    let v = verify_copy(&m.f, "20260101T000000Z");
    std::fs::write(v.join("extra.txt"), "?\n").unwrap();
    // An interrupted removal of the same name as a live entry.
    std::fs::create_dir_all(state.join("gc").join(format!("attic-{second}"))).unwrap();

    let before = identities(&m.f);
    let r = run(&m.f, &["gc", "--commit"], None);
    std::fs::set_permissions(&retired, std::fs::Permissions::from_mode(0o755)).unwrap();
    ok(&r);
    insta::assert_snapshot!("gc_every_reason_to_keep", r.stdout);
    unchanged_outside(&before, &identities(&m.f), &[]);
}

// ---------------------------------------------------------------------------
// Verify copies and interrupted removals
// ---------------------------------------------------------------------------

/// A verify-config copy (D55) goes when its name is typed — sockets and all.
#[test]
fn a_verify_copy_is_removed_when_named() {
    let f = Fixture::new_in("gc", "verify");
    let v = verify_copy(&f, "20260101T000000Z");
    let r = run(&f, &["gc", "--commit"], Some("20260101T000000Z\n"));
    ok(&r);
    insta::assert_snapshot!("gc_verify_removed", r.stdout);
    assert!(read::lstat_or_absent(&v).unwrap().is_none());
}

/// A crash part way through a removal leaves the tombstone half-emptied in
/// `state/gc/` (D62). Built here by hand — the state is a rename followed by
/// some of the removals — and then listed as an interrupted removal, and
/// finished only when its own name is typed again.
#[test]
fn an_interrupted_removal_is_listed_and_finished_only_when_named_again() {
    let m = switching::build("gc_interrupted");
    ok(&run(&m.f, &["switch", "new", "--commit"], None));
    let attic = m.f.state().join("attic");
    let name = names(&attic).remove(0);
    let gc = m.f.state().join("gc");
    std::fs::create_dir_all(&gc).unwrap();
    let tomb = gc.join(format!("attic-{name}"));
    std::fs::rename(attic.join(&name), &tomb).unwrap();
    std::fs::remove_file(tomb.join(m.hypr.strip_prefix("/").unwrap())).unwrap();

    let r = run(&m.f, &["gc"], None);
    ok(&r);
    insta::assert_snapshot!("gc_interrupted_listed", r.stdout);

    // The attic entry's own name is not the tombstone's.
    let r = run(&m.f, &["gc", "--commit"], Some(&format!("{name}\n")));
    ok(&r);
    assert!(read::lstat(&tomb).unwrap().is_some());

    let before_targets = m.profile_identities();
    let r = run(&m.f, &["gc", "--commit"], Some(&format!("attic-{name}\n")));
    ok(&r);
    insta::assert_snapshot!("gc_interrupted_finished", r.stdout);
    assert!(read::lstat_or_absent(&tomb).unwrap().is_none());
    assert_eq!(m.profile_identities(), before_targets);
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// While an operation is in flight nothing is itemised, let alone removed:
/// its attic is `recover`'s working space.
#[test]
fn an_operation_in_flight_refuses_gc() {
    let m = switching::build("gc_in_flight");
    ok(&run(&m.f, &["switch", "new", "--commit"], None));
    let jdir = m.f.state().join("journal");
    let done = jdir.join(&names(&jdir)[0]);
    std::fs::copy(&done, jdir.join("current.toml")).unwrap();
    let before = identities(&m.f);
    for args in [&["gc"][..], &["gc", "--commit"][..]] {
        let r = run(&m.f, args, Some("anything\n"));
        assert_eq!(r.code, ricepilot::error::ExitCode::Refused as i32);
        assert_eq!(r.stdout, "");
        insta::assert_snapshot!("gc_in_flight", r.stderr);
    }
    assert_eq!(identities(&m.f), before);
}

/// Another ricepilot holding the lock: gc does not queue, and does not look.
#[test]
fn a_held_lock_refuses_gc() {
    let m = switched_and_rolled_back("gc_locked");
    let _held = ricepilot::ops::lock::acquire(&m.f.path(".run/ricepilot.lock")).unwrap();
    let r = run(&m.f, &["gc", "--commit"], None);
    assert_eq!(r.code, ricepilot::error::ExitCode::Locked as i32);
    assert_eq!(r.stdout, "");
}

/// An attic that is a symlink is not followed out of the state directory:
/// gc refuses, and the directory it points at — with a perfectly
/// ricepilot-shaped entry in it — is untouched.
#[test]
fn an_attic_that_is_a_link_is_not_followed() {
    let f = Fixture::new_in("gc", "attic_link");
    let elsewhere = f.dir("elsewhere/attic/20260101T000000Z/home");
    let attic = f.state().join("attic");
    std::os::unix::fs::symlink(elsewhere.parent().unwrap().parent().unwrap(), &attic).unwrap();
    // The lock file, made once, so that making it is not the difference.
    drop(ricepilot::ops::lock::acquire(&f.path(".run/ricepilot.lock")).unwrap());
    let before = identities(&f);
    let r = run(&f, &["gc", "--commit"], Some("20260101T000000Z\n"));
    assert_eq!(r.code, ricepilot::error::ExitCode::Refused as i32);
    insta::assert_snapshot!("gc_attic_is_a_link", r.stderr);
    assert_eq!(identities(&f), before);
}

// ---------------------------------------------------------------------------
// The proof type
// ---------------------------------------------------------------------------

/// `Named` — the only thing `gc::collect` accepts as a yes — is written in
/// one place, `confirm::typed_back`, and `gc::collect` is called from one
/// place, the command. So nothing in the crate removes an entry without a
/// name having been read back from a human.
#[test]
fn the_name_is_only_ever_read_back_from_a_human() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let rel = p.strip_prefix(&src).unwrap().display().to_string();
            for needle in ["Named {", "gc::collect(", "remove::erase(", "remove::bury("] {
                if text.contains(needle) {
                    hits.entry(needle).or_default().push(rel.clone());
                }
            }
        }
    }
    let only = |needle: &str, file: &str| {
        assert_eq!(
            hits.get(needle).cloned().unwrap_or_default(),
            vec![file.to_string()],
            "{needle}"
        );
    };
    // The struct's definition and its one constructor.
    only("Named {", "cli/confirm.rs");
    only("gc::collect(", "cli/gc.rs");
    only("remove::erase(", "gc/mod.rs");
    only("remove::bury(", "gc/mod.rs");
}
