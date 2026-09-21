//! `state/rescue.sh`: rung 3 of the recovery ladder (`docs/DESIGN.md` §7).
//!
//! A fully unrolled POSIX `sh` script that restores the previous generation
//! using nothing but `ln -sT`, `mv -T` and `mkdir -p`, at absolute paths. No
//! loops, no variables, no `PATH` lookup, no ricepilot binary. It is checked
//! with `sh -n` before it is written and written atomically as a real file, so
//! a switch that goes wrong cannot take it with them.
//!
//! It has to run from a TTY on a machine where the session will not start:
//! no D-Bus, no `hyprctl`, no fish, no quickshell, and nothing that assumes
//! the desktop came up.
//!
//! Everything it can do is a rename or a symlink creation. It cannot remove
//! anything (R2), so a destination the previous generation did not have is
//! displaced into a rescue attic, exactly as `rollback` displaces one (D36).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::generations::Generation;
use crate::ops::{exec, mutate, read};
use crate::{Error, Result};

/// The commands the script is allowed to name, and where they were found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binaries {
    pub ln: PathBuf,
    pub mv: PathBuf,
    pub mkdir: PathBuf,
}

/// Directories searched for the three commands, in order.
///
/// `PATH` is deliberately not consulted: the script runs on a machine whose
/// session did not start, possibly from a shell whose environment is whatever
/// the greeter left behind, and a rescue script that depends on the
/// environment being right is a rescue script for a problem that is not the
/// one it exists for.
const BIN_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/local/bin"];

impl Binaries {
    /// Locate each command by `lstat`ing the candidates. Reality, not a
    /// documented layout (`SAFETY.md` R7) — and if one is missing, the script
    /// is not written, because a rescue script naming a binary that is not
    /// there is worse than none: it fails at the moment it is relied on.
    pub fn locate() -> Result<Self> {
        Ok(Binaries {
            ln: find("ln")?,
            mv: find("mv")?,
            mkdir: find("mkdir")?,
        })
    }
}

fn find(name: &str) -> Result<PathBuf> {
    for dir in BIN_DIRS {
        let candidate = Path::new(dir).join(name);
        if read::lstat_or_absent(&candidate)?.is_some() {
            return Ok(candidate);
        }
    }
    Err(Error::Refused {
        rule: "R4",
        path: PathBuf::from(name),
        why: format!(
            "`{name}` is not in any of {}, so a rescue script naming an absolute path to it \
             could not be written. it would have failed at the one moment it was needed",
            BIN_DIRS.join(", ")
        ),
    })
}

/// Single-quote a path for `sh`. Everything inside single quotes is literal
/// except a single quote itself, which is closed, escaped and reopened.
fn quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The staging name the script links at before the `mv -T`. A sibling of the
/// destination, so the rename stays inside one directory and cannot meet an
/// `EXDEV` (`docs/DESIGN.md` §2).
fn staging(dest: &Path) -> PathBuf {
    let file = dest
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    match dest.parent() {
        Some(p) => p.join(format!("{file}.rp-rescue")),
        None => PathBuf::from(format!("{file}.rp-rescue")),
    }
}

/// Where the script parks a link the previous generation did not have.
pub fn rescue_attic(state: &Path, id: u32) -> PathBuf {
    state.join("attic").join(format!("rescue-{id:04}"))
}

/// Generate the script that takes the machine back to `to`.
///
/// Pure: it takes values and returns a `String`, so every line of it is
/// testable without a filesystem — and so the script that is checked by
/// `sh -n` is the exact string that is written.
pub fn script(to: &Generation, bins: &Binaries, attic: &Path) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "#!/bin/sh");
    let _ = writeln!(
        s,
        "# ricepilot rescue script — restores generation {:04}.",
        to.id
    );
    let _ = writeln!(s, "# profile: {}", to.profile);
    let _ = writeln!(s, "# written: {}", to.created);
    let _ = writeln!(s, "#");
    let _ = writeln!(
        s,
        "# Run it from a TTY with:  sh <this file>   — it needs no ricepilot, no D-Bus,"
    );
    let _ = writeln!(
        s,
        "# no hyprctl and no PATH. Every command below is an absolute path, every step is"
    );
    let _ = writeln!(
        s,
        "# written out in full, and nothing here removes anything: a link this generation"
    );
    let _ = writeln!(s, "# did not have is moved into");
    let _ = writeln!(s, "#     {}", attic.display());
    let _ = writeln!(s, "#");
    let _ = writeln!(
        s,
        "# It reports each step and carries on past a failure, so one destination it"
    );
    let _ = writeln!(
        s,
        "# cannot restore does not cost you the ones it can. Read the output."
    );
    let _ = writeln!(s);

    if to.entries.is_empty() {
        let _ = writeln!(s, "echo 'this generation managed no destinations.'");
        return s;
    }

    for e in &to.entries {
        let dest = quote(&e.dest);
        match &e.target {
            Some(target) => {
                let _ = writeln!(s, "# {} -> {}", e.dest.display(), target.display());
                let _ = writeln!(
                    s,
                    "if {} -sT {} {} && {} -T {} {}; then",
                    bins.ln.display(),
                    quote(target),
                    quote(&staging(&e.dest)),
                    bins.mv.display(),
                    quote(&staging(&e.dest)),
                    dest
                );
            }
            None => {
                // The previous generation had nothing here. Removing is not
                // available (R2), so the link is displaced into the rescue
                // attic — the same answer `rollback` gives (D36).
                let parked = attic.join(e.dest.strip_prefix("/").unwrap_or(&e.dest));
                let parent = parked.parent().unwrap_or(attic).to_path_buf();
                let _ = writeln!(s, "# {} had nothing here; displace it", e.dest.display());
                let _ = writeln!(
                    s,
                    "if {} -p {} && {} -T {} {}; then",
                    bins.mkdir.display(),
                    quote(&parent),
                    bins.mv.display(),
                    dest,
                    quote(&parked)
                );
            }
        }
        let _ = writeln!(s, "  echo 'ok      {}'", echo_safe(&e.dest));
        let _ = writeln!(s, "else");
        let _ = writeln!(s, "  echo 'FAILED  {}'", echo_safe(&e.dest));
        let _ = writeln!(s, "fi");
        let _ = writeln!(s);
    }

    let _ = writeln!(s, "echo 'ricepilot: rescue finished. log out and back in.'");
    s
}

/// A path as it appears inside a single-quoted `echo`. Same escaping as
/// [`quote`], without the surrounding quotes, since the caller supplies them.
fn echo_safe(p: &Path) -> String {
    p.to_string_lossy().replace('\'', "'\\''")
}

pub fn path(state: &Path) -> PathBuf {
    state.join("rescue.sh")
}

/// Write `state/rescue.sh` for the generation `to`.
///
/// The script is parsed by `sh -n` **before** it is written. A rescue script
/// with a syntax error is not a degraded rescue script; it is a file that
/// looks like a way out and is not one, discovered by a user at a TTY with no
/// desktop. So a script that does not parse is refused and the previous one —
/// which did parse — is left exactly where it is.
pub fn regenerate(state: &Path, to: &Generation) -> Result<PathBuf> {
    let bins = Binaries::locate()?;
    let attic = rescue_attic(state, to.id);
    let text = script(to, &bins, &attic);
    exec::sh_syntax_check(&text)?;

    let p = path(state);
    mutate::write_atomic(&p, text.as_bytes())?;
    Ok(p)
}
