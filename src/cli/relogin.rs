//! `switch --relogin` and `rollback --relogin`: after a switch that finished,
//! offer to end the session with `uwsm stop` — and only then (D58).
//!
//! `uwsm stop` is the one subprocess on the allowlist that changes the running
//! machine: it logs the user out. A logout after a switch that did not
//! finish, or whose rescue script is not there, is how a user ends up at a
//! greeter with no way back. So the offer is the last step of a sequence, and
//! each step is a value the next one cannot be reached without:
//!
//! 1. [`switch::Ended::Completed`](super::switch::Ended) — made only on the
//!    last line of a switch that ran to the end of phase C and retired its
//!    journal. Any other ending (declined, nothing to do, a dry run) says why
//!    no logout is offered and stops.
//! 2. The pre-flight. [`gather`] reads the disk and the environment — only
//!    reads — into [`Facts`]; [`decide`], which is pure, turns them into one
//!    [`Check`] per precondition. All of them must pass, or [`preflight`]
//!    returns the whole list and no question is asked. Only when every check
//!    passes does it return a [`Cleared`], which has private fields and is
//!    made nowhere else.
//! 3. The question, through [`confirm::affirmed`]: a y/N that defaults to
//!    no, with no flag that skips it (D47). A yes is a
//!    [`Yes`](super::confirm::Yes), made nowhere else.
//! 4. The pre-flight again — the answer may have taken minutes, and a rice
//!    installer can replace a link in less.
//! 5. [`Relogin::after`], the token's one constructor, which takes the
//!    second `Cleared` and the `Yes`; then `ops::exec` runs `uwsm stop` —
//!    by absolute path, with the environment [`exec::uwsm_stop_invocation`]
//!    describes — while this process still holds the switch's lock.
//!
//! Never `hyprctl dispatch exit`, never a signal to Hyprland: there is no
//! call on the allowlist that could do either (D52).

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::ExitCode;
use crate::ops::exec::{self, Call, Relogin};
use crate::ops::read::{self, Kind};
use crate::{generations, ledger, rescue, Error, Result};

use super::switch::{Completed, Ended, Outcome};
use super::{confirm, paths, Output};

/// The environment, as a lookup: `std::env::var_os` in the binary, a table
/// in a test.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Where systemd's user manager keeps a record of each running unit:
/// `$XDG_RUNTIME_DIR/systemd/units/invocation:<unit>`, present while the unit
/// is active. Read, never written.
pub const UNITS_DIR: &str = "systemd/units";

/// uwsm runs the compositor as `wayland-wm@<desktop entry>.service`. Its
/// record in [`UNITS_DIR`] is this prefix, the instance, and
/// [`UWSM_UNIT_SUFFIX`].
pub const UWSM_UNIT_PREFIX: &str = "invocation:wayland-wm@";
pub const UWSM_UNIT_SUFFIX: &str = ".service";

/// The question. Everything the person needs to answer it is printed
/// immediately above it.
pub const QUESTION: &str = "log out now with `uwsm stop`?";

// ---------------------------------------------------------------------------
// The facts
// ---------------------------------------------------------------------------

/// Everything the pre-flight read, as data. [`gather`] reads it; [`decide`]
/// judges it without reading anything, so every refusal can be shown from a
/// table of facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The generation the switch recorded and made current.
    pub generation: u32,
    pub switch: SwitchFacts,
    pub journal: JournalFacts,
    pub rescue: RescueFacts,
    pub dests: DestsFacts,
    pub lock: LockFacts,
    pub session: SessionFacts,
}

/// A read that failed is a fact too: the precondition it was for is not met.
pub type Read<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchFacts {
    /// What `generations/current` says now.
    pub current: Read<Option<u32>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalFacts {
    /// `journal/current.toml`.
    pub in_flight_path: PathBuf,
    /// Whether anything is at it.
    pub in_flight: Read<bool>,
    /// `journal/done-<id>.toml`, where this switch retired its journal.
    pub done_path: PathBuf,
    /// What is at it.
    pub done: Read<Option<Kind>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueFacts {
    pub path: PathBuf,
    /// The generation it must restore: the one before [`Facts::generation`].
    pub restores: u32,
    pub found: RescueFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RescueFound {
    Missing,
    /// Something that is not a regular file.
    NotAFile,
    Unreadable(String),
    Read {
        /// Whether its text is exactly what `rescue::script` writes for
        /// [`RescueFacts::restores`] — `Err` if that could not be worked out.
        is_the_script: Read<bool>,
        /// Whether `sh -n` accepts the text on the disk.
        parses: Read<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestsFacts {
    /// The ledger as it is now, or why it could not be read.
    pub ledger: Read<()>,
    pub each: Vec<DestFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestFacts {
    pub dest: PathBuf,
    /// What the plan said: a link to this path, or — for a destination the
    /// switch retired into the attic — nothing at all.
    pub want: Option<PathBuf>,
    pub found: Found,
    /// The ledger's row for `dest`, read now: target, dev, ino.
    pub ledger: Option<(PathBuf, u64, u64)>,
}

/// What one `lstat` (and, for a link, one `readlink`) found at a destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Absent,
    Link { target: PathBuf, dev: u64, ino: u64 },
    Other(&'static str),
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockFacts {
    pub path: PathBuf,
    /// Whether the file at the lock's path is still the inode this process
    /// holds the lock on.
    pub at_its_path: Read<bool>,
}

/// What the environment and `$XDG_RUNTIME_DIR` say about the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    /// The test sandbox's root, when `RICEPILOT_SANDBOX` is set (D56).
    pub sandbox: Option<PathBuf>,
    /// `XDG_RUNTIME_DIR`, as given.
    pub runtime: Option<PathBuf>,
    /// `WAYLAND_DISPLAY`, as given.
    pub wayland: Option<OsString>,
    /// What is at `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY`. `None` when it was not
    /// looked at, because one of the two could not be used.
    pub socket: Option<Read<Option<Kind>>>,
    /// Whether `DBUS_SESSION_BUS_ADDRESS` is set to something.
    pub bus: bool,
    /// The uwsm compositor units systemd records as running, by name. `None`
    /// when `$XDG_RUNTIME_DIR` could not be used, so nothing was looked at.
    pub units: Option<Read<Vec<String>>>,
}

// ---------------------------------------------------------------------------
// Gathering: reads only
// ---------------------------------------------------------------------------

fn said(e: &Error) -> String {
    e.to_string()
}

/// Read every fact the pre-flight needs, after the switch. Only reads: an
/// `lstat`, a `readlink`, a directory listing, a file's text, `sh -n`. A read
/// that fails is recorded as the fact that it failed.
pub fn gather(c: &Completed, get: Env) -> Facts {
    let p = c.paths();
    Facts {
        generation: c.generation(),
        switch: SwitchFacts {
            current: generations::current(&p.state).map_err(|e| said(&e)),
        },
        journal: JournalFacts {
            in_flight_path: p.journal_path(),
            in_flight: read::lstat_or_absent(&p.journal_path())
                .map(|m| m.is_some())
                .map_err(|e| said(&e)),
            done_path: c.journal_done().to_path_buf(),
            done: read::lstat_or_absent(c.journal_done())
                .map(|m| m.map(|m| m.kind))
                .map_err(|e| said(&e)),
        },
        rescue: gather_rescue(c),
        dests: gather_dests(c),
        lock: LockFacts {
            path: c.lock().path().to_path_buf(),
            at_its_path: c.lock().is_at_its_path().map_err(|e| said(&e)),
        },
        session: gather_session(get),
    }
}

fn gather_rescue(c: &Completed) -> RescueFacts {
    let path = c.rescue().to_path_buf();
    let back_to = c.back_to();
    let found = match read::lstat_or_absent(&path) {
        Err(e) => RescueFound::Unreadable(said(&e)),
        Ok(None) => RescueFound::Missing,
        Ok(Some(m)) if m.kind != Kind::File => RescueFound::NotAFile,
        Ok(Some(_)) => match read::slurp(&path) {
            Err(e) => RescueFound::Unreadable(said(&e)),
            Ok(text) => {
                // The script a switch writes for that generation, worked out
                // again from the generation and the binaries — the same
                // comparison `doctor` makes (D57).
                let attic = rescue::rescue_attic(&c.paths().state, back_to.id);
                let is_the_script = rescue::Binaries::locate()
                    .map(|b| rescue::script(back_to, &b, &attic) == text)
                    .map_err(|e| said(&e));
                let parses = exec::run(Call::ShSyntaxCheck { script: &text })
                    .map(|ran| ran.success())
                    .map_err(|e| said(&e));
                RescueFound::Read {
                    is_the_script,
                    parses,
                }
            }
        },
    };
    RescueFacts {
        path,
        restores: back_to.id,
        found,
    }
}

fn gather_dests(c: &Completed) -> DestsFacts {
    let led = ledger::load(&c.paths().ledger_path());
    let rows = led.as_ref().map(|l| l.entries.clone()).unwrap_or_default();
    let row = |dest: &Path| {
        rows.iter()
            .find(|e| e.dest == dest)
            .map(|e| (e.target.clone(), e.dev, e.ino))
    };
    let mut each = Vec::new();
    let planned = c
        .targets()
        .iter()
        .map(|t| (t.dest.clone(), Some(t.src.clone())))
        .chain(c.retired().iter().map(|d| (d.clone(), None)));
    for (dest, want) in planned {
        each.push(DestFacts {
            found: found_at(&dest),
            ledger: row(&dest),
            dest,
            want,
        });
    }
    DestsFacts {
        ledger: led.map(|_| ()).map_err(|e| said(&e)),
        each,
    }
}

fn found_at(dest: &Path) -> Found {
    match read::lstat(dest) {
        Err(e) => Found::Unreadable(said(&e)),
        Ok(None) => Found::Absent,
        Ok(Some(m)) => match m.kind {
            Kind::Symlink => match read::readlink(dest) {
                Ok(target) => Found::Link {
                    target,
                    dev: m.dev,
                    ino: m.ino,
                },
                Err(e) => Found::Unreadable(said(&e)),
            },
            Kind::Dir => Found::Other("real directory"),
            Kind::File => Found::Other("regular file"),
            Kind::Other => Found::Other("socket, fifo or device"),
        },
    }
}

/// `WAYLAND_DISPLAY` as a socket name ricepilot will look up: a plain name,
/// resolved in `$XDG_RUNTIME_DIR`. Wayland itself also accepts an absolute
/// path; ricepilot does not follow one, and declines instead.
fn plain_name(v: &OsString) -> Option<&std::ffi::OsStr> {
    let p = Path::new(v);
    let mut c = p.components();
    match (c.next(), c.next()) {
        (Some(std::path::Component::Normal(n)), None) => Some(n),
        _ => None,
    }
}

/// Whether `runtime` is a directory the pre-flight may look in: absolute,
/// free of `..`, and — inside the test sandbox — below its root, so no test
/// reads the real session's runtime directory (D56).
fn usable_runtime(runtime: &Path, sandbox: Option<&Path>) -> bool {
    match sandbox {
        Some(root) => paths::lexically_under(runtime, root),
        None => {
            runtime.is_absolute()
                && runtime
                    .components()
                    .all(|c| !matches!(c, std::path::Component::ParentDir))
        }
    }
}

fn gather_session(get: Env) -> SessionFacts {
    let nonempty = |name: &str| get(name).filter(|v| !v.is_empty());
    let sandbox = get(paths::SANDBOX_VAR).map(PathBuf::from);
    let runtime = nonempty("XDG_RUNTIME_DIR").map(PathBuf::from);
    let wayland = nonempty("WAYLAND_DISPLAY");
    let bus = nonempty("DBUS_SESSION_BUS_ADDRESS").is_some();

    let dir = runtime
        .as_deref()
        .filter(|r| usable_runtime(r, sandbox.as_deref()));
    let socket = match (dir, wayland.as_ref().and_then(plain_name)) {
        (Some(dir), Some(name)) => Some(
            read::lstat_or_absent(&dir.join(name))
                .map(|m| m.map(|m| m.kind))
                .map_err(|e| said(&e)),
        ),
        _ => None,
    };
    let units = dir.map(|dir| {
        let at = dir.join(UNITS_DIR);
        match read::lstat_or_absent(&at) {
            Ok(None) => Ok(Vec::new()),
            Ok(Some(_)) => read::list_dir(&at)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|n| {
                            let n = n.to_str()?;
                            let unit = n.strip_prefix("invocation:")?;
                            (n.starts_with(UWSM_UNIT_PREFIX) && n.ends_with(UWSM_UNIT_SUFFIX))
                                .then(|| unit.to_string())
                        })
                        .collect()
                })
                .map_err(|e| said(&e)),
            Err(e) => Err(said(&e)),
        }
    });
    SessionFacts {
        sandbox,
        runtime,
        wayland,
        socket,
        bus,
        units,
    }
}

// ---------------------------------------------------------------------------
// Deciding: pure
// ---------------------------------------------------------------------------

/// One precondition's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// A short name: `switch`, `journal`, `rescue`, `links`, `lock`,
    /// `session`.
    pub what: &'static str,
    pub ok: bool,
    /// What was found. Lines after the first are continuation.
    pub text: String,
}

fn pass(what: &'static str, text: String) -> Check {
    Check {
        what,
        ok: true,
        text,
    }
}

fn fail(what: &'static str, text: String) -> Check {
    Check {
        what,
        ok: false,
        text,
    }
}

/// Judge the facts: one [`Check`] per precondition, in a fixed order. Pure.
///
/// The preconditions are D58's, and every one must pass:
///
/// * `switch` — the generation the switch recorded is `current`;
/// * `journal` — nothing is in flight, and the switch's journal was retired
///   to `done-<id>.toml`;
/// * `rescue` — `rescue.sh` is a regular file, is exactly the script for the
///   generation before, and passes `sh -n`;
/// * `links` — every destination is what the plan said (a link to its
///   source, or nothing where the plan retired one), and the ledger's row
///   matches the link's target and `(dev, ino)`;
/// * `lock` — this process holds the lock, on the file at the lock's path;
/// * `session` — the process is inside a Wayland session that uwsm manages.
pub fn decide(f: &Facts) -> Vec<Check> {
    vec![
        decide_switch(f),
        decide_journal(&f.journal),
        decide_rescue(&f.rescue),
        decide_dests(&f.dests),
        decide_lock(&f.lock),
        decide_session(&f.session),
    ]
}

fn decide_switch(f: &Facts) -> Check {
    let g = f.generation;
    match &f.switch.current {
        Ok(Some(n)) if *n == g => pass("switch", format!("generation {g:04} is current")),
        Ok(other) => fail(
            "switch",
            format!(
                "generations/current says {}, not {g:04}, the generation this switch recorded.\n\
                 something changed ricepilot's history after the switch finished",
                other.map_or("nothing".to_string(), |n| format!("{n:04}"))
            ),
        ),
        Err(e) => fail(
            "switch",
            format!("generations/current could not be read: {e}"),
        ),
    }
}

fn decide_journal(j: &JournalFacts) -> Check {
    match (&j.in_flight, &j.done) {
        (Err(e), _) | (_, Err(e)) => fail("journal", format!("could not be read: {e}")),
        (Ok(true), _) => fail(
            "journal",
            format!(
                "{} exists: an operation is in flight and did not finish.\n\
                 `ricepilot recover` first; a machine half way through something is not one to \
                 log in to",
                j.in_flight_path.display()
            ),
        ),
        (Ok(false), Ok(Some(Kind::File))) => pass(
            "journal",
            format!("the switch's journal is retired: {}", j.done_path.display()),
        ),
        (Ok(false), Ok(_)) => fail(
            "journal",
            format!(
                "{} is not there, so this switch's journal was not retired as a file",
                j.done_path.display()
            ),
        ),
    }
}

fn decide_rescue(r: &RescueFacts) -> Check {
    let p = r.path.display();
    let g = r.restores;
    let why = "it is how you get back from a TTY if the next login does not come up";
    match &r.found {
        RescueFound::Missing => fail("rescue", format!("{p} is missing.\n{why}")),
        RescueFound::NotAFile => fail(
            "rescue",
            format!("{p} is not a regular file; something replaced it.\n{why}"),
        ),
        RescueFound::Unreadable(e) => fail("rescue", format!("{p} could not be read: {e}")),
        RescueFound::Read {
            is_the_script,
            parses,
        } => {
            let mut wrong = Vec::new();
            match is_the_script {
                Ok(true) => {}
                Ok(false) => wrong.push(format!(
                    "{p} is not the script this switch wrote to restore generation {g:04}"
                )),
                Err(e) => wrong.push(format!(
                    "the script for generation {g:04} could not be worked out ({e}), so {p} \
                     could not be compared with it"
                )),
            }
            match parses {
                Ok(true) => {}
                Ok(false) => wrong.push(format!("`sh -n` rejects {p}")),
                Err(e) => wrong.push(format!("`sh -n` could not be run on {p}: {e}")),
            }
            if wrong.is_empty() {
                pass(
                    "rescue",
                    format!("{p} restores generation {g:04} and passes `sh -n`"),
                )
            } else {
                wrong.push(why.to_string());
                fail("rescue", wrong.join("\n"))
            }
        }
    }
}

fn describe(found: &Found) -> String {
    match found {
        Found::Absent => "nothing is there".into(),
        Found::Link { target, .. } => format!("it is a link to {}", target.display()),
        Found::Other(kind) => format!("it is a {kind}"),
        Found::Unreadable(e) => format!("it could not be read: {e}"),
    }
}

fn decide_dests(d: &DestsFacts) -> Check {
    if let Err(e) = &d.ledger {
        return fail("links", format!("the ledger could not be read: {e}"));
    }
    let mut wrong = Vec::new();
    let (mut linked, mut empty) = (0usize, 0usize);
    for e in &d.each {
        let dest = e.dest.display();
        match (&e.want, &e.found) {
            (Some(src), Found::Link { target, dev, ino }) if target == src => match &e.ledger {
                Some((t, ldev, lino)) if t == src && (ldev, lino) == (dev, ino) => linked += 1,
                // Inode numbers are not printed: they mean nothing to the
                // reader, and they differ on every machine.
                Some(_) => wrong.push(format!(
                    "{dest}: points where the plan said, but is not the link the ledger\n\
                     recorded — a look-alike replaced it"
                )),
                None => wrong.push(format!("{dest}: the ledger has no row for it")),
            },
            (Some(src), found) => wrong.push(format!(
                "{dest}: the plan linked it to {}, and {}",
                src.display(),
                describe(found)
            )),
            (None, Found::Absent) => match &e.ledger {
                None => empty += 1,
                Some(_) => wrong.push(format!(
                    "{dest}: the plan retired it, and the ledger still has a row for it"
                )),
            },
            (None, found) => wrong.push(format!(
                "{dest}: the plan retired it into the attic, and {}",
                describe(found)
            )),
        }
    }
    if wrong.is_empty() {
        let retired = if empty > 0 {
            format!(", {empty} retired and empty")
        } else {
            String::new()
        };
        pass(
            "links",
            format!(
                "{linked} link(s) are exactly what the plan said and what the ledger \
                 recorded{retired}"
            ),
        )
    } else {
        fail("links", wrong.join("\n"))
    }
}

fn decide_lock(l: &LockFacts) -> Check {
    let p = l.path.display();
    match &l.at_its_path {
        Ok(true) => pass(
            "lock",
            format!("this ricepilot holds {p}, and it is still the file at that path"),
        ),
        Ok(false) => fail(
            "lock",
            format!(
                "{p} is no longer the file this ricepilot locked. it was replaced, so another\n\
                 ricepilot could be holding a lock of its own on the new one"
            ),
        ),
        Err(e) => fail("lock", format!("{p} could not be checked: {e}")),
    }
}

fn decide_session(s: &SessionFacts) -> Check {
    let mut wrong = Vec::new();
    match (&s.runtime, &s.sandbox) {
        (None, _) => wrong
            .push("XDG_RUNTIME_DIR is not set, so there is no session here to find".to_string()),
        (Some(r), Some(root)) if !paths::lexically_under(r, root) => wrong.push(format!(
            "XDG_RUNTIME_DIR={} is outside the test sandbox {}.\n\
             inside the sandbox ricepilot reads no real session's state (D56)",
            r.display(),
            root.display()
        )),
        (Some(r), None) if !usable_runtime(r, None) => wrong.push(format!(
            "XDG_RUNTIME_DIR={} is not an absolute path free of `..`",
            r.display()
        )),
        _ => {}
    }
    match (&s.wayland, &s.socket) {
        (None, _) => wrong.push(
            "WAYLAND_DISPLAY is not set: this ricepilot is not running inside a graphical \
             session\n(a TTY, ssh, a timer), and `uwsm stop` would end a session you are not \
             looking at"
                .to_string(),
        ),
        (Some(w), _) if plain_name(w).is_none() => wrong.push(format!(
            "WAYLAND_DISPLAY={} is not a plain socket name.\n\
             ricepilot looks for the socket only in XDG_RUNTIME_DIR",
            w.to_string_lossy()
        )),
        // Not looked at, because XDG_RUNTIME_DIR could not be used: said above.
        (Some(_), None) => {}
        // A socket, where a compositor listens.
        (Some(_), Some(Ok(Some(Kind::Other)))) => {}
        (Some(w), Some(Ok(None))) => wrong.push(format!(
            "WAYLAND_DISPLAY={} names no socket in XDG_RUNTIME_DIR: no compositor is listening \
             there",
            w.to_string_lossy()
        )),
        (Some(w), Some(Ok(Some(_)))) => wrong.push(format!(
            "WAYLAND_DISPLAY={}: what is at that name in XDG_RUNTIME_DIR is not a socket",
            w.to_string_lossy()
        )),
        (Some(_), Some(Err(e))) => {
            wrong.push(format!("the Wayland socket could not be looked at: {e}"))
        }
    }
    if !s.bus {
        wrong.push(
            "DBUS_SESSION_BUS_ADDRESS is not set: uwsm asks systemd for the logout over the \
             session\nbus, and without one `uwsm stop` has nothing to ask"
                .to_string(),
        );
    }
    let mut unit = None;
    match &s.units {
        None => {}
        Some(Err(e)) => wrong.push(format!(
            "systemd's record of running units could not be read ({e}), and it is the one\n\
             read-only way ricepilot has to see that uwsm manages this session"
        )),
        Some(Ok(units)) if units.is_empty() => wrong.push(format!(
            "no uwsm compositor unit (`wayland-wm@….service`) is running for this user —\n\
             {}/{UNITS_DIR} records none. this session is not managed by uwsm, and\n\
             `uwsm stop` is the only logout ricepilot runs: never `hyprctl dispatch exit`, \
             never\na signal to Hyprland. log out the way you logged in",
            s.runtime
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        )),
        Some(Ok(units)) if units.len() > 1 => wrong.push(format!(
            "more than one uwsm compositor unit is running: {}.\n\
             `uwsm stop` would stop one of them, and ricepilot will not guess which one is \
             this session",
            units.join(", ")
        )),
        Some(Ok(units)) => unit = units.first().cloned(),
    }
    match (wrong.is_empty(), unit) {
        (true, Some(u)) => pass(
            "session",
            format!(
                "inside a Wayland session ({}) that uwsm manages: {u} is running",
                s.wayland
                    .as_ref()
                    .map(|w| w.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
        ),
        _ => fail("session", wrong.join("\n")),
    }
}

// ---------------------------------------------------------------------------
// The proof, and the offer
// ---------------------------------------------------------------------------

/// Proof that every precondition passed, on a check made after the switch
/// finished. Made only by [`preflight`] — its fields are private — and
/// demanded by [`Relogin::after`]. It owns the [`Completed`], and with it the
/// process lock, so nothing else can start while it lives.
pub struct Cleared {
    completed: Box<Completed>,
    checks: Vec<Check>,
}

impl Cleared {
    pub fn checks(&self) -> &[Check] {
        &self.checks
    }
    pub fn completed(&self) -> &Completed {
        &self.completed
    }
}

/// The pre-flight did not pass. The lock was released with the
/// [`Completed`].
#[derive(Debug)]
pub struct Declined {
    pub generation: u32,
    pub checks: Vec<Check>,
}

/// Read, then judge. A [`Cleared`] only when every check passes; otherwise
/// every check, so the person sees each one that failed, not the first.
pub fn preflight(completed: Box<Completed>, get: Env) -> std::result::Result<Cleared, Declined> {
    let checks = decide(&gather(&completed, get));
    if checks.iter().all(|c| c.ok) {
        Ok(Cleared { completed, checks })
    } else {
        Err(Declined {
            generation: completed.generation(),
            checks,
        })
    }
}

/// `--relogin`, after `switch::run` or a rollback through it.
///
/// Returns the switch's own output with a line saying why no logout is
/// offered, unless the switch completed. When it did, the switch's output is
/// printed straight away — the person answering must be able to read what
/// happened — and what is returned is the rest.
///
/// The exit status is the switch's: a logout not offered, or declined, is
/// said on stdout and leaves the status of the switch, which did its job
/// (D58). Only running `uwsm stop` and it failing is an error.
pub fn offer(outcome: Outcome, get: Env) -> Result<Output> {
    let Outcome { output, ended } = outcome;
    let code = output.code;
    let completed = match ended {
        Ended::Completed(c) => c,
        other => {
            return Ok(Output {
                text: output.text + &not_offered(&other),
                code,
            })
        }
    };

    print!("{}", output.text);
    let _ = std::io::stdout().flush();

    let cleared = match preflight(completed, get) {
        Ok(c) => c,
        Err(d) => {
            return Ok(Output {
                text: declined(&d, false),
                code,
            })
        }
    };
    print!("{}", cleared_text(&cleared));
    let _ = std::io::stdout().flush();

    let Some(yes) = confirm::affirmed(QUESTION) else {
        return Ok(Output {
            text: NOT_CONFIRMED.to_string(),
            code,
        });
    };

    // Checked again: the answer may have taken minutes.
    let cleared = match preflight(cleared.completed, get) {
        Ok(c) => c,
        Err(d) => {
            return Ok(Output {
                text: declined(&d, true),
                code,
            })
        }
    };
    let token = Relogin::after(&cleared, yes);
    let ran = exec::run(Call::UwsmStop(token))?;
    // The lock is released only now, after `uwsm stop` has returned.
    drop(cleared);
    if ran.success() {
        return Ok(Output {
            text: STOPPED.to_string(),
            code: ExitCode::Ok,
        });
    }
    Err(Error::Io {
        context: format!(
            "`uwsm stop` exited {} and the session may still be running. the switch itself is \
             complete; log out by hand",
            ran.code
                .map_or("on a signal".to_string(), |c| format!("with status {c}"))
        ),
        source: std::io::Error::other(ran.stderr.lines().next().unwrap_or("").to_string()),
    })
}

// ---------------------------------------------------------------------------
// Wording
// ---------------------------------------------------------------------------

/// Why nothing is offered after a switch that did not complete.
pub fn not_offered(ended: &Ended) -> String {
    match ended {
        Ended::Declined => {
            "\n--relogin: the switch was declined, so no logout is offered.\n".into()
        }
        Ended::DryRun => {
            "\n--relogin: nothing was switched — this was a dry run — so no logout is \
                          offered.\nwith --commit, one is offered after the switch, if every \
                          check passes.\n"
                .into()
        }
        Ended::NothingToDo => "\n--relogin: nothing was switched — every destination already \
                               matches — so no logout\nis offered. if the switch that linked \
                               them has not been followed by a login yet, log\nout yourself:\n  \
                               uwsm stop\n"
            .into(),
        Ended::Completed(_) => String::new(),
    }
}

/// Every check, one per line, `ok` or `NO`, continuation lines indented.
pub fn checks_block(checks: &[Check]) -> String {
    let mut s = String::new();
    for c in checks {
        let mut lines = c.text.lines();
        let _ = writeln!(
            s,
            "  {:<3} {:<8} {}",
            if c.ok { "ok" } else { "NO" },
            c.what,
            lines.next().unwrap_or("")
        );
        for l in lines {
            let _ = writeln!(s, "  {:<3} {:<8} {l}", "", "");
        }
    }
    s
}

/// The block printed immediately above the question.
pub fn cleared_text(c: &Cleared) -> String {
    let done = c.completed();
    let mut s = String::new();
    let _ = writeln!(s);
    let _ = writeln!(s, "--relogin: every check passed.");
    s.push_str(&checks_block(c.checks()));
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "`uwsm stop` ends this session the clean way: every window closes, and anything \
         unsaved"
    );
    let _ = writeln!(
        s,
        "in them is lost. the login screen comes back, and the next login starts generation \
         {:04}.",
        done.generation()
    );
    let _ = writeln!(s, "if it does not come up, from a TTY (Ctrl+Alt+F2 … F6):");
    let _ = writeln!(s, "  sh {}", done.rescue().display());
    s
}

/// The pre-flight's refusal: every check, and what that leaves.
pub fn declined(d: &Declined, after_answer: bool) -> String {
    let failed = d.checks.iter().filter(|c| !c.ok).count();
    let mut s = String::new();
    let _ = writeln!(s);
    if after_answer {
        let _ = writeln!(
            s,
            "--relogin: checked again after your answer, and not logging out. {failed} of {} \
             check(s)",
            d.checks.len()
        );
        let _ = writeln!(s, "no longer pass:");
    } else {
        let _ = writeln!(
            s,
            "--relogin: not logging out. {failed} of {} check(s) did not pass:",
            d.checks.len()
        );
    }
    s.push_str(&checks_block(&d.checks));
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "nothing was run. the switch itself is complete: generation {:04} is what the next \
         login",
        d.generation
    );
    let _ = writeln!(
        s,
        "starts. ricepilot ends a session only when every check above passes."
    );
    s
}

/// After a no, or no answer at all.
pub const NOT_CONFIRMED: &str =
    "not logged out, and nothing was run. the switch takes effect at the next login.\n";

/// After `uwsm stop` returned success — if ricepilot is still here to say so.
pub const STOPPED: &str = "`uwsm stop` asked systemd to end the session.\n";
