//! The planner. **Pure**: `(Observed, Target) -> Plan`, zero IO, no syscalls,
//! no clock, no randomness. This is what makes dry-run trustworthy — the plan
//! printed by `plan` is byte-identical to the one `switch --commit` executes.
//!
//! Implemented in M1.

use std::path::PathBuf;

use crate::observe::Observed;

/// A single mutation. Executed only by [`crate::ops`]. The set is closed and
/// deliberately small: there is no delete, no write-through, no copy-into-tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Create `link_path.tmp-<n>` pointing at `target`, in the same directory.
    CreateTempLink {
        link_path: PathBuf,
        target: PathBuf,
    },
    /// `renameat2(RENAME_EXCHANGE)` the staged temp link with the live dest.
    /// Falls back to rename-to-attic-then-rename where the kernel or
    /// filesystem lacks `RENAME_EXCHANGE`.
    Exchange {
        dest: PathBuf,
        staged: PathBuf,
    },
    /// Rename a displaced object into `state/attic/<ts>/`. Never a delete.
    RenameToAttic {
        from: PathBuf,
        attic_rel: PathBuf,
    },
    FsyncDir {
        dir: PathBuf,
    },
}

/// Why the plan declines. Each variant renders a message that names the path
/// and the `SAFETY.md` rule, and is snapshot-tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Unowned { dest: PathBuf },
    CrossDevice { dest: PathBuf, from: u64, to: u64 },
    Denylisted { dest: PathBuf, entry: &'static str },
    Mountpoint { dest: PathBuf },
    NestedDest { outer: PathBuf, inner: PathBuf },
    MissingRequires { packages: Vec<String> },
    VerifyConfigFailed { file: PathBuf, detail: String },
}

/// The complete outcome of planning. Either every destination can be switched
/// safely, or the whole switch is declined — never a partial plan (R5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing to do: reality already matches the target.
    NoOp,
    Apply {
        ops: Vec<Op>,
    },
    Decline {
        refusals: Vec<Refusal>,
    },
}

/// The target state, projected from a profile manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub dest: PathBuf,
    pub src: PathBuf,
}

/// Pure. `plan(s, s)` is always [`Plan::NoOp`] (proptest gate, M1).
pub fn plan(_observed: &[Observed], _target: &[Target]) -> Plan {
    todo!("M1")
}
