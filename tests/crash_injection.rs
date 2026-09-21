//! The M2 gate: abort after step *k*, for every *k*, then `recover`, and
//! assert the machine is fully old or fully new — never mixed.
//!
//! Run twice over, both exchange modes each time:
//!
//! * in-process, stopping the executor after step *k*. Exhaustive and cheap.
//!   `apply` has no cleanup on error, so stopping it leaves exactly what a
//!   crash at the same point leaves.
//! * out of process, in a helper that calls `std::process::abort()` after step
//!   *k*, so the claim also covers "the journal was on disk before it was
//!   needed" rather than only "the in-memory state was discarded".

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::scenario::{self, Scenario};
use ricepilot::journal::{self, Direction, Side};
use ricepilot::ops::mutate::{self, ExchangeMode};
use ricepilot::ops::read;
use ricepilot::{Error, Result};

const BOTH: [ExchangeMode; 2] = [ExchangeMode::Renameat2, ExchangeMode::Fallback];

/// A switch as `switch --commit` will perform it in M3: journal first, made
/// durable, and only then the first effect (R4).
fn journal_then_apply(s: &Scenario, crash_after: Option<usize>) -> Result<()> {
    journal::write(&s.journal_path(), &s.journal)?;
    mutate::apply(&s.ops, &s.apply_ctx(), &mut |i| {
        if crash_after == Some(i) {
            Err(Error::Refused {
                rule: "test",
                path: PathBuf::from("/"),
                why: format!("simulated crash after step {i}"),
            })
        } else {
            Ok(())
        }
    })
}

fn recover(s: &Scenario) -> journal::Recovery {
    let j = journal::read_current(&s.journal_path())
        .unwrap()
        .expect("an interrupted switch leaves its journal in place");
    let r = journal::plan_recovery(&j).unwrap();
    journal::execute(&r, &s.journal_path(), j.mode().unwrap(), &j.attic).unwrap();
    r
}

#[test]
fn aborting_after_every_step_recovers_to_fully_old_or_fully_new() {
    for mode in BOTH {
        // One extra k than there are ops: the last iteration crashes after the
        // final step, which is the "everything happened but nothing recorded
        // it" case.
        let op_count = scenario::build(&format!("crash_count_{}", mode.as_str()), mode)
            .ops
            .len();

        for k in 0..op_count {
            let case = format!("crash_{}_{k}", mode.as_str());
            let s = scenario::build(&case, mode);

            let err = journal_then_apply(&s, Some(k)).unwrap_err();
            assert!(err.to_string().contains("simulated crash"), "{err}");

            let mid = s.live();
            let r = recover(&s);
            let after = s.live();

            assert!(
                after == s.all_old() || after == s.all_new(),
                "{mode:?} k={k}: recovery left a mixed machine.\n  mid: {mid:?}\n  after: {after:?}"
            );

            // And the direction matches what was observed, rather than being
            // whatever fell out.
            match r.direction {
                Direction::Forward => assert_eq!(after, s.all_new(), "{mode:?} k={k}"),
                Direction::Backward => assert_eq!(after, s.all_old(), "{mode:?} k={k}"),
            }

            // Nothing was taken away, at any k.
            assert_no_link_was_lost(&s, r.direction, mode, k);

            // The journal is retired, so a second `recover` has nothing to do.
            assert_eq!(
                journal::read_current(&s.journal_path()).unwrap(),
                None,
                "{mode:?} k={k}: a completed recovery retires its journal"
            );
        }
    }
}

/// Every link that existed before the switch is still reachable afterwards:
/// either live at its destination, or sitting in the attic.
fn assert_no_link_was_lost(s: &Scenario, dir: Direction, mode: ExchangeMode, k: usize) {
    for e in &s.journal.entries {
        let Some(old) = &e.old_target else { continue };
        let live = mutate::link_target(&e.dest).unwrap().flatten();
        if live.as_deref() == Some(old.as_path()) {
            continue;
        }
        let found = attic_holds(&s.attic, old);
        assert!(
            found,
            "{mode:?} k={k} {dir:?}: the link {} -> {} is neither live nor in the attic",
            e.dest.display(),
            old.display()
        );
    }
}

fn attic_holds(attic: &std::path::Path, target: &std::path::Path) -> bool {
    let mut stack = vec![attic.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for name in read::list_dir(&dir).unwrap() {
            let p = dir.join(name);
            let m = read::lstat(&p).unwrap().unwrap();
            match m.kind {
                read::Kind::Dir => stack.push(p),
                read::Kind::Symlink if read::readlink(&p).unwrap() == target => return true,
                _ => {}
            }
        }
    }
    false
}

#[test]
fn recovery_is_idempotent_because_it_reads_state_rather_than_steps() {
    for mode in BOTH {
        let s = scenario::build(&format!("crash_idempotent_{}", mode.as_str()), mode);
        journal_then_apply(&s, Some(1)).unwrap_err();

        let j = journal::read_current(&s.journal_path()).unwrap().unwrap();
        let first = journal::plan_recovery(&j).unwrap();
        journal::execute(&first, &s.journal_path(), mode, &j.attic).unwrap();
        let after_first = s.live();

        // Planning again against the *same* journal, now that it has been
        // carried out: a step-driven replay would re-run the recorded renames.
        // A state-driven one finds every destination already where it was
        // driven to and has nothing left but the fsyncs.
        let second = journal::plan_recovery(&j).unwrap();
        assert!(
            second
                .actions
                .iter()
                .all(|a| matches!(a, journal::Action::FsyncDir { .. })),
            "{mode:?}: a second pass wanted to do real work: {:?}",
            second.actions
        );
        journal::execute(&second, &after_first_journal(&s), mode, &j.attic).ok();
        assert_eq!(s.live(), after_first, "{mode:?}");
    }
}

/// The journal was retired by the first pass; re-write it so the second
/// `execute` has something to retire, without changing what it recovers.
fn after_first_journal(s: &Scenario) -> PathBuf {
    let p = s.journal_path();
    journal::write(&p, &s.journal).unwrap();
    p
}

#[test]
fn a_crash_before_the_first_exchange_abandons_the_switch() {
    for mode in BOTH {
        let s = scenario::build(&format!("crash_backward_{}", mode.as_str()), mode);
        // Step 0 is the first staging link; no exchange has happened.
        journal_then_apply(&s, Some(0)).unwrap_err();

        let r = recover(&s);
        assert_eq!(r.direction, Direction::Backward, "{mode:?}");
        assert!(r.statuses.iter().all(|st| st.side == Side::Old), "{mode:?}");
        assert_eq!(s.live(), s.all_old(), "{mode:?}");

        // The abandoned staged link went to the attic, not away.
        assert!(
            attic_holds(&s.attic, &s.new_root.join("hypr")),
            "{mode:?}: the staged link is displaced, never removed"
        );
    }
}

#[test]
fn a_foreign_link_appearing_mid_switch_is_refused_not_guessed_at() {
    let s = scenario::build("crash_foreign", ExchangeMode::Renameat2);
    journal_then_apply(&s, Some(1)).unwrap_err();

    // Something else re-pointed a managed destination while ricepilot was
    // down. It is now neither the profile it was on nor the one it was moving
    // to, and no amount of reading the journal says what happened to it.
    let dest = &s.journal.entries[0].dest;
    let elsewhere = s.f.dir("rice/other/hypr");
    let attic_rel = PathBuf::from("displaced-by-test");
    mutate::rename_to_attic(dest, &s.attic, &attic_rel).unwrap();
    mutate::create_symlink(dest, &elsewhere).unwrap();

    let j = journal::read_current(&s.journal_path()).unwrap().unwrap();
    let err = journal::plan_recovery(&j).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("will not guess what happened to it"), "{msg}");
    assert!(msg.contains(&dest.display().to_string()), "{msg}");

    // Zero side effects: the refusal happened during planning (R4).
    assert_eq!(read::readlink(dest).unwrap(), elsewhere);
    assert!(journal::read_current(&s.journal_path()).unwrap().is_some());
}

#[test]
fn the_out_of_process_abort_reaches_the_same_conclusion() {
    // The in-process runs prove recovery handles each intermediate state. This
    // proves the journal reached the disk before the state it describes did:
    // the helper really aborts, so nothing it held in memory survives to help.
    for mode in BOTH {
        let op_count = scenario::build(&format!("abort_count_{}", mode.as_str()), mode)
            .ops
            .len();

        for k in 0..op_count {
            let case = format!("abort_{}_{k}", mode.as_str());
            let out = Command::new(helper())
                .arg(&case)
                .arg(mode.as_str())
                .arg(k.to_string())
                .output()
                .expect("the crash helper must run");
            assert!(
                !out.status.success(),
                "{mode:?} k={k}: the helper was supposed to abort, it exited {:?}",
                out.status
            );

            let s = scenario::attach(&case, mode);
            let j = journal::read_current(&s.journal_path())
                .unwrap()
                .expect("the journal was fsync'd before the first effect");
            let r = journal::plan_recovery(&j).unwrap();
            journal::execute(&r, &s.journal_path(), mode, &j.attic).unwrap();

            let after = s.live();
            assert!(
                after == s.all_old() || after == s.all_new(),
                "{mode:?} k={k}: a real abort left a mixed machine: {after:?}"
            );
            match r.direction {
                Direction::Forward => assert_eq!(after, s.all_new(), "{mode:?} k={k}"),
                Direction::Backward => assert_eq!(after, s.all_old(), "{mode:?} k={k}"),
            }
        }
    }
}

/// `examples/crash_switch`.
///
/// It stays an example rather than becoming a `[[bin]]`: a helper whose whole
/// job is to abort a switch half way through has no business in anything
/// `cargo install` puts on a machine. The cost is that `cargo test` does not
/// build it and gives it no `CARGO_BIN_EXE_*`, so it is built on demand — once
/// per run — through the `cargo` that invoked us.
fn helper() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = Command::new(cargo)
                .args(["build", "--locked", "--example", "crash_switch"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .status()
                .expect("building the crash helper");
            assert!(status.success(), "the crash helper must build");

            let mut dir = std::env::current_exe().expect("test binary path");
            dir.pop(); // deps/
            dir.pop(); // debug/
            dir.join("examples").join("crash_switch")
        })
        .clone()
}
