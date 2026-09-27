//! A fifo where ricepilot expects a regular file is refused, never opened
//! for a blocking read (red-team #2 finding 5). An `O_RDONLY` open of a fifo
//! waits for a writer for ever, so before `ops::read` looked first and
//! opened `O_NONBLOCK`, one planted at the ledger hung `status`, `doctor`
//! and `diff`.
//!
//! Every read here runs under a deadline: a regression fails the test
//! instead of hanging the suite.

// Builds fixture trees with the standard library; the guards it protects
// only ever scan src/.
#![allow(clippy::disallowed_methods)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{redact, switching, Fixture};
use ricepilot::ops::read;

const DEADLINE: Duration = Duration::from_secs(30);

fn fifo(p: &Path) -> PathBuf {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(p);
    rustix::fs::mknodat(
        rustix::fs::CWD,
        p,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_bits_truncate(0o600),
        0,
    )
    .unwrap();
    p.to_path_buf()
}

/// Run `f` on another thread and wait at most [`DEADLINE`] for it. A thread
/// stuck in `open(2)` cannot be stopped, but the test fails, and the process
/// exits when the harness does.
fn within_deadline<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("{what} did not return within {DEADLINE:?}: it blocked"))
}

fn refused_as_a_fifo(r: ricepilot::Result<impl std::fmt::Debug>) {
    match r {
        Err(ricepilot::Error::Refused { why, .. }) => {
            assert!(why.contains("this is a fifo"), "{why}")
        }
        other => panic!("expected a refusal naming a fifo, got {other:?}"),
    }
}

#[test]
fn slurp_and_read_into_refuse_a_fifo_without_blocking() {
    let f = Fixture::new_in("fifo", "ops");
    let p = fifo(&f.path("planted.toml"));

    let q = p.clone();
    refused_as_a_fifo(within_deadline("slurp", move || read::slurp(&q)));
    let q = p.clone();
    refused_as_a_fifo(within_deadline("read_into", move || {
        read::read_into(&q, &mut |_| {})
    }));
}

#[test]
fn a_directory_is_not_read_as_a_file_either() {
    let f = Fixture::new_in("fifo", "dir");
    let d = f.dir("not-a-file");
    match read::slurp(&d) {
        Err(ricepilot::Error::Refused { why, .. }) => {
            assert!(why.contains("this is a directory"), "{why}")
        }
        other => panic!("expected a refusal naming a directory, got {other:?}"),
    }
}

#[test]
fn a_regular_file_and_absence_read_as_before() {
    let f = Fixture::new_in("fifo", "regular");
    let p = f.file("plain.toml", "a = 1\n");
    assert_eq!(read::slurp(&p).unwrap(), "a = 1\n");
    match read::slurp(&f.path("absent.toml")) {
        Err(ricepilot::Error::Io { source, .. }) => {
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound)
        }
        other => panic!("absence must still be NotFound, got {other:?}"),
    }
}

/// The real binary, each read-only command, a fifo where the ledger is. Each
/// must finish — whatever it then says — and say what the thing is.
#[test]
fn status_doctor_and_diff_finish_with_a_fifo_for_a_ledger() {
    let m = switching::build("fifo-ledger");
    fifo(&m.f.state().join("ledger.toml"));

    let mut shown = String::new();
    for args in [&["status"][..], &["doctor"], &["diff", "old"]] {
        let mut child = common::ricepilot(&m.f)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() > DEADLINE {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`ricepilot {}` hung on a fifo ledger", args.join(" "));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains("this is a fifo"),
            "`ricepilot {}` did not name the fifo:\n{text}",
            args.join(" ")
        );
        if args == ["status"] {
            shown = format!(
                "exit {}\n{}",
                out.status.code().unwrap(),
                redact(&text, &m.f)
            );
        }
    }
    insta::assert_snapshot!(shown);
}
