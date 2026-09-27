//! `ricepilot diff <profile>`, end to end (D60): a profile's links and tree
//! against the live filesystem and what ricepilot recorded — identical,
//! drifted, missing, volatile-only, an installer's real directory, a link
//! ricepilot did not make, a record it cannot read — and every refusal.
//!
//! The gate is the same one `doctor` has (D57): every run takes the
//! `(dev, ino, mtime_ns)` of every path in the whole fixture — home, state,
//! profiles, rice trees — before and after, and fails if one moved or one
//! appeared. And at the bottom, textually: `src/diff.rs` and `src/diff/` may
//! name only a listed set of pure items and reach the machine only through
//! `Look`.
//!
//! Every invocation is the real binary inside the test sandbox (D56).

// Builds and breaks fixture trees with the standard library; the guards it
// protects only ever scan src/.
#![allow(clippy::disallowed_methods)]

mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{identities, paths_after, redact, source, switching, undate_ids, Fixture};
use ricepilot::error::ExitCode;
use ricepilot::ops::read;

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// What differs between machines and runs and is not what the report is
/// about: the fixture's location and the timestamps (with the `-<n>` two
/// operations in one second get, D40). diff prints no inode, no mtime and no
/// subprocess output, so there is nothing else to take out.
fn run(f: &Fixture, args: &[&str]) -> Run {
    let mut cmd = common::ricepilot(f);
    cmd.args(args);
    let out = cmd.output().unwrap();
    Run {
        stdout: undate_ids(&redact(&String::from_utf8_lossy(&out.stdout), f)),
        stderr: undate_ids(&redact(&String::from_utf8_lossy(&out.stderr), f)),
        code: out.status.code().unwrap(),
    }
}

/// `ricepilot diff <args>`, asserting it changed nothing anywhere in the
/// fixture — refusals included.
fn diff(f: &Fixture, args: &[&str]) -> Run {
    let before = identities(f);
    let mut all = vec!["diff"];
    all.extend_from_slice(args);
    let r = run(f, &all);
    assert_eq!(
        before,
        identities(f),
        "diff changed the fixture (a path moved, appeared or went away)"
    );
    r
}

fn code(r: &Run, want: ExitCode) {
    assert_eq!(r.code, want as i32, "{}\n{}", r.stdout, r.stderr);
}

fn reported(r: &Run, want: ExitCode) {
    code(r, want);
    assert!(r.stderr.is_empty(), "{}", r.stderr);
}

/// The M3 machine — `old` live and in the ledger, `new` registered and
/// claiming `btop` as well — switched to `new` through the real command: a
/// ledger owning three links into `new`, and `new`'s tree recorded.
fn switched(case: &str) -> switching::Machine {
    let m = switching::build(&format!("diff_{case}"));
    let r = run(&m.f, &["switch", "new", "--commit"]);
    code(&r, ExitCode::Ok);
    assert_eq!(m.live(), m.all_new());
    m
}

fn rm(p: &Path) {
    let m = std::fs::symlink_metadata(p).unwrap();
    if m.is_dir() {
        std::fs::remove_dir_all(p).unwrap();
    } else {
        std::fs::remove_file(p).unwrap();
    }
}

/// Give `p` an mtime no fixture file was made at, so a rewrite with the same
/// bytes is certain to read as `touched`.
fn stamp(p: &Path) {
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
        .unwrap();
}

fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

// ---------------------------------------------------------------------------
// Identical
// ---------------------------------------------------------------------------

/// The profile that is live, as it was recorded: every link `same`, the tree
/// what was recorded, the volatile files listed apart, exit 0.
#[test]
fn a_live_profile_as_recorded_is_identical_and_exits_zero() {
    let m = switched("identical");
    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Ok);
    insta::assert_snapshot!(r.stdout);
}

/// diff takes no lock, so it answers while another ricepilot holds it.
#[test]
fn diff_runs_beside_a_held_lock() {
    let m = switched("locked");
    let _held = ricepilot::ops::lock::acquire(&m.f.path(".run/ricepilot.lock")).unwrap();
    reported(&diff(&m.f, &["new"]), ExitCode::Ok);
    // And a command that does take it is refused, so it really is held.
    code(&run(&m.f, &["recover"]), ExitCode::Locked);
}

// ---------------------------------------------------------------------------
// Volatile only
// ---------------------------------------------------------------------------

/// Everything that changed is `volatile` or was rewritten with the bytes it
/// already had: listed and counted, never drift. Exit 0.
#[test]
fn only_volatile_paths_and_identical_rewrites_are_not_drift() {
    let m = switched("volatile_only");
    std::fs::write(m.new_root.join("foot/noise.log"), "rewritten by an app\n").unwrap();
    std::fs::write(m.new_root.join("hypr/session.log"), "appeared\n").unwrap();
    // The same bytes again, with an mtime that cannot be the recorded one.
    let marker = m.new_root.join("btop/marker");
    std::fs::write(&marker, "new\n").unwrap();
    stamp(&marker);

    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Ok);
    assert!(r.stdout.contains("  hypr/session.log\n"), "{}", r.stdout);
    insta::assert_snapshot!(r.stdout);
}

// ---------------------------------------------------------------------------
// Drifted
// ---------------------------------------------------------------------------

/// The tree edited by hand — a changed file, an added one, a mode — and a
/// link re-pointed by something that is not ricepilot. Exit 6.
#[test]
fn drift_in_the_tree_and_in_the_links_exits_six() {
    let m = switching::build("diff_drifted");
    // A mode the record is certain to have, whatever the umask.
    chmod(&m.new_root.join("btop/marker"), 0o644);
    code(&run(&m.f, &["switch", "new", "--commit"]), ExitCode::Ok);

    std::fs::write(m.new_root.join("hypr/marker"), "edited by hand\n").unwrap();
    std::fs::write(m.new_root.join("foot/extra.ini"), "added\n").unwrap();
    chmod(&m.new_root.join("btop/marker"), 0o600);
    m.f.link(".config/foot", &m.old_root.join("foot"));

    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Drift);
    insta::assert_snapshot!(r.stdout);
}

/// What caelestia's installer does: the link replaced by a real directory
/// of its own. The row compares it, path by path, with the profile's source
/// for it — `volatile` anchored where the source sits in the profile, so the
/// installer's `noise.log` is left out as the profile's is.
#[test]
fn a_link_an_installer_turned_into_a_directory_is_compared_with_its_source() {
    let m = switched("installer");
    rm(&m.foot);
    m.f.file(".config/foot/marker", "written by an installer\n");
    m.f.file(".config/foot/foot.ini", "font=monospace\n");
    m.f.file(".config/foot/noise.log", "the installer's log\n");
    // And one whose content is the source's, copied with a new mtime.
    rm(&m.hypr);
    let copy = m.f.file(".config/hypr/marker", "new\n");
    stamp(&copy);

    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Drift);
    assert!(!r.stdout.contains("noise.log (file)"), "{}", r.stdout);
    insta::assert_snapshot!(r.stdout);
}

/// A real directory with a file in it diff may not open: the row says the
/// content was NOT compared and why, rather than guessing.
#[test]
fn a_real_directory_that_cannot_be_compared_says_why() {
    let m = switched("installer_unreadable");
    rm(&m.foot);
    let secret = m.f.file(".config/foot/secret", "mode 000\n");
    chmod(&secret, 0o000);
    if std::fs::read(&secret).is_ok() {
        chmod(&secret, 0o644);
        eprintln!(
            "SKIPPED a_real_directory_that_cannot_be_compared_says_why: this user can read a \
             mode-000 file"
        );
        return;
    }
    let r = diff(&m.f, &["new"]);
    // Put the mode back, so the next run of the harness can rebuild the tree.
    chmod(&secret, 0o644);
    reported(&r, ExitCode::Drift);
    insta::assert_snapshot!(r.stdout);
}

// ---------------------------------------------------------------------------
// Missing
// ---------------------------------------------------------------------------

/// A link gone from its destination, and a source gone from the tree — so
/// ricepilot's link to it dangles. Both are differences; the tree's record
/// says what went.
#[test]
fn a_missing_link_and_a_missing_source_differ() {
    let m = switched("missing");
    rm(&m.btop);
    std::fs::rename(m.new_root.join("foot"), m.f.path("foot.moved-away")).unwrap();

    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Drift);
    insta::assert_snapshot!(r.stdout);
}

/// Before any switch: `new` has nothing recorded and none of its links;
/// `old`'s links are all in place, and it has nothing recorded either —
/// which is no difference, and is said to be about the links alone.
#[test]
fn before_any_switch_nothing_is_recorded() {
    let m = switching::build("diff_before");
    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Drift);
    insta::assert_snapshot!("before_any_switch_new", r.stdout);

    let r = diff(&m.f, &["old"]);
    reported(&r, ExitCode::Ok);
    insta::assert_snapshot!("before_any_switch_old", r.stdout);
}

/// The profile switched away from: its links are another profile's, and
/// ricepilot owns a link it does not declare — which a switch to it would
/// retire.
#[test]
fn the_profile_switched_away_from_differs_in_every_link() {
    let m = switched("away");
    let r = diff(&m.f, &["old"]);
    reported(&r, ExitCode::Drift);
    insta::assert_snapshot!(r.stdout);
}

// ---------------------------------------------------------------------------
// By reference, and never linked
// ---------------------------------------------------------------------------

/// A by-reference rice as its own installer left it, before `init`, with
/// every shape a destination can have that is not ricepilot's own link: a
/// link to exactly the profile's source that ricepilot did not make — which
/// differs, because ownership is part of what a switch acts on — a dangling
/// one, a real file, and nothing; and sources that are missing or are a link
/// rather than a directory. A `generated` path is listed as never linked and
/// never compared.
#[test]
fn a_by_reference_rice_before_init_differs_in_every_shape() {
    let f = Fixture::new_in("m5diff", "by_reference");
    let root = f.dir("rice/caelestia");
    f.file("rice/caelestia/hypr/hyprland.lua", "-- a lua config\n");
    f.file("rice/caelestia/fish/config.fish", "set -g x 1\n");
    f.file("rice/caelestia/foot/foot.ini", "font=monospace\n");
    f.link("rice/caelestia/kitty", &root.join("foot"));
    let dir_link = |leaf: &str| {
        format!(
            "\n[[path]]\ndest       = \"~/.config/{leaf}\"\nsrc        = \"{leaf}\"\nkind       \
             = \"dir-link\"\nactivation = \"relogin\"\n"
        )
    };
    let mut manifest = format!(
        "name = \"caelestia\"\nroot = \"{}\"\nvolatile = [\"**/fish_variables\"]\n",
        root.display()
    );
    for leaf in ["hypr", "fish", "foot", "btop", "kitty"] {
        manifest.push_str(&dir_link(leaf));
    }
    manifest.push_str(
        "\n[[path]]\ndest       = \"~/.config/fuzzel\"\nsrc        = \"fuzzel\"\nkind       = \
         \"generated\"\nactivation = \"never\"\n",
    );
    f.profile("caelestia", &manifest);
    f.link(".config/hypr", &root.join("hypr"));
    f.link(".config/fish", &root.join("nowhere"));
    f.file(".config/foot", "not a directory\n");

    let r = diff(&f, &["caelestia"]);
    reported(&r, ExitCode::Drift);
    insta::assert_snapshot!(r.stdout);
}

/// The profile's manifest edited since the record: `volatile` widened, and
/// the tree moved to a new root. Both are said beside the comparison, since
/// each changes what the comparison means; the links into the old root are
/// no longer into any registered profile, so they are not ricepilot's.
#[test]
fn a_manifest_edited_since_the_record_says_so() {
    let m = switched("edited_manifest");
    let moved = m.f.path("rice/new-moved");
    std::fs::rename(&m.new_root, &moved).unwrap();
    let text = std::fs::read_to_string(manifest_of(&m, "new")).unwrap();
    let edited = text
        .replacen(
            &format!("root = \"{}\"", m.new_root.display()),
            &format!("root = \"{}\"", moved.display()),
            1,
        )
        .replacen(
            "volatile = [\"**/*.log\"]",
            "volatile = [\"**/*.log\", \"btop\"]",
            1,
        );
    assert_ne!(edited, text);
    std::fs::write(manifest_of(&m, "new"), edited).unwrap();

    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Drift);
    insta::assert_snapshot!(r.stdout);
}

// ---------------------------------------------------------------------------
// What cannot be read, and what is half done
// ---------------------------------------------------------------------------

/// A recorded manifest that does not parse: the links are still compared,
/// the content is NOT, and the status is 4 — neither 0 nor 6 would be true.
#[test]
fn a_record_that_does_not_parse_is_reported_and_exits_four() {
    let m = switched("unparsable");
    std::fs::write(
        m.f.state().join("manifests/new.toml"),
        "profile = \"new\"\nentry = 3\n",
    )
    .unwrap();

    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Failed);
    insta::assert_snapshot!(r.stdout);
}

/// A journal at `current.toml` — a retired one put back, which is the file
/// an interrupted switch leaves — is said before anything else.
#[test]
fn an_interrupted_operation_is_said_first() {
    let m = switched("journal");
    let dir = m.f.state().join("journal");
    let done = read::list_dir(&dir)
        .unwrap()
        .into_iter()
        .find(|n| n.to_string_lossy().starts_with("done-"))
        .unwrap();
    std::fs::copy(dir.join(done), dir.join("current.toml")).unwrap();

    let r = diff(&m.f, &["new"]);
    reported(&r, ExitCode::Ok);
    insta::assert_snapshot!(r.stdout);
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Everything diff refuses, each on its own machine, each with nothing
/// touched: a profile that is not registered, a name that is not one, a
/// manifest that does not parse, another registered profile's manifest that
/// does not parse (its root is part of the ownership predicate), a ledger
/// that does not parse, a destination behind a symlinked directory (D9),
/// and a manifest asking for what v1 does not do.
#[test]
fn every_refusal() {
    type Setup = fn(&switching::Machine);
    let cases: [(&str, Setup, &str, ExitCode); 7] = [
        ("unknown", |_| {}, "nosuch", ExitCode::Refused),
        ("not_a_name", |_| {}, "../new", ExitCode::Refused),
        (
            "unparsable_manifest",
            |m| {
                std::fs::write(manifest_of(m, "new"), "name = \"new\"\nkind = [\n").unwrap();
            },
            "new",
            ExitCode::Refused,
        ),
        (
            "another_profile_unparsable",
            |m| {
                std::fs::write(manifest_of(m, "old"), "name = \"old\"\nkind = [\n").unwrap();
            },
            "new",
            ExitCode::Refused,
        ),
        (
            "ledger",
            |m| {
                std::fs::write(m.f.state().join("ledger.toml"), "[[entry]]\ndest = 3\n").unwrap();
            },
            "new",
            ExitCode::Refused,
        ),
        (
            "config_is_a_link",
            |m| {
                let real = m.f.path(".config-real");
                std::fs::rename(m.f.path(".config"), &real).unwrap();
                std::os::unix::fs::symlink(&real, m.f.path(".config")).unwrap();
            },
            "new",
            ExitCode::Refused,
        ),
        (
            "file_copy",
            |m| {
                let text = std::fs::read_to_string(manifest_of(m, "new")).unwrap();
                std::fs::write(
                    manifest_of(m, "new"),
                    text.replacen("kind       = \"dir-link\"", "kind       = \"file-copy\"", 1),
                )
                .unwrap();
            },
            "new",
            ExitCode::NotPossible,
        ),
    ];
    let mut s = String::new();
    for (case, setup, profile, want) in cases {
        let m = switching::build(&format!("diff_refuse_{case}"));
        setup(&m);
        let r = diff(&m.f, &[profile]);
        code(&r, want);
        assert!(r.stdout.is_empty(), "{case}: {}", r.stdout);
        s.push_str(&format!(
            "--- {case}: diff {profile}\nexit {}\n{}\n",
            r.code, r.stderr
        ));
    }
    insta::assert_snapshot!(s);
}

fn manifest_of(m: &switching::Machine, profile: &str) -> std::path::PathBuf {
    m.f.path(&format!(
        ".local/share/ricepilot/profiles/{profile}/profile.toml"
    ))
}

// ---------------------------------------------------------------------------
// Read-only by construction
// ---------------------------------------------------------------------------

fn diff_sources() -> Vec<(String, String)> {
    let mut files = vec!["src/diff.rs".to_string()];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/diff");
    let mut more: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| format!("src/diff/{}", e.unwrap().file_name().to_string_lossy()))
        .collect();
    more.sort();
    files.extend(more);
    files
        .into_iter()
        .map(|f| {
            let text = source(&f);
            (f, text)
        })
        .collect()
}

/// **The construction** (D60, as D57 for doctor). diff reaches the machine
/// only through the `&dyn Look` it is given. Every crate item it names is on
/// this list, and each is pure — a type, a path computation, a parser of
/// text already read, a comparison — or reads only through a `Look` it is
/// passed (the `*_via` functions). `ops::look::Live` is named once, by
/// `run`, to hand to `compare`. Nothing here writes, locks, journals,
/// starts a process or builds the verify-config sandbox.
const DIFF_MAY_NAME: &[&str] = &[
    "crate::Error",
    "crate::Result",
    "crate::cli::Output",
    "crate::cli::paths::Paths",
    "crate::cli::paths::load_all_via",
    "crate::cli::paths::load_via",
    "crate::cli::switch::source_facts_via",
    "crate::error::ExitCode",
    "crate::ledger::Ledger",
    "crate::ledger::load_via",
    "crate::manifest::Activation",
    "crate::manifest::Kind",
    "crate::manifest::expand_home",
    "crate::observe::Ownership",
    "crate::observe::Shape",
    "crate::observe::shape_via",
    "crate::ops::look::Live",
    "crate::ops::look::Look",
    "crate::plan::SourceState",
    "crate::verify::AgainstRecord",
    "crate::verify::Difference",
    "crate::verify::against_record_via",
    "crate::verify::build_within_via",
    "crate::verify::compare",
    "crate::verify::manifest_path",
];

/// Words that must not appear anywhere in diff's sources, prose included
/// (as with D3): each names a way to change the machine, a read that goes
/// around `Look`, or a process — diff starts none, not even the two
/// read-only ones `Look` offers.
const DIFF_MUST_NOT_SPELL: &[&str] = &[
    "mutate",
    "ops::lock",
    "lock::",
    "write_atomic",
    "journal::write",
    "mark_done",
    "::save",
    "regenerate",
    "SandboxedConfig",
    "sandbox::",
    "hyprverify",
    "exec::",
    "Call::",
    "ops::read",
    "std::process",
    "std::env",
    "set_var",
    ".record(",
    ".observe(",
    "observe_one",
    "ricepilot::",
    "pacman",
    "sh_syntax",
];

#[test]
fn diff_names_nothing_that_could_change_the_machine() {
    let mut hits = Vec::new();
    for (file, text) in diff_sources() {
        for p in paths_after(&text, "crate::") {
            if !DIFF_MAY_NAME.contains(&p.as_str()) {
                hits.push(format!("{file}: names `{p}`, which is not on the list"));
            }
        }
        for p in paths_after(&text, "super::") {
            let ours = ["Content", "Found", "Recorded", "Report", "Row", "Wanted"]
                .iter()
                .any(|t| p == format!("super::{t}"));
            if !ours {
                hits.push(format!("{file}: names `{p}`"));
            }
        }
        for word in DIFF_MUST_NOT_SPELL {
            for (n, line) in text.lines().enumerate() {
                if line.contains(word) {
                    hits.push(format!(
                        "{file}:{}: spells `{word}`: {}",
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "diff is read-only by construction (D60): it names only pure items and reaches the \
         machine through `Look`\n{}",
        hits.join("\n")
    );
}

/// The guard above can fail: a planted write, a grouped import, a process
/// and a read around `Look` are each caught.
#[test]
fn the_diff_guard_bites() {
    let planted = "use crate::ops::{look, x};\nfn f() { crate::verify::save(p, m); }\n\
                   // pacman -Q\nfn g() { crate::observe::observe(d, o); }\n";
    let names = paths_after(planted, "crate::");
    for n in [
        "crate::ops::{",
        "crate::verify::save",
        "crate::observe::observe",
    ] {
        assert!(names.contains(&n.to_string()), "{n} not found in {names:?}");
    }
    assert!(names.iter().all(|n| !DIFF_MAY_NAME.contains(&n.as_str())));
    assert!(DIFF_MUST_NOT_SPELL.iter().any(|w| planted.contains(w)));
}
