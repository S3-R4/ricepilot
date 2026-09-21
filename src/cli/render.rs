//! Every user-visible string ricepilot prints for a read-only command.
//!
//! Rendering is pure — it takes values and returns a `String` — so every
//! message, including every refusal, is snapshot-testable without a
//! filesystem or a subprocess.

use std::fmt::Write as _;
use std::path::Path;

use crate::journal::{Direction, Recovery};
use crate::manifest::Manifest;
use crate::observe::Observed;
use crate::plan::{Plan, Refusal};

use super::paths::{Paths, Profile};

/// `ricepilot plan <profile>` — the dry-run output. This is the whole of the
/// promise in `SAFETY.md` R4: what is printed here is the value
/// `switch --commit` executes.
pub fn plan(profile: &str, observed: &[Observed], plan: &Plan) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "plan: switch to profile `{profile}`");
    let _ = writeln!(s);

    let _ = writeln!(s, "observed:");
    if observed.is_empty() {
        let _ = writeln!(s, "  (this profile declares no dir-link destinations)");
    }
    for o in observed {
        let _ = writeln!(s, "  {:<14} {}", o.shape.as_str(), o.dest.display());
    }
    let _ = writeln!(s);

    match plan {
        Plan::NoOp => {
            let _ = writeln!(s, "nothing to do: every destination already matches.");
        }
        Plan::Apply { ops } => {
            let _ = writeln!(s, "would apply {} operation(s):", ops.len());
            for op in ops {
                let _ = writeln!(s, "  {op}");
            }
            let _ = writeln!(s);
            let _ = writeln!(
                s,
                "nothing has been changed. re-run with --commit to apply."
            );
            let _ = writeln!(
                s,
                "a switch takes effect at the next login; it does not touch the running session."
            );
        }
        Plan::Decline { refusals } => {
            let _ = writeln!(s, "declined. nothing has been changed.");
            let _ = writeln!(s);
            let _ = write!(s, "{}", refusal_list(refusals));
        }
    }
    s
}

/// The refusal block, on its own so each variant's wording can be snapshotted
/// independently of the surrounding plan.
pub fn refusal_list(refusals: &[Refusal]) -> String {
    let mut s = String::new();
    for r in refusals {
        let _ = writeln!(s, "  [{}] {r}", r.rule());
    }
    s
}

/// `ricepilot list`.
pub fn list(profiles: &[Profile], home: &Path) -> String {
    let mut s = String::new();
    if profiles.is_empty() {
        return "no profiles registered. run `ricepilot init` to register the live rice.\n".into();
    }
    let _ = writeln!(s, "{:<16} {:<14} ROOT", "PROFILE", "KIND");
    for p in profiles {
        let kind = if p.manifest.root.is_some() {
            "by-reference"
        } else {
            "owned payload"
        };
        let _ = writeln!(s, "{:<16} {:<14} {}", p.name, kind, p.root(home).display());
    }
    s
}

/// `ricepilot show <profile>` — the manifest as ricepilot understands it,
/// which is not necessarily what the file says: defaults are filled in and
/// `~` is expanded, and seeing that is the point of the command.
pub fn show(name: &str, m: &Manifest, root: &Path, home: &Path) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "profile:      {name}");
    let _ = writeln!(
        s,
        "root:         {} ({})",
        root.display(),
        if m.root.is_some() {
            "by reference — ricepilot never writes here"
        } else {
            "owned payload"
        }
    );
    let _ = writeln!(
        s,
        "hypr dialect: {}",
        m.hypr_dialect.as_deref().unwrap_or("(unset)")
    );
    let _ = writeln!(
        s,
        "requires:     {}",
        if m.requires.is_empty() {
            "(none)".to_string()
        } else {
            m.requires.join(", ")
        }
    );
    let _ = writeln!(s);

    let _ = writeln!(s, "paths:");
    if m.paths.is_empty() {
        let _ = writeln!(s, "  (none)");
    }
    for p in &m.paths {
        let _ = writeln!(
            s,
            "  {:<10} {:<8} {:<34} <- {}",
            p.kind.as_str(),
            p.activation.as_str(),
            crate::manifest::expand_home(&p.dest, home).display(),
            root.join(&p.src).display()
        );
    }

    let _ = writeln!(s);
    let _ = writeln!(s, "volatile globs (excluded from hashing):");
    if m.volatile.is_empty() {
        let _ = writeln!(s, "  (none)");
    }
    for v in &m.volatile {
        let _ = writeln!(s, "  {v}");
    }

    let _ = writeln!(s);
    let _ = writeln!(s, "generated paths (never linked, never profile content):");
    if m.generated.is_empty() {
        let _ = writeln!(s, "  (none)");
    }
    for g in &m.generated {
        let _ = writeln!(s, "  {}", g.display());
    }
    s
}

/// `ricepilot status`. Reports what it observed, not what a manifest claims
/// (`SAFETY.md` R7), and says plainly which parts are not built yet rather
/// than printing a reassuring blank.
pub fn status(
    paths: &Paths,
    profiles: &[Profile],
    ledger: &crate::ledger::Ledger,
    generation: Option<&crate::generations::Generation>,
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "home:      {}", paths.home.display());
    let _ = writeln!(s, "data:      {}", paths.data.display());
    let _ = writeln!(s, "state:     {}", paths.state.display());
    let _ = writeln!(s, "profiles:  {}", profiles.len());
    for p in profiles {
        let _ = writeln!(s, "             {}", p.name);
    }
    if ledger.entries.is_empty() {
        let _ = writeln!(
            s,
            "owned:     none — no live path is registered as owned yet"
        );
    } else {
        let _ = writeln!(s, "owned:     {} path(s)", ledger.entries.len());
        for e in &ledger.entries {
            let _ = writeln!(
                s,
                "             {} -> {} ({})",
                e.dest.display(),
                e.target.display(),
                e.profile
            );
        }
    }
    match generation {
        Some(g) => {
            let _ = writeln!(
                s,
                "generation: {:04} — `{}`, switched {}",
                g.id, g.profile, g.created
            );
        }
        None => {
            let _ = writeln!(
                s,
                "generation: none — ricepilot has not switched anything on this machine"
            );
        }
    }
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "drift reporting at switch time is not implemented yet (milestone M5); \
         `ricepilot verify <profile>`"
    );
    let _ = writeln!(
        s,
        "compares a profile against the manifest its switch recorded."
    );
    s
}

/// `ricepilot recover` when there is no in-flight journal — which is what a
/// healthy machine looks like, and is worth saying plainly rather than
/// printing nothing.
pub const NOTHING_TO_RECOVER: &str = "no switch is in flight: there is no journal at \
`state/journal/current.toml`.\nnothing to recover, and nothing has been changed.\n";

/// Why recovery is going the way it is going. There are only two directions
/// and no third outcome (`SAFETY.md` R5), so both are spelled out.
fn direction_note(d: Direction) -> &'static str {
    match d {
        Direction::Forward => {
            "forward — at least one destination is already switched, so finishing is the only \
             outcome\n           that does not undo something already in effect."
        }
        Direction::Backward => {
            "backward — no exchange had happened yet, so the switch never started. The staged \
             links\n            go to the attic; nothing that was live is touched."
        }
    }
}

/// `ricepilot recover`, dry-run and committed. The two differ only in the last
/// paragraph: what is printed above it is the same value either way, which is
/// the whole of R4's promise.
pub fn recover(r: &Recovery, committed: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "recover: an interrupted switch to profile `{}` ({})",
        r.profile, r.id
    );
    let _ = writeln!(s);

    let _ = writeln!(s, "observed:");
    for st in &r.statuses {
        let _ = writeln!(s, "  {:<14} {}", st.side.as_str(), st.dest.display());
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "direction: {}", direction_note(r.direction));
    let _ = writeln!(s);

    if r.actions.is_empty() {
        let _ = writeln!(s, "there is nothing left to do.");
    } else {
        let _ = writeln!(
            s,
            "{} {} operation(s):",
            if committed { "applied" } else { "would apply" },
            r.actions.len()
        );
        for a in &r.actions {
            let _ = writeln!(s, "  {a}");
        }
    }
    let _ = writeln!(s);

    if committed {
        let _ = writeln!(
            s,
            "recovered. every destination is now on one side of the switch, and the journal has"
        );
        let _ = writeln!(s, "been retired to `state/journal/done-{}.toml`.", r.id);
        let _ = writeln!(
            s,
            "displaced links are in `state/attic/{}/`; nothing was removed.",
            r.id
        );
        if r.direction == Direction::Forward {
            let _ = writeln!(s);
            let _ = writeln!(
                s,
                "the switch took effect on disk. it reaches the session at the next login."
            );
        }
    } else {
        let _ = writeln!(
            s,
            "nothing has been changed. re-run with --commit to carry this out."
        );
    }
    s
}

/// `ricepilot verify <profile>`.
///
/// Says what was compared before it says what differs. A drift report whose
/// reader cannot tell which paths were *excluded* from it is a report that
/// invites the wrong conclusion from a short list.
pub fn verify(
    profile: &str,
    root: &Path,
    recorded: &crate::verify::TreeManifest,
    diffs: &[crate::verify::Difference],
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "verify: profile `{profile}`");
    let _ = writeln!(s, "root:      {}", root.display());
    let _ = writeln!(
        s,
        "recorded:  {} ({} path(s))",
        recorded.created,
        recorded.entries.len()
    );
    let _ = writeln!(
        s,
        "excluded:  {}",
        if recorded.volatile.is_empty() {
            "(no volatile globs declared)".to_string()
        } else {
            recorded.volatile.join(", ")
        }
    );
    let _ = writeln!(s);

    let substantive: Vec<&crate::verify::Difference> =
        diffs.iter().filter(|d| d.is_substantive()).collect();
    let touched = diffs.len() - substantive.len();

    if substantive.is_empty() {
        let _ = writeln!(s, "clean: every recorded path still hashes to what it did.");
    } else {
        let _ = writeln!(s, "{} path(s) differ:", substantive.len());
        for d in &substantive {
            let _ = writeln!(s, "  {d}");
        }
    }
    if touched > 0 {
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "{touched} further path(s) have a newer modification time and byte-identical \
             content."
        );
        let _ = writeln!(
            s,
            "that is not drift; it is an application having rewritten a file with what was \
             already"
        );
        let _ = writeln!(s, "in it. declare the path `volatile` to stop hashing it.");
    }
    s
}

/// `ricepilot rescue`. Prints where the script is and what running it does —
/// and prints the command in full, because the person reading this may be
/// typing it at a TTY from a photograph of another screen.
pub fn rescue(path: &Path) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "the standalone rescue script is at:");
    let _ = writeln!(s, "  {}", path.display());
    let _ = writeln!(s);
    let _ = writeln!(s, "run it from a TTY (Ctrl+Alt+F2 … F6) with:");
    let _ = writeln!(s, "  sh {}", path.display());
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "it restores the previous generation using absolute paths only. it needs no ricepilot,"
    );
    let _ = writeln!(
        s,
        "no D-Bus, no hyprctl and no shell but /bin/sh, and it removes nothing: anything it"
    );
    let _ = writeln!(s, "displaces goes to `state/attic/`.");
    s
}

/// The part of `switch`/`rollback` output that is the same dry-run and
/// committed: what was observed and what the plan is. R4's promise is that
/// this block is identical either way, so it is one function.
pub fn switch_header(
    req: &super::switch::Request,
    observed: &[Observed],
    plan: &Plan,
    committing: bool,
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{}: {}", req.kind.as_str(), req.label);
    let _ = writeln!(s);

    let _ = writeln!(s, "observed:");
    if observed.is_empty() {
        let _ = writeln!(s, "  (nothing to switch: no dir-link destinations)");
    }
    for o in observed {
        let retiring = if req.retire.contains(&o.dest) {
            "  (no longer managed after this)"
        } else {
            ""
        };
        let _ = writeln!(
            s,
            "  {:<14} {}{retiring}",
            o.shape.as_str(),
            o.dest.display()
        );
    }
    let _ = writeln!(s);

    match plan {
        Plan::NoOp => {
            let _ = writeln!(s, "nothing to do: every destination already matches.");
        }
        Plan::Apply { ops } => {
            // The list is the same value either way — that is R4's promise —
            // but the verb is not. Telling someone who typed `--commit` what
            // ricepilot "would" do is how a person ends up unsure whether
            // their machine changed.
            let _ = writeln!(
                s,
                "{} {} operation(s):",
                if committing {
                    "applying"
                } else {
                    "would apply"
                },
                ops.len()
            );
            for op in ops {
                let _ = writeln!(s, "  {op}");
            }
        }
        Plan::Decline { refusals } => {
            let _ = writeln!(s, "declined. nothing has been changed.");
            let _ = writeln!(s);
            let _ = write!(s, "{}", refusal_list(refusals));
        }
    }
    s
}

/// The dry-run tail. Says what did not happen, and what a switch does and does
/// not reach — a user who expects their session to change is a user who will
/// conclude the tool did not work.
pub const UNCOMMITTED: &str = "\nnothing has been changed. re-run with --commit to apply.\n\
a switch is an on-disk relink: it takes effect at the next login and does not touch\nthe running \
session.\n";

/// The committed tail: what happened, where the displaced things are, and the
/// two ways back.
#[allow(clippy::too_many_arguments)]
pub fn switch_done(
    req: &super::switch::Request,
    new_id: u32,
    back_to: u32,
    attic: &Path,
    script: &Path,
    manifest: Option<&Path>,
    retired: &[std::path::PathBuf],
) -> String {
    let mut s = String::new();
    let _ = writeln!(s);
    let _ = writeln!(s, "applied. this is generation {new_id:04}.");
    let _ = writeln!(s);
    let _ = writeln!(s, "displaced links are in:");
    let _ = writeln!(s, "  {}", attic.display());
    let _ = writeln!(s, "nothing was removed.");

    if !retired.is_empty() {
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "{} destination(s) are no longer managed. ricepilot cannot make a path absent —",
            retired.len()
        );
        let _ = writeln!(
            s,
            "that would be a deletion — so their links were moved into the attic above:"
        );
        for r in retired {
            let _ = writeln!(s, "  {}", r.display());
        }
    }

    if let Some(m) = manifest {
        let _ = writeln!(s);
        let _ = writeln!(s, "a blake3 manifest of the profile was recorded at:");
        let _ = writeln!(s, "  {}", m.display());
        let _ = writeln!(s, "`ricepilot verify` compares against it.");
    }

    let _ = writeln!(s);
    let _ = writeln!(s, "to undo this:");
    let _ = writeln!(
        s,
        "  ricepilot rollback --commit            re-applies generation {back_to:04}"
    );
    let _ = writeln!(
        s,
        "  sh {}   the same thing, from a TTY, without ricepilot",
        script.display()
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "this changed the filesystem, not the running session. log out and back in to see it:"
    );
    let _ = writeln!(s, "  uwsm stop");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "never `hyprctl dispatch exit` and never kill Hyprland — `uwsm stop` is the clean"
    );
    let _ = writeln!(
        s,
        "logout. if the session does not come back, TTY F2–F6 and run the rescue script above."
    );
    let _ = req;
    s
}
