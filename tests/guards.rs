//! The M0 gate, as a permanent regression test rather than a one-off demo:
//! plant each forbidden construct in a fixture tree and assert the CI guard
//! scripts reject it. If someone weakens a pattern, this goes red.
//!
//! Fixtures live under the harness's fixture root, `target/fixtures`, and
//! never outside it (SAFETY.md R1, D56).

// The fixture builder legitimately uses std::fs: it is a test harness living
// outside src/, building throwaway trees under target/. The guards it invokes
// only ever scan src/ in CI.
#![allow(clippy::disallowed_methods)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::fixture_root;

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

// ---------------------------------------------------------------------------
// gc is the only place a delete primitive may exist (D63)
// ---------------------------------------------------------------------------

/// The call `src/gc/remove.rs` makes, as it spells it.
const THE_ONE_CALL: &str =
    "rustix::fs::unlinkat(dir.as_fd(), name, rustix::fs::AtFlags::REMOVEDIR)";

/// The exemption: `gc/remove.rs` may spell the delete call and its flags,
/// and both guards accept it there.
#[test]
fn the_delete_call_is_accepted_in_gc_remove_and_nowhere_else() {
    let body = format!("fn unlink() {{ {THE_ONE_CALL}; }}\n");
    let tree = fixture("gc_remove_ok", "gc/remove.rs", &body);
    assert!(run_guard("check-no-delete.sh", &tree));
    assert!(run_guard("check-ops-boundary.sh", &tree));

    // The same line in any other file — including elsewhere in gc/, and a
    // file called remove.rs somewhere else — fails one guard or the other.
    for (i, rel) in [
        "gc/mod.rs",
        "gc/render.rs",
        "gc/sub/remove.rs",
        "gc/remove.rs.d/x.rs",
        "ops/mutate.rs",
        "ops/read.rs",
        "cli/gc.rs",
        "journal.rs",
        "remove.rs",
    ]
    .iter()
    .enumerate()
    {
        let tree = fixture(&format!("gc_call_elsewhere{i}"), rel, &body);
        let no_delete = run_guard("check-no-delete.sh", &tree);
        let ops = run_guard("check-ops-boundary.sh", &tree);
        assert!(
            !(no_delete && ops),
            "the delete call in {rel} passed both guards"
        );
    }
}

/// Inside `gc/remove.rs` the exemption is two spellings and nothing more:
/// every other filesystem or process access still fails the ops guard.
#[test]
fn nothing_else_is_exempt_in_gc_remove() {
    for (i, snippet) in [
        "rustix::fs::openat(d, p, f, m)",
        "rustix::fs::unlink(p)",
        "rustix::fs::rmdir(p)",
        "rustix::fs::renameat(a, b, c, d)",
        "rustix::fs::statat(d, p, f)",
        "rustix::fs::Dir::new(fd)",
        "std::fs::remove_dir_all(p)",
        "std::fs::remove_file(p)",
        "std::process::Command::new(\"rm\")",
        "std::os::unix::fs::symlink(a, b)",
        "fs::read(p)",
        "File::open(p)",
        "read_dir(p)",
        "create_dir(p)",
        "use rustix::fs as f",
    ]
    .iter()
    .enumerate()
    {
        let body = format!("fn unlink() {{ {THE_ONE_CALL}; }}\nfn planted() {{ {snippet}; }}\n");
        let tree = fixture(&format!("gc_remove_extra{i}"), "gc/remove.rs", &body);
        assert!(
            !run_guard("check-ops-boundary.sh", &tree),
            "ops guard accepted `{snippet}` in gc/remove.rs"
        );
    }
}

/// The delete guard exempts `gc/` and nothing else: each primitive planted
/// in `ops/` — the one directory the ops guard trusts — and in the modules
/// around gc is rejected, and the same one in any file under `gc/` is not.
#[test]
fn delete_primitives_are_rejected_in_ops_and_beside_gc_and_allowed_in_gc() {
    let snippets = [
        "rustix::fs::unlinkat(d, p, f)",
        "rustix::fs::rmdir(p)",
        "std::fs::remove_dir_all(p)",
        "std::fs::remove_file(p)",
        "std::fs::remove_dir(p)",
        "OFlags::O_TRUNC",
        "f.set_len(0)",
        "\"rm -rf\"",
    ];
    for (i, snippet) in snippets.iter().enumerate() {
        let body = format!("fn planted() {{ {snippet}; }}\n");
        for (j, rel) in [
            "ops/mutate.rs",
            "ops/read.rs",
            "ops/exec/sandbox.rs",
            "cli/gc.rs",
            "doctor.rs",
            "gcx/mod.rs",
        ]
        .iter()
        .enumerate()
        {
            let tree = fixture(&format!("delete_planted{i}_{j}"), rel, &body);
            assert!(
                !run_guard("check-no-delete.sh", &tree),
                "delete guard accepted `{snippet}` in {rel}"
            );
        }
        for (j, rel) in ["gc/remove.rs", "gc/mod.rs", "gc/deep/er.rs"]
            .iter()
            .enumerate()
        {
            let tree = fixture(&format!("delete_in_gc{i}_{j}"), rel, &body);
            assert!(
                run_guard("check-no-delete.sh", &tree),
                "delete guard rejected `{snippet}` in {rel}, which is inside gc/"
            );
        }
    }
}

/// On the real tree: the delete call is spelled once, in `src/gc/remove.rs`;
/// the one `allow(clippy::disallowed_methods)` in `src/` is there too; and
/// `clippy.toml` still disallows every removal call, so that `allow` is the
/// only way any of them compiles.
#[test]
fn the_real_tree_has_one_delete_call_and_one_allow_both_in_gc_remove() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    let count = |needle: &str| -> Vec<(String, usize)> {
        let mut out = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            let n = text.matches(needle).count();
            if n > 0 {
                let rel = f.strip_prefix(&src).unwrap().display().to_string();
                out.push((rel, n));
            }
        }
        out
    };
    assert_eq!(
        count("rustix::fs::unlinkat("),
        vec![("gc/remove.rs".to_string(), 1)]
    );
    assert_eq!(
        count("#[allow(clippy::disallowed_methods)]"),
        vec![("gc/remove.rs".to_string(), 1)]
    );
    for needle in [
        "rustix::fs::unlink(",
        "rustix::fs::rmdir(",
        "std::fs::remove_",
    ] {
        assert_eq!(count(needle), vec![], "{needle} is spelled in src/");
    }

    let clippy =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("clippy.toml")).unwrap();
    for path in [
        "std::fs::remove_file",
        "std::fs::remove_dir",
        "std::fs::remove_dir_all",
        "rustix::fs::unlinkat",
        "rustix::fs::unlink",
        "rustix::fs::rmdir",
    ] {
        assert!(
            clippy.contains(&format!("path = \"{path}\"")),
            "clippy.toml no longer disallows {path}"
        );
    }
}
