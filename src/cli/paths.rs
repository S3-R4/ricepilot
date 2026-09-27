//! Where ricepilot keeps things, and how tests keep away from the real one.
//!
//! Every location is overridable by an environment variable. That is not a
//! convenience: `SAFETY.md` R1 forbids any test from touching the real
//! `$HOME`, and the only way to *prove* a test did not is for the test to be
//! able to say where home is.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    /// `~/.local/share/ricepilot`
    pub data: PathBuf,
    /// `~/.local/state/ricepilot`
    pub state: PathBuf,
    /// `$XDG_RUNTIME_DIR`, where the process lock lives. `None` when neither
    /// override nor `XDG_RUNTIME_DIR` is set: a mutating command then refuses
    /// rather than inventing a location, because a lock nobody else looks for
    /// is worse than no lock at all.
    pub runtime: Option<PathBuf>,
}

/// The variable that puts ricepilot inside the test sandbox (D56). Its value
/// is the sandbox root — `<repo>/target/fixtures` — and while it is set:
///
/// * every location must be given by its own override
///   (`RICEPILOT_HOME`, `RICEPILOT_DATA_DIR`, `RICEPILOT_STATE_DIR`,
///   `RICEPILOT_RUNTIME_DIR`) and lie under the root: nothing falls back to
///   `HOME` or `XDG_RUNTIME_DIR`, which is where the real home is;
/// * `ops::exec` refuses the entries that reach the live session
///   (`Hyprland`, `hyprctl`, `uwsm`), see [`crate::ops::exec::SANDBOX_VAR`].
///
/// It only ever takes capabilities away. There is no variable that gives
/// ricepilot a binary, a location or a permission it would not otherwise
/// have.
pub const SANDBOX_VAR: &str = crate::ops::exec::SANDBOX_VAR;

/// The four location overrides, which the sandbox requires every one of.
pub const OVERRIDES: [&str; 4] = [
    "RICEPILOT_HOME",
    "RICEPILOT_DATA_DIR",
    "RICEPILOT_STATE_DIR",
    "RICEPILOT_RUNTIME_DIR",
];

impl Paths {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    /// [`Paths::from_env`] against any environment, so the resolution — and
    /// the sandbox's refusals — can be tested without touching the test
    /// process's own variables.
    pub fn from_lookup(get: impl Fn(&str) -> Option<OsString>) -> Result<Self> {
        if let Some(root) = sandbox_root(&get)? {
            return Self::sandboxed(&root, &get);
        }
        let home = match get("RICEPILOT_HOME").or_else(|| get("HOME")) {
            Some(h) => PathBuf::from(h),
            None => {
                return Err(Error::Refused {
                    rule: "R1",
                    path: PathBuf::from("$HOME"),
                    why: "neither RICEPILOT_HOME nor HOME is set".into(),
                })
            }
        };
        Ok(Self::rooted_with(home, &get))
    }

    /// Every location from its override, each under `root`, or a refusal.
    fn sandboxed(root: &Path, get: &impl Fn(&str) -> Option<OsString>) -> Result<Self> {
        let mut found = Vec::with_capacity(OVERRIDES.len());
        for name in OVERRIDES {
            let Some(v) = get(name) else {
                return Err(Error::Refused {
                    rule: "R1",
                    path: PathBuf::from(format!("${name}")),
                    why: format!(
                        "{SANDBOX_VAR} is set and {name} is not. Inside the test sandbox every \
                         location must be given explicitly; ricepilot does not fall back to \
                         HOME or XDG_RUNTIME_DIR, which is where the real home is"
                    ),
                });
            };
            let p = PathBuf::from(v);
            if !lexically_under(&p, root) {
                return Err(Error::Refused {
                    rule: "R1",
                    path: p,
                    why: format!(
                        "{name} is not under the sandbox root {} that {SANDBOX_VAR} names \
                         (an absolute path, below the root, with no `..`)",
                        root.display()
                    ),
                });
            }
            found.push(p);
        }
        let [home, data, state, runtime] = <[PathBuf; 4]>::try_from(found).expect("four");
        Ok(Self {
            home,
            data,
            state,
            runtime: Some(runtime),
        })
    }

    /// Locations for an explicit `home`, with the data, state and runtime
    /// overrides read from the environment. Inside the sandbox (D56) the
    /// runtime directory is only ever `RICEPILOT_RUNTIME_DIR`: the fallback,
    /// `XDG_RUNTIME_DIR`, is the real session's.
    pub fn rooted_at(home: impl Into<PathBuf>) -> Self {
        Self::rooted_with(home.into(), &|name| std::env::var_os(name))
    }

    fn rooted_with(home: PathBuf, get: &impl Fn(&str) -> Option<OsString>) -> Self {
        let data = match get("RICEPILOT_DATA_DIR") {
            Some(d) => PathBuf::from(d),
            None => home.join(".local/share/ricepilot"),
        };
        let state = match get("RICEPILOT_STATE_DIR") {
            Some(d) => PathBuf::from(d),
            None => home.join(".local/state/ricepilot"),
        };
        let runtime = match get(SANDBOX_VAR) {
            Some(_) => get("RICEPILOT_RUNTIME_DIR"),
            None => get("RICEPILOT_RUNTIME_DIR").or_else(|| get("XDG_RUNTIME_DIR")),
        }
        .map(PathBuf::from);
        Self {
            home,
            data,
            state,
            runtime,
        }
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

    /// `state/journal/`. The in-flight record lives at `current.toml`; a
    /// finished one is *renamed* to `done-<id>.toml`, never taken away.
    pub fn journal_dir(&self) -> PathBuf {
        self.state.join("journal")
    }

    pub fn journal_path(&self) -> PathBuf {
        self.journal_dir().join("current.toml")
    }

    /// `$XDG_RUNTIME_DIR/ricepilot.lock`.
    pub fn lock_path(&self) -> Result<PathBuf> {
        match &self.runtime {
            Some(r) => Ok(r.join("ricepilot.lock")),
            None => Err(Error::Refused {
                rule: "R4",
                path: PathBuf::from("$XDG_RUNTIME_DIR"),
                why: "neither RICEPILOT_RUNTIME_DIR nor XDG_RUNTIME_DIR is set, so there is \
                      nowhere to take the lock that a second ricepilot would look in"
                    .into(),
            }),
        }
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

/// The sandbox root, if [`SANDBOX_VAR`] is set. A value that is not an
/// absolute, `..`-free path is refused rather than ignored: a sandbox that
/// silently switched itself off would be worse than none.
fn sandbox_root(get: &impl Fn(&str) -> Option<OsString>) -> Result<Option<PathBuf>> {
    let Some(v) = get(SANDBOX_VAR) else {
        return Ok(None);
    };
    let root = PathBuf::from(v);
    if root.is_absolute() && plain(&root) {
        return Ok(Some(root));
    }
    Err(Error::Refused {
        rule: "R1",
        path: root,
        why: format!("{SANDBOX_VAR} must name an absolute directory with no `..`"),
    })
}

/// Absolute, free of `.` and `..`, and strictly below `root`. Lexical, as
/// everything outside `src/ops/` has to be; a symlink inside the sandbox is
/// `ops::read`'s to refuse (D9).
pub fn lexically_under(p: &Path, root: &Path) -> bool {
    p.is_absolute() && plain(p) && p != root && p.starts_with(root)
}

fn plain(p: &Path) -> bool {
    p.components()
        .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    const ROOT: &str = "/repo/target/fixtures";
    const FULL: [(&str, &str); 5] = [
        (SANDBOX_VAR, ROOT),
        ("RICEPILOT_HOME", "/repo/target/fixtures/m1/c/home"),
        (
            "RICEPILOT_DATA_DIR",
            "/repo/target/fixtures/m1/c/home/.local/share/ricepilot",
        ),
        (
            "RICEPILOT_STATE_DIR",
            "/repo/target/fixtures/m1/c/home/.local/state/ricepilot",
        ),
        (
            "RICEPILOT_RUNTIME_DIR",
            "/repo/target/fixtures/m1/c/home/.run",
        ),
    ];

    fn refusal(e: Error) -> (&'static str, PathBuf) {
        match e {
            Error::Refused { rule, path, .. } => (rule, path),
            other => panic!("not a refusal: {other}"),
        }
    }

    /// Outside the sandbox, the fallbacks are what they always were.
    #[test]
    fn outside_the_sandbox_home_and_the_runtime_dir_fall_back() {
        let p =
            Paths::from_lookup(env(&[("HOME", "/home/u"), ("XDG_RUNTIME_DIR", "/run/u")])).unwrap();
        assert_eq!(p.home, Path::new("/home/u"));
        assert_eq!(p.state, Path::new("/home/u/.local/state/ricepilot"));
        assert_eq!(p.runtime.as_deref(), Some(Path::new("/run/u")));
    }

    /// Inside it, each of the four overrides is required: dropping any one is
    /// refused, even with the real HOME and XDG_RUNTIME_DIR right there to be
    /// fallen back on.
    #[test]
    fn inside_the_sandbox_a_missing_override_is_refused_not_fallen_back_from() {
        let mut pairs = FULL.to_vec();
        pairs.push(("HOME", "/home/u"));
        pairs.push(("XDG_RUNTIME_DIR", "/run/u"));
        let p = Paths::from_lookup(env(&pairs)).unwrap();
        assert_eq!(p.home, Path::new(FULL[1].1));
        assert_eq!(p.runtime.as_deref(), Some(Path::new(FULL[4].1)));

        for name in OVERRIDES {
            let without: Vec<_> = pairs.iter().copied().filter(|(k, _)| *k != name).collect();
            let (rule, path) = refusal(Paths::from_lookup(env(&without)).unwrap_err());
            assert_eq!(rule, "R1");
            assert_eq!(path, PathBuf::from(format!("${name}")));
        }
    }

    /// And each must lie below the root, lexically, with no `..` to climb out.
    #[test]
    fn inside_the_sandbox_a_location_outside_the_root_is_refused() {
        for (name, bad) in [
            ("RICEPILOT_HOME", "/home/u"),
            ("RICEPILOT_DATA_DIR", "/home/u/.local/share/ricepilot"),
            ("RICEPILOT_STATE_DIR", "/repo/target/fixtures/../../home/u"),
            ("RICEPILOT_RUNTIME_DIR", "/run/user/1000"),
            ("RICEPILOT_HOME", ROOT),
            ("RICEPILOT_HOME", "relative/home"),
            ("RICEPILOT_HOME", "/repo/target/fixtures-elsewhere/home"),
        ] {
            let pairs: Vec<_> = FULL
                .iter()
                .map(|&(k, v)| if k == name { (k, bad) } else { (k, v) })
                .collect();
            let (rule, path) = refusal(Paths::from_lookup(env(&pairs)).unwrap_err());
            assert_eq!((rule, path), ("R1", PathBuf::from(bad)), "{name}={bad}");
        }
    }

    /// A sandbox root that is not a plain absolute path switches nothing off:
    /// it is refused.
    #[test]
    fn a_malformed_sandbox_root_is_refused() {
        for bad in ["", "target/fixtures", "/repo/../target"] {
            let pairs: Vec<_> = FULL
                .iter()
                .map(|&(k, v)| if k == SANDBOX_VAR { (k, bad) } else { (k, v) })
                .collect();
            let (rule, _) = refusal(Paths::from_lookup(env(&pairs)).unwrap_err());
            assert_eq!(rule, "R1", "{bad:?}");
        }
    }
}
