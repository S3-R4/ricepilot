//! `ricepilot gc [--commit]` — the one command that removes anything.
//!
//! 1. Take the lock, for the dry run too (the reason `recover` does, D27): an
//!    itemisation of an attic a switch is writing into is out of date by the
//!    time it is read.
//! 2. Itemise every entry — path, what it holds, its size, and why it is a
//!    candidate or why it is kept — and print all of it (D61).
//! 3. Stop there unless `--commit`.
//! 4. For each candidate in turn, ask for its name to be typed back
//!    (`confirm::typed_back`, D47's two widgets, no default that removes).
//!    Only the exact name removes it, through `gc::collect`, which reads the
//!    entry again and refuses if it is not what was listed (D62).
//!
//! There is no `--yes`, and no answer that covers more than one entry.

use crate::error::ExitCode;
use crate::ops::lock;
use crate::{gc, Result};

use super::paths::Paths;
use super::{confirm, Output};

pub fn run(paths: &Paths, commit: bool) -> Result<Output> {
    let held = lock::acquire(&paths.lock_path()?)?;
    let survey = gc::candidates(paths)?;
    let report = gc::render::survey(&survey, paths, commit);

    let asked: Vec<&gc::Item> = survey.candidates().collect();
    if !commit || asked.is_empty() {
        return Ok(Output::from(report));
    }

    // ---- Everything is itemised above the first question. ----
    print!("{report}");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut removed = 0;
    let mut failed: Option<ExitCode> = None;
    let mut failures = 0;
    for item in &asked {
        let line = match confirm::typed_back(&gc::render::question(item), &item.name) {
            None => gc::render::not_typed(item),
            Some(named) => match gc::collect(&held, paths, item, &named) {
                Ok(done) => {
                    removed += 1;
                    gc::render::removed(&done)
                }
                Err(e) => {
                    failures += 1;
                    failed.get_or_insert(e.exit_code());
                    gc::render::not_removed(item, &e)
                }
            },
        };
        println!("{line}");
    }

    Ok(Output {
        text: gc::render::summary(removed, asked.len(), failures),
        code: failed.unwrap_or(ExitCode::Ok),
    })
}
