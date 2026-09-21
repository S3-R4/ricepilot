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

## D19 — The `RENAME_EXCHANGE` fallback reaches the atomic path's postcondition

*M2.* The brief describes the fallback as "rename-to-attic-then-rename". That
leaves the old link in the attic and the new one at the destination — which is
*a* correct end state, but not the same one `renameat2(RENAME_EXCHANGE)`
produces. `RENAME_EXCHANGE` leaves the old link at the staged name, and the
ops that follow it in the plan (`RenameToAttic { from: staged, … }`) are
written against that.

Two postconditions means the printed plan and the executed plan differ
depending on what the kernel supports, and R4's whole promise is that they are
one value.

The fallback is therefore three renames, all inside the destination's own
directory:

```
b -> b.rp-swap      frees b
a -> b              the window: nothing is at a
b.rp-swap -> a
```

Same postcondition, same subsequent ops, no cross-directory rename that could
meet an `EXDEV` the pre-flight did not predict. The cost is a window in which
the destination does not exist, which is what `recover` is for, and which the
crash-injection harness enumerates for every step in both modes.

## D20 — The M2 gate's rollback property is proved at the ops level

*M2.* The brief's M2 gate asks that "apply-then-rollback restores byte-exact
link topology and leaves every pre-existing target's inode/mtime unchanged",
but `rollback` is an M3 command. Building it early to satisfy an M2 gate would
mean shipping the riskiest command in the tool before generations exist to
give it something to roll back *to*.

The property is proved where it actually lives: apply a plan, plan its
inverse, apply that, then compare the live link targets against what they were
and walk both profile trees comparing every `(dev, ino, mtime_ns)`. Both
exchange modes. What is left for M3 is the `rollback` command's own concerns —
which generation, and what to tell the user — not the atomicity property.

## D21 — A created link has no exact inverse

*M2.* Shape 5 (absent destination) is switched with a single `symlinkat`
(D11). Undoing that means the destination should be absent again, and making
something absent is a removal, which exists nowhere outside `src/gc/` (R2).

The nearest honest thing is to displace the created link into the attic,
leaving the destination absent without anything having been deleted. That is
`rollback`'s decision to make and to word, so it is M3's. The M2 round-trip
property is therefore stated over destinations that were exchanged, where an
exact inverse does exist, and this is recorded rather than glossed.

## D22 — The `RENAME_EXCHANGE` probe uses two real links, and keeps them

*M2.* The two cheap probes are both false positives. Probing with names that
do not exist answers `ENOENT` from path resolution before the filesystem ever
sees the flag; probing a name against itself is short-circuited by the VFS
before dispatch. Either reports support on a filesystem that has none, and the
discovery would happen during phase B — the one moment in the design with no
margin.

So the probe creates `.rp-probe-a` and `.rp-probe-b` in ricepilot's own state
directory and really exchanges them. They are reused on subsequent runs rather
than cleaned up, because cleaning up is a removal (R2) and two symlinks are a
small price for an honest answer.

`exchange` takes the mode as an argument rather than consulting a global, so
the fallback is testable on a kernel that supports `renameat2`. A gate that
requires both paths covered is not met by "whichever one CI happened to take".

## D23 — Recovery is driven by state, and its direction is decided once

*M2.* The journal records the before-and-after of each destination, not a list
of steps. `recover` reads each destination's live link target, plus the two
sibling names the fallback can park a link at, and decides from what is there.

A step log is the obvious design and the wrong one: it invites re-running a
rename whose effect is already present against a filesystem that has moved on.
State-driven replay is idempotent for free — a second pass finds every
destination where the first drove it and emits nothing but the fsyncs, which
is asserted directly.

Direction is decided once for the whole switch, and there are only two. If any
destination is already new — or mid-exchange, which means one has begun —
recovery goes forward, because going back would undo something already in
effect using the same window that just failed. If none is, no exchange
happened, and the switch is abandoned. R5 forbids the third outcome, so the
tests assert its absence rather than trusting the code to avoid it.

A slot holding anything else is a refusal naming the path: something re-pointed
a managed destination while ricepilot was down, and nothing in the journal says
what.

## D24 — A destination whose two states look identical is refused at journal time

*M2.* State-driven recovery answers "which side is this on?" by comparing the
live link target against the recorded old and new ones. If those two were the
same string, no reading of the filesystem could tell the sides apart and
"fully old or fully new" would stop being a checkable claim.

The planner never emits such an entry — shape 1 with a matching target produces
no op at all — but `Journal::from_plan` refuses it anyway. The property that
recovery depends on is checked by recovery's own input, not assumed from a
neighbouring module's behaviour.

## D25 — A finished journal is renamed, not removed

*M2.* `state/journal/current.toml` becomes `done-<id>.toml`. R2 has no
exception for ricepilot's own bookkeeping, and the sequence of switches a
machine has been through is the first thing anyone wants when it did not come
back up.

Retiring it is also deliberately *not* one of the recovery actions, and happens
only after every action has succeeded. While the journal is in place the
machine can be recovered again; a recovery that failed half way must not have
taken that away as its first act.

## D26 — The crash helper is an example, not a `[[bin]]`

*M2.* The gate requires aborting after step *k* for every *k*. In-process
injection covers every intermediate state, but it cannot prove the journal
reached the disk before the state it describes existed — the process asserting
that is the process that wrote it.

`examples/crash_switch` closes the gap by calling `std::process::abort()`
(not `panic!`, which unwinds, runs destructors and flushes). It lives in
`examples/` because a helper whose entire purpose is to abort a switch half way
through belongs neither inside the boundary the guard scripts protect nor in
anything `cargo install` would put on a machine. The cost is that `cargo test`
neither builds it nor exports a `CARGO_BIN_EXE_*` for it, so the test builds it
on demand through the `cargo` that invoked it.

It reuses `tests/common/` through `#[path]` so the scenario the helper crashes
and the scenario the test inspects are the same code.

## D27 — `recover` takes the lock for its dry run too

*M2.* A dry run has no effects on anything ricepilot manages, so the lock looks
unnecessary. It is not: reading a half-finished switch while another ricepilot
is in the middle of finishing it produces a report about a machine that has
stopped existing by the time it is printed. A dry run whose answer is stale is
worse than one that declines, because its whole purpose is to be the thing the
user decides on.

The observable cost is that `recover` creates its lock file in
`$XDG_RUNTIME_DIR` even when it changes nothing.

## D28 — With no runtime directory, there is no lock location to invent

*M2.* `Paths::lock_path()` refuses when neither `RICEPILOT_RUNTIME_DIR` nor
`XDG_RUNTIME_DIR` is set, rather than falling back to the state directory.

A lock in a location no other ricepilot looks in reads as mutual exclusion and
provides none — strictly worse than no lock, because it is silent. The override
exists for D16's reason: the tests run under `env_clear()` and must be able to
say where the lock goes when there is no real `$XDG_RUNTIME_DIR` to find.

## D29 — `lstat_or_absent`, for a directory that has never existed

*M2.* `ops::read::lstat` reports a missing *intermediate* component as an
error, which is right when the caller believes the directory is there. It is
the wrong answer for "is there an in-flight journal?" on a machine where
`state/journal/` has never been created — the ordinary case, not a fault.

`lstat_or_absent` softens absence and nothing else: a *symlinked* component
still refuses, so D9 is untouched.

## D30 — Fixture cases start from an empty tree

*M2.* M1's harness recreated a case's contents over whatever was there. A
crash-injection case is *defined* by the exact state it starts from, so a
staged link left by the previous run would make it pass or fail for a reason
unrelated to the code — as it did, twice, while this milestone was being built.

Each case's directory is now emptied first. D18 already covers why the harness
may call a removal function and the crate may not.

## D31 — The ledger row carries `(dev, ino)` and a profile name, and the M1 note goes with it

*M3.* Fact 3 of the ownership predicate is a ledger row matching the path, the
target string **and** the `(dev, ino)`. Only the last of those is evidence. A
row holding path and target agrees with itself after an installer removes our
link and puts an identical-looking one of its own at the same path pointing at
the same place — which is exactly the case ricepilot must refuse, so the inode
identity is what the row is for. `tests/ledger.rs` asserts that case directly
rather than asserting the happy path twice.

The row also carries the profile that created it. That is *not* part of the
predicate — ownership is the three facts and nothing else — it is what
`status` prints and what the retirement decision reads. Keeping it in the same
file rather than in a fourth one means there is one answer to "what does
ricepilot own", and it is the file whose name says so.

`render::NO_LEDGER_NOTE` and the `!ledger_present && any_foreign` branch in
`cmd_plan` are deleted in the same commit. They existed to explain why a link
that looked correct was reported as foreign while the ledger reader was
unwritten. It is written; the explanation is now false, and a note that
outlives its reason is a lie told in a reassuring tone.

## D32 — `Meta` carries what one `fstatat` can answer, and `mtime_ns` moves to `read`

*M3.* `ops::read::Meta` held kind, dev, ino and mode, because that is what the
planner needs. `verify` needs uid, gid and the mtime as well, and `mtime_ns`
was sitting in `ops::mutate` — where M2 put it because it was the mutating
side's own proof obligation, and where it never belonged, since reading a
timestamp changes nothing.

Both are fixed together. `Meta` now carries every field a single
`fstatat(AT_SYMLINK_NOFOLLOW)` answers, and `mtime_ns` is a thin wrapper over
`lstat` on the read side.

Widening `Meta` rather than adding a second stat-shaped struct is the point,
not a convenience: a manifest entry that took its hash from one syscall and its
mode from another would be describing two files whenever something wrote
between them. One `statat`, one answer.

This is an ops-surface change, so it is its own commit and this is its reason.

## D33 — The volatile matcher is written, not depended on

*M3.* `verify` needs glob matching and `Cargo.toml` has no glob crate. Adding
one changes the committed `Cargo.lock`, and a dependency in a tool whose whole
claim is that its complete set of effects can be audited is a real cost: every
crate added is a crate a reviewer has to take on trust, and the transitive set
does not stay the size it was on the day it was added.

The matcher is forty lines because the syntax it has to support is the syntax
the manifest actually uses: `*` and `?` within one segment, `**` for any
number of segments, and a pattern matching a directory excluding its subtree
(which is what makes `generated = ["btop/themes"]` mean what a reader expects).
Character classes, brace expansion and negation are not supported and are not
needed; a manifest using one would simply not match, rather than matching
something unintended.

The segment matcher backtracks iteratively rather than recursively, so a
pattern like `*a*a*a*a*b` costs time and not stack. That case is a test.

The alternative — `globset`, which is good — was rejected for the dependency,
not for its behaviour. If v1.1 needs the full syntax, taking it then is a
smaller decision than taking it now for four patterns.

## D34 — `verify` reports drift on stdout and in its exit code

*M3.* A check whose result a script cannot act on is not a check, and an
itemised report reduced to one line on stderr is not a report. `verify` wants
both, so `cli::run` returns an `Output { text, code }` and `ExitCode` gains
`Drift = 6`.

Drift is deliberately *not* an `Error`. Nothing failed: `verify` ran, read the
tree, compared it, and the answer was "this profile has changed". Modelling
that as an error would put it in the same category as a refusal, and refusals
mean "ricepilot declined to act", which is a different thing to tell a user.

Two further calls inside the comparison:

* A directory's `mtime_ns` is recorded and **not** compared. It moves whenever
  one of its entries is added, removed or replaced, so comparing it reports a
  shadow of every real change, once per ancestor. The entries have rows of
  their own; the shadow adds nothing and trains the reader to skim.
* A file rewritten with byte-identical content is `Touched`, reported in its
  own paragraph and excluded from the drift exit code. caelestia's theme engine
  does this constantly. It is worth saying (R7) and it is not drift.

## D35 — A generation records absences, and the first switch writes generation 0000

*M3.* A generation is the complete link topology of every managed destination
after a switch: per destination, the target its link points at **or** the fact
that nothing is there. Recording the absence is not tidiness. It is what tells
`rollback` that a destination the previous generation did not have is one it
must displace rather than leave behind — and leaving one behind silently is the
failure D21 names.

The first switch writes *two* generations: `0000` from the pre-switch
observation, then `0001` from the post-switch one. Without `0000` the sentence
"rollback re-applies generation NNNN-1" would have a special case at the only
moment a new user is likely to need it, and a special case in the recovery
ladder is a special case nobody has tested on the day it matters. Generation
`0000`'s profile field is the literal string `(the state before the first
switch)`, because it belongs to no profile ricepilot put there.

`current` is a real file written with `write_atomic`, and `current()` refuses
to read one that is a symlink. The file exists to survive a switch that went
wrong; a switch that goes wrong is one that did something unintended to a
symlink, so the file that says how to get back must not be one. Refusing
rather than following also means the check cannot be defeated by pointing the
pointer at itself.

## D36 — A created link is retired into the attic, and that is said out loud

*M3, and the answer to D21.* Shape 5 switches an absent destination with a
single `symlinkat`. Undoing that means the destination should be absent again,
and making something absent is a removal, which exists nowhere outside
`src/gc/` (R2). There is no exact inverse, and pretending otherwise would mean
either adding a delete primitive or leaving a link behind that the user
believes `rollback` took away.

The answer is **retirement**: the link is renamed into
`state/attic/<ts>/<its absolute path>`, the destination is left empty, and
nothing is deleted. `rollback --commit` reports each retired path and where its
link now is. A user who expected removal gets removal's observable effect —
nothing at that path — plus a sentence saying where the link went, which is
strictly more than a delete would have given them.

What makes this a decision rather than a detail is where it lives. Retirement
is **planned**, not bolted onto `rollback`:

* `PlanContext.retire` names the destinations the target state no longer
  includes, and `plan` emits `Op::RenameToAttic` for each one it still owns —
  in phase C, after the exchanges, which is when it executes.
* A retire destination observed as anything but an owned link or absent is
  refused with `Refusal::Unowned`, for the same reason a switch onto one is:
  ricepilot did not put it there.
* A destination named by both the target list and the retire list is being
  switched, not retired. Without that check a `rollback` onto the same path
  would displace the link it had just staged.
* The journal records retirements in their own `retire` list, because a
  retirement has no "new target" and so is not an `Entry`. Its two states —
  the link is at the destination, or it is in the attic — are as
  distinguishable by reading the filesystem as any other pair, so D23's rule
  covers it unchanged. `#[serde(default)]` keeps an older journal parseable.
* Recovery finishes an unfinished retirement going forward and does nothing
  going backward, because retirement happens in phase C: if no exchange took
  effect, no retirement did either.

One consequence is worth stating plainly, and it is a test: a switch whose
*only* effect is a retirement, interrupted before it happened, is **abandoned**
rather than finished. No exchange took place, so by D23 the switch never
started. The machine is fully old, which is one of the two outcomes R5 allows.
Finishing it would be recovery inventing an effect rather than completing one.

The same mechanism serves `switch` — a profile that drops a path its
predecessor managed retires that path — so `rollback` is not the only caller
and therefore not the only tested path.

## D37 — `sh -n` reads the script on stdin, and a script that fails it is not written

*M3.* `rescue.sh` must be parsed before it is written. A rescue script with a
syntax error is not a degraded rescue script — it is a file that looks like a
way out and is not one, discovered by a user at a TTY with no desktop. So a
script that does not parse is refused and the previous one, which did parse, is
left exactly where it is.

`sh -n` is fed the script on **stdin** rather than through a temporary file.
A temp file would have to be tidied away afterwards, and tidying away is a
removal, which exists nowhere outside `src/gc/` (R2). This is the first entry
in `ops::exec`'s allowlist; the rest of that module remains M5.

Three further calls in the generated script:

* **It does not `set -e`.** Each destination is an independent `if … then …
  else echo FAILED … fi`, so one path it cannot restore does not cost the user
  the ones it can. This is the opposite of R5's all-or-nothing rule, on
  purpose: R5 governs ricepilot's own transactional mutations, and this script
  is rung 3 of the ladder — the thing you run *because* the transactional path
  did not work. Maximising what comes back is the right goal there, and the
  script reports each step so the outcome is never silent.
* **`PATH` is not consulted.** The three commands are located by `lstat`ing
  `/usr/bin`, `/bin` and `/usr/local/bin` when the script is generated, and
  written out absolute. A rescue script that needs the environment to be right
  is a rescue script for a problem other than the one it exists for. If a
  command is not found, the script is not written at all — naming a binary
  that is not there would fail at the one moment it was relied on.
* **A destination the restored generation did not have is displaced**, into
  `state/attic/rescue-NNNN/`, with `mkdir -p` and `mv -T`. Same answer as D36,
  for the same reason: the script has no delete either.

Paths are single-quoted with `'` → `'\''`, and a fixture whose paths contain a
quote is run through a real `/bin/sh` to prove it.

## D38 — The journal is written before the staging links, not after

*M3.* `docs/DESIGN.md` §6 had phase A create the temp links at step 7 and write
the journal at step 8. Implementing the switch showed that ordering is wrong,
so the code does it the other way round and the doc is corrected.

A staging link created before the journal exists is an effect with no record.
Crash there and `recover` finds no journal, reports "nothing to recover" — and
it is right, because nothing it can see is in flight — while a `.rp-tmp-0`
symlink sits in the user's `~/.config` forever. Worse, the *next* switch fails
with `EEXIST` when `create_symlink_tmp` refuses to reuse it (which is itself
correct: a stale staged link is evidence of an interrupted switch, and quietly
overwriting evidence is not a mutator's job). The user is then stuck with a
message about a temp file they never heard of.

Writing the journal first costs nothing. Recovery already handles the state
where a destination is old and nothing is staged — it stages the link and
exchanges, which is the arm a step-driven replay would get wrong — so a crash
between the journal and the first staging link resolves exactly like any other.

## D39 — The `RENAME_EXCHANGE` probe happens after the commit gate

*M3.* The brief says to probe the exchange mode once, at startup. The probe
creates two symlinks in ricepilot's own state directory and keeps them (D22),
which means probing at startup would give `switch` without `--commit` a side
effect — and R4's promise is that a dry run has *zero* of them, not "none worth
mentioning".

So the probe is the first thing after the commit gate. Nothing before that
point needs the answer: the plan is the same value in either mode (D19), and
only the journal records which one was used. A test asserts that a dry run
leaves not even `.rp-probe-a` behind.

## D40 — A switch id is unique, not merely a timestamp

*M3.* The journal id and the attic directory share one name so each is
findable from the other. That name was `timestamp_id`, to the second — and
`rollback --commit` immediately after `switch --commit` is not an unusual
thing to do, it is what a person does when a rice turns out to be wrong.

Two switches in the same second produced the same id, so the second one's
`done-<id>.toml` collided with the first's. `rename_within` refuses to replace
an existing file, which is correct, so the switch failed **after applying
every one of its effects** — the worst possible place for it to fail, and a
test found it on the first try.

`journal::unique_id` appends `-1`, `-2` … until neither `journal/done-<id>.toml`
nor `attic/<id>` is taken. The id stays readable (`gc` makes the operator type
an attic directory's name back, and `20260921T092114Z-1` is still something a
person can check they typed), and the journal and the attic keep naming each
other.

## D41 — Rolling back to generation 0000 says what that generation is

*M3.* Generation `0000` is the topology ricepilot **found**, before it switched
anything. On the target machine those links point into the caelestia clone —
which is also profile `caelestia`'s root — so it is tempting to label the
generation with that profile's name.

It is not labelled that way. ricepilot did not create those links and has no
record saying it did; that is precisely why the ownership predicate would
call them foreign. Inferring a profile name from where a link happens to point
is exactly the reasoning the predicate exists to forbid, and doing it in a
label rather than in a decision does not make it sounder — it makes it
invisible.

So generation `0000`'s profile reads `(the state before the first switch)`,
`rollback` to it says so in its heading, and the ledger rows it writes carry
that string. It is uglier than a profile name and it is what is true. A
`rollback` heading that read "to profile `caelestia`" would be telling the
user that ricepilot is putting back something it had recorded, when what it is
really doing is re-creating a shape it once observed.

The practical consequence: rolling back to `0000` records no blake3 manifest,
because there is no single registered tree to hash. `verify` says plainly that
nothing has been recorded rather than comparing against a manifest of
"wherever those links happen to point".

## D42 — The planner refuses a link that would dangle, and the source facts are data

*M3.* Nothing in M1 or M2 checked what a target's `src` actually was. That was
harmless while no command could act; it stops being harmless the moment
`switch --commit` exists. A profile whose payload has moved, or which was
registered by reference against a clone that has since been deleted, would
produce a dangling link at every managed destination — and a dangling
`~/.config/hypr` is a compositor with no configuration at the next login.

Worse, it would look exactly like a successful switch. The tool would print
"applied", the user would log out, and the first evidence of the problem would
be a session that does not come up. That is precisely the failure this project
exists to prevent, so it is a pre-flight refusal with the destination and the
source both named.

A source that exists and is **not a directory** is refused too, including a
symlink to one. `lstat` does not follow it and neither does ricepilot: pointing
a managed destination at a link whose target it has never looked at is the same
chain of trust the ownership predicate refuses to extend.

The facts are gathered by the caller and passed in as `PlanContext.sources`,
exactly as `missing_requires` already is — gathering is IO, using is not, and
`plan` stays pure. A target whose `src` has no fact is not checked, which keeps
M1's planner tests (written before the check existed) meaningful rather than
rewritten; both callers that can act supply one fact per target, and `plan` and
`switch` therefore agree. A dry run that said it would work and a switch that
then refused would be the worst of both.

## D43 — The copier is in-process; `cp` is not added to the subprocess allowlist

*M4.* The brief specifies a `cp -a --reflink=auto` baseline at `init`
([AGENT_PROMPT.md](../AGENT_PROMPT.md) §3). That is a *mutating* subprocess,
and [SAFETY.md](SAFETY.md) R3's allowlist in `src/ops/exec.rs` is closed and
currently contains nothing that changes a byte: `sh -n` parses without
running, `pacman -Q` queries, `hyprctl` queries, `Hyprland --verify-config`
runs on a sandboxed copy, `git status` reports. `uwsm stop` is the one
exception and it is user-initiated behind a y/N.

Adding `cp` would widen that from "ricepilot can ask questions of the system"
to "ricepilot can hand a mutation to a program whose behaviour is decided by a
flag string". `cp` with the wrong flags is one of the concrete ways this
project fails: `-r` instead of `-a` silently resolves every symlink, and
caelestia's tree contains an absolute one (`userChrome.css`) that would drag a
second tree in; `-T` forgotten turns a copy *onto* a path into a copy *inside*
it; and a `cp` that is interrupted leaves a partial tree that ricepilot has no
delete to tidy away.

**So the copier lives in `ops::mutate` and no subprocess is added.**

The cost the brief warns about — no reflink — is avoided a different way:
`rustix::fs::ioctl_ficlone` is what `cp --reflink=auto` asks the kernel for,
and it is one `ioctl` on two open descriptors, not a program. `copy_tree`
tries it per file and falls back to a 64K read/write loop on *any* error,
because every reason it can fail (wrong filesystem, no support, not a regular
file) is a reason to copy the bytes instead, and none of them has written
anything to the destination. On the target machine's btrfs `/home` the
baseline is therefore still instant and still free; on ext4 it costs what a
copy costs.

What is genuinely given up is `cp`'s decades of edge cases: sparse files are
not detected (`FICLONE` handles them; the fallback writes the holes out),
xattrs and ACLs are not carried across, and hard links between two files in
the tree become two independent files. None of those change what a config
tree *means*, and all three are visible — `verify` records mode, uid, gid and
`mtime_ns` and reports any difference — which is the property that matters:
if the copy is not faithful, the hash check `capture` and `adopt` run before
they declare success is what says so.

## D44 — The copier walks for the uncopyable before it writes anything

*M4.* A socket or a fifo cannot be copied — a copy of one is not the same
object, and pretending otherwise would produce a profile that silently is not
the tree it claims to be. The obvious implementation refuses when it reaches
one, which means a tree with a socket half way through it leaves two thirds of
a copy behind.

R4 says a refusal has **zero** side effects, and R2 means there is no delete
to tidy a partial tree away with, so the two rules together force the order:
`copy_tree` stats the whole source first and refuses the whole copy, naming
the path, before it creates anything. The extra walk costs stats and no data.

The same reasoning is why every directory is `mkdirat`ed fresh and every file
is opened `O_CREAT | O_EXCL`: a copy interrupted by a crash rather than by a
refusal *does* leave a partial tree, and the next attempt must be a refusal
naming it rather than something that quietly writes over the evidence.

## D45 — `capture` stages the profile outside `profiles/` and has no journal

*M4.* Two decisions, and they are the same decision seen from two sides.

**Staging.** The obvious implementation copies straight into
`profiles/<name>/` and writes `profile.toml` last. But `paths::load_all`
reports a profile directory with no readable `profile.toml` as an *error*
rather than skipping it — deliberately, because a half-written profile is
worth knowing about — so an interrupted capture would make every later
`ricepilot list` fail, and R2 means there is no delete to clear it with.

So the profile is built at `data/staging/<name>-<id>/` and moved into place
with one `rename`. Before that rename there is no profile `<name>`; after it
there is a complete one, manifest included. An interrupted capture leaves a
directory under `data/staging/` that nothing reads and the report names.

**No journal.** R4 requires a fsync'd write-ahead journal before the first
effect, and it is worth being explicit about why `capture` has none rather
than letting the absence look like an oversight. A journal exists so that a
mutation to the **live machine** which was interrupted can be finished or
abandoned by observing reality (D23, D24). `capture` makes no mutation to the
live machine at all: it reads live directories, writes inside ricepilot's own
data directory, and its single externally visible step is that one atomic
rename. There is no intermediate state for `recover` to resolve, and a
journal describing one would be a record of something `recover` could not act
on.

`adopt` is the opposite case and gets a journal record of its own (D46).

## D46 — `adopt` gets a third journal record type rather than a bent `Entry`

*M4.* `journal::Entry` models a destination **by its link target**:
`old_target` and `new_target` are path strings, `slot_of` asks
`mutate::link_target`, and a destination that is a real directory comes back
as `Slot::Foreign("not a symlink")`, which `side_of` turns into a refusal. So
an `adopt` journalled with `Entry` would be unrecoverable by construction —
`recover` would refuse the very state `adopt` is designed to pass through.

Retirement hit the same wall in M3 and the answer was `journal::Retire`: a
second record type with its own two states and its own arm in `actions_for`,
rather than an `Entry` bent until it fit. `adopt` gets the third,
`journal::Adopt`, for the same reason and with the same shape.

Its pre-state is identified by `(dev, ino)` rather than by a target string,
because a real directory has no target. That keeps D24's requirement intact —
the two states must be distinguishable by reading the filesystem — and makes
the "old" side *stronger* evidence than a link target is: a directory that an
installer removed and recreated between the journal and the crash has a
different inode, so recovery refuses it rather than moving someone else's
directory into the attic.

The alternative — teaching `Entry` about `(dev, ino)` — would have put a field
that is meaningless for four of the five shapes into the record every switch
writes, and would have made `side_of`'s refusal for a real directory
conditional on which command wrote the journal. That refusal is load-bearing
for `switch`: a real directory at a managed destination is the signature of an
installer having run, and `switch` must keep refusing it.

## D47 — There is no `--yes`. The confirmation always runs; only the widget differs

*M4.* `init` and `adopt` require per-path confirmation ([SAFETY.md](SAFETY.md)
R6), and every message ricepilot prints is snapshot-tested — which means the
prompts need a path a test can drive.

The obvious answer is a `--yes` flag, and it is the wrong one. A flag a test
can pass is a flag a user can pass; R6 exists precisely so a human sees what
is about to be touched; and a confirmation skipped in every test is one whose
first real execution happens on the user's machine, with their configuration
under it.

So there is no bypass. The confirmation always runs, and only the *widget*
differs:

* stdin is a terminal → an `inquire` prompt, defaulting to no.
* stdin is not a terminal → the same question text is printed and one line is
  read back.

Both share `question_text`, both default to no, and both treat end-of-input,
an empty line and anything that is not `y`/`yes` as no. A test pipes an
answer in and therefore exercises the real question, the real parsing and the
real refusal. What it does not exercise is `inquire`'s key handling, which
belongs to `inquire`; R7 says name an untested path rather than imply it is
covered, so the module says so and that branch is kept to one call.

The non-terminal path also echoes the answer it took. Someone reading a
transcript of a command that moved their configuration should be able to see
what it was told, not only what it did.

## D48 — `adopt` copies before it journals

*M4.* R4 says the journal is fsync'd before the first effect, and `adopt`
copies a directory into the profile *before* it writes one. Two reasons, and
the second is the load-bearing one.

The copy is not an effect on the live machine. It writes into ricepilot's own
data directory, at a name that did not exist; nothing in `~/.config` changes,
no link is created, and the user's directory is untouched and unread-from
except to be read. The states a journal exists to resolve — a destination
half way between two shapes — cannot arise from it.

And the journal *cannot* be written first. It records `new_target`, the copy
the new link will point at, and D42 refuses to point a managed destination at
something that does not exist. A journal naming a target that is not there
yet would describe a state recovery must never drive the machine into.

So the order is: confirm, copy, hash-verify, journal, stage, exchange, attic.
A crash before the journal leaves a copy in the profile and a live machine
nobody touched — which the next attempt refuses, naming the copy, rather than
writing over it (R2 again: there is no delete, so a partial copy has to be
something you can look at).

## D49 — `rollback` does not undo an `adopt`, and the output says so

*M4.* After an adopt, `~/.config/hypr` is a link ricepilot owns and the
user's real directory is in the attic. A `rollback` re-applies the previous
generation, which had no entry for that destination — so the retirement rule
(D36) displaces the *link* into the attic and leaves the path empty. It does
not bring the directory back, because bringing it back means renaming
something out of the attic, and nothing in the switch machinery does that.

The choice was between teaching `rollback` to restore from the attic and
saying plainly what it does. It says plainly what it does. A `rollback` that
sometimes restores an attic directory and sometimes retires a link, depending
on which command created the generation, is a command nobody can predict at
the moment they need it most — and "restore from the attic" is a capability
with its own failure modes (what if the destination is occupied? what if two
adopts displaced the same path?) that belongs in `gc`'s neighbourhood, not
bolted to the switch path.

So: `adopt` records a generation (history stays complete and `status` stays
truthful), regenerates `rescue.sh` for the generation before it (which has no
entry for the adopted path, so rescue leaves it alone — correct), and its
committed output names the attic path, gives the two-command way back by
hand, and states in as many words that `rollback` will not do it.

## D50 — `init --root` is required, and "adopting a link" means recording it

*M4.* Two things about `init` that are easy to misread.

**The root is named, never discovered.** It would be easy to find the rice by
reading `~/.config`, collecting the symlink targets and taking their common
ancestor — on the target machine that would even work. It is not done.
DESIGN §9 says management is allowlist-only: a path is managed because a
manifest names it, never because it happened to be found, and the tree those
paths point into is the most load-bearing name of all. A wrong answer there
would register the wrong directory as the profile's root and make every
later ownership judgement wrong in the same direction. So `--root` is
required and the refusal names the likely answer rather than assuming it.

**Adopting an existing link is a ledger row, not a mutation.** The links
`init` adopts already exist and already point where they point. Adopting one
writes a `ledger.toml` row recording its path, target and `(dev, ino)` — the
third fact of the ownership predicate — which is what makes a later `switch`
willing to act on it instead of refusing it as unowned. Nothing is created,
moved or retargeted, and the rice clone is never written to. `init` on a
by-reference rice makes **no** live mutation at all, which is why it has no
journal, for the same reason `capture` has none (D45).

That is also why `init` reports the links it will not offer rather than
omitting them. caelestia links `~/.config/uwsm`, which is hard-denylisted;
a link into the rice that currently resolves to nothing is another. Both are
facts about the rice worth hearing, and leaving them out of the report
because they are inactionable would make the report a list of what ricepilot
found convenient rather than what is there (R7).

The mode-600 proposal is one question for the set rather than one per file.
`volatile` is a manifest *classification*, not something that gets touched —
R6's per-path rule is about paths ricepilot would act on — the files are
listed immediately above the question, and the manifest is a file the user
owns and can edit. Asking twenty times for a decision that changes no path
would train someone to answer without reading, which is the failure mode R6
exists to avoid.
