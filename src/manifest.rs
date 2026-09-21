//! `profile.toml` parsing and validation. Schema is documented in
//! `docs/DESIGN.md` §3.
//!
//! Parsing is pure: it takes the manifest text, never a path, so the only
//! module that reads a byte off the disk is still [`crate::ops`].

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// How a path is activated. v1 supports `dir-link` only; `file-copy` is v1.1.
/// `generated` and `volatile` are *classifications*, never activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    DirLink,
    FileCopy,
    Generated,
    Volatile,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::DirLink => "dir-link",
            Kind::FileCopy => "file-copy",
            Kind::Generated => "generated",
            Kind::Volatile => "volatile",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activation {
    Relogin,
    Live,
    Never,
}

impl Activation {
    pub fn as_str(self) -> &'static str {
        match self {
            Activation::Relogin => "relogin",
            Activation::Live => "live",
            Activation::Never => "never",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathEntry {
    pub dest: PathBuf,
    pub src: PathBuf,
    pub kind: Kind,
    pub activation: Activation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    /// By-reference profile: the payload lives outside the ricepilot data dir
    /// (e.g. the live caelestia clone). ricepilot never writes into it.
    #[serde(default)]
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub hypr_dialect: Option<String>,
    #[serde(default, rename = "path")]
    pub paths: Vec<PathEntry>,
    /// Globs excluded from hashing. Seeded from the profile's `.gitignore`
    /// union the discovered runtime writers.
    #[serde(default)]
    pub volatile: Vec<String>,
    /// Paths written by a theme engine or installer. Backed up to the attic on
    /// switch; never linked, never treated as profile content.
    #[serde(default)]
    pub generated: Vec<PathBuf>,
}

fn invalid(profile: &str, detail: impl Into<String>) -> Error {
    Error::Manifest {
        profile: profile.to_string(),
        detail: detail.into(),
    }
}

/// Expand a leading `~` against `home`. Only a leading `~/` (or a bare `~`)
/// is special; a `~` anywhere else is an ordinary character in a file name.
pub fn expand_home(path: &Path, home: &Path) -> PathBuf {
    let s = path.as_os_str().as_encoded_bytes();
    if s == b"~" {
        return home.to_path_buf();
    }
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// Reject a path that escapes its root, is absolute, or contains `.`/`..`.
/// A `src` of `../../..` would make a profile able to point a live link at
/// anything on the machine, which is precisely what the ownership predicate
/// exists to prevent.
fn check_relative_src(profile: &str, src: &Path) -> Result<()> {
    if src.as_os_str().is_empty() {
        return Err(invalid(profile, "`src` must not be empty"));
    }
    for c in src.components() {
        match c {
            Component::Normal(_) => {}
            _ => {
                return Err(invalid(
                    profile,
                    format!(
                        "`src = {}` must be relative to the profile root and free of `.` and `..`",
                        src.display()
                    ),
                ))
            }
        }
    }
    Ok(())
}

/// Parse and validate a `profile.toml`.
///
/// Validation is deliberately strict — an unknown key is an error rather than
/// a silent ignore, because a typo in `activation` or `kind` is the kind of
/// mistake whose consequence is a live symlink pointing somewhere unintended.
pub fn parse(text: &str) -> Result<Manifest> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| Error::Manifest {
        profile: "<unparsed>".to_string(),
        detail: e.message().to_string(),
    })?;
    validate(&manifest)?;
    Ok(manifest)
}

fn validate(m: &Manifest) -> Result<()> {
    if m.name.is_empty() {
        return Err(invalid("<unnamed>", "`name` must not be empty"));
    }
    if m.name.contains('/') || m.name == "." || m.name == ".." {
        return Err(invalid(
            &m.name,
            "`name` is used as a directory name and must not contain `/` or be `.`/`..`",
        ));
    }
    if let Some(root) = &m.root {
        if !root.starts_with("/") && !root.starts_with("~") {
            return Err(invalid(
                &m.name,
                format!(
                    "`root = {}` must be absolute or start with `~`",
                    root.display()
                ),
            ));
        }
    }
    if let Some(d) = &m.hypr_dialect {
        if d != "conf" && d != "lua" {
            return Err(invalid(
                &m.name,
                format!("`hypr_dialect = {d:?}` must be \"conf\" or \"lua\""),
            ));
        }
    }

    let mut seen: Vec<&Path> = Vec::new();
    for p in &m.paths {
        if !p.dest.starts_with("/") && !p.dest.starts_with("~") {
            return Err(invalid(
                &m.name,
                format!(
                    "`dest = {}` must be absolute or start with `~`",
                    p.dest.display()
                ),
            ));
        }
        // `observe` refuses a `..` when it walks the path, but catching it
        // here means a bad manifest is rejected when it is read rather than
        // when it is acted on, and the message names the manifest.
        if p.dest
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(invalid(
                &m.name,
                format!(
                    "`dest = {}` must be free of `.` and `..`: a destination that is not \
                     literally where it appears to be cannot be reasoned about safely",
                    p.dest.display()
                ),
            ));
        }
        if seen.contains(&p.dest.as_path()) {
            return Err(invalid(
                &m.name,
                format!("`dest = {}` is declared twice", p.dest.display()),
            ));
        }
        seen.push(p.dest.as_path());
        check_relative_src(&m.name, &p.src)?;

        // v1 activates directory links only. The other kinds are accepted in
        // the schema so a manifest can classify a path now and have it
        // activated by a later version, but asking for one today is an
        // explicit "not possible", not a silent no-op.
        match p.kind {
            Kind::DirLink => {}
            Kind::FileCopy => {
                return Err(Error::NotPossible {
                    anchor: "file-activation",
                    why: format!(
                        "`{}` declares kind = file-copy; v1 activates dir-link only (v1.1)",
                        p.dest.display()
                    ),
                })
            }
            Kind::Generated | Kind::Volatile => {
                if p.activation != Activation::Never {
                    return Err(invalid(
                        &m.name,
                        format!(
                            "`{}` is kind = {} and must set activation = \"never\": \
                             generated and volatile paths are classifications, never activated",
                            p.dest.display(),
                            p.kind.as_str()
                        ),
                    ));
                }
            }
        }

        if p.kind == Kind::DirLink && p.activation == Activation::Live {
            return Err(Error::NotPossible {
                anchor: "live-apply",
                why: format!(
                    "`{}` declares activation = live; v1 switches at next login only",
                    p.dest.display()
                ),
            });
        }
    }
    Ok(())
}

impl Manifest {
    /// Where this profile's payload lives: its declared `root` for a
    /// by-reference profile, otherwise the profile's own directory.
    pub fn root_dir(&self, profile_dir: &Path, home: &Path) -> PathBuf {
        match &self.root {
            Some(r) => expand_home(r, home),
            None => profile_dir.to_path_buf(),
        }
    }

    /// Project the manifest into the planner's target state: one absolute
    /// destination and the absolute path its link must point at.
    ///
    /// Only `dir-link` entries with `activation = relogin` produce a target.
    /// `generated` and `volatile` entries are classifications and are absent
    /// from the plan by construction, not by a runtime check.
    pub fn targets(&self, profile_dir: &Path, home: &Path) -> Vec<crate::plan::Target> {
        let root = self.root_dir(profile_dir, home);
        self.paths
            .iter()
            .filter(|p| p.kind == Kind::DirLink && p.activation == Activation::Relogin)
            .map(|p| crate::plan::Target {
                dest: expand_home(&p.dest, home),
                src: root.join(&p.src),
            })
            .collect()
    }
}
