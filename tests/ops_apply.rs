//! Applying a plan, and applying its inverse.
//!
//! The M2 gate asks for "apply-then-rollback restores byte-exact link topology
//! and leaves every pre-existing target's inode and mtime unchanged". The
//! `rollback` *command* is M3 (D20), so the property is proved here at the
//! level where it actually lives: a plan, its inverse, and the ops in between.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::scenario;
use ricepilot::ops::mutate::{self, ExchangeMode};
use ricepilot::ops::read;

const BOTH: [ExchangeMode; 2] = [ExchangeMode::Renameat2, ExchangeMode::Fallback];

/// Every inode and mtime in both profile trees. A switch re-points links; if
/// any of these moves, it did something else as well.
fn tree_identities(root: &std::path::Path) -> BTreeMap<PathBuf, (u64, u64, i64)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for name in read::list_dir(&dir).unwrap() {
            let p = dir.join(name);
            let m = read::lstat(&p).unwrap().unwrap();
            out.insert(p.clone(), (m.dev, m.ino, mutate::mtime_ns(&p).unwrap()));
            if m.kind == read::Kind::Dir {
                stack.push(p);
            }
        }
    }
    out
}

#[test]
fn a_plan_applies_to_exactly_the_topology_it_promised() {
    for mode in BOTH {
        let s = scenario::build(&format!("apply_forward_{}", mode.as_str()), mode);
        assert_eq!(s.live(), s.all_old());

        mutate::apply(&s.ops, &s.apply_ctx(), &mut |_| Ok(())).unwrap();

        assert_eq!(s.live(), s.all_new(), "{mode:?}");
    }
}

#[test]
fn every_displaced_link_is_in_the_attic_and_nothing_was_taken_away() {
    for mode in BOTH {
        let s = scenario::build(&format!("apply_attic_{}", mode.as_str()), mode);
        mutate::apply(&s.ops, &s.apply_ctx(), &mut |_| Ok(())).unwrap();

        for e in &s.journal.entries {
            let Some(rel) = &e.attic_rel else { continue };
            let landed = s.attic.join(rel);
            assert_eq!(
                read::readlink(&landed).unwrap(),
                e.old_target.clone().unwrap(),
                "{mode:?}: the displaced link must be in the attic, pointing where it did"
            );
        }
        // And the staging names are free again, because their contents moved.
        for e in &s.journal.entries {
            if let Some(staged) = &e.staged {
                assert!(read::lstat(staged).unwrap().is_none(), "{mode:?}");
            }
        }
    }
}

#[test]
fn apply_then_the_inverse_restores_byte_exact_link_topology() {
    for mode in BOTH {
        // Only exchanged destinations: a destination that started absent has
        // no exact inverse without a delete primitive, which is D21's subject
        // and M3's problem.
        let s = scenario::build_links_only(&format!("apply_inverse_{}", mode.as_str()), mode);

        let topology_before = s.live();
        let old_before = tree_identities(&s.old_root);
        let new_before = tree_identities(&s.new_root);

        mutate::apply(&s.ops, &s.apply_ctx(), &mut |_| Ok(())).unwrap();
        assert_eq!(s.live(), s.all_new(), "{mode:?}");

        let back_attic = s.f.attic("20260921T101113Z");
        let inverse = s.inverse_ops(&back_attic);
        let ctx = mutate::ApplyContext {
            mode,
            attic: back_attic,
        };
        mutate::apply(&inverse, &ctx, &mut |_| Ok(())).unwrap();

        assert_eq!(
            s.live(),
            topology_before,
            "{mode:?}: the inverse must restore the exact link targets"
        );
        assert_eq!(
            tree_identities(&s.old_root),
            old_before,
            "{mode:?}: no inode or mtime in the old profile may move"
        );
        assert_eq!(
            tree_identities(&s.new_root),
            new_before,
            "{mode:?}: nor in the new one"
        );
    }
}

#[test]
fn an_error_mid_apply_leaves_the_state_a_crash_would_leave() {
    // There is no cleanup on error and no unwinding rollback, deliberately: if
    // an error tidied up after itself, the recovery path would only ever run
    // after a real crash and would therefore be the one path never exercised.
    let s = scenario::build("apply_no_cleanup", ExchangeMode::Renameat2);
    let staged: Vec<PathBuf> = s
        .journal
        .entries
        .iter()
        .filter_map(|e| e.staged.clone())
        .collect();

    let err = mutate::apply(&s.ops, &s.apply_ctx(), &mut |i| {
        if i == 0 {
            Err(ricepilot::Error::Refused {
                rule: "test",
                path: PathBuf::from("/"),
                why: "stop".into(),
            })
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(err.to_string().contains("stop"));

    assert!(
        read::lstat(&staged[0]).unwrap().is_some(),
        "the staged link from step 0 is still there, exactly as a crash would leave it"
    );
    assert_eq!(s.live(), s.all_old(), "and nothing has been switched yet");
}
