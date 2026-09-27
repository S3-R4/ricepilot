//! A blake3 manifest over a profile: a content hash for regular files, a hash
//! of the *target string* for symlinks (never followed), plus mode, uid, gid,
//! `mtime_ns` and the file type. Volatile globs are excluded.
//!
//! What this answers is "has the profile changed since ricepilot switched to
//! it" — and it answers it by reading the tree, never by consulting the
//! manifest that describes it (`SAFETY.md` R7).
//!
//! A symlink's target is hashed as a string rather than followed for the same
//! reason every open in [`crate::ops::read`] is `O_NOFOLLOW`: following it
//! would make the answer a statement about somewhere else, and an absolute
//! in-tree symlink (caelestia has them) would silently drag a second tree into
//! the hash.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ops::look::{Live, Look};
use crate::ops::{mutate, read};
use crate::{Error, Result};

/// What one recorded path is. Serialised as a word rather than a mode bit so
/// the file can be read by a human on a machine that will not boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    File,
    Symlink,
    Dir,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::File => "file",
            EntryKind::Symlink => "symlink",
            EntryKind::Dir => "directory",
        }
    }
}

/// One path in the tree, as a single `fstatat` plus (for files and symlinks) a
/// hash saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// Relative to the profile root, with `/` separators.
    pub path: String,
    pub kind: EntryKind,
    /// blake3 of the content for a file, of the raw target string for a
    /// symlink. A directory has no hash: its content *is* its entries, and
    /// they each have a row of their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime_ns: i64,
}

/// The whole recorded manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeManifest {
    pub profile: String,
    pub root: PathBuf,
    /// When it was taken, in the same timestamp form as the journal and the
    /// attic, so the three can be lined up by eye.
    pub created: String,
    /// The globs that were excluded when it was taken. Recorded because a
    /// comparison against a manifest built with a different exclusion set is
    /// not a comparison at all, and because "why is this path not listed" is
    /// otherwise unanswerable from the file alone.
    #[serde(default)]
    pub volatile: Vec<String>,
    #[serde(default, rename = "entry")]
    pub entries: Vec<FileEntry>,
}

/// How one path differs between the recorded manifest and the tree as it
/// stands now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    Added {
        path: String,
        kind: EntryKind,
    },
    Removed {
        path: String,
        kind: EntryKind,
    },
    KindChanged {
        path: String,
        was: EntryKind,
        now: EntryKind,
    },
    ContentChanged {
        path: String,
        kind: EntryKind,
    },
    MetadataChanged {
        path: String,
        detail: String,
    },
    /// The mtime moved and nothing else did. Reported separately because a
    /// file whose content is byte-identical has not drifted in any sense that
    /// affects a switch, and burying that among real changes would train the
    /// reader to skim the list.
    Touched {
        path: String,
    },
}

impl Difference {
    pub fn path(&self) -> &str {
        match self {
            Difference::Added { path, .. }
            | Difference::Removed { path, .. }
            | Difference::KindChanged { path, .. }
            | Difference::ContentChanged { path, .. }
            | Difference::MetadataChanged { path, .. }
            | Difference::Touched { path } => path,
        }
    }

    /// Whether this is drift in the sense that matters: the profile's content
    /// is not what it was. A `Touched` path is not.
    pub fn is_substantive(&self) -> bool {
        !matches!(self, Difference::Touched { .. })
    }
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Difference::Added { path, kind } => {
                write!(f, "added        {path} ({})", kind.as_str())
            }
            Difference::Removed { path, kind } => {
                write!(f, "gone         {path} (was a {})", kind.as_str())
            }
            Difference::KindChanged { path, was, now } => write!(
                f,
                "kind         {path} (was a {}, is now a {})",
                was.as_str(),
                now.as_str()
            ),
            Difference::ContentChanged { path, kind } => match kind {
                EntryKind::Symlink => write!(f, "retargeted   {path}"),
                _ => write!(f, "changed      {path}"),
            },
            Difference::MetadataChanged { path, detail } => {
                write!(f, "metadata     {path} ({detail})")
            }
            Difference::Touched { path } => write!(f, "touched      {path} (content identical)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Globs
// ---------------------------------------------------------------------------

/// Whether `rel` — a `/`-separated path relative to the profile root — is
/// excluded by `pattern`.
///
/// The supported syntax is the subset the manifest actually uses: `*` within
/// one segment, `?` for one character within one segment, and `**` for any
/// number of segments including none. A pattern that matches a *directory*
/// excludes everything beneath it, which is what makes `generated = ["btop/
/// themes"]` mean what a reader expects.
///
/// Written out rather than pulled in as a dependency: see D33.
pub fn glob_matches(pattern: &str, rel: &str) -> bool {
    let pat: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let path: Vec<&str> = rel.trim_matches('/').split('/').collect();
    // A pattern matching an ancestor excludes the whole subtree beneath it.
    (1..=path.len()).any(|n| match_segments(&pat, &path[..n]))
}

/// Whether any of `patterns` excludes `rel`.
pub fn is_volatile(patterns: &[String], rel: &str) -> bool {
    patterns.iter().any(|p| glob_matches(p, rel))
}

fn match_segments(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => {
            // Zero segments, or one more consumed and try again.
            match_segments(rest, path) || (!path.is_empty() && match_segments(pat, &path[1..]))
        }
        Some((head, rest)) => match path.split_first() {
            Some((seg, tail)) if match_one(head, seg) => match_segments(rest, tail),
            _ => false,
        },
    }
}

/// `*` and `?` within a single segment. Neither ever matches a `/`, because
/// segments were split on it before we got here.
fn match_one(pat: &str, seg: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = seg.chars().collect();
    // Iterative backtracking rather than recursion: a pathological pattern
    // like `*a*a*a*` should cost time, not stack.
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            resume = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            resume += 1;
            si = resume;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

// ---------------------------------------------------------------------------
// Building and comparing
// ---------------------------------------------------------------------------

fn hex(hash: &blake3::Hash) -> String {
    hash.to_hex().to_string()
}

fn hash_file(look: &dyn Look, path: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    look.read_into(path, &mut |chunk| {
        hasher.update(chunk);
    })?;
    Ok(hex(&hasher.finalize()))
}

/// A symlink is hashed by its **target string**, byte for byte, exactly as
/// `readlinkat` returned it. Following it would make the hash a statement
/// about a different tree.
fn hash_link(look: &dyn Look, path: &Path) -> Result<String> {
    let target = look.readlink(path)?;
    Ok(hex(&blake3::hash(target.as_os_str().as_encoded_bytes())))
}

/// Walk `root` and record every path not excluded by `volatile`.
///
/// Read-only, and it never follows a symlink: a link to a directory is one
/// row, not a subtree.
pub fn build(
    profile: &str,
    root: &Path,
    volatile: &[String],
    created: impl Into<String>,
) -> Result<TreeManifest> {
    build_via(&Live, profile, root, volatile, created)
}

/// [`build`], reading through `look`. The one walk: `doctor` and `diff`, which
/// may only read (D57, D60), use this rather than a copy of it, so the tree
/// they compare is built exactly the way the recorded one was.
pub fn build_via(
    look: &dyn Look,
    profile: &str,
    root: &Path,
    volatile: &[String],
    created: impl Into<String>,
) -> Result<TreeManifest> {
    build_within_via(look, profile, root, Path::new(""), volatile, created).map(|(m, _)| m)
}

/// [`build_via`] for a tree that is `anchor` inside a profile root, and what
/// `volatile` left out of it.
///
/// `volatile` globs are written relative to the profile root, so a directory
/// walked on its own — `diff` comparing a real directory at a destination
/// with the profile's `src` for it — must have each path matched as the path
/// it has in the profile (`foot/noise.log`), while the manifest records it as
/// the path it has in the tree walked (`noise.log`). The anchor is the empty
/// path for the profile root itself.
///
/// The second value lists every path the walk skipped because a glob matched
/// it, relative to `root` and sorted: the top of each excluded subtree, since
/// the walk does not go into one. `diff` prints them, so a reader can see what
/// was left out rather than infer it from a short list (D60).
pub fn build_within_via(
    look: &dyn Look,
    profile: &str,
    root: &Path,
    anchor: &Path,
    volatile: &[String],
    created: impl Into<String>,
) -> Result<(TreeManifest, Vec<String>)> {
    let mut entries = Vec::new();
    let mut excluded = Vec::new();
    let at = Walk {
        look,
        root,
        anchor,
        volatile,
    };
    at.walk(Path::new(""), &mut entries, &mut excluded)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    excluded.sort();
    Ok((
        TreeManifest {
            profile: profile.to_string(),
            root: root.to_path_buf(),
            created: created.into(),
            volatile: volatile.to_vec(),
            entries,
        },
        excluded,
    ))
}

/// What one walk needs at every level.
struct Walk<'a> {
    look: &'a dyn Look,
    root: &'a Path,
    anchor: &'a Path,
    volatile: &'a [String],
}

impl Walk<'_> {
    fn walk(&self, rel: &Path, out: &mut Vec<FileEntry>, excluded: &mut Vec<String>) -> Result<()> {
        let (look, root, volatile) = (self.look, self.root, self.volatile);
        let here = if rel.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(rel)
        };
        for name in look.list_dir(&here)? {
            let child_rel = rel.join(&name);
            let rel_str = child_rel.to_string_lossy().into_owned();
            let in_profile = self.anchor.join(&child_rel);
            if is_volatile(volatile, &in_profile.to_string_lossy()) {
                excluded.push(rel_str);
                continue;
            }
            let path = root.join(&child_rel);
            let meta = match look.lstat(&path)? {
                Some(m) => m,
                // Something went away between the listing and the stat.
                // Recording nothing for it is the honest answer: it is not
                // there now.
                None => continue,
            };
            let (kind, hash) = match meta.kind {
                read::Kind::File => (EntryKind::File, Some(hash_file(look, &path)?)),
                read::Kind::Symlink => (EntryKind::Symlink, Some(hash_link(look, &path)?)),
                read::Kind::Dir => (EntryKind::Dir, None),
                // A socket or a fifo in a config tree is recorded as present
                // with no hash rather than skipped: "there is something here I
                // cannot hash" is a fact worth keeping.
                read::Kind::Other => (EntryKind::File, None),
            };
            out.push(FileEntry {
                path: rel_str,
                kind,
                hash,
                mode: meta.mode,
                uid: meta.uid,
                gid: meta.gid,
                mtime_ns: meta.mtime_ns,
            });
            if meta.kind == read::Kind::Dir {
                self.walk(&child_rel, out, excluded)?;
            }
        }
        Ok(())
    }
}

/// Compare a recorded manifest against one taken now. Pure.
pub fn compare(recorded: &TreeManifest, now: &TreeManifest) -> Vec<Difference> {
    let mut out = Vec::new();
    for was in &recorded.entries {
        match now.entries.iter().find(|e| e.path == was.path) {
            None => out.push(Difference::Removed {
                path: was.path.clone(),
                kind: was.kind,
            }),
            Some(is) => out.extend(compare_one(was, is)),
        }
    }
    for is in &now.entries {
        if !recorded.entries.iter().any(|e| e.path == is.path) {
            out.push(Difference::Added {
                path: is.path.clone(),
                kind: is.kind,
            });
        }
    }
    out.sort_by(|a, b| a.path().cmp(b.path()));
    out
}

fn compare_one(was: &FileEntry, is: &FileEntry) -> Vec<Difference> {
    if was.kind != is.kind {
        return vec![Difference::KindChanged {
            path: was.path.clone(),
            was: was.kind,
            now: is.kind,
        }];
    }
    let mut out = Vec::new();
    if was.hash != is.hash {
        out.push(Difference::ContentChanged {
            path: was.path.clone(),
            kind: was.kind,
        });
    }
    let mut meta = Vec::new();
    if was.mode != is.mode {
        meta.push(format!("mode {:04o} -> {:04o}", was.mode, is.mode));
    }
    if was.uid != is.uid {
        meta.push(format!("uid {} -> {}", was.uid, is.uid));
    }
    if was.gid != is.gid {
        meta.push(format!("gid {} -> {}", was.gid, is.gid));
    }
    if !meta.is_empty() {
        out.push(Difference::MetadataChanged {
            path: was.path.clone(),
            detail: meta.join(", "),
        });
    }
    // A directory's mtime moves whenever one of its entries is added,
    // removed or replaced — so comparing it would report a shadow of every
    // change already reported against the entry itself, once per ancestor. It
    // is recorded (it is evidence) and not compared (it is derivative).
    if out.is_empty() && was.kind != EntryKind::Dir && was.mtime_ns != is.mtime_ns {
        out.push(Difference::Touched {
            path: was.path.clone(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// The recorded file
// ---------------------------------------------------------------------------

/// Where a profile's recorded manifest lives.
pub fn manifest_path(state: &Path, profile: &str) -> PathBuf {
    state.join("manifests").join(format!("{profile}.toml"))
}

pub fn save(path: &Path, m: &TreeManifest) -> Result<()> {
    let text = toml::to_string_pretty(m).map_err(|e| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!("could not serialise the tree manifest: {e}"),
    })?;
    mutate::write_atomic(path, text.as_bytes())
}

/// `Ok(None)` when nothing has been recorded for this profile yet — which is
/// every profile until a switch records one, and is a thing to say plainly
/// rather than a comparison against an empty manifest that would report the
/// whole tree as added.
pub fn load(path: &Path) -> Result<Option<TreeManifest>> {
    load_via(&Live, path)
}

/// [`load`], reading through `look`.
pub fn load_via(look: &dyn Look, path: &Path) -> Result<Option<TreeManifest>> {
    if look.lstat_or_absent(path)?.is_none() {
        return Ok(None);
    }
    let text = look.slurp(path)?;
    parse(&text, path).map(Some)
}

/// A profile's tree against the manifest ricepilot recorded for it, or why
/// the two could not be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgainstRecord {
    /// Nothing has been recorded for the profile yet — every profile until
    /// ricepilot captures it or switches to it.
    Unrecorded,
    /// The recorded manifest, or the tree, could not be read. `why` says
    /// which, and what went wrong.
    Unreadable { why: String },
    /// Compared: [`compare`]'s answer, against `recorded`, and what the
    /// profile's `volatile` globs left out of the walk.
    Compared {
        recorded: TreeManifest,
        diffs: Vec<Difference>,
        excluded: Vec<String>,
    },
}

/// The tree at `root` against `state/manifests/<profile>.toml`, `volatile`
/// excluded: the comparison a `switch` reports before it links a profile
/// (D59) and `diff` reports on demand (D60), made once, here, reading only
/// through `look`.
///
/// Nothing that goes wrong is an `Err`: a record that is missing, does not
/// parse or cannot be read, and a tree that cannot be walked, are each an
/// answer — the caller decides whether that answer refuses anything.
pub fn against_record_via(
    look: &dyn Look,
    state: &Path,
    profile: &str,
    root: &Path,
    volatile: &[String],
) -> AgainstRecord {
    let manifest = manifest_path(state, profile);
    let recorded = match load_via(look, &manifest) {
        Ok(Some(m)) => m,
        Ok(None) => return AgainstRecord::Unrecorded,
        // `parse` already says what it could not do.
        Err(Error::Refused { why, .. }) => return AgainstRecord::Unreadable { why },
        Err(e) => {
            return AgainstRecord::Unreadable {
                why: format!("the recorded manifest could not be read: {e}"),
            }
        }
    };
    let walked = build_within_via(
        look,
        profile,
        root,
        Path::new(""),
        volatile,
        recorded.created.clone(),
    );
    let (now, excluded) = match walked {
        Ok(w) => w,
        Err(Error::Refused { why, path, .. }) => {
            return AgainstRecord::Unreadable {
                why: format!("its tree could not be read: {} {why}", path.display()),
            }
        }
        Err(e) => {
            return AgainstRecord::Unreadable {
                why: format!("its tree could not be read: {e}"),
            }
        }
    };
    let diffs = compare(&recorded, &now);
    AgainstRecord::Compared {
        recorded,
        diffs,
        excluded,
    }
}

/// The recorded manifest from its text. Pure; `path` only names it in the
/// refusal.
pub fn parse(text: &str, path: &Path) -> Result<TreeManifest> {
    toml::from_str(text).map_err(|e| Error::Refused {
        rule: "R4",
        path: path.to_path_buf(),
        why: format!("the recorded manifest does not parse: {}", e.message()),
    })
}
