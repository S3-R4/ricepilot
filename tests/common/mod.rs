//! Fixture harness. Builds throwaway trees under a configurable root and
//! never outside the repo (`SAFETY.md` R1): the default is
//! `target/fixtures/`, *not* `/tmp`, because `/tmp` is a tmpfs with a
//! different `st_dev` and `rename(2)` behaves differently across it.

// This is a test harness living outside src/, so it may build trees with the
// ordinary standard library. The guards it protects only ever scan src/.
#![allow(clippy::disallowed_methods)]
#![allow(dead_code)]

pub mod scenario;
pub mod switching;

use std::path::{Path, PathBuf};

pub fn fixture_root() -> PathBuf {
    match std::env::var_os("RICEPILOT_FIXTURE_ROOT") {
        Some(v) => PathBuf::from(v),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("fixtures"),
    }
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
        Self {
            home: fixture_root().join(group).join(case).join("home"),
        }
    }

    /// Same, in a named group, so one milestone's fixtures cannot collide with
    /// another's.
    pub fn new_in(group: &str, case: &str) -> Self {
        let home = fixture_root().join(group).join(case).join("home");
        // Each case owns a stable directory, and that directory starts empty:
        // a crash-injection case is *defined* by the exact state it starts
        // from, so a leftover staged link from the previous run would make a
        // test pass or fail for a reason that has nothing to do with the code.
        // D18 covers why the harness may do this and the crate may not.
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::create_dir_all(home.join(".local/share/ricepilot/profiles")).unwrap();
        std::fs::create_dir_all(home.join(".local/state/ricepilot")).unwrap();
        Self { home }
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

    /// The environment every fixture-driven command must run under, so no
    /// test can reach the real `$HOME`.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("RICEPILOT_HOME".into(), self.home.display().to_string()),
            (
                "RICEPILOT_RUNTIME_DIR".into(),
                self.home.join(".run").display().to_string(),
            ),
            (
                "RICEPILOT_DATA_DIR".into(),
                self.home
                    .join(".local/share/ricepilot")
                    .display()
                    .to_string(),
            ),
            (
                "RICEPILOT_STATE_DIR".into(),
                self.home
                    .join(".local/state/ricepilot")
                    .display()
                    .to_string(),
            ),
        ]
    }
}

/// Redact the fixture root out of command output so snapshots are stable
/// across machines and checkouts.
pub fn redact(out: &str, fixture: &Fixture) -> String {
    out.replace(&fixture.home.display().to_string(), "<HOME>")
}
