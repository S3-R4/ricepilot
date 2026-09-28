//! **The only module permitted to contain a delete primitive** (`SAFETY.md`
//! R2), and the one irreversible thing ricepilot can do. Since M4 the attic
//! can hold the user's own directories — `adopt` *moves* the original there,
//! it does not copy it (D46, D49) — so "it was only the attic" is no longer
//! true, and everything here is written as if the entry in front of it were
//! somebody's only copy of their configuration.
//!
//! What gc looks at, all of it inside the state directory and nothing else:
//!
//! * `state/attic/<id>/` — what a switch, rollback, adopt or recover
//!   displaced — and `state/attic/rescue-NNNN/`, what `rescue.sh` did;
//! * `state/verify/<id>/` — the verify-config scratch copies (D55);
//! * `state/gc/<area>-<name>/` — a removal gc began and did not finish (D62).
//!
//! [`candidates`] only reads. For every entry it decides, and says why,
//! whether the entry is a candidate or is kept (D61): an attic entry is a
//! candidate only when every object in it is one its own journal (or, for a
//! rescue attic, its generation) says ricepilot put there, when nothing
//! ricepilot records still points into it, and when any directory `adopt`
//! displaced into it has an identical copy in a registered profile *and* its
//! destination is ricepilot's link. Anything it cannot account for keeps the
//! entry.
//!
//! [`collect`] removes one candidate, given the lock and a
//! [`crate::cli::confirm::Named`] — the entry's name typed back by a human —
//! after reading the entry again and finding it the same, inode for inode.
//! The removal is [`remove`], the one file in the crate that names the
//! syscall (D62, D63).

pub mod remove;
pub mod render;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::cli::confirm::Named;
use crate::cli::paths::{self, Paths, Profile};
use crate::generations::{self, Generation};
use crate::journal::{self, Displaced, Journal};
use crate::ledger;
use crate::observe::{self, Ownership, Shape};
use crate::ops::lock::Lock;
use crate::ops::look::Live;
use crate::ops::read::{self, DirFd, Found, Kind};
use crate::verify;
use crate::{Error, Result};

/// How deep gc will walk into an entry. An attic entry is the displaced
/// object's absolute path (a handful of levels) plus whatever depth an
/// adopted directory had; deeper than this is kept, not walked, so a hostile
/// or accidental tree cannot run the process out of descriptors.
const MAX_DEPTH: usize = 128;

/// Where an entry lives. In the order gc lists and asks about them: a
/// removal that was interrupted first, since finishing it is the one thing
/// the operator has already said yes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    /// `state/gc/`: an entry gc renamed there to remove, and did not finish.
    Interrupted,
    /// `state/attic/`.
    Attic,
    /// `state/verify/`.
    Verify,
}

impl Area {
    pub fn dir(self, state: &Path) -> PathBuf {
        match self {
            Area::Interrupted => gc_dir(state),
            Area::Attic => state.join("attic"),
            Area::Verify => state.join("verify"),
        }
    }

    /// The prefix an entry of this area takes in `state/gc/`.
    pub fn word(self) -> &'static str {
        match self {
            Area::Interrupted => "gc",
            Area::Attic => "attic",
            Area::Verify => "verify",
        }
    }
}

/// `state/gc/`: where [`collect`] moves an entry before it removes anything
/// from it, and the only directory the removal ever runs in (D62).
pub fn gc_dir(state: &Path) -> PathBuf {
    state.join("gc")
}

/// One object inside an entry, as the walk found it. `rel` is relative to
/// the entry's own directory, which is the object with an empty `rel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obj {
    pub rel: PathBuf,
    pub found: Found,
}

/// What an entry holds, counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub dirs: usize,
    pub links: usize,
    pub files: usize,
    /// Sockets, fifos, devices — anything that is none of the other three.
    pub other: usize,
    /// `st_size` of everything but the directories: file contents and link
    /// target strings. A directory's own size depends on the filesystem.
    pub bytes: u64,
}

impl Tally {
    fn add(&mut self, f: &Found) {
        match f.meta.kind {
            Kind::Dir => self.dirs += 1,
            Kind::Symlink => self.links += 1,
            Kind::File => self.files += 1,
            Kind::Other => self.other += 1,
        }
        if f.meta.kind != Kind::Dir {
            self.bytes += f.size;
        }
    }

    pub fn of(listing: &[Obj]) -> Tally {
        let mut t = Tally::default();
        for o in listing {
            t.add(&o.found);
        }
        t
    }
}

/// One entry, itemised: where it is, what it holds, and the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub area: Area,
    /// The directory's own name — what the operator types back.
    pub name: String,
    pub path: PathBuf,
    /// Every object in it, the entry's own directory first, parents before
    /// their children. Empty when the entry could not be read at all.
    pub listing: Vec<Obj>,
    pub tally: Tally,
    pub verdict: Verdict,
}

impl Item {
    pub fn is_candidate(&self) -> bool {
        matches!(self.verdict, Verdict::Candidate(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Candidate(Why),
    /// Every reason, not only the first: the report is read by someone
    /// deciding what to do about the entry by hand.
    Kept(Vec<Keep>),
}

/// Why an entry is a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// A switch, rollback, adopt or recover's attic: every object in it is at
    /// a place its journal names.
    Displaced {
        journal: PathBuf,
        links: usize,
        adopted: Vec<Copied>,
    },
    /// What `rescue.sh` displaced restoring generation `generation`.
    Rescue { generation: u32, links: usize },
    /// A verify-config scratch copy (D55).
    VerifyCopy,
    /// A removal that was interrupted; `was` is where the entry used to be.
    Interrupted { was: PathBuf },
}

/// A directory `adopt` displaced, and the proof that losing it loses nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// Relative to the entry.
    pub at: PathBuf,
    pub dest: PathBuf,
    pub profile: String,
    pub copy: PathBuf,
}

/// Why an entry is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keep {
    /// Every entry ricepilot makes is a directory.
    NotADirectory {
        kind: Kind,
    },
    /// Not a name ricepilot gives an entry of this area.
    UnknownName,
    Unreadable {
        why: String,
    },
    /// On another filesystem or another mount than the area: gc never
    /// crosses one.
    OtherMount {
        at: PathBuf,
    },
    /// The kernel reports no mount id, so a same-device bind mount cannot be
    /// ruled out.
    NoMountId {
        at: PathBuf,
    },
    /// A directory gc could not empty: not owner-writable, or not owned by
    /// the owner of the state directory. gc changes no permissions.
    CannotEmpty {
        at: PathBuf,
        mode: u32,
    },
    TooDeep {
        at: PathBuf,
    },
    /// The record that says what the entry holds is not there.
    NoRecord {
        expected: PathBuf,
    },
    RecordUnreadable {
        path: PathBuf,
        why: String,
    },
    /// Something ricepilot records, or a link it manages, points into it.
    Referenced {
        by: String,
        target: PathBuf,
    },
    /// An object no record says ricepilot put there.
    Unaccounted {
        at: PathBuf,
        kind: Kind,
    },
    /// Something other than a link where the record says a link was put.
    NotALink {
        at: PathBuf,
        kind: Kind,
        dest: PathBuf,
    },
    /// A directory `adopt` displaced that gc cannot prove is held elsewhere.
    Adopted {
        at: PathBuf,
        dest: PathBuf,
        copy: PathBuf,
        why: NotHeld,
    },
    /// Not something the verify-config sandbox makes.
    NotTheSandbox {
        at: PathBuf,
    },
    /// An interrupted removal of the same name is waiting in `state/gc/`.
    Pending {
        tombstone: PathBuf,
    },
}

/// Why a displaced directory is not provably held anywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotHeld {
    /// The destination is not ricepilot's link now — the adopt-then-rollback
    /// state (D49), in which this directory is the way back.
    DestNotOurs {
        now: String,
    },
    /// The copy is not inside any registered profile.
    CopyNotInAProfile,
    CopyMissing {
        now: String,
    },
    /// The copy differs; `first` is the first difference.
    CopyDiffers {
        count: usize,
        first: String,
    },
    CouldNotCompare {
        why: String,
    },
}

/// Everything gc looked at, and what it could not look at.
#[derive(Debug, Clone, Default)]
pub struct Survey {
    pub items: Vec<Item>,
    /// Things gc could not check and says so about (R7).
    pub notes: Vec<String>,
}

impl Survey {
    pub fn candidates(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| i.is_candidate())
    }
}

/// What [`collect`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collected {
    pub path: PathBuf,
    pub tally: Tally,
}

/// Itemise every entry gc would consider — candidates and kept alike, with
/// the reason for each. Read-only; the caller holds the lock (D61).
///
/// Refuses outright, rather than itemising, when it cannot tell what refers
/// to what: an operation in flight, or a ledger, generation or profile
/// manifest that does not parse.
pub fn candidates(paths: &Paths) -> Result<Survey> {
    refuse_in_flight(paths)?;
    let refs = Refs::load(paths)?;
    let mut survey = Survey {
        items: Vec::new(),
        notes: refs.notes.clone(),
    };
    for area in [Area::Interrupted, Area::Attic, Area::Verify] {
        let Some(dir) = open_area(paths, area)? else {
            continue;
        };
        let at = dir.found()?;
        for name in dir.names()? {
            survey.items.push(assess(&refs, area, &dir, &at, &name));
        }
    }
    Ok(survey)
}

/// Remove one candidate. `named` is the proof a human typed its name back;
/// `lock` that no other ricepilot is running.
///
/// Before anything changes, the entry is assessed again from nothing — the
/// records re-read, the tree re-walked — and must still be a candidate with
/// exactly the listing the operator was shown: every object the same
/// `(dev, ino)`, kind, size, mode and mtime. Anything else is a refusal with
/// nothing removed. Then [`remove::bury`] moves it to `state/gc/` and
/// [`remove::erase`] takes it apart there (D62).
pub fn collect(lock: &Lock, paths: &Paths, item: &Item, named: &Named) -> Result<Collected> {
    let _held = lock;
    if named.name() != item.name {
        return Err(Error::Refused {
            rule: "R6",
            path: item.path.clone(),
            why: format!(
                "the name typed back was {:?}, not this entry's; nothing was removed",
                named.name()
            ),
        });
    }
    if let Verdict::Kept(why) = &item.verdict {
        return Err(Error::Refused {
            rule: "R2",
            path: item.path.clone(),
            why: format!(
                "is kept, not a candidate ({}); nothing was removed",
                render::keep_reason(why.first(), &paths.home)
            ),
        });
    }

    refuse_in_flight(paths)?;
    let now = assess_again(paths, item.area, &item.name)?;
    if let Verdict::Kept(why) = &now.verdict {
        return Err(Error::Refused {
            rule: "R4",
            path: item.path.clone(),
            why: format!(
                "is no longer a candidate ({}); nothing was removed",
                render::keep_reason(why.first(), &paths.home)
            ),
        });
    }
    if let Some(what) = first_difference(&item.listing, &now.listing) {
        return Err(Error::Refused {
            rule: "R4",
            path: item.path.clone(),
            why: format!(
                "has changed since it was listed ({what}); nothing was removed. run `ricepilot \
                 gc` again to see it as it is now"
            ),
        });
    }

    let Some(root) = item.listing.first().map(|o| o.found) else {
        return Err(gone(&item.path));
    };
    let tomb = match item.area {
        Area::Interrupted => remove::Tombstone::already(&gc_dir(&paths.state), &item.name, root),
        area => remove::bury(
            &item.path,
            &gc_dir(&paths.state),
            &format!("{}-{}", area.word(), item.name),
            root,
        )?,
    };
    let tally = remove::erase(&tomb, &item.listing).map_err(|e| Error::Refused {
        rule: "R2",
        path: tomb.path(),
        why: format!(
            "gc stopped part way through removing this: {e}. what is left stays there, and \
             `ricepilot gc` lists it as an interrupted removal"
        ),
    })?;
    Ok(Collected {
        path: item.path.clone(),
        tally,
    })
}

// ---------------------------------------------------------------------------
// What refers to what
// ---------------------------------------------------------------------------

/// Nothing is itemised, let alone removed, while an operation is in flight:
/// until `recover` has run, the attic of that operation is the recovery's
/// working space and every other record may describe a half-finished state.
fn refuse_in_flight(paths: &Paths) -> Result<()> {
    if read::lstat_or_absent(&paths.journal_path())?.is_some() {
        return Err(Error::Refused {
            rule: "R4",
            path: paths.journal_path(),
            why: "an operation is in flight and did not finish. run `ricepilot recover` first; \
                  until then gc will not decide what in the attic is still needed"
                .into(),
        });
    }
    Ok(())
}

/// Everything in ricepilot's records that an entry might be needed for.
struct Refs {
    state: PathBuf,
    /// Owner of the state directory: the owner every directory gc empties
    /// must have.
    uid: u32,
    profiles: Vec<Profile>,
    ownership: Ownership,
    generations: BTreeMap<u32, Generation>,
    /// Retired journals by the attic directory they name, or why one could
    /// not be read.
    journals: BTreeMap<String, std::result::Result<Journal, (PathBuf, String)>>,
    /// `(who, where)`: every path a record or a managed link points at,
    /// resolved lexically.
    pointers: Vec<(String, PathBuf)>,
    /// Names in `state/gc/`.
    tombstones: Vec<String>,
    notes: Vec<String>,
}

impl Refs {
    fn load(paths: &Paths) -> Result<Self> {
        let state = paths.state.clone();
        let uid = read::lstat(&state)?.map(|m| m.uid).unwrap_or(u32::MAX);
        let led = ledger::load(&paths.ledger_path())?;
        let profiles = paths::load_all(paths)?;
        let ownership = led.ownership(profiles.iter().map(|p| p.root(&paths.home)).collect());

        let mut generations = BTreeMap::new();
        let gen_dir = generations::dir(&state);
        if read::lstat_or_absent(&gen_dir)?.is_some() {
            for name in read::list_dir(&gen_dir)? {
                let name = name.to_string_lossy();
                let Some(id) = name
                    .strip_suffix(".toml")
                    .filter(|n| n.len() >= 4 && n.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|n| n.parse::<u32>().ok())
                else {
                    continue;
                };
                generations.insert(id, generations::load(&state, id)?);
            }
        }

        let mut journals = BTreeMap::new();
        let jdir = paths.journal_dir();
        if read::lstat_or_absent(&jdir)?.is_some() {
            for name in read::list_dir(&jdir)? {
                let name = name.to_string_lossy().into_owned();
                let Some(id) = name
                    .strip_prefix("done-")
                    .and_then(|n| n.strip_suffix(".toml"))
                else {
                    continue;
                };
                let path = jdir.join(&name);
                match read::slurp(&path).and_then(|t| journal::parse(&t, &path)) {
                    Ok(j) => {
                        let key = j
                            .attic
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_else(|| id.to_string());
                        journals.insert(key, Ok(j));
                    }
                    Err(e) => {
                        journals.insert(id.to_string(), Err((path, said(&e))));
                    }
                }
            }
        }

        let mut pointers = Vec::new();
        let mut dests: Vec<PathBuf> = Vec::new();
        for g in generations.values() {
            for e in &g.entries {
                dests.push(e.dest.clone());
                if let Some(t) = &e.target {
                    pointers.push((
                        format!(
                            "generation {:04} links {} to it",
                            g.id,
                            tilde(&e.dest, &paths.home)
                        ),
                        resolve(&e.dest, t),
                    ));
                }
            }
        }
        for row in &led.entries {
            dests.push(row.dest.clone());
            pointers.push((
                format!(
                    "the ledger records {} as a link to it",
                    tilde(&row.dest, &paths.home)
                ),
                resolve(&row.dest, &row.target),
            ));
        }
        for p in &profiles {
            pointers.push((
                format!("profile `{}` has its root there", p.name),
                lexical(&p.root(&paths.home)),
            ));
            for t in p.manifest.targets(&p.dir, &paths.home) {
                dests.push(t.dest);
            }
        }
        // The live links at every destination ricepilot knows about, as they
        // are now — which is how a link made by hand into the attic (the
        // way back from D49, done with `ln -s` rather than `mv`) is found.
        dests.sort();
        dests.dedup();
        let mut notes = Vec::new();
        for d in &dests {
            match read::lstat_or_absent(d) {
                Ok(Some(m)) if m.kind == Kind::Symlink => match read::readlink(d) {
                    Ok(t) => pointers.push((
                        format!("the link at {} points into it", tilde(d, &paths.home)),
                        resolve(d, &t),
                    )),
                    Err(e) => notes.push(could_not_see(d, &e, &paths.home)),
                },
                Ok(_) => {}
                Err(e) => notes.push(could_not_see(d, &e, &paths.home)),
            }
        }

        let tombstones = match read::lstat_or_absent(&gc_dir(&state))? {
            Some(_) => read::list_dir(&gc_dir(&state))?
                .into_iter()
                .map(|n| n.to_string_lossy().into_owned())
                .collect(),
            None => Vec::new(),
        };

        Ok(Refs {
            state,
            uid,
            profiles,
            ownership,
            generations,
            journals,
            pointers,
            tombstones,
            notes,
        })
    }

    /// Every record or managed link that points at `path` or below it.
    fn pointing_into(&self, path: &Path) -> Vec<Keep> {
        let path = lexical(path);
        let mut out: Vec<Keep> = Vec::new();
        for (by, target) in &self.pointers {
            if target.starts_with(&path) {
                let k = Keep::Referenced {
                    by: by.clone(),
                    target: target.clone(),
                };
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        out
    }
}

fn could_not_see(d: &Path, e: &Error, home: &Path) -> String {
    format!(
        "could not read {} to see whether it points into the attic ({}); an entry it points \
         into would still be listed as a candidate",
        tilde(d, home),
        said(e)
    )
}

/// Where a link's target string leads, lexically: relative to the link's
/// directory, with `.` and `..` taken as written. Nothing is followed.
fn resolve(link: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        lexical(target)
    } else {
        lexical(&link.parent().unwrap_or(Path::new("/")).join(target))
    }
}

fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Assessing one entry
// ---------------------------------------------------------------------------

fn open_area(paths: &Paths, area: Area) -> Result<Option<DirFd>> {
    let dir = area.dir(&paths.state);
    match read::lstat_or_absent(&dir)? {
        None => Ok(None),
        Some(m) if m.kind == Kind::Dir => Ok(Some(DirFd::open(&dir)?)),
        Some(_) => Err(Error::Refused {
            rule: "R3",
            path: dir,
            why: "is not a directory. gc looks only inside the real directories ricepilot made, \
                  and never follows a link out of the state directory"
                .into(),
        }),
    }
}

/// [`assess`] for one entry by name, with every record read afresh.
fn assess_again(paths: &Paths, area: Area, name: &str) -> Result<Item> {
    let refs = Refs::load(paths)?;
    let dir = open_area(paths, area)?.ok_or_else(|| gone(&area.dir(&paths.state)))?;
    let at = dir.found()?;
    if dir.child(OsStr::new(name))?.is_none() {
        return Err(gone(&dir.path().join(name)));
    }
    Ok(assess(&refs, area, &dir, &at, OsStr::new(name)))
}

fn gone(p: &Path) -> Error {
    Error::Refused {
        rule: "R4",
        path: p.to_path_buf(),
        why: "is no longer there; nothing was removed".into(),
    }
}

fn assess(refs: &Refs, area: Area, dir: &DirFd, at: &Found, name: &OsStr) -> Item {
    let path = dir.path().join(name);
    let name = name.to_string_lossy().into_owned();
    let (listing, mut keep) = match walk(dir, at, OsStr::new(&name)) {
        Ok(w) => w,
        Err(e) => (Vec::new(), vec![Keep::Unreadable { why: said(&e) }]),
    };
    let tally = Tally::of(&listing);
    let item = |verdict| Item {
        area,
        name: name.clone(),
        path: path.clone(),
        listing: listing.clone(),
        tally,
        verdict,
    };
    let Some(root) = listing.first() else {
        return item(Verdict::Kept(keep));
    };
    if root.found.meta.kind != Kind::Dir {
        return item(Verdict::Kept(vec![Keep::NotADirectory {
            kind: root.found.meta.kind,
        }]));
    }
    let ours = name_is_ours(area, &name);
    if !ours {
        keep.push(Keep::UnknownName);
    }
    for o in &listing {
        let m = o.found.meta;
        if m.kind == Kind::Dir && (m.uid != refs.uid || m.mode & 0o300 != 0o300) {
            keep.push(Keep::CannotEmpty {
                at: path.join(&o.rel),
                mode: m.mode,
            });
        }
    }
    keep.extend(refs.pointing_into(&path));
    if area != Area::Interrupted {
        let tomb = format!("{}-{name}", area.word());
        if refs.tombstones.contains(&tomb) {
            keep.push(Keep::Pending {
                tombstone: gc_dir(&refs.state).join(tomb),
            });
        }
    }

    let why = match area {
        Area::Verify => {
            for o in &listing {
                let top = o.rel.components().next();
                let ok = match top {
                    None => true,
                    Some(c) => c.as_os_str() == "root" || c.as_os_str() == "run",
                };
                if !ok {
                    keep.push(Keep::NotTheSandbox {
                        at: path.join(&o.rel),
                    });
                    break;
                }
            }
            Why::VerifyCopy
        }
        Area::Interrupted => Why::Interrupted {
            was: name
                .split_once('-')
                .map(|(a, n)| refs.state.join(a).join(n))
                .unwrap_or_else(|| path.clone()),
        },
        // What an entry ricepilot did not name holds is not looked into:
        // there is no record to account for it against.
        Area::Attic if !ours => unaccounted(PathBuf::new()),
        Area::Attic => account(refs, &name, &path, &listing, &mut keep),
    };

    if keep.is_empty() {
        item(Verdict::Candidate(why))
    } else {
        item(Verdict::Kept(keep))
    }
}

/// Walk one entry through directory descriptors, never following a link and
/// never going below a mount. Returns the listing and anything the walk
/// itself found that keeps the entry.
fn walk(area: &DirFd, at: &Found, name: &OsStr) -> Result<(Vec<Obj>, Vec<Keep>)> {
    let mut listing = Vec::new();
    let mut keep = Vec::new();
    let Some(root) = area.child(name)? else {
        return Err(gone(&area.path().join(name)));
    };
    listing.push(Obj {
        rel: PathBuf::new(),
        found: root,
    });
    if root.meta.kind != Kind::Dir {
        return Ok((listing, keep));
    }
    let path = area.path().join(name);
    if let Some(k) = off_the_mount(at, &root, &path) {
        keep.push(k);
        return Ok((listing, keep));
    }
    let dir = area.open_child(name)?;
    same_or_raced(&dir, &root)?;
    descend(&dir, Path::new(""), 1, at, &mut listing, &mut keep)?;
    Ok((listing, keep))
}

fn descend(
    dir: &DirFd,
    rel: &Path,
    depth: usize,
    at: &Found,
    listing: &mut Vec<Obj>,
    keep: &mut Vec<Keep>,
) -> Result<()> {
    for name in dir.names()? {
        // Gone between the listing and the look: it is not there now.
        let Some(found) = dir.child(&name)? else {
            continue;
        };
        let child = rel.join(&name);
        listing.push(Obj {
            rel: child.clone(),
            found,
        });
        let path = dir.path().join(&name);
        if let Some(k) = off_the_mount(at, &found, &path) {
            keep.push(k);
            continue;
        }
        if found.meta.kind != Kind::Dir {
            continue;
        }
        if depth >= MAX_DEPTH {
            keep.push(Keep::TooDeep { at: path });
            continue;
        }
        let sub = dir.open_child(&name)?;
        same_or_raced(&sub, &found)?;
        descend(&sub, &child, depth + 1, at, listing, keep)?;
    }
    Ok(())
}

/// Whether `f` is on the same filesystem and the same mount as the area
/// directory. `st_dev` alone misses a bind mount from the same filesystem;
/// the mount id does not.
fn off_the_mount(at: &Found, f: &Found, path: &Path) -> Option<Keep> {
    if f.meta.dev != at.meta.dev {
        return Some(Keep::OtherMount {
            at: path.to_path_buf(),
        });
    }
    match (at.mount, f.mount) {
        (Some(a), Some(b)) if a == b => None,
        (Some(_), Some(_)) => Some(Keep::OtherMount {
            at: path.to_path_buf(),
        }),
        _ => Some(Keep::NoMountId {
            at: path.to_path_buf(),
        }),
    }
}

/// The directory just opened is the one just looked at.
fn same_or_raced(dir: &DirFd, seen: &Found) -> Result<()> {
    let now = dir.found()?;
    if (now.meta.dev, now.meta.ino) != (seen.meta.dev, seen.meta.ino) {
        return Err(Error::Refused {
            rule: "R4",
            path: dir.path().to_path_buf(),
            why: "was replaced while gc was reading it".into(),
        });
    }
    Ok(())
}

/// The names ricepilot gives: a switch id (`YYYYMMDDTHHMMSSZ`, perhaps with
/// `-N`, D40) for an attic or verify entry, `rescue-NNNN` for a rescue attic,
/// and `<area>-<name>` in `state/gc/`. A verify copy may carry one `-M` more:
/// the sandbox names it `<id>-M` when `<id>` is taken there, and the id may
/// already have its own `-N` (D78).
pub fn name_is_ours(area: Area, name: &str) -> bool {
    match area {
        Area::Attic => is_id(name) || rescue_generation(name).is_some(),
        Area::Verify => {
            is_id(name)
                || name
                    .rsplit_once('-')
                    .is_some_and(|(id, m)| is_numeric_suffix(m) && is_id(id))
        }
        Area::Interrupted => match name.split_once('-') {
            Some(("attic", n)) => name_is_ours(Area::Attic, n),
            Some(("verify", n)) => name_is_ours(Area::Verify, n),
            _ => false,
        },
    }
}

fn is_id(name: &str) -> bool {
    let b = name.as_bytes();
    let stamp = b.len() >= 16
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'Z';
    stamp
        && match &name[16..] {
            "" => true,
            rest => rest.strip_prefix('-').is_some_and(is_numeric_suffix),
        }
}

fn is_numeric_suffix(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())
}

fn rescue_generation(name: &str) -> Option<u32> {
    name.strip_prefix("rescue-")
        .filter(|n| n.len() >= 4 && n.bytes().all(|c| c.is_ascii_digit()))
        .and_then(|n| n.parse().ok())
}

/// Account for every object in an attic entry against the record that says
/// what was put there, and prove any displaced directory is held elsewhere.
fn account(refs: &Refs, name: &str, path: &Path, listing: &[Obj], keep: &mut Vec<Keep>) -> Why {
    // The places things were put, and whether `rename_to_attic`'s `.N`
    // suffix may have been added to them.
    let (places, suffixed, mut why): (Vec<(PathBuf, Displaced)>, bool, Why) =
        match rescue_generation(name) {
            Some(n) => match refs.generations.get(&n) {
                None => {
                    keep.push(Keep::NoRecord {
                        expected: generations::path(&refs.state, n),
                    });
                    return Why::Rescue {
                        generation: n,
                        links: 0,
                    };
                }
                Some(g) => (
                    g.entries
                        .iter()
                        .filter(|e| e.target.is_none())
                        .map(|e| {
                            (
                                crate::rescue::parked_rel(&e.dest),
                                Displaced::Link {
                                    dest: e.dest.clone(),
                                },
                            )
                        })
                        .collect(),
                    false,
                    Why::Rescue {
                        generation: n,
                        links: 0,
                    },
                ),
            },
            None => {
                let journal = refs.state.join("journal").join(format!("done-{name}.toml"));
                match refs.journals.get(name) {
                    None => {
                        keep.push(Keep::NoRecord {
                            expected: journal.clone(),
                        });
                        return unaccounted(journal);
                    }
                    Some(Err((p, e))) => {
                        keep.push(Keep::RecordUnreadable {
                            path: p.clone(),
                            why: e.clone(),
                        });
                        return unaccounted(journal);
                    }
                    Some(Ok(j)) => (
                        j.displaced(),
                        true,
                        Why::Displaced {
                            journal,
                            links: 0,
                            adopted: Vec::new(),
                        },
                    ),
                }
            }
        };

    let mut links = 0;
    let mut dirs: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    for o in listing.iter().skip(1) {
        if dirs.iter().any(|(at, _, _)| o.rel.starts_with(at)) {
            // Inside a displaced directory: accounted for, or not, by the
            // proof for that directory below.
            continue;
        }
        let kind = o.found.meta.kind;
        let at = path.join(&o.rel);
        match place_of(&o.rel, &places, suffixed) {
            Some(Displaced::Link { dest }) => match kind {
                Kind::Symlink => links += 1,
                _ => keep.push(Keep::NotALink {
                    at,
                    kind,
                    dest: dest.clone(),
                }),
            },
            // `adopt` moves a real directory here and nothing else: no
            // recovery arm sends a link to this place (a staged link goes to
            // `<name>.staged`, a place of its own). So anything but a
            // directory is unaccounted for — a link here would carry a
            // target string no record holds.
            Some(Displaced::Directory { dest, new_target }) => match kind {
                Kind::Dir => dirs.push((o.rel.clone(), dest.clone(), new_target.clone())),
                _ => keep.push(Keep::Unaccounted { at, kind }),
            },
            None => {
                let holds_a_place = places
                    .iter()
                    .any(|(p, _)| p.starts_with(&o.rel) && p != &o.rel);
                if !(kind == Kind::Dir && holds_a_place) {
                    keep.push(Keep::Unaccounted { at, kind });
                }
            }
        }
    }

    let mut adopted = Vec::new();
    for (rel, dest, copy) in dirs {
        match held_elsewhere(refs, &path.join(&rel), &dest, &copy) {
            Ok(profile) => adopted.push(Copied {
                at: rel,
                dest,
                profile,
                copy,
            }),
            Err(why) => keep.push(Keep::Adopted {
                at: path.join(&rel),
                dest,
                copy,
                why,
            }),
        }
    }

    match &mut why {
        Why::Displaced {
            links: l,
            adopted: a,
            ..
        } => {
            *l = links;
            *a = adopted;
        }
        Why::Rescue { links: l, .. } => *l = links,
        _ => {}
    }
    why
}

/// The reason an entry would have had, for one kept before it was counted.
fn unaccounted(journal: PathBuf) -> Why {
    Why::Displaced {
        journal,
        links: 0,
        adopted: Vec::new(),
    }
}

/// Which place `rel` is, allowing `rename_to_attic`'s `<name>.N` when
/// `suffixed`.
fn place_of<'a>(
    rel: &Path,
    places: &'a [(PathBuf, Displaced)],
    suffixed: bool,
) -> Option<&'a Displaced> {
    places.iter().find_map(|(p, d)| {
        let hit = rel == p
            || (suffixed
                && rel.parent() == p.parent()
                && match (rel.file_name(), p.file_name()) {
                    (Some(r), Some(f)) => r
                        .to_string_lossy()
                        .strip_prefix(&*f.to_string_lossy())
                        .and_then(|s| s.strip_prefix('.'))
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())),
                    _ => false,
                });
        hit.then_some(d)
    })
}

/// Whether the directory `adopt` displaced from `dest` into `at` is held
/// elsewhere: `dest` is ricepilot's link now — not the D49 state, in which
/// this directory is the way back — and `copy` is a real directory inside a
/// registered profile holding every path the directory holds, with the same
/// content, kind, mode and owner. Paths only the copy has do not matter;
/// paths only the directory has, or has differently, are what would be lost.
fn held_elsewhere(
    refs: &Refs,
    at: &Path,
    dest: &Path,
    copy: &Path,
) -> std::result::Result<String, NotHeld> {
    let could_not = |e: Error| NotHeld::CouldNotCompare { why: said(&e) };

    let meta = read::lstat_or_absent(dest).map_err(could_not)?;
    let shape = observe::shape_via(&Live, dest, meta, &refs.ownership).map_err(could_not)?;
    if !matches!(shape, Shape::OwnedLink { .. }) {
        return Err(NotHeld::DestNotOurs {
            now: shape.as_str().to_string(),
        });
    }

    let Some(profile) = refs
        .profiles
        .iter()
        .find(|p| paths::lexically_under(copy, &p.dir))
    else {
        return Err(NotHeld::CopyNotInAProfile);
    };
    match read::lstat_or_absent(copy).map_err(could_not)? {
        Some(m) if m.kind == Kind::Dir => {}
        Some(m) => {
            return Err(NotHeld::CopyMissing {
                now: format!("a {}", kind_word(m.kind)),
            })
        }
        None => {
            return Err(NotHeld::CopyMissing {
                now: "nothing".into(),
            })
        }
    }

    let was = verify::build("gc", at, &[], "gc").map_err(could_not)?;
    let now = verify::build("gc", copy, &[], "gc").map_err(could_not)?;
    let lost: Vec<verify::Difference> = verify::compare(&was, &now)
        .into_iter()
        .filter(|d| d.is_substantive() && !matches!(d, verify::Difference::Added { .. }))
        .collect();
    if let Some(first) = lost.first() {
        return Err(NotHeld::CopyDiffers {
            count: lost.len(),
            first: first.to_string(),
        });
    }
    Ok(profile.name.clone())
}

/// The first way two listings differ, in words, or `None` if they do not.
fn first_difference(was: &[Obj], now: &[Obj]) -> Option<String> {
    let index = |l: &[Obj]| -> BTreeMap<PathBuf, Found> {
        l.iter().map(|o| (o.rel.clone(), o.found)).collect()
    };
    let (a, b) = (index(was), index(now));
    let show = |rel: &Path| {
        if rel.as_os_str().is_empty() {
            "the entry itself".to_string()
        } else {
            rel.display().to_string()
        }
    };
    for (rel, f) in &a {
        match b.get(rel) {
            None => return Some(format!("{} is gone", show(rel))),
            Some(g) if g != f => return Some(format!("{} is not what it was", show(rel))),
            Some(_) => {}
        }
    }
    b.keys()
        .find(|rel| !a.contains_key(*rel))
        .map(|rel| format!("{} is new", show(rel)))
}

pub fn kind_word(k: Kind) -> &'static str {
    match k {
        Kind::Dir => "directory",
        Kind::File => "file",
        Kind::Symlink => "link",
        Kind::Other => "socket, fifo or device",
    }
}

/// `~/…` for a path under the home, as the rest of the output spells it.
pub fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// An error as a clause: a refusal's reason without its path and rule.
fn said(e: &Error) -> String {
    match e {
        Error::Refused { why, path, .. } => format!("{}: {why}", path.display()),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::read::Meta;

    fn found(kind: Kind, dev: u64, ino: u64, mount: Option<u64>) -> Found {
        Found {
            meta: Meta {
                kind,
                dev,
                ino,
                mode: 0o755,
                uid: 1000,
                gid: 1000,
                mtime_ns: 1,
            },
            size: 0,
            mount,
        }
    }

    /// The mount check a test cannot make happen for real without
    /// privileges: another device, another mount on the same device (a bind
    /// mount), and a kernel that reports no mount id all keep the entry.
    #[test]
    fn anything_off_the_areas_mount_keeps_the_entry() {
        let area = found(Kind::Dir, 1, 10, Some(7));
        let p = Path::new("/s/attic/x");
        assert_eq!(
            off_the_mount(&area, &found(Kind::Dir, 1, 11, Some(7)), p),
            None
        );
        assert_eq!(
            off_the_mount(&area, &found(Kind::Dir, 2, 11, Some(7)), p),
            Some(Keep::OtherMount { at: p.into() })
        );
        assert_eq!(
            off_the_mount(&area, &found(Kind::Dir, 1, 11, Some(8)), p),
            Some(Keep::OtherMount { at: p.into() })
        );
        assert_eq!(
            off_the_mount(&area, &found(Kind::File, 1, 11, None), p),
            Some(Keep::NoMountId { at: p.into() })
        );
        assert_eq!(
            off_the_mount(
                &found(Kind::Dir, 1, 10, None),
                &found(Kind::Dir, 1, 11, None),
                p
            ),
            Some(Keep::NoMountId { at: p.into() })
        );
    }

    #[test]
    fn only_the_names_ricepilot_gives_are_its_own() {
        for ok in [
            "20260927T101500Z",
            "20260927T101500Z-1",
            "20260927T101500Z-12",
        ] {
            assert!(name_is_ours(Area::Attic, ok), "{ok}");
            assert!(name_is_ours(Area::Verify, ok), "{ok}");
            assert!(name_is_ours(Area::Interrupted, &format!("attic-{ok}")));
            assert!(name_is_ours(Area::Interrupted, &format!("verify-{ok}")));
        }
        assert!(name_is_ours(Area::Attic, "rescue-0003"));
        assert!(name_is_ours(Area::Attic, "rescue-12345"));
        assert!(name_is_ours(Area::Interrupted, "attic-rescue-0003"));
        for bad in [
            "",
            "backup",
            "20260927T101500",
            "20260927t101500z",
            "20260927T101500Z-",
            "20260927T101500Z-x",
            "20260927T101500Zx",
            "rescue-003",
            "rescue-00a3",
            "../20260927T101500Z",
        ] {
            assert!(!name_is_ours(Area::Attic, bad), "{bad:?}");
        }
        assert!(!name_is_ours(Area::Verify, "rescue-0003"));
        // The sandbox's own `-M` on top of a switch id's `-N` (D78): a verify
        // copy only, one level only.
        for ok in ["20260927T101500Z-1-1", "20260927T101500Z-3-12"] {
            assert!(name_is_ours(Area::Verify, ok), "{ok}");
            assert!(name_is_ours(Area::Interrupted, &format!("verify-{ok}")));
            assert!(!name_is_ours(Area::Attic, ok), "{ok}");
            assert!(!name_is_ours(Area::Interrupted, &format!("attic-{ok}")));
        }
        for bad in [
            "20260927T101500Z-1-2-3",
            "20260927T101500Z-1-",
            "20260927T101500Z--1",
            "20260927T101500Z-x-1",
            "20260927T101500Z-1-x",
        ] {
            assert!(!name_is_ours(Area::Verify, bad), "{bad:?}");
        }
        assert!(!name_is_ours(Area::Interrupted, "verify-rescue-0003"));
        assert!(!name_is_ours(Area::Interrupted, "20260927T101500Z"));
        assert!(!name_is_ours(Area::Interrupted, "gc-20260927T101500Z"));
    }

    /// A place, and `rename_to_attic`'s `<name>.N` beside it where a journal
    /// allows it — never a different name, and never `.N` for a rescue attic.
    #[test]
    fn places_match_exactly_or_with_the_attic_suffix() {
        let link = Displaced::Link {
            dest: "/h/.config/hypr".into(),
        };
        let places = vec![(PathBuf::from("h/.config/hypr"), link.clone())];
        let at = |p: &str, suffixed| place_of(Path::new(p), &places, suffixed).is_some();
        assert!(at("h/.config/hypr", false));
        assert!(at("h/.config/hypr.1", true));
        assert!(at("h/.config/hypr.42", true));
        assert!(!at("h/.config/hypr.1", false));
        assert!(!at("h/.config/hypr.", true));
        assert!(!at("h/.config/hypr.x", true));
        assert!(!at("h/.config/hypr1", true));
        assert!(!at("h/.config/hyp", true));
        assert!(!at("h/.config/hypr/inside", true));
        assert!(!at("h/.config", true));
        assert!(!at("x/.config/hypr.1", true));
    }

    #[test]
    fn a_link_target_is_resolved_as_written_and_never_followed() {
        let l = Path::new("/h/.config/hypr");
        assert_eq!(resolve(l, Path::new("/abs/x")), PathBuf::from("/abs/x"));
        assert_eq!(
            resolve(l, Path::new("../.local/state/rp/attic/X/y")),
            PathBuf::from("/h/.local/state/rp/attic/X/y")
        );
        assert_eq!(
            resolve(l, Path::new("./a/../b")),
            PathBuf::from("/h/.config/b")
        );
        assert_eq!(lexical(Path::new("/a/../../b")), PathBuf::from("/b"));
    }

    #[test]
    fn a_listing_that_differs_in_anything_is_not_the_one_shown() {
        let obj = |rel: &str, ino: u64| Obj {
            rel: rel.into(),
            found: found(Kind::Dir, 1, ino, Some(7)),
        };
        let was = vec![obj("", 1), obj("a", 2), obj("a/b", 3)];
        assert_eq!(first_difference(&was, &was.clone()), None);
        let mut moved = was.clone();
        moved[2].found.meta.mtime_ns = 2;
        assert_eq!(
            first_difference(&was, &moved).as_deref(),
            Some("a/b is not what it was")
        );
        let mut replaced = was.clone();
        replaced[0].found.meta.ino = 9;
        assert_eq!(
            first_difference(&was, &replaced).as_deref(),
            Some("the entry itself is not what it was")
        );
        assert_eq!(
            first_difference(&was, &was[..2]).as_deref(),
            Some("a/b is gone")
        );
        let mut grown = was.clone();
        grown.push(obj("a/c", 4));
        assert_eq!(
            first_difference(&was, &grown).as_deref(),
            Some("a/c is new")
        );
    }
}
