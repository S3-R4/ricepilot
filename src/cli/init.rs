//! `ricepilot init --root <dir>`
//!
//! Register a live rice **by reference** — the profile's root is the user's
//! own clone, which ricepilot reads and never writes to — adopt the
//! directory links that already point into it, after per-path confirmation,
//! and take a baseline copy of the tree.
//!
//! The thing worth understanding about `init` is how little it does to the
//! live machine: **nothing**. The links it "adopts" already exist and
//! already point where they point; adopting one means writing a ledger row
//! that says ricepilot now considers it owned, which is what lets a later
//! `switch` act on it instead of refusing it as unowned. No link is created,
//! moved or retargeted, and the rice clone is never written to (R3). That is
//! why, like `capture`, it has no journal: there is no live mutation for
//! `recover` to resolve (D45).
//!
//! What it *does* do is look, and report three things a person should decide
//! about before any of this becomes load-bearing: absolute symlinks inside
//! the tree, files only their owner can read, and links into the rice at
//! destinations v1 will not manage under any circumstances.
//!
//! There is no discovery of the root. `~/.config` has 128 entries on the
//! target machine and about twenty are rice; a path is managed because it
//! was named, never because it was found (DESIGN §9), and the same rule
//! applies to the tree they point into.

use std::path::PathBuf;

use crate::error::ExitCode;
use crate::ops::{lock, mutate, read};
use crate::{ledger, plan, survey, Error, Result};

use super::paths::Paths;
use super::{confirm, render, Output};

/// One live link into the rice, and what ricepilot can do about it.
pub struct Candidate {
    pub link: survey::LiveLink,
    /// Why it cannot be adopted, if it cannot. Reported either way: a link
    /// into the rice at a denylisted destination is a fact about the rice
    /// the user should hear, not one to leave out of the report because it
    /// is inactionable.
    pub blocked: Option<String>,
}

impl Candidate {
    pub fn adoptable(&self) -> bool {
        self.blocked.is_none()
    }
}

/// Everything phase A worked out. Read-only.
pub struct Init {
    pub name: String,
    pub root: PathBuf,
    /// Where the profile will be registered.
    pub dir: PathBuf,
    /// Where the baseline copy will go.
    pub baseline: PathBuf,
    pub survey: survey::Survey,
    /// Which directory ricepilot looked in for links into the rice.
    pub config_dir: PathBuf,
    pub candidates: Vec<Candidate>,
}

impl Init {
    /// The globs `init` proposes as `volatile`: one per owner-only file.
    pub fn proposed_volatile(&self) -> Vec<String> {
        self.survey.private.iter().map(|p| p.rel.clone()).collect()
    }
}

pub fn plan_it(paths: &Paths, root: Option<&std::path::Path>, name: Option<&str>) -> Result<Init> {
    let Some(root) = root else {
        return Err(Error::Refused {
            rule: "R6",
            path: paths.home.join(".local/share"),
            why: "name the rice to register, with `--root <dir>` — for a caelestia install that \
                  is `~/.local/share/caelestia`. ricepilot does not go looking: a path is \
                  managed because you named it, and that applies to the tree as much as to the \
                  destinations"
                .into(),
        });
    };
    let root = crate::manifest::expand_home(root, &paths.home);
    if !root.is_absolute() {
        return Err(Error::Refused {
            rule: "R1",
            path: root,
            why: "must be an absolute path or start with `~`".into(),
        });
    }
    let meta = read::lstat_or_absent(&root)?.ok_or_else(|| Error::Refused {
        rule: "R4",
        path: root.clone(),
        why: "there is nothing here to register".into(),
    })?;
    if meta.kind != read::Kind::Dir {
        return Err(Error::Refused {
            rule: "R4",
            path: root.clone(),
            why: "is not a directory. a profile's root is the tree its destinations point into"
                .into(),
        });
    }

    let name = match name {
        Some(n) => n.to_string(),
        None => root
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Refused {
                rule: "R4",
                path: root.clone(),
                why: "has no final path component to name the profile after; pass `--name`".into(),
            })?,
    };
    if name.contains('/') || name == "." || name == ".." {
        return Err(Error::Manifest {
            profile: name,
            detail: "profile name must be a single directory name".into(),
        });
    }
    let dir = paths.profile_dir(&name);
    if read::lstat_or_absent(&dir)?.is_some() {
        return Err(Error::Refused {
            rule: "R2",
            path: dir,
            why: format!(
                "profile `{name}` is already registered. ricepilot never writes over a profile; \
                 `ricepilot show {name}` says what it holds"
            ),
        });
    }
    let baseline = paths.state.join("baseline").join(&name);
    if read::lstat_or_absent(&baseline)?.is_some() {
        return Err(Error::Refused {
            rule: "R2",
            path: baseline,
            why: "a baseline copy is already here. ricepilot removes nothing, so it will not \
                  write over one; move it aside if you want a fresh one"
                .into(),
        });
    }

    let config_dir = paths.home.join(".config");
    let links = match read::lstat_or_absent(&config_dir)? {
        Some(m) if m.kind == read::Kind::Dir => survey::links_into(&config_dir, &root)?,
        // No config directory at all is a strange machine, not an error:
        // there is simply nothing pointing into the rice yet.
        _ => Vec::new(),
    };

    let candidates = links
        .into_iter()
        .map(|link| Candidate {
            blocked: blocked_because(&link, paths),
            link,
        })
        .collect();

    Ok(Init {
        name,
        survey: survey::survey(&root)?,
        root,
        dir,
        baseline,
        config_dir,
        candidates,
    })
}

/// Why a link into the rice cannot be adopted. `None` means it can.
fn blocked_because(link: &survey::LiveLink, paths: &Paths) -> Option<String> {
    for entry in plan::DENYLIST {
        let expanded = crate::manifest::expand_home(std::path::Path::new(entry), &paths.home);
        if link.dest == expanded || link.dest.starts_with(&expanded) {
            return Some(format!(
                "inside `{entry}`, which v1 will not manage under any circumstances"
            ));
        }
    }
    if !link.points_at_dir {
        return Some(
            "does not point at a directory right now. v1 activates directory links only, and a \
             link into the rice that resolves to nothing is worth looking at before it is \
             registered as healthy"
                .into(),
        );
    }
    None
}

/// `ricepilot init`. Dry-run unless `commit`.
pub fn run(
    paths: &Paths,
    root: Option<&std::path::Path>,
    name: Option<&str>,
    commit: bool,
) -> Result<Output> {
    let _lock = lock::acquire(&paths.lock_path()?)?;

    let i = plan_it(paths, root, name)?;
    let header = render::init_header(&i, commit);

    if !commit {
        return Ok(Output {
            text: header + render::INIT_UNCOMMITTED,
            code: ExitCode::Ok,
        });
    }

    // ---- R6: one question per path, and one for the volatile proposal. ----
    print!("{header}");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut adopt: Vec<&Candidate> = Vec::new();
    for c in i.candidates.iter().filter(|c| c.adoptable()) {
        if confirm::ask(&render::init_question(c)) {
            adopt.push(c);
        }
    }
    let proposed = i.proposed_volatile();
    let volatile = if proposed.is_empty() {
        Vec::new()
    } else if confirm::ask(&render::init_volatile_question(&proposed)) {
        proposed
    } else {
        Vec::new()
    };

    // ---- The baseline. A copy, never a move (`AGENT_PROMPT.md` §3). ----
    let stats = mutate::copy_tree(&i.root, &i.baseline)?;

    // ---- The profile, assembled at a staging name and renamed in (D45).
    let manifest = render_manifest(&i, &adopt, &volatile, &paths.home);
    // Parsed back before it is written: a manifest ricepilot cannot read is
    // never one it creates.
    crate::manifest::parse(&manifest)?;

    let id = crate::journal::unique_id(
        &paths.state,
        &crate::journal::timestamp_id(std::time::SystemTime::now()),
    )?;
    let staging = paths.data.join("staging").join(format!("{}-{id}", i.name));
    mutate::make_dirs(&staging)?;
    mutate::write_atomic(&staging.join("profile.toml"), manifest.as_bytes())?;
    mutate::fsync_dir(&staging)?;
    mutate::make_dirs(&paths.profiles_dir())?;
    mutate::rename_within(&staging, &i.dir)?;
    mutate::fsync_dir(&paths.profiles_dir())?;

    // ---- The ledger. This is the whole of what "adopting a link" means
    // here: the link already exists and already points where it points, and
    // the row is what makes a later `switch` willing to act on it instead of
    // refusing it as unowned.
    let dests: Vec<PathBuf> = adopt.iter().map(|c| c.link.dest.clone()).collect();
    let mut led = ledger::load(&paths.ledger_path())?;
    led.record(&dests, &i.name)?;
    ledger::save(&paths.ledger_path(), &led)?;

    Ok(Output {
        text: render::init_done(&i, &dests, &volatile, &stats),
        code: ExitCode::Ok,
    })
}

fn render_manifest(
    i: &Init,
    adopt: &[&Candidate],
    volatile: &[String],
    home: &std::path::Path,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "# written by `ricepilot init`.");
    let _ = writeln!(
        s,
        "# by-reference: `root` is your own tree. ricepilot reads it and never writes to it,"
    );
    let _ = writeln!(
        s,
        "# and it never rewrites this file either — it is yours to edit."
    );
    let _ = writeln!(s, "name = \"{}\"", i.name);
    let _ = writeln!(s, "root = \"{}\"", super::capture::tildify(&i.root, home));
    let _ = writeln!(s, "generated = []");
    let _ = writeln!(s, "volatile = [");
    for v in volatile {
        let _ = writeln!(s, "  \"{v}\",");
    }
    let _ = writeln!(s, "]");
    for c in adopt {
        // `src` is the target's path relative to the root, which is what the
        // link already points at — read off the link rather than guessed
        // from the destination's name.
        let src = c
            .link
            .target
            .strip_prefix(&i.root)
            .unwrap_or(&c.link.target)
            .display()
            .to_string();
        let _ = writeln!(s);
        let _ = writeln!(s, "[[path]]");
        let _ = writeln!(
            s,
            "dest       = \"{}\"",
            super::capture::tildify(&c.link.dest, home)
        );
        let _ = writeln!(s, "src        = \"{src}\"");
        let _ = writeln!(s, "kind       = \"dir-link\"");
        let _ = writeln!(s, "activation = \"relogin\"");
    }
    s
}
