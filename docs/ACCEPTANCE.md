# ACCEPTANCE — the sandbox acceptance run (M5 task F)

This is the result of running ricepilot's whole dry-run → `--commit` path in
a disposable Docker container, against a synthetic rice shaped like the
target machine's (AGENT_PROMPT §1: 7 directory links + 2 file links into a
caelestia git clone that has modified and untracked files). It was **not**
run on a real machine, a real `$HOME`, a real Hyprland, `hyprctl` or `uwsm`.

- **Harness:** `packaging/acceptance/` (`run.sh` runs on the host, and
  `steps.sh`, `lock-check.sh` and `setup-rice.sh` run in the container).
  Re-run it with `sh packaging/acceptance/run.sh`. Full logs go to
  `target/acceptance/<UTC timestamp>/` and are not committed: every step's
  command, stdout, stderr and exit code, the `results.tsv`, and the stand-ins'
  call log.
- **Run recorded here:** 2026-09-28T17:33Z. ricepilot at `f06c899`, built
  from `git archive HEAD` inside the image with `cargo build --release
  --locked --frozen`. Base image
  `archlinux:latest@sha256:f3691b4dde62ba4c4b6f0ae2c1fbf28e8c0c8c4b9a35c7e06dc1f70e21aa29f6`,
  rustc 1.98.1, dash 0.5.13.4, busybox 1.36.1, bash 5.3.20, strace 7.2,
  host kernel 7.0.12.
- **Isolation:** the network is used only by `docker build` (pacman,
  `cargo fetch`). The run itself is `docker run --network none --cap-drop
  ALL --security-opt no-new-privileges --user rice`, with no bind mount and
  no `--privileged`. Logs come out through `docker cp`. The container has
  two users: `rice` (uid 1000) runs every step, and `rice2` (uid 1001) runs
  the D74 lock-owner checks.
- **Stand-ins:** `/usr/local/bin/{Hyprland,hyprctl,uwsm}` are logging
  fakes (`packaging/acceptance/fakes/`). The image has no real ones.
  `Hyprland --verify-config` parses nothing: it records its argv and
  environment and flags any `exec` line that reached it. `uwsm` records the
  argv and environment it was given and stops nothing.
- **Result:** every step PASS except one SKIPPED: a real logout and login
  (see below). Before this run the harness found **five real ricepilot
  bugs**, now fixed with tests and decisions **D77–D81**. The table below is
  the run after those fixes.

## Bugs the run found (fixed before the recorded run)

| D | what the run showed | fix |
|---|---|---|
| D77 | SIGKILL during `switch --commit`, then `recover --commit`: the links were always fully old or fully new, but after a forward recovery the next `switch` refused them as **foreign**. recover never wrote the generation, the ledger, the tree manifest or `rescue.sh`. | A forward `recover` now settles the records before it retires the journal, and it does so idempotently. |
| D78 | `gc` listed three of ricepilot's own verify-config copies (`<stamp>-N-M`) as "not a name ricepilot gives", so they were kept for ever. | A verify entry may carry the sandbox's extra `-M`. |
| D79 | After `init` → `switch bare` → `rollback`, the M5 gate's `verify caelestia` refused: no manifest had ever been recorded for the rice the machine was already on. | `init --commit` records the registered tree's blake3 manifest. |
| D80 | After a clean `adopt`, `doctor`, `verify` and `switch --strict` blamed "an app, a script or an editor" for adopt's own writes, and `profile.toml` went from mode 0644 to 0600. | adopt keeps `profile.toml`'s mode and re-records the manifest for its own paths only. |
| D81 | After `rescue.sh` (under all three shells), doctor told the user to `switch` back to the profile they had just rescued the machine from. It also did not name the retired links the script put back, so the documented rollback refused them. | doctor recognises what rescue.sh restored and prints RECOVERY.md's set-aside + rollback. Following those commands to the letter now ends with a healthy doctor, and the table shows it. |

The first harness runs also had harness bugs, fixed in the harness rather
than in ricepilot: the D56 step's sandbox root was the home itself (the
root must be strictly above it); the "fully new" expectation forgot that
`bare` retires four caelestia links; the lock holder was a `gc` with nothing
to ask; and a check assumed an adopted directory is never a gc candidate
(D61 allows it when a byte-identical copy is in the profile).

## Main path

Exit codes: 0 ok, 2 refused, 5 locked, 6 drift, 7 unhealthy (README).
"Result" is PASS only when the exit code was one expected and every check
after the step passed. Checks are separated by `;`.

| # | step | what | exit | result | checks |
|---|---|---|---|---|---|
| 1 | `setup` | build the synthetic rice (setup-rice.sh) | 0 | PASS | 7 dir links + 2 file links, into the clone; the clone has modified and untracked files |
| 2 | `doctor-1` | doctor on the untouched rice (read-only) | 0 | PASS | doctor wrote nothing: no data or state directory |
| 3 | `init-dry` | init (dry run) | 0 | PASS | dry run registered nothing |
| 4 | `init-commit` | init --commit, yes to every question over stdin | 0 | PASS | profile caelestia registered by reference; baseline copy taken; init moved no link; uwsm link reported, not offered (denylist) |
| 5 | `status-1` | status after init | 0 | PASS |  |
| 6 | `show-caelestia` | show caelestia | 0 | PASS |  |
| 7 | `show-bare` | create a minimal `bare` profile (hypr + fish) by hand, then show bare | 0 | PASS |  |
| 8 | `list` | list | 0 | PASS |  |
| 9 | `plan-bare` | plan bare | 0 | PASS |  |
| 10 | `switch-dry` | switch bare (dry run) | 0 | PASS | dry run moved no link; verify-config ran the fake Hyprland; no exec line reached Hyprland (D55) |
| 11 | `switch-commit-relogin` | switch bare --commit --relogin, outside any session | 0 | PASS | hypr now points into bare; fish now points into bare; file links untouched; relogin declined; uwsm never called; rescue.sh written and parses under dash |
| 12 | `verify-bare` | verify bare | 0 | PASS |  |
| 13 | `status-2` | status after switch | 0 | PASS |  |
| 14 | `rollback-relogin-yes` | rollback --commit --relogin in a fake uwsm session, answered yes (stand-in uwsm) | 0 | PASS | every --relogin check passed; the stand-in uwsm was run exactly once; argv was exactly `stop` |
| 15 | `switch-relogin-no` | switch bare --commit --relogin in the fake session, answered no | 0 | PASS | the y/N question was asked; answered no: uwsm not run again |
| 16 | `relogin-sandbox-d56` | switch plain --commit --relogin inside RICEPILOT_SANDBOX, fake session, answered yes | 2 | PASS | the switch was applied and the logout question was asked; uwsm refused by the sandbox, never run (D56); the refusal names uwsm and the sandbox |
| 17 | `real-logout-login` | no real uwsm/Hyprland/SDDM in the container; the real `uwsm stop`, logout and login are left for the user's machine (the stand-in shows the argv/env it would get) | — | SKIPPED |  |
| 18 | `rollback-commit` | rollback --commit (back to the links init found) | 0 | PASS | all 7 dir links and both file links as before (readlink) |
| 19 | `verify-caelestia` | verify caelestia (clean; the manifest init recorded, D79) | 0 | PASS | git status --porcelain identical to before; clone byte-identical: types, modes, sizes, mtimes, link targets, sha256 |
| 20 | `rescue-cmd` | rescue (prints where rescue.sh is) | 0 | PASS |  |
| 21 | `strict-volatile` | switch bare --commit --strict, only a volatile file changed since recorded: goes ahead | 0 | PASS | hypr now points into bare |
| 22 | `strict-rollback` | rollback --commit | 0 | PASS | links as before |
| 23 | `strict-drift` | switch bare --commit --strict after an edit outside volatile: refused | 2 | PASS | no link moved; the refusal names the changed file |
| 24 | `nonstrict-drift` | switch bare (dry run, no --strict): the same drift is reported, not refused | 0 | PASS | reported, not refused |
| 25 | `adopt-by-ref` | adopt kitty into caelestia (by reference) is refused, R3 | 2 | PASS | kitty still a real directory |
| 26 | `adopt-dry` | adopt ~/.config/kitty --into bare (dry run) | 0 | PASS | dry run: kitty still a real directory |
| 27 | `adopt-commit` | adopt ~/.config/kitty --into bare --commit, answered yes | 0 | PASS | kitty is now a link into bare; the original kitty directory is in the attic, content intact; the link resolves to the same content |
| 28 | `capture-dry` | capture nvimrice --from ~/.config/nvim (dry run) | 0 | PASS | dry run: no profile made |
| 29 | `capture-commit` | capture nvimrice --from ~/.config/nvim --commit | 0 | PASS | the profile holds a copy with the same content; ~/.config/nvim is untouched: still a real directory, same content |
| 30 | `verify-nvimrice` | verify nvimrice (capture recorded its manifest) | 0 | PASS |  |
| 31 | `diff-bare` | diff bare | 6 | PASS |  |
| 32 | `diff-caelestia` | diff caelestia | 6 | PASS |  |
| 33 | `doctor-2` | doctor after the run | 7 | PASS |  |
| 34 | `gc-dry` | gc (dry run) | 0 | PASS | dry run removed nothing; every verify copy is recognised as ricepilot's (D78); the adopted kitty original is offered only because bare holds an identical copy (D61) |
| 35 | `gc-wrong` | gc --commit, a WRONG name typed for every entry | 0 | PASS | wrong names removed nothing; at least one candidate was offered |
| 36 | `lock-busy` | a second ricepilot while one holds the lock (gc waiting for a name) | 5 | PASS | the holder was really waiting at a question; the holder, given end of input, removed nothing |
| 37 | `gc-right` | gc --commit, the right name typed for every candidate | 0 | PASS | exactly the typed entries are gone; kitty still resolves to its original content; links unchanged by gc |
| 38 | `doctor-3` | doctor after gc | 7 | PASS |  |
| 113 | `rescue-dash` | rescue.sh under dash restores the previous generation | 0 | PASS | the switch had changed the links; links are the previous generation's again; the displaced links went to the attic, not deleted |
| 114 | `rescue-dash-doctor` | doctor after rescue.sh (dash): the restored links are out of step with the ledger | 7 | PASS | it advises set-aside + rollback, never a switch back to bare (D81) |
| 115 | `rescue-dash-rollback` | the rollback --commit doctor printed | 0 | PASS | links are still the previous generation's |
| 116 | `rescue-dash-doctor-after` | doctor after following its advice: healthy | 0 | PASS |  |
| 117 | `rescue-busybox-sh` | rescue.sh under busybox sh restores the previous generation | 0 | PASS | the switch had changed the links; links are the previous generation's again; the displaced links went to the attic, not deleted |
| 118 | `rescue-busybox-sh-doctor` | doctor after rescue.sh (busybox sh): the restored links are out of step with the ledger | 7 | PASS | it advises set-aside + rollback, never a switch back to bare (D81) |
| 119 | `rescue-busybox-sh-rollback` | the rollback --commit doctor printed | 0 | PASS | links are still the previous generation's |
| 120 | `rescue-busybox-sh-doctor-after` | doctor after following its advice: healthy | 0 | PASS |  |
| 121 | `rescue-bash` | rescue.sh under bash restores the previous generation | 0 | PASS | the switch had changed the links; links are the previous generation's again; the displaced links went to the attic, not deleted |
| 122 | `rescue-bash-doctor` | doctor after rescue.sh (bash): the restored links are out of step with the ledger | 7 | PASS | it advises set-aside + rollback, never a switch back to bare (D81) |
| 123 | `rescue-bash-rollback` | the rollback --commit doctor printed | 0 | PASS | links are still the previous generation's |
| 124 | `rescue-bash-doctor-after` | doctor after following its advice: healthy | 0 | PASS |  |
| 125 | `lock-create` | rice takes the lock in /srv/shared-runtime (for the D74 check by rice2) | 0 | PASS | the lock file is rice's |

Notes on the non-zero exits that are expected:

- **16** `2`: inside `RICEPILOT_SANDBOX` the switch is applied and the y/N
  question is asked and answered yes, and then `exec` refuses `uwsm` (D56).
- **31–32** `diff` exits 6. For `bare`, the links are caelestia's again
  after the rollback, and there is the deliberate `hyprland.conf` edit from
  step 23. For `caelestia`, `kitty` was adopted into `bare`.
- **33, 38** `doctor` exits 7 for one problem, the deliberate
  out-of-`volatile` edit to `bare/hypr/hyprland.conf` from step 23. The
  hazards (the fake `hypr-session`, caelestia-cli's migration path, no
  snapper) do not set 7.
- **114, 118, 122** `doctor` exits 7 after `rescue.sh`, as README and
  RECOVERY.md step 5 document. **116, 120, 124** are doctor again after its
  own printed commands were run, and it exits 0.

## Crash injection: SIGKILL at an exact syscall, then `recover --commit`

For every *k*, a fresh synthetic home (setup + `init --commit` + `bare`).
The real binary is killed with `strace -e inject=<syscall>:signal=KILL:when=k`
during `switch bare --commit`, or during `adopt ~/.config/kitty --into bare
--commit`. Then `recover --commit` runs. Per case, the checks are:

- **switch:** every destination fully old or fully new, never mixed; no
  journal left in flight; a `switch bare --commit` afterwards lands fully
  new (ricepilot still owns the links, D77); and a `rollback --commit`
  after that lands fully old.
- **adopt:** kitty is either still the real directory with its content, or
  a link into `bare` with the original, content intact, in the attic; no
  journal left; a finished adopt's link is listed by `status`.

"forward / backward / none" counts how each case's recovery went: finished
the operation, abandoned it, or found no journal because the kill came
before the journal was written.

| command | syscall | cases (*k* = 1…n) | forward / backward / none | checks passed | result |
|---|---|---|---|---|---|
| switch | `renameat2` | 3 | 1 / 1 / 1 | 15/15 | PASS |
| switch | `renameat` | 15 | 12 / 0 / 3 | 75/75 | PASS |
| switch | `symlinkat` | 4 | 0 / 2 / 2 | 20/20 | PASS |
| switch | `fsync` | 19 | 12 / 1 / 6 | 95/95 | PASS |
| adopt | `renameat2` | 2 | 0 / 1 / 1 | 8/8 | PASS |
| adopt | `renameat` | 9 | 6 / 0 / 3 | 36/36 | PASS |
| adopt | `symlinkat` | 3 | 0 / 1 / 2 | 12/12 | PASS |
| adopt | `fsync` | 19 | 10 / 1 / 8 | 76/76 | PASS |

## D74: the lock file's owner, with a second uid (`rice2`)

`rice` takes the lock in a shared mode-1777 directory (step 125). Then
`rice2` runs `lock-check.sh`:

| # | step | what | exit | result | checks |
|---|---|---|---|---|---|
| 101 | `lock-foreign-owner` | rice2 against a lock file owned by rice (D74) | 2 | PASS | the refusal says the lock must be a file you own; the lock file is still rice's |
| 102 | `lock-symlink` | rice2, the lock's name is a symlink to a file that does not exist (D74) | 2 | PASS | the symlink's target was not created |
| 103 | `lock-fifo` | rice2, the lock's name is a fifo: refused, and no hang (D74) | 2 | PASS |  |
| 104 | `lock-own` | rice2 with its own runtime directory: the lock is taken | 0 | PASS | rice2's lock file is rice2's, mode 600 |


## Not exercised, and why

- **A real `uwsm stop`, logout and login (SKIPPED).** The container has no
  SDDM, Hyprland or uwsm. The stand-in shows exactly what the real one
  would get: argv `stop`, and an environment of `HOME`, `XDG_RUNTIME_DIR`,
  `DBUS_SESSION_BUS_ADDRESS`, `PATH=/usr/bin` and `LC_ALL=C`. The M5 gate's
  logins are left for the supervised run on the user's machine.
- **A real Hyprland parse.** The verify-config stand-in accepts every
  config. The run shows only that ricepilot runs it on a scratch copy with
  every `exec` line stripped (none reached it, D55).
- **A real TTY.** Every y/N and typed-name answer came over a pipe. The
  terminal widget of the confirmations is not driven.
- **btrfs, `EXDEV`, and a second mount for `gc`.** The container's
  filesystem is overlayfs, where everything is on one device.

## Remaining concerns (not fixed here)

- `doctor` calls the current generation's profile "the profile linked now".
  After an `adopt` into `bare` while every other link is caelestia's, it
  says that about `bare`, which is only true of `kitty`.
- A switch into a profile that does not declare a destination the ledger
  owns retires it into the attic: caelestia → bare moves btop, fastfetch,
  foot and spicetify away. This is by design (D36) and reversible, but it
  surprises you the first time.
- The M5 gate's `verify caelestia` clean on the real machine needs
  caelestia's own in-tree writers (`scheme/current.conf`, `btop/themes/*`
  and so on) to be declared `volatile`. Since D79, `doctor` also reports them
  as drift from the first run after `init`.
