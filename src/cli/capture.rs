//! `ricepilot capture <profile> --from <dir>…`
//!
//! Copy one or more live directories into a new profile and write its
//! manifest. It does **not** activate anything: nothing in `~/.config`
//! changes, no link is created, no ledger row is written. `capture` is how a
//! rice you are running becomes a profile you can switch *back* to.
//!
//! Three properties are worth stating, because each is a decision:
//!
//! * **The profile is built at a staging name and renamed into place last.**
//!   A crash half way through would otherwise leave a directory under
//!   `profiles/` with no manifest in it, and `list` reports a profile
//!   directory without a readable `profile.toml` as an error rather than
//!   skipping it — so an interrupted capture would break every later `list`.
//!   The staging directory lives outside `profiles/`, and the final step is
//!   one rename ([DECISIONS.md](../../docs/DECISIONS.md) D45).
//! * **There is no journal.** A journal exists so that a mutation to the live
//!   machine which was interrupted can be finished or abandoned by observing
//!   reality. `capture` makes no mutation to the live machine at all: it
//!   reads live directories and writes inside ricepilot's own data directory,
//!   and its single externally visible step is an atomic rename (D45).
//! * **The copy is hash-verified before the profile exists.** `verify::build`
//!   over the source and over the copy must compare equal — not "no error was
//!   returned", but "these two trees have the same blake3 manifest".

use std::path::{Path, PathBuf};

use crate::error::ExitCode;
use crate::ops::{lock, mutate, read};
use crate::{survey, verify, Error, Result};

use super::paths::Paths;
use super::{render, Output};

/// One live directory to capture, and where it lands in the profile.
pub struct Source {
    /// The live directory, absolute.
    pub live: PathBuf,
    /// Its name inside the profile, which is also the manifest's `src`.
    pub leaf: String,
    pub survey: survey::Survey,
}

/// What `capture` decided to do, worked out without changing anything.
pub struct Capture {
    pub profile: String,
    pub sources: Vec<Source>,
    /// Where the profile will be, once it is one.
    pub dir: PathBuf,
    /// The `profile.toml` that will be written, exactly as it will be
    /// written. Parsed back before it is printed, so a manifest ricepilot
    /// could not read is never one it offers to write.
    pub manifest: String,
}

/// Work out the whole capture. Read-only (`SAFETY.md` R4).
pub fn plan(paths: &Paths, name: &str, from: &[PathBuf]) -> Result<Capture> {
    if from.is_empty() {
        return Err(Error::Refused {
            rule: "R4",
            path: paths.profile_dir(name),
            why: "name at least one directory to capture, with `--from <dir>`. ricepilot does \
                  not guess which of the 128 entries in a config directory are rice — a path \
                  is managed because you named it"
                .into(),
        });
    }
    // The same check `paths::load` makes, made before anything reads a disk:
    // a profile name is a directory name.
    if name.contains('/') || name == "." || name == ".." {
        return Err(Error::Manifest {
            profile: name.to_string(),
            detail: "profile name must be a single directory name".into(),
        });
    }
    let dir = paths.profile_dir(name);
    if read::lstat_or_absent(&dir)?.is_some() {
        return Err(Error::Refused {
            rule: "R2",
            path: dir,
            why: format!(
                "profile `{name}` already exists. ricepilot never writes over a profile; \
                 capture under another name"
            ),
        });
    }

    let mut sources: Vec<Source> = Vec::new();
    for raw in from {
        let live = crate::manifest::expand_home(raw, &paths.home);
        if !live.is_absolute() {
            return Err(Error::Refused {
                rule: "R1",
                path: live,
                why: "must be an absolute path or start with `~`".into(),
            });
        }
        let meta = read::lstat_or_absent(&live)?.ok_or_else(|| Error::Refused {
            rule: "R4",
            path: live.clone(),
            why: "there is nothing here to capture".into(),
        })?;
        if meta.kind != read::Kind::Dir {
            return Err(Error::Refused {
                rule: "R4",
                path: live.clone(),
                why: match meta.kind {
                    read::Kind::Symlink => "is a symlink, and v1 captures directories only. it \
                                            is not followed here for the same reason nothing \
                                            is followed anywhere else: what it points at is \
                                            somewhere ricepilot has not looked. name that \
                                            directory instead"
                        .to_string(),
                    read::Kind::File => "is a regular file, and v1 captures directories only; \
                                         file deployment is v1.1"
                        .to_string(),
                    _ => "is neither a directory, a file nor a symlink, and v1 captures \
                          directories only"
                        .to_string(),
                },
            });
        }
        let leaf = live
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Refused {
                rule: "R4",
                path: live.clone(),
                why: "has no final path component to name it by inside the profile".into(),
            })?;
        if let Some(other) = sources.iter().find(|s| s.leaf == leaf) {
            return Err(Error::Refused {
                rule: "R4",
                path: live.clone(),
                why: format!(
                    "would be stored in the profile as `{leaf}`, which {} already claims. \
                     capture them separately",
                    other.live.display()
                ),
            });
        }
        let survey = survey::survey(&live)?;
        sources.push(Source { live, leaf, survey });
    }

    let manifest = render_manifest(name, &sources, &paths.home);
    // Parse it back. A manifest ricepilot writes and cannot read would turn
    // every later command on this profile into an error, and the cheapest
    // moment to find that out is before the file exists.
    crate::manifest::parse(&manifest)?;

    Ok(Capture {
        profile: name.to_string(),
        sources,
        dir: paths.profile_dir(name),
        manifest,
    })
}

/// `~/.config/hypr` rather than `/home/u/.config/hypr` where it applies: the
/// manifest is a file a person reads and edits.
pub fn tildify(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn render_manifest(name: &str, sources: &[Source], home: &Path) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "# written by `ricepilot capture {name}`.");
    let _ = writeln!(
        s,
        "# ricepilot reads this file and never rewrites it; it is yours to edit."
    );
    let _ = writeln!(s, "name = \"{name}\"");
    let _ = writeln!(s, "volatile = []");
    let _ = writeln!(s, "generated = []");
    for src in sources {
        let _ = writeln!(s);
        let _ = writeln!(s, "[[path]]");
        let _ = writeln!(s, "dest       = \"{}\"", tildify(&src.live, home));
        let _ = writeln!(s, "src        = \"{}\"", src.leaf);
        let _ = writeln!(s, "kind       = \"dir-link\"");
        let _ = writeln!(s, "activation = \"relogin\"");
    }
    s
}

/// `ricepilot capture`. Dry-run unless `commit`.
pub fn run(paths: &Paths, name: &str, from: &[PathBuf], commit: bool) -> Result<Output> {
    // The lock is taken for the dry run too: a report about which live
    // directories exist, printed while another ricepilot is switching them,
    // would be a report about a machine that has since moved on.
    let _lock = lock::acquire(&paths.lock_path()?)?;

    let c = plan(paths, name, from)?;
    let header = render::capture_header(&c);
    if !commit {
        return Ok(Output {
            text: header + render::CAPTURE_UNCOMMITTED,
            code: ExitCode::Ok,
        });
    }

    // Built here, renamed into `profiles/` at the very end (D45).
    let id = crate::journal::unique_id(
        &paths.state,
        &crate::journal::timestamp_id(std::time::SystemTime::now()),
    )?;
    let staging = paths.data.join("staging").join(format!("{name}-{id}"));
    mutate::make_dirs(&staging)?;

    let mut copied = Vec::new();
    for src in &c.sources {
        let into = staging.join(&src.leaf);
        let stats = mutate::copy_tree(&src.live, &into)?;

        // "The copy is identical" is a checkable claim, so it is checked
        // rather than assumed (R7). A blake3 manifest of the live directory
        // and one of the copy must compare equal, mode, owner and mtime
        // included.
        let before = verify::build(name, &src.live, &[], &id)?;
        let after = verify::build(name, &into, &[], &id)?;
        let diffs = verify::compare(&before, &after);
        if !diffs.is_empty() {
            return Err(Error::Refused {
                rule: "R7",
                path: into,
                why: format!(
                    "the copy of {} does not hash the same as the original ({}). the partial \
                     copy is under {} — ricepilot removes nothing, so it is still there to \
                     look at, and profile `{name}` was not created",
                    src.live.display(),
                    diffs
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("; "),
                    staging.display()
                ),
            });
        }
        copied.push((src.leaf.clone(), stats));
    }

    mutate::write_atomic(&staging.join("profile.toml"), c.manifest.as_bytes())?;
    mutate::fsync_dir(&staging)?;

    // The one externally visible step, and it is atomic: before it there is
    // no profile `<name>`, after it there is a complete one.
    mutate::make_dirs(&paths.profiles_dir())?;
    mutate::rename_within(&staging, &c.dir)?;
    mutate::fsync_dir(&paths.profiles_dir())?;

    // Record a blake3 manifest of the profile as captured, so `verify` has
    // something to compare against without waiting for a switch.
    let recorded = verify::build(name, &c.dir, &[], &id)?;
    let recorded_path = verify::manifest_path(&paths.state, name);
    verify::save(&recorded_path, &recorded)?;

    Ok(Output {
        text: header + &render::capture_done(&c, &copied, &recorded_path),
        code: ExitCode::Ok,
    })
}
