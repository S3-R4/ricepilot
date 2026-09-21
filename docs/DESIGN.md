# DESIGN

## 1. What ricepilot does

It switches a Linux desktop between complete dotfile profiles by atomically
re-pointing **directory symlinks** in `~/.config`. A switch is an on-disk
relink only; it takes effect at the next login. Live re-application is
explicitly out of scope for v1 (see [NOT-POSSIBLE.md](NOT-POSSIBLE.md)).

It does not install packages, does not edit config content, does not delete
anything, and does not write into a profile. Those are not omissions — they
are the design (see [SAFETY.md](SAFETY.md)).

## 2. On-disk layout

```
~/.local/share/ricepilot/profiles/<name>/
    profile.toml            manifest
    <payload…>              or, for a by-reference profile, nothing:
                            profile.toml sets root = <external dir>

~/.local/state/ricepilot/
    ledger.toml             which live paths ricepilot owns
    generations/NNNN.toml   one per successful switch
    generations/current     pointer to the active generation
    journal/                write-ahead log of an in-flight switch
    manifests/<name>.toml   blake3 manifest of a profile, recorded at switch
    attic/<ts>/             displaced objects, never deleted
    baseline/               cp -a --reflink=auto copy taken at init
    rescue.sh               standalone POSIX sh restore of generation N-1

$XDG_RUNTIME_DIR/ricepilot.lock   flock(LOCK_EX|LOCK_NB), held process-wide
```

A **by-reference** profile (`root = /home/u/.local/share/caelestia`) points at
a directory ricepilot does not own and never writes to — typically the user's
live rice clone. This is how `init` registers an existing rice without copying
7 GB and without a second source of truth.

### Everything renamed must live under `/home`

On the target machine `/`, `/home`, `/tmp` and `/var/tmp` are separate btrfs
subvolumes or tmpfs with **different `st_dev`**, so `rename(2)` between them
fails with `EXDEV`. The attic, the staging temp links, the journal and the
state directory therefore all live under `/home`, and a `st_dev` mismatch
between a destination's parent and the attic is a pre-flight refusal, not a
runtime error.

## 3. Manifest schema (`profile.toml`)

```toml
name = "caelestia"
root = "/home/u/.local/share/caelestia"   # optional: by-reference profile
requires = ["hyprland", "foot", "fish"]   # checked with `pacman -Q`, never installed
hypr_dialect = "conf"                     # "conf" | "lua"; ≥0.55 may use hyprland.lua
volatile = ["**/fish_variables", "shell.json", "**/*.log"]
generated = ["hypr/scheme/current.conf", "btop/themes"]

# Every table-valued key must come after the scalar ones: in TOML a bare key
# written below a [[path]] header belongs to that table, not to the document.
[[path]]
dest       = "~/.config/hypr"
src        = "hypr"                       # relative to the profile root
kind       = "dir-link"                   # dir-link | file-copy | generated | volatile
activation = "relogin"                    # relogin | live | never
```

* `kind` — v1 activates **`dir-link` only**. `file-copy` (copy-deploy with a
  ledger and a three-state divergence check) is v1.1. `generated` and
  `volatile` are *classifications*: they are never activated at all.
* `volatile` — globs excluded from hashing, seeded from the profile's
  `.gitignore` ∪ the discovered runtime writers. Drift here is reported and
  the switch proceeds, unless `--strict`.
* `generated` — paths a theme engine or installer rewrites. These are not
  profile content; they are backed up to the attic on switch and never linked.

### Why `generated` exists

caelestia's theme engine (`caelestia shell -d`) rewrites ~20 files per scheme
change with tempfile + `os.replace`, which **destroys a symlink at that path**.
Some are in-tree (`hypr/scheme/current.conf`, `btop/themes/*`,
`spicetify/…/color.ini`), some out-of-tree (`fuzzel`, `cava`, `htop`, GTK 3/4
CSS, `nvtop`, `qtengine`, `zed`, Discord client themes). Treating any of them
as profile content would make ricepilot fight a daemon and lose. They are
declared `generated` and left alone.

## 4. Modules and the boundary

| module | role | may do IO? |
|---|---|---|
| `observe.rs` | read reality into the five shapes | via `ops::read` only |
| `plan.rs` | **pure** `(Observed, Target) -> Plan` | no |
| `manifest.rs` | parse `profile.toml` | no |
| `ops/read.rs` | `*at()`, `O_PATH\|O_NOFOLLOW`, `statfs` | yes, read-only |
| `ops/mutate.rs` | the closed set of mutators | yes |
| `ops/exec.rs` | closed subprocess allowlist | yes |
| `ops/lock.rs` | `flock(LOCK_EX\|LOCK_NB)` | yes |
| `journal.rs` | WAL, state-driven idempotent replay | via `ops` |
| `ledger.rs`, `generations.rs` | ownership and history | via `ops` |
| `verify.rs` | blake3 manifest | via `ops::read` |
| `rescue.rs` | regenerate `rescue.sh` | via `ops::mutate` |
| `gc/` | **the only delete primitive in the crate** | yes |
| `cli/` | clap surface, wording, confirmations | no |

`plan.rs` being pure is the load-bearing property: the plan printed by
`ricepilot plan` is the same value `switch --commit` executes, so dry-run is a
guarantee rather than a best effort.

## 5. The five shapes and what happens to each

Every destination path is exactly one of five shapes. The table is total —
there is no default case, and no shape falls through to "try it and see".

| # | observed shape | `switch` | `adopt` |
|---|---|---|---|
| 1 | **owned link** (all three ownership facts hold) | exchange with the staged link; old link → attic | already owned → no-op, report |
| 2 | **foreign link** (symlink, ledger disagrees or target outside any profile root) | **refuse**: unowned path | **refuse**: ricepilot did not create this and cannot know what depends on it |
| 3 | **real dir** | **refuse** on `switch` — a real dir at a managed dest means someone (an installer) replaced our link; `doctor` reports it | copy into the profile, verify hashes, then exchange; displaced dir → attic |
| 4 | **real file** | **refuse**: v1 activates directories only | **refuse**: `file-copy` is v1.1 |
| 5 | **absent** | create the link directly (no exchange needed); record in ledger | create the link; nothing to displace |

A dangling symlink is a *foreign link* (shape 2) with `dangling = true`: it is
still a symlink we did not create, so it is still a refusal.

Shape 3 on `switch` deserves emphasis: caelestia's installer converts our
links back into real directories. Silently re-linking would discard whatever
the installer wrote. Refusing and telling the user is the correct outcome.

## 6. The switch algorithm

**Phase A — decide (no mutation):**

1. Acquire the lock (`LOCK_NB`; a second ricepilot exits `Locked`, it does not
   queue behind a half-finished switch).
2. Observe every destination through dirfds and `*at()`.
3. Classify each into one of the five shapes.
4. `plan()` — pure.
5. Pre-flight refusals: unowned path; `st_dev` mismatch with the attic;
   denylisted destination; destination is a mountpoint or nested inside
   another destination; missing `requires`; sandboxed verify-config failure.
6. Print the plan. **Stop here unless `--commit`.**
7. Probe `RENAME_EXCHANGE` (the probe writes two symlinks, so it happens after
   the commit gate and not at startup — D39).
8. Write the journal; fsync the journal file **and** its directory. This is
   *before* the staging links, not after: a staged link created without a
   journal naming it would be invisible to `recover` and would block the next
   switch with `EEXIST` (D38).
9. Create every temp link (`dest.rp-tmp-<n>`, in the destination's own
   directory so the later rename is same-directory and same-`st_dev`).

**Phase B — the tight loop:** nothing but `renameat2(RENAME_EXCHANGE)` calls.
No allocation, no IO decisions, no user interaction. This is the only window
in which the filesystem is inconsistent, and it is as short as it can be made.

**Phase C — settle:** move displaced objects to the attic — including any
destination the new target state no longer includes, which is *retired* into
the attic rather than removed ([DECISIONS.md](DECISIONS.md) D36); fsync the
touched directories; write generation `NNNN` and flip `current`; record the
POST observation; regenerate `rescue.sh`; print the relogin notice with the
exact commands.

`rollback` re-applies generation `NNNN-1` through this same path — it is not a
separate, less-tested code path. `recover` replays the journal by **observing
reality** and deciding per destination whether it is old or new; it never
blindly re-runs recorded steps. Its direction is decided once for the whole
switch and there are only two of them (D23): forward if any destination is
already new or mid-exchange, otherwise the switch is abandoned and the staged
links go to the attic.

### `RENAME_EXCHANGE` and its fallback

`renameat2(RENAME_EXCHANGE)` is probed once against two real symlinks in
ricepilot's own state directory — the cheaper probes are false positives, see
[DECISIONS.md](DECISIONS.md) D22.

Where the kernel or filesystem lacks it, the fallback is three renames inside
the destination's own directory (`b → b.rp-swap`, `a → b`, `b.rp-swap → a`),
which reaches exactly the postcondition `RENAME_EXCHANGE` does so that the ops
following it in the plan are the same either way (D19). It has a window in
which the destination does not exist.

Both paths are tested; the crash-injection harness (abort after step *k*, then
`recover`) covers every *k* in both modes, in-process and again in a helper
that really aborts.

## 7. Failure and recovery ladder

Each rung works when the one above it does not:

1. `ricepilot rollback --commit` — normal case.
2. `ricepilot recover --commit` — after a crash mid-switch.
3. `sh ~/.local/state/ricepilot/rescue.sh` — fully unrolled POSIX sh with
   absolute binary paths, no loops, no variables, no ricepilot binary. `sh -n`
   checked before it is written, written atomically as a **real file** so a
   bad switch cannot take it with them. Runs from a TTY with no D-Bus, no
   hyprctl, no fish, no quickshell.

   It deliberately does **not** `set -e`: each destination is an independent
   `if … else echo FAILED … fi`, so one path it cannot restore does not cost
   the user the ones it can (D37). It has no delete either, so a destination
   the restored generation did not have is displaced into
   `state/attic/rescue-NNNN/`.
4. TTY (F2–F6) → `Hyprland --safe-mode`.
5. Log out to the greeter, whose plain `Hyprland` session entry is immune to
   anything in `$HOME`.

`~/.local/state/ricepilot/attic/` still holds everything that was displaced,
because nothing is ever deleted.

## 8. Hyprland specifics

* Hyprland holds no open fd on its config files, so retargeting
  `~/.config/hypr` is inert until a reload or a new session.
* `hyprctl reload` does **not** reset the runtime submap, and every user bind
  on the target machine lives in submap `global`. Any live reload must be
  followed by `hyprctl dispatch submap reset` plus a bind-reachability probe.
* `Hyprland --verify-config -c <file>` parses **twice** and forks every
  `exec =` line on the second pass. It is only ever run on a scratch copy with
  `exec =`/`execr =` stripped, absolute path variables rewritten to point at
  the scratch copy, and `HYPRLAND_INSTANCE_SIGNATURE` unset. Exit 0 means
  "syntax parsed", nothing more.
* `hyprctl config full-reload` does not exist. Do not add it.
* Dialect-agnostic: ≥0.55 may use `hyprland.lua`.
* Clean logout is `uwsm stop` — never `hyprctl dispatch exit`, never killing
  Hyprland.

## 9. Denylist (v1, hard)

`~/.config/uwsm`, `~/.config/systemd`, `environment.d`, `autostart`, `dconf`,
`pulse`, `mimeapps.list`, `user-dirs.*`, browser and Electron directories,
`~/.ssh`, `~/.gnupg`, keyrings, and editor `settings.json`.

`~/.config` holds 128 entries and ~7.9 GB, of which ~20 are rice. Management
is therefore **allowlist-only**: a path is managed because a manifest names
it, never because it happened to be found.

## 10. CLI surface

Read-only: `status`, `doctor`, `list`, `show`, `plan`, `verify`, `diff`,
`rescue` (prints the script's path).

`verify <profile>` compares the profile tree against the blake3 manifest
recorded the last time ricepilot switched to it: content hash for regular
files, hash of the target *string* for symlinks (never followed), plus mode,
uid, gid, `mtime_ns` and type, with `volatile` globs excluded. It exits `6`
when the profile has changed, so a script can act on the answer without
parsing the report (see [DECISIONS.md](DECISIONS.md) D34).

Mutating, all dry-run by default and requiring `--commit`: `init`, `capture`,
`adopt`, `switch` (`--relogin`, `--strict`), `rollback`, `recover`, `gc`.

* `init` registers the live rice **by reference**, adopts the existing
  directory links only after explicit per-path confirmation, and takes a
  `cp -a --reflink=auto` baseline copy (a copy, never a move) preserving
  modes, symlinks-as-symlinks and times. It reports absolute symlinks found
  inside the tree and proposes mode-600 files as `volatile` for confirmation.
* `adopt` is the riskiest command — it is the only one that turns a real user
  directory into a link. Per path, confirmed, hash-verified before the
  exchange, journalled, displaced directory to the attic.
* `doctor` reports and never fixes: owned links that are no longer links; the
  installer's legacy-migration hazard; writers that write into the profile
  tree; foreign theme daemons running; `/home` unsnapshotted; relogin-scoped
  drift since the last switch. Where a fix needs root or judgement, it prints
  the exact command and stops.
