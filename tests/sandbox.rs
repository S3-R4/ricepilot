//! The test sandbox (D56): no test can reach the real home or the live
//! session, and this file fails if that stops being true.
//!
//! Three kinds of check. What the harness hands out — every fixture path,
//! every variable of `Fixture::env()`, every `Paths` built from them — lies
//! under `<repo>/target/fixtures`. What the binary does inside the sandbox —
//! it refuses a missing or escaping location, and refuses the real
//! `Hyprland`. And what the other test files are allowed to spell: nothing
//! outside `tests/common/` starts ricepilot, a helper or a session-reaching
//! call by any other route.

// Reads the other test sources, which `ops::read` has no business doing.
#![allow(clippy::disallowed_methods)]

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use common::{adopting, scenario, switching, Fixture, SANDBOX_ROOT};
use ricepilot::cli::paths::{self, Paths, OVERRIDES};
use ricepilot::ops::exec::{self, Allowed, LIVE_TESTS_VAR, SANDBOX_VAR};
use ricepilot::ops::mutate::ExchangeMode;

/// The real home, if this process has one — the thing nothing may point at.
fn real_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn lookup(env: &[(String, String)]) -> impl Fn(&str) -> Option<OsString> + '_ {
    move |name| {
        env.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| OsString::from(v))
    }
}

/// Every location `p` names is below the sandbox root, and none is the real
/// home or inside the parts of it R1 names.
fn assert_confined(what: &str, p: &Path) {
    let root = Path::new(SANDBOX_ROOT);
    assert!(
        paths::lexically_under(p, root),
        "R1: {what} = {} is not below {SANDBOX_ROOT}",
        p.display()
    );
    if let Some(home) = real_home() {
        assert_ne!(p, home, "R1: {what} is the real home");
        for private in [".config", ".local", ".cache"] {
            assert!(
                !p.starts_with(home.join(private)),
                "R1: {what} = {} is inside the real ~/{private}",
                p.display()
            );
        }
    }
}

fn assert_paths_confined(what: &str, p: &Paths) {
    assert_confined(&format!("{what}.home"), &p.home);
    assert_confined(&format!("{what}.data"), &p.data);
    assert_confined(&format!("{what}.state"), &p.state);
    if let Some(r) = &p.runtime {
        assert_confined(&format!("{what}.runtime"), r);
    }
}

/// Every fixture shape the harness builds, one of each.
fn every_kind_of_fixture() -> Vec<(&'static str, Fixture)> {
    vec![
        ("Fixture::new", Fixture::new("sandbox_plain")),
        ("Fixture::new_in", Fixture::new_in("m5", "sandbox_grouped")),
        ("switching::build", switching::build("sandbox_switching").f),
        ("adopting::build", adopting::build("sandbox_adopting").f),
        (
            "scenario::build",
            scenario::build("sandbox_scenario", ExchangeMode::Fallback).f,
        ),
    ]
}

// ---- what the harness hands out ----

#[test]
fn the_harness_puts_this_process_in_the_sandbox() {
    let _ = common::fixture_root();
    assert_eq!(
        std::env::var_os(SANDBOX_VAR),
        Some(OsString::from(SANDBOX_ROOT))
    );
    assert_eq!(
        Path::new(SANDBOX_ROOT),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/fixtures")
    );
    assert!(Path::new(SANDBOX_ROOT).is_absolute());
}

/// Every fixture's home, state directory and profile root, and every value
/// of its environment, is under `<repo>/target/fixtures` — lexically, and
/// through every symlink on the disk.
#[test]
fn every_fixture_path_and_variable_stays_in_the_sandbox() {
    for (what, f) in every_kind_of_fixture() {
        assert_confined(&format!("{what} home"), &f.home);
        assert_confined(&format!("{what} state"), &f.state());
        assert_confined(
            &format!("{what} profiles"),
            &f.path(".local/share/ricepilot/profiles"),
        );
        common::assert_resolves_in_sandbox(what, &f.home);
        common::assert_resolves_in_sandbox(what, &f.state());

        let env = f.env();
        let keys: BTreeMap<&str, &str> =
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(keys.len(), env.len(), "{what}: a variable is set twice");
        // Everything that could name a home is set, and set into the fixture:
        // a variable left unset is one the binary might fall back on.
        for required in OVERRIDES.iter().copied().chain([
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
        ]) {
            let Some(v) = keys.get(required) else {
                panic!("{what}: Fixture::env() does not set {required}");
            };
            assert_confined(&format!("{what} {required}"), Path::new(v));
            assert!(
                Path::new(v).starts_with(&f.home),
                "{what}: {required} is not inside this fixture"
            );
        }
        assert_eq!(keys.get(SANDBOX_VAR), Some(&SANDBOX_ROOT), "{what}");
        assert_eq!(
            keys.contains_key(LIVE_TESTS_VAR),
            common::live_tests(),
            "{what}: the live-tests opt-in must be passed on exactly when it was given"
        );

        // And what ricepilot makes of that environment is confined too.
        let p = Paths::from_lookup(lookup(&env)).unwrap();
        assert_paths_confined(what, &p);
        assert_eq!(p.home, f.home, "{what}");
    }

    // The in-process `Paths` the harness builds for its own assertions.
    assert_paths_confined(
        "switching::Machine::paths",
        &switching::build("sandbox_switching_paths").paths(),
    );
    assert_paths_confined(
        "adopting::Machine::paths",
        &adopting::build("sandbox_adopting_paths").paths(),
    );
}

/// The harness panics rather than build a fixture outside the sandbox.
#[test]
fn the_harness_panics_on_a_location_outside_the_sandbox() {
    for bad in [
        PathBuf::from("/tmp/ricepilot-fixtures"),
        real_home().unwrap_or_else(|| PathBuf::from("/home/u")),
        Path::new(SANDBOX_ROOT).join("../../outside"),
        Path::new(SANDBOX_ROOT).with_file_name("fixtures-elsewhere"),
    ] {
        let caught = std::panic::catch_unwind(|| common::assert_in_sandbox("probe", &bad));
        assert!(caught.is_err(), "{} was accepted", bad.display());
    }
    let caught = std::panic::catch_unwind(|| {
        Fixture {
            home: PathBuf::from("/home/u"),
        }
        .env()
    });
    assert!(
        caught.is_err(),
        "Fixture::env() handed out a home outside the sandbox"
    );
}

// ---- what the binary does inside the sandbox ----

fn cli(f: &Fixture, args: &[&str], env: &[(String, String)]) -> (i32, String) {
    let mut cmd = common::ricepilot(f);
    cmd.env_clear().envs(env.iter().cloned()).args(args);
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        common::redact(&String::from_utf8_lossy(&out.stderr), f).replace(SANDBOX_ROOT, "<SANDBOX>"),
    )
}

/// Inside the sandbox, a missing override is refused, even with a `HOME`
/// and an `XDG_RUNTIME_DIR` right there to fall back on — and so is one that
/// points outside the root.
#[test]
fn the_binary_refuses_to_fall_back_or_escape() {
    let f = Fixture::new_in("m5", "sandbox_binary_paths");
    let mut s = String::new();
    for name in OVERRIDES {
        let env: Vec<_> = f.env().into_iter().filter(|(k, _)| k != name).collect();
        let (code, err) = cli(&f, &["status"], &env);
        s.push_str(&format!("--- without {name}\nexit {code}\n{err}\n"));
    }
    let outside = real_home().unwrap_or_else(|| PathBuf::from("/home/u"));
    let env: Vec<_> = f
        .env()
        .into_iter()
        .map(|(k, v)| {
            if k == "RICEPILOT_STATE_DIR" {
                (
                    k,
                    outside.join(".local/state/ricepilot").display().to_string(),
                )
            } else {
                (k, v)
            }
        })
        .collect();
    let (code, err) = cli(&f, &["status"], &env);
    let err = err.replace(&outside.display().to_string(), "<REAL-HOME>");
    s.push_str(&format!(
        "--- RICEPILOT_STATE_DIR outside\nexit {code}\n{err}\n"
    ));

    let (code, err) = cli(&f, &["status"], &f.env());
    assert_eq!(code, 0, "{err}");
    insta::assert_snapshot!(s);
}

/// A profile whose `~/.config/hypr` has a `hyprland.conf` the sandbox *can*
/// copy — the one shape no other default test uses, because it is the one
/// on which the pre-flight would go on to run the real `Hyprland`. Inside
/// the test sandbox it does not: `ops::exec` refuses, and the plan fails
/// with that refusal. On a machine without Hyprland nothing is checked, as
/// always.
#[test]
fn the_binary_never_runs_the_real_hyprland_in_the_sandbox() {
    let test = "the_binary_never_runs_the_real_hyprland_in_the_sandbox";
    if common::live_tests() {
        eprintln!("SKIPPED {test}: RICEPILOT_LIVE_TESTS=1 lets the real Hyprland run");
        return;
    }
    let f = Fixture::new_in("m5", "sandbox_no_hyprland");
    let markers = f.dir("markers");
    let root = f.dir("rice/new");
    f.file(
        "rice/new/hypr/hyprland.conf",
        &format!(
            "exec-once = : > {}/ran\ngeneral {{\n    border_size = 2\n}}\n",
            markers.display()
        ),
    );
    f.profile(
        "new",
        &format!(
            "name = \"new\"\nroot = \"{}\"\n\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \
             \"hypr\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
            root.display()
        ),
    );

    // Before the binary is run: the environment it will get is one in which
    // `ops::exec` refuses Hyprland. If not, stop here rather than find out.
    let refusal = exec::sandbox_refuses(Allowed::HyprlandVerifyConfig, lookup(&f.env()))
        .expect("Fixture::env() does not keep Hyprland from running; refusing to continue");

    let (code, err) = cli(&f, &["plan", "new"], &f.env());
    if exec::locate(Allowed::HyprlandVerifyConfig)
        .unwrap()
        .is_some()
    {
        assert_eq!(code, ricepilot::error::ExitCode::Refused as i32, "{err}");
        assert_eq!(
            err,
            common::redact(&format!("ricepilot: {refusal}\n"), &f),
            "not the sandbox's refusal"
        );
    } else {
        eprintln!("NOTE {test}: Hyprland is not installed, so only the not-checked path ran");
        assert_eq!(code, 0, "{err}");
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        ricepilot::ops::read::list_dir(&markers).unwrap().is_empty(),
        "an exec line ran"
    );
}

// ---- what the other test files may spell ----

/// Every `.rs` under `tests/` and `examples/`, except the harness and this
/// file, with its text.
fn other_test_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&repo.join("tests"), &mut files);
    walk(&repo.join("examples"), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(repo).unwrap().display().to_string();
            (rel, std::fs::read_to_string(&p).unwrap())
        })
        .filter(|(rel, _)| !rel.starts_with("tests/common/") && rel != "tests/sandbox.rs")
        .collect()
}

/// Nothing outside the harness starts ricepilot, a crash helper or a script
/// by its own route, and nothing takes a process out of the sandbox.
#[test]
fn nothing_outside_the_harness_starts_a_process_its_own_way() {
    // What `Command::new(` may be followed by, and where: building an
    // example with cargo, the harness's own git (with an empty environment),
    // and `tests/guards.rs` running the guard scripts and planting strings.
    let allowed = |file: &str, rest: &str| {
        rest.starts_with("cargo)")
            || rest.starts_with("\"/usr/bin/git\")")
            || (file == "tests/guards.rs"
                && (rest.starts_with("\"sh\")") || rest.starts_with("\\\"")))
    };
    let mut hits = Vec::new();
    for (file, text) in other_test_sources() {
        for (n, line) in text.lines().enumerate() {
            let at = format!("{file}:{}: {}", n + 1, line.trim());
            for needle in ["cargo_bin(", "env_remove(", "remove_var("] {
                if line.contains(needle) {
                    hits.push(format!("{at}   [{needle}]"));
                }
            }
            // Naming the sandbox is fine (a snapshot of its refusal does);
            // setting it to something else is not.
            let names_it = line.contains("SANDBOX_VAR") || line.contains("RICEPILOT_SANDBOX");
            let sets = ["set_var(", ".env(", ".envs("];
            if names_it && sets.iter().any(|s| line.contains(s)) {
                hits.push(format!("{at}   [sets the sandbox]"));
            }
            for (i, _) in line.match_indices("Command::new(") {
                if !allowed(&file, &line[i + "Command::new(".len()..]) {
                    hits.push(format!("{at}   [Command::new]"));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "start ricepilot with common::ricepilot, a crash helper with common::helper and a \
         script with common::sh, and leave the sandbox alone (D56):\n{}",
        hits.join("\n")
    );
}

/// The in-process calls that reach the session are named only where a test
/// is gated on `RICEPILOT_LIVE_TESTS=1` — and even there, `ops::exec` itself
/// refuses them unless the opt-in was given.
#[test]
fn session_reaching_calls_are_named_only_behind_the_live_opt_in() {
    let allowed: &[(&str, &str)] = &[
        // `hyprctl_version_asks_the_running_compositor`, behind live_tests().
        ("tests/exec.rs", "Call::Hyprctl("),
        // A string: the guard that looks for this call in `src/`.
        ("tests/exec.rs", "Call::HyprlandVerifyConfig("),
    ];
    let mut hits = Vec::new();
    for (file, text) in other_test_sources() {
        for (n, line) in text.lines().enumerate() {
            for needle in [
                "Call::Hyprctl(",
                "Call::HyprlandVerifyConfig(",
                "Call::UwsmStop(",
                "hyprverify::check(",
            ] {
                if line.contains(needle) && !allowed.contains(&(file.as_str(), needle)) {
                    hits.push(format!("{file}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(hits.is_empty(), "{}", hits.join("\n"));
}

/// Every crash helper builds its fixture with the harness, which puts it in
/// the sandbox, and one that reads its locations from the environment sets
/// `Fixture::env()` on itself first.
#[test]
fn every_crash_helper_uses_the_harness() {
    let helpers: Vec<(String, String)> = other_test_sources()
        .into_iter()
        .filter(|(file, _)| file.starts_with("examples/"))
        .collect();
    assert!(!helpers.is_empty());
    for (file, text) in helpers {
        assert!(
            text.contains("#[path = \"../tests/common/mod.rs\"]"),
            "{file} does not use the harness"
        );
        // One that resolves `Paths` from its environment must have put the
        // fixture's there first.
        if let Some(resolve) = text.find("Paths::from_env()") {
            let set = text.find(".env() {\n        std::env::set_var(");
            assert!(
                set.is_some_and(|i| i < resolve),
                "{file} resolves Paths before it puts Fixture::env() on itself"
            );
        }
    }
}
