//! The diff report as text. Pure: a [`Report`] in, a `String` out.
//!
//! The links first, one row each, `same` or `DIFFERS`; then the content
//! against the record; then what `volatile` left out; then one line saying
//! what the exit status says. Nothing is wrapped to a width: a wrap point that
//! depends on how long a path is would make two checkouts' reports differ.

use std::fmt::Write as _;

use super::Content;
use super::Found;
use super::Recorded;
use super::Report;
use super::Row;
use super::Wanted;
use crate::plan::SourceState;
use crate::verify::AgainstRecord;
use crate::verify::Difference;

pub fn text(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "diff: profile `{}` against the live filesystem",
        r.profile
    );
    let _ = writeln!(
        s,
        "root: {} ({})",
        r.root.display(),
        if r.by_reference {
            "by reference — ricepilot never writes here"
        } else {
            "owned payload"
        }
    );
    if let Some(j) = &r.in_flight {
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "NOTE: an operation is running now or did not finish: {} exists.",
            j.display()
        );
        let _ = writeln!(
            s,
            "until `ricepilot recover` has run, the links below may be half of one profile and \
             half of another."
        );
    }

    links(&mut s, r);
    content(&mut s, r);
    verdict(&mut s, r);
    s
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn links(s: &mut String, r: &Report) {
    let _ = writeln!(s);
    let differing = r.links_differing();
    if r.rows.is_empty() {
        let _ = writeln!(s, "links: this profile declares no dir-link destinations");
    } else if differing == 0 {
        let _ = writeln!(
            s,
            "links: {} as this profile has {}",
            if r.rows.len() == 1 {
                "the one destination is".to_string()
            } else {
                format!("all {} destinations are", r.rows.len())
            },
            if r.rows.len() == 1 { "it" } else { "them" }
        );
    } else {
        let _ = writeln!(s, "links: {differing} of {} differ", r.rows.len());
    }
    for row in &r.rows {
        link_row(s, row);
    }
    for u in &r.never_linked {
        let _ = writeln!(s, "  never    {}", u.dest.display());
        let _ = writeln!(
            s,
            "             declared kind = \"{}\", activation = \"{}\": ricepilot never links it, \
             and it is not compared",
            u.kind, u.activation
        );
    }
}

fn link_row(s: &mut String, row: &Row) {
    if row.same() {
        if let Wanted::Link { src, .. } = &row.wanted {
            let _ = writeln!(s, "  same     {} -> {}", row.dest.display(), src.display());
        }
        return;
    }
    let _ = writeln!(s, "  DIFFERS  {}", row.dest.display());
    let _ = writeln!(s, "             live:    {}", found(&row.found));
    match &row.wanted {
        Wanted::Link { src, source } => {
            let _ = writeln!(s, "             profile: a link to {}", src.display());
            match source {
                SourceState::Dir => {}
                SourceState::Missing => {
                    let _ = writeln!(
                        s,
                        "             source:  missing — nothing is there, so a link to it \
                         dangles"
                    );
                }
                SourceState::NotADir => {
                    let _ = writeln!(
                        s,
                        "             source:  not a directory — ricepilot links directories, \
                         and does not follow a link to one"
                    );
                }
            }
        }
        Wanted::Undeclared => {
            let _ = writeln!(
                s,
                "             profile: nothing — it does not link this path"
            );
        }
    }
    if let Found::RealDir {
        content: Some(content),
    } = &row.found
    {
        real_dir(s, content);
    }
}

fn found(f: &Found) -> String {
    match f {
        Found::Owned { target, profile } => format!(
            "ricepilot's link to {}, for profile `{profile}`",
            target.display()
        ),
        Found::Foreign {
            target,
            dangling,
            recorded,
        } => {
            let dangles = if *dangling { " (it dangles)" } else { "" };
            match recorded {
                Recorded::No => format!(
                    "a link to {}{dangles}, which ricepilot did not make",
                    target.display()
                ),
                Recorded::Otherwise => format!(
                    "a link to {}{dangles}, and not the one ricepilot recorded here",
                    target.display()
                ),
                Recorded::OutsideEveryProfile => format!(
                    "ricepilot's link to {}{dangles}, which no registered profile's root \
                     contains any more",
                    target.display()
                ),
            }
        }
        Found::RealDir { .. } => "a real directory, not a link".into(),
        Found::RealFile => "a file, not a link".into(),
        Found::Absent => "nothing".into(),
    }
}

/// A real directory against the profile's source for it: what an installer
/// left, set beside what the link would have shown.
fn real_dir(s: &mut String, c: &Content) {
    const IN: &str = "               ";
    match c {
        Content::Unreadable { why } => {
            let _ = writeln!(
                s,
                "             its content was NOT compared with the source: {why}"
            );
        }
        Content::Compared { diffs } => {
            let (real, touched) = split(diffs);
            if real.is_empty() {
                let _ = writeln!(
                    s,
                    "             its content is the source's, outside `volatile`."
                );
            } else {
                let _ = writeln!(
                    s,
                    "             in the directory, against the source (`added`: only in the \
                     directory; `gone`: only in the source):"
                );
                for d in real {
                    let _ = writeln!(s, "{IN}{d}");
                }
            }
            if touched > 0 {
                let _ = writeln!(
                    s,
                    "{IN}{} identical content and another modification time.",
                    plural(touched, "further path has", "further paths have")
                );
            }
        }
    }
}

/// The differences that are drift, and how many are only a newer mtime.
fn split(diffs: &[Difference]) -> (Vec<&Difference>, usize) {
    let real: Vec<&Difference> = diffs.iter().filter(|d| d.is_substantive()).collect();
    let touched = diffs.len() - real.len();
    (real, touched)
}

fn globs(v: &[String]) -> String {
    v.iter()
        .map(|g| format!("`{g}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn content(s: &mut String, r: &Report) {
    let _ = writeln!(s);
    match &r.content {
        AgainstRecord::Unrecorded => {
            let _ = writeln!(
                s,
                "content: NOT compared — nothing has been recorded for `{}` yet.",
                r.profile
            );
            let _ = writeln!(s, "  ({})", r.manifest.display());
            let _ = writeln!(
                s,
                "  ricepilot records a profile's tree when it captures the profile and each time \
                 it switches to it."
            );
        }
        AgainstRecord::Unreadable { why } => {
            let _ = writeln!(s, "content: NOT compared — {why}.");
            let _ = writeln!(s, "  ({})", r.manifest.display());
        }
        AgainstRecord::Compared {
            recorded,
            diffs,
            excluded,
        } => {
            let (real, touched) = split(diffs);
            if real.is_empty() {
                let _ = writeln!(
                    s,
                    "content: the tree is what ricepilot recorded at {}.",
                    recorded.created
                );
            } else {
                let _ = writeln!(
                    s,
                    "content: {} since ricepilot recorded the tree at {}:",
                    plural(real.len(), "path has changed", "paths have changed"),
                    recorded.created
                );
            }
            let _ = writeln!(
                s,
                "  ({}, {})",
                r.manifest.display(),
                plural(recorded.entries.len(), "path", "paths")
            );
            for d in &real {
                let _ = writeln!(s, "  {d}");
            }
            if touched > 0 {
                let _ = writeln!(
                    s,
                    "  {} rewritten with identical content; that is not drift.",
                    plural(touched, "further path was", "further paths were")
                );
            }
            if recorded.root != r.root {
                let _ = writeln!(
                    s,
                    "  the record was taken of {}; the profile's root is now {}.",
                    recorded.root.display(),
                    r.root.display()
                );
            }
            if recorded.volatile != r.volatile {
                let _ = writeln!(
                    s,
                    "  the record left out {}; the profile now declares {} volatile, so a path in \
                     one list and not the other shows as added or gone.",
                    if recorded.volatile.is_empty() {
                        "nothing".to_string()
                    } else {
                        globs(&recorded.volatile)
                    },
                    if r.volatile.is_empty() {
                        "nothing".to_string()
                    } else {
                        globs(&r.volatile)
                    }
                );
            }
            volatile(s, r, excluded);
        }
    }
}

/// What `volatile` kept out of the comparison: listed, never counted as
/// drift. There is no record of these to compare with — by declaration an
/// application rewrites them — so all that can truthfully be said is that
/// they are there.
fn volatile(s: &mut String, r: &Report, excluded: &[String]) {
    let _ = writeln!(s);
    if r.volatile.is_empty() {
        let _ = writeln!(
            s,
            "volatile: nothing is declared `volatile`, so every path was compared."
        );
    } else if excluded.is_empty() {
        let _ = writeln!(
            s,
            "volatile: nothing in the tree matches {}.",
            globs(&r.volatile)
        );
    } else {
        let _ = writeln!(
            s,
            "volatile: {} not compared, because the manifest declares {} volatile:",
            plural(excluded.len(), "path", "paths"),
            globs(&r.volatile)
        );
        for p in excluded {
            let _ = writeln!(s, "  {p}");
        }
    }
}

fn verdict(s: &mut String, r: &Report) {
    let _ = writeln!(s);
    let links = r.links_differing();
    let paths = r.paths_differing();
    let mut parts = Vec::new();
    if links > 0 {
        parts.push(plural(links, "link", "links"));
    }
    if paths > 0 {
        parts.push(plural(paths, "path", "paths"));
    }
    let line = match (&r.content, parts.is_empty()) {
        (AgainstRecord::Unreadable { .. }, true) => {
            "incomplete: no link differs, and the content could not be compared (above).".into()
        }
        (AgainstRecord::Unreadable { .. }, false) => format!(
            "differs: {}; and the content could not be compared (above).",
            parts.join(" and ")
        ),
        (_, false) => format!("differs: {}.", parts.join(" and ")),
        (AgainstRecord::Unrecorded, true) => {
            "no link differs; the content was NOT compared (above).".into()
        }
        (AgainstRecord::Compared { .. }, true) => {
            "identical: every link is this profile's, and its tree is what ricepilot recorded."
                .into()
        }
    };
    let _ = writeln!(s, "{line}");
    if links > 0 {
        let _ = writeln!(
            s,
            "`ricepilot plan {}` shows what a switch would do about the links.",
            r.profile
        );
    }
    let _ = writeln!(s, "nothing was changed.");
}
