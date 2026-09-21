//! Retirement: what happens to a destination ricepilot owns and the new target
//! state does not include.
//!
//! D21 left this open. A link created at a destination that was absent has no
//! exact inverse, because making something absent again is a removal and there
//! is none outside `src/gc/`. The answer is to displace it into the attic —
//! and the reason this suite exists is that "displace it" has to be planned,
//! journalled and recoverable like every other effect, not bolted onto
//! `rollback` as a special case.

mod common;

use std::path::PathBuf;

use common::Fixture;
use ricepilot::journal::{Direction, Journal, Side};
use ricepilot::ledger::Ledger;
use ricepilot::observe::{observe, Observed, Ownership};
use ricepilot::ops::mutate::{self, ApplyContext, ExchangeMode};
use ricepilot::ops::read;
use ricepilot::plan::{plan, Op, Plan, PlanContext, Target};

const ID: &str = "20260921T101112Z";

struct World {
    f: Fixture,
    root: PathBuf,
    two: PathBuf,
    hypr: PathBuf,
    btop: PathBuf,
    attic: PathBuf,
    own: Ownership,
}

/// Two owned links into profile `one`, and a profile `two` to switch to.
/// `hypr` moves across; `btop` is the destination the new target state drops.
fn world(case: &str) -> World {
    let f = Fixture::new_in("m3", case);
    let root = f.dir("rice/one");
    let two = f.dir("rice/two");
    f.dir("rice/one/hypr");
    f.dir("rice/one/btop");
    f.dir("rice/two/hypr");
    let hypr = f.link(".config/hypr", &root.join("hypr"));
    let btop = f.link(".config/btop", &root.join("btop"));

    let mut ledger = Ledger::default();
    ledger.record(&[hypr.clone(), btop.clone()], "one").unwrap();
    let own = ledger.ownership(vec![root.clone(), two.clone()]);
    let attic = f.attic(ID);

    World {
        f,
        root,
        two,
        hypr,
        btop,
        attic,
        own,
    }
}

impl World {
    fn observe(&self, dests: &[PathBuf]) -> Vec<Observed> {
        observe(dests, &self.own).unwrap()
    }

    fn ctx(&self, retire: Vec<PathBuf>) -> PlanContext {
        let dev = read::dev_of_nearest_existing_ancestor(&self.attic).unwrap();
        PlanContext::new(self.f.home.clone(), self.attic.clone(), dev).retiring(retire)
    }
}

/// The plan for an ordinary switch that also drops a destination: `hypr` moves
/// from profile `one` to profile `two`, and `btop` stops being managed.
fn retire_only(w: &World) -> (Vec<Observed>, Vec<Target>, Plan) {
    let targets = vec![Target {
        dest: w.hypr.clone(),
        src: w.two.join("hypr"),
    }];
    let observed = w.observe(&[w.hypr.clone(), w.btop.clone()]);
    let p = plan(&observed, &targets, &w.ctx(vec![w.btop.clone()]));
    (observed, targets, p)
}

#[test]
fn a_retired_destination_is_displaced_into_the_attic() {
    let w = world("retire_plans");
    let (_obs, _t, p) = retire_only(&w);

    let Plan::Apply { ops } = &p else {
        panic!("expected an applicable plan, got {p:?}");
    };
    assert!(ops.contains(&Op::RenameToAttic {
        from: w.btop.clone(),
        attic_rel: w.btop.strip_prefix("/").unwrap().to_path_buf(),
    }));
    assert!(
        !ops.iter()
            .any(|o| matches!(o, Op::Exchange { dest, .. } if dest == &w.btop)),
        "a retirement is a displacement, not a swap"
    );
}

/// The whole of retirement's claim: the link is somewhere else afterwards, not
/// gone. `switch` reports the attic path and this asserts it holds the link.
#[test]
fn applying_a_retirement_moves_the_link_and_removes_nothing() {
    let w = world("retire_applies");
    let (_obs, _t, p) = retire_only(&w);
    let Plan::Apply { ops } = &p else {
        panic!("expected an applicable plan")
    };

    let ctx = ApplyContext {
        mode: ExchangeMode::Renameat2,
        attic: w.attic.clone(),
    };
    mutate::apply(ops, &ctx, &mut |_| Ok(())).unwrap();

    assert_eq!(
        read::lstat(&w.btop).unwrap(),
        None,
        "the destination is empty"
    );
    let in_attic = w.attic.join(w.btop.strip_prefix("/").unwrap());
    assert_eq!(
        read::readlink(&in_attic).unwrap(),
        w.root.join("btop"),
        "and the link itself is in the attic, intact"
    );
    // The profile tree it pointed at was never touched.
    assert!(read::lstat(&w.root.join("btop")).unwrap().is_some());
}

/// A destination named by both lists is being switched, not retired. Treating
/// it as a retirement would displace the link the switch just staged.
#[test]
fn a_destination_in_both_lists_is_switched_not_retired() {
    let w = world("retire_both");
    let targets = vec![Target {
        dest: w.hypr.clone(),
        src: w.two.join("hypr"),
    }];
    let observed = w.observe(std::slice::from_ref(&w.hypr));
    let p = plan(&observed, &targets, &w.ctx(vec![w.hypr.clone()]));

    let Plan::Apply { ops } = &p else {
        panic!("expected an applicable plan, got {p:?}")
    };
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::Exchange { dest, .. } if dest == &w.hypr)));
    assert!(
        !ops.contains(&Op::RenameToAttic {
            from: w.hypr.clone(),
            attic_rel: w.hypr.strip_prefix("/").unwrap().to_path_buf(),
        }),
        "the switched link must not also be displaced"
    );
}

/// Something else at a path ricepilot believed it owned is refused for exactly
/// the reason a switch onto one is: it did not put it there.
#[test]
fn retiring_a_path_that_is_no_longer_ours_is_refused() {
    let w = world("retire_foreign");
    let elsewhere = w.f.dir("rice/other/btop");
    w.f.link(".config/btop", &elsewhere);

    let observed = w.observe(std::slice::from_ref(&w.btop));
    let p = plan(&observed, &[], &w.ctx(vec![w.btop.clone()]));
    let Plan::Decline { refusals } = &p else {
        panic!("expected a decline, got {p:?}")
    };
    assert_eq!(refusals.len(), 1);
    assert!(refusals[0].to_string().contains("did not create"));
}

/// Retiring something that is already gone is not an error and not an op. A
/// second `rollback` over the same generation is the ordinary way to reach it.
#[test]
fn retiring_a_destination_that_is_already_absent_is_a_no_op() {
    let w = world("retire_absent");
    w.f.clear(".config/btop");
    let observed = w.observe(std::slice::from_ref(&w.btop));
    assert_eq!(
        plan(&observed, &[], &w.ctx(vec![w.btop.clone()])),
        Plan::NoOp
    );
}

// ---------------------------------------------------------------------------
// Journal and recovery
// ---------------------------------------------------------------------------

fn journal_of(w: &World) -> (Vec<Op>, Journal) {
    let (observed, _t, p) = retire_only(w);
    let Plan::Apply { ops } = p else {
        panic!("expected an applicable plan")
    };
    let j = Journal::from_plan(
        ID,
        "one",
        w.attic.clone(),
        ExchangeMode::Renameat2,
        &ops,
        &observed,
    )
    .unwrap();
    (ops, j)
}

#[test]
fn the_journal_records_a_retirement_separately_from_an_exchange() {
    let w = world("retire_journal");
    let (_ops, j) = journal_of(&w);

    assert_eq!(j.retire.len(), 1);
    assert_eq!(j.retire[0].dest, w.btop);
    assert_eq!(j.retire[0].old_target, w.root.join("btop"));
    assert!(
        !j.entries.iter().any(|e| e.dest == w.btop),
        "a retirement has no new target, so it is not an Entry"
    );

    // And it survives a trip through the file, which is the only form recovery
    // ever sees it in.
    let path = w.f.state().join("journal").join("current.toml");
    ricepilot::journal::write(&path, &j).unwrap();
    assert_eq!(
        ricepilot::journal::read_current(&path).unwrap().as_ref(),
        Some(&j)
    );
}

/// Crash before the retirement happened: the destination still holds its link,
/// the exchange has taken effect, so recovery goes forward and finishes it.
#[test]
fn recovery_finishes_a_retirement_that_did_not_happen() {
    let w = world("retire_recover_forward");
    let (ops, j) = journal_of(&w);
    let path = w.f.state().join("journal").join("current.toml");
    ricepilot::journal::write(&path, &j).unwrap();

    // Apply everything except the retirement and the fsyncs that follow it.
    let upto: Vec<Op> = ops
        .iter()
        .take_while(|o| !matches!(o, Op::RenameToAttic { from, .. } if from == &w.btop))
        .cloned()
        .collect();
    let ctx = ApplyContext {
        mode: ExchangeMode::Renameat2,
        attic: w.attic.clone(),
    };
    mutate::apply(&upto, &ctx, &mut |_| Ok(())).unwrap();

    let r = ricepilot::journal::plan_recovery(&j).unwrap();
    assert_eq!(r.direction, Direction::Forward);
    assert!(r
        .statuses
        .iter()
        .any(|s| s.dest == w.btop && s.side == Side::Old));

    ricepilot::journal::execute(&r, &path, ExchangeMode::Renameat2, &w.attic).unwrap();
    assert_eq!(read::lstat(&w.btop).unwrap(), None);
}

/// And a second pass finds it already done and emits nothing for it, which is
/// what makes state-driven recovery idempotent (D23).
#[test]
fn a_retirement_already_carried_out_produces_no_action() {
    let w = world("retire_recover_idempotent");
    let (ops, j) = journal_of(&w);
    let ctx = ApplyContext {
        mode: ExchangeMode::Renameat2,
        attic: w.attic.clone(),
    };
    mutate::apply(&ops, &ctx, &mut |_| Ok(())).unwrap();

    let r = ricepilot::journal::plan_recovery(&j).unwrap();
    assert!(r
        .statuses
        .iter()
        .any(|s| s.dest == w.btop && s.side == Side::New));
    assert!(
        !r.actions.iter().any(
            |a| matches!(a, ricepilot::journal::Action::ToAttic { from, .. } if from == &w.btop)
        ),
        "recovery must not displace a link a second time"
    );
}

/// Something re-pointed the destination while ricepilot was down. Recovery
/// refuses and names the path rather than displacing a link nobody can account
/// for.
#[test]
fn a_retirement_target_that_changed_underneath_is_refused() {
    let w = world("retire_recover_foreign");
    let (_ops, j) = journal_of(&w);
    let elsewhere = w.f.dir("rice/other/btop");
    w.f.link(".config/btop", &elsewhere);

    let err = ricepilot::journal::plan_recovery(&j).unwrap_err();
    assert_eq!(err.exit_code(), ricepilot::error::ExitCode::Refused);
    assert!(err.to_string().contains(".config/btop"));
}

/// A switch whose *only* effect is a retirement, crashed before it happened.
///
/// No exchange took place — there were none to take place — so by D23 the
/// switch never started and is abandoned. That is the right answer and not an
/// oversight: the machine is fully old, which is one of the two outcomes R5
/// allows, and finishing a retirement nobody had begun would be recovery
/// inventing an effect rather than completing one.
#[test]
fn a_switch_that_is_only_a_retirement_and_never_started_is_abandoned() {
    let w = world("retire_only_abandoned");
    let observed = w.observe(std::slice::from_ref(&w.btop));
    let Plan::Apply { ops } = plan(&observed, &[], &w.ctx(vec![w.btop.clone()])) else {
        panic!("expected an applicable plan")
    };
    let j = Journal::from_plan(
        ID,
        "one",
        w.attic.clone(),
        ExchangeMode::Renameat2,
        &ops,
        &observed,
    )
    .unwrap();

    let r = ricepilot::journal::plan_recovery(&j).unwrap();
    assert_eq!(r.direction, Direction::Backward);
    assert!(
        !r.actions
            .iter()
            .any(|a| matches!(a, ricepilot::journal::Action::ToAttic { .. })),
        "an abandoned switch displaces nothing that was live"
    );

    ricepilot::journal::write(&w.f.state().join("journal").join("current.toml"), &j).unwrap();
    ricepilot::journal::execute(
        &r,
        &w.f.state().join("journal").join("current.toml"),
        ExchangeMode::Renameat2,
        &w.attic,
    )
    .unwrap();
    assert_eq!(
        read::readlink(&w.btop).unwrap(),
        w.root.join("btop"),
        "the destination is exactly as it was"
    );
}
