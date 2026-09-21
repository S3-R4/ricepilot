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
(`RICEPILOT_FIXTURE_ROOT`, default `target/fixtures/` inside the repo — *not*
`/tmp`, which is a tmpfs with a different `st_dev` and would silently change
what `rename(2)` does).

The only exception is the supervised acceptance run at M5, with the user
present, through ricepilot's own dry-run → `--commit` path.

*Enforced by*: fixture root defaults to the repo; reviewed in CI by eye.

## R2 — No delete primitive outside `src/gc/`, no syscall outside `src/ops/`

Displaced objects are **renamed into an attic**, never removed. The ability to
remove anything lives in one small, auditable directory that never runs
implicitly: `gc` itemises what it would remove and requires the operator to
type the attic directory's name back.

Separately, every filesystem and subprocess call lives in `src/ops/`. This is
what makes the dry-run promise checkable: `plan.rs` *cannot* perform IO, and
the complete set of effects ricepilot can have on a machine is the public
surface of one directory.

*Enforced by*: `scripts/check-no-delete.sh`, `scripts/check-ops-boundary.sh`
(both run in CI, both proven to bite by `tests/guards.rs`), and clippy
`disallowed-methods` in `clippy.toml` for paths a grep would miss.

## R3 — ricepilot never writes into a profile and never runs an installer

It never opens a profile file for writing, never templates or `sed`s content,
never writes through a symlink, never runs `git stash|checkout|clean|reset` on
a user repo, never runs `sudo`, never writes under `/etc`, and never invokes
`caelestia install|update`, `install.fish`, `paru -S` or any other rice
installer. Where an action requires one of these, ricepilot prints the exact
command for a human to run and stops.

*Enforced by*: `src/ops/exec.rs` holds a closed allowlist of subprocesses
(`pacman -Q`, `hyprctl`, `Hyprland --verify-config` on a sandboxed copy,
`uwsm stop`, `git status`); the ops-boundary grep prevents any other module
spawning a process at all.

## R4 — Dry-run by default, pre-flight before mutation, journal before effect

Every mutating command prints its complete plan and exits without touching
anything unless `--commit` is given. Before the first mutation there is a
complete two-phase pre-flight (observe everything, then decide everything) and
a write-ahead journal fsync'd to both the file and its containing directory.

A refusal names the offending path and the rule it violates, exits non-zero
with a stable code (`src/error.rs`), and has **zero** side effects.

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
claims as unverified. This applies to the tool's own output too: `doctor` and
`verify` report what they observed, never what the manifest says should be
true.

## The ownership predicate

A live path is *owned* by ricepilot only if **all three** hold:

1. Opening it with `O_PATH | O_NOFOLLOW` yields `S_IFLNK`.
2. Its `readlinkat` target is lexically inside a registered profile root.
3. A ledger entry matches the path, the target string **and** the `(dev, ino)`.

Anything else — a real directory, a foreign symlink, a link whose ledger entry
disagrees — is *unowned*, and ricepilot refuses to act on it. A documented
layout is never evidence: reality is always `lstat`ed.
