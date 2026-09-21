//! Fixture harness. Builds throwaway trees under a configurable root and
//! never outside the repo (`SAFETY.md` R1): the default is
//! `target/fixtures/`, *not* `/tmp`, because `/tmp` is a tmpfs with a
//! different `st_dev` and `rename(2)` behaves differently across it.

// This is a test harness living outside src/, so it may build trees with the
// ordinary standard library. The guards it protects only ever scan src/.
#![allow(clippy::disallowed_methods)]
#![allow(dead_code)]

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
    /// Build (or rebuild from empty) the tree for `case`.
    pub fn new(case: &str) -> Self {
        let home = fixture_root().join("m1").join(case).join("home");
        // Rebuilding under a fresh name per run would leave litter; instead
        // each case owns a stable directory whose *contents* are recreated.
        // Nothing here removes anything — the case directory is simply
        // overwritten, and a stale entry from an older run would fail the
        // test loudly rather than silently.
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::create_dir_all(home.join(".local/share/ricepilot/profiles")).unwrap();
        std::fs::create_dir_all(home.join(".local/state/ricepilot")).unwrap();
        Self { home }
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
