# DECISIONS

Engineering decisions made by the implementer, with the reasoning, so they are
not silently re-litigated. Decisions that change *what gets touched on the
real machine* are the user's and are not recorded here — they are asked
([SAFETY.md](SAFETY.md) R6).

Product decisions taken before implementation began (name, language, `switch`
semantics, package policy, v1 activation kinds, the denylist) are stated in
[DESIGN.md](DESIGN.md) and are closed.

---

## D1 — `src/ops/` is the sole syscall surface, not merely the sole *mutator*

*M0.* The brief requires a CI grep for `std::fs`/`rustix::fs` outside
`src/ops/`. Taken literally that would also forbid `observe.rs` from doing its
own read-only `lstat`, which it needs.

Rather than carve out an exception, `ops` is split into `read.rs` (read-only:
`*at()`, `O_PATH|O_NOFOLLOW`, `statfs`) and `mutate.rs` (the closed set of
mutators), and `observe.rs` calls into `ops::read`.

The exception would have been the beginning of a slope; the split costs one
indirection and keeps the grep a single unambiguous rule. It also gives the
read side its own auditable surface, which matters because following a symlink
by accident is as dangerous here as writing one.

## D2 — Subprocess invocation lives inside the `ops` boundary too

*M0.* R3 forbids a long list of subprocesses (`sudo`, installers, `git
stash`, …). Enforcing that by caller discipline across five modules is
exactly the kind of rule that erodes.

`src/ops/exec.rs` holds a closed `Allowed` enum — `pacman -Q`, `hyprctl`,
`Hyprland --verify-config`, `uwsm stop`, `git status` — and the ops-boundary
grep forbids `std::process::Command` everywhere else, so no other module can
spawn a process at all. Adding a subprocess becomes a visible, reviewable edit
to one file.

## D3 — The guard scripts do not exempt comments

*M0.* Stripping comments before grepping would let a violation hide behind a
trailing `//`, and the scripts are the cheapest, most load-bearing check in
CI. They are textual and deliberately blunt: if `remove_dir_all` appears
anywhere under `src/` outside `src/gc/`, including in prose, a human should
look at it.

The cost is that documentation must phrase things carefully. That is a cost
worth paying for a check with no false negatives.

## D4 — The M0 gate is a test, not a demonstration

*M0.* The brief's gate is "CI goes red on a planted `remove_dir_all`". Doing
that once by hand proves the check worked that day.

`tests/guards.rs` instead builds fixture trees containing each forbidden
construct and asserts the scripts reject every one, plus asserts they *accept*
the same construct inside `src/gc/` and `src/ops/`. If someone later weakens a
pattern to silence a false positive, the test goes red.

## D5 — Fixtures live under `target/fixtures/`, never `/tmp`

*M0.* `/tmp` is a tmpfs with a different `st_dev` from `/home`. Since the
whole design turns on `rename(2)` succeeding within one device and failing
across devices, testing on a filesystem with different semantics than the
target would hide exactly the bugs that matter. The root is overridable with
`RICEPILOT_FIXTURE_ROOT` for anyone whose repo is not on `/home`.

## D6 — `clippy.toml` `disallowed-methods` alongside the greps, not instead

*M0.* The greps are textual and catch prose; clippy resolves paths and catches
a delete reached through a re-export or alias. Neither subsumes the other, and
both are cheap. clippy additionally bans the symlink-following convenience
methods (`Path::exists`, `is_dir`, `canonicalize`) that would otherwise be the
natural thing to reach for and would quietly defeat `O_NOFOLLOW`.

## D7 — Stable numeric exit codes from the start

*M0.* `Refused` (2), `NotPossible` (3), `Failed` (4), `Locked` (5) are
distinguished in `src/error.rs` before any of them can be raised. The
acceptance run and any wrapper script need to tell "declined safely" apart
from "broke halfway", and retrofitting that distinction after the call sites
exist means auditing every one.
