//! Where ricepilot keeps things, and how tests keep away from the real one.
//!
//! Every location is overridable by an environment variable. That is not a
//! convenience: `SAFETY.md` R1 forbids any test from touching the real
//! `$HOME`, and the only way to *prove* a test did not is for the test to be
//! able to say where home is.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    /// `~/.local/share/ricepilot`
    pub data: PathBuf,
    /// `~/.local/state/ricepilot`
    pub state: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = match std::env::var_os("RICEPILOT_HOME").or_else(|| std::env::var_os("HOME")) {
            Some(h) => PathBuf::from(h),
            None => {
                return Err(Error::Refused {
                    rule: "R1",
                    path: PathBuf::from("$HOME"),
                    why: "neither RICEPILOT_HOME nor HOME is set".into(),
                })
            }
        };
        Ok(Self::rooted_at(home))
    }

    pub fn rooted_at(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let data = match std::env::var_os("RICEPILOT_DATA_DIR") {
            Some(d) => PathBuf::from(d),
            None => home.join(".local/share/ricepilot"),
        };
        let state = match std::env::var_os("RICEPILOT_STATE_DIR") {
            Some(d) => PathBuf::from(d),
            None => home.join(".local/state/ricepilot"),
        };
        Self { home, data, state }
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.data.join("profiles")
    }

    pub fn profile_dir(&self, name: &str) -> PathBuf {
        self.profiles_dir().join(name)
    }

    pub fn manifest_path(&self, name: &str) -> PathBuf {
        self.profile_dir(name).join("profile.toml")
    }

    pub fn attic_dir(&self) -> PathBuf {
        self.state.join("attic")
    }

    pub fn ledger_path(&self) -> PathBuf {
        self.state.join("ledger.toml")
    }
}

/// A registered profile: its name, its directory and its parsed manifest.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
    pub manifest: crate::manifest::Manifest,
}

impl Profile {
    pub fn root(&self, home: &Path) -> PathBuf {
        self.manifest.root_dir(&self.dir, home)
    }
}

/// Load one profile by name.
pub fn load(paths: &Paths, name: &str) -> Result<Profile> {
    if name.contains('/') || name == "." || name == ".." {
        return Err(Error::Manifest {
            profile: name.to_string(),
            detail: "profile name must be a single directory name".into(),
        });
    }
    let manifest_path = paths.manifest_path(name);
    if crate::ops::read::lstat(&paths.profile_dir(name))?.is_none() {
        return Err(Error::Manifest {
            profile: name.to_string(),
            detail: format!(
                "no such profile. expected a directory at {}",
                paths.profile_dir(name).display()
            ),
        });
    }
    if crate::ops::read::lstat(&manifest_path)?.is_none() {
        return Err(Error::Manifest {
            profile: name.to_string(),
            detail: format!("has no profile.toml at {}", manifest_path.display()),
        });
    }
    let text = crate::ops::read::slurp(&manifest_path)?;
    let manifest = crate::manifest::parse(&text)?;
    if manifest.name != name {
        return Err(Error::Manifest {
            profile: name.to_string(),
            detail: format!(
                "manifest declares name = {:?} but lives in a directory called {name:?}",
                manifest.name
            ),
        });
    }
    Ok(Profile {
        name: name.to_string(),
        dir: paths.profile_dir(name),
        manifest,
    })
}

/// Every registered profile, in name order. A profile directory without a
/// readable `profile.toml` is reported as an error rather than skipped: a
/// half-written profile is worth knowing about.
pub fn load_all(paths: &Paths) -> Result<Vec<Profile>> {
    let dir = paths.profiles_dir();
    if crate::ops::read::lstat(&dir)?.is_none() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in crate::ops::read::list_dir(&dir)? {
        let name = entry.to_string_lossy().into_owned();
        out.push(load(paths, &name)?);
    }
    Ok(out)
}
