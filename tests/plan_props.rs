//! Properties of the planner. `plan` is pure, so these need no filesystem.

use std::path::PathBuf;

use proptest::prelude::*;
use ricepilot::observe::{Observed, Shape};
use ricepilot::plan::{plan, Plan, PlanContext, Target};

const DEV: u64 = 66;

fn ctx() -> PlanContext {
    PlanContext::new("/home/u", "/home/u/.local/state/ricepilot/attic/1", DEV)
}

/// Path components that cannot accidentally collide with the denylist, with
/// `~`, or with a `.`/`..` component.
fn component() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,7}"
}

fn owned(dest: PathBuf, target: PathBuf) -> Observed {
    Observed {
        dest,
        shape: Shape::OwnedLink { target },
        parent_dev: DEV,
        parent_fs_type: 0x9123683E,
        is_mountpoint: false,
    }
}

proptest! {
    /// The load-bearing property: planning to reach the state you are already
    /// in does nothing. If this ever fails, `switch` is not idempotent and
    /// re-running it after a partial failure is not safe.
    #[test]
    fn planning_the_current_state_is_a_noop(
        names in prop::collection::hash_set(component(), 0..6),
        src in component(),
    ) {
        let mut observed = Vec::new();
        let mut target = Vec::new();
        for name in &names {
            let dest = PathBuf::from(format!("/home/u/.config/{name}"));
            let t = PathBuf::from(format!("/home/u/.local/share/ricepilot/profiles/{src}/{name}"));
            observed.push(owned(dest.clone(), t.clone()));
            target.push(Target { dest, src: t });
        }
        prop_assert_eq!(plan(&observed, &target, &ctx()), Plan::NoOp);
    }

    /// A plan is never partial (`SAFETY.md` R5): if anything is refused, no
    /// operation is emitted at all.
    #[test]
    fn a_refusal_anywhere_suppresses_every_operation(
        good in prop::collection::hash_set(component(), 0..4),
        bad in component(),
    ) {
        prop_assume!(!good.contains(&bad));

        let mut observed = Vec::new();
        let mut target = Vec::new();
        for name in good.iter().chain(std::iter::once(&bad)) {
            let dest = PathBuf::from(format!("/home/u/.config/{name}"));
            let src = PathBuf::from(format!("/home/u/.local/share/ricepilot/profiles/new/{name}"));
            let shape = if name == &bad {
                Shape::RealDir
            } else {
                Shape::OwnedLink {
                    target: PathBuf::from(format!("/home/u/.local/share/ricepilot/profiles/old/{name}")),
                }
            };
            observed.push(Observed { dest: dest.clone(), shape, parent_dev: DEV, parent_fs_type: 0, is_mountpoint: false });
            target.push(Target { dest, src });
        }

        match plan(&observed, &target, &ctx()) {
            Plan::Decline { refusals } => prop_assert!(!refusals.is_empty()),
            other => prop_assert!(false, "expected Decline, got {:?}", other),
        }
    }

    /// Every staged link is a sibling of its destination, which is what makes
    /// the later exchange a same-directory — and therefore same-`st_dev` —
    /// rename.
    #[test]
    fn staged_links_are_siblings_of_their_destination(
        names in prop::collection::hash_set(component(), 1..5),
    ) {
        let mut observed = Vec::new();
        let mut target = Vec::new();
        for name in &names {
            let dest = PathBuf::from(format!("/home/u/.config/{name}"));
            observed.push(owned(
                dest.clone(),
                PathBuf::from(format!("/home/u/.local/share/ricepilot/profiles/old/{name}")),
            ));
            target.push(Target {
                dest,
                src: PathBuf::from(format!("/home/u/.local/share/ricepilot/profiles/new/{name}")),
            });
        }

        let Plan::Apply { ops } = plan(&observed, &target, &ctx()) else {
            prop_assert!(false, "expected Apply");
            unreachable!()
        };
        for op in &ops {
            if let ricepilot::plan::Op::CreateTempLink { link_path, .. } = op {
                prop_assert_eq!(link_path.parent(), Some(std::path::Path::new("/home/u/.config")));
            }
        }
    }

    /// Nothing the planner emits is a removal. The type system already says
    /// so — `Op` has no delete variant — but the plan is what a reader of
    /// `ricepilot plan` is trusting, so assert it on the rendered text too.
    #[test]
    fn no_rendered_operation_describes_a_removal(
        names in prop::collection::hash_set(component(), 1..5),
    ) {
        let mut observed = Vec::new();
        let mut target = Vec::new();
        for name in &names {
            let dest = PathBuf::from(format!("/home/u/.config/{name}"));
            observed.push(owned(
                dest.clone(),
                PathBuf::from(format!("/home/u/.local/share/ricepilot/profiles/old/{name}")),
            ));
            target.push(Target {
                dest,
                src: PathBuf::from(format!("/home/u/.local/share/ricepilot/profiles/new/{name}")),
            });
        }
        let p = plan(&observed, &target, &ctx());
        let text = ricepilot::cli::render::plan("new", &observed, &p);
        for word in ["delete", "remove", "erase", "destroy"] {
            prop_assert!(!text.contains(word), "plan output mentions `{}`", word);
        }
    }
}
