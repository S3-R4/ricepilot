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
