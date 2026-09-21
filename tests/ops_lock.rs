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
