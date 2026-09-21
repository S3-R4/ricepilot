//! `profile.toml` parsing. Pure, so no fixture tree is needed.

use std::path::{Path, PathBuf};

use ricepilot::manifest::{parse, Activation, Kind};

const GOOD: &str = r#"
name = "caelestia"
root = "~/.local/share/caelestia"
requires = ["hyprland", "foot"]
hypr_dialect = "conf"
volatile = ["**/fish_variables", "shell.json"]
generated = ["hypr/scheme/current.conf"]

[[path]]
dest       = "~/.config/hypr"
src        = "hypr"
kind       = "dir-link"
activation = "relogin"

[[path]]
dest       = "~/.config/foot"
src        = "foot"
kind       = "dir-link"
activation = "relogin"

[[path]]
dest       = "~/.config/fuzzel"
src        = "fuzzel"
kind       = "generated"
activation = "never"
"#;

#[test]
fn parses_the_documented_schema() {
    let m = parse(GOOD).unwrap();
    assert_eq!(m.name, "caelestia");
    assert_eq!(m.root, Some(PathBuf::from("~/.local/share/caelestia")));
    assert_eq!(m.requires, ["hyprland", "foot"]);
    assert_eq!(m.hypr_dialect.as_deref(), Some("conf"));
    assert_eq!(m.paths.len(), 3);
    assert_eq!(m.paths[0].kind, Kind::DirLink);
    assert_eq!(m.paths[0].activation, Activation::Relogin);
    assert_eq!(m.volatile.len(), 2);
    assert_eq!(m.generated, [PathBuf::from("hypr/scheme/current.conf")]);
}

#[test]
fn only_dir_link_relogin_entries_become_targets() {
    let m = parse(GOOD).unwrap();
    let home = Path::new("/home/u");
    let targets = m.targets(
        Path::new("/home/u/.local/share/ricepilot/profiles/caelestia"),
        home,
    );

    // The `generated` entry classifies a path; it is never activated, so it
    // is absent from the plan by construction rather than by a later check.
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].dest, PathBuf::from("/home/u/.config/hypr"));
    assert_eq!(
        targets[0].src,
        PathBuf::from("/home/u/.local/share/caelestia/hypr")
    );
}

#[test]
fn a_profile_without_a_root_resolves_src_against_its_own_directory() {
    let m = parse(
        r#"
name = "bare"
[[path]]
dest = "~/.config/hypr"
src = "hypr"
kind = "dir-link"
activation = "relogin"
"#,
    )
    .unwrap();
    let dir = Path::new("/home/u/.local/share/ricepilot/profiles/bare");
    let t = m.targets(dir, Path::new("/home/u"));
    assert_eq!(t[0].src, dir.join("hypr"));
}

/// Each of these is a manifest that must not be accepted. A `src` that
/// escapes the profile root is the important one: it would let a profile
/// point a live link at anything on the machine.
#[test]
fn rejects_malformed_and_dangerous_manifests() {
    let cases: &[(&str, &str)] = &[
        ("empty name", r#"name = """#),
        (
            "name with a slash",
            r#"name = "a/b""#,
        ),
        (
            "unknown top-level key",
            "name = \"p\"\nrequries = [\"typo\"]\n",
        ),
        (
            "unknown path key",
            "name = \"p\"\n[[path]]\ndest = \"~/a\"\nsrc = \"a\"\nkind = \"dir-link\"\nactivation = \"relogin\"\nmode = \"0755\"\n",
        ),
        (
            "src escaping the profile root",
            "name = \"p\"\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \"../../../etc\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
        ),
        (
            "absolute src",
            "name = \"p\"\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \"/etc\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
        ),
        (
            "empty src",
            "name = \"p\"\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \"\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
        ),
        (
            "relative dest",
            "name = \"p\"\n[[path]]\ndest = \".config/hypr\"\nsrc = \"hypr\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
        ),
        (
            "dest climbing out with ..",
            "name = \"p\"\n[[path]]\ndest = \"~/.config/../../etc/hypr\"\nsrc = \"hypr\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
        ),
        (
            "duplicate dest",
            "name = \"p\"\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \"a\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \"b\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
        ),
        (
            "relative root",
            "name = \"p\"\nroot = \"relative/thing\"\n",
        ),
        (
            "unknown hypr dialect",
            "name = \"p\"\nhypr_dialect = \"toml\"\n",
        ),
        (
            "generated path asking to be activated",
            "name = \"p\"\n[[path]]\ndest = \"~/.config/fuzzel\"\nsrc = \"fuzzel\"\nkind = \"generated\"\nactivation = \"relogin\"\n",
        ),
        (
            "file-copy, which is v1.1",
            "name = \"p\"\n[[path]]\ndest = \"~/.config/starship.toml\"\nsrc = \"starship.toml\"\nkind = \"file-copy\"\nactivation = \"relogin\"\n",
        ),
        (
            "live activation, which v1 does not do",
            "name = \"p\"\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \"hypr\"\nkind = \"dir-link\"\nactivation = \"live\"\n",
        ),
    ];

    for (what, text) in cases {
        assert!(
            parse(text).is_err(),
            "manifest with {what} was accepted: {text}"
        );
    }
}

/// The rejection messages are what a user sees when a profile will not load,
/// so they are pinned like the refusals are.
#[test]
fn rejection_messages() {
    let mut out = String::new();
    for text in [
        "name = \"p\"\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \"../../../etc\"\nkind = \"dir-link\"\nactivation = \"relogin\"\n",
        "name = \"p\"\n[[path]]\ndest = \"~/.config/starship.toml\"\nsrc = \"starship.toml\"\nkind = \"file-copy\"\nactivation = \"relogin\"\n",
        "name = \"p\"\n[[path]]\ndest = \"~/.config/hypr\"\nsrc = \"hypr\"\nkind = \"dir-link\"\nactivation = \"live\"\n",
        "name = \"p\"\n[[path]]\ndest = \"~/.config/fuzzel\"\nsrc = \"fuzzel\"\nkind = \"generated\"\nactivation = \"relogin\"\n",
        "name = \"p\"\nhypr_dialect = \"toml\"\n",
        "name = \"p\"\nroot = \"relative/thing\"\n",
        "name = \"p\"\nrequries = [\"typo\"]\n",
    ] {
        out.push_str(&parse(text).unwrap_err().to_string());
        out.push('\n');
    }
    insta::assert_snapshot!(out);
}

#[test]
fn tilde_is_only_special_at_the_start() {
    use ricepilot::manifest::expand_home;
    let home = Path::new("/home/u");
    assert_eq!(
        expand_home(Path::new("~/.config"), home),
        home.join(".config")
    );
    assert_eq!(expand_home(Path::new("~"), home), home);
    assert_eq!(
        expand_home(Path::new("/etc/~weird"), home),
        PathBuf::from("/etc/~weird")
    );
}

/// Every `NotPossible` anchor in the source must name a real section of
/// `docs/NOT-POSSIBLE.md`, or the message sends the reader to a page that
/// does not answer their question.
#[test]
fn every_not_possible_anchor_exists_in_the_document() {
    #![allow(clippy::disallowed_methods)]
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = std::fs::read_to_string(root.join("docs/NOT-POSSIBLE.md")).unwrap();
    let anchors: Vec<&str> = doc
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .map(str::trim)
        .collect();

    let mut referenced = Vec::new();
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            for line in std::fs::read_to_string(&p).unwrap().lines() {
                if let Some(rest) = line.split("anchor: \"").nth(1) {
                    if let Some(a) = rest.split('"').next() {
                        referenced.push(a.to_string());
                    }
                }
            }
        }
    }

    assert!(!referenced.is_empty(), "found no anchors to check");
    for a in &referenced {
        assert!(
            anchors.contains(&a.as_str()),
            "src references docs/NOT-POSSIBLE.md#{a}, which has no `## {a}` section"
        );
    }
}
