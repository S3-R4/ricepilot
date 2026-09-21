//! The planner. **Pure**: `(Observed, Target) -> Plan`, zero IO, no syscalls,
//! no clock, no randomness. This is what makes dry-run trustworthy — the plan
//! printed by `plan` is byte-identical to the one `switch --commit` executes.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::observe::{Observed, Shape};

/// A single mutation. Executed only by [`crate::ops`]. The set is closed and
/// deliberately small: there is no delete, no write-through, no copy-into-tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Create `link_path` pointing at `target`, in the destination's own
    /// directory, so the later exchange is a same-directory rename.
    CreateTempLink {
        link_path: PathBuf,
        target: PathBuf,
    },
    /// Create the link directly at a destination that does not exist. There
    /// is nothing to displace, and `symlinkat` is already atomic, so this
    /// needs no exchange (shape 5).
    CreateLink {
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

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Op::CreateTempLink { link_path, target } => write!(
                f,
                "stage link   {} -> {}",
                link_path.display(),
                target.display()
            ),
            Op::CreateLink { link_path, target } => write!(
                f,
                "create link  {} -> {}",
                link_path.display(),
                target.display()
            ),
            Op::Exchange { dest, staged } => write!(
                f,
                "exchange     {} <-> {}",
                dest.display(),
                staged.display()
            ),
            Op::RenameToAttic { from, attic_rel } => write!(
                f,
                "to attic     {} -> <attic>/{}",
                from.display(),
                attic_rel.display()
            ),
            Op::FsyncDir { dir } => write!(f, "fsync dir    {}", dir.display()),
        }
    }
}

/// Why the plan declines. Each variant renders a message that names the path
/// and the `SAFETY.md` rule, and is snapshot-tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Shape 2: a symlink ricepilot did not create, or whose ledger entry
    /// disagrees. It cannot know what depends on it.
    Unowned {
        dest: PathBuf,
        shape: &'static str,
    },
    /// Shape 3: a real directory where an owned link should be — typically
    /// because a rice installer converted the link back.
    RealDirAtDest {
        dest: PathBuf,
    },
    /// Shape 4: v1 activates directories only.
    RealFileAtDest {
        dest: PathBuf,
    },
    CrossDevice {
        dest: PathBuf,
        from: u64,
        to: u64,
    },
    Denylisted {
        dest: PathBuf,
        entry: &'static str,
    },
    Mountpoint {
        dest: PathBuf,
    },
    NestedDest {
        outer: PathBuf,
        inner: PathBuf,
    },
    MissingRequires {
        packages: Vec<String>,
    },
    VerifyConfigFailed {
        file: PathBuf,
        detail: String,
    },
    /// A declared destination that phase A did not observe. A bug rather than
    /// a user error, but the decision table is total and so is this enum:
    /// planning on an incomplete observation is never allowed to proceed.
    NotObserved {
        dest: PathBuf,
    },
}

impl Refusal {
    /// The `SAFETY.md` rule each refusal enforces.
    pub fn rule(&self) -> &'static str {
        match self {
            Refusal::Unowned { .. }
            | Refusal::RealDirAtDest { .. }
            | Refusal::Denylisted { .. }
            | Refusal::NotObserved { .. } => "R4",
            Refusal::RealFileAtDest { .. } | Refusal::VerifyConfigFailed { .. } => "R5",
            Refusal::CrossDevice { .. }
            | Refusal::Mountpoint { .. }
            | Refusal::NestedDest { .. }
            | Refusal::MissingRequires { .. } => "R4",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unowned { dest, shape } => write!(
                f,
                "{}: is a {}, which ricepilot did not create. It cannot know what depends on it, \
                 so it will not replace it. Register it with `ricepilot adopt` or move it aside \
                 yourself.",
                dest.display(),
                shape
            ),
            Refusal::RealDirAtDest { dest } => write!(
                f,
                "{}: is a real directory, not the link ricepilot owns. Something (typically a \
                 rice installer) replaced the link. Re-linking would discard whatever it wrote; \
                 run `ricepilot doctor` and decide deliberately.",
                dest.display()
            ),
            Refusal::RealFileAtDest { dest } => write!(
                f,
                "{}: is a regular file. v1 activates directory links only; file deployment is \
                 v1.1.",
                dest.display()
            ),
            Refusal::CrossDevice { dest, from, to } => write!(
                f,
                "{}: sits on device {from} but the attic is on device {to}. `rename(2)` across \
                 filesystems fails with EXDEV, so nothing could be displaced safely.",
                dest.display()
            ),
            Refusal::Denylisted { dest, entry } => write!(
                f,
                "{}: is inside `{entry}`, which v1 will not manage under any circumstances.",
                dest.display()
            ),
            Refusal::Mountpoint { dest } => write!(
                f,
                "{}: is a mount point. Renaming it would cross a filesystem boundary.",
                dest.display()
            ),
            Refusal::NestedDest { outer, inner } => write!(
                f,
                "{}: is nested inside the managed destination {}. Switching both would make the \
                 inner one's meaning depend on the order of two renames.",
                inner.display(),
                outer.display()
            ),
            Refusal::MissingRequires { packages } => write!(
                f,
                "this profile requires packages that are not installed: {}. ricepilot never \
                 installs anything; run: paru -S --needed {}",
                packages.join(", "),
                packages.join(" ")
            ),
            Refusal::VerifyConfigFailed { file, detail } => write!(
                f,
                "{}: did not parse in a sandboxed verify-config run: {detail}",
                file.display()
            ),
            Refusal::NotObserved { dest } => write!(
                f,
                "{}: was declared but not observed. ricepilot will not plan against an incomplete \
                 picture of the filesystem.",
                dest.display()
            ),
        }
    }
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

/// The facts a plan needs that are not per-destination. All of them are
/// *data*: gathering them is IO, using them is not, which is what keeps
/// [`plan`] pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanContext {
    /// Used only to expand the `~`-relative denylist.
    pub home: PathBuf,
    /// `state/attic/<ts>/`, the directory displaced objects are renamed into.
    pub attic: PathBuf,
    /// `st_dev` of the attic. Compared against each destination's parent so
    /// an `EXDEV` failure is a pre-flight refusal, not a runtime surprise.
    pub attic_dev: u64,
    /// Packages `pacman -Q` could not find (M5 fills this; M1 leaves it
    /// empty).
    pub missing_requires: Vec<String>,
    /// Destinations ricepilot owns that the target state does **not** include,
    /// and which are therefore displaced into the attic by this switch.
    ///
    /// This is how a switch stops owning a path without anything being
    /// removed (D36). It is also what makes `rollback` honest: a link the
    /// forward switch created at a destination the previous generation did not
    /// have has no exact inverse — making it absent again is a removal, and
    /// there is none outside `src/gc/` — so the nearest true thing is to
    /// displace it, and to say so.
    pub retire: Vec<PathBuf>,
}

impl PlanContext {
    /// A context for a home directory, with the attic on the same device —
    /// the normal case, since both live under `/home`.
    pub fn new(home: impl Into<PathBuf>, attic: impl Into<PathBuf>, attic_dev: u64) -> Self {
        Self {
            home: home.into(),
            attic: attic.into(),
            attic_dev,
            missing_requires: Vec::new(),
            retire: Vec::new(),
        }
    }

    /// The destinations this switch stops owning.
    pub fn retiring(mut self, retire: Vec<PathBuf>) -> Self {
        self.retire = retire;
        self
    }
}

/// `docs/DESIGN.md` §9. Hard for v1: a destination at or under one of these
/// is refused even if a manifest names it explicitly. Entries are `~`-relative
/// and expanded against [`PlanContext::home`].
pub const DENYLIST: &[&str] = &[
    "~/.config/uwsm",
    "~/.config/systemd",
    "~/.config/environment.d",
    "~/.config/autostart",
    "~/.config/dconf",
    "~/.config/pulse",
    "~/.config/mimeapps.list",
    "~/.config/user-dirs.dirs",
    "~/.config/user-dirs.locale",
    "~/.ssh",
    "~/.gnupg",
    "~/.local/share/keyrings",
    "~/.config/google-chrome",
    "~/.config/chromium",
    "~/.config/BraveSoftware",
    "~/.config/microsoft-edge",
    "~/.mozilla",
    "~/.config/Electron",
    "~/.config/discord",
    "~/.config/Code",
    "~/.config/VSCodium",
];

fn denylist_hit(dest: &Path, home: &Path) -> Option<&'static str> {
    DENYLIST.iter().copied().find(|entry| {
        let expanded = crate::manifest::expand_home(Path::new(entry), home);
        dest == expanded || dest.starts_with(&expanded)
    })
}

/// The temp name a staged link takes: a sibling of the destination, so the
/// exchange is same-directory and therefore same-`st_dev`.
fn temp_name(dest: &Path, n: usize) -> PathBuf {
    let file = dest
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    match dest.parent() {
        Some(p) => p.join(format!("{file}.rp-tmp-{n}")),
        None => PathBuf::from(format!("{file}.rp-tmp-{n}")),
    }
}

/// Where a displaced object lands inside the attic: its absolute path with
/// the leading `/` stripped, so two destinations can never collide and the
/// attic reads as a map of where everything came from.
fn attic_rel(dest: &Path) -> PathBuf {
    dest.strip_prefix("/").unwrap_or(dest).to_path_buf()
}

/// Pure. `plan(s, s)` is always [`Plan::NoOp`].
///
/// Refusals are collected, not short-circuited: a user who has three problems
/// should be told about three problems, not made to fix them one run at a
/// time. If any refusal is produced the result is [`Plan::Decline`] and no op
/// is returned at all — there is no partial plan (`SAFETY.md` R5).
pub fn plan(observed: &[Observed], target: &[Target], ctx: &PlanContext) -> Plan {
    let mut refusals: Vec<Refusal> = Vec::new();

    if !ctx.missing_requires.is_empty() {
        refusals.push(Refusal::MissingRequires {
            packages: ctx.missing_requires.clone(),
        });
    }

    // Two managed destinations where one contains the other: the inner one's
    // meaning after the switch would depend on which rename happened first.
    for outer in target {
        for inner in target {
            if outer.dest != inner.dest && inner.dest.starts_with(&outer.dest) {
                refusals.push(Refusal::NestedDest {
                    outer: outer.dest.clone(),
                    inner: inner.dest.clone(),
                });
            }
        }
    }

    // Ops are collected per phase, not per destination, because that is the
    // order phase A/B/C execute them in (`docs/DESIGN.md` §6) and the printed
    // plan should be the executed plan.
    let mut staging: Vec<Op> = Vec::new();
    let mut exchanges: Vec<Op> = Vec::new();
    let mut creates: Vec<Op> = Vec::new();
    let mut attic: Vec<Op> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();

    for (n, t) in target.iter().enumerate() {
        let Some(obs) = observed.iter().find(|o| o.dest == t.dest) else {
            refusals.push(Refusal::NotObserved {
                dest: t.dest.clone(),
            });
            continue;
        };

        if let Some(entry) = denylist_hit(&t.dest, &ctx.home) {
            refusals.push(Refusal::Denylisted {
                dest: t.dest.clone(),
                entry,
            });
            continue;
        }

        if obs.is_mountpoint {
            refusals.push(Refusal::Mountpoint {
                dest: t.dest.clone(),
            });
            continue;
        }

        match &obs.shape {
            // Row 1: already ours. Same target — nothing to do at all.
            Shape::OwnedLink { target: current } if *current == t.src => {}

            // Row 1: ours, pointing at the previous profile. Stage, exchange,
            // and the displaced old link goes to the attic — never deleted.
            Shape::OwnedLink { .. } => {
                if obs.parent_dev != ctx.attic_dev {
                    refusals.push(Refusal::CrossDevice {
                        dest: t.dest.clone(),
                        from: obs.parent_dev,
                        to: ctx.attic_dev,
                    });
                    continue;
                }
                let staged = temp_name(&t.dest, n);
                staging.push(Op::CreateTempLink {
                    link_path: staged.clone(),
                    target: t.src.clone(),
                });
                exchanges.push(Op::Exchange {
                    dest: t.dest.clone(),
                    staged: staged.clone(),
                });
                // After the exchange the *staged* name holds the old link.
                attic.push(Op::RenameToAttic {
                    from: staged,
                    attic_rel: attic_rel(&t.dest),
                });
                if let Some(p) = t.dest.parent() {
                    if !dirs.contains(&p.to_path_buf()) {
                        dirs.push(p.to_path_buf());
                    }
                }
            }

            // Row 2.
            Shape::ForeignLink { .. } => refusals.push(Refusal::Unowned {
                dest: t.dest.clone(),
                shape: obs.shape.as_str(),
            }),

            // Row 3.
            Shape::RealDir => refusals.push(Refusal::RealDirAtDest {
                dest: t.dest.clone(),
            }),

            // Row 4.
            Shape::RealFile => refusals.push(Refusal::RealFileAtDest {
                dest: t.dest.clone(),
            }),

            // Row 5: nothing there, nothing to displace.
            Shape::Absent => {
                creates.push(Op::CreateLink {
                    link_path: t.dest.clone(),
                    target: t.src.clone(),
                });
                if let Some(p) = t.dest.parent() {
                    if !dirs.contains(&p.to_path_buf()) {
                        dirs.push(p.to_path_buf());
                    }
                }
            }
        }
    }

    // Destinations this switch stops owning. They are displaced into the
    // attic, never removed (R2), and they are planned *after* the exchanges
    // because that is the order phase C executes them in.
    for dest in &ctx.retire {
        // A path in both lists is being switched, not retired; the target
        // state includes it, so there is nothing to stop owning.
        if target.iter().any(|t| &t.dest == dest) {
            continue;
        }
        let Some(obs) = observed.iter().find(|o| &o.dest == dest) else {
            refusals.push(Refusal::NotObserved { dest: dest.clone() });
            continue;
        };
        match &obs.shape {
            // Ours, and no longer wanted: into the attic it goes.
            Shape::OwnedLink { .. } => {
                if obs.parent_dev != ctx.attic_dev {
                    refusals.push(Refusal::CrossDevice {
                        dest: dest.clone(),
                        from: obs.parent_dev,
                        to: ctx.attic_dev,
                    });
                    continue;
                }
                attic.push(Op::RenameToAttic {
                    from: dest.clone(),
                    attic_rel: attic_rel(dest),
                });
                if let Some(p) = dest.parent() {
                    if !dirs.contains(&p.to_path_buf()) {
                        dirs.push(p.to_path_buf());
                    }
                }
            }
            // Already gone. Nothing to displace, and nothing to say about it.
            Shape::Absent => {}
            // Something else is at a path ricepilot believed it owned. It is
            // refused for exactly the reason a switch onto one is: ricepilot
            // did not put it there and cannot know what depends on it.
            _ => refusals.push(Refusal::Unowned {
                dest: dest.clone(),
                shape: obs.shape.as_str(),
            }),
        }
    }

    if !refusals.is_empty() {
        return Plan::Decline { refusals };
    }

    let mut ops = staging;
    ops.append(&mut exchanges);
    ops.append(&mut creates);
    let touched_attic = !attic.is_empty();
    ops.append(&mut attic);
    for dir in dirs {
        ops.push(Op::FsyncDir { dir });
    }
    if touched_attic {
        ops.push(Op::FsyncDir {
            dir: ctx.attic.clone(),
        });
    }

    if ops.is_empty() {
        Plan::NoOp
    } else {
        Plan::Apply { ops }
    }
}
