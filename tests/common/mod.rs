//! Fixture harness. Builds throwaway trees under `<repo>/target/fixtures/`
//! and never outside it (`SAFETY.md` R1): not `/tmp`, because `/tmp` is a
//! tmpfs with a different `st_dev` and `rename(2)` behaves differently across
//! it, and not the real home, because the user has said so.
//!
//! # The sandbox (D56)
//!
//! Every process that uses this module — each integration-test binary and
//! each crash helper in `examples/` — sets `RICEPILOT_SANDBOX` to
//! [`SANDBOX_ROOT`] on itself the first time it touches the harness
//! ([`enter_the_sandbox`]), and passes it to every ricepilot it starts. Inside the sandbox the library refuses to fall back
//! to `HOME` or `XDG_RUNTIME_DIR`, refuses any location outside the root, and
//! refuses to start `Hyprland`, `hyprctl` or `uwsm` — so a test that forgot
//! something fails loudly rather than reaching the user's session.
//! `RICEPILOT_LIVE_TESTS=1` is the one exception, and only for Hyprland and
//! hyprctl.
//!
//! Nothing outside this module starts ricepilot or a helper: [`ricepilot`]
//! and [`helper`] are the only ways, and `tests/sandbox.rs` fails the build if
//! anything else spells them.

// This is a test harness living outside src/, so it may build trees with the
// ordinary standard library. The guards it protects only ever scan src/.
#![allow(clippy::disallowed_methods)]
#![allow(dead_code)]

pub mod adopting;
pub mod scenario;
pub mod switching;

use std::path::{Path, PathBuf};
use std::process::Command;

use ricepilot::ops::exec::{LIVE_TESTS_VAR, SANDBOX_VAR};

/// `<repo>/target/fixtures`: the sandbox root. Every fixture, and every
/// location the binary under test may resolve, lies below it.
pub const SANDBOX_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target/fixtures");

/// Put this process in the sandbox, once. Called by [`fixture_root`], which
/// every fixture is built through, and by everything else here that a test
/// might reach first — so a test cannot build a fixture, or ask the harness
/// anything, and still be outside the sandbox.
///
/// A constructor that ran before `main` would be tighter, and is not
/// available: it needs `#[link_section]`, which `unsafe_code = "forbid"`
/// rejects, rightly. `set_var` is serialised against every read std makes of
/// the environment (and a `Command` spawn is one of those); it is not
/// serialised against C code calling `getenv` on another thread, and the
/// crate's timestamps are its own arithmetic, not `localtime`. What this
/// cannot cover — a test that names a session-reaching call before it has
/// touched the harness at all — `tests/sandbox.rs` forbids by name.
pub fn enter_the_sandbox() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var(SANDBOX_VAR, SANDBOX_ROOT));
}

/// Whether the tests that reach the live session were asked for.
pub fn live_tests() -> bool {
    enter_the_sandbox();
    std::env::var_os(LIVE_TESTS_VAR).is_some_and(|v| v == "1")
}

/// Panic unless `p` is the sandbox root or lexically below it.
pub fn assert_in_sandbox(what: &str, p: &Path) {
    let root = Path::new(SANDBOX_ROOT);
    assert!(
        p == root || ricepilot::cli::paths::lexically_under(p, root),
        "R1: {what} {} is outside the sandbox {SANDBOX_ROOT}",
        p.display()
    );
}

/// Panic unless `p`, which must exist, resolves — through every symlink —
/// to somewhere below the repository's own `target/fixtures`. Catches a
/// `target` that is a symlink to somewhere else, which the lexical check
/// cannot see.
pub fn assert_resolves_in_sandbox(what: &str, p: &Path) {
    let repo = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
    let real = std::fs::canonicalize(p).unwrap();
    assert!(
        real.starts_with(repo.join("target").join("fixtures")),
        "R1: {what} {} resolves to {}, outside {}/target/fixtures",
        p.display(),
        real.display(),
        repo.display()
    );
}

/// The fixture root. `RICEPILOT_FIXTURE_ROOT` may choose a directory below
/// [`SANDBOX_ROOT`], and nothing else: one outside it panics (D56 narrows
/// D5).
pub fn fixture_root() -> PathBuf {
    enter_the_sandbox();
    let root = match std::env::var_os("RICEPILOT_FIXTURE_ROOT") {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(SANDBOX_ROOT),
    };
    assert_in_sandbox("RICEPILOT_FIXTURE_ROOT", &root);
    root
}

/// The harness's own variables, which every process it starts gets: the
/// sandbox, the fixture root if one was chosen, and the live-tests opt-in if
/// it was given.
pub fn harness_env() -> Vec<(String, String)> {
    let mut v = vec![(SANDBOX_VAR.to_string(), SANDBOX_ROOT.to_string())];
    if std::env::var_os("RICEPILOT_FIXTURE_ROOT").is_some() {
        v.push((
            "RICEPILOT_FIXTURE_ROOT".into(),
            fixture_root().display().to_string(),
        ));
    }
    if live_tests() {
        v.push((LIVE_TESTS_VAR.into(), "1".into()));
    }
    v
}

/// The ricepilot binary under test, with an empty environment plus
/// [`Fixture::env`]: nothing of this process's `HOME`, `XDG_*` or anything
/// else reaches it. The only way a test starts ricepilot.
pub fn ricepilot(f: &Fixture) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("ricepilot"));
    cmd.env_clear().envs(f.env());
    cmd
}

/// A crash helper from `examples/`, which builds its own fixture from its
/// arguments and sets [`Fixture::env`] on itself: an empty environment plus
/// [`harness_env`]. The only way a test starts one.
pub fn helper(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear().envs(harness_env());
    cmd
}

/// `/bin/sh <script>`, for the tests that run a generated rescue script, with
/// an empty environment: the script names every path absolutely and needs
/// nothing from this one.
pub fn sh(script: &Path) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(script).env_clear().current_dir("/");
    cmd
}

/// A throwaway `$HOME`-shaped tree for one test case.
pub struct Fixture {
    pub home: PathBuf,
}

impl Fixture {
    /// Build (or rebuild from empty) the tree for `case` in the `m1` group.
    pub fn new(case: &str) -> Self {
        Self::new_in("m1", case)
    }

    /// Take the tree for `case` exactly as it stands, without rebuilding it.
    /// Used to inspect what a helper process left behind.
    pub fn attach(group: &str, case: &str) -> Self {
        let home = fixture_root().join(group).join(case).join("home");
        Self::check_sandbox(&home);
        Self { home }
    }

    /// Same, in a named group, so one milestone's fixtures cannot collide with
    /// another's.
    pub fn new_in(group: &str, case: &str) -> Self {
        let home = fixture_root().join(group).join(case).join("home");
        Self::check_sandbox(&home);
        // Each case owns a stable directory, and that directory starts empty:
        // a crash-injection case is *defined* by the exact state it starts
        // from, so a leftover staged link from the previous run would make a
        // test pass or fail for a reason that has nothing to do with the code.
        // D18 covers why the harness may do this and the crate may not.
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::create_dir_all(home.join(".local/share/ricepilot/profiles")).unwrap();
        std::fs::create_dir_all(home.join(".local/state/ricepilot")).unwrap();
        assert_resolves_in_sandbox("the fixture home", &home);
        Self { home }
    }

    /// Before anything is built: the process is in the sandbox and `home`
    /// is inside it. Something that took the process out of the sandbox
    /// again stops here, not at the first subprocess.
    fn check_sandbox(home: &Path) {
        assert_eq!(
            std::env::var_os(SANDBOX_VAR).as_deref(),
            Some(std::ffi::OsStr::new(SANDBOX_ROOT)),
            "R1: this process is not in the test sandbox ({SANDBOX_VAR} is not {SANDBOX_ROOT})"
        );
        assert_in_sandbox("the fixture home", home);
    }

    /// `~/.local/state/ricepilot`, where the journal, the attic and the
    /// `RENAME_EXCHANGE` probe links live.
    pub fn state(&self) -> PathBuf {
        self.path(".local/state/ricepilot")
    }

    /// A fresh attic directory for one switch, as `switch` would name it.
    pub fn attic(&self, ts: &str) -> PathBuf {
        let p = self.state().join("attic").join(ts);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The `(dev, ino)` of whatever is at `rel`, without following a final
    /// symlink. Used to assert a pre-existing target was never touched.
    pub fn ident(&self, rel: &str) -> (u64, u64) {
        let m = std::fs::symlink_metadata(self.path(rel)).unwrap();
        use std::os::unix::fs::MetadataExt as _;
        (m.dev(), m.ino())
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    pub fn dir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    pub fn file(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Create a symlink at `rel` pointing at `target`, replacing whatever a
    /// previous run left there.
    pub fn link(&self, rel: &str, target: &Path) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        // A leftover link from an earlier run of the same case must not make
        // the test pass or fail for the wrong reason.
        let _ = std::fs::remove_file(&p);
        std::os::unix::fs::symlink(target, &p).unwrap();
        p
    }

    /// Bind a Unix socket at `rel`, replacing whatever a previous run left
    /// there, and return its path. The listener is dropped at once; the
    /// socket file stays, which is all an `lstat` sees.
    ///
    /// It is bound through `/proc/self/fd/<fd>/<name>`, `<fd>` being the
    /// parent directory held open, rather than through its own path (D64).
    /// `sun_path` holds 107 bytes, and a fixture below a deep checkout —
    /// makepkg's `packaging/arch/src/ricepilot/target/fixtures/…` — is longer
    /// than that, so `bind` would fail with "path must be shorter than
    /// SUN_LEN" for a reason that has nothing to do with the test. The kernel
    /// resolves the `/proc` link to the directory itself, so the socket is
    /// made exactly where the long path says.
    pub fn socket(&self, rel: &str) -> PathBuf {
        use std::os::fd::AsRawFd as _;
        let p = self.path(rel);
        let parent = p.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        let _ = std::fs::remove_file(&p);
        let dir = std::fs::File::open(parent).unwrap();
        let short = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
            .join(p.file_name().unwrap());
        std::os::unix::net::UnixListener::bind(short).unwrap();
        p
    }

    /// Ensure nothing exists at `rel`, so the "absent" shape is really absent.
    pub fn clear(&self, rel: &str) {
        let p = self.path(rel);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir_all(&p);
    }

    /// Write a `profile.toml` for `name` and return its directory.
    pub fn profile(&self, name: &str, toml: &str) -> PathBuf {
        let dir = self.dir(&format!(".local/share/ricepilot/profiles/{name}"));
        std::fs::write(dir.join("profile.toml"), toml).unwrap();
        dir
    }

    /// The environment every fixture-driven command runs under: the
    /// sandbox, every location override, and `HOME` and `XDG_*` pointing
    /// into the fixture too — so that even something that ignored the
    /// overrides would find only the fixture. Panics if any location it would
    /// hand out lies outside the sandbox (D56).
    pub fn env(&self) -> Vec<(String, String)> {
        let at = |rel: &str| self.home.join(rel).display().to_string();
        let mut v = harness_env();
        v.extend([
            ("RICEPILOT_HOME".into(), self.home.display().to_string()),
            ("RICEPILOT_RUNTIME_DIR".into(), at(".run")),
            ("RICEPILOT_DATA_DIR".into(), at(".local/share/ricepilot")),
            ("RICEPILOT_STATE_DIR".into(), at(".local/state/ricepilot")),
            ("HOME".into(), self.home.display().to_string()),
            ("XDG_CONFIG_HOME".into(), at(".config")),
            ("XDG_DATA_HOME".into(), at(".local/share")),
            ("XDG_STATE_HOME".into(), at(".local/state")),
            ("XDG_CACHE_HOME".into(), at(".cache")),
            ("XDG_RUNTIME_DIR".into(), at(".run")),
        ]);
        for (k, val) in &v {
            if k != LIVE_TESTS_VAR {
                assert_in_sandbox(k, Path::new(val));
            }
        }
        v
    }
}

/// `(dev, ino, mtime_ns)` of every path under the fixture's case directory,
/// the directory itself included — the shape of
/// `switching::Machine::profile_identities`, over everything. A read-only
/// command run between two of these must leave them equal: nothing moved,
/// appeared or went away (D57, D60).
pub fn identities(f: &Fixture) -> std::collections::BTreeMap<PathBuf, (u64, u64, i64)> {
    fn collect(p: &Path, out: &mut std::collections::BTreeMap<PathBuf, (u64, u64, i64)>) {
        let m = ricepilot::ops::read::lstat(p).unwrap().unwrap();
        out.insert(p.to_path_buf(), (m.dev, m.ino, m.mtime_ns));
        if m.kind == ricepilot::ops::read::Kind::Dir {
            for name in ricepilot::ops::read::list_dir(p).unwrap() {
                collect(&p.join(name), out);
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    collect(f.home.parent().unwrap(), &mut out);
    out
}

/// A file of this repository's own source, for the tests that check what a
/// module may name.
pub fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

/// Every `prefix…` path spelled in `text`, as the longest run of path
/// characters, with a trailing `::` dropped. A brace right after one — a
/// grouped import — is returned as `…::{`, so it can never pass a list of
/// single items. What `doctor` and `diff` may name is checked with this
/// (D57, D60).
pub fn paths_after(text: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices(prefix) {
        let before = text[..i].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':') {
            continue;
        }
        let rest = &text[i..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(rest.len());
        let mut p = rest[..end].to_string();
        if rest[end..].starts_with('{') {
            p.push('{');
        } else {
            while p.ends_with(':') {
                p.pop();
            }
        }
        out.push(p);
    }
    out
}

/// Redact the fixture root out of command output so snapshots are stable
/// across machines and checkouts.
pub fn redact(out: &str, fixture: &Fixture) -> String {
    out.replace(&fixture.home.display().to_string(), "<HOME>")
}

/// Replace every `YYYYMMDDTHHMMSSZ` timestamp with `<TS>`, so snapshots do not
/// depend on when they were taken. Only the timestamp: the paths around it are
/// exactly what the user is told, and redacting those would hide the thing
/// worth reviewing.
pub fn undate(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let is_ts = |i: usize| {
        i + 16 <= b.len()
            && b[i..i + 8].iter().all(char::is_ascii_digit)
            && b[i + 8] == 'T'
            && b[i + 9..i + 15].iter().all(char::is_ascii_digit)
            && b[i + 15] == 'Z'
    };
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if is_ts(i) {
            out.push_str("<TS>");
            i += 16;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// [`undate`], and the `-<n>` that `journal::unique_id` appends after a
/// timestamp when two operations start in the same second (D40). Whether a
/// second command lands in the first one's second is a race with the wall
/// clock, so for a snapshot of several commands in a row the suffix is not
/// part of what is reviewed; `tests/journal.rs` tests D40 itself.
pub fn undate_ids(s: &str) -> String {
    let dated = undate(s);
    let mut out = String::with_capacity(dated.len());
    let mut rest = dated.as_str();
    while let Some(i) = rest.find("<TS>-") {
        out.push_str(&rest[..i + "<TS>".len()]);
        let after = &rest[i + "<TS>-".len()..];
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        rest = if digits > 0 {
            &after[digits..]
        } else {
            out.push('-');
            after
        };
    }
    out.push_str(rest);
    out
}
