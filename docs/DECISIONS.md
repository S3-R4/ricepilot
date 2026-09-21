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

## D8 — A malformed manifest is a refusal, not a failure

*M1.* `Error` had no variant for "this `profile.toml` does not make sense",
and the nearest fits were wrong in opposite directions: `Io` (exit 4) says
something broke, and `Refused` carries a single offending path when the
problem is often the document as a whole.

`Error::Manifest { profile, detail }` exits `Refused` (2), because that code's
contract — "a pre-flight check declined the operation; nothing was touched" —
is exactly true of a manifest that would not parse. A wrapper script or the
acceptance run can then keep treating 2 as "declined safely" without knowing
whether the decline came from the filesystem or from the document.

A manifest asking for something v1 deliberately does not do (`kind =
file-copy`, `activation = live`) is `NotPossible` (3) instead, so "you asked
for a v1.1 feature" is distinguishable from "you made a typo".

## D9 — A symlinked intermediate path component is refused, not followed

*M1.* `O_PATH|O_NOFOLLOW` protects the *final* component. The components
above it still have to be walked, and the obvious implementation follows
whatever it finds.

`ops::read` instead opens each component with `O_PATH|O_NOFOLLOW` and refuses
if one turns out to be a symlink. If `~/.config` were a link, every fact
observation gathered about `~/.config/hypr` would be a fact about somewhere
else, and the whole ownership predicate would be answering the wrong
question. Refusing is strict — it would reject a legitimately symlinked
`~/.config` — but the failure mode of the alternative is silent and the
failure mode of this is a message naming the component.

Paths containing `.` or `..` are refused for the same reason: `..` cannot be
resolved component-by-component without following whatever the previous
component turned out to be.

## D10 — Shapes 3 and 4 get their own `Refusal` variants

*M1.* `docs/DESIGN.md` §5 refuses a real directory and a real file at a
managed destination, and both could have been reported as
`Refusal::Unowned`. They are `RealDirAtDest` and `RealFileAtDest` instead.

The three are the same decision but not the same message, and the message is
the product here: a real directory means "an installer replaced our link and
re-linking would discard what it wrote", a real file means "v1 does
directories only, this is a v1.1 feature", and a foreign link means "we did
not make this and cannot know what depends on it". Collapsing them would
have made the most common real-world case — caelestia's installer converting
a link back into a directory — read as a generic ownership complaint.

`Refusal::NotObserved` exists for the same totality reason: planning against
a destination phase A did not observe is a bug, but it resolves to a decline
rather than a panic or a silent skip.

## D11 — `Op::CreateLink` is separate from `CreateTempLink` + `Exchange`

*M1.* Shape 5 (absent) has nothing to displace, and `symlinkat` is already
atomic, so staging a temp link and exchanging it would add a rename that can
only fail. Reusing the exchange path "for uniformity" would also mean
`Exchange` had to tolerate a missing destination, which is precisely the
condition it exists to rule out.

The cost is one more variant in a closed set that a reader must check for
delete primitives. The benefit is that `Exchange` keeps a single, strong
precondition.

## D12 — `plan` takes a third argument, and it is still pure

*M1.* Three inputs are global rather than per-destination: the home directory
(only used to expand the `~`-relative denylist), the attic's path and device
(for the `EXDEV` pre-flight), and the missing-package list.

They are passed as a `PlanContext` value rather than read inside `plan`.
*Gathering* them is IO; *using* them is not, so `plan` stays a pure function
of its arguments and the dry-run guarantee is unchanged. Folding them into
`Target` would have duplicated them per row and invited them to disagree.

## D13 — Ownership is an explicit oracle passed into `observe`

*M1.* Two of the three ownership facts come from the ledger, which is M3
work. Rather than have `observe` reach for a state file that does not exist
yet, it takes `Ownership { profile_roots, entries }` as data.

This is not only sequencing: it means the five-shape classification can be
tested against a real filesystem without a state directory, and that the
question "why was this path called foreign?" has an answer that is entirely
in the caller's hands. M3 fills the struct from `state/ledger.toml`; nothing
in `observe` changes.

Until then `ricepilot plan` passes an empty entry list, so no live link can
satisfy fact 3 and every pre-`init` link is correctly reported as foreign.
The command prints a note saying so, but only when a foreign link was
actually observed — a note that contradicts the plan above it is worse than
no note.

## D14 — Ops are emitted in phase order, not destination order

*M1.* The printed plan is the executed plan (`SAFETY.md` R4), so the order
ops appear in is part of what is being promised. `plan` therefore groups them
the way `docs/DESIGN.md` §6 executes them — every staging link, then every
exchange, then every attic move, then the directory fsyncs — rather than
walking each destination to completion in turn.

Grouping per destination would have read more naturally and would have been
wrong: phase B's whole purpose is to be a tight loop of nothing but
exchanges, and a plan that interleaved a `symlinkat` between two of them
would not be that plan.

## D15 — `ops::read::slurp`, because the boundary grep owns the obvious name

*M1.* `scripts/check-ops-boundary.sh` bans the string `read_to_string`
anywhere under `src/` outside `src/ops/` (D3: the greps do not exempt
comments). A helper called `read_to_string` in `ops` would be legal, but
every call site elsewhere would then contain the banned string and fail the
check.

Renaming the helper is cheaper than weakening the pattern. The same reasoning
covers `list_dir` rather than `read_dir`.

## D16 — Locations are environment-overridable so tests can prove R1

*M1.* `RICEPILOT_HOME`, `RICEPILOT_DATA_DIR` and `RICEPILOT_STATE_DIR`
override where ricepilot looks. This is not a convenience feature: R1 says no
test may touch the real `$HOME`, and the only way to *demonstrate* that is
for the test to run the binary with `env_clear()` and a fixture home, so
there is no real `HOME` in the environment to find.

## D17 — The documented manifest example was invalid TOML

*M1.* `docs/DESIGN.md` §3 showed `volatile` and `generated` written below the
`[[path]]` tables. In TOML a bare key under a table header belongs to that
table, so those two keys would have been parsed as fields of the last
`[[path]]` entry, silently producing a profile with no volatile globs.

`#[serde(deny_unknown_fields)]` on both structs turned that into a parse
error the first time the example was used as a test fixture. The example in
§3 is corrected and carries a note about the ordering rule. The strictness is
kept for the same reason it paid off here: a typo in `kind` or `activation`
is a mistake whose consequence is a live symlink pointing somewhere
unintended, and silently ignoring an unknown key is how that happens.

## D18 — The M1 fixture harness may remove things; the crate still may not

*M1.* `tests/common/mod.rs` clears leftovers from a previous run of the same
case, which means it calls removal functions. R2 constrains *the crate* —
the guard scripts scan `src/` — and the harness is not shipped, cannot be
reached from the binary, and only ever operates under `target/fixtures/`.

The alternative, a uniquely-named directory per run, avoids the primitive at
the cost of accumulating fixture trees indefinitely. Recorded here so that a
future reader who greps for the word in `tests/` knows it was deliberate.
