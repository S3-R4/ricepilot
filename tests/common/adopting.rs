//! A machine for the `adopt` crash harness: one real directory to adopt and
//! one profile with its own payload to adopt it into.
//!
//! A sibling of [`super::switching`] rather than an extension of it, for the
//! reason that one is a sibling of `scenario`: `switching` builds two
//! profiles and a ledger that already owns the live links, which is the
//! opposite of what `adopt` needs — a path ricepilot owns nothing at.

#![allow(dead_code)]

use std::path::PathBuf;

use ricepilot::cli::paths::Paths;
use ricepilot::ops::read;

use super::Fixture;

const GROUP: &str = "m4crash";

pub struct Machine {
    pub f: Fixture,
    /// `~/.config/hypr`, a real directory.
    pub dest: PathBuf,
    /// The profile directory, which owns its payload.
    pub profile: PathBuf,
    /// Where the copy lands, and what the new link points at.
    pub target: PathBuf,
}

pub fn build(case: &str) -> Machine {
    let f = Fixture::new_in(GROUP, case);
    let profile = f.profile("mine", "name = \"mine\"\nvolatile = []\ngenerated = []\n");

    f.dir(".config/hypr/scripts");
    f.file(".config/hypr/hyprland.conf", "monitor=,preferred,auto,1\n");
    f.file(".config/hypr/scripts/configs.fish", "#!/usr/bin/fish\n");
    let dest = f.path(".config/hypr");

    Machine {
        target: profile.join("hypr"),
        dest,
        profile,
        f,
    }
}

/// Re-describe a machine whose tree already exists, without disturbing it —
/// for inspecting what a helper process left behind.
pub fn attach(case: &str) -> Machine {
    let f = Fixture::attach(GROUP, case);
    let profile = f.path(".local/share/ricepilot/profiles/mine");
    Machine {
        dest: f.path(".config/hypr"),
        target: profile.join("hypr"),
        profile,
        f,
    }
}

/// Which side of the adopt the machine is on, or `None` if it is neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The user's directory is still a directory at its own path.
    Old,
    /// The destination is the new link, and the directory is in the attic.
    New,
}

impl Machine {
    pub fn paths(&self) -> Paths {
        Paths::rooted_at(self.f.home.clone())
    }

    /// Read the machine and say which side it is on. `None` means mixed,
    /// which is what R5 forbids and what the harness asserts against.
    pub fn side(&self) -> Option<Side> {
        let meta = read::lstat(&self.dest).ok()??;
        match meta.kind {
            read::Kind::Dir => {
                // Old: nothing was staged and nothing reached the attic.
                (!self.anything_in_the_attic() && self.staging_is_clear()).then_some(Side::Old)
            }
            read::Kind::Symlink => {
                let points_at_the_copy = read::readlink(&self.dest).ok()? == self.target;
                let directory_is_safe = self.in_attic().is_some();
                (points_at_the_copy && directory_is_safe && self.staging_is_clear())
                    .then_some(Side::New)
            }
            _ => None,
        }
    }

    /// No `.rp-tmp-0` or `.rp-tmp-0.rp-swap` left lying in `~/.config`.
    pub fn staging_is_clear(&self) -> bool {
        let staged = ricepilot::plan::adopt_staged_name(&self.dest);
        let swap = ricepilot::ops::mutate::fallback_slot(&staged);
        read::lstat_or_absent(&staged).ok().flatten().is_none()
            && read::lstat_or_absent(&swap).ok().flatten().is_none()
    }

    fn anything_in_the_attic(&self) -> bool {
        self.in_attic().is_some()
    }

    /// Where the displaced directory ended up, if it has been displaced. The
    /// attic directory is named after the operation's id, so it is found
    /// rather than predicted.
    pub fn in_attic(&self) -> Option<PathBuf> {
        let attic = self.f.state().join("attic");
        let rel = ricepilot::plan::adopt_attic_rel(&self.dest);
        for id in read::list_dir(&attic).ok()? {
            let candidate = attic.join(id).join(&rel);
            if read::lstat_or_absent(&candidate).ok().flatten().is_some() {
                return Some(candidate);
            }
        }
        None
    }
}
