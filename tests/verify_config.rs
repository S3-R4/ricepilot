//! The verify-config pre-flight as the user meets it: the `hypr config:`
//! block every plan and switch prints, and the refusal (D55).
//!
//! **Nothing here runs `Hyprland`.** The sandbox itself is proved against a
//! fake stand-in by the unit tests in `src/ops/exec/sandbox.rs`, where the
//! real binary cannot be located at all. The CLI cases below are chosen so
//! the binary is never reached: a Lua config is declined before Hyprland is
//! even looked for, and an unsandboxable one is refused by the copier before
//! it would be run — which each test asserts before it runs the command. The
//! real binary is run only by `tests/verify_config_live.rs`, and only under
//! `RICEPILOT_LIVE_TESTS=1`.

mod common;

use std::path::PathBuf;

use common::{redact, undate, Fixture};
use ricepilot::cli::render;
use ricepilot::hyprverify::{NotChecked, Outcome};
use ricepilot::ops::exec::{self, sandbox, Allowed, SandboxedConfig};
use ricepilot::plan::{Plan, Refusal};

fn cli(f: &Fixture, args: &[&str]) -> (i32, String, String) {
    let mut cmd = common::ricepilot(f);
    cmd.args(args);
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        undate(&redact(&String::from_utf8_lossy(&out.stdout), f)),
        redact(&String::from_utf8_lossy(&out.stderr), f),
    )
}

/// A profile `new` whose `hypr` tree holds `files`, linked nowhere yet.
fn with_hypr(case: &str, files: &[(&str, &str)]) -> Fixture {
    let f = Fixture::new_in("m5", case);
    let root = f.dir("rice/new");
    f.dir("rice/new/hypr");
    for (rel, body) in files {
        f.file(&format!("rice/new/hypr/{rel}"), body);
    }
    f.profile(
        "new",
        &format!(
            "name = \"new\"\nroot = \"{}\"\n\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \
             \"hypr\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
            root.display()
        ),
    );
    f
}

/// Every form of the block, and the refusal, side by side.
#[test]
fn the_hypr_config_block_in_every_form() {
    let conf = PathBuf::from("/home/u/rice/new/hypr/hyprland.conf");
    let scratch = PathBuf::from("/home/u/.local/state/ricepilot/verify/20260927T120000Z");
    let outcomes = [
        (
            "parsed",
            Outcome::Parsed {
                config: conf.clone(),
                scratch: scratch.clone(),
                files: 4,
                stripped: 7,
            },
        ),
        (
            "did not parse",
            Outcome::Failed {
                config: conf.clone(),
                scratch: Some(scratch.clone()),
                detail: "Hyprland reported:\n      Config error in file \
                         /home/u/rice/new/hypr/hyprland.conf at line 2: cannot parse \"banana\" \
                         as an int."
                    .into(),
            },
        ),
        (
            "could not be sandboxed",
            Outcome::Failed {
                config: conf.clone(),
                scratch: None,
                detail: "it could not be copied into a sandbox, so Hyprland was not run on it: \
                         /home/u/rice/new/hypr/hyprland.conf line 3: `source = ../x.conf`: `..` \
                         would be resolved through the destination's symlink, which points \
                         somewhere else after the switch; ricepilot will not guess which way"
                    .into(),
            },
        ),
        (
            "no Hyprland",
            Outcome::NotChecked {
                config: conf.clone(),
                why: NotChecked::NoHyprland,
            },
        ),
        (
            "lua",
            Outcome::NotChecked {
                config: PathBuf::from("/home/u/rice/new/hypr/hyprland.lua"),
                why: NotChecked::Lua,
            },
        ),
    ];
    let mut s = String::new();
    for (what, o) in &outcomes {
        s.push_str(&format!("--- {what}\n{}", render::hypr_check(Some(o))));
        if let Some((file, detail)) = o.failure() {
            s.push_str(&render::refusal_list(&[Refusal::VerifyConfigFailed {
                file,
                detail,
            }]));
            s.push('\n');
        }
    }
    insta::assert_snapshot!(s);
}

/// A dry run's footer says "nothing has been changed" only when nothing
/// was: when the verify-config pre-flight wrote its scratch copy under
/// `state/verify/`, it says so and where (D75). Rendered from each outcome,
/// since the CLI reaches a parsed or failed-after-copying config only by
/// running `Hyprland`, which no test here does.
#[test]
fn the_dry_run_footer_says_when_a_scratch_copy_was_written() {
    let conf = PathBuf::from("/home/u/rice/new/hypr/hyprland.conf");
    let scratch = PathBuf::from("/home/u/.local/state/ricepilot/verify/20260927T120000Z");
    let outcomes = [
        ("no hypr config", None),
        (
            "parsed",
            Some(Outcome::Parsed {
                config: conf.clone(),
                scratch: scratch.clone(),
                files: 1,
                stripped: 0,
            }),
        ),
        (
            "did not parse",
            Some(Outcome::Failed {
                config: conf.clone(),
                scratch: Some(scratch.clone()),
                detail: "Hyprland reported: …".into(),
            }),
        ),
        (
            "refused before a copy was made",
            Some(Outcome::Failed {
                config: conf.clone(),
                scratch: None,
                detail: "it could not be copied into a sandbox".into(),
            }),
        ),
        (
            "not checked",
            Some(Outcome::NotChecked {
                config: conf.clone(),
                why: NotChecked::NoHyprland,
            }),
        ),
    ];
    let mut s = String::new();
    for (what, o) in &outcomes {
        let o = o.as_ref();
        let apply = render::plan_with("new", &[], o, &Plan::Apply { ops: vec![] });
        let decline = render::plan_with("new", &[], o, &Plan::Decline { refusals: vec![] });
        let footer = |t: &str| {
            t.lines()
                .filter(|l| l.contains("changed"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        s.push_str(&format!(
            "--- {what}\nplan:\n{}\nplan, declined:\n{}\nswitch dry run:\n{}\n",
            footer(&apply),
            footer(&decline),
            render::uncommitted(o).trim_start()
        ));
        let scratch_said = footer(&apply).contains("scratch copy was written");
        assert_eq!(
            scratch_said,
            o.and_then(Outcome::scratch).is_some(),
            "{what}: {apply}"
        );
    }
    insta::assert_snapshot!(s);
}

/// A profile with no Hyprland config prints no block at all, so every
/// existing plan reads exactly as it did.
#[test]
fn no_hypr_config_no_block() {
    assert_eq!(render::hypr_check(None), "");
    let plain = render::plan("p", &[], &Plan::NoOp);
    assert_eq!(render::plan_with("p", &[], None, &Plan::NoOp), plain);
}

/// A `hyprland.lua` beside a `hyprland.conf` is what Hyprland 0.55 loads, so
/// it is what would have to be checked, and it cannot be: the plan says so
/// and goes ahead. Declined before `Hyprland` is looked for, so this runs
/// nothing on any machine.
#[test]
fn a_lua_config_is_reported_unchecked_and_nothing_is_run() {
    let f = with_hypr(
        "verify_lua",
        &[
            ("hyprland.lua", "os.execute('touch /nonexistent/never')\n"),
            ("hyprland.conf", "exec-once = touch /nonexistent/never\n"),
        ],
    );
    let (code, out, err) = cli(&f, &["plan", "new"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        ricepilot::ops::read::lstat_or_absent(&f.state().join("verify"))
            .unwrap()
            .is_none(),
        "a Lua config made a scratch copy"
    );
    insta::assert_snapshot!(out);

    let (code, out, err) = cli(&f, &["switch", "new"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("hyprland.lua was NOT checked"), "{out}");
}

/// A config the copier refuses declines the switch, on a machine with
/// Hyprland, without Hyprland being run and without anything being written.
/// Where Hyprland is not installed nothing is checked, and the plan says so.
///
/// Before the command runs, the test asks the sandbox itself, directly,
/// whether this config can be sandboxed — and stops if it can, because then
/// the command would go on to run the real binary.
#[test]
fn an_unsandboxable_config_is_refused_without_running_anything() {
    let f = with_hypr(
        "verify_unsandboxable",
        &[(
            "hyprland.conf",
            "exec-once = touch /nonexistent/never\nsource = ../outside.conf\n",
        )],
    );
    let home = f.home.clone();
    let built = SandboxedConfig::build(&sandbox::Request {
        home: &home,
        entry: &home.join(".config/hypr/hyprland.conf"),
        links: &[(home.join(".config/hypr"), f.path("rice/new/hypr"))],
        absent: &[],
        parent: &f.state().join("verify"),
        id: "precheck",
    })
    .unwrap();
    assert!(
        built.is_err(),
        "this fixture can be sandboxed, so the CLI would run the real Hyprland on it; refusing \
         to continue"
    );

    // `plan` reports a decline and exits 0, as it always has; `switch`
    // exits `Refused`.
    let (plan_code, plan_out, _) = cli(&f, &["plan", "new"]);
    let (code, out, _) = cli(&f, &["switch", "new"]);
    assert!(
        ricepilot::ops::read::lstat_or_absent(&f.state().join("verify"))
            .unwrap()
            .is_none(),
        "a refused config left a scratch copy"
    );
    assert_eq!(plan_code, 0, "{plan_out}");
    if exec::locate(Allowed::HyprlandVerifyConfig)
        .unwrap()
        .is_some()
    {
        assert!(plan_out.contains("declined"), "{plan_out}");
        assert_eq!(code, ricepilot::error::ExitCode::Refused as i32, "{out}");
        assert!(
            out.contains("did not pass the sandboxed verify-config"),
            "{out}"
        );
        assert!(out.contains("[R5]"), "{out}");
        assert!(out.contains("Hyprland was not run on it"), "{out}");
        assert!(out.contains("`..`"), "{out}");
    } else {
        eprintln!(
            "NOTE an_unsandboxable_config_is_refused_without_running_anything: Hyprland is not \
             installed, so only the not-checked note was asserted"
        );
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("was NOT checked"), "{out}");
    }
}

/// A line ending in `\` anywhere in the config — here in a sourced file —
/// is refused by the sandbox before it would run Hyprland (D67), and the
/// refusal the user sees names the file, the line and the rule. Built from
/// the sandbox's own answer, so it is the same text whether or not
/// `Hyprland` is installed; nothing here runs it.
#[test]
fn a_continued_line_is_refused_naming_the_file_and_the_rule() {
    let f = with_hypr(
        "verify_continued",
        &[
            ("hyprland.conf", "source = ./keys.conf\n"),
            (
                "keys.conf",
                "bind = SUPER, Q, killactive\nex\\\nec-once = touch /nonexistent/never\n",
            ),
        ],
    );
    let home = f.home.clone();
    let why = SandboxedConfig::build(&sandbox::Request {
        home: &home,
        entry: &home.join(".config/hypr/hyprland.conf"),
        links: &[(home.join(".config/hypr"), f.path("rice/new/hypr"))],
        absent: &[],
        parent: &f.state().join("verify"),
        id: "continued",
    })
    .unwrap()
    .expect_err("a config with a continued line was sandboxed");
    assert!(
        ricepilot::ops::read::lstat_or_absent(&f.state().join("verify"))
            .unwrap()
            .is_none(),
        "a refused config left a scratch copy"
    );

    let o = ricepilot::hyprverify::unsandboxed(f.path("rice/new/hypr/hyprland.conf"), &why);
    let mut s = render::hypr_check(Some(&o));
    let (file, detail) = o.failure().unwrap();
    s.push_str(&render::refusal_list(&[Refusal::VerifyConfigFailed {
        file,
        detail,
    }]));
    let s = redact(&s, &f);
    assert!(s.contains("keys.conf line 2"), "{s}");
    insta::assert_snapshot!(s);
}
