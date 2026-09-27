# SAFETY — the rules ricepilot is built around

ricepilot re-points symlinks in a live desktop's config directory. The failure
mode is not "a bug"; it is "the user cannot log in". Every rule below exists
because a plausible mistake would have that consequence, and each one is
enforced by something mechanical — a CI check, a type, or a required flag —
rather than by developer discipline.

Rules are referenced by identifier from error messages and code comments.

## R1 — Development never touches the real `$HOME`

No test, build step, script or development command reads, writes, links, moves
or deletes anything under `$HOME/.config`, `$HOME/.local` or the user's rice
clone. Tests operate on throwaway trees under a configurable fixture root
(`target/fixtures/` inside the repo; `RICEPILOT_FIXTURE_ROOT` may pick a
directory below it and nothing else — *not*
`/tmp`, which is a tmpfs with a different `st_dev` and would silently change
what `rename(2)` does).

The only exception is the supervised acceptance run at M5, with the user
present, through ricepilot's own dry-run → `--commit` path.

*Enforced by*: the test sandbox (D56). Every test process and every process
it starts carries `RICEPILOT_SANDBOX=<repo>/target/fixtures`; inside it
ricepilot refuses any location not given explicitly and below that root, and
refuses to start `Hyprland`, `hyprctl` or `uwsm` (only
`RICEPILOT_LIVE_TESTS=1` lets a test run the first two). The harness panics on
a fixture outside the root, and `tests/sandbox.rs` fails if a fixture path or
variable escapes it or if a test starts a process by any route but the
harness's.

## R2 — No delete primitive outside `src/gc/`, no syscall outside `src/ops/`

Displaced objects are **renamed into an attic**, never removed. The ability to
remove anything lives in one small, auditable directory that never runs
implicitly: `gc` itemises what it would remove and why it keeps the rest,
and requires the operator to type each entry's name back. The one call that
removes is in one file, `src/gc/remove.rs`, and acts only through directory
descriptors opened `O_NOFOLLOW`, only on what was listed, never across a
mount ([DECISIONS.md](DECISIONS.md) D61, D62).

Separately, every filesystem and subprocess call lives in `src/ops/`. This is
what makes the dry-run promise checkable: `plan.rs` *cannot* perform IO, and
the complete set of effects ricepilot can have on a machine is the public
surface of one directory.

*Enforced by*: `scripts/check-no-delete.sh`, `scripts/check-ops-boundary.sh`
(both run in CI, both proven to bite by `tests/guards.rs`), and clippy
`disallowed-methods` in `clippy.toml` for paths a grep would miss. The two
rules meet in exactly one file: `src/gc/remove.rs` may spell
`rustix::fs::unlinkat` and its flags and nothing else outside `src/ops/`, and
holds the only `allow` of the clippy lint that disallows it everywhere else
([DECISIONS.md](DECISIONS.md) D63).

## R3 — ricepilot never writes into a profile and never runs an installer

It never opens a profile file for writing, never templates or `sed`s content,
never writes through a symlink, never runs `git stash|checkout|clean|reset` on
a user repo, never runs `sudo`, never writes under `/etc`, and never invokes
`caelestia install|update`, `install.fish`, `paru -S` or any other rice
installer. Where an action requires one of these, ricepilot prints the exact
command for a human to run and stops.

*Enforced by*: `src/ops/exec.rs` holds a closed allowlist of subprocesses
(`sh -n`, `pacman -Q`, `hyprctl version`, `Hyprland --verify-config` on a
sandboxed copy, `uwsm stop` — only after `--relogin`'s pre-flight and a yes,
[DECISIONS.md](DECISIONS.md) D58 — `git status`); callers pass a typed call, never
an argument vector, and every child is started by absolute path with an
empty environment ([DECISIONS.md](DECISIONS.md) D52). The ops-boundary grep
prevents any other module spawning a process at all.

## R4 — Dry-run by default, pre-flight before mutation, journal before effect

Every mutating command prints its complete plan and exits without touching
anything unless `--commit` is given. Before the first mutation there is a
complete two-phase pre-flight (observe everything, then decide everything) and
a write-ahead journal fsync'd to both the file and its containing directory.

A refusal names the offending path and the rule it violates, exits non-zero
with a stable code (`src/error.rs`), and has **zero** side effects.

One thing is written before the commit gate, and it is written in ricepilot's
own state directory, never a live path or a profile: when a switch ships a
Hyprland config, the pre-flight's sandboxed `Hyprland --verify-config` needs
a stripped copy of it on disk, under `state/verify/<id>/`. A dry run and a
refusal can leave that copy behind; it is kept as the record of what was
parsed ([DECISIONS.md](DECISIONS.md) D55).

*Enforced by*: `--commit` is a required flag on every mutating subcommand;
`plan.rs` is pure so the printed plan and the executed plan are the same
value; every refusal message is snapshot-tested.

## R5 — "Not possible" is a first-class outcome

When a layer cannot be switched safely, ricepilot declines and explains. It
never half-applies. `Plan` has no partial variant: it is `NoOp`, `Apply` with
every op, or `Decline` with every reason. A crash mid-apply resolves — via
`recover`, which observes reality rather than replaying steps blindly — to
fully old or fully new, never mixed.

Declined capabilities are catalogued in [`NOT-POSSIBLE.md`](NOT-POSSIBLE.md).

## R6 — Ask before anything that changes what gets touched on the real machine

Decisions that change which real paths ricepilot would act on are the user's.
Routine engineering decisions are made by the implementer and recorded in
[`DECISIONS.md`](DECISIONS.md).

## R7 — Report truthfully

Failing tests are reported as failing, skipped steps as skipped, unverified
claims as unverified. This applies to the tool's own output too: `doctor`,
`verify` and `diff` report what they observed, never what the manifest says
should be true, and say what they did not compare.

## The ownership predicate

A live path is *owned* by ricepilot only if **all three** hold:

1. Opening it with `O_PATH | O_NOFOLLOW` yields `S_IFLNK`.
2. Its `readlinkat` target is lexically inside a registered profile root.
3. A ledger entry matches the path, the target string **and** the `(dev, ino)`.

Anything else — a real directory, a foreign symlink, a link whose ledger entry
disagrees — is *unowned*, and ricepilot refuses to act on it. A documented
layout is never evidence: reality is always `lstat`ed.
