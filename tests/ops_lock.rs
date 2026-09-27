//! The process lock. `flock` attaches to the open file description, so a
//! second `open` + `flock` from *this* process is refused exactly as another
//! ricepilot's would be — which is what makes this testable without spawning
//! anything.

mod common;

use common::Fixture;
use ricepilot::cli::paths::Paths;
use ricepilot::error::ExitCode;
use ricepilot::ops::lock;

#[test]
fn a_second_holder_is_refused_and_exits_locked() {
    let f = Fixture::new_in("m2", "lock_contention");
    let path = f.path(".run/ricepilot.lock");

    let held = lock::acquire(&path).unwrap();
    assert_eq!(held.path(), path);

    let err = lock::acquire(&path).unwrap_err();
    assert_eq!(err.exit_code(), ExitCode::Locked);
    assert!(err.to_string().contains("another ricepilot holds"), "{err}");

    // Releasing is dropping: the lock's lifetime is the value's lifetime.
    drop(held);
    lock::acquire(&path).unwrap();
}

#[test]
fn the_lock_file_is_created_with_its_directory() {
    let f = Fixture::new_in("m2", "lock_mkdir");
    let path = f.path(".run/deeper/ricepilot.lock");
    let _held = lock::acquire(&path).unwrap();
    assert!(ricepilot::ops::read::lstat(&path).unwrap().is_some());
}

#[test]
fn without_a_runtime_dir_there_is_no_lock_path_to_invent() {
    // A lock in a location no other ricepilot would look in is worse than no
    // lock: it reads as mutual exclusion and provides none.
    let paths = Paths {
        home: "/nonexistent".into(),
        data: "/nonexistent/data".into(),
        state: "/nonexistent/state".into(),
        runtime: None,
    };
    let err = paths.lock_path().unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("XDG_RUNTIME_DIR"), "{msg}");
    assert_eq!(err.exit_code(), ExitCode::Refused);
}

fn fifo_at(p: &std::path::Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        p,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_bits_truncate(0o600),
        0,
    )
    .unwrap();
}

/// `ricepilot recover`, which takes the lock even for its dry run: its exit
/// code and what it printed, the fixture redacted.
fn recover_shown(f: &Fixture) -> String {
    let out = common::ricepilot(f).arg("recover").output().unwrap();
    common::redact(
        &format!(
            "exit {}\n{}{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        f,
    )
}

/// A symlink planted at the lock's name is not followed (D74). Followed, the
/// `O_CREAT` open would have created a file wherever it pointed — here, a
/// file that does not exist yet, which is still absent afterwards.
#[test]
fn a_symlink_at_the_lock_path_is_refused_and_not_followed() {
    let f = Fixture::new_in("m2", "lock_symlink");
    let elsewhere = f.path("elsewhere/planted");
    f.dir("elsewhere");
    let lock_path = f.link(".run/ricepilot.lock", &elsewhere);

    let err = lock::acquire(&lock_path).unwrap_err();
    assert_eq!(err.exit_code(), ExitCode::Refused);
    assert!(err.to_string().contains("this is a symlink"), "{err}");
    assert!(ricepilot::ops::read::lstat(&elsewhere).unwrap().is_none());

    let shown = recover_shown(&f);
    assert!(ricepilot::ops::read::lstat(&elsewhere).unwrap().is_none());
    assert_eq!(
        ricepilot::ops::read::readlink(&lock_path).unwrap(),
        elsewhere,
        "the planted link is left as it was"
    );
    insta::assert_snapshot!(shown);
}

/// A fifo at the lock's name is refused without being opened, so nothing
/// can block on it either (D74).
#[test]
fn a_fifo_at_the_lock_path_is_refused() {
    let f = Fixture::new_in("m2", "lock_fifo");
    let lock_path = f.path(".run/ricepilot.lock");
    fifo_at(&lock_path);

    let err = lock::acquire(&lock_path).unwrap_err();
    assert_eq!(err.exit_code(), ExitCode::Refused);
    assert!(err.to_string().contains("this is a fifo"), "{err}");

    insta::assert_snapshot!(recover_shown(&f));
}

/// The lock file is still created, and created mode 0600, when nothing is
/// there — and a regular file already there is taken as it is.
#[test]
fn a_fresh_lock_file_is_a_regular_file_mode_0600() {
    let f = Fixture::new_in("m2", "lock_fresh");
    let lock_path = f.path(".run/ricepilot.lock");
    drop(lock::acquire(&lock_path).unwrap());
    let m = ricepilot::ops::read::lstat(&lock_path).unwrap().unwrap();
    assert_eq!(m.kind, ricepilot::ops::read::Kind::File);
    assert_eq!(m.mode & 0o777, 0o600);
    lock::acquire(&lock_path).unwrap();
}
