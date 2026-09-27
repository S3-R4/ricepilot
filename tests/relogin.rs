//! `switch --relogin` and `rollback --relogin` (D58), up to the exec boundary
//! and never past it.
//!
//! `uwsm stop` ends the session, and the user has said the suite must never
//! touch the real one. Nothing here can: every process these tests start is
//! inside the test sandbox (D56), where `ops::exec` refuses `uwsm` before it
//! looks for it, and the in-process tests stop at the pre-flight, which only
//! reads. So what is tested is everything *up to* the call: each
//! precondition's refusal, the prompt, "no" and no answer running nothing,
//! "yes" arriving at the sandbox's refusal — which is how a test sees that the
//! call would have been made — and the checks running again after the
//! answer. The argv and environment the call would get are asserted in
//! `tests/exec.rs` from the pure `uwsm_stop_invocation`.
//!
//! The session a real `--relogin` needs is built inside the fixture's own
//! runtime directory: a Wayland socket, and the record systemd keeps of a
//! running `wayland-wm@….service`. Neither is the real one; the real runtime
//! directory is never read.

// The harness tampers with fixture trees with the ordinary standard library;
// the guards only ever scan src/.
#![allow(clippy::disallowed_methods)]

mod common;

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use common::{redact, switching, undate, Fixture, SANDBOX_ROOT};
use ricepilot::cli::paths::Paths;
use ricepilot::cli::relogin::{self, Check, Declined};
use ricepilot::cli::switch::{Completed, Ended};
use ricepilot::error::ExitCode;
use ricepilot::ops::exec::{self, Allowed, SANDBOX_VAR};

const UNIT: &str = "wayland-wm@hyprland.desktop.service";

// ---- a session, inside the fixture ----

/// Make the fixture's runtime directory look like the inside of a uwsm
/// session: a Wayland socket and systemd's record of the compositor unit.
/// Returns the variables a process in that session has.
fn uwsm_session(f: &Fixture) -> Vec<(String, String)> {
    f.dir(".run");
    wayland_socket(f, "wayland-1");
    unit_record(f, UNIT);
    session_env(f)
}

fn session_env(f: &Fixture) -> Vec<(String, String)> {
    vec![
        ("WAYLAND_DISPLAY".into(), "wayland-1".into()),
        (
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={}", f.path(".run/bus").display()),
        ),
    ]
}

/// A Unix socket at `.run/<name>`. The listener is dropped at once; the
/// socket file stays, which is all an `lstat` sees.
fn wayland_socket(f: &Fixture, name: &str) {
    let p = f.path(&format!(".run/{name}"));
    let _ = std::fs::remove_file(&p);
    std::os::unix::net::UnixListener::bind(&p).unwrap();
}

/// What systemd's user manager keeps for a running unit:
/// `systemd/units/invocation:<unit>`, a link to its invocation id.
fn unit_record(f: &Fixture, unit: &str) {
    f.link(
        &format!(".run/systemd/units/invocation:{unit}"),
        Path::new("0123456789abcdef0123456789abcdef"),
    );
}

// ---- the binary ----

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn tidy(s: &str, f: &Fixture) -> String {
    undate(&redact(s, f))
}

/// Run the binary with `extra` added to the fixture's environment and
/// `answer` on stdin (`None`: stdin is closed at once, no answer at all).
fn run(f: &Fixture, args: &[&str], extra: &[(String, String)], answer: Option<&str>) -> Run {
    let mut cmd = common::ricepilot(f);
    cmd.envs(extra.iter().cloned())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(a) = answer {
        // A process that exits before asking closes the pipe; that is fine.
        let _ = stdin.write_all(format!("{a}\n").as_bytes());
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    Run {
        stdout: tidy(&String::from_utf8_lossy(&out.stdout), f),
        stderr: tidy(&String::from_utf8_lossy(&out.stderr), f),
        code: out.status.code().unwrap(),
    }
}

/// What the sandbox says when anything tries to start `uwsm`, as the binary
/// prints it.
fn the_sandbox_refusal(f: &Fixture) -> String {
    let e = exec::sandbox_refuses(Allowed::UwsmStop, |name| {
        (name == SANDBOX_VAR).then(|| OsString::from(SANDBOX_ROOT))
    })
    .expect("the sandbox refuses uwsm");
    tidy(&format!("ricepilot: {e}\n"), f)
}

/// Nothing was started: in the sandbox every attempt to start `uwsm` ends in
/// the R1 refusal on stderr and a non-zero exit, so an empty stderr and exit
/// 0 mean no attempt was made.
fn ran_nothing(r: &Run) {
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stderr, "", "something was started");
}

#[test]
fn a_dry_run_offers_no_logout() {
    let m = switching::build("rl_dry");
    let env = uwsm_session(&m.f);
    let r = run(&m.f, &["switch", "new", "--relogin"], &env, Some("y"));
    ran_nothing(&r);
    assert_eq!(m.live(), m.all_old(), "a dry run changes nothing");
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn a_switch_with_nothing_to_do_offers_no_logout() {
    let m = switching::build("rl_noop");
    let env = uwsm_session(&m.f);
    let r = run(
        &m.f,
        &["switch", "old", "--commit", "--relogin"],
        &env,
        Some("y"),
    );
    ran_nothing(&r);
    insta::assert_snapshot!(r.stdout);
}

#[test]
fn a_declined_switch_offers_no_logout() {
    let m = switching::build("rl_declined");
    let env = uwsm_session(&m.f);
    // An installer turned an owned link back into a directory (shape 3).
    m.f.clear(".config/foot");
    m.f.dir(".config/foot");
    let r = run(
        &m.f,
        &["switch", "new", "--commit", "--relogin"],
        &env,
        Some("y"),
    );
    assert_eq!(r.code, ExitCode::Refused as i32, "{}", r.stderr);
    assert_eq!(r.stderr, "");
    insta::assert_snapshot!(r.stdout);
}

/// The fixture's environment has no Wayland display, no session bus and no
/// uwsm unit: the switch completes and the logout is refused, with every
/// reason. The switch's own exit status stands (D58).
#[test]
fn outside_a_uwsm_session_the_switch_completes_and_no_logout_is_offered() {
    let m = switching::build("rl_outside");
    let r = run(&m.f, &["switch", "new", "--commit", "--relogin"], &[], None);
    ran_nothing(&r);
    assert_eq!(m.live(), m.all_new(), "the switch itself went ahead");
    assert!(!r.stdout.contains("[y/N]"), "a question was asked");
    insta::assert_snapshot!(r.stdout);
}

/// Every check passes, the question is asked with everything needed to
/// answer it above it, and "n" runs nothing.
#[test]
fn the_prompt_and_a_no() {
    let m = switching::build("rl_no");
    let env = uwsm_session(&m.f);
    let r = run(
        &m.f,
        &["switch", "new", "--commit", "--relogin"],
        &env,
        Some("n"),
    );
    ran_nothing(&r);
    assert_eq!(m.live(), m.all_new());
    assert!(r.stdout.contains(&format!(
        "{} no\n",
        ricepilot::cli::confirm::question_text(relogin::QUESTION)
    )));
    insta::assert_snapshot!(r.stdout);
}

/// No answer at all — stdin closed — is a no (D47).
#[test]
fn no_answer_is_no() {
    let m = switching::build("rl_eof");
    let env = uwsm_session(&m.f);
    let r = run(
        &m.f,
        &["switch", "new", "--commit", "--relogin"],
        &env,
        None,
    );
    ran_nothing(&r);
    assert!(r.stdout.ends_with(relogin::NOT_CONFIRMED), "{}", r.stdout);
    for other in ["", "yes please", "Y E S", "sure"] {
        let m = switching::build("rl_eof");
        let env = uwsm_session(&m.f);
        let r = run(
            &m.f,
            &["switch", "new", "--commit", "--relogin"],
            &env,
            Some(other),
        );
        ran_nothing(&r);
        assert!(r.stdout.ends_with(relogin::NOT_CONFIRMED), "{other:?}");
    }
}

/// "y" goes all the way to the exec boundary: the checks are made a second
/// time, the token is made, `ops::exec::run` is called with `uwsm stop` —
/// and the test sandbox refuses it before anything is looked for, let alone
/// started. That refusal is the evidence the call was made.
#[test]
fn a_yes_reaches_the_sandbox_refusal() {
    let m = switching::build("rl_yes");
    let env = uwsm_session(&m.f);
    let r = run(
        &m.f,
        &["switch", "new", "--commit", "--relogin"],
        &env,
        Some("y"),
    );
    assert_eq!(r.code, ExitCode::Refused as i32, "{}", r.stderr);
    assert_eq!(r.stderr, the_sandbox_refusal(&m.f));
    assert!(r.stdout.ends_with(" yes\n"), "{}", r.stdout);
    assert_eq!(m.live(), m.all_new(), "the switch itself is complete");
    insta::assert_snapshot!(r.stdout);
}

/// `rollback` goes through the same switch path and gets the same offer.
#[test]
fn rollback_relogin_is_the_same_offer() {
    let m = switching::build("rl_rollback");
    let env = uwsm_session(&m.f);
    let r = run(&m.f, &["switch", "new", "--commit"], &env, None);
    assert_eq!(r.code, 0, "{}", r.stderr);

    let r = run(
        &m.f,
        &["rollback", "--commit", "--relogin"],
        &env,
        Some("n"),
    );
    ran_nothing(&r);
    assert_eq!(m.live(), m.all_old());
    insta::assert_snapshot!(r.stdout);

    // And a yes arrives at the same refusal.
    let r = run(&m.f, &["switch", "new", "--commit"], &env, None);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let r = run(
        &m.f,
        &["rollback", "--commit", "--relogin"],
        &env,
        Some("y"),
    );
    assert_eq!(r.code, ExitCode::Refused as i32, "{}", r.stderr);
    assert_eq!(r.stderr, the_sandbox_refusal(&m.f));
}

/// The answer can take minutes; the checks are made again after it. The
/// test waits for the question, replaces `rescue.sh` while it is being asked,
/// then answers yes: nothing is started, and the second check says why.
#[test]
fn the_checks_run_again_after_the_answer() {
    let m = switching::build("rl_recheck");
    let env = uwsm_session(&m.f);
    let mut cmd = common::ricepilot(&m.f);
    cmd.envs(env.iter().cloned())
        .args(["switch", "new", "--commit", "--relogin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    // Read until the question is on the screen (or the process is gone).
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).ends_with("[y/N] ") {
        let n = stdout.read(&mut buf).unwrap();
        assert!(n > 0, "no question: {}", String::from_utf8_lossy(&seen));
        seen.extend_from_slice(&buf[..n]);
    }

    std::fs::write(ricepilot::rescue::path(&m.f.state()), "#!/bin/sh\n").unwrap();

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"y\n").unwrap();
    drop(stdin);
    stdout.read_to_end(&mut seen).unwrap();
    let out = child.wait_with_output().unwrap();
    let r = Run {
        stdout: tidy(&String::from_utf8_lossy(&seen), &m.f),
        stderr: tidy(&String::from_utf8_lossy(&out.stderr), &m.f),
        code: out.status.code().unwrap(),
    };
    ran_nothing(&r);
    let tail = &r.stdout[r.stdout.find(" yes\n").unwrap()..];
    insta::assert_snapshot!(tail);
}

// ---- the pre-flight, in process, against a machine tampered with after the switch ----

fn paths_of(f: &Fixture) -> Paths {
    Paths {
        home: f.home.clone(),
        data: f.path(".local/share/ricepilot"),
        state: f.state(),
        runtime: Some(f.path(".run")),
    }
}

/// Switch `m` to `profile` in process and keep the proof that it completed —
/// which also keeps the lock.
fn switched(m: &switching::Machine, profile: &str) -> Box<Completed> {
    let paths = paths_of(&m.f);
    let req = ricepilot::cli::switch_request(&paths, profile).unwrap();
    match ricepilot::cli::switch::run(&paths, &req, true)
        .unwrap()
        .ended
    {
        Ended::Completed(c) => c,
        _ => panic!("the switch to {profile} did not complete"),
    }
}

/// The environment of a process in the fixture's session, as a table: the
/// sandbox, the fixture's runtime directory and whatever `extra` adds.
fn table(f: &Fixture, extra: &[(&str, String)]) -> Vec<(String, String)> {
    let mut v = vec![
        (SANDBOX_VAR.to_string(), SANDBOX_ROOT.to_string()),
        (
            "XDG_RUNTIME_DIR".to_string(),
            f.path(".run").display().to_string(),
        ),
    ];
    v.extend(session_env(f));
    for (k, val) in extra {
        v.retain(|(name, _)| name != k);
        if !val.is_empty() {
            v.push((k.to_string(), val.clone()));
        }
    }
    v
}

fn lookup(v: &[(String, String)]) -> impl Fn(&str) -> Option<OsString> + '_ {
    move |name| {
        v.iter()
            .find(|(k, _)| k == name)
            .map(|(_, val)| OsString::from(val))
    }
}

fn refused(c: Box<Completed>, env: &[(String, String)]) -> Declined {
    match relogin::preflight(c, &lookup(env)) {
        Ok(_) => panic!("the pre-flight passed"),
        Err(d) => d,
    }
}

fn failed(d: &Declined) -> Vec<&'static str> {
    d.checks.iter().filter(|c| !c.ok).map(|c| c.what).collect()
}

fn shown(d: &Declined, f: &Fixture) -> String {
    tidy(&relogin::declined(d, false), f).replace(SANDBOX_ROOT, "<SANDBOX>")
}

/// A machine one in-process switch into `new`, in a session, with every
/// check passing — the starting point each tamper test changes one thing of.
fn ready(case: &str) -> (switching::Machine, Box<Completed>, Vec<(String, String)>) {
    let m = switching::build(case);
    uwsm_session(&m.f);
    let c = switched(&m, "new");
    let env = table(&m.f, &[]);
    (m, c, env)
}

#[test]
fn untouched_every_check_passes() {
    let (m, c, env) = ready("rlp_ok");
    let cleared = match relogin::preflight(c, &lookup(&env)) {
        Ok(c) => c,
        Err(d) => panic!("{}", relogin::declined(&d, false)),
    };
    assert!(cleared.checks().iter().all(|c| c.ok));
    insta::assert_snapshot!(tidy(&relogin::cleared_text(&cleared), &m.f));
}

#[test]
fn refused_when_the_generation_is_not_current() {
    let (m, c, env) = ready("rlp_current");
    ricepilot::generations::set_current(&m.f.state(), 0).unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["switch"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

#[test]
fn refused_when_an_operation_is_in_flight() {
    let (m, c, env) = ready("rlp_inflight");
    std::fs::write(paths_of(&m.f).journal_path(), "").unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["journal"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

#[test]
fn refused_when_the_journal_was_not_retired() {
    let (m, c, env) = ready("rlp_notretired");
    let done = c.journal_done().to_path_buf();
    std::fs::rename(&done, done.with_extension("moved")).unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["journal"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

#[test]
fn refused_when_rescue_is_missing() {
    let (m, c, env) = ready("rlp_rescue_gone");
    let p = c.rescue().to_path_buf();
    std::fs::rename(&p, p.with_extension("moved")).unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["rescue"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

#[test]
fn refused_when_rescue_is_not_a_file() {
    let (m, c, env) = ready("rlp_rescue_dir");
    let p = c.rescue().to_path_buf();
    std::fs::rename(&p, p.with_extension("moved")).unwrap();
    std::fs::create_dir(&p).unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["rescue"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

/// A script that parses but is not the one for the generation before.
#[test]
fn refused_when_rescue_is_not_the_script_for_this_generation() {
    let (m, c, env) = ready("rlp_rescue_other");
    std::fs::write(
        c.rescue(),
        "#!/bin/sh\n# ricepilot rescue script — restores generation 0007.\n",
    )
    .unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["rescue"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

/// Neither the script nor parseable. What `sh` says is not shown — it is
/// bash here and dash on CI.
#[test]
fn refused_when_rescue_does_not_parse() {
    let (m, c, env) = ready("rlp_rescue_syntax");
    std::fs::write(c.rescue(), "if true; then\n").unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["rescue"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

#[test]
fn refused_when_a_link_was_re_pointed() {
    let (m, c, env) = ready("rlp_repointed");
    m.f.link(".config/hypr", &m.old_root.join("hypr"));
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["links"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

/// Same target, different inode: the ledger's `(dev, ino)` is what tells
/// them apart.
#[test]
fn refused_when_a_link_was_replaced_by_a_look_alike() {
    let (m, c, env) = ready("rlp_lookalike");
    // Made beside it and renamed over it, so the old inode is still in use
    // when the new one is allocated and the two cannot share a number.
    let beside = m.f.path(".config/foot.lookalike");
    std::os::unix::fs::symlink(m.new_root.join("foot"), &beside).unwrap();
    std::fs::rename(&beside, &m.foot).unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["links"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

#[test]
fn refused_when_a_link_became_a_directory() {
    let (m, c, env) = ready("rlp_installer");
    m.f.clear(".config/btop");
    m.f.dir(".config/btop");
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["links"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

/// Switching back to `old` retires `btop`; something then puts a directory
/// where the plan left nothing.
#[test]
fn refused_when_a_retired_destination_is_occupied_again() {
    let m = switching::build("rlp_retired");
    uwsm_session(&m.f);
    drop(switched(&m, "new"));
    let c = switched(&m, "old");
    assert_eq!(c.retired(), std::slice::from_ref(&m.btop));
    m.f.dir(".config/btop");
    let d = refused(c, &table(&m.f, &[]));
    assert_eq!(failed(&d), ["links"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

/// The lock file replaced under the held lock: another ricepilot would lock
/// the new file and think itself alone.
#[test]
fn refused_when_the_lock_file_was_replaced() {
    let (m, c, env) = ready("rlp_lock");
    let lock = c.lock().path().to_path_buf();
    let fresh = lock.with_extension("fresh");
    std::fs::write(&fresh, "").unwrap();
    std::fs::rename(&fresh, &lock).unwrap();
    let d = refused(c, &env);
    assert_eq!(failed(&d), ["lock"]);
    insta::assert_snapshot!(shown(&d, &m.f));
}

/// The session check, one environment at a time. Each case builds its own
/// machine, since a refusal releases the lock and the proof with it.
#[test]
fn every_session_refusal() {
    type Setup = fn(&Fixture) -> Vec<(&'static str, String)>;
    let cases: &[(&str, Setup)] = &[
        ("nothing set: a TTY, ssh, a timer", |_| {
            vec![
                ("XDG_RUNTIME_DIR", String::new()),
                ("WAYLAND_DISPLAY", String::new()),
                ("DBUS_SESSION_BUS_ADDRESS", String::new()),
            ]
        }),
        ("no WAYLAND_DISPLAY", |_| {
            vec![("WAYLAND_DISPLAY", String::new())]
        }),
        ("no session bus", |_| {
            vec![("DBUS_SESSION_BUS_ADDRESS", String::new())]
        }),
        (
            "the runtime directory is the real one, outside the sandbox",
            |_| vec![("XDG_RUNTIME_DIR", "/run/user/1000".to_string())],
        ),
        ("WAYLAND_DISPLAY is a path", |_| {
            vec![("WAYLAND_DISPLAY", "/elsewhere/wayland-0".to_string())]
        }),
        ("WAYLAND_DISPLAY names no socket", |_| {
            vec![("WAYLAND_DISPLAY", "wayland-9".to_string())]
        }),
        ("WAYLAND_DISPLAY names a regular file", |f| {
            f.file(".run/wayland-file", "");
            vec![("WAYLAND_DISPLAY", "wayland-file".to_string())]
        }),
        ("no uwsm unit: a session uwsm did not start", |f| {
            let _ = std::fs::remove_file(f.path(&format!(".run/systemd/units/invocation:{UNIT}")));
            unit_record(f, "some-other.service");
            vec![]
        }),
        ("two uwsm units", |f| {
            unit_record(f, "wayland-wm@sway.desktop.service");
            vec![]
        }),
    ];
    let mut s = String::new();
    for (i, (name, setup)) in cases.iter().enumerate() {
        let (m, c, _) = ready(&format!("rlp_session_{i}"));
        let extra = setup(&m.f);
        let d = refused(c, &table(&m.f, &extra));
        assert_eq!(failed(&d), ["session"], "{name}");
        let session: Vec<Check> = d
            .checks
            .iter()
            .filter(|c| c.what == "session")
            .cloned()
            .collect();
        s.push_str(&format!(
            "--- {name}\n{}\n",
            tidy(&relogin::checks_block(&session), &m.f).replace(SANDBOX_ROOT, "<SANDBOX>")
        ));
    }
    insta::assert_snapshot!(s);
}

// ---- the decision alone, for the reads that fail ----

/// Every check's wording for a read that failed, from facts written by hand
/// — the branches a fixture cannot reach without breaking the filesystem.
#[test]
fn every_unreadable_fact_is_a_refusal() {
    use relogin::*;
    let e = || Err::<(), String>("<the error>".into());
    let facts = Facts {
        generation: 3,
        switch: SwitchFacts {
            current: Err("<the error>".into()),
        },
        journal: JournalFacts {
            in_flight_path: PathBuf::from("/s/journal/current.toml"),
            in_flight: Ok(false),
            done_path: PathBuf::from("/s/journal/done-x.toml"),
            done: Err("<the error>".into()),
        },
        rescue: RescueFacts {
            path: PathBuf::from("/s/rescue.sh"),
            restores: 2,
            found: RescueFound::Read {
                is_the_script: Err("<the error>".into()),
                parses: Err("<the error>".into()),
            },
        },
        dests: DestsFacts {
            ledger: e(),
            each: vec![],
        },
        lock: LockFacts {
            path: PathBuf::from("/r/ricepilot.lock"),
            at_its_path: Err("<the error>".into()),
        },
        session: SessionFacts {
            sandbox: None,
            runtime: Some(PathBuf::from("/r")),
            wayland: Some("wayland-1".into()),
            socket: Some(Err("<the error>".into())),
            bus: true,
            units: Some(Err("<the error>".into())),
        },
    };
    let checks = decide(&facts);
    assert!(checks.iter().all(|c| !c.ok));
    let mut s = checks_block(&checks);

    // And the other shapes a destination and the rescue script can take.
    let facts = Facts {
        rescue: RescueFacts {
            found: RescueFound::Unreadable("<the error>".into()),
            ..facts.rescue.clone()
        },
        dests: DestsFacts {
            ledger: Ok(()),
            each: vec![
                DestFacts {
                    dest: PathBuf::from("/h/.config/a"),
                    want: Some(PathBuf::from("/p/a")),
                    found: Found::Unreadable("<the error>".into()),
                    ledger: None,
                },
                DestFacts {
                    dest: PathBuf::from("/h/.config/b"),
                    want: Some(PathBuf::from("/p/b")),
                    found: Found::Link {
                        target: PathBuf::from("/p/b"),
                        dev: 1,
                        ino: 2,
                    },
                    ledger: None,
                },
                DestFacts {
                    dest: PathBuf::from("/h/.config/c"),
                    want: None,
                    found: Found::Absent,
                    ledger: Some((PathBuf::from("/p/c"), 1, 3)),
                },
                DestFacts {
                    dest: PathBuf::from("/h/.config/d"),
                    want: Some(PathBuf::from("/p/d")),
                    found: Found::Absent,
                    ledger: None,
                },
            ],
        },
        lock: LockFacts {
            at_its_path: Ok(true),
            ..facts.lock.clone()
        },
        ..facts
    };
    s.push_str("---\n");
    s.push_str(&checks_block(&decide(&facts)));
    insta::assert_snapshot!(s);
}
