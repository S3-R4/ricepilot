//! A whole machine for the M3 command suites: two registered profiles, a
//! ledger that owns the live links, and a destination that only the second
//! profile claims.
//!
//! A sibling of [`super::scenario`] rather than a generalisation of it.
//! `scenario` is M2-shaped — it hardcodes the `m2` fixture group and drives
//! `ops::apply` directly — and quietly repurposing it would leave M2's suites
//! depending on changes made for M3's.

#![allow(dead_code)]

use std::path::PathBuf;

use ricepilot::cli::paths::Paths;
use ricepilot::ledger::Ledger;
use ricepilot::ops::read;

use super::Fixture;

pub struct Machine {
    pub f: Fixture,
    pub old_root: PathBuf,
    pub new_root: PathBuf,
    /// Owned by `old`, claimed by both profiles.
    pub hypr: PathBuf,
    pub foot: PathBuf,
    /// Absent, and claimed only by `new` — so switching creates it and
    /// rolling back must retire it (D36).
    pub btop: PathBuf,
}

const GROUP: &str = "m3cli";

fn manifest(name: &str, root: &std::path::Path, leaves: &[&str]) -> String {
    let mut s = format!(
        "name = \"{name}\"\nroot = \"{}\"\nvolatile = [\"**/*.log\"]\n",
        root.display()
    );
    for leaf in leaves {
        s.push_str(&format!(
            "\n[[path]]\ndest       = \"~/.config/{leaf}\"\nsrc        = \"{leaf}\"\nkind       \
             = \"dir-link\"\nactivation = \"relogin\"\n"
        ));
    }
    s
}

/// Build the machine. `old` is live and recorded in the ledger; `new` is
/// registered and claims one destination more.
pub fn build(case: &str) -> Machine {
    let f = Fixture::new_in(GROUP, case);
    let old_root = f.dir("rice/old");
    let new_root = f.dir("rice/new");
    for leaf in ["hypr", "foot"] {
        f.dir(&format!("rice/old/{leaf}"));
        f.file(&format!("rice/old/{leaf}/marker"), "old\n");
    }
    for leaf in ["hypr", "foot", "btop"] {
        f.dir(&format!("rice/new/{leaf}"));
        f.file(&format!("rice/new/{leaf}/marker"), "new\n");
        f.file(&format!("rice/new/{leaf}/noise.log"), "volatile\n");
    }

    f.profile("old", &manifest("old", &old_root, &["hypr", "foot"]));
    f.profile(
        "new",
        &manifest("new", &new_root, &["hypr", "foot", "btop"]),
    );

    f.dir(".config");
    let hypr = f.link(".config/hypr", &old_root.join("hypr"));
    let foot = f.link(".config/foot", &old_root.join("foot"));
    let btop = f.path(".config/btop");
    f.clear(".config/btop");

    // The ledger is what makes these links *ours*. Without it they are foreign
    // and every switch onto them is refused — correctly, and uselessly for a
    // test about switching.
    let mut ledger = Ledger::default();
    ledger.record(&[hypr.clone(), foot.clone()], "old").unwrap();
    ricepilot::ledger::save(&f.state().join("ledger.toml"), &ledger).unwrap();

    Machine {
        f,
        old_root,
        new_root,
        hypr,
        foot,
        btop,
    }
}

/// Re-describe a machine whose tree already exists, without disturbing it —
/// for inspecting what a helper process left behind.
pub fn attach(case: &str) -> Machine {
    let f = Fixture::attach(GROUP, case);
    Machine {
        old_root: f.path("rice/old"),
        new_root: f.path("rice/new"),
        hypr: f.path(".config/hypr"),
        foot: f.path(".config/foot"),
        btop: f.path(".config/btop"),
        f,
    }
}

impl Machine {
    pub fn paths(&self) -> Paths {
        // `Paths::rooted_at` reads the override variables, which the helper
        // process and the test both set from `Fixture::env`.
        Paths::rooted_at(self.f.home.clone())
    }

    pub fn dests(&self) -> Vec<PathBuf> {
        vec![self.hypr.clone(), self.foot.clone(), self.btop.clone()]
    }

    /// What each destination points at now: `None` for absent, `Some(None)`
    /// for something that is not a symlink.
    pub fn live(&self) -> Vec<Option<Option<PathBuf>>> {
        self.dests()
            .iter()
            .map(|d| ricepilot::ops::mutate::link_target(d).unwrap())
            .collect()
    }

    /// Fully on profile `old`: two links into it, nothing at `btop`.
    pub fn all_old(&self) -> Vec<Option<Option<PathBuf>>> {
        vec![
            Some(Some(self.old_root.join("hypr"))),
            Some(Some(self.old_root.join("foot"))),
            None,
        ]
    }

    /// Fully on profile `new`: three links into it.
    pub fn all_new(&self) -> Vec<Option<Option<PathBuf>>> {
        vec![
            Some(Some(self.new_root.join("hypr"))),
            Some(Some(self.new_root.join("foot"))),
            Some(Some(self.new_root.join("btop"))),
        ]
    }

    /// `(dev, ino, mtime_ns)` of every path in both profile trees, for the
    /// round-trip property: a switch must not touch a byte of either profile.
    pub fn profile_identities(&self) -> std::collections::BTreeMap<PathBuf, (u64, u64, i64)> {
        let mut out = std::collections::BTreeMap::new();
        for root in [&self.old_root, &self.new_root] {
            collect(root, &mut out);
        }
        out
    }
}

fn collect(dir: &std::path::Path, out: &mut std::collections::BTreeMap<PathBuf, (u64, u64, i64)>) {
    for name in read::list_dir(dir).unwrap() {
        let p = dir.join(name);
        let m = read::lstat(&p).unwrap().unwrap();
        out.insert(p.clone(), (m.dev, m.ino, m.mtime_ns));
        if m.kind == read::Kind::Dir {
            collect(&p, out);
        }
    }
}
