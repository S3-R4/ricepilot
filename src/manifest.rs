//! `profile.toml` parsing. Schema is documented in `docs/DESIGN.md`.
//!
//! Implemented in M1.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activation {
    Relogin,
    Live,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathEntry {
    pub dest: PathBuf,
    pub src: PathBuf,
    pub kind: Kind,
    pub activation: Activation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

pub fn parse(_toml: &str) -> crate::Result<Manifest> {
    todo!("M1")
}
