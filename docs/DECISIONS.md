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
`RICEPILOT_FIXTURE_ROOT` for anyone whose repo is not on `/home`. *(Narrowed
by D56: the override may only name a directory below `<repo>/target/fixtures`;
anything else makes the harness panic.)*

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
there is no real `HOME` in the environment to find. *(D56: inside the test
sandbox, every one of these plus `RICEPILOT_RUNTIME_DIR` is required and must
lie below `<repo>/target/fixtures`; there is no fallback.)*

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

So the probe is the first thing after the commit gate. (D55 makes one
exception to "zero": a switch that ships a Hyprland config writes a sandboxed
scratch copy under `state/verify/` in phase A, dry run included.) Nothing before that
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

## D51 — `adopt` adds the path to the profile's manifest, or the next switch undoes it

*M4.* Found by asking what happens *after* an adopt, which is the question
this milestone's riskiest bug was hiding behind.

`adopt` writes a ledger row saying ricepilot owns the destination. It did
not, at first, touch the profile's `profile.toml`. So the profile's target
state did not include that destination — and `switch <that profile>` builds
its retirement list from exactly that difference: every path the ledger owns
that the target state does not claim (D36). The very next switch into the
profile you had just adopted into would therefore **retire the link adopt
had made**, displacing it into the attic and leaving the destination empty.
No delete, nothing lost, every bit of it reported — and completely wrong.

So `adopt` appends a `[[path]]` block to the manifest, before the journal.
Before, because a crash between the manifest and the link leaves a manifest
declaring a destination that is still a real directory, which `plan` reads
as shape 3 and refuses — honest and harmless. The other order leaves a link
and a ledger row that no manifest claims, which is the footgun above.

It is appended as **text**, not re-serialised from the parsed value. The
file belongs to the user: they may have edited it, commented it or ordered
it to taste, and round-tripping it through a serialiser would throw all of
that away silently. A new `[[path]]` at the end of a TOML file is always
valid, because anything trailing already belongs to the last table. The
result is parsed before it is written, which is also what catches a
destination the manifest already declares — `manifest::validate` refuses a
duplicate `dest` and names it.

This is the one place ricepilot rewrites a file inside a profile, and it is
the profile's *manifest* rather than its content, for a profile whose
payload ricepilot owns. R3 is about profile content: never templating,
never `sed`-ing, never writing through a symlink, never running an
installer. `capture` already writes this file; `adopt` extending it is the
same act. A by-reference profile is refused outright, so the user's rice
clone is never written to either way. The generated manifest's own comment
says this, rather than claiming ricepilot never rewrites it.

The regression test is the sequence, not the mechanism: adopt, then `plan`
must read the destination as an owned link with nothing to do, and
`switch --commit` must leave the link exactly where adopt put it.

## D52 — How `ops::exec` starts a child: typed calls, absolute paths, an empty environment, limits

*M5.* `ops::exec::run` is the first code in the crate that can start any
program other than `/bin/sh -n`, and one entry on its list (`uwsm stop`) ends
the user's session. Four choices, each made so the allowlist is closed by
construction rather than by the discipline of its callers.

**A caller passes a typed `Call`, not an argv.** The M0 stub was
`run(Allowed, &[&str])`, which would have made `run(Allowed::Hyprctl,
&["dispatch", "exit"])` a legal call to an allowlisted program. Each `Call`
variant now builds its own argument vector from typed fields: `hyprctl`
takes a `HyprctlQuery` whose only value is `Version`; `git` is always
`status --porcelain=v1`; `pacman` is always `-Q -- <one name>`. What a
caller *does* supply is checked before anything is located or spawned: a
package name must be a makepkg name (ASCII alphanumerics and `@._+-`, not
starting with `-` or `.`), so a `requires` entry of `--config=/x` never
reaches pacman, and `--` is passed as well; a repository must be an absolute
path free of `..`. `hyprctl dispatch submap reset`, which DESIGN §8 mentions,
is deliberately *not* a variant: it follows a live reload, v1 has none, and
the only thing it could do today is change the running session.

**Binaries by absolute path, `PATH` never consulted.** Each is found by
`lstat` in `/usr/bin` then `/usr/local/bin`, as `rescue.rs` finds its three.
`/bin` is not searched: on Arch it is a symlink to `usr/bin`, and
`ops::read` refuses a symlinked intermediate component (D9), so it would
contribute a refusal and nothing else. `sh` stays at `/bin/sh`, the one path
POSIX names, and is `exec`'d through that link rather than looked up. A
missing binary is a refusal naming the directories searched.
`tests/exec_env.rs` puts an impostor for every allowlisted name first on
`PATH` and proves none of them runs.

**Every child gets an empty environment, on purpose.** `env_clear()`, then
`LC_ALL=C` (so anything parsed — pacman's "was not found" — is in the one
locale whose messages do not change), then only what the call cannot work
without, passed through by name: `hyprctl` gets `XDG_RUNTIME_DIR` and
`HYPRLAND_INSTANCE_SIGNATURE` to find the compositor's socket; `uwsm stop`
gets `HOME`, `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS` and a fixed
`PATH=/usr/bin` because it is a Python program that talks to systemd;
`Hyprland --verify-config` gets nothing, and above all not the instance
signature. `git status` gets no `HOME` and `GIT_CONFIG_NOSYSTEM=1`, so no
global or system config is read, plus `GIT_CEILING_DIRECTORIES` set to the
repository's parent so a directory that is not a repository is not answered
for by the one above it — the fixture tree lives inside ricepilot's own
checkout, so this is tested against exactly that case. Every child starts in
`/`, so none inherits ricepilot's working directory. Nothing else of
ricepilot's environment — `LD_PRELOAD`, `GIT_DIR`, `GIT_CONFIG_PARAMETERS` —
reaches a child; the same test sets all three and shows they have no effect.

`git status` also runs with `--no-optional-locks` and `-c
core.fsmonitor=false`. Without the first it refreshes and rewrites
`.git/index` — a write into the user's clone, which the fixture test
observes plain `git status` doing and asserts ricepilot's does not. Without
the second, a repository can name a program for status to run. What remains
is a clean filter from the repository's *own* `.git/config`, which `status`
can invoke on a racily-clean file; it is the repository owner's own
configuration, and there is no general way to switch filters off from the
command line. Recorded here rather than claimed away.

**Timeouts and an output cap.** std has no wait-with-timeout, so `run`
polls `try_wait` every 5 ms and kills the child — since D54, its whole
process group — at its deadline: `sh -n`
and `pacman -Q` 10 s, `hyprctl` 5 s, `git status` 60 s (a cold cache on a
large tree), `Hyprland --verify-config` 30 s (it parses twice), `uwsm stop`
60 s. Every pipe is serviced by its own thread so a child that fills stderr
while ricepilot writes its stdin cannot deadlock the pair. At most 1 MiB per
stream is kept; the rest is read and discarded, so the child never blocks,
and `Ran::truncated` says so — a truncated answer is not a complete one and
a caller comparing output must treat it as unknown. After the child exits
its pipes get 500 ms; a grandchild holding one open is abandoned rather than
waited for. A timeout is an `Io` error (exit 4), not a refusal: nothing was
decided, something failed. A non-zero *exit*, on the other hand, is not an
error at all — `pacman -Q` exiting 1 is the answer "not installed" — and
`hyprctl` exits 0 even when it reached no compositor, so its callers read
what it said rather than its status.

**The session-ending call cannot be built yet.** `Call::UwsmStop` carries a
`Relogin` token with a private field and no constructor, so no code outside
`ops::exec` can make one (a `compile_fail` doctest proves it) and
`ops::exec` itself does not (`tests/exec.rs` fails if anything in `src/`
constructs one or names the call). `--relogin` is the task that adds the
single constructor, behind its confirmation and its pre-flight. The same
shape gates `Call::HyprlandVerifyConfig`, which takes a `SandboxedConfig`
that only the verify-config sandbox — the next task — will be able to make,
so "run verify-config on the real config" is not a mistake anyone can type.
Neither has been run: `uwsm stop` because it ends the session, and
verify-config because it has no sandbox yet. (D55 adds the sandbox and the
one constructor; the real binary is still never run by default. D58 adds
`Relogin`'s one constructor, behind a pre-flight and a `Yes`; `uwsm stop` is
still never run by a test.) The argv `uwsm stop` will be
run with is a constant, asserted in a test.

## D53 — The requires-check: one `pacman -Q` per package, exit status only, and rollback too

*M5.* `PlanContext::missing_requires` and `Refusal::MissingRequires` existed
from M1 (D12); `src/requires.rs` now fills the list before `plan()` runs, in
`plan`, in `switch`'s phase A and in `rollback`'s. Four choices.

**One package per call, and only the exit status is read.** `pacman -Q a b c`
would be one process instead of three, and its stdout lists what it found —
but by the name of the package that satisfied the query, not the name asked
for: `pacman -Q sh` answers `bash 5.3.9-2`, because bash *provides* sh. A
check that compared names would report a satisfied requirement as missing
and print a `paru -S` line for something already there. Exit 0 from a
single-name query means "installed, or provided by something installed",
which is the question. Profiles list a handful of packages, so the cost is a
handful of millisecond processes. `pacman -T` would answer the provides
question and version constraints too, but it is not what the allowlist
names, and `requires` entries are package names, not constraints — the
manifest now refuses anything else when it is read (`hyprland>=0.55`,
`--config=…`), with the same rule `ops::exec` applies before spawning.

**Exit 1 is "missing" only with pacman's own sentence.** pacman exits 1 both
for a name it does not know and for failures such as an unreadable
database. Under `LC_ALL=C` (D52) the first always says `was not found`, so
exit 1 with that text is "missing"; any other exit, a signal, or exit 1
without it is a refusal naming the command to run by hand. Guessing
"missing" would have been the safe direction for the switch, but the
refusal would then send the user to install a package they already have.

**No pacman, but `requires` declared: refuse.** The alternative — skip the
check and switch — makes the pre-flight incomplete without saying so, and
R4 says a mutation is preceded by a *complete* pre-flight. The refusal says
what to do on a machine without pacman: take `requires` out of the
manifest. A profile with no `requires` never looks for pacman at all, which
is also why every existing CLI test fixture now declares none: they are
about shapes, not packages, and CI has no pacman. `tests/requires.rs`
covers the check against the real pacman and skips, saying so, without one;
the two refusals only a pacman-less or broken machine reaches are
snapshotted directly.

**`rollback` checks the profile it returns to.** Going back into a profile is
a switch into it, through the same code path (DESIGN §6), and a session
whose compositor is not installed is broken regardless of which command
linked it. A declined rollback leaves the machine on the generation it is on
— one that worked well enough to roll back *from* — and prints the `paru`
line. Rolling back to generation `0000`, which belongs to no profile, checks
nothing. `rescue.sh` checks nothing either, deliberately: it is for a TTY
where the session has already failed to start, and it must not depend on
pacman any more than on ricepilot.

## D54 — A child that overruns is killed with its whole process group

*M5.* D52's timeout killed the direct child and nothing else. A child that
had started something of its own — `sh -c 'helper &'`, a wedged `git` hook, a
daemon an `exec =` line forked — left that grandchild running after
ricepilot had reported the call as killed, still holding ricepilot's pipes
open and still doing whatever it was doing.

Every child is now started with `CommandExt::process_group(0)`, so it leads
a process group of its own (std does the `setpgid` between fork and exec; no
unsafe), and on timeout the group is killed with
`rustix::process::kill_process_group(pid, SIGKILL)` before the leader is
killed and reaped. The order matters: while the leader is unreaped its pid
cannot be reused, so the group id cannot name anyone else's processes.
`ESRCH` — the whole group already gone — is not a failure.

`src/ops/exec.rs` tests it against the real supervisor: an `sh -c` that puts
`sleep 30` behind `&`, writes its pid to a file under `target/fixtures/` and
waits, run with a 500 ms limit; the test then asserts the `sleep` is gone. A
control test kills the same child the old way and asserts the `sleep`
survives it, so the first test cannot pass for an unrelated reason; it was
also checked by hand that reverting the group kill makes the first test fail.
(The supervisor was split out of `run_within` as `supervise` so a test can
hand it a child that does more than `sh -n` ever will, without adding a test
entry to the allowlist.)

Three things this does not do, recorded rather than implied:

* **A child that exits normally** is not followed by a group kill. Doing that
  safely needs the leader kept as a zombie (`waitid(…, WNOWAIT)`) until the
  group is killed, and none of the allowlisted queries leaves anything
  behind. A grandchild that outlives a normal exit still has its pipes
  abandoned after 500 ms, as D52 says.
* **A grandchild that leaves the group** — `setsid()`, or `setpgid` of its
  own — is out of reach. Hyprland's `exec` double-forks to detach the
  command from the compositor, and whether a given version also leaves the
  group is Hyprland's business, not something ricepilot can rely on. So the
  process group is *not* what keeps `Hyprland --verify-config` from running
  a user's `exec =` lines; stripping them from the scratch copy is (D55).
* **Ctrl-C** at the terminal now reaches ricepilot and not its child, since
  the child is no longer in the terminal's foreground group. A ricepilot
  ended that way leaves its child to finish on its own (the deadline was
  ricepilot's to enforce, and ricepilot is gone); none reads the terminal
  (stdin is `/dev/null` or a pipe), so none is stopped by `SIGTTIN` either.
  For `uwsm stop` that is the better way round: a session teardown should not
  be interrupted half way by a keypress aimed at ricepilot.

## D55 — The sandboxed verify-config: what is stripped, what is run, and where the copy stays

*M5.* `Hyprland --verify-config -c <file>` parses a config twice and forks
every `exec =` line on the second pass, so it may only ever see a scratch
copy. `src/hyprconf.rs` (pure) reads the conf dialect as far as that needs;
`src/ops/exec/sandbox.rs` holds `SandboxedConfig::build`, the type's only
constructor (its fields are private to `ops::exec` and that child module);
`src/hyprverify.rs` decides when to run it and turns the answer into
`Refusal::VerifyConfigFailed`, which `plan()` now emits. Decisions:

**What is stripped, and in which direction it errs.** A line is blanked if
its keyword's last `:`-segment starts with `exec` (so `exec`, `execr`,
`exec-once`, `execr-once`, `exec-shutdown`, and anything later versions add),
or is `plugin` (a shared object to load; whether `--verify-config` loads it is
not visible from here). The keyword is looked for behind any leading
whitespace, `#`, byte-order mark or other non-alphanumeric noise and compared
case-insensitively, so `exec=x`, `\texec-once\t=`, `EXEC-ONCE`,
`general:exec` and even a commented-out `# exec-once` are blanked; a line
continued with `\` is blanked whole, and a continuation line that itself
looks like exec is blanked whatever it continues. Blanking a line Hyprland
would not have run costs a less thorough syntax check; keeping one it would
have run costs a process nobody asked for, so every ambiguity is resolved
towards blanking. Lines are replaced by empty lines, never removed, so the
line numbers Hyprland reports are the original's. `bind = …, exec, …` and a
workspace rule's `on-created-empty:` are kept: they are commands run on a key
press or a new workspace, which a config check never has. Hyprland's `exec`
double-forks to detach, so the process-group kill (D54) is not what makes
this safe; the stripping is.

**`source`, resolved as it will be after the switch.** Each `source =` is
resolved the way Hyprland 0.55.4 was observed to: `$name` from variables
defined earlier (including in files sourced earlier), `$HOME` from the
environment, a leading `~`, and a relative path against the directory of the
file it is written in. A path under a destination the switch links is read
from that link's *new* source, never through the link that is there now
(which still points at the profile being left); a path under a destination
being retired reads as absent; a path into a profile's source tree is the
same file seen through its destination. Globs are expanded with `*` and `?`
per component, dotfiles only by a leading `.`, in sorted order, as glob(3)
does. Every file reached — including ones outside the profile, such as
caelestia's `~/.config/caelestia/*.conf` — is copied into the mirror at the
path Hyprland would see it at, stripped the same way, and the `source` line
is rewritten to name the copy (glob kept, since every match is copied).
`$variable` definitions whose value is a path into the profile, or starts
with `~`, are rewritten into the mirror too. A literal `source` of a file
that does not exist is left for Hyprland to report.

**What is refused rather than approximated.** A `source` whose resolution
ricepilot cannot be sure of is not sandboxed, and a config that is not
sandboxed is not run — the switch declines with the reason: a keyword built
from a variable (`foo$x = …`), a `source` or variable definition that is or
might be continued across lines, an undefined `$name` in a `source` path
(other than `$HOME`), `..` (lexical and kernel resolution disagree once the
destination is a symlink), `~user`, `[…]`/`{…}` globs, a sourced symlink or a
glob matching a directory, a cycle, more than 256 files, 32 levels or 1 MiB
per file, and a file sourced twice to different effect. Symlinked
intermediate components are refused by `ops::read` as always (D9). Refusing
is the only answer consistent with R4: a check that could not be done is not
a check that passed.

**The post-check.** After writing, `build` re-reads every file it wrote and
refuses to return a `SandboxedConfig` if any line still reads as an
exec-family keyword or any `source` names a path outside the mirror — the
same detector applied to what is on the disk, not trust in the rewriter.

**The child's environment.** `env_clear()` (D52), then `HOME` and
`XDG_{CONFIG,CACHE,DATA,STATE}_HOME` inside the mirror and `XDG_RUNTIME_DIR`
an empty `run/` beside it. Hyprland refuses to start at all without a
runtime directory, and the real one is where the live compositor's socket
is. No `HYPRLAND_INSTANCE_SIGNATURE`, `WAYLAND_DISPLAY` or
`DBUS_SESSION_BUS_ADDRESS` reaches it. The scratch directory must be on the
home directory's device and not under `/tmp`, checked before anything is
read.

**Which file, and the dialect.** Reality, not the manifest: if the tree the
switch would link at `~/.config/hypr` has a `hyprland.lua`, that is what
Hyprland 0.55 loads (its own log says "Lua config not found, using legacy
config" otherwise), and a Lua config is a program — `os.execute` is one call
away — so no line-stripping makes it safe to parse. It is not run, the plan
says "NOT checked", and the switch goes ahead — unless `--strict`, which
refuses it (D59) — (`NOT-POSSIBLE.md#verify-lua-config`). Otherwise `hyprland.conf` is checked.
`hypr_dialect` in the manifest decides nothing here. A tree with neither file
ships no Hyprland config and is not checked; that Hyprland would then write a
default config into the profile is left for `doctor`.

**No Hyprland installed: skip, with a note.** Unlike a missing pacman (D53),
this does not refuse. D53 refused because `requires` is a declaration the
pre-flight must be able to answer; verify-config is a check whose pass means
only "the syntax parsed", and on a machine with no `Hyprland` in `/usr/bin`
or `/usr/local/bin` there is nothing to answer it with — and, if the profile
needs Hyprland, `requires = ["hyprland"]` is where that is said and refused.
The plan prints `was NOT checked` and why, so a skipped check is never
mistaken for a passed one (R7), and `--strict` refuses it (D59). Nothing is
written in that case.

**When it runs, and the one effect it has.** In phase A of `switch` and
`rollback` (one code path, so rolling back into a profile checks it too;
`rescue.sh` checks nothing, as D53 says) and in `plan` — dry runs included,
so the plan printed is the one `--commit` acts on. That makes the scratch
copy the single thing a dry run or a refusal can leave behind, and SAFETY R4
and D39 now say so: it is in ricepilot's own state directory, never a live
path or a profile. A pass prints `parsed … that is a syntax check and nothing
more` every time; exit 0 is never reported as more (`NOT-POSSIBLE.md#verify-
config-as-proof`).

**The scratch copy stays.** `state/verify/<id>/` — the switch id, so it sits
beside `attic/<id>/` and `journal/done-<id>.toml`; `plan`, which has no
switch id, uses its timestamp — with `-N` appended if the name is taken.
Inside: `root/` (the mirror) and `run/`. Nothing outside `src/gc/` can remove
it, and it is the evidence of what Hyprland was actually shown, so it is
kept; each is a few config files. `gc` is what reclaims it. A config refused
before copying leaves nothing.

**Tests never run the real binary by default.** The sandbox is proved in
unit tests against a fake `Hyprland`: a POSIX sh stand-in under
`target/fixtures/` that records argv, environment and every file it reads,
follows `source` lines and runs every exec-family line it finds, twice. A
fixture with exec lines in the entry file, a relative source, a glob, an
absolute path into the profile, a file outside the profile and a live
`~/.config/hypr` still pointing at the old profile writes a marker per exec
line; with the sandbox, none appears, and a control run of the stand-in on
the original shows it would. Stripping and resolution have pure unit tests
with the nasty spellings above. The stand-in is injected with a `cfg(test)`
thread-local that exists only in the crate's own unit-test build — there is
no variable or flag that swaps a binary in a ricepilot anyone runs — and in
that build `locate` never returns the real `Hyprland`. The integration tests
reach the CLI only with configs that never get as far as running it (a Lua
config, declined first; an unsandboxable one, which the test first confirms
the sandbox refuses). The existing adopt/init fixtures that happened to
contain a `hyprland.conf` now call it `monitors.conf`, so that no default
test runs the compositor. The real binary is run only by
`tests/verify_config_live.rs`, under `RICEPILOT_LIVE_TESTS=1`; `hyprctl
version` in `tests/exec.rs` is behind the same opt-in, since it talks to the
running compositor. *(D56 makes this structural: inside the test sandbox
`ops::exec` refuses both unless that opt-in is given.)*

## D56 — The test sandbox: `RICEPILOT_SANDBOX` only ever takes away

*M5, by the user's order that the suite must never be able to touch the real
system.* Until now nothing but convention kept the integration tests off the
real machine: a test that forgot `Fixture::env()` or `env_clear()` ran
ricepilot against the real `$HOME`, `Paths::rooted_at` fell back to the real
`XDG_RUNTIME_DIR` for its lock, the crash helpers inherited the test
process's `HOME`, and the only thing between a `plan` in a test and the real
`/usr/bin/Hyprland --verify-config` was that no fixture happened to ship a
`hyprland.conf` (D55). Each of those is now closed by code.

**One variable, `RICEPILOT_SANDBOX=<root>`.** The harness sets it to
`<repo>/target/fixtures` (`common::SANDBOX_ROOT`) on its own process the
first time any test touches the harness (`common::enter_the_sandbox`, called
by `fixture_root()`), and passes it to every process it starts. While it is
set:

* `Paths::from_env` requires every one of `RICEPILOT_HOME`,
  `RICEPILOT_DATA_DIR`, `RICEPILOT_STATE_DIR` and `RICEPILOT_RUNTIME_DIR`,
  and each must be an absolute, `..`-free path strictly below the root.
  Nothing falls back to `HOME` or `XDG_RUNTIME_DIR`; a missing or escaping
  one is refused (R1) and snapshotted. `Paths::rooted_at` — the harness's
  in-process constructor — no longer falls back to `XDG_RUNTIME_DIR` either,
  so an in-process lock can only be taken under an explicit runtime override.
  A value of the variable that is not a plain absolute path is refused, not
  ignored: a sandbox that switched itself off would be worse than none.
* `ops::exec::run` refuses `Hyprland`, `hyprctl` and `uwsm`
  (`Allowed::reaches_the_session`) before it locates anything
  (`exec::sandbox_refuses`, R1). It is checked in `run`, the one place a
  child is started, so whatever a caller has found or built — `hyprverify`
  locates Hyprland and builds the sandboxed copy first — nothing that reaches
  the session is spawned. `sh -n`, `pacman -Q` and `git status` still run:
  they are the read-only probes the tests exist to exercise.
* `RICEPILOT_LIVE_TESTS=1` is read only here, and restores Hyprland and
  hyprctl — what an unsandboxed ricepilot does anyway — and never `uwsm`.
  It remains the only way a test runs a real session binary.

**Why disable rather than redirect.** The alternative was a variable naming
a directory of fake binaries under `target/fixtures`. That is exactly the
"variable that swaps a binary in a ricepilot anyone runs" D55 ruled out, and
scoping it ("honoured only when the fixture overrides are also set") would
make it harder to misuse, not impossible — the overrides are ordinary
variables too. `RICEPILOT_SANDBOX` needs no scoping because it has no
direction to be misused in: set by a user, by a script, by accident, it can
only make ricepilot refuse more. There is no variable that adds a binary, a
search directory, an allowlist entry or a location. The fake `Hyprland` stays
the `cfg(test)` thread-local of D55; in that build `hyprctl` and `uwsm` are
now never located at all.

**What the harness does.** `Fixture::env()` sets, besides the sandbox and
the four overrides, `HOME` and every `XDG_*` base directory into the
fixture, so even code that ignored the overrides would find only the fixture;
it panics if any value it would hand out lies outside the root.
`fixture_root()` panics on a `RICEPILOT_FIXTURE_ROOT` outside the root —
which narrows D5: the override can still pick a subdirectory, and can no
longer move fixtures off the repository (the user's order outranks the
convenience). `Fixture::new_in` panics unless the process is in the sandbox,
and after building checks that the home *resolves* — through every symlink,
so a `target` symlinked elsewhere is caught — below the canonical
`<repo>/target/fixtures`. Starting a process has exactly three routes, all
in `tests/common/mod.rs`: `ricepilot(f)` (empty environment plus
`Fixture::env()`), `helper(path)` for the crash helpers (empty environment
plus the harness's own variables; the helper builds its fixture and sets
`Fixture::env()` on itself) and `sh(script)` for the rescue scripts (empty
environment).

**The guard, `tests/sandbox.rs`.** It fails if any fixture kind's home,
state directory, profile root, `Fixture::env()` value or resolved `Paths`
lies outside the root or inside the real `~/.config`, `~/.local` or
`~/.cache`; if `Fixture::env()` leaves any of the overrides, `HOME` or
`XDG_*` unset; if the binary does not refuse a missing or escaping override;
and if a `plan` of a sandboxable `hyprland.conf` — on a machine with Hyprland
installed — does anything but fail with the sandbox's refusal (the test first
asks `exec::sandbox_refuses` whether the environment it is about to use
refuses Hyprland, and stops if not; the config's exec line writes a marker,
which must not appear). Textually, over every other file in `tests/` and
`examples/`: no `cargo_bin(`, no `Command::new(` other than `cargo` building
an example, the harness's `/usr/bin/git` and `tests/guards.rs`'s own, no
`env_remove(`/`remove_var(`, nothing that sets the sandbox variable, and no
`Call::Hyprctl(`, `Call::HyprlandVerifyConfig(`, `Call::UwsmStop(` or
`hyprverify::check(` outside the named live-gated test. Every crash helper
must use the harness, and one that resolves `Paths` from its environment
must set `Fixture::env()` on itself first.

**What is not closed, and why.** The in-process variable is set with
`std::env::set_var` on first use of the harness, not before `main`: a
constructor needs `#[link_section = ".init_array"]`, which `unsafe_code =
"forbid"` rejects. A test that named a session-reaching call before touching
the harness at all would run outside the sandbox; the textual guard is what
forbids that. The lexical "below the root" check cannot see a symlink placed
*inside* the sandbox; `ops::read` refuses symlinked intermediate components
(D9) and the harness's canonical check covers the fixture homes. Neither
`HOME` nor anything else stops a developer running the real binary by hand;
that was never a test, and R1 governs it by rule.

## D57 — `doctor` is read-only by construction, reports problems before hazards, and exits 7

*M5.* `doctor` is the command someone runs when they already think something
is wrong — possibly while a switch is stuck, possibly from a TTY. Four calls.

**By construction, not by care.** Rust cannot stop one module naming
another, so "read-only" is made structural in three layers:

* `src/ops/look.rs` defines `Look`, a trait whose every method reads —
  `lstat`, `readlink`, `resolves`, `list_dir`, `slurp`, `read_into`,
  `size_of` — or runs one of the two read-only subprocesses, `pacman -Q` and
  `sh -n`. Its one implementation, `Live`, is one call per method into
  `ops::read` or `ops::exec::run` with `Call::PacmanQuery` /
  `Call::ShSyntaxCheck`. There is no method that creates, renames, writes,
  locks, or starts anything else.
* `doctor::diagnose(&dyn Look, &Where) -> Report` learns about the machine
  only through that value. The pieces of other modules it reuses were
  re-pointed at a `Look` rather than copied, so there is still one tree walk
  (`verify::build_via`), one requires-check (`requires::missing_via`) and one
  rescue-script generator (`rescue::Binaries::locate_via` + the pure
  `rescue::script`); the old entry points call them with `Live` and behave
  exactly as before. The loaders doctor needed (ledger, generation,
  `current`, journal, recorded manifest) gained pure `parse(text, path)`
  functions, which the loaders now call.
* `tests/doctor.rs` checks the sources textually. Every `crate::` path in
  `src/doctor.rs` and `src/doctor/` must be on a list of pure items
  (types, constants, path computations, parsers of text already read, and
  the three `*_via` readers); a grouped import is refused so nothing hides
  behind braces; and words naming a mutation or a read around `Look` —
  `mutate`, `lock::`, `write_atomic`, `journal::write`, `mark_done`, `::save`,
  `regenerate`, `SandboxedConfig`, `hyprverify::check`, `exec::`, `Call::`,
  `ops::read`, `std::process`, `std::env`, `.record(`, `.observe(` — may not
  appear at all, prose included, as with D3. `src/ops/look.rs` gets the same
  treatment with its own list. A third test plants a write, a grouped import
  and a forbidden word and shows the check catches each. The methods of the
  values doctor holds (`Ledger`, `Generation`, `Manifest`, `Journal`,
  `TreeManifest`) were audited: none writes, and the two that read around
  `Look` are on the forbidden list.

So doctor takes no lock (a test runs it while the lock is held, and it
answers), builds no verify-config scratch copy — a `hyprland.conf` is
reported "not checked by doctor; `ricepilot plan` runs the sandboxed
verify-config", a `hyprland.lua` as uncheckable, whatever `hypr_dialect`
says — and starts no `hyprctl`. The gate test takes `(dev, ino, mtime_ns)` of
every path in each fixture (home, state, profile payloads, rice trees, the
fixture's stand-in `/`) before and after every doctor run in the suite and
fails on any difference, including a path that appeared.

**What it reports, and in which order.** Problems first, because they are
ricepilot's own state and a human must act: an in-flight journal (first of
all — until `recover` runs, everything else may describe a half-finished
state), a profile or ledger or generation that does not parse, each owned
link that is gone, is a real directory or a file, points elsewhere, is a
different inode, points outside every registered root, or dangles; a ledger
row whose profile's manifest no longer declares it, declares it as something
that is not a link, or declares another `src` (D51 — the next switch would
retire or re-point it); the adopt-then-rollback state (D49), found from the
retired journals' `Adopt` records: the destination is empty, nobody owns it,
and the user's directory is — or is no longer — at `attic/<id>/<path>`; a
declared source that is missing; a tree linked at `~/.config/hypr` with
neither `hyprland.conf` nor `hyprland.lua` (Hyprland would write a default
config into the profile); drift of any profile with a recorded manifest,
through the same walk and comparison `verify` uses; `rescue.sh` missing when
a generation is current, not a regular file, rejected by `sh -n`, or not the
exact text a switch would write now for generation N-1; and requires of the
*live* profile that are not installed. Requires missing for another profile
are a note — `switch` refuses them when it matters, and a spare profile
should not keep doctor red.

Then hazards: standing facts about the machine that ricepilot works around
and cannot change — `/home` with no snapper config (with the root-only
`snapper -c home create-config /home`, printed, never run; a config doctor
cannot read makes it "may not be snapshotted" and names
`snapper list-configs`), `~/.local/bin/hypr-session` writing a `SESSION_DIR`
into `~/.config/hypr` once ricepilot manages that path (with the patch, as a
unified diff of the one line, never applied), `caelestia shell -d` /
`caelestia resizer -d` running (from `/proc/*/cmdline`), a profile rooted at
`~/.local/share/caelestia` (caelestia-cli's migration can delete it), an
attic over 1 GiB or 100 entries, and more than 20 verify-config copies
(D55). Then notes — what was not checked and why — and every passing check
folded into a single `ok:` line.

Each finding prints its path, one clause saying what is wrong, the rule, the
observation, and shell lines to run, with `#` comments saying which is which.
The printed commands are chosen to be safe to paste: dry runs before
`--commit`, `mv -T x x.set-aside` rather than anything that removes, `diff`
before either. `adopt` is not suggested for a real directory at an owned
destination: the profile already declares that path, and adopt would refuse
the duplicate.

Text is broken by hand, never wrapped to a width: a wrap point that depends on
how long a path is would make two checkouts' reports differ.

**Exit status: 0, or 7 (`Unhealthy`).** 7 when at least one problem was
found; 0 otherwise, hazards included. Not `Drift` (6), which says "this
profile changed" — doctor reports drift among other things, and a script that
distinguishes the two should be able to. Not an error either, for D34's
reason: doctor ran and this is its answer. Hazards do not set it because they
are true on every run until the user changes the machine, and a status that
is never 0 is one nobody reads. Nothing doctor fails to read is fatal: every
check turns its own failure into a finding or a note, and the rest carry on.

**The machine-wide checks take a root, and the sandbox takes it away.**
`/proc` and `/etc/snapper` are read under `Where::system`. The CLI passes `/`,
or `None` inside the test sandbox (D56) — which skips them and says so — so
no test reads the real machine's processes or snapper configuration. The
tests of those checks call `diagnose` in-process with a fixture directory as
the root: a value, not a variable, so nothing a user can set points doctor
anywhere else. `hypr-session` and the caelestia root are under the home and
are always checked.

What is not covered: doctor cannot tell a stuck switch from a running one
(it would need the lock), so the journal finding says to wait if another
ricepilot is running. The theme-daemon match is on argv (`caelestia` then
`shell`/`resizer` then `-d`/`--daemon`); a daemon started some other way is
not seen. The `SESSION_DIR` patch is offered only for an assignment it can
read; a script that builds the path otherwise is reported with the line and
no patch.

## D58 — `--relogin`: offered after a completed switch, six checks and a yes, and the token needs all three

*M5.* `uwsm stop` is the one entry on the allowlist that changes the running
machine: it logs the user out. A logout after a switch whose rescue script
did not get written is how a user ends up at a greeter with no way back, so
what must be true before it is *offered* is the decision, and each part of
it is a value the next step cannot be reached without.

**When it is offered at all.** Only after a `switch` or `rollback` that ran
to the end of phase C *in this process*: `switch::Ended::Completed`, made on
the last line of `run_with`, after the journal is retired. A dry run, a
declined switch and a switch with nothing to do each say why no logout is
offered and exit as they would have. "Nothing to do" is deliberately not
"the switch happened earlier, log out anyway": this process did not do that
switch and holds no proof of it, and the message says to run `uwsm stop` by
hand if that is what is wanted.

**`rollback` gets it too.** It is the same code path (DESIGN §6), it ends in
the same `Completed`, and it gets the same checks. The argument for leaving
it off — rollback is what you run when things are already wrong — is an
argument for the checks, not against the offer: a rollback from a session
that came up broken is exactly when the user wants the greeter back on the
old profile, and from a TTY the session check declines anyway.

**The six preconditions.** Read after the switch — only read — into
`relogin::Facts`, judged by the pure `relogin::decide`; every one must pass,
and a refusal lists all six, each `ok` or `NO`, not the first to fail:

1. **switch** — `generations/current` is the generation this switch recorded.
2. **journal** — nothing is at `journal/current.toml`, and this switch's
   `journal/done-<id>.toml` is a regular file.
3. **rescue** — `rescue.sh` is a regular file, byte-for-byte the script
   `rescue::script` writes for generation N-1 with the binaries found now
   (the comparison `doctor` makes, D57), and `sh -n` accepts the text on the
   disk. Its stderr is not shown: bash here, dash on CI.
4. **links** — every destination the plan linked is a symlink whose target
   string is the plan's source, and the ledger, re-read, has a row with that
   target *and* the link's `(dev, ino)` — so a look-alike link put there by
   something else fails; every destination the plan retired is absent and has
   no row.
5. **lock** — this process holds the `flock` (the `Completed` owns the
   `Lock`, taken in phase A), and the lock path still names the inode it is
   held on (`Lock::is_at_its_path`: one `fstat`, one `lstat`). `flock` is
   per inode, so a lock file replaced under its holder lets a second
   ricepilot lock the new one and believe itself alone.
6. **session** — `XDG_RUNTIME_DIR` is set and absolute (inside the test
   sandbox: below its root, or nothing is read, D56); `WAYLAND_DISPLAY` is a
   plain name, and what is at it in `XDG_RUNTIME_DIR` is neither a file, a
   directory nor a link; `DBUS_SESSION_BUS_ADDRESS` is set; and exactly one
   `wayland-wm@*.service` — the unit uwsm runs the compositor as — is
   recorded in `$XDG_RUNTIME_DIR/systemd/units/invocation:<unit>`, the
   record systemd's user manager keeps for each active unit (observed with
   systemd's user manager and uwsm 0.26.5 on the target machine). A listing,
   two `lstat`s and four variables. Not `uwsm check is-active`: that is a
   second uwsm subprocess, over D-Bus, for a question the filesystem answers
   read-only, and the allowlist is not widened to ask it.

Outside a uwsm session it declines rather than asks. From a TTY, ssh or a
timer, `uwsm stop` would end a session the user is not looking at; in a
session uwsm did not start it has nothing to stop; and the only other
logouts — `hyprctl dispatch exit`, a signal to Hyprland — are the ones
AGENT_PROMPT §1 rules out, and nothing on the allowlist can do either.

**Checked, asked, checked again.** The first check decides whether to ask.
The switch's own output, every check and the `sh …/rescue.sh` line are
printed above the question, which is `confirm`'s y/N — default no, no flag
that skips it (D47). An answer can take minutes, and an installer can
replace a link in less, so after a yes the checks run again and the token is
made from the second result. The lock is held from phase A of the switch
until `uwsm stop` returns, so no other ricepilot starts in between.

**The token has one constructor, and it takes the proof.**
`ops::exec::Relogin::after(&Cleared, Yes)`, `pub(crate)`, is the one place a
`Relogin` is written. `cli::relogin::Cleared` has private fields, is made
only by `relogin::preflight` when all six checks pass, and owns the
`Completed` (and so the lock). `cli::confirm::Yes` has a private field and is
made only by `confirm::affirmed`, when the question was answered yes.
`tests/exec.rs` holds the source text to this — each of `Relogin { … }`,
`Cleared { … }`, `Yes { … }` and `Completed { … }` written once, in its one
file; `Relogin::after(`, `UwsmStop(` and `confirm::affirmed(` named only in
`cli/relogin.rs` — and `compile_fail` doctests show nothing outside the
crate can write a `Relogin` or call its constructor.

**The environment.** D52's policy, unchanged, now built by the pure
`exec::uwsm_stop_invocation(get)`, which `Call::invocation` returns for
`UwsmStop` and which `run` uses: `LC_ALL=C`; `HOME`, `XDG_RUNTIME_DIR` and
`DBUS_SESSION_BUS_ADDRESS` when set (uwsm finds the user's systemd through
the bus); `PATH=/usr/bin`. Not `WAYLAND_DISPLAY`,
`HYPRLAND_INSTANCE_SIGNATURE`, `PYTHONPATH` or `LD_PRELOAD`. `tests/exec.rs`
asserts the argv and the whole environment from a table.

**Exit status.** A logout not offered, declined by a check or answered no
leaves the switch's status: the switch did its job and says so, and the
logout was an offer, reported on stdout. Running `uwsm stop` and it not
working is an error: not installed is a refusal (R3), a timeout or a
non-zero exit is `Failed` (4) with "the switch itself is complete; log out
by hand".

**Tested up to the exec boundary, never past it.** Inside the test sandbox
`ops::exec::run` refuses `uwsm` before looking for it (D56), so a test that
answers yes reaches exactly that refusal — its presence on stderr is the
evidence the call was made, and its absence after a no, an empty answer or
end of input is the evidence nothing was. The session is built in the
fixture's own runtime directory (a bound Unix socket, a unit record); the
real one is never read. Each precondition is broken after a real in-process
switch and its refusal snapshotted; the second check is tested by replacing
`rescue.sh` while the question is on the screen, then answering yes. What
is not tested, and cannot be without logging the user out: that `uwsm stop`
with this environment ends the session. That is the acceptance run's.

What this does not prove, recorded rather than implied: that the Wayland
socket belongs to the compositor in the uwsm unit (a nested compositor inside
the session would pass); `read::Kind::Other` accepts a fifo as well as a
socket; the bus address is required to be set, not checked to be this
user's; systemd's `invocation:` records are its implementation, not a
documented interface; and a ricepilot started with a different
`RICEPILOT_RUNTIME_DIR` takes a different lock, as it always has.

## D59 — `--strict` refuses what a switch otherwise reports and goes ahead past: drift outside `volatile`, a profile it could not compare, a config it did not check

*M5.* DESIGN §3 said drift is "reported and the switch proceeds, unless
`--strict`", and until now a switch reported no drift at all: the only
report-and-proceed outcome it had was the verify-config's "NOT checked".
So `--strict` needed two things — something to report, and a rule for when
reporting becomes refusing.

**What is reported: the target profile against its recorded manifest.**
Phase A of `switch` and `rollback` (one code path, so a rollback reports it
too) now compares the tree it is about to link with the blake3 manifest
ricepilot recorded the last time it switched to, or captured, that profile —
`verify`'s walk and `verify`'s comparison, the profile's current `volatile`
globs excluded (`switch::drift`, read-only, gathered into
`plan::Drift` so `plan()` stays pure). Substantive differences are listed in
a `profile drift:` block and the switch goes ahead: the profile is what it is
now, and a hand edit or a `git pull` in a rice is the ordinary case. A path
rewritten with identical content is counted and never drift, as in `verify`
(D34). Without `--strict`, a profile that matches, or has nothing recorded
yet, prints nothing: a block on every switch would teach the reader to skip
the one that matters, and none of the existing output changed. A comparison
that could not read the recorded manifest or the tree says so and goes
ahead, as the switch did before the comparison existed (phase C records a
fresh manifest over one that does not parse). The comparison runs in a dry run too, so the plan printed is
the plan `--commit` acts on. It is a second walk of the tree (phase C
records one); that is the price of reporting before rather than after.

**What "volatile drift" means here.** Paths matching `volatile` are, by
declaration, rewritten by applications at runtime (`fish_variables`,
`shell.json`), and they are excluded from the recorded manifest, so there is
no baseline to drift from — deliberately. Recording them and refusing when
they change would make `--strict` refuse every switch into any profile a
running app writes into, which is all of them. So `volatile` is what the
comparison leaves out, and says it left out ("not compared, because the
manifest declares them `volatile`: …"), and the drift reported and refused is
what the `volatile` list did *not* account for — an unreviewed edit, or an
application writing where no one declared it would. That is the drift
`--strict` is for.

**What `--strict` refuses** (all `R4`, all collected, with the rest of the
pre-flight's refusals, into one `Decline` — nothing is touched):

* `ProfileDrifted` — substantive drift outside `volatile`. The refusal names
  the recorded manifest, the count and the first five paths, and says to
  `ricepilot verify <profile>` and switch without `--strict` to take the
  profile as it is.
* `ProfileNotCompared` — nothing recorded yet, a recorded manifest that does
  not parse, or a tree that could not be walked. A check that could not be
  made is not a check that passed (R4, D55); a switch without `--strict`
  records the manifest, and the refusal says so.
* `VerifyConfigNotChecked` — the sandboxed verify-config did not check the
  Hyprland config the switch would link: a `hyprland.lua`, which is never run
  (`NOT-POSSIBLE.md#verify-lua-config`), or no `Hyprland` installed (D55). A
  tree with no Hyprland config at all ships nothing to check and is not
  refused. `--strict` into a Lua profile therefore always refuses, which is
  the honest answer: it was never checked, and never will be.

Refusals are decided in `plan()` from `PlanContext::strict` and the two
facts, so the decline is the same value in a dry run and under `--commit`. A
no-op switch under `--strict` into a drifted profile is declined like any
other refusal on a no-op (a missing `requires` already was).

**`rollback` is never strict.** It reports drift in the profile it returns
to, and goes ahead. It is the way back, and it is the command someone runs
when the profile they are on is the problem; a flag that only ever adds ways
for it to refuse would make it harder to take at exactly that moment. `plan`
does not grow the flag either; it has always been the non-strict pre-flight.

**Tested** end to end: drift reported and gone ahead past; a recorded
manifest that does not parse reported and gone ahead past; the same drift
refused under `--strict` with the live links, the generation and the attic
unchanged; a volatile file rewritten and a file rewritten with its own
content passing `--strict`; nothing recorded refused; a `hyprland.lua`
refused (never run: it is declined before anything is looked for); a
rollback reporting drift and going ahead. Every refusal's wording is
snapshotted, including the two no fixture here reaches (no `Hyprland`; an
unparseable recorded manifest).

## D60 — `diff`: a profile's links and its tree against the live filesystem, read-only by construction, exit 6 on a difference and 4 when it could not look

*M5.* "A profile against the live filesystem" had to be given a meaning.
For a `dir-link` profile the live filesystem holds two things of the
profile's, and `diff` compares both.

**The links.** Each `[[path]]` a switch would act on (`dir-link`,
`activation = "relogin"`) is classified by the ownership predicate itself —
`observe::shape_via`, the classification half of `observe_one`, now reading
through a `Look` so that the predicate `diff` reports is the one a switch
acts on — and set beside what the profile puts there: ricepilot's link, all
three facts holding, to `<root>/<src>`, with a directory at `<src>`. That is
`same`; anything else is `DIFFERS`, and the row says what is there instead:
another profile's link; a link ricepilot did not make, *even one pointing
exactly at the profile's source* — the by-reference caelestia clone before
`init` looks like that, and ownership is part of the topology because a
switch refuses such a link; a link whose ledger row names another target or
inode; ricepilot's own link into a root no registered profile has any more
(fact 2); a real directory; a file; nothing. A source that is missing or is
not a directory makes its row differ even with the right link in place,
since that link dangles (D42). Every link the ledger owns that the profile
does not link — does not declare, or declares as something never linked —
is a row too: switching to the profile would retire it (D36); a ledger row with nothing at its destination is no difference (a
switch would leave nothing there as well) and is `doctor`'s to report.
`generated` and `volatile` classifications, and a `dir-link` with
`activation = "never"`, are listed as never linked and not compared.

A real directory where the link should be — what caelestia's installer
leaves — is also compared path by path with the profile's source for that
destination, through the same walk and `verify::compare`, with `volatile`
anchored where the source sits in the profile (`verify::build_within_via`),
so the directory's `fish_variables` is left out exactly as the profile's
is. `doctor` prints `diff -r` for a user to run; this is that, read-only,
with the same exclusions as everything else.

**The content.** The profile's tree from its root — for a by-reference
profile, the external directory it names — against
`state/manifests/<name>.toml`. That comparison is
`verify::against_record_via`, and `switch`'s drift report (D59) now calls
the same function, so the two cannot disagree about what drifted or why a
tree was not compared. Nothing recorded, a record that does not parse or
cannot be read, and a tree that cannot be walked are each reported as NOT
compared, with the reason. A path rewritten with identical content is
counted and is not drift (D34).

`volatile` paths are shown apart and never as drift: the walk now returns
the paths the globs kept it out of (the top of each excluded subtree), and
`diff` lists them under their own heading. They are excluded from the
record as well, so there is no baseline to compare them with — by
declaration an application rewrites them (D59) — and the one true statement
is that they are there and were not compared. When the record was taken
with other `volatile` globs, or of another root, than the manifest now
declares, a line says so beside the comparison, since a path in one list
and not the other shows up as added or gone.

What `diff` is not: not a plan (no ops, no refusals of a switch — the
verdict points at `ricepilot plan <profile>` when a link differs, rather
than restating the five-shape table); not `doctor` (no hazards, no
commands); and it never follows a foreign link to compare what is behind
it.

**Read-only by construction**, as `doctor` is (D57). `diff::compare` takes
a `&dyn Look`, and what it needed from other modules was re-pointed at a
`Look` rather than copied: `cli::paths::load_via` / `load_all_via`,
`ledger::load_via`, `verify::load_via`, `switch::source_facts_via`,
`observe::shape_via`, `verify::build_within_via`,
`verify::against_record_via`. The old entry points call them with `Live`
and behave as before: no `switch`, `--strict` or `doctor` snapshot moved.
`diff` takes no lock (a
test runs it with the lock held) and starts no process, not even the two
read-only ones `Look` offers. `tests/diff.rs` checks the source text the way
`tests/doctor.rs` does — every `crate::` path in `src/diff.rs` and
`src/diff/` on a list of pure items or `*_via` readers, no grouped import,
and no word naming a write, a lock, a process or a read around `Look`
(`pacman`, `sh_syntax`, `hyprverify` and `observe_one` included), prose
too — and a planted violation shows the check bites. Every `diff` run in
the suite, refusals included, takes the whole fixture's
`(dev, ino, mtime_ns)` before and after and fails on any difference; that
helper and the source scanner moved into `tests/common` so `doctor`'s
tests and these share one copy.

**Exit status: 6 when something differs, like `verify`.** `Drift` (6) when
a link or a path differs. It is `verify`'s code because it is `verify`'s
answer — "what you asked about is not what it should be" — and `diff`
contains `verify`'s check; a script can act on it without parsing the
report, and it is not an error, for D34's reason: the command did its job.
A separate code for "only the links differ" was rejected: the report says
which, and one code meaning "differs" is easier to use correctly than two.

**4 when it could not look.** `Failed` (4) when the content could not be
compared because something could not be read — a record that does not
parse, a tree that cannot be walked. The report is still printed on stdout
with the links, but 0 would claim a match that was not seen and 6 a drift
that was not seen; 4 takes precedence over 6 for that reason.

**0 otherwise — including when nothing has been recorded yet.** Drift is
defined against a record (D34); with none there is nothing to drift from.
The report says the content was NOT compared and the last line says the
answer is about the links alone. This is the state right after `init`,
which records the ledger and no tree manifest, and a status that could
never be 0 there would be one nobody reads. A script that needs the content
guarantee runs `verify`, which refuses (2) when nothing is recorded. A real
directory whose comparison with its source could not be read does not
change the status: its row already differs.

**Refusals are `plan`'s**, in the same words, because they come from the
same loaders: a profile that is not registered or is not a name, a manifest
that is not valid — the profile's own, or another registered profile's,
since every registered root is fact 2 of the predicate for every link — a
ledger that does not parse (fact 3), and a destination that cannot be
classified (a symlinked intermediate component, D9). A manifest that does
not parse used to be reported as profile `<unparsed>`, because
`manifest::parse` sees text rather than a file; `paths::load_via` now names
the profile and the file, which matters most when the refusal is caused by
a profile other than the one asked about.

`status`, which still said drift reporting was "not implemented yet", now
names `diff` and `verify`.

What is not covered, recorded rather than implied: with no lock, a `diff`
beside a running switch can see half of it — the in-flight journal is noted
first when there is one, but a switch that starts after that line is not
seen; the rows are read one at a time, not as one snapshot; the content is
the whole profile tree, as `verify`'s is, not only the `src` directories
linked; the walk hashes every file, so a large by-reference tree costs what
`verify` costs; and a real directory's comparison hashes both sides.
