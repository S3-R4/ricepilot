//! The requires-check: which of a profile's `requires` are not installed.
//!
//! A profile may declare `requires = [...]`. Before a switch into it,
//! ricepilot asks `pacman -Q` about each name and hands the ones it could not
//! find to the planner as [`crate::plan::PlanContext::missing_requires`],
//! which declines the switch and prints the exact `paru -S --needed …` line.
//! ricepilot never installs anything (`NOT-POSSIBLE.md#installing-packages`).
//!
//! This is IO (a subprocess per package), so it lives here and hands the
//! planner data, the same division [`crate::plan::SourceFact`] uses: the
//! planner stays pure (D12).
//!
//! It asks one package at a time and reads only the exit status. `pacman -Q
//! sh` answers with `bash 5.3…` because bash *provides* sh, so a check that
//! read the names back out of pacman's output would call a satisfied
//! requirement missing. Exit 0 is "installed, or provided by something
//! installed", which is the question (D53).

use std::path::PathBuf;

use crate::ops::exec::{self, Allowed, Call};
use crate::{Error, Result};

/// The names in `packages` that `pacman -Q` could not find, in the order
/// given, without duplicates.
///
/// An empty list asks nothing and needs no pacman. A non-empty list on a
/// machine without pacman is a refusal: the pre-flight could not be
/// completed, and a switch whose pre-flight was not completed does not happen
/// (`SAFETY.md` R4).
pub fn missing(packages: &[String]) -> Result<Vec<String>> {
    let mut wanted: Vec<&str> = Vec::new();
    for p in packages {
        if !wanted.contains(&p.as_str()) {
            wanted.push(p);
        }
    }
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    if exec::locate(Allowed::PacmanQuery)?.is_none() {
        return Err(uncheckable(packages));
    }

    let mut out = Vec::new();
    for package in wanted {
        let ran = exec::run(Call::PacmanQuery { package })?;
        match ran.code {
            Some(0) => {}
            // Under `LC_ALL=C` (D52) this is pacman's one sentence for a name
            // it does not know. Anything else exiting 1 — an unreadable
            // database, say — is not an answer about this package.
            Some(1) if ran.stderr.contains("was not found") => out.push(package.to_string()),
            _ => return Err(unanswered(package, ran.code)),
        }
    }
    Ok(out)
}

/// The refusal when a profile has `requires` and there is no pacman to ask.
/// Public so its wording can be reviewed on a machine that has pacman.
pub fn uncheckable(packages: &[String]) -> Error {
    Error::Refused {
        rule: "R4",
        path: PathBuf::from(exec::BIN_DIRS[0]).join("pacman"),
        why: format!(
            "this profile requires {}, and ricepilot checks that with `pacman -Q`, which is not \
             installed in any of {}. it will not switch into a profile whose packages it could \
             not check; on a machine without pacman, take `requires` out of the manifest",
            packages.join(", "),
            exec::BIN_DIRS.join(", ")
        ),
    }
}

/// The refusal when pacman ran and said something that is neither "installed"
/// nor "not found". Public for the same reason as [`uncheckable`].
pub fn unanswered(package: &str, code: Option<i32>) -> Error {
    let how = match code {
        Some(c) => format!("exited {c}"),
        None => "was ended by a signal".to_string(),
    };
    Error::Refused {
        rule: "R4",
        path: PathBuf::from(package),
        why: format!(
            "`pacman -Q {package}` {how} without saying whether it is installed. ricepilot will \
             not guess, so the switch is not planned; run the same command yourself to see why"
        ),
    }
}
