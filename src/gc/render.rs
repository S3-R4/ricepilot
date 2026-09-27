//! Everything `gc` says. Pure: values in, text out, so every line of it is
//! snapshot-tested through the command that prints it.
//!
//! Lines are broken by hand, never wrapped to a width, for the reason D57
//! gives: a wrap point that depends on how long a path is would make two
//! checkouts' output differ.

use std::fmt::Write as _;
use std::path::Path;

use crate::cli::paths::Paths;
use crate::Error;

use super::{kind_word, tilde, Area, Collected, Item, Keep, NotHeld, Survey, Tally, Verdict, Why};

/// The itemisation: every entry, candidate or kept, with its path, what it
/// holds, its size and the reason — printed before anything is asked (D61).
pub fn survey(s: &Survey, paths: &Paths, commit: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "gc: {}", paths.state.display());
    let _ = writeln!(
        out,
        "nothing outside that directory is removed, and nothing in it unless its name is typed \
         back."
    );

    if s.items.is_empty() {
        let _ = writeln!(
            out,
            "\nthe attic, the verify-config copies and state/gc are empty or absent: there is \
             nothing to remove."
        );
        notes(&mut out, s);
        return out;
    }

    for area in [Area::Interrupted, Area::Attic, Area::Verify] {
        let items: Vec<&Item> = s.items.iter().filter(|i| i.area == area).collect();
        if items.is_empty() {
            continue;
        }
        let _ = writeln!(
            out,
            "\n{} — {}",
            match area {
                Area::Interrupted => "interrupted removals",
                Area::Attic => "the attic",
                Area::Verify => "verify-config copies",
            },
            area.dir(&paths.state).display()
        );
        for i in items {
            entry(&mut out, i, &paths.home);
        }
    }

    let candidates: Vec<&Item> = s.candidates().collect();
    let kept = s.items.len() - candidates.len();
    let bytes: u64 = candidates.iter().map(|i| i.tally.bytes).sum();
    let _ = writeln!(
        out,
        "\n{}, {} kept.",
        match candidates.len() {
            0 => "no candidates".to_string(),
            1 => format!("1 candidate ({})", size(bytes)),
            n => format!("{n} candidates ({})", size(bytes)),
        },
        kept
    );
    notes(&mut out, s);
    if !commit {
        let _ = writeln!(
            out,
            "dry run: nothing was removed. `ricepilot gc --commit` asks for each candidate's \
             name, and removes only the ones typed back."
        );
    } else if candidates.is_empty() {
        let _ = writeln!(out, "nothing to remove.");
    }
    out
}

fn notes(out: &mut String, s: &Survey) {
    for n in &s.notes {
        let _ = writeln!(out, "note: {n}");
    }
}

fn entry(out: &mut String, i: &Item, home: &Path) {
    let word = match i.verdict {
        Verdict::Candidate(_) => "candidate",
        Verdict::Kept(_) => "KEPT",
    };
    let _ = writeln!(out, "  {word:<10} {}", i.path.display());
    if !i.listing.is_empty() {
        let _ = writeln!(out, "             {}", tally(&i.tally));
    }
    match &i.verdict {
        Verdict::Candidate(why) => {
            for line in why_lines(why, &i.path, home) {
                let _ = writeln!(out, "             {line}");
            }
        }
        Verdict::Kept(keep) => {
            for k in keep {
                let _ = writeln!(out, "             - {}", keep_line(k, home));
            }
        }
    }
}

/// What an entry holds: `4 directories, 2 links; 51 bytes`.
pub fn tally(t: &Tally) -> String {
    let mut parts = Vec::new();
    for (n, one, many) in [
        (t.dirs, "directory", "directories"),
        (t.links, "link", "links"),
        (t.files, "file", "files"),
        (
            t.other,
            "socket, fifo or device",
            "sockets, fifos or devices",
        ),
    ] {
        if n > 0 {
            parts.push(plural(n, one, many));
        }
    }
    format!("{}; {}", parts.join(", "), size(t.bytes))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn size(bytes: u64) -> String {
    if bytes < 1024 {
        return plural(bytes as usize, "byte", "bytes");
    }
    let mut v = bytes as f64;
    let mut unit = "B";
    for u in ["KiB", "MiB", "GiB", "TiB"] {
        if v < 1024.0 {
            break;
        }
        v /= 1024.0;
        unit = u;
    }
    format!("{bytes} bytes ({v:.1} {unit})")
}

fn why_lines(why: &Why, path: &Path, home: &Path) -> Vec<String> {
    match why {
        Why::Displaced {
            journal,
            links,
            adopted,
        } => {
            if *links == 0 && adopted.is_empty() {
                return vec![format!(
                    "holds only empty directories, the ones {} says things were displaced into",
                    journal.display()
                )];
            }
            let mut v = vec![format!(
                "holds only what {} says was displaced into it:",
                journal.display()
            )];
            if *links > 0 {
                v.push(format!(
                    "  {}, whose targets that journal records",
                    plural(*links, "link", "links")
                ));
            }
            for c in adopted {
                v.push(format!(
                    "  {}, the directory adopt moved out of {}; profile `{}` holds an identical \
                     copy of it at {}, and {} is ricepilot's link",
                    path.join(&c.at).display(),
                    tilde(&c.dest, home),
                    c.profile,
                    c.copy.display(),
                    tilde(&c.dest, home)
                ));
            }
            v
        }
        Why::Rescue { generation, links } => vec![format!(
            "holds only what rescue.sh displaced restoring generation {generation:04}: {}",
            plural(*links, "link", "links")
        )],
        Why::VerifyCopy => vec![
            "a scratch copy the sandboxed verify-config parsed (D55); nothing refers to it".into(),
        ],
        Why::Interrupted { was } => vec![format!(
            "gc was removing {} and stopped; what is left is part of what was confirmed then (D62)",
            was.display()
        )],
    }
}

/// One reason an entry is kept, as a clause. The first of them is also what
/// `collect` says when it refuses a kept entry.
pub fn keep_reason(k: Option<&Keep>, home: &Path) -> String {
    match k {
        Some(k) => keep_line(k, home),
        None => "kept".into(),
    }
}

fn keep_line(k: &Keep, home: &Path) -> String {
    let at = |p: &Path| p.display().to_string();
    match k {
        Keep::NotADirectory { kind } => format!(
            "is a {}, not a directory: every entry ricepilot makes here is a directory, so this \
             one is not ricepilot's to remove",
            kind_word(*kind)
        ),
        Keep::UnknownName => "is not a name ricepilot gives an entry here (a switch id like \
                              20260927T101500Z, or rescue-NNNN in the attic), so it is not \
                              ricepilot's to remove"
            .into(),
        Keep::Unreadable { why } => format!("could not be read: {why}"),
        Keep::OtherMount { at: p } => format!(
            "{} is on another filesystem or mount, and gc never crosses one",
            at(p)
        ),
        Keep::NoMountId { at: p } => format!(
            "the kernel does not say which mount {} is on, so gc cannot rule out a bind mount \
             there, and it never crosses one",
            at(p)
        ),
        Keep::CannotEmpty { at: p, mode } => format!(
            "{} is a directory gc could not empty (mode {mode:04o}, or owned by someone else); \
             gc changes no permissions",
            at(p)
        ),
        Keep::TooDeep { at: p } => format!(
            "{} is more than {} levels down, deeper than gc walks",
            at(p),
            super::MAX_DEPTH
        ),
        Keep::NoRecord { expected } => format!(
            "{} is not there, and it is the record of what this holds; gc removes only what a \
             record accounts for",
            at(expected)
        ),
        Keep::RecordUnreadable { path: p, why } => format!(
            "{}, the record of what this holds, could not be read: {why}",
            at(p)
        ),
        Keep::Referenced { by, target } => format!("{by}: {}", at(target)),
        Keep::Unaccounted { at: p, kind } => format!(
            "holds {}, a {} no record says ricepilot put there",
            at(p),
            kind_word(*kind)
        ),
        Keep::NotALink { at: p, kind, dest } => format!(
            "holds {}, a {} where the record says the link displaced from {} went; gc removes \
             displaced links, and nothing else",
            at(p),
            kind_word(*kind),
            tilde(dest, home)
        ),
        Keep::Adopted {
            at: p,
            dest,
            copy,
            why,
        } => {
            let d = tilde(dest, home);
            let head = format!("holds {}, the directory adopt moved out of {d}", at(p));
            match why {
                NotHeld::DestNotOurs { now } if now == "absent" => format!(
                    "{head}, and nothing is at {d} now: this is the way back to it, which \
                     rollback does not take (D49). to put it back: mv -nT {} {}",
                    at(p),
                    dest.display()
                ),
                NotHeld::DestNotOurs { now } => format!(
                    "{head}, and {d} is not ricepilot's link now (it is a {now}), so this may be \
                     the only way back to it (D49)"
                ),
                NotHeld::CopyNotInAProfile => format!(
                    "{head}; its copy, {}, is not inside a registered profile, so nothing \
                     ricepilot knows of holds what it holds",
                    at(copy)
                ),
                NotHeld::CopyMissing { now } if now == "nothing" => format!(
                    "{head}; its copy, {}, is not there any more, so this is the only copy",
                    at(copy)
                ),
                NotHeld::CopyMissing { now } => format!(
                    "{head}; its copy, {}, is {now} now, not a directory, so this is the only \
                     copy",
                    at(copy)
                ),
                NotHeld::CopyDiffers { count, first } => format!(
                    "{head}; its copy, {}, differs from it in {} (first: {}), so this holds the \
                     only copy of those",
                    at(copy),
                    plural(*count, "path", "paths"),
                    first.split_whitespace().collect::<Vec<_>>().join(" ")
                ),
                NotHeld::CouldNotCompare { why } => format!(
                    "{head}; gc could not compare it with its copy, {}: {why}",
                    at(copy)
                ),
            }
        }
        Keep::NotTheSandbox { at: p } => format!(
            "holds {}, which the verify-config sandbox does not make (it makes root/ and run/)",
            at(p)
        ),
        Keep::Pending { tombstone } => format!(
            "an interrupted removal of the same name, {}, is still waiting; finish that first",
            at(tombstone)
        ),
    }
}

/// The line above each prompt.
pub fn question(i: &Item) -> String {
    format!(
        "\nremove {}? it holds {}. this cannot be undone.",
        i.path.display(),
        tally(&i.tally)
    )
}

/// After the operator did not type the name back.
pub fn not_typed(i: &Item) -> String {
    format!(
        "kept {}: its name was not typed back, so nothing was removed.",
        i.path.display()
    )
}

pub fn removed(c: &Collected) -> String {
    format!("removed {}: {}.", c.path.display(), tally(&c.tally))
}

pub fn not_removed(i: &Item, e: &Error) -> String {
    format!("NOT removed {}: {e}", i.path.display())
}

/// The last line of a `--commit` run.
pub fn summary(removed: usize, asked: usize, failed: usize) -> String {
    let mut s = format!(
        "\nremoved {removed} of {}.",
        plural(asked, "candidate", "candidates")
    );
    if failed > 0 {
        let _ = write!(
            s,
            " {} could not be removed; see above.",
            plural(failed, "entry", "entries")
        );
    }
    s.push('\n');
    s
}
