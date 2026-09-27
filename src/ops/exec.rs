//! Subprocess invocation. Lives inside the `ops` boundary for the same reason
//! the filesystem calls do: every escape from this process into the outside
//! world should be visible in one directory.
//!
//! The allowlist is closed and each entry is read-only or user-initiated:
//! `sh -n` (parse a script, never run it), `pacman -Q` (query, never install),
//! `hyprctl version` (a query), `Hyprland --verify-config` (sandboxed scratch
//! copy only), `uwsm stop` (only behind `--relogin` and a y/N), `git status`
//! (read-only, reporting).
//!
//! `sudo`, `caelestia`, `install.fish`, `paru -S`, `rm`, `git stash|checkout|
//! clean|reset` and anything writing under `/etc` are refused here, not left
//! to caller discipline (`SAFETY.md` R3).
//!
//! # How closed is closed
//!
//! A caller does not pass an argument vector. It passes a [`Call`], and each
//! variant builds its own argv from typed fields: there is no way to ask for
//! `hyprctl dispatch …` or `git checkout` through this module, because no
//! variant spells them. What a caller *can* supply — a package name, a
//! repository path — is checked before anything is spawned (D52).
//!
//! # How a child is started (D52)
//!
//! * **By absolute path.** Each binary is looked for in [`BIN_DIRS`] by
//!   `lstat`, the way [`crate::rescue`] finds its three. `PATH` is never
//!   consulted: a `PATH` with `~/.local/bin` first is exactly how a
//!   same-named script would be run with the user's session at stake.
//! * **With an empty environment**, plus `LC_ALL=C` so anything parsed is in
//!   the one locale whose messages do not change, plus the few variables a
//!   given call cannot work without ([`Allowed::passes`]). Nothing else of
//!   ricepilot's environment reaches a child — no `LD_PRELOAD`, no `GIT_DIR`,
//!   no `HOME` unless the call needs one.
//! * **From `/`**, so no child inherits ricepilot's working directory and
//!   discovers something (a git repository, a config file) by accident.
//! * **Under a timeout and an output cap** ([`Allowed::timeout`],
//!   [`OUTPUT_CAP`]). A child that overruns is killed, and so is everything
//!   it started: each child leads a process group of its own, and the group
//!   is what is killed (D54). Output past the cap is drained and discarded,
//!   and the [`Ran`] says it was.
//! * **Never, from inside the test sandbox, if it reaches the session.**
//!   With [`SANDBOX_VAR`] set, `Hyprland`, `hyprctl` and `uwsm` are refused
//!   before anything is located ([`sandbox_refuses`], D56). The test harness
//!   sets it for every test process and every binary it starts.
//!
//! # The two entries that are not ordinary queries
//!
//! [`Call::UwsmStop`] ends the user's session. It needs a [`Relogin`] token,
//! and in this build nothing can make one: the token has a private field and
//! no constructor. `--relogin` will add the one constructor, behind its
//! confirmation and its pre-flight; until then the call is unreachable by
//! construction, not by convention.
//!
//! [`Call::HyprlandVerifyConfig`] takes a [`SandboxedConfig`], whose one
//! constructor is [`SandboxedConfig::build`] in [`sandbox`].
//! `Hyprland --verify-config` forks every `exec =` line it parses, so the only
//! thing it may ever be pointed at is a scratch copy with those lines
//! stripped, and the constructor is what makes one (D55).

use std::ffi::OsString;
use std::io::{Read, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::ops::read;
use crate::{Error, Result};

/// A subprocess ricepilot is permitted to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    ShSyntaxCheck,
    PacmanQuery,
    Hyprctl,
    HyprlandVerifyConfig,
    UwsmStop,
    GitStatus,
}

/// Where `sh -n` is. `/bin/sh` is the one path POSIX itself names, and the
/// rescue script is the thing that has to work when nothing else does, so it
/// is not looked up through `PATH`.
pub const SH: &str = "/bin/sh";

/// Directories searched for every other binary, in order.
///
/// `/bin` is not among them: on Arch (and on current Debian and Ubuntu) it is
/// a symlink to `usr/bin`, and [`read::lstat`] refuses a symlinked
/// intermediate component (D9) — so it would add nothing but a refusal.
/// `/usr/local/bin` is there for a locally built Hyprland; it is root-owned
/// like `/usr/bin`, and is searched second.
pub const BIN_DIRS: &[&str] = &["/usr/bin", "/usr/local/bin"];

/// The most a child may print on one stream that is kept. More than any
/// allowlisted query prints on a real machine (`git status` of a rice clone is
/// a few kilobytes), small enough that a runaway child cannot make ricepilot
/// hold an unbounded buffer (D52).
pub const OUTPUT_CAP: usize = 1 << 20;

/// The argv `uwsm stop` is run with. A constant, so what `--relogin` will do
/// can be asserted without being able to do it.
pub const UWSM_STOP_ARGV: &[&str] = &["stop"];

/// The test sandbox (D56). While it is set, [`run`] refuses every entry that
/// reaches the live session — [`Allowed::reaches_the_session`] — before
/// anything is located or spawned. The value is the sandbox root, which
/// `cli::paths` holds every location to.
///
/// It can only take away. No variable adds a binary, a directory to search
/// or an entry to the allowlist; the fake `Hyprland` the unit tests use is a
/// `cfg(test)` thread-local, not an environment variable (D55).
pub const SANDBOX_VAR: &str = "RICEPILOT_SANDBOX";

/// The one way the tests run the real `Hyprland` or `hyprctl`: set to `1`
/// alongside [`SANDBOX_VAR`]. Read only inside the sandbox, where it restores
/// what an unsandboxed ricepilot does anyway — so it can widen nothing — and
/// never for `uwsm stop`.
pub const LIVE_TESTS_VAR: &str = "RICEPILOT_LIVE_TESTS";

/// How long a killed child's pipes are waited on after it has gone. A
/// grandchild can hold them open; ricepilot does not wait for one.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

impl Allowed {
    /// Every entry, for the tests that must exercise or excuse each one.
    pub const ALL: [Allowed; 6] = [
        Allowed::ShSyntaxCheck,
        Allowed::PacmanQuery,
        Allowed::Hyprctl,
        Allowed::HyprlandVerifyConfig,
        Allowed::UwsmStop,
        Allowed::GitStatus,
    ];

    /// The binary's file name.
    pub fn program(self) -> &'static str {
        match self {
            Allowed::ShSyntaxCheck => "sh",
            Allowed::PacmanQuery => "pacman",
            Allowed::Hyprctl => "hyprctl",
            Allowed::HyprlandVerifyConfig => "Hyprland",
            Allowed::UwsmStop => "uwsm",
            Allowed::GitStatus => "git",
        }
    }

    /// Whether this entry reaches the running session: `Hyprland` is the
    /// compositor, `hyprctl` talks to it, `uwsm stop` ends it. These are the
    /// entries the test sandbox refuses (D56).
    pub fn reaches_the_session(self) -> bool {
        match self {
            Allowed::Hyprctl | Allowed::HyprlandVerifyConfig | Allowed::UwsmStop => true,
            Allowed::ShSyntaxCheck | Allowed::PacmanQuery | Allowed::GitStatus => false,
        }
    }

    /// How long the child may run before it is killed (D52).
    ///
    /// Queries answer in milliseconds; the limits are there so a wedged child
    /// (a pacman waiting on a stale NFS mount, a hyprctl on a socket nobody
    /// answers) turns into an error rather than a ricepilot that never exits.
    pub fn timeout(self) -> Duration {
        Duration::from_secs(match self {
            Allowed::ShSyntaxCheck => 10,
            Allowed::PacmanQuery => 10,
            Allowed::Hyprctl => 5,
            // A parse of a whole config, twice (`NOT-POSSIBLE.md`).
            Allowed::HyprlandVerifyConfig => 30,
            // Tearing a session down can take a while; ricepilot is very
            // likely one of the things torn down before this expires.
            Allowed::UwsmStop => 60,
            // A cold cache on a large tree.
            Allowed::GitStatus => 60,
        })
    }

    /// The variables of ricepilot's own environment this call passes through,
    /// when they are set. Everything else is cleared (D52).
    pub fn passes(self) -> &'static [&'static str] {
        match self {
            // Parses stdin. Needs nothing.
            Allowed::ShSyntaxCheck => &[],
            // Reads /var/lib/pacman/local. Needs nothing.
            Allowed::PacmanQuery => &[],
            // Finds the compositor's socket under
            // $XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE.
            Allowed::Hyprctl => &["XDG_RUNTIME_DIR", "HYPRLAND_INSTANCE_SIGNATURE"],
            // Deliberately nothing, and above all not
            // HYPRLAND_INSTANCE_SIGNATURE: a verify run must not be able to
            // find, let alone talk to, the running compositor.
            Allowed::HyprlandVerifyConfig => &[],
            // Talks to systemd over the session bus.
            Allowed::UwsmStop => &["HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"],
            // No HOME and no XDG_CONFIG_HOME, so neither the user's global
            // git config nor anything it would run is read.
            Allowed::GitStatus => &[],
        }
    }
}

/// `hyprctl` subcommands ricepilot may run. Queries only.
///
/// `dispatch submap reset` is not here. It exists to follow a live
/// `hyprctl reload` (`docs/DESIGN.md` §8), and v1 has no live reload, so the
/// only thing it could do today is change the running session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HyprctlQuery {
    Version,
}

/// A Hyprland config that is safe to hand to `Hyprland --verify-config`: a
/// scratch copy, every sourced file copied with it, every exec-family line
/// blanked, paths into the profile rewritten into the copy (D55).
///
/// Its one constructor is [`SandboxedConfig::build`], in the child module
/// [`sandbox`], which documents what it guarantees. The fields are private to
/// this module and that one, so "run verify-config on the user's real
/// config" is not a mistake anyone can type.
#[derive(Debug)]
pub struct SandboxedConfig {
    /// The copy of the entry file, which is what `-c` names.
    file: PathBuf,
    /// The scratch directory: `state/verify/<id>/`.
    scratch: PathBuf,
    /// `HOME` for the child: the mirror of the real home, inside `scratch`.
    home: PathBuf,
    /// `XDG_RUNTIME_DIR` for the child, inside `scratch`.
    runtime: PathBuf,
    copied: Vec<sandbox::Copied>,
}

impl SandboxedConfig {
    /// The scratch directory. Kept after the run, as the evidence of what was
    /// parsed; only `gc` can take it away (D55).
    pub fn scratch(&self) -> &Path {
        &self.scratch
    }

    /// Every file copied, with what was stripped from it.
    pub fn copied(&self) -> &[sandbox::Copied] {
        &self.copied
    }

    /// `text` with each copy's path replaced by the path it was copied from,
    /// so what Hyprland says about the copy is said about the user's file.
    pub fn in_terms_of_the_originals(&self, text: &str) -> String {
        let mut pairs: Vec<(String, String)> = self
            .copied
            .iter()
            .map(|c| {
                (
                    c.copy.to_string_lossy().into_owned(),
                    c.real.to_string_lossy().into_owned(),
                )
            })
            .collect();
        pairs.sort_by_key(|(copy, _)| std::cmp::Reverse(copy.len()));
        let mut out = text.to_string();
        for (copy, real) in pairs {
            out = out.replace(&copy, &real);
        }
        out
    }
}

pub mod sandbox;

/// Proof that `uwsm stop` has been asked for and confirmed.
///
/// Has no constructor in this build, and the private field means none can be
/// written outside this module. `--relogin` will add exactly one, behind its
/// y/N and its pre-flight; until it does, [`Call::UwsmStop`] cannot be built.
///
/// The type is public, so it can be named:
///
/// ```
/// use ricepilot::ops::exec::{Relogin, SandboxedConfig};
/// ```
///
/// but it cannot be made, and neither can a sandboxed config:
///
/// ```compile_fail,E0451
/// let _ = ricepilot::ops::exec::Relogin { _sealed: () };
/// ```
///
/// ```compile_fail,E0451
/// let _ = ricepilot::ops::exec::SandboxedConfig { file: "/home/u/.config/hypr/hyprland.conf".into() };
/// ```
#[derive(Debug)]
pub struct Relogin {
    _sealed: (),
}

/// One invocation on the allowlist, with its arguments as typed values.
#[derive(Debug)]
pub enum Call<'a> {
    /// `sh -n`, with the script on stdin.
    ShSyntaxCheck { script: &'a str },
    /// `pacman -Q -- <package>`, one package. Exit 0 means installed or
    /// provided by something installed.
    PacmanQuery { package: &'a str },
    /// `hyprctl <query>`. Note that `hyprctl` exits 0 even when it could not
    /// reach a compositor; the caller has to read what it said.
    Hyprctl(HyprctlQuery),
    /// `Hyprland --verify-config -c <scratch file>`.
    HyprlandVerifyConfig(&'a SandboxedConfig),
    /// `uwsm stop`. Ends the session.
    UwsmStop(Relogin),
    /// `git status --porcelain=v1` of the repository at `repo`, without
    /// taking the index lock or writing the index.
    GitStatus { repo: &'a Path },
}

impl Call<'_> {
    pub fn allowed(&self) -> Allowed {
        match self {
            Call::ShSyntaxCheck { .. } => Allowed::ShSyntaxCheck,
            Call::PacmanQuery { .. } => Allowed::PacmanQuery,
            Call::Hyprctl(_) => Allowed::Hyprctl,
            Call::HyprlandVerifyConfig(_) => Allowed::HyprlandVerifyConfig,
            Call::UwsmStop(_) => Allowed::UwsmStop,
            Call::GitStatus { .. } => Allowed::GitStatus,
        }
    }

    /// The arguments after the program, or a refusal if a caller-supplied
    /// value could be read as something other than what it claims to be.
    fn argv(&self) -> Result<Vec<OsString>> {
        let s = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        Ok(match self {
            Call::ShSyntaxCheck { .. } => s(&["-n"]),
            Call::PacmanQuery { package } => {
                check_package_name(package)?;
                // `--` as well as the name check: belt and braces against a
                // name ever being read as an option.
                let mut v = s(&["-Q", "--"]);
                v.push(OsString::from(package));
                v
            }
            Call::Hyprctl(HyprctlQuery::Version) => s(&["version"]),
            Call::HyprlandVerifyConfig(cfg) => {
                let mut v = s(&["--verify-config", "-c"]);
                v.push(cfg.file.clone().into_os_string());
                v
            }
            Call::UwsmStop(_) => s(UWSM_STOP_ARGV),
            Call::GitStatus { repo } => {
                check_repo(repo)?;
                // `--no-optional-locks`: `git status` otherwise refreshes and
                // rewrites `.git/index`, which is a write into the user's
                // clone. `core.fsmonitor=false`: a repository may name a
                // program to run on every status; this is status, not that.
                let mut v = s(&["--no-optional-locks", "-c", "core.fsmonitor=false", "-C"]);
                v.push(repo.as_os_str().to_os_string());
                v.extend(s(&["status", "--porcelain=v1", "--untracked-files=all"]));
                v
            }
        })
    }

    /// Variables set on the child beyond `LC_ALL=C` and [`Allowed::passes`].
    fn fixed_env(&self) -> Vec<(OsString, OsString)> {
        match self {
            Call::GitStatus { repo } => vec![
                // The repository is the one named, not one git finds by
                // walking up from it: a directory that is not a repository
                // must not be answered for by the repository above it.
                (
                    "GIT_CEILING_DIRECTORIES".into(),
                    repo.parent().unwrap_or(Path::new("/")).into(),
                ),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ],
            // uwsm is a Python program that runs `systemctl`. It gets a fixed
            // search path, not ricepilot's.
            Call::UwsmStop(_) => vec![("PATH".into(), "/usr/bin".into())],
            // Everything Hyprland could look for, inside the scratch copy:
            // `~` there is the mirror of the real home, and the runtime
            // directory — where a compositor's socket and log would go — is
            // an empty directory of its own. Hyprland refuses to start
            // without one, and the real one is exactly what it must not see.
            // No HYPRLAND_INSTANCE_SIGNATURE, no WAYLAND_DISPLAY, no
            // DBUS_SESSION_BUS_ADDRESS: `env_clear` took them all (D55).
            Call::HyprlandVerifyConfig(cfg) => {
                let h = |rel: &str| cfg.home.join(rel).into_os_string();
                vec![
                    ("HOME".into(), cfg.home.clone().into_os_string()),
                    ("XDG_CONFIG_HOME".into(), h(".config")),
                    ("XDG_CACHE_HOME".into(), h(".cache")),
                    ("XDG_DATA_HOME".into(), h(".local/share")),
                    ("XDG_STATE_HOME".into(), h(".local/state")),
                    (
                        "XDG_RUNTIME_DIR".into(),
                        cfg.runtime.clone().into_os_string(),
                    ),
                ]
            }
            _ => Vec::new(),
        }
    }

    fn stdin(&self) -> Option<&[u8]> {
        match self {
            Call::ShSyntaxCheck { script } => Some(script.as_bytes()),
            _ => None,
        }
    }

    /// How the call is named in an error: the program and its argv.
    fn describe(&self) -> String {
        let args = self
            .argv()
            .map(|v| {
                v.iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        format!("`{} {args}`", self.allowed().program())
    }
}

/// What a child did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub program: PathBuf,
    /// `None` if the child was ended by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Output passed [`OUTPUT_CAP`] on at least one stream and the rest was
    /// discarded. A truncated answer is not a complete one; a caller that
    /// compares or parses output must treat it as unknown.
    pub truncated: bool,
}

impl Ran {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Where `what`'s binary is, if it is installed in [`BIN_DIRS`].
///
/// Reality, not a documented layout (`SAFETY.md` R7): each candidate is
/// `lstat`ed. `sh` is always [`SH`] and is not searched for.
pub fn locate(what: Allowed) -> Result<Option<PathBuf>> {
    if what == Allowed::ShSyntaxCheck {
        return Ok(Some(PathBuf::from(SH)));
    }
    // In the crate's own unit tests, `Hyprland` is whatever stand-in the test
    // installed, or absent — never the real binary (D55) — and `hyprctl` and
    // `uwsm` are absent (D56). Compiled out of every other build, so there is
    // no variable or flag that swaps a binary in a ricepilot anyone runs.
    #[cfg(test)]
    if what.reaches_the_session() {
        return Ok(match what {
            Allowed::HyprlandVerifyConfig => {
                test_support::FAKE_HYPRLAND.with(|f| f.borrow().clone())
            }
            _ => None,
        });
    }
    for dir in BIN_DIRS {
        let candidate = Path::new(dir).join(what.program());
        if let Some(m) = read::lstat_or_absent(&candidate)? {
            // A symlink is accepted (distributions ship `sh -> bash`); a
            // directory or a device at that name is not a binary.
            if matches!(m.kind, read::Kind::File | read::Kind::Symlink) {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

/// Run one allowlisted call and report what it did.
///
/// A non-zero exit is **not** an error here: `pacman -Q` exiting 1 is the
/// answer "not installed". Errors are for a binary that is not there, a value
/// that was refused, a child that could not be started, and a child that ran
/// out of time.
pub fn run(call: Call<'_>) -> Result<Ran> {
    let limit = call.allowed().timeout();
    run_within(call, limit)
}

/// The refusal [`run`] gives when `what` is not installed. Public so its
/// wording can be reviewed on a machine that has every binary.
pub fn not_installed(what: Allowed) -> Error {
    Error::Refused {
        rule: "R3",
        path: PathBuf::from(what.program()),
        why: format!(
            "`{}` is not installed in any of {}. ricepilot runs a subprocess only by absolute \
             path and does not look anywhere else — `PATH` is not consulted",
            what.program(),
            BIN_DIRS.join(", ")
        ),
    }
}

/// The refusal [`run`] gives inside the test sandbox for an entry that
/// reaches the live session, or `None` if `what` may run (D56).
///
/// Checked in `run` itself, the one place a child is started, rather than in
/// [`locate`]: whatever a caller has found or built, nothing that reaches the
/// session is spawned from inside the sandbox.
pub fn sandbox_refuses(what: Allowed, get: impl Fn(&str) -> Option<OsString>) -> Option<Error> {
    get(SANDBOX_VAR)?;
    if !what.reaches_the_session() {
        return None;
    }
    let live = get(LIVE_TESTS_VAR).is_some_and(|v| v == "1");
    if live && what != Allowed::UwsmStop {
        return None;
    }
    Some(Error::Refused {
        rule: "R1",
        path: PathBuf::from(what.program()),
        why: format!(
            "`{}` reaches the live session, and {SANDBOX_VAR} is set: inside the test sandbox \
             ricepilot never runs it{}",
            what.program(),
            if what == Allowed::UwsmStop {
                ""
            } else {
                " (only an explicit RICEPILOT_LIVE_TESTS=1 lets a test run the real one)"
            }
        ),
    })
}

fn run_within(call: Call<'_>, limit: Duration) -> Result<Ran> {
    run_in(call, limit, |name| std::env::var_os(name))
}

/// [`run_within`] against the environment `get` describes: the sandbox check
/// and the variables passed through both read it.
fn run_in(call: Call<'_>, limit: Duration, get: impl Fn(&str) -> Option<OsString>) -> Result<Ran> {
    let what = call.allowed();
    if let Some(refusal) = sandbox_refuses(what, &get) {
        return Err(refusal);
    }
    let args = call.argv()?;
    let Some(program) = locate(what)? else {
        return Err(not_installed(what));
    };

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .env_clear()
        .env("LC_ALL", "C")
        .current_dir("/");
    for name in what.passes() {
        if let Some(v) = get(name) {
            cmd.env(name, v);
        }
    }
    for (k, v) in call.fixed_env() {
        cmd.env(k, v);
    }
    let input = call.stdin().map(<[u8]>::to_vec);
    supervise(cmd, &program, &call.describe(), input, limit)
}

/// Start `cmd` and see it through: stdin fed, both pipes drained, the
/// deadline enforced. Everything [`run`] does after it has decided *what* to
/// run, split out so the timeout path can be tested against a child that does
/// more than `sh -n` ever will.
///
/// The child is started as the leader of **a process group of its own**, and
/// a child that overruns is killed as a group (D54). Killing only the direct
/// child would leave anything it had started — a `sleep` behind `&`, a
/// daemon an `exec =` line forked — running with ricepilot's pipes still
/// open, after ricepilot had reported the call as ended. `process_group(0)`
/// is `setpgid(0, 0)` in the child between fork and exec, done by std; no
/// unsafe is involved.
fn supervise(
    mut cmd: Command,
    program: &Path,
    describe: &str,
    input: Option<Vec<u8>>,
    limit: Duration,
) -> Result<Ran> {
    cmd.process_group(0);
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|source| Error::Io {
        context: format!("starting {describe} ({})", program.display()),
        source,
    })?;

    // Every pipe is serviced by its own thread, so a child that fills one
    // while ricepilot is writing another cannot deadlock the pair.
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            // A child that exits before reading everything closes the pipe;
            // that is its answer, and the exit status carries it.
            let _ = stdin.write_all(&bytes);
        });
    }
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());

    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // The group first, while the leader is still unreaped: its
                // pid cannot be reused as long as it is a zombie, so the group
                // id cannot name anybody else's processes. ESRCH (the whole
                // group already gone) is not a failure.
                let _ = rustix::process::kill_process_group(
                    rustix::process::Pid::from_child(&child),
                    rustix::process::Signal::KILL,
                );
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Io {
                    context: format!(
                        "{describe} did not finish within {}s and was killed, with every \
                         process it had started",
                        limit.as_secs()
                    ),
                    source: std::io::Error::from(std::io::ErrorKind::TimedOut),
                });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(source) => {
                return Err(Error::Io {
                    context: format!("waiting for {describe}"),
                    source,
                })
            }
        }
    };

    let (stdout, t_out) = collect(out);
    let (stderr, t_err) = collect(err);
    Ok(Ran {
        program: program.to_path_buf(),
        code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        truncated: t_out || t_err,
    })
}

type Drained = mpsc::Receiver<(Vec<u8>, bool)>;

/// Read a pipe to its end on a thread, keeping at most [`OUTPUT_CAP`] bytes.
/// Past the cap it keeps reading and discards, so the child never blocks on a
/// full pipe.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Drained {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut truncated = false;
        if let Some(mut pipe) = pipe {
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let room = OUTPUT_CAP - kept.len();
                        if n > room {
                            truncated = true;
                        }
                        kept.extend_from_slice(&buf[..n.min(room)]);
                    }
                }
            }
        }
        let _ = tx.send((kept, truncated));
    });
    rx
}

/// The drained bytes, once the child has exited. A pipe still held open by a
/// grandchild after [`DRAIN_GRACE`] is abandoned and reported as truncated.
fn collect(rx: Drained) -> (Vec<u8>, bool) {
    rx.recv_timeout(DRAIN_GRACE)
        .unwrap_or_else(|_| (Vec::new(), true))
}

/// A pacman package name as makepkg defines one: ASCII alphanumerics and
/// `@._+-`, not starting with `-` or `.`. Anything else is refused before
/// pacman sees it — and, earlier still, when the manifest is read.
pub fn is_package_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c))
}

fn check_package_name(name: &str) -> Result<()> {
    if is_package_name(name) {
        return Ok(());
    }
    Err(Error::Refused {
        rule: "R3",
        path: PathBuf::from(name),
        why: "is not a package name (ASCII letters, digits and `@._+-`, not starting with `-` \
              or `.`), so it was not passed to `pacman -Q`"
            .into(),
    })
}

/// `git -C` is given an absolute path and nothing that could be taken for an
/// option or a relative walk.
fn check_repo(repo: &Path) -> Result<()> {
    if repo.is_absolute()
        && !repo
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Ok(());
    }
    Err(Error::Refused {
        rule: "R3",
        path: repo.to_path_buf(),
        why: "`git status` is only run on an absolute path free of `..`".into(),
    })
}

/// Parse `script` with `sh -n` without running a line of it.
///
/// The script is fed on **stdin** rather than written to a temp file. A temp
/// file would have to be tidied away afterwards, and tidying away is a
/// removal, which exists nowhere outside `src/gc/` (R2). Stdin has no such
/// problem and `sh -n` reads it happily.
///
/// `-n` means "read commands and check for syntax errors, but do not execute".
/// It is the only mode this function will ever invoke `sh` in.
pub fn sh_syntax_check(script: &str) -> Result<()> {
    let ran = run(Call::ShSyntaxCheck { script })?;
    if ran.success() {
        return Ok(());
    }
    Err(Error::Refused {
        rule: "R4",
        path: PathBuf::from(SH),
        why: format!(
            "the generated rescue script did not parse, so it was not written: {}",
            ran.stderr.trim()
        ),
    })
}

/// The stand-in for `Hyprland` in unit tests. `cfg(test)` only: it does not
/// exist in the library integration tests link against, nor in the binary.
#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        pub(crate) static FAKE_HYPRLAND: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// Make `Hyprland` mean `fake` for the rest of this test's thread.
    pub(crate) fn use_fake_hyprland(fake: Option<PathBuf>) {
        FAKE_HYPRLAND.with(|f| *f.borrow_mut() = fake);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap keeps exactly [`OUTPUT_CAP`] bytes, says it dropped the rest,
    /// and reads to the end rather than leaving the writer blocked.
    #[test]
    fn output_past_the_cap_is_drained_and_reported() {
        let big = std::io::repeat(b'x').take(OUTPUT_CAP as u64 * 2 + 17);
        let (kept, truncated) = collect(drain(Some(big)));
        assert_eq!(kept.len(), OUTPUT_CAP);
        assert!(truncated);

        let small = std::io::repeat(b'y').take(10);
        let (kept, truncated) = collect(drain(Some(small)));
        assert_eq!(kept, b"yyyyyyyyyy");
        assert!(!truncated);
    }

    /// The sandbox refuses exactly the entries that reach the session; the
    /// live-tests opt-in restores Hyprland and hyprctl and never `uwsm stop`;
    /// outside the sandbox nothing is refused (D56).
    #[test]
    fn the_sandbox_refuses_what_reaches_the_session_and_nothing_else() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let outside = env(&[(LIVE_TESTS_VAR, "1")]);
        let sandbox = env(&[(SANDBOX_VAR, "/r/target/fixtures")]);
        let live = env(&[(SANDBOX_VAR, "/r/target/fixtures"), (LIVE_TESTS_VAR, "1")]);
        let not_one = env(&[(SANDBOX_VAR, "/r/target/fixtures"), (LIVE_TESTS_VAR, "yes")]);
        for what in Allowed::ALL {
            assert!(sandbox_refuses(what, outside).is_none(), "{what:?}");
            let refused = sandbox_refuses(what, sandbox).is_some();
            assert_eq!(refused, what.reaches_the_session(), "{what:?}");
            assert_eq!(
                sandbox_refuses(what, not_one).is_some(),
                what.reaches_the_session(),
                "{what:?}"
            );
            assert_eq!(
                sandbox_refuses(what, live).is_some(),
                what == Allowed::UwsmStop,
                "{what:?}"
            );
        }
    }

    /// And `run` itself honours it, before `locate`. `hyprctl version` is the
    /// call because it needs no sandboxed config; in this build `hyprctl` is
    /// never located (above), so were the check missing the answer would be
    /// "not installed" (R3), not the sandbox's R1 — and nothing would run.
    #[test]
    fn run_checks_the_sandbox_before_anything_else() {
        let e = run_in(
            Call::Hyprctl(HyprctlQuery::Version),
            Duration::from_secs(5),
            |name| (name == SANDBOX_VAR).then(|| OsString::from("/r/target/fixtures")),
        )
        .unwrap_err();
        assert!(
            matches!(&e, Error::Refused { rule: "R1", .. }),
            "not the sandbox refusal: {e}"
        );
    }

    /// A child that overruns is killed and the call is an error, not a hang.
    /// A zero limit makes the first poll the one that expires.
    #[test]
    fn a_child_that_overruns_is_killed_and_reported() {
        let script = "echo parsed\n".repeat(200_000);
        let err = run_within(Call::ShSyntaxCheck { script: &script }, Duration::ZERO).unwrap_err();
        match err {
            Error::Io { context, source } => {
                assert_eq!(source.kind(), std::io::ErrorKind::TimedOut);
                assert!(context.contains("was killed"), "{context}");
            }
            other => panic!("expected a timeout, got {other}"),
        }
    }

    /// A scratch directory for the process-group tests, under the repo's
    /// `target/fixtures/` like every other fixture (D5).
    fn pgroup_fixture(case: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/fixtures/m5")
            .join(case);
        crate::ops::mutate::make_dirs(&dir).unwrap();
        dir
    }

    /// `sh -c` that starts a long `sleep` in the background, writes its pid
    /// down and waits on it — a child with a grandchild, which is what an
    /// `exec =` line or a wedged helper looks like from here.
    fn with_a_grandchild(pidfile: &Path) -> Command {
        let mut cmd = Command::new(SH);
        cmd.arg("-c")
            .arg(format!(
                "sleep 30 & echo $! > '{}'; wait",
                pidfile.display()
            ))
            .env_clear()
            .current_dir("/");
        cmd
    }

    fn grandchild(pidfile: &Path) -> rustix::process::Pid {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = read::slurp(pidfile) {
                if let Ok(n) = text.trim().parse::<i32>() {
                    return rustix::process::Pid::from_raw(n).unwrap();
                }
            }
            assert!(Instant::now() < deadline, "the grandchild never started");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether `pid` is still a live process. A zombie awaiting its reaper
    /// counts as gone: it runs nothing and holds nothing open.
    fn alive(pid: rustix::process::Pid) -> bool {
        if rustix::process::test_kill_process(pid).is_err() {
            return false;
        }
        let stat = read::slurp(&PathBuf::from(format!(
            "/proc/{}/stat",
            pid.as_raw_nonzero()
        )))
        .unwrap_or_default();
        // `pid (comm) S …`: the state is the first field after the `)`.
        !matches!(
            stat.rsplit_once(')').map(|(_, rest)| rest.trim_start()),
            Some(r) if r.starts_with('Z') || r.starts_with('X')
        )
    }

    fn gone_within(pid: rustix::process::Pid, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        !alive(pid)
    }

    /// The timeout kills the whole group: the `sleep` the child started is
    /// gone as well, not left running with nobody waiting for it (D54).
    #[test]
    fn a_timeout_kills_the_grandchildren_too() {
        let dir = pgroup_fixture("exec_pgroup_killed");
        let pidfile = dir.join("grandchild.pid");
        crate::ops::mutate::write_atomic(&pidfile, b"").unwrap();

        let err = supervise(
            with_a_grandchild(&pidfile),
            Path::new(SH),
            "`sh -c …`",
            None,
            Duration::from_millis(500),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Io { source, .. } if source.kind() == std::io::ErrorKind::TimedOut),
            "{err}"
        );

        let pid = grandchild(&pidfile);
        assert!(
            gone_within(pid, Duration::from_secs(5)),
            "the grandchild {} outlived the timeout",
            pid.as_raw_nonzero()
        );
    }

    /// The control: the same child, killed the way `run` used to kill it
    /// (the direct child only), leaves its grandchild running. Without this
    /// the test above could pass because `sleep` died for some other reason.
    #[test]
    fn killing_only_the_child_would_leave_the_grandchild_running() {
        let dir = pgroup_fixture("exec_pgroup_control");
        let pidfile = dir.join("grandchild.pid");
        crate::ops::mutate::write_atomic(&pidfile, b"").unwrap();

        let mut cmd = with_a_grandchild(&pidfile);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap();
        let pid = grandchild(&pidfile);
        child.kill().unwrap();
        child.wait().unwrap();

        let survived = !gone_within(pid, Duration::from_millis(300));
        // Tidy up the control's own orphan before asserting anything.
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        assert!(
            survived,
            "killing sh alone took its grandchild with it, so the test above proves nothing"
        );
    }
}
