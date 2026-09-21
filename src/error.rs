//! The single error type. Every refusal names the offending path and the rule
//! it violates (`SAFETY.md` R4) and maps to a stable exit code.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

/// Stable process exit codes. Scripts and the acceptance run depend on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    Ok = 0,
    /// A pre-flight check declined the operation. Nothing was touched.
    Refused = 2,
    /// The requested capability is not offered. See `docs/NOT-POSSIBLE.md`.
    NotPossible = 3,
    /// An unexpected IO or consistency failure.
    Failed = 4,
    /// Another ricepilot process holds the lock.
    Locked = 5,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A pre-flight refusal: `rule` is the `SAFETY.md` identifier, `path` the
    /// concrete path that triggered it.
    #[error("refusing: {why} ({path}) [{rule}]")]
    Refused {
        rule: &'static str,
        path: PathBuf,
        why: String,
    },

    #[error("not possible: {why} [see docs/NOT-POSSIBLE.md#{anchor}]")]
    NotPossible { anchor: &'static str, why: String },

    /// A `profile.toml` is malformed or declares something v1 will not do.
    /// Nothing has been touched, so this is a refusal, not a failure.
    #[error("profile `{profile}`: {detail}")]
    Manifest { profile: String, detail: String },

    #[error("another ricepilot holds {path}")]
    Locked { path: PathBuf },

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Error::Refused { .. } | Error::Manifest { .. } => ExitCode::Refused,
            Error::NotPossible { .. } => ExitCode::NotPossible,
            Error::Locked { .. } => ExitCode::Locked,
            Error::Io { .. } => ExitCode::Failed,
        }
    }
}
