# ricepilot

Switch a Linux desktop between complete rice profiles by atomically
re-pointing directory symlinks.

A *rice* here is a set of dotfile directories — `~/.config/hypr`, `fish`,
`foot`, `btop` and so on — that together make a desktop. A *profile* is one
such set, registered with ricepilot. `ricepilot switch <profile>` re-points the
links in `~/.config` at that profile's directories, all of them or none of
them, and records what it did so it can be undone. The switch is on disk only:
it takes effect at the next login.

The overriding requirement is that ricepilot must never break the system it
runs on and never modify a profile's contents. The rules that follow from that
are in [`docs/SAFETY.md`](docs/SAFETY.md); what ricepilot deliberately refuses
to do is in [`docs/NOT-POSSIBLE.md`](docs/NOT-POSSIBLE.md); how it works is in
[`docs/DESIGN.md`](docs/DESIGN.md); why each non-obvious call went the way it
did is in [`docs/DECISIONS.md`](docs/DECISIONS.md). **If a login is already
broken, go to [`docs/RECOVERY.md`](docs/RECOVERY.md).**

## Status

**Pre-release (0.1.0). The M5 code is done; the acceptance run is not.**
Every command below is implemented and covered by the test suite, which runs
entirely inside a sandbox under `target/fixtures/` and never touches a real
home directory, the live session, `Hyprland`, `hyprctl` or `uwsm`. ricepilot
has **never been run against a real `$HOME`**. The acceptance run — the
whole dry-run → `--commit` path, `init`, `adopt`, `switch`, `rollback`,
`doctor`, `diff`, `gc`, and `recover` after an injected crash, against a
fake shaped like the target rice — is still to come, and will be done in a
disposable sandbox (a container, a VM or a throwaway user), not on a machine
anyone depends on. Until it has, treat ricepilot as untested on real
systems.

It is written for, and its facts were checked on, one machine: CachyOS (Arch)
with Hyprland 0.55 started by uwsm 0.26 under SDDM, `/home` on btrfs, and the
caelestia rice. Other setups may work; nothing else has been tried.

## Build and install

Needs Rust 1.96 or newer (`rust-version` in `Cargo.toml`) and glibc. No
dependency outside `Cargo.lock`.

```sh
cargo build --release --locked
./target/release/ricepilot --help
```

On Arch, [`packaging/arch/PKGBUILD`](packaging/arch/PKGBUILD) builds a package
from what is committed in this repository (not from uncommitted edits), runs
the test suite in `check()`, and installs the binary to `/usr/bin/ricepilot`
and the documentation to `/usr/share/doc/ricepilot/`:

```sh
cd packaging/arch
makepkg              # build and test; writes ricepilot-0.1.0-1-x86_64.pkg.tar.zst here
makepkg --install    # the same, then install it with pacman
```

The PKGBUILD lives in `packaging/arch/` rather than at the root on purpose:
at the root, makepkg's build directory would be the crate's own `src/`
(D65).

Run the tests with `cargo test --locked`. Running them needs no network and
no root. Every fixture lives under `target/fixtures/`, and inside the test
sandbox ricepilot refuses any location outside it and refuses to start
`Hyprland`, `hyprctl` or `uwsm` (D56).

What ricepilot runs as subprocesses, each by absolute path — `/bin/sh`, or
the first of `/usr/bin` and `/usr/local/bin` that has it, never through
`PATH` — with an empty environment plus what the call needs (D52):

| program | when | if it is missing |
|---|---|---|
| `/bin/sh -n` | `switch --commit` and `rollback --commit`, to parse `rescue.sh` before writing it; `doctor` and `--relogin`, to check it | required: a rescue script that could not be checked is never written |
| `pacman -Q <pkg>` | `plan`, `switch`, `rollback` and `doctor`, for a profile that declares `requires` | a profile with `requires` is refused; one without needs no pacman |
| `Hyprland --verify-config` | `plan`, `switch` and `rollback`, when the `~/.config/hypr` they would link has a `hyprland.conf` — run on a stripped scratch copy (D55) | the plan says the config was **not** checked, and goes ahead (`--strict` refuses) |
| `uwsm stop` | `switch --commit --relogin` / `rollback --commit --relogin`, after the checks and a yes | refused (exit 2); log out by hand |

`hyprctl version` and `git status` are on the allowlist too, but no command
runs them today.

## How to use it

```sh
ricepilot doctor                                      # read-only health report
ricepilot init --root ~/.local/share/caelestia        # dry run: what would be registered
ricepilot init --root ~/.local/share/caelestia --commit   # asks about each existing link
ricepilot status                                      # what ricepilot believes it owns
ricepilot plan other                                  # the plan for switching to `other`
ricepilot switch other                                # the same plan: a dry run
ricepilot switch other --commit                       # do it; takes effect at next login
ricepilot rollback --commit                           # back to the previous generation
```

A profile lives at `~/.local/share/ricepilot/profiles/<name>/profile.toml`,
with its directories beside it — or, *by reference*, in another tree its
`root` names, which is how `init` registers a rice where it already is
(`root = "~/.local/share/caelestia"`) without copying it. The manifest format
is in [DESIGN §3](docs/DESIGN.md#3-manifest-schema-profiletoml). `capture`
makes a new profile by copying live directories; `adopt` turns one real
directory in `~/.config` into a link into a profile.

### Commands

| command | what it does | changes anything? |
|---|---|---|
| `init --root <dir> [--name <n>] [--commit]` | registers an existing rice by reference, records the links already pointing into it (one question per link), and takes a baseline copy into `state/baseline/<name>/` | with `--commit`: the profile's manifest, the ledger, the baseline. Nothing in `~/.config` |
| `status` | home, data and state directories; registered profiles; the links ricepilot owns; the current generation | no |
| `doctor` | read-only health report: problems first, then hazards, each with the exact commands for you to run | no — it runs none of them (D57) |
| `list` | registered profiles | no |
| `show <profile>` | one profile's manifest as ricepilot reads it | no |
| `capture <profile> --from <dir>… [--commit]` | copies live directories into a new profile and writes its manifest; activates nothing | with `--commit`: the new profile only |
| `adopt <path> --into <profile> [--commit]` | turns a real directory into a link into the profile: asks, copies it into the profile, verifies the copy by hash, then exchanges the directory for the link. The original goes to the attic | with `--commit` and a yes: that one path, and the profile it is copied into |
| `plan <profile>` | the plan a switch would carry out, including every refusal | no link, ledger or profile; see the verify-config copy below |
| `switch <profile> [--commit] [--strict] [--relogin]` | re-points every link the profile declares, all or none, and records a new generation | with `--commit` |
| `rollback [--commit] [--relogin]` | re-applies the previous generation through the same code path as `switch` | with `--commit` |
| `recover [--commit]` | finishes or undoes an operation that was interrupted, decided by looking at every destination | with `--commit` |
| `rescue` | prints where `rescue.sh` is and how to run it | no |
| `verify <profile>` | compares a profile's tree with the blake3 manifest recorded when ricepilot last switched to (or captured) it; `volatile` paths are left out | no |
| `diff <profile>` | compares a profile with the live filesystem: its links, and its tree against what was recorded (D60) | no |
| `gc [--commit]` | lists everything in the attic and the verify-config copies, and why each could go or is kept; with `--commit`, removes only the entries whose names you type back | with `--commit` and the typed name: the one irreversible command |

`switch --strict` refuses, instead of reporting and going ahead: a profile
whose tree differs from what was recorded (outside `volatile`), a profile
that could not be compared, and a Hyprland config the sandboxed verify-config
did not check (D59). `rollback` is never strict: it is the way back.

### Dry run, then `--commit`

Every command that can change anything prints its complete plan and stops,
unless given `--commit`. The plan printed by a dry run is the same value
`--commit` executes: the planner is a pure function with no IO. A refusal
names the path and the rule, lists every reason rather than the first, exits
non-zero, and changes nothing.

Some commands ask as well, and there is no flag that skips the asking — no
`--yes`, no "all" (D47): `init` asks about each link it would record, `adopt`
about its path, `--relogin` before it logs you out, and `gc` wants each
entry's name typed back. An empty answer, or end of input, is always no.
When stdin is not a
terminal the same question is read as a line, so it can be answered from a
script — but it is still asked.

Every change to a live path is journalled, and the journal fsync'd, before
it is made. A switch, rollback or adopt that is interrupted — a crash, a
power cut — leaves `state/journal/current.toml` behind, and `ricepilot
recover` brings every destination to one side of the switch, never half of
each.

### Exit codes

From `src/error.rs`; scripts can rely on them.

| code | meaning | who returns it |
|---|---|---|
| 0 | done — or nothing to do, or a dry run | every command |
| 2 | refused: a pre-flight check declined, a manifest is invalid, or a question was answered no. Nothing was changed. | every command. clap also exits 2 for a mistyped command line |
| 3 | not possible in v1: a manifest asks for `kind = "file-copy"` or `activation = "live"` | commands that read manifests |
| 4 | failed: an unexpected IO or consistency error. `diff` also returns it when it could not read what it had to compare. | any |
| 5 | locked: another ricepilot holds `$XDG_RUNTIME_DIR/ricepilot.lock` | `init`, `capture`, `adopt`, `switch`, `rollback`, `recover`, `gc` — dry runs included |
| 6 | drift: the command worked, and the answer is "this differs" | `verify`, `diff` |
| 7 | unhealthy: `doctor` found at least one problem (hazards alone do not set it) | `doctor` |

`gc` exits 0 when every question was answered — a name not typed is an
answer — and otherwise with the first failure: 2 when an entry changed while
it was being asked about, or its removal stopped part way; 4 for an IO error
before a removal began. `--relogin` leaves the switch's own code when it does
not log out; `uwsm stop` not installed is 2, and running and failing is 4.

## Where things live

| path | what |
|---|---|
| `~/.local/share/ricepilot/profiles/<name>/` | a profile: `profile.toml`, and its directories unless it is by reference |
| `~/.local/state/ricepilot/ledger.toml` | the links ricepilot owns, each with its target and `(dev, ino)` |
| `~/.local/state/ricepilot/generations/` | `NNNN.toml` per switch or rollback, and `current` |
| `~/.local/state/ricepilot/journal/` | `current.toml` while an operation is in flight; `done-<id>.toml` after |
| `~/.local/state/ricepilot/manifests/<name>.toml` | the blake3 manifest of a profile's tree, recorded at each switch into it and by `capture` |
| `~/.local/state/ricepilot/attic/<id>/` | everything a switch, rollback or adopt displaced, at its original absolute path under that directory |
| `~/.local/state/ricepilot/attic/rescue-NNNN/` | what `rescue.sh` displaced |
| `~/.local/state/ricepilot/verify/<id>/` | verify-config scratch copies (D55) |
| `~/.local/state/ricepilot/gc/` | a removal `gc` began and did not finish (D62) |
| `~/.local/state/ricepilot/baseline/<name>/` | `init`'s baseline copy of a rice |
| `~/.local/state/ricepilot/rescue.sh` | a standalone POSIX `sh` script that restores the previous generation |
| `~/.local/state/ricepilot/.rp-probe-a`, `.rp-probe-b` | the two links the `RENAME_EXCHANGE` probe made (D22) |
| `$XDG_RUNTIME_DIR/ricepilot.lock` | the process lock (`flock`; released when the process exits, however it exits) |

These are under `$HOME` literally: `XDG_DATA_HOME` and `XDG_STATE_HOME` are
not consulted. `RICEPILOT_HOME`, `RICEPILOT_DATA_DIR`, `RICEPILOT_STATE_DIR`
and `RICEPILOT_RUNTIME_DIR` override the four locations; the test suite uses
them, and outside it there is no reason to. With neither
`RICEPILOT_RUNTIME_DIR` nor `XDG_RUNTIME_DIR` set, every command that takes
the lock refuses. Everything ricepilot renames must be on the same filesystem
as the links it manages — on the target machine, `/home` — because `rename(2)`
does not cross filesystems, and a mismatch is refused before anything moves.

## Limits

What ricepilot does not do, or does not know, and what that costs you.

- **A switch takes effect at the next login.** There is no live apply in v1
  (`NOT-POSSIBLE.md#live-apply`). `--relogin` offers the logout; it does not
  make the change live.
- **GTK colours follow only apps started after login.** GTK reads its theme
  when an app starts, and ricepilot manages none of it: caelestia's theme
  engine writes the GTK CSS into `~/.config/gtk-3.0` and `gtk-4.0`, which no
  profile links, and `dconf` is denylisted. An app that is already running
  keeps the colours it started with.
- **ricepilot never writes into a profile, but apps do — see `volatile`.**
  A profile's directories are linked live, so fish rewrites
  `fish_variables`, btop its config on exit, the caelestia shell
  `shell.json`, and on the target machine a `~/.local/bin/hypr-session`
  script creates `~/.config/hypr/sessions` inside whatever profile is
  linked (`doctor` prints a one-line patch for it). Paths declared
  `volatile` in the manifest are left out of every hash and never count as
  drift. Everything else an app writes is reported as drift by `verify`,
  `diff`, `doctor` and `switch` — and refused by `switch --strict`.
- **`rollback` does not undo an `adopt` (D49).** `adopt` moved your original
  directory into the attic. Rolling back re-applies the generation before the
  adopt, which had no link at that path, so the link is moved to the attic
  too and the path is left **empty** — your directory is not put back.
  `doctor` reports this and prints the `mv -T` that returns it;
  [RECOVERY.md](docs/RECOVERY.md#6-put-something-back-from-the-attic-by-hand)
  says how.
- **The verify-config leaves scratch copies under
  `~/.local/state/ricepilot/verify/`.** Every `plan`, `switch` and `rollback`
  into a profile with a `hyprland.conf` — dry runs and declined switches
  included —
  copies the config there with its `exec` lines stripped, for `Hyprland
  --verify-config` to parse (D55). They are kept as the record of what was
  parsed and are never cleaned up automatically; `doctor` reports when they
  pile up, and `gc` removes them, one typed name each. Exit 0 from
  `--verify-config` means the syntax parsed, nothing more; a
  `hyprland.lua` is a program and is never run, so it is never checked
  (`NOT-POSSIBLE.md#verify-lua-config`).
- **`gc` is the one irreversible command, and it keeps what it cannot account
  for (D61).** Nothing else removes anything: displaced links and directories
  go to the attic. `gc` lists every attic entry and verify copy with its
  size and every reason it could go or is kept, then asks for each
  candidate's name to be typed back; a wrong name, an empty line or end of
  input removes nothing. An attic entry is a candidate only when its own
  record accounts for every object in it and nothing ricepilot records points
  into it. A directory `adopt` displaced is kept unless a registered profile
  holds an identical copy of it **and** its path is ricepilot's link again —
  so after an adopt-then-rollback it is always kept. `gc` refuses while an
  operation is in flight, never follows a link, never crosses a mount, and
  removes only what it listed (D62).
- **`--relogin` runs `uwsm stop` only after its checks and a yes (D58).** It
  is offered only after a `switch --commit` or `rollback --commit` that
  completed in the same process, and only when all six checks pass: the new generation is
  current, the journal is retired, `rescue.sh` is the script for the previous
  generation and parses, every link is what the plan said and what the ledger
  recorded, the lock is still held, and the process is inside a session uwsm
  manages. It asks y/N, defaulting to no, then runs the checks again. Outside
  a uwsm session — a TTY, ssh — it declines. It never runs `hyprctl dispatch
  exit` and never signals Hyprland. That `uwsm stop` with this environment
  really ends a session has not been tested; that is the acceptance run's job.
- **v1 manages directory links only.** A manifest path with
  `kind = "file-copy"` or `activation = "live"` exits 3; a destination that
  is a real file is refused. File links that already exist (caelestia's
  `starship.toml`, `codium-flags.conf`) are left alone. `generated` and
  `volatile` are classifications, never activated.
- **The denylist is hard.** A destination at or under any of these is refused
  even when a manifest names it (`src/plan.rs`, `DENYLIST`):
  `~/.config/uwsm`, `~/.config/systemd`, `~/.config/environment.d`,
  `~/.config/autostart`, `~/.config/dconf`, `~/.config/pulse`,
  `~/.config/mimeapps.list`, `~/.config/user-dirs.dirs`,
  `~/.config/user-dirs.locale`, `~/.ssh`, `~/.gnupg`,
  `~/.local/share/keyrings`, `~/.config/google-chrome`, `~/.config/chromium`,
  `~/.config/BraveSoftware`, `~/.config/microsoft-edge`, `~/.mozilla`,
  `~/.config/Electron`, `~/.config/discord`, `~/.config/Code`,
  `~/.config/VSCodium`. caelestia links `~/.config/uwsm`; `init` reports
  that link and does not offer it.
- **Only named paths are managed.** A path is managed because a manifest
  names it, never because ricepilot found it. A link or directory ricepilot
  did not create is refused, not replaced (`NOT-POSSIBLE.md#unowned-path`);
  a link an installer turned back into a directory is refused until you
  decide what to do with what the installer wrote.
- **No packages, no installers, no root.** `requires` is checked with
  `pacman -Q` and the exact `paru -S --needed …` line is printed; ricepilot
  never runs it, never runs `sudo`, never runs a rice installer, and never
  writes under `/etc`.
- **No snapshots.** Rootless btrfs snapshots are not possible; `doctor`
  prints the root-only `snapper -c home create-config /home` for you, and
  never runs it.
- **`rescue.sh` is not quite `rollback`.** It restores only the
  destinations the previous generation recorded: a link that only the
  current profile has is left pointing into it, where `rollback` would move
  it to the attic. And because it works without ricepilot, it leaves
  ricepilot's records out of step — every switch then refuses the restored
  links as ones it did not make, until they are set aside and a
  `rollback --commit` records them again. RECOVERY.md walks through both.
- **Not tested, and why:** that `uwsm stop` ends the session (it is never
  run by a test); the terminal widget of the confirmations (only the
  line-read path is driven by tests); `gc` against a real second mount
  (needs privileges); Lua Hyprland configs (never run, by design).

## When something goes wrong

[`docs/RECOVERY.md`](docs/RECOVERY.md) — written for someone at a broken
login, possibly on a TTY: how to get a shell, what to look at, and the ways
back from least to most invasive. Installed, it is at
`/usr/share/doc/ricepilot/docs/RECOVERY.md`. In short:

```sh
ricepilot doctor                          # what is wrong; changes nothing
ricepilot recover --commit                # only if an operation was interrupted
ricepilot rollback --commit               # back one generation
sh ~/.local/state/ricepilot/rescue.sh     # without ricepilot, from a TTY; read §5 first
```

## License

`MIT OR Apache-2.0`, as declared in `Cargo.toml`. The repository does not
yet include the license texts.
