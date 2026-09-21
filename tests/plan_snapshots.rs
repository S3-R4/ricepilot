//! Snapshots of everything M1 prints.
//!
//! The plan output is the dry-run promise (`SAFETY.md` R4) and every refusal
//! message is a thing a user reads at the worst possible moment, so both are
//! pinned rather than left to drift.

use std::path::PathBuf;

use ricepilot::observe::{Observed, Shape};
use ricepilot::plan::{plan, Op, Plan, PlanContext, Refusal, Target};

const DEV: u64 = 66;

fn ctx() -> PlanContext {
    PlanContext::new(
        "/home/u",
        "/home/u/.local/state/ricepilot/attic/20260921T120000Z",
        DEV,
    )
}

fn dest(name: &str) -> PathBuf {
    PathBuf::from(format!("/home/u/.config/{name}"))
}

fn src(profile: &str, name: &str) -> PathBuf {
    PathBuf::from(format!(
        "/home/u/.local/share/ricepilot/profiles/{profile}/{name}"
    ))
}

fn observed(name: &str, shape: Shape) -> Observed {
    Observed {
        dest: dest(name),
        shape,
        parent_dev: DEV,
        parent_fs_type: 0x9123683E,
        is_mountpoint: false,
    }
}

fn render(name: &str, shape: Shape) -> String {
    let obs = vec![observed(name, shape)];
    let tgt = vec![Target {
        dest: dest(name),
        src: src("new", name),
    }];
    let p = plan(&obs, &tgt, &ctx());
    ricepilot::cli::render::plan("new", &obs, &p)
}

// ---------------------------------------------------------------- shapes

#[test]
fn shape_1_owned_link_already_correct() {
    insta::assert_snapshot!(render(
        "hypr",
        Shape::OwnedLink {
            target: src("new", "hypr")
        }
    ));
}

#[test]
fn shape_1_owned_link_pointing_at_the_previous_profile() {
    insta::assert_snapshot!(render(
        "hypr",
        Shape::OwnedLink {
            target: src("old", "hypr")
        }
    ));
}

#[test]
fn shape_2_foreign_link() {
    insta::assert_snapshot!(render(
        "hypr",
        Shape::ForeignLink {
            target: PathBuf::from("/home/u/somewhere/hypr"),
            dangling: false,
        }
    ));
}

#[test]
fn shape_2_dangling_foreign_link() {
    insta::assert_snapshot!(render(
        "hypr",
        Shape::ForeignLink {
            target: PathBuf::from("/home/u/gone/hypr"),
            dangling: true,
        }
    ));
}

#[test]
fn shape_3_real_dir() {
    insta::assert_snapshot!(render("hypr", Shape::RealDir));
}

#[test]
fn shape_4_real_file() {
    insta::assert_snapshot!(render("starship.toml", Shape::RealFile));
}

#[test]
fn shape_5_absent() {
    insta::assert_snapshot!(render("hypr", Shape::Absent));
}

#[test]
fn a_full_switch_across_several_destinations() {
    let names = ["hypr", "foot", "btop"];
    let obs: Vec<Observed> = names
        .iter()
        .map(|n| {
            observed(
                n,
                Shape::OwnedLink {
                    target: src("old", n),
                },
            )
        })
        .collect();
    let tgt: Vec<Target> = names
        .iter()
        .map(|n| Target {
            dest: dest(n),
            src: src("new", n),
        })
        .collect();
    let p = plan(&obs, &tgt, &ctx());
    insta::assert_snapshot!(ricepilot::cli::render::plan("new", &obs, &p));
}

// -------------------------------------------------------------- refusals

/// Every variant of [`Refusal`], rendered. A new variant added without a line
/// here is a message no one has read.
#[test]
fn every_refusal_message() {
    let refusals = vec![
        Refusal::Unowned {
            dest: dest("hypr"),
            shape: "foreign link",
        },
        Refusal::Unowned {
            dest: dest("foot"),
            shape: "dangling foreign link",
        },
        Refusal::RealDirAtDest { dest: dest("hypr") },
        Refusal::RealFileAtDest {
            dest: dest("starship.toml"),
        },
        Refusal::CrossDevice {
            dest: dest("hypr"),
            from: 66,
            to: 70,
        },
        Refusal::Denylisted {
            dest: PathBuf::from("/home/u/.config/uwsm"),
            entry: "~/.config/uwsm",
        },
        Refusal::Mountpoint {
            dest: PathBuf::from("/home/u/.config/mounted"),
        },
        Refusal::NestedDest {
            outer: dest("quickshell"),
            inner: dest("quickshell/overview"),
        },
        Refusal::MissingRequires {
            packages: vec!["hyprland".into(), "foot".into()],
        },
        Refusal::VerifyConfigFailed {
            file: dest("hypr/hyprland.conf"),
            detail: "line 12: unknown keyword `bindd`".into(),
        },
        Refusal::SourceMissing {
            dest: dest("hypr"),
            src: PathBuf::from("/home/u/.local/share/caelestia/hypr"),
        },
        Refusal::SourceNotADirectory {
            dest: dest("hypr"),
            src: PathBuf::from("/home/u/.local/share/caelestia/hypr"),
        },
        Refusal::NotObserved { dest: dest("btop") },
    ];
    insta::assert_snapshot!(ricepilot::cli::render::refusal_list(&refusals));
}

#[test]
fn a_denylisted_destination_is_refused_even_when_a_manifest_names_it() {
    let d = PathBuf::from("/home/u/.config/uwsm/env-hyprland");
    let obs = vec![Observed {
        dest: d.clone(),
        shape: Shape::Absent,
        parent_dev: DEV,
        parent_fs_type: 0,
        is_mountpoint: false,
    }];
    let tgt = vec![Target {
        dest: d,
        src: src("new", "uwsm"),
    }];
    insta::assert_snapshot!(ricepilot::cli::render::plan(
        "new",
        &obs,
        &plan(&obs, &tgt, &ctx())
    ));
}

#[test]
fn a_mountpoint_destination_is_refused() {
    let obs = vec![Observed {
        dest: dest("mounted"),
        shape: Shape::RealDir,
        parent_dev: DEV,
        parent_fs_type: 0,
        is_mountpoint: true,
    }];
    let tgt = vec![Target {
        dest: dest("mounted"),
        src: src("new", "mounted"),
    }];
    insta::assert_snapshot!(ricepilot::cli::render::plan(
        "new",
        &obs,
        &plan(&obs, &tgt, &ctx())
    ));
}

#[test]
fn an_attic_on_another_filesystem_is_refused_before_any_rename() {
    let obs = vec![observed(
        "hypr",
        Shape::OwnedLink {
            target: src("old", "hypr"),
        },
    )];
    let tgt = vec![Target {
        dest: dest("hypr"),
        src: src("new", "hypr"),
    }];
    let mut c = ctx();
    c.attic_dev = 70;
    insta::assert_snapshot!(ricepilot::cli::render::plan(
        "new",
        &obs,
        &plan(&obs, &tgt, &c)
    ));
}

#[test]
fn missing_packages_are_refused_and_the_exact_command_is_printed() {
    let obs = vec![observed(
        "hypr",
        Shape::OwnedLink {
            target: src("new", "hypr"),
        },
    )];
    let tgt = vec![Target {
        dest: dest("hypr"),
        src: src("new", "hypr"),
    }];
    let mut c = ctx();
    c.missing_requires = vec!["hyprland".into(), "foot".into()];
    insta::assert_snapshot!(ricepilot::cli::render::plan(
        "new",
        &obs,
        &plan(&obs, &tgt, &c)
    ));
}

#[test]
fn nested_destinations_are_refused() {
    let names = ["quickshell", "quickshell/overview"];
    let obs: Vec<Observed> = names.iter().map(|n| observed(n, Shape::Absent)).collect();
    let tgt: Vec<Target> = names
        .iter()
        .map(|n| Target {
            dest: dest(n),
            src: src("new", n),
        })
        .collect();
    insta::assert_snapshot!(ricepilot::cli::render::plan(
        "new",
        &obs,
        &plan(&obs, &tgt, &ctx())
    ));
}

// ------------------------------------------------------------- structure

/// The displaced old link must end up in the attic, never anywhere else and
/// never nowhere.
#[test]
fn the_displaced_link_goes_to_the_attic() {
    let obs = vec![observed(
        "hypr",
        Shape::OwnedLink {
            target: src("old", "hypr"),
        },
    )];
    let tgt = vec![Target {
        dest: dest("hypr"),
        src: src("new", "hypr"),
    }];
    let Plan::Apply { ops } = plan(&obs, &tgt, &ctx()) else {
        panic!("expected Apply");
    };
    assert!(ops.iter().any(|o| matches!(o, Op::RenameToAttic { .. })));
}
