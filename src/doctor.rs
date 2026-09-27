//! `ricepilot doctor`: a health report for someone who already suspects
//! something is wrong.
//!
//! **Read-only by construction** (D57). Everything [`diagnose`] learns about
//! the machine comes through the `&dyn Look` it is handed, and
//! [`crate::ops::look::Look`] has no method that changes anything: it reads
//! files, lists directories, and runs `pacman -Q` and `sh -n`. This module
//! names no other effectful item — not the ledger's or a generation's
//! writer, not the journal's, not the lock, not the verify-config sandbox —
//! and `tests/doctor.rs` fails if it starts to: every crate path spelled
//! in `src/doctor.rs` and `src/doctor/` is checked against a list of pure
//! items. So `doctor` never takes the lock (it can run beside a stuck
//! switch, which is when it is needed), never builds a scratch copy for
//! `Hyprland --verify-config` (it says the config was not checked by it, and
//! `ricepilot plan` is what checks it), and prints commands instead of
//! running them — including the two ricepilot is forbidden to run itself:
//! `snapper -c home create-config /home` and the `hypr-session`
//! `SESSION_DIR` patch.
//!
//! Nothing here stops at the first thing it cannot read. A doctor that exits
//! on a broken ledger is useless to exactly the person running it, so every
//! check turns its own failure into a finding and the rest carry on.
//!
//! The report puts **problems** first — ricepilot's own state or links, each
//! with its path, the rule, and the exact commands to run, and any one of
//! them makes the exit status [`ExitCode::Unhealthy`] — then **hazards**
//! (standing facts about the machine ricepilot works around but cannot
//! change), then what was **not checked** and why, and folds every healthy
//! check into one line.
//!
//! Text is broken into lines by hand, never wrapped by width: a wrap point
//! that depends on how long a path is would make the report differ between
//! two checkouts that differ only in where they live.

mod render;

use std::path::{Path, PathBuf};

use crate::cli::paths::Paths;
use crate::error::ExitCode;
use crate::manifest::Manifest;
use crate::ops::look::Kind;
use crate::ops::look::Look;
use crate::plan::Target;
use crate::Error;

/// Where to look.
#[derive(Debug, Clone)]
pub struct Where {
    pub paths: Paths,
    /// The root the machine-wide checks read under — `/proc` for running
    /// theme daemons, `/etc/snapper/configs` for snapshots. `None` skips them
    /// and says so: the CLI passes `None` inside the test sandbox (D56),
    /// where the machine is not the fixture's to describe. Tests of those
    /// checks pass a fixture directory here, as a value; no variable can.
    pub system: Option<PathBuf>,
}

/// One thing to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The path the finding is about.
    pub path: PathBuf,
    /// What is wrong with it, as one clause.
    pub what: String,
    /// The rule or decision it is judged by.
    pub rule: String,
    /// What was observed, and why it matters: one paragraph per entry, with
    /// its lines already broken.
    pub detail: Vec<String>,
    /// Shell lines to run, in order. A line starting with `#` is a comment
    /// saying what the next ones are for.
    pub run: Vec<String>,
}

/// The whole report. [`Report::text`] is what is printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub problems: Vec<Finding>,
    pub hazards: Vec<Finding>,
    /// What was not checked, and why; and facts worth knowing that are not
    /// problems.
    pub notes: Vec<String>,
    /// One short phrase per check that passed.
    pub healthy: Vec<String>,
}

impl Report {
    /// 0 when nothing needs a human, [`ExitCode::Unhealthy`] when a problem
    /// does. Hazards alone do not set it: they are facts about the machine
    /// that hold on every run until the user changes the machine, and a
    /// status that is never 0 is a status nobody reads (D57).
    pub fn exit_code(&self) -> ExitCode {
        if self.problems.is_empty() {
            ExitCode::Ok
        } else {
            ExitCode::Unhealthy
        }
    }

    pub fn text(&self) -> String {
        render::text(self)
    }
}

/// `ricepilot doctor`, against the machine.
pub fn run(paths: &Paths, system: Option<PathBuf>) -> crate::cli::Output {
    let at = Where {
        paths: paths.clone(),
        system,
    };
    let report = diagnose(&crate::ops::look::Live, &at);
    crate::cli::Output {
        text: report.text(),
        code: report.exit_code(),
    }
}

/// A registered profile, as far as doctor could read it.
struct Prof {
    name: String,
    manifest_path: PathBuf,
    manifest: Manifest,
    root: PathBuf,
    targets: Vec<Target>,
}

/// Run every check. Never fails: a check that cannot read what it needs
/// reports that as its finding.
pub fn diagnose(look: &dyn Look, at: &Where) -> Report {
    let mut r = Report::default();
    let p = &at.paths;

    journal_in_flight(look, p, &mut r);
    let profiles = profiles(look, p, &mut r);
    let ledger = ledger(look, p, &mut r);
    let generation = generation(look, p, &mut r);
    let live = generation
        .as_ref()
        .map(|g| g.profile.clone())
        .filter(|name| profiles.iter().any(|q| &q.name == name));

    if let Some(l) = &ledger {
        owned_links(look, p, l, &profiles, &mut r);
        manifests_agree_with_ledger(p, l, &profiles, &mut r);
    }
    adopted_then_emptied(look, p, ledger.as_ref(), &profiles, &mut r);
    sources(look, p, ledger.as_ref(), &profiles, &mut r);
    drift(look, p, &profiles, live.as_deref(), &mut r);
    rescue(look, p, generation.as_ref(), &mut r);
    requires(look, &profiles, live.as_deref(), &mut r);
    attic(look, p, &mut r);
    verify_copies(look, p, &mut r);
    interrupted_gc(look, p, &mut r);
    hypr_session(look, p, ledger.as_ref(), &profiles, &mut r);
    caelestia_cli(p, &profiles, &mut r);
    match &at.system {
        Some(sys) => {
            snapper(look, sys, &mut r);
            theme_daemons(look, sys, &mut r);
        }
        None => r.notes.push(
            "running theme daemons (/proc) and snapper's configuration (/etc/snapper) were \
             not checked: this is the test sandbox (D56)"
                .into(),
        ),
    }
    r
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The message of an error, without the `refusing:` framing that suits a
/// command declining and does not suit a report of what was found.
fn said(e: &Error) -> String {
    match e {
        Error::Refused { why, path, .. } => format!("{why} ({})", path.display()),
        Error::Manifest { detail, .. } => detail.clone(),
        other => other.to_string(),
    }
}

/// A path as one shell word: bare when it is made only of characters no
/// shell treats specially, single-quoted otherwise.
fn sh(p: &Path) -> String {
    let s = p.to_string_lossy();
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+@%=:,".contains(c));
    if plain {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `~/…` for a path under home, as a manifest spells a destination.
fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// The name the printed commands move a thing to: `<dest>.set-aside`, or
/// `<dest>.set-aside-2`, `-3`, … when that is taken now. A sibling, so the
/// move stays on one filesystem and inside one directory.
///
/// Free when this report was made is not free when the command is pasted,
/// so the command is always `mv -nT` as well (D70): if the name has been
/// taken by then, nothing moves and nothing is replaced, and the `plan`
/// printed after it still refuses the occupied path.
fn set_aside(look: &dyn Look, dest: &Path) -> PathBuf {
    let named = |n: u32| {
        let mut s = dest.as_os_str().to_os_string();
        s.push(".set-aside");
        if n > 1 {
            s.push(format!("-{n}"));
        }
        PathBuf::from(s)
    };
    (1..=99)
        .map(named)
        .find(|q| matches!(look.lstat_or_absent(q), Ok(None)))
        .unwrap_or_else(|| named(1))
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Apparent size and file count of a tree, never following a symlink. A
/// directory's own size is left out: it differs between filesystems and says
/// nothing about what is in it.
fn tree_size(look: &dyn Look, path: &Path) -> crate::Result<(u64, usize)> {
    let Some(m) = look.lstat_or_absent(path)? else {
        return Ok((0, 0));
    };
    match m.kind {
        Kind::Dir => {
            let mut total = (0, 0);
            for name in look.list_dir(path)? {
                let (b, n) = tree_size(look, &path.join(name))?;
                total.0 += b;
                total.1 += n;
            }
            Ok(total)
        }
        Kind::File => Ok((look.size_of(path)?, 1)),
        Kind::Symlink | Kind::Other => Ok((0, 1)),
    }
}

fn unreadable(path: &Path, what: &str, e: &Error) -> Finding {
    Finding {
        path: path.to_path_buf(),
        what: format!("{what} cannot be read"),
        rule: "SAFETY.md R7 (report what was observed)".into(),
        detail: vec![format!("{}.", said(e))],
        run: vec![format!("ls -la {}", sh(path))],
    }
}

// ---------------------------------------------------------------------------
// ricepilot's own state
// ---------------------------------------------------------------------------

fn journal_in_flight(look: &dyn Look, p: &Paths, r: &mut Report) {
    let path = p.journal_path();
    match look.lstat_or_absent(&path) {
        Ok(None) => r.healthy.push("no interrupted operation".into()),
        Ok(Some(_)) => {
            let parsed = look
                .slurp(&path)
                .and_then(|t| crate::journal::parse(&t, &path));
            let mut detail = Vec::new();
            match &parsed {
                Ok(j) => {
                    let what = if j.adopt.is_empty() {
                        "a switch or a rollback"
                    } else {
                        "an adopt"
                    };
                    let mut s = format!(
                        "{what} into profile `{}` (id {}) wrote this journal and did not\n\
                         retire it, so it may have stopped part way. it touches:",
                        j.profile, j.id
                    );
                    let dests = j
                        .entries
                        .iter()
                        .map(|e| &e.dest)
                        .chain(j.retire.iter().map(|e| &e.dest))
                        .chain(j.adopt.iter().map(|e| &e.dest));
                    for d in dests {
                        s.push_str(&format!("\n  {}", d.display()));
                    }
                    detail.push(s);
                }
                Err(e) => detail.push(format!("it does not parse: {}.", said(e))),
            }
            detail.push(
                "if another ricepilot is running right now, this is its journal: let it\n\
                 finish and run doctor again. otherwise `recover` reads every destination\n\
                 and finishes or undoes the operation as a whole, never half of it. until\n\
                 then, what follows may describe a half-finished state."
                    .into(),
            );
            r.problems.push(Finding {
                path,
                what: "an operation was interrupted and has not been recovered".into(),
                rule: "SAFETY.md R5 (never half-applied); DESIGN.md §7, rung 2".into(),
                detail,
                run: vec![
                    "# see what recovery would do; this changes nothing".into(),
                    "ricepilot recover".into(),
                    "ricepilot recover --commit".into(),
                ],
            });
        }
        Err(e) => r.problems.push(unreadable(&path, "the journal", &e)),
    }
}

fn profiles(look: &dyn Look, p: &Paths, r: &mut Report) -> Vec<Prof> {
    let dir = p.profiles_dir();
    let names = match look
        .lstat_or_absent(&dir)
        .and_then(|m| m.map(|_| look.list_dir(&dir)).transpose())
    {
        Ok(Some(n)) => n,
        Ok(None) => return Vec::new(),
        Err(e) => {
            r.problems
                .push(unreadable(&dir, "the profiles directory", &e));
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for name in names {
        let name = name.to_string_lossy().into_owned();
        let manifest_path = p.manifest_path(&name);
        let loaded = look
            .lstat_or_absent(&manifest_path)
            .and_then(|m| m.map(|_| look.slurp(&manifest_path)).transpose())
            .and_then(|t| t.map(|t| crate::manifest::parse(&t)).transpose());
        let why = match loaded {
            Ok(Some(m)) if m.name == name => {
                let pdir = p.profile_dir(&name);
                out.push(Prof {
                    root: m.root_dir(&pdir, &p.home),
                    targets: m.targets(&pdir, &p.home),
                    name,
                    manifest_path,
                    manifest: m,
                });
                continue;
            }
            Ok(Some(m)) => format!(
                "it declares name = {:?} but lives in a directory called {name:?}.",
                m.name
            ),
            Ok(None) => "the profile directory has no profile.toml.".into(),
            Err(e) => format!("{}.", said(&e)),
        };
        r.problems.push(Finding {
            path: manifest_path,
            what: format!("profile `{name}` cannot be read"),
            rule: "D8 (a malformed manifest is refused, never guessed at)".into(),
            detail: vec![
                why,
                format!(
                    "every command that loads all the profiles — `list`, `status`, `plan`,\n\
                     `switch` — refuses until it is fixed. doctor skipped `{name}` and\n\
                     checked the rest."
                ),
            ],
            run: vec![format!("ricepilot show {name}")],
        });
    }
    out
}

fn ledger(look: &dyn Look, p: &Paths, r: &mut Report) -> Option<crate::ledger::Ledger> {
    let path = p.ledger_path();
    let read = look.lstat_or_absent(&path).and_then(|m| match m {
        None => Ok(crate::ledger::Ledger::default()),
        Some(_) => look
            .slurp(&path)
            .and_then(|t| crate::ledger::parse(&t, &path)),
    });
    match read {
        Ok(l) => Some(l),
        Err(e) => {
            r.problems.push(Finding {
                path: path.clone(),
                what: "the ledger cannot be read".into(),
                rule: "SAFETY.md, the ownership predicate (its fact 3 is the ledger)".into(),
                detail: vec![
                    format!("{}.", said(&e)),
                    "until it parses, every switch refuses the paths ricepilot owns as\n\
                     unowned. doctor did not check the owned links."
                        .into(),
                ],
                run: vec![format!("less {}", sh(&path))],
            });
            None
        }
    }
}

/// The current generation, if there is one and it can be read.
fn generation(
    look: &dyn Look,
    p: &Paths,
    r: &mut Report,
) -> Option<crate::generations::Generation> {
    let state = &p.state;
    let ptr = crate::generations::current_path(state);
    let meta = match look.lstat_or_absent(&ptr) {
        Ok(None) => return None,
        Ok(Some(m)) => m,
        Err(e) => {
            r.problems
                .push(unreadable(&ptr, "the current-generation pointer", &e));
            return None;
        }
    };
    if meta.kind != Kind::File {
        r.problems.push(Finding {
            path: ptr.clone(),
            what: "the current-generation pointer is not a regular file".into(),
            rule: "D35 (the pointer is a real file, so a bad switch cannot damage it)".into(),
            detail: vec![
                "ricepilot writes it as a regular file and refuses to read anything else,\n\
                 so `status`, `rollback` and the rescue script cannot say where this\n\
                 machine is."
                    .into(),
            ],
            run: vec![format!("ls -la {}", sh(&ptr))],
        });
        return None;
    }
    let loaded = look
        .slurp(&ptr)
        .and_then(|t| crate::generations::parse_current(&t, &ptr))
        .and_then(|id| {
            let g = crate::generations::path(state, id);
            look.slurp(&g)
                .and_then(|t| crate::generations::parse(&t, &g, id))
        });
    match loaded {
        Ok(g) => Some(g),
        Err(e) => {
            r.problems.push(Finding {
                path: ptr,
                what: "the current generation cannot be read".into(),
                rule: "D35".into(),
                detail: vec![format!("{}.", said(&e))],
                run: vec![format!("ls -la {}", sh(&crate::generations::dir(state)))],
            });
            None
        }
    }
}

/// Are the links the ledger records still the links it recorded?
fn owned_links(
    look: &dyn Look,
    p: &Paths,
    ledger: &crate::ledger::Ledger,
    profiles: &[Prof],
    r: &mut Report,
) {
    let mut intact = 0usize;
    for e in &ledger.entries {
        let dest = &e.dest;
        let registered = profiles.iter().any(|q| q.name == e.profile);
        let relink = |run: &mut Vec<String>| {
            if registered {
                run.push(format!("ricepilot plan {}", e.profile));
                run.push(format!("ricepilot switch {} --commit", e.profile));
            } else {
                run.push("ricepilot list".into());
            }
        };
        let aside = |why: &str| {
            let mut run = vec![
                format!("# {why}"),
                format!("mv -nT {} {}", sh(dest), sh(&set_aside(look, dest))),
            ];
            relink(&mut run);
            run
        };
        let recorded = format!(
            "the ledger recorded a link here to\n  {}\nfor profile `{}`.",
            e.target.display(),
            e.profile
        );
        let meta = match look.lstat_or_absent(dest) {
            Ok(m) => m,
            Err(err) => {
                r.problems.push(unreadable(dest, "an owned link", &err));
                continue;
            }
        };
        let Some(meta) = meta else {
            let mut run = vec!["# ricepilot sees an empty path, and links it again".into()];
            relink(&mut run);
            r.problems.push(Finding {
                path: dest.clone(),
                what: "is gone: nothing is where ricepilot's link was".into(),
                rule: "SAFETY.md, the ownership predicate".into(),
                detail: vec![
                    recorded,
                    "something other than ricepilot took it away: ricepilot never removes\n\
                     anything, and when it stops managing a path it says so and forgets it."
                        .into(),
                ],
                run,
            });
            continue;
        };
        match meta.kind {
            Kind::Dir => {
                let mut run = vec![
                    "# what is in it, against the tree the link pointed into".into(),
                    format!("diff -r {} {}", sh(&e.target), sh(dest)),
                ];
                run.extend(aside(
                    "set it aside (nothing is lost), then put the link back",
                ));
                r.problems.push(Finding {
                    path: dest.clone(),
                    what: "is a real directory, not the link ricepilot recorded".into(),
                    rule: "SAFETY.md, the ownership predicate; \
                           NOT-POSSIBLE.md#real-dir-at-managed-dest"
                        .into(),
                    detail: vec![
                        recorded,
                        "this is what a rice installer does — caelestia's install.fish and\n\
                         `caelestia install` turn links back into directories — and whatever\n\
                         it wrote is in there. every switch refuses this path until it is\n\
                         resolved, because re-linking it would strand that."
                            .into(),
                    ],
                    run,
                });
            }
            Kind::File | Kind::Other => {
                r.problems.push(Finding {
                    path: dest.clone(),
                    what: "is no longer a link: something put a file here".into(),
                    rule: "SAFETY.md, the ownership predicate".into(),
                    detail: vec![
                        recorded,
                        "every switch refuses this path until it is set aside.".into(),
                    ],
                    run: aside("set it aside (nothing is lost), then put the link back"),
                });
            }
            Kind::Symlink => {
                let now = match look.readlink(dest) {
                    Ok(t) => t,
                    Err(err) => {
                        r.problems.push(unreadable(dest, "an owned link", &err));
                        continue;
                    }
                };
                let again = "set it aside (nothing is lost), then let ricepilot link it again";
                if now != e.target {
                    let into = profiles
                        .iter()
                        .find(|q| now.starts_with(&q.root))
                        .map(|q| format!("\nwhich is inside profile `{}`'s tree", q.name))
                        .unwrap_or_default();
                    r.problems.push(Finding {
                        path: dest.clone(),
                        what: "points somewhere other than where ricepilot linked it".into(),
                        rule: "SAFETY.md, the ownership predicate (fact 3: its target must \
                               match the ledger)"
                            .into(),
                        detail: vec![
                            format!("it points at\n  {}{into}", now.display()),
                            recorded,
                            "ricepilot did not re-point it, so it is someone else's link now,\n\
                             and every switch refuses it."
                                .into(),
                        ],
                        run: aside(again),
                    });
                } else if (meta.dev, meta.ino) != (e.dev, e.ino) {
                    r.problems.push(Finding {
                        path: dest.clone(),
                        what: "was replaced by a link that only looks like ricepilot's".into(),
                        rule: "SAFETY.md, the ownership predicate (fact 3: its (dev, ino) must \
                               match the ledger)"
                            .into(),
                        detail: vec![
                            format!(
                                "it points where the ledger says, but it is not the inode\n\
                                 ricepilot made:\n  \
                                 now       dev {} ino {}\n  \
                                 recorded  dev {} ino {}",
                                meta.dev, meta.ino, e.dev, e.ino
                            ),
                            "something removed ricepilot's link and made its own. ricepilot\n\
                             cannot know what that something will do next, so every switch\n\
                             refuses it."
                                .into(),
                        ],
                        run: aside(again),
                    });
                } else if !profiles.iter().any(|q| now.starts_with(&q.root)) {
                    r.problems.push(Finding {
                        path: dest.clone(),
                        what: "points into a tree that is no longer a registered profile".into(),
                        rule: "SAFETY.md, the ownership predicate (fact 2: its target must be \
                               inside a registered profile root)"
                            .into(),
                        detail: vec![
                            recorded,
                            "no registered profile's root contains that any more, so every\n\
                             switch treats this link as foreign."
                                .into(),
                        ],
                        run: vec!["ricepilot list".into()],
                    });
                } else if !look.resolves(dest).unwrap_or(false) {
                    r.problems.push(Finding {
                        path: dest.clone(),
                        what: "is ricepilot's link, and it dangles".into(),
                        rule: "D42 (a managed link must not dangle)".into(),
                        detail: vec![
                            recorded,
                            format!(
                                "nothing is there now, so what reads {} at the next\n\
                                 login finds nothing.",
                                tilde(dest, &p.home)
                            ),
                        ],
                        run: vec![
                            "# what going back a generation would do; this changes no link".into(),
                            "ricepilot rollback".into(),
                        ],
                    });
                } else {
                    intact += 1;
                }
            }
        }
    }
    if ledger.entries.is_empty() {
        r.healthy.push("no owned links".into());
    } else if intact > 0 {
        r.healthy.push(format!(
            "{} intact",
            plural(intact, "owned link", "owned links")
        ));
    }
}

/// D51: the ledger and each profile's manifest must agree, or the next
/// switch into that profile retires or re-points a link nobody asked it to.
fn manifests_agree_with_ledger(
    p: &Paths,
    ledger: &crate::ledger::Ledger,
    profiles: &[Prof],
    r: &mut Report,
) {
    let mut disagreements = 0usize;
    for e in &ledger.entries {
        let Some(q) = profiles.iter().find(|q| q.name == e.profile) else {
            continue;
        };
        let dest = tilde(&e.dest, &p.home);
        let declared = q
            .manifest
            .paths
            .iter()
            .find(|pe| crate::manifest::expand_home(&pe.dest, &p.home) == e.dest);
        let src = e
            .target
            .strip_prefix(&q.root)
            .map(|s| s.display().to_string())
            .unwrap_or_else(|_| "<the directory inside the profile>".into());
        let check = vec![
            "# see what the next switch would do; this changes no link".to_string(),
            format!("ricepilot plan {}", q.name),
        ];
        let owned = format!(
            "the ledger records the link\n  {} -> {}\nas profile `{}`'s.",
            e.dest.display(),
            e.target.display(),
            q.name
        );
        let finding = match declared {
            None => Finding {
                path: q.manifest_path.clone(),
                what: format!("does not declare {dest}, which profile `{}` owns", q.name),
                rule: "D51 (a profile's manifest declares every path it owns)".into(),
                detail: vec![
                    owned,
                    format!(
                        "the manifest has no [[path]] for it — typically because the block\n\
                         `ricepilot adopt` appended was edited out — so the next\n\
                         `ricepilot switch {}` would retire the link into the attic and\n\
                         leave the path empty. put this back at the end of the manifest:",
                        q.name
                    ),
                    format!(
                        "  [[path]]\n  dest       = \"{dest}\"\n  src        = \"{src}\"\n  \
                         kind       = \"dir-link\"\n  activation = \"relogin\""
                    ),
                ],
                run: vec![
                    "# then this must say there is nothing to do for it".into(),
                    format!("ricepilot plan {}", q.name),
                ],
            },
            Some(pe)
                if pe.kind != crate::manifest::Kind::DirLink
                    || pe.activation != crate::manifest::Activation::Relogin =>
            {
                Finding {
                    path: q.manifest_path.clone(),
                    what: format!(
                        "declares {dest} as kind = {:?}, activation = {:?}, which is not a link",
                        pe.kind.as_str(),
                        pe.activation.as_str()
                    ),
                    rule: "D51 (a profile's manifest declares every path it owns)".into(),
                    detail: vec![
                        owned,
                        format!(
                            "only kind = \"dir-link\" with activation = \"relogin\" is a link in\n\
                             v1, so the next `ricepilot switch {}` would retire it into the\n\
                             attic and leave the path empty.",
                            q.name
                        ),
                    ],
                    run: check,
                }
            }
            Some(pe) if q.root.join(&pe.src) != e.target => Finding {
                path: q.manifest_path.clone(),
                what: format!(
                    "says {dest} comes from src = {:?}, and the link points elsewhere",
                    pe.src.display().to_string()
                ),
                rule: "D51 (a profile's manifest and the ledger agree)".into(),
                detail: vec![
                    owned,
                    format!(
                        "the next `ricepilot switch {}` re-points it at\n  {}\n\
                         if that is not what you meant, set src = {src:?} again.",
                        q.name,
                        q.root.join(&pe.src).display()
                    ),
                ],
                run: check,
            },
            Some(_) => continue,
        };
        disagreements += 1;
        r.problems.push(finding);
    }
    if disagreements == 0 && !ledger.entries.is_empty() {
        r.healthy.push("the ledger and the manifests agree".into());
    }
}

/// D49: an adopt followed by a rollback leaves the destination empty and the
/// user's own directory in the attic. Found from the retired journals, which
/// are the only record of where each adopted directory went.
fn adopted_then_emptied(
    look: &dyn Look,
    p: &Paths,
    ledger: Option<&crate::ledger::Ledger>,
    profiles: &[Prof],
    r: &mut Report,
) {
    let dir = p.journal_dir();
    let names = match look
        .lstat_or_absent(&dir)
        .and_then(|m| m.map(|_| look.list_dir(&dir)).transpose())
    {
        Ok(Some(n)) => n,
        Ok(None) => return,
        Err(e) => {
            r.notes.push(format!(
                "the retired journals in {} could not be listed ({}), so doctor could not look \
                 for adopted directories a rollback left in the attic",
                dir.display(),
                said(&e)
            ));
            return;
        }
    };

    // The latest adopt of each destination: journals are named by time
    // (D40), so a later one replaces an earlier one.
    struct Adopted {
        id: String,
        profile: String,
        original: PathBuf,
        copy: PathBuf,
    }
    let mut adopted: Vec<(PathBuf, Adopted)> = Vec::new();
    for name in names {
        let name = name.to_string_lossy().into_owned();
        if !(name.starts_with("done-") && name.ends_with(".toml")) {
            continue;
        }
        let path = dir.join(&name);
        let j = match look
            .slurp(&path)
            .and_then(|t| crate::journal::parse(&t, &path))
        {
            Ok(j) => j,
            Err(e) => {
                r.notes.push(format!(
                    "the retired journal {} does not parse ({})",
                    path.display(),
                    said(&e)
                ));
                continue;
            }
        };
        for a in &j.adopt {
            let entry = Adopted {
                id: j.id.clone(),
                profile: j.profile.clone(),
                original: j.attic.join(&a.attic_rel),
                copy: a.new_target.clone(),
            };
            match adopted.iter_mut().find(|(d, _)| d == &a.dest) {
                Some(slot) => slot.1 = entry,
                None => adopted.push((a.dest.clone(), entry)),
            }
        }
    }

    for (dest, a) in adopted {
        if ledger.is_some_and(|l| l.entries.iter().any(|e| e.dest == dest)) {
            continue;
        }
        if !matches!(look.lstat_or_absent(&dest), Ok(None)) {
            continue;
        }
        let is_dir =
            |q: &Path| matches!(look.lstat_or_absent(q), Ok(Some(m)) if m.kind == Kind::Dir);
        let declared = profiles
            .iter()
            .find(|q| q.name == a.profile)
            .is_some_and(|q| q.targets.iter().any(|t| t.dest == dest && t.src == a.copy));
        let relink = declared && is_dir(&a.copy);
        let original_there = is_dir(&a.original);

        let mut detail = vec![format!(
            "`ricepilot adopt` (id {}) moved your directory from here to the attic\n\
             and linked this path into profile `{}`. a rollback since then retired\n\
             that link, which leaves the path empty; it does not put the directory\n\
             back.",
            a.id, a.profile
        )];
        let mut run = Vec::new();
        if original_there {
            detail.push(format!(
                "your directory is still at\n  {}",
                a.original.display()
            ));
            run.push("# your own directory, back where it was".into());
            run.push(format!("mv -nT {} {}", sh(&a.original), sh(&dest)));
            if relink {
                detail.push(format!(
                    "moved back, it is a real directory again. profile `{}` still declares\n\
                     this path, so a later `ricepilot switch {}` refuses it rather than\n\
                     re-linking over it — the safe way round.",
                    a.profile, a.profile
                ));
            }
        } else {
            detail.push(format!(
                "and it is no longer at\n  {}",
                a.original.display()
            ));
        }
        if relink {
            run.push(format!(
                "# or, instead, link the copy in profile `{}` again",
                a.profile
            ));
            run.push(format!("ricepilot switch {} --commit", a.profile));
        }
        if run.is_empty() {
            run.push(format!("ls -la {}", sh(&p.attic_dir())));
        }
        r.problems.push(Finding {
            path: dest.clone(),
            what: if original_there {
                "is empty, and the directory you adopted from here is in the attic".into()
            } else {
                "is empty, and the directory you adopted from here is not in the attic".into()
            },
            rule: "D49 (`rollback` does not undo an `adopt`)".into(),
            detail,
            run,
        });
    }
}

/// Every declared source exists, and a tree linked at `~/.config/hypr`
/// has an entry file. Also says which Hyprland configs cannot be, or were
/// not, checked.
fn sources(
    look: &dyn Look,
    p: &Paths,
    ledger: Option<&crate::ledger::Ledger>,
    profiles: &[Prof],
    r: &mut Report,
) {
    let hypr = crate::hyprverify::hypr_dest(&p.home);
    let mut entry_ok = 0usize;
    for q in profiles {
        for t in &q.targets {
            // An owned link into a missing source is the owned-link check's to
            // report; saying it twice would bury the command.
            let owned_into = ledger.is_some_and(|l| {
                l.entries
                    .iter()
                    .any(|e| e.dest == t.dest && e.target == t.src)
            });
            let m = match look.lstat_or_absent(&t.src) {
                Ok(m) => m,
                Err(e) => {
                    r.problems.push(unreadable(&t.src, "a declared source", &e));
                    continue;
                }
            };
            match m {
                Some(m) if m.kind == Kind::Dir => {}
                _ if owned_into => continue,
                m => {
                    r.problems.push(Finding {
                        path: t.src.clone(),
                        what: format!(
                            "{}, and profile `{}` links {} to it",
                            if m.is_none() {
                                "does not exist"
                            } else {
                                "is not a directory"
                            },
                            q.name,
                            tilde(&t.dest, &p.home)
                        ),
                        rule: "D42 (a switch refuses a link that would dangle)".into(),
                        detail: vec![format!(
                            "`ricepilot switch {}` refuses until it is a directory.",
                            q.name
                        )],
                        run: vec![format!("ricepilot show {}", q.name)],
                    });
                    continue;
                }
            }
            if t.dest != hypr {
                continue;
            }
            let lua = t.src.join(crate::hyprverify::LUA_ENTRY);
            let conf = t.src.join(crate::hyprverify::CONF_ENTRY);
            let has = |f: &Path| matches!(look.lstat_or_absent(f), Ok(Some(_)));
            if has(&lua) {
                entry_ok += 1;
                let mut note = format!(
                    "profile `{}`: {} is a Lua config, which nothing can check before a login \
                     (NOT-POSSIBLE.md#verify-lua-config)",
                    q.name,
                    lua.display()
                );
                if q.manifest.hypr_dialect.as_deref() == Some("conf") {
                    note.push_str(
                        "; the manifest says hypr_dialect = \"conf\", but Hyprland loads \
                         hyprland.lua whenever it exists",
                    );
                }
                r.notes.push(note);
            } else if has(&conf) {
                entry_ok += 1;
                r.notes.push(format!(
                    "profile `{}`: {} was not checked by doctor, which never builds the scratch \
                     copy the check needs; `ricepilot plan {}` runs the sandboxed verify-config",
                    q.name,
                    conf.display(),
                    q.name
                ));
            } else {
                r.problems.push(Finding {
                    path: t.src.clone(),
                    what: format!(
                        "has neither {} nor {}, and profile `{}` links it at {}",
                        crate::hyprverify::CONF_ENTRY,
                        crate::hyprverify::LUA_ENTRY,
                        q.name,
                        tilde(&hypr, &p.home)
                    ),
                    rule: "SAFETY.md R3 (nothing writes into a profile) — Hyprland would".into(),
                    detail: vec![
                        "a Hyprland that finds no config writes a default hyprland.conf where\n\
                         it looked — through the link, into this profile's tree — and starts\n\
                         with that instead of your rice."
                            .into(),
                        format!(
                            "give the tree an entry file, or take {} out of\n  {}",
                            tilde(&hypr, &p.home),
                            q.manifest_path.display()
                        ),
                    ],
                    run: vec![format!("ls -la {}", sh(&t.src))],
                });
            }
        }
    }
    if entry_ok > 0 {
        r.healthy.push(format!(
            "{} an entry file",
            plural(entry_ok, "hypr tree has", "hypr trees have")
        ));
    }
}

/// Has each profile's tree changed since ricepilot recorded it?
fn drift(look: &dyn Look, p: &Paths, profiles: &[Prof], live: Option<&str>, r: &mut Report) {
    const SHOWN: usize = 8;
    let mut clean = 0usize;
    for q in profiles {
        let path = crate::verify::manifest_path(&p.state, &q.name);
        let recorded = match look.lstat_or_absent(&path) {
            Ok(None) => {
                r.notes.push(format!(
                    "profile `{}`: no manifest has been recorded for it yet, so its tree was not \
                     compared with anything",
                    q.name
                ));
                continue;
            }
            Ok(Some(_)) => look
                .slurp(&path)
                .and_then(|t| crate::verify::parse(&t, &path)),
            Err(e) => Err(e),
        };
        let recorded = match recorded {
            Ok(m) => m,
            Err(e) => {
                r.problems
                    .push(unreadable(&path, "a recorded tree manifest", &e));
                continue;
            }
        };
        let now = match crate::verify::build_via(
            look,
            &q.name,
            &q.root,
            &q.manifest.volatile,
            recorded.created.clone(),
        ) {
            Ok(m) => m,
            Err(e) => {
                r.problems.push(unreadable(&q.root, "a profile's tree", &e));
                continue;
            }
        };
        let diffs: Vec<_> = crate::verify::compare(&recorded, &now)
            .into_iter()
            .filter(|d| d.is_substantive())
            .collect();
        if diffs.is_empty() {
            clean += 1;
            continue;
        }
        let mut listed = format!(
            "{} changed since ricepilot recorded it at {}:",
            plural(diffs.len(), "path has", "paths have"),
            recorded.created
        );
        for d in diffs.iter().take(SHOWN) {
            listed.push_str(&format!("\n  {d}"));
        }
        if diffs.len() > SHOWN {
            listed.push_str(&format!("\n  … and {} more", diffs.len() - SHOWN));
        }
        let mut detail = vec![listed];
        if live == Some(q.name.as_str()) {
            detail.push(
                "this is the profile linked now, so these changes are what the next login\n\
                 loads."
                    .into(),
            );
        }
        detail.push(
            "ricepilot never writes into a profile; an app, a script or an editor did.\n\
             paths an app rewrites on its own belong in `volatile`."
                .into(),
        );
        r.problems.push(Finding {
            path: q.root.clone(),
            what: format!(
                "profile `{}`'s tree has changed since it was recorded",
                q.name
            ),
            rule: "SAFETY.md R7; D34".into(),
            detail,
            run: vec![format!("ricepilot verify {}", q.name)],
        });
    }
    if clean > 0 {
        r.healthy.push(format!(
            "{} {} recorded manifest",
            plural(clean, "profile tree", "profile trees"),
            if clean == 1 {
                "matches its"
            } else {
                "match their"
            }
        ));
    }
}

/// `rescue.sh`: there when it should be, the script a switch would have
/// written for the generation before the current one, and parseable.
fn rescue(
    look: &dyn Look,
    p: &Paths,
    current: Option<&crate::generations::Generation>,
    r: &mut Report,
) {
    let path = crate::rescue::path(&p.state);
    let rung1 = vec![
        "# rung 1 of the ladder still works; see what it would do".to_string(),
        "ricepilot rollback".to_string(),
        "# the next `switch` or `rollback --commit` writes the script again".to_string(),
    ];
    let Some(current) = current.filter(|g| g.id > 0) else {
        match look.lstat_or_absent(&path) {
            Ok(None) => r
                .healthy
                .push("no switch yet, so no rescue script is needed".into()),
            _ => r.notes.push(format!(
                "{} exists, but there is no current generation for it to go back from",
                path.display()
            )),
        }
        return;
    };
    let before = current.id - 1;
    let rule = "DESIGN.md §7, rung 3; D37".to_string();
    let meta = match look.lstat_or_absent(&path) {
        Ok(m) => m,
        Err(e) => {
            r.problems.push(unreadable(&path, "the rescue script", &e));
            return;
        }
    };
    let Some(meta) = meta else {
        r.problems.push(Finding {
            path,
            what: "is missing".into(),
            rule,
            detail: vec![format!(
                "generation {:04} is current, so a script should be here that restores\n\
                 generation {before:04} from a TTY without ricepilot. a switch that could not\n\
                 write it said so when it finished.",
                current.id
            )],
            run: rung1,
        });
        return;
    };
    if meta.kind != Kind::File {
        r.problems.push(Finding {
            path,
            what: "is not a regular file".into(),
            rule,
            detail: vec![
                "ricepilot writes it as a real file so that a switch that goes wrong cannot\n\
                 take it with it. something replaced it."
                    .into(),
            ],
            run: rung1,
        });
        return;
    }
    let text = match look.slurp(&path) {
        Ok(t) => t,
        Err(e) => {
            r.problems.push(unreadable(&path, "the rescue script", &e));
            return;
        }
    };

    let mut facts = Vec::new();
    match look.sh_syntax(&text) {
        Ok(ran) if ran.success() => {}
        Ok(ran) => facts.push(format!(
            "`sh -n` rejects it:\n  {}",
            ran.stderr.lines().next().unwrap_or("").trim()
        )),
        Err(e) => r.notes.push(format!(
            "{}: `sh -n` could not be run ({})",
            path.display(),
            said(&e)
        )),
    }

    let restores = text.lines().find_map(|l| {
        l.strip_prefix("# ricepilot rescue script — restores generation ")
            .map(|rest| rest.trim_end_matches('.').to_string())
    });
    let prev_path = crate::generations::path(&p.state, before);
    let expected = look
        .slurp(&prev_path)
        .and_then(|t| crate::generations::parse(&t, &prev_path, before))
        .and_then(|g| {
            crate::rescue::Binaries::locate_via(look).map(|b| {
                crate::rescue::script(&g, &b, &crate::rescue::rescue_attic(&p.state, before))
            })
        });
    match expected {
        Ok(want) if want == text => {}
        Ok(_) => facts.push(match restores {
            Some(n) if n == format!("{before:04}") => format!(
                "it says it restores generation {before:04}, the one before the current\n\
                 {:04}, but it is not the script ricepilot would write for it: it was\n\
                 edited, or generation {before:04} was.",
                current.id
            ),
            Some(n) => format!(
                "it restores generation {n}, but the generation before the current {:04}\n\
                 is {before:04}: it is stale.",
                current.id
            ),
            None => "it is not a script ricepilot wrote.".into(),
        }),
        Err(e) => r.notes.push(format!(
            "{}: the script ricepilot would write for generation {before:04} could not be \
             worked out ({}), so it was not compared",
            path.display(),
            said(&e)
        )),
    }

    if facts.is_empty() {
        r.healthy.push(format!(
            "rescue.sh restores generation {before:04} and parses"
        ));
        return;
    }
    facts.push(
        "it is rung 3 of the recovery ladder, the one for a TTY when nothing else\n\
         works. do not rely on it as it is."
            .into(),
    );
    let mut run = vec![format!("sh -n {}", sh(&path))];
    run.extend(rung1);
    r.problems.push(Finding {
        path,
        what: "cannot be relied on".into(),
        rule,
        detail: facts,
        run,
    });
}

fn requires(look: &dyn Look, profiles: &[Prof], live: Option<&str>, r: &mut Report) {
    let mut satisfied = 0usize;
    for q in profiles {
        if q.manifest.requires.is_empty() {
            continue;
        }
        match crate::requires::missing_via(look, &q.manifest.requires) {
            Ok(m) if m.is_empty() => satisfied += 1,
            Ok(m) => {
                let paru = format!("paru -S --needed {}", m.join(" "));
                if live == Some(q.name.as_str()) {
                    r.problems.push(Finding {
                        path: q.manifest_path.clone(),
                        what: format!(
                            "requires {}, which {} not installed, and profile `{}` is the one \
                             linked now",
                            m.join(", "),
                            if m.len() == 1 { "is" } else { "are" },
                            q.name
                        ),
                        rule: "D53; NOT-POSSIBLE.md#installing-packages".into(),
                        detail: vec!["the next login starts a rice whose programs are missing.\n\
                             ricepilot never installs anything; this is the command to run."
                            .into()],
                        run: vec![paru],
                    });
                } else {
                    r.notes.push(format!(
                        "profile `{}` requires {}, which {} not installed; `ricepilot switch \
                         {}` refuses until: {paru}",
                        q.name,
                        m.join(", "),
                        if m.len() == 1 { "is" } else { "are" },
                        q.name
                    ));
                }
            }
            Err(e) => r.notes.push(format!(
                "profile `{}`: its requires could not be checked ({})",
                q.name,
                said(&e)
            )),
        }
    }
    if satisfied > 0 {
        r.healthy.push(format!(
            "requires installed for {}",
            plural(satisfied, "profile", "profiles")
        ));
    }
}

/// An attic larger than this, in total, is worth a hazard.
const ATTIC_LARGE: u64 = 1 << 30;
/// More attic entries than this are worth a hazard.
const ATTIC_MANY: usize = 100;
/// More verify-config scratch copies than this are worth a hazard.
const VERIFY_MANY: usize = 20;

fn attic(look: &dyn Look, p: &Paths, r: &mut Report) {
    let dir = p.attic_dir();
    let entries = match sized_entries(look, &dir) {
        Ok(e) => e,
        Err(e) => {
            r.notes.push(format!(
                "the attic {} could not be measured ({})",
                dir.display(),
                said(&e)
            ));
            return;
        }
    };
    let Some(newest) = entries.last() else {
        r.healthy.push("attic empty".into());
        return;
    };
    let total: u64 = entries.iter().map(|e| e.1).sum();
    if total <= ATTIC_LARGE && entries.len() <= ATTIC_MANY {
        r.healthy.push(format!(
            "attic: {}, {} (the newest, {}, {})",
            plural(entries.len(), "entry", "entries"),
            human(total),
            newest.0,
            human(newest.1)
        ));
        return;
    }
    let mut largest = entries.clone();
    largest.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut listed = "everything a switch, rollback or adopt displaced is here: nothing but\n\
                      `gc` removes anything, and gc only what you name. the largest:"
        .to_string();
    for (name, bytes, files) in largest.iter().take(5) {
        listed.push_str(&format!(
            "\n  {name}  {}  ({})",
            human(*bytes),
            plural(*files, "item", "items")
        ));
    }
    r.hazards.push(Finding {
        path: dir.clone(),
        what: format!(
            "has grown to {} in {}; the newest, {}, holds {}",
            human(total),
            plural(entries.len(), "entry", "entries"),
            newest.0,
            human(newest.1)
        ),
        rule: "SAFETY.md R2 (displaced; only `gc` removes, D61)".into(),
        detail: vec![
            listed,
            "it can hold your own directories — an adopt moves the original here (D49).\n\
             gc keeps one unless a profile holds an identical copy and the path is\n\
             ricepilot's link, and says why it keeps anything; read that before you answer."
                .into(),
        ],
        run: vec![
            format!("du -sh {}/*", sh(&dir)),
            "# lists every entry, and why each could go or is kept; removes nothing".into(),
            "ricepilot gc".into(),
            "# asks for each candidate's name to be typed back, and removes only those".into(),
            "ricepilot gc --commit".into(),
        ],
    });
}

fn verify_copies(look: &dyn Look, p: &Paths, r: &mut Report) {
    let dir = p.state.join("verify");
    let entries = match sized_entries(look, &dir) {
        Ok(e) => e,
        Err(e) => {
            r.notes.push(format!(
                "the verify-config copies in {} could not be measured ({})",
                dir.display(),
                said(&e)
            ));
            return;
        }
    };
    let total: u64 = entries.iter().map(|e| e.1).sum();
    let (Some(oldest), Some(newest)) = (entries.first(), entries.last()) else {
        r.healthy.push("no verify-config copies".into());
        return;
    };
    if entries.len() <= VERIFY_MANY {
        r.healthy.push(format!(
            "{}, {}",
            plural(entries.len(), "verify-config copy", "verify-config copies"),
            human(total)
        ));
        return;
    }
    r.hazards.push(Finding {
        path: dir.clone(),
        what: format!(
            "holds {} scratch copies, {} in all",
            entries.len(),
            human(total)
        ),
        rule: "D55 (the scratch copy stays, as the record of what was parsed)".into(),
        detail: vec![format!(
            "every `plan`, `switch` and `rollback` into a profile with a hyprland.conf\n\
             leaves one. the oldest is {}, the newest {}. only `gc` reclaims them.",
            oldest.0, newest.0
        )],
        run: vec![
            format!("du -sh {}", sh(&dir)),
            "# lists them; `--commit` asks for each one's name before it removes it".into(),
            "ricepilot gc".into(),
        ],
    });
}

/// A removal `gc` began and did not finish: what is left of an entry it had
/// already moved to `state/gc/` (D62). A problem rather than a hazard — it is
/// ricepilot's own state, half way through something, and one command
/// finishes it — though nothing on the live machine depends on it.
fn interrupted_gc(look: &dyn Look, p: &Paths, r: &mut Report) {
    let dir = p.state.join("gc");
    let entries = match sized_entries(look, &dir) {
        Ok(e) => e,
        Err(e) => {
            r.problems
                .push(unreadable(&dir, "the directory gc removes from", &e));
            return;
        }
    };
    if entries.is_empty() {
        r.healthy.push("no interrupted gc".into());
        return;
    }
    let mut listed = "the operator typed each one's name, and gc stopped part way through\n\
                      taking it apart:"
        .to_string();
    for (name, bytes, files) in &entries {
        listed.push_str(&format!(
            "\n  {name}  {}  ({} left)",
            human(*bytes),
            plural(*files, "item", "items")
        ));
    }
    r.problems.push(Finding {
        path: dir.clone(),
        what: format!(
            "holds {} gc began removing and did not finish",
            plural(entries.len(), "entry", "entries")
        ),
        rule: "D62 (gc removes only inside state/gc, and a crash leaves it there)".into(),
        detail: vec![listed],
        run: vec![
            "# lists what is left; `--commit` asks for its name again before it finishes".into(),
            "ricepilot gc".into(),
        ],
    });
}

/// `(name, bytes, files)` for each entry of `dir`, in name order — which for
/// the attic and `state/verify` is time order. An absent `dir` has none.
fn sized_entries(look: &dyn Look, dir: &Path) -> crate::Result<Vec<(String, u64, usize)>> {
    if look.lstat_or_absent(dir)?.is_none() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for name in look.list_dir(dir)? {
        let (bytes, files) = tree_size(look, &dir.join(&name))?;
        out.push((name.to_string_lossy().into_owned(), bytes, files));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// Where the directory hypr-session makes on every call belongs: the state
/// directory, not the config tree ricepilot links.
const SESSION_DIR_FIXED: &str = "\"${XDG_STATE_HOME:-$HOME/.local/state}/hypr/sessions\"";

/// `~/.local/bin/hypr-session` writing into `~/.config/hypr`, which is a
/// profile's tree once ricepilot manages that path.
fn hypr_session(
    look: &dyn Look,
    p: &Paths,
    ledger: Option<&crate::ledger::Ledger>,
    profiles: &[Prof],
    r: &mut Report,
) {
    let script = p.home.join(".local/bin/hypr-session");
    let hypr = crate::hyprverify::hypr_dest(&p.home);
    match look.lstat_or_absent(&script) {
        Ok(Some(m)) if m.kind == Kind::File => {}
        _ => return,
    }
    let managed = ledger.is_some_and(|l| l.entries.iter().any(|e| e.dest == hypr))
        || profiles
            .iter()
            .any(|q| q.targets.iter().any(|t| t.dest == hypr));
    if !managed {
        return;
    }
    let text = match look.size_of(&script) {
        Ok(n) if n <= 1 << 20 => match look.slurp(&script) {
            Ok(t) => t,
            Err(e) => {
                r.notes.push(format!(
                    "{} could not be read ({})",
                    script.display(),
                    said(&e)
                ));
                return;
            }
        },
        _ => return,
    };

    let into_config =
        |v: &str| v.contains("hypr") && (v.contains(".config") || v.contains("XDG_CONFIG_HOME"));
    let mut hit: Option<(usize, String, Option<String>)> = None;
    for (i, line) in text.lines().enumerate() {
        let body = line.trim_start();
        if body.starts_with('#') {
            continue;
        }
        let assignment = body
            .strip_prefix("export ")
            .or_else(|| body.strip_prefix("local "))
            .unwrap_or(body);
        if let Some(value) = assignment.strip_prefix("SESSION_DIR=") {
            if into_config(value) {
                let keep = &line[..line.len() - value.len()];
                hit = Some((
                    i + 1,
                    line.to_string(),
                    Some(format!("{keep}{SESSION_DIR_FIXED}")),
                ));
                break;
            }
        } else if hit.is_none() && body.contains(".config/hypr/sessions") {
            hit = Some((i + 1, line.to_string(), None));
        }
    }
    let Some((n, line, fixed)) = hit else {
        return;
    };
    let mut detail = vec![format!(
        "on every call it creates {}/sessions — through ricepilot's link, so\n\
         inside a profile's tree. `verify` then reports the profile as changed,\n\
         and the directory travels with the profile.",
        tilde(&hypr, &p.home)
    )];
    detail.push(match fixed {
        Some(fixed) => format!(
            "the patch, for you to apply; ricepilot does not edit your scripts:\n  \
             --- {path}\n  +++ {path}\n  @@ line {n} @@\n  -{line}\n  +{fixed}",
            path = script.display()
        ),
        None => format!(
            "line {n} names the directory itself:\n  {}\nmove it under {SESSION_DIR_FIXED};\n\
             ricepilot does not edit your scripts.",
            line.trim()
        ),
    });
    r.hazards.push(Finding {
        path: script.clone(),
        what: format!(
            "writes into {}, which ricepilot links",
            tilde(&hypr, &p.home)
        ),
        rule: "SAFETY.md R3 (nothing writes into a profile) — this script does".into(),
        detail,
        run: vec![format!("$EDITOR {}", sh(&script))],
    });
}

/// caelestia-cli's `install` and `update` can take the clone away through
/// their legacy-migration path. A standing hazard for any profile rooted
/// there, not something doctor can see happening.
fn caelestia_cli(p: &Paths, profiles: &[Prof], r: &mut Report) {
    let clone = p.home.join(".local/share/caelestia");
    let at_risk: Vec<&Prof> = profiles
        .iter()
        .filter(|q| q.manifest.root.is_some() && q.root.starts_with(&clone))
        .collect();
    let Some(first) = at_risk.first() else {
        return;
    };
    let names: Vec<String> = at_risk.iter().map(|q| format!("`{}`", q.name)).collect();
    r.hazards.push(Finding {
        path: first.root.clone(),
        what: format!(
            "is the root of {}, and caelestia-cli migrates from it",
            names.join(", ")
        ),
        rule: "NOT-POSSIBLE.md#running-rice-installers".into(),
        detail: vec![
            "`caelestia install` and `caelestia update` can delete this whole tree through\n\
             caelestia-cli's legacy-migration path, and caelestia's install.fish\n\
             overwrites by deleting first. ricepilot never runs either. if you do, run\n\
             `ricepilot doctor` afterwards: the links they turn into directories show\n\
             up as problems."
                .into(),
        ],
        run: vec![
            "# what the clone holds that is committed nowhere: the part an installer loses".into(),
            format!("git -C {} status", sh(&first.root)),
        ],
    });
}

/// snapper on `/home`. Root-only, so ricepilot prints it and never runs it.
fn snapper(look: &dyn Look, sys: &Path, r: &mut Report) {
    let configs = sys.join("etc/snapper/configs");
    let names = match look
        .lstat_or_absent(&configs)
        .and_then(|m| m.map(|_| look.list_dir(&configs)).transpose())
    {
        Ok(n) => n.unwrap_or_default(),
        Err(e) => {
            r.notes.push(format!(
                "{} could not be listed ({}), so doctor cannot say whether /home is \
                 snapshotted",
                configs.display(),
                said(&e)
            ));
            return;
        }
    };
    let mut other = Vec::new();
    let mut unread = Vec::new();
    for name in &names {
        let name = name.to_string_lossy().into_owned();
        // snapper's configs are often readable by root alone.
        let Ok(text) = look.slurp(&configs.join(&name)) else {
            unread.push(name);
            continue;
        };
        let covers_home = text.lines().any(|l| {
            l.trim()
                .strip_prefix("SUBVOLUME=")
                .is_some_and(|v| v.trim_matches('"') == "/home")
        });
        if covers_home {
            r.healthy
                .push(format!("/home has a snapper config (`{name}`)"));
            return;
        }
        other.push(name);
    }
    let mut found = match (other.is_empty(), names.is_empty()) {
        (_, true) => format!("there is no snapper config in {}.", configs.display()),
        (true, false) => String::new(),
        (false, false) => format!(
            "snapper has {} for other subvolumes: {}.",
            plural(other.len(), "config", "configs"),
            other.join(", ")
        ),
    };
    if !unread.is_empty() {
        if !found.is_empty() {
            found.push('\n');
        }
        found.push_str(&format!(
            "doctor could not read {} — snapper's configs are often root-only — so\n\
             /home may be covered after all; `snapper list-configs` says.",
            unread
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let (what, covered) = if unread.is_empty() {
        (
            "is not snapshotted",
            "everything ricepilot moves lives under /home, and nothing there is covered\n\
             by a snapshot.",
        )
    } else {
        (
            "may not be snapshotted",
            "everything ricepilot moves lives under /home. unless one of those configs\n\
             covers it, nothing there is covered by a snapshot.",
        )
    };
    r.hazards.push(Finding {
        path: PathBuf::from("/home"),
        what: what.into(),
        rule: "NOT-POSSIBLE.md#snapshotting".into(),
        detail: vec![
            found,
            covered.into(),
            "ricepilot cannot take one: a snapshot needs root, and ricepilot never\n\
             runs sudo. on a btrfs /home this is the command, as root:"
                .into(),
        ],
        run: vec!["snapper -c home create-config /home".into()],
    });
}

/// `caelestia shell -d` and `caelestia resizer -d`, from `/proc/*/cmdline`.
fn theme_daemons(look: &dyn Look, sys: &Path, r: &mut Report) {
    let proc_dir = sys.join("proc");
    let pids: Vec<String> = look
        .list_dir(&proc_dir)
        .unwrap_or_default()
        .into_iter()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let mut running: Vec<(&str, Vec<String>)> =
        vec![("shell", Vec::new()), ("resizer", Vec::new())];
    for pid in pids {
        // A process that exits between the listing and this read is simply
        // not running any more.
        let Ok(cmdline) = look.slurp(&proc_dir.join(&pid).join("cmdline")) else {
            continue;
        };
        let argv: Vec<&str> = cmdline.split('\0').filter(|a| !a.is_empty()).collect();
        let Some(i) = argv
            .iter()
            .position(|a| Path::new(a).file_name().is_some_and(|f| f == "caelestia"))
        else {
            continue;
        };
        let Some(sub) = argv.get(i + 1) else {
            continue;
        };
        let daemon = argv[i + 2..].iter().any(|a| *a == "-d" || *a == "--daemon");
        if let Some(slot) = running.iter_mut().find(|(s, _)| s == sub) {
            if daemon {
                slot.1.push(pid.clone());
            }
        }
    }
    let mut any = false;
    for (sub, pids) in running {
        if pids.is_empty() {
            continue;
        }
        any = true;
        r.hazards.push(Finding {
            path: proc_dir.join(&pids[0]),
            what: format!(
                "`caelestia {sub} -d` is running ({} {})",
                if pids.len() == 1 { "pid" } else { "pids" },
                pids.join(", ")
            ),
            rule: "DESIGN.md §3, why `generated` exists".into(),
            detail: vec![format!(
                "caelestia's theme engine rewrites its generated files with a temporary\n\
                 file and a rename, which replaces a symlink at any of those paths and\n\
                 changes files inside the profile's tree. expect `verify` to report them,\n\
                 and declare them `generated` or `volatile` in the profile. ricepilot does\n\
                 not stop the {sub}."
            )],
            run: Vec::new(),
        });
    }
    if !any {
        r.healthy.push("no caelestia theme daemon running".into());
    }
}
