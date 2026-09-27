//! The verify-config pre-flight: does the Hyprland config a switch would put
//! at `~/.config/hypr` parse?
//!
//! Only ever by way of a [`SandboxedConfig`] — a scratch copy with every
//! exec-family line stripped, whose one constructor lives in
//! [`crate::ops::exec::sandbox`] — and only ever as a syntax check. Exit 0
//! means the syntax parsed and nothing else
//! (`NOT-POSSIBLE.md#verify-config-as-proof`).
//!
//! What is checked, and what is not (D55):
//!
//! * A switch whose targets include no `~/.config/hypr`, or whose tree there
//!   has neither `hyprland.lua` nor `hyprland.conf`, ships no Hyprland config
//!   and nothing is checked.
//! * `hyprland.lua` present: Hyprland 0.55 loads it in preference to
//!   `hyprland.conf` — whatever `hypr_dialect` says — and a Lua config is a
//!   program. It is not run (`NOT-POSSIBLE.md#verify-lua-config`); the plan
//!   says it was not checked.
//! * No `Hyprland` binary: nothing to check with; the plan says so and the
//!   switch goes ahead. Whether Hyprland must be installed is what `requires`
//!   is for, and the requires-check refuses when it is not (D53).
//! * Either kind of "not checked" is a refusal under `--strict` (D59).
//! * Otherwise the copy is made under `state/verify/<id>/`, Hyprland is run
//!   on it, and a non-zero exit — or a config that could not be copied
//!   faithfully — declines the switch with `Refusal::VerifyConfigFailed`.

use std::path::{Path, PathBuf};

use crate::ops::exec::{self, sandbox, Allowed, Call, SandboxedConfig};
use crate::ops::read;
use crate::plan::Target;
use crate::Result;

/// The Lua entry point Hyprland ≥ 0.55 prefers, and the conf one it falls
/// back to (its own log: "Lua config not found, using legacy config").
pub const LUA_ENTRY: &str = "hyprland.lua";
pub const CONF_ENTRY: &str = "hyprland.conf";

/// Most lines of Hyprland's report carried into a refusal.
const REPORT_LINES: usize = 12;

/// What the pre-flight found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The copy parsed.
    Parsed {
        config: PathBuf,
        scratch: PathBuf,
        files: usize,
        stripped: usize,
    },
    /// It did not, or it could not be copied faithfully. Declines the switch.
    Failed {
        config: PathBuf,
        /// `None` when nothing was written: the config was refused before a
        /// copy was made.
        scratch: Option<PathBuf>,
        detail: String,
    },
    /// Not checked, and why. Does not decline the switch, unless it is
    /// `--strict` (D59).
    NotChecked { config: PathBuf, why: NotChecked },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotChecked {
    /// No `Hyprland` in [`exec::BIN_DIRS`].
    NoHyprland,
    /// `hyprland.lua`: a program, not a config ricepilot can sandbox.
    Lua,
}

impl Outcome {
    /// The refusal this outcome makes, if it makes one.
    pub fn failure(&self) -> Option<(PathBuf, String)> {
        match self {
            Outcome::Failed { config, detail, .. } => Some((config.clone(), detail.clone())),
            _ => None,
        }
    }

    /// The config that was not checked, and why in a few words: what
    /// `--strict` refuses instead of reporting (D59).
    pub fn not_checked(&self) -> Option<(PathBuf, String)> {
        match self {
            Outcome::NotChecked { config, why } => Some((
                config.clone(),
                match why {
                    NotChecked::NoHyprland => format!(
                        "`Hyprland` is not installed in {}",
                        exec::BIN_DIRS.join(", ")
                    ),
                    NotChecked::Lua => "a Lua config is a program, which ricepilot does not \
                                        run; see NOT-POSSIBLE.md#verify-lua-config"
                        .to_string(),
                },
            )),
            _ => None,
        }
    }
}

/// `~/.config/hypr`.
pub fn hypr_dest(home: &Path) -> PathBuf {
    home.join(".config/hypr")
}

/// The outcome for a config the sandbox would not copy: Hyprland was not
/// run on it, and why. Public so the refusal can be snapshotted from the
/// sandbox's own answer without a `Hyprland` installed.
pub fn unsandboxed(config: PathBuf, why: &crate::hyprconf::Unsandboxable) -> Outcome {
    Outcome::Failed {
        config,
        scratch: None,
        detail: format!(
            "it could not be copied into a sandbox, so Hyprland was not run on it: {why}"
        ),
    }
}

/// Run the pre-flight for a switch to `targets`, retiring `retire`.
///
/// `state` is ricepilot's state directory and `id` names the scratch copy —
/// the switch id, so the copy and the attic and journal of the same switch
/// can be found from one another.
pub fn check(
    home: &Path,
    state: &Path,
    id: &str,
    targets: &[Target],
    retire: &[PathBuf],
) -> Result<Option<Outcome>> {
    let dest = hypr_dest(home);
    let Some(t) = targets.iter().find(|t| t.dest == dest) else {
        return Ok(None);
    };
    // A source that is missing or not a directory is the planner's to refuse
    // (`SourceMissing`, `SourceNotADirectory`); there is nothing here to read.
    match read::lstat_or_absent(&t.src)? {
        Some(m) if m.kind == read::Kind::Dir => {}
        _ => return Ok(None),
    }

    let lua = t.src.join(LUA_ENTRY);
    if read::lstat_or_absent(&lua)?.is_some() {
        return Ok(Some(Outcome::NotChecked {
            config: lua,
            why: NotChecked::Lua,
        }));
    }
    let conf = t.src.join(CONF_ENTRY);
    if read::lstat_or_absent(&conf)?.is_none() {
        return Ok(None);
    }
    if exec::locate(Allowed::HyprlandVerifyConfig)?.is_none() {
        return Ok(Some(Outcome::NotChecked {
            config: conf,
            why: NotChecked::NoHyprland,
        }));
    }

    let links: Vec<(PathBuf, PathBuf)> = targets
        .iter()
        .map(|t| (t.dest.clone(), t.src.clone()))
        .collect();
    let entry = dest.join(CONF_ENTRY);
    let built = SandboxedConfig::build(&sandbox::Request {
        home,
        entry: &entry,
        links: &links,
        absent: retire,
        parent: &state.join("verify"),
        id,
    })?;
    let cfg = match built {
        Ok(cfg) => cfg,
        Err(why) => return Ok(Some(unsandboxed(conf, &why))),
    };

    let ran = exec::run(Call::HyprlandVerifyConfig(&cfg))?;
    let stripped = cfg.copied().iter().map(|c| c.stripped.len()).sum();
    if ran.success() {
        return Ok(Some(Outcome::Parsed {
            config: conf,
            scratch: cfg.scratch().to_path_buf(),
            files: cfg.copied().len(),
            stripped,
        }));
    }
    Ok(Some(Outcome::Failed {
        config: conf,
        scratch: Some(cfg.scratch().to_path_buf()),
        detail: report(&cfg, ran.code, &ran.stdout),
    }))
}

/// What Hyprland said, in terms of the user's files rather than the copies:
/// the lines after its `Config parsing result:` banner.
fn report(cfg: &SandboxedConfig, code: Option<i32>, stdout: &str) -> String {
    let plain = strip_ansi(stdout);
    let Some((_, result)) = plain.split_once("Config parsing result:") else {
        return match code {
            Some(c) => format!("Hyprland exited {c} without reporting a parse result"),
            None => "Hyprland was ended by a signal before reporting a parse result".into(),
        };
    };
    let lines: Vec<String> = result
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| cfg.in_terms_of_the_originals(l))
        .collect();
    let mut s = String::from("Hyprland reported:");
    for l in lines.iter().take(REPORT_LINES) {
        s.push_str("\n      ");
        s.push_str(l);
    }
    if lines.len() > REPORT_LINES {
        s.push_str(&format!(
            "\n      … and {} more line(s)",
            lines.len() - REPORT_LINES
        ));
    }
    s
}

/// Hyprland colours its log lines even into a pipe.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_is_stripped() {
        assert_eq!(
            strip_ansi("\u{1b}[1;31mERR \u{1b}[0m]: x"),
            "ERR ]: x".to_string()
        );
    }
}
