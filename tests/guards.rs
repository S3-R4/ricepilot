//! The M0 gate, as a permanent regression test rather than a one-off demo:
//! plant each forbidden construct in a fixture tree and assert the CI guard
//! scripts reject it. If someone weakens a pattern, this goes red.
//!
//! Fixtures live under a configurable root (`RICEPILOT_FIXTURE_ROOT`,
//! default `target/fixtures`) and never outside the repo (SAFETY.md R1).

// The fixture builder legitimately uses std::fs: it is a test harness living
// outside src/, building throwaway trees under target/. The guards it invokes
// only ever scan src/ in CI.
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_root() -> PathBuf {
    match std::env::var_os("RICEPILOT_FIXTURE_ROOT") {
        Some(v) => PathBuf::from(v),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("fixtures"),
    }
}

/// Build a throwaway `src`-shaped tree containing `body` at `rel`.
fn fixture(case: &str, rel: &str, body: &str) -> PathBuf {
    let root = fixture_root().join("guards").join(case);
    let file = root.join(rel);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::create_dir_all(root.join("gc")).unwrap();
    std::fs::create_dir_all(root.join("ops")).unwrap();
    std::fs::write(&file, body).unwrap();
    root
}

fn run_guard(script: &str, tree: &Path) -> bool {
    Command::new("sh")
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(script),
        )
        .arg(tree)
        .status()
        .unwrap()
        .success()
}

#[test]
fn guards_pass_on_the_real_source_tree() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(run_guard("check-no-delete.sh", &src));
    assert!(run_guard("check-ops-boundary.sh", &src));
}

#[test]
fn delete_guard_rejects_each_delete_primitive() {
    for (i, snippet) in [
        "std::fs::remove_dir_all(p)",
        "std::fs::remove_file(p)",
        "std::fs::remove_dir(p)",
        "rustix::fs::unlinkat(d, p, f)",
        "rustix::fs::unlink(p)",
        "libc_rmdir(p)",
        "shutil.rmtree(p)",
        "Command::new(\"sh\").arg(\"rm -rf /\")",
        "f.set_len(0)",
        "OFlags::O_TRUNC",
    ]
    .iter()
    .enumerate()
    {
        let tree = fixture(
            &format!("delete{i}"),
            "planted.rs",
            &format!("fn planted() {{ {snippet}; }}\n"),
        );
        assert!(
            !run_guard("check-no-delete.sh", &tree),
            "delete guard accepted `{snippet}`"
        );
    }
}

#[test]
fn delete_guard_allows_the_same_primitive_inside_gc() {
    let tree = fixture(
        "delete_in_gc",
        "gc/collect.rs",
        "fn collect() { std::fs::remove_dir_all(p); }\n",
    );
    assert!(run_guard("check-no-delete.sh", &tree));
}

#[test]
fn ops_guard_rejects_filesystem_and_subprocess_access() {
    for (i, snippet) in [
        "std::fs::rename(a, b)",
        "rustix::fs::renameat(a, b, c, d)",
        "std::os::unix::fs::symlink(a, b)",
        "fs::read(p)",
        "File::open(p)",
        "OpenOptions::new()",
        "std::process::Command::new(\"hyprctl\")",
        "std::fs::read_to_string(p)",
        "std::fs::read_dir(p)",
        "std::fs::create_dir_all(p)",
    ]
    .iter()
    .enumerate()
    {
        let tree = fixture(
            &format!("ops{i}"),
            "planted.rs",
            &format!("fn planted() {{ {snippet}; }}\n"),
        );
        assert!(
            !run_guard("check-ops-boundary.sh", &tree),
            "ops guard accepted `{snippet}`"
        );
    }
}

#[test]
fn ops_guard_allows_the_same_calls_inside_ops() {
    let tree = fixture(
        "ops_in_ops",
        "ops/mutate.rs",
        "fn m() { std::fs::rename(a, b); std::process::Command::new(\"hyprctl\"); }\n",
    );
    assert!(run_guard("check-ops-boundary.sh", &tree));
}
