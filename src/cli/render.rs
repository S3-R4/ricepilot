//! Every user-visible string ricepilot prints for a read-only command.
//!
//! Rendering is pure — it takes values and returns a `String` — so every
//! message, including every refusal, is snapshot-testable without a
//! filesystem or a subprocess.

use std::fmt::Write as _;
use std::path::Path;

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
pub fn status(paths: &Paths, profiles: &[Profile], ledger_present: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "home:      {}", paths.home.display());
    let _ = writeln!(s, "data:      {}", paths.data.display());
    let _ = writeln!(s, "state:     {}", paths.state.display());
    let _ = writeln!(s, "profiles:  {}", profiles.len());
    for p in profiles {
        let _ = writeln!(s, "             {}", p.name);
    }
    let _ = writeln!(
        s,
        "ledger:    {}",
        if ledger_present {
            "present"
        } else {
            "absent — no live path is registered as owned yet"
        }
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "generations and drift reporting are not implemented yet (milestone M3)."
    );
    s
}

/// Printed by `plan` while the ledger reader is still M3 work. Without it the
/// command would report every existing link as foreign and give no hint why.
pub const NO_LEDGER_NOTE: &str = "\nnote: no ledger is being read yet (milestone M3), so no live \
link can satisfy the\n      ownership predicate. Existing links are reported as foreign, which is \
the\n      correct answer until `init` registers them.\n";
