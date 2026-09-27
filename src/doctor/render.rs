//! The doctor report as text. Pure: a [`Report`] in, a `String` out.
//!
//! Laid out for a reader who already suspects something is wrong: the count
//! first, then every problem with its path, what is wrong, the rule and the
//! commands, then the hazards, then what was not checked, and every healthy
//! check folded into the one `ok:` line at the end. Nothing is wrapped by
//! width (see the module above).

use std::fmt::Write as _;

use super::Finding;
use super::Report;

pub fn text(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{}", headline(r));

    for (i, f) in r.problems.iter().enumerate() {
        finding(&mut s, "PROBLEM", i + 1, r.problems.len(), f);
    }
    for (i, f) in r.hazards.iter().enumerate() {
        finding(&mut s, "HAZARD", i + 1, r.hazards.len(), f);
    }

    if !r.notes.is_empty() {
        let _ = writeln!(s);
        let _ = writeln!(s, "not checked, or worth knowing:");
        for n in &r.notes {
            let _ = writeln!(s, "  - {n}");
        }
    }

    if !r.healthy.is_empty() {
        let _ = writeln!(s);
        let _ = writeln!(s, "ok: {}", r.healthy.join("; "));
    }
    s
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn headline(r: &Report) -> String {
    let found = match (r.problems.len(), r.hazards.len()) {
        (0, 0) => "no problems".to_string(),
        (0, h) => format!("no problems, {}", count(h, "hazard", "hazards")),
        (p, 0) => count(p, "problem", "problems"),
        (p, h) => format!(
            "{}, {}",
            count(p, "problem", "problems"),
            count(h, "hazard", "hazards")
        ),
    };
    let tail = if r.problems.is_empty() && r.hazards.is_empty() {
        "nothing was changed."
    } else {
        "nothing was changed; the commands are for you to run."
    };
    format!("ricepilot doctor: {found}. {tail}")
}

fn finding(s: &mut String, label: &str, n: usize, of: usize, f: &Finding) {
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "{label} {n} of {of}: {}",
        crate::shellword::written_out(&f.path)
    );
    let _ = writeln!(s, "  {}.", f.what);
    let _ = writeln!(s, "  rule: {}", f.rule);
    for d in &f.detail {
        let _ = writeln!(s);
        for line in d.lines() {
            let _ = writeln!(s, "  {line}");
        }
    }
    if !f.run.is_empty() {
        let _ = writeln!(s);
        let _ = writeln!(s, "  run:");
        for line in f
            .run
            .iter()
            .flat_map(|l| crate::shellword::command_lines(l))
        {
            let _ = writeln!(s, "    {line}");
        }
    }
}
