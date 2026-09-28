# RECOVERY — when the login is broken

This is for someone whose desktop did not come back after a ricepilot switch,
or came back wrong, possibly looking at a text console with no graphical
session. Read it top to bottom and stop at the first step that fixes things:
the steps are ordered from the ones that change nothing to the ones that
change the most.

Four things are true however bad it looks:

- **A switch deletes nothing.** Nor does a rollback, an adopt or a
  recover: ricepilot moves what it displaces into
  `~/.local/state/ricepilot/attic/`. Only `ricepilot gc --commit` removes
  anything, and only entries whose names are typed back to it. **Do not run
  `gc --commit` while recovering.**
- **Nothing here needs `rm`.** Every command below moves or links. If you
  find yourself about to delete something, move it aside instead
  (`mv -nT <path> <path>.set-aside`).
- **Every `mv` here has `-n`: it never replaces anything.** If the name it
  moves to is already taken, it does nothing, and says nothing. Check with
  `ls -ld <path> <path>.set-aside` afterwards; if the old one is still there,
  pick another name (`<path>.set-aside-2`). `doctor` picks a free one for you.
- **Every ricepilot command that can change something is a dry run unless
  you add `--commit`.** Run it without first, read what it would do, then add
  `--commit`.

In the commands, `~` is your home directory, and `you` in a path is your user
name. The paths are ricepilot's defaults; if you set `RICEPILOT_STATE_DIR` or
the other overrides, use yours.

## 1. Get somewhere to type

None of this changes a file.

**The session came up, but wrong** (no bar, wrong colours, odd keys): if you
can open a terminal, use it, and go to [step 2](#2-look-first-changes-nothing).

**Hyprland crashed at login.** The login screen's Hyprland sessions start
Hyprland through `start-hyprland`, which relaunches it in **safe mode** after
a crash: Hyprland says your last session crashed, and your config is not
loaded. That is Hyprland protecting you from the config, not ricepilot; the
files are unchanged. You can troubleshoot from there if it gives you a
terminal, or go to a text console.

**A text console (TTY).** Press **Ctrl+Alt+F2** (F3 to F6 work too). At the
`login:` prompt type your user name, then your password; nothing is echoed
while you type the password. You get your normal login shell. If that is
fish, and fish's config is one of the directories your rice links, the
prompt may look odd or print errors — that is fish reading the same config.
Every command in this document works from any shell; type `sh` for a plain
one if fish gets in the way. The graphical screen is usually back on
**Ctrl+Alt+F1**.

ricepilot's commands that change anything take a lock in
`$XDG_RUNTIME_DIR`, which a console login normally sets. Check:

```sh
echo $XDG_RUNTIME_DIR        # expect /run/user/<your uid>
```

If it prints nothing, ricepilot refuses with "neither RICEPILOT_RUNTIME_DIR
nor XDG_RUNTIME_DIR is set". Set it to the same directory your graphical
session uses — a different directory would mean a different lock:

```sh
export XDG_RUNTIME_DIR=/run/user/$(id -u)
```

**A graphical session without your config.** From a TTY, `Hyprland
--safe-mode` starts Hyprland in safe mode directly, config not loaded. It is
a plain Hyprland, not a session uwsm manages; the fixes below are all
commands you can type at the TTY instead.

## 2. Look first (changes nothing)

```sh
ricepilot doctor
```

`doctor` only reads. It lists **problems** first — each with its path, the
rule it breaks, and the exact commands to fix it — then hazards about the
machine, then one `ok:` line for everything healthy. It runs none of the
commands it prints. It exits `7` when there is a problem and `0` when there
is none.

Also read-only:

```sh
ricepilot status           # which links ricepilot owns, and the current generation
ricepilot rescue           # where rescue.sh is, and how to run it
ricepilot diff <profile>   # a profile's links and tree against what is on disk
```

Why the session did not start is usually in one of these, also read-only:

```sh
journalctl --user -b -u 'wayland-wm@*'           # the compositor uwsm ran, this boot
less ~/.local/share/sddm/wayland-session.log     # what the login screen's session printed
```

What `doctor` says decides the next step:

| `doctor` says | go to |
|---|---|
| "an operation was interrupted and has not been recovered" | [step 3](#3-finish-an-interrupted-operation) |
| nothing is wrong with ricepilot's links, but the profile you switched to is the problem | [step 4](#4-go-back-one-generation) |
| "is empty, and the directory you adopted from here is in the attic" | [step 6](#6-put-something-back-from-the-attic-by-hand) |
| a link that is now a real directory, or points somewhere else | the commands it prints under that problem |
| `ricepilot` itself will not run | [step 5](#5-without-ricepilot-rescuesh) |

If a command exits `5` ("another ricepilot holds …/ricepilot.lock"), a
ricepilot is still running — perhaps one waiting at a question in the
session you left. `pgrep -a ricepilot` shows it. The lock is released when
that process exits, however it exits, so a lock file left behind never
blocks anything by itself.

## 3. Finish an interrupted operation

If a switch, rollback or adopt was cut off part way — a crash, a power cut,
a killed terminal — `~/.local/state/ricepilot/journal/current.toml` is
still there, and `switch`, `rollback`, `adopt` and `gc` refuse until it is
dealt with.

```sh
ricepilot recover            # what it would do; changes nothing
ricepilot recover --commit
```

`recover` looks at every destination the journal names and decides once, for
the whole operation: if any destination already has the new link, it
finishes the switch; if none does, it abandons it and moves the staged links
to the attic. It never leaves half of each. When it finishes a switch, it
also records what the switch had not got to — the new generation, the ledger
rows that make the new links ricepilot's, and `rescue.sh` (D77). It then
retires the journal.
Afterwards, run `ricepilot doctor` again.

Use `recover` for this, not `rescue.sh`: the rescue script was written by the
switch *before* the interrupted one and knows nothing about it.

## 4. Go back one generation

Every completed switch and rollback is a numbered *generation*.
`rollback` re-applies the one before the current one, through the same
checked, journalled path a switch takes.

```sh
ricepilot rollback           # the plan; changes no link and no config
ricepilot rollback --commit
```

Without `--commit` the rollback moves, links and removes nothing, but it is
not entirely without effect. If the generation it would go back to ships a
`hyprland.conf`, it writes a copy of that config, `exec` lines stripped,
under `~/.local/state/ricepilot/verify/`, and runs `Hyprland --verify-config`
on the copy — a separate Hyprland process with a scratch home, an empty
runtime directory and none of your session's variables, so it cannot reach
the running session (D55). The copy is kept until `ricepilot gc` removes
it. (`recover` without `--commit` does not
do this.)

Then log in again ([step 7](#7-log-in-again)).

- **Run it once.** A rollback is itself a new generation — history is kept,
  not rewound — so a second `rollback --commit` goes back to the profile you
  just left.
- **It does not undo an `adopt`.** If the generation you are leaving is one
  where you adopted a directory, rolling back moves the link away and leaves
  that path empty; your original directory stays in the attic. `doctor`
  reports this; [step 6](#6-put-something-back-from-the-attic-by-hand) puts
  it back.
- **It can refuse.** If a link at a destination is not the one ricepilot
  recorded — an installer replaced it, or you ran `rescue.sh` — the plan says
  which and why, and nothing is changed. `doctor` prints what to do about each
  one.
- Generation `0000` is the machine as ricepilot found it before its first
  switch. There is nothing before it to roll back to.

## 5. Without ricepilot: `rescue.sh`

For when `ricepilot` will not run, or you would rather not use it:

```sh
sh ~/.local/state/ricepilot/rescue.sh
```

It is a plain POSIX shell script ricepilot writes after every completed
switch and rollback. It restores the generation **before** the current one
— the one `rollback` would go to — using only `test`, `ln`, `mv` and
`mkdir`, each named by its absolute path: no ricepilot, no D-Bus, no
`hyprctl`, no fish, no `PATH`. It removes nothing: where the older
generation recorded that nothing was there, the link there now is moved into
`~/.local/state/ricepilot/attic/rescue-NNNN/`. And it only ever replaces a
**link**: before each step it checks that what is at the path is a link, or
nothing, and otherwise leaves it alone. The top of the file says which
generation it restores and what profile that was, and a comment line per
destination says what it will do there:

```sh
head -n 3 ~/.local/state/ricepilot/rescue.sh
grep '^# /' ~/.local/state/ricepilot/rescue.sh
```

It prints one line per destination — `ok`, `SKIPPED` or `FAILED` — and
carries on past a problem, so one path it cannot fix does not cost you the
others. It exits 0 only if every line is `ok`; either way, **read the
output.** Running it a second time is safe: a path already restored is
linked again to the same place, and one already empty says so.

- **`SKIPPED … it is not a link now`**: something — usually a rice
  installer, or you — put a real file or directory where ricepilot's link
  was, and the script will not put a link over it. It changes nothing there.
  To finish by hand, move what is there aside, then run the script again:

  ```sh
  mv -nT ~/.config/foot ~/.config/foot.set-aside
  ls -ld ~/.config/foot          # must say "No such file or directory"
  sh ~/.local/state/ricepilot/rescue.sh
  ```

  (`foot` is an example; use the path from the `SKIPPED` line.)

- **`SKIPPED … is taken`**: a path the older generation had nothing at is a
  link again, and the place in `attic/rescue-NNNN/` the script would move it
  to already holds what an earlier run moved there. It moves nothing rather
  than replace that. Move the link aside with `mv -nT` as above.

- **`FAILED`**: the check passed but `ln` or `mv` did not — their own error
  message is printed just above. If it was the `mv`, the link the script
  made is left beside the path as `<path>.rp-rescue`, and later runs fail at
  the same step until it is gone; move it aside with `mv -nT` too.

- **After a rollback it points the other way.** The script restores the
  generation before the current one, and a rollback is a generation: after a
  `rollback --commit` it restores the generation you rolled back *from*. The
  header of the file says which.

- **It only touches the destinations the older generation recorded.** A
  link that only the current profile has — a directory it links and the
  older one never did — is not in the script, and is left pointing into the
  current profile. `rollback` would move such a link to the attic; the
  script does not. `ricepilot status` lists every link ricepilot owns;
  any of them missing from the `grep` above is one of these. Without
  ricepilot, `ls -l ~/.config` shows which links point into the profile you
  are leaving. Move each one aside yourself if it is part of what broke:

  ```sh
  mv -nT ~/.config/btop ~/.config/btop.set-aside
  ```

- **After an interrupted operation, use [step 3](#3-finish-an-interrupted-operation).**

**Afterwards, ricepilot's records are out of step.** The script restored the
links without ricepilot, so ricepilot sees them as links it did not make:
`doctor` reports each as pointing "somewhere other than where ricepilot
linked it" (or, for one the script parked, as "gone"), says it is where the
older generation had it, and prints the commands below for it; `switch` and
`rollback` refuse them until then. Once you are logged in
and things work, bring the records back in line — move each restored link
aside, then let ricepilot make it again:

```sh
mv -nT ~/.config/foot ~/.config/foot.set-aside   # once per path doctor names
ricepilot rollback                               # the plan: it should create each link
ricepilot rollback --commit
ricepilot doctor                                 # expect no problems
```

The `rollback` here re-applies the same generation the script restored, so
it creates the same links, this time recorded; a link you set aside because
only the newer profile had it is reported as no longer managed, and nothing
is put back there. The `.set-aside` links are ordinary symlinks; they are
harmless, and you can leave them. A second round of this needs a second
name — `~/.config/foot.set-aside` is taken by the first — which `doctor`
prints as `~/.config/foot.set-aside-2`; the `-n` makes a reused name a move
that does not happen rather than one that replaces the first.

## 6. Put something back from the attic by hand

The attic is `~/.local/state/ricepilot/attic/`. Each entry is named for the
operation that filled it — a UTC timestamp such as `20260927T164327Z`,
sometimes with `-1`, `-2` added — or `rescue-NNNN` for what `rescue.sh`
displaced. Inside an entry, each thing sits at its **original absolute
path** with the leading `/` removed. So the `~/.config/hypr` that an adopt
moved away is at:

```
~/.local/state/ricepilot/attic/<id>/home/you/.config/hypr
```

Most attic entries hold only old symlinks, which `rollback` recreates
properly; do not move those back by hand. What you may want by hand is a
**real directory**: the one `adopt` moved there (`rollback` does not bring
it back, D49), or one `rescue.sh` displaced.

To find it, any of these (all read-only):

```sh
ricepilot doctor     # in the adopt-then-rollback case, prints the exact mv -nT
ricepilot gc         # without --commit: lists every entry, what it holds, and why it is kept
ls ~/.local/state/ricepilot/attic/
ls -la ~/.local/state/ricepilot/attic/*/home/*/.config/
```

To put it back:

```sh
ls -ld ~/.config/hypr                   # must say "No such file or directory"
mv -nT ~/.local/state/ricepilot/attic/<id>/home/you/.config/hypr ~/.config/hypr
```

If something is already at the destination, move that aside first
(`mv -nT ~/.config/hypr ~/.config/hypr.set-aside`) — never remove it. Plain
`mv -T` would put a file or a link over a file or a link that is already
there, and a directory over an empty one; `-n` stops that, so a mistyped or
reused name moves nothing instead of replacing something. Keep both on the same
filesystem (everything under your home is): across filesystems `mv` copies
and deletes instead of renaming.

A directory you moved back is a real directory again, not a link ricepilot
owns. If a profile still declares that path, a later `switch` into it
refuses the path rather than linking over your directory — the safe way
round — and `doctor` says so.

## 7. Log in again

A switch takes effect at the next login, so every fix above ends with
leaving the session and logging in.

**From inside the graphical session**, in a terminal:

```sh
uwsm stop
```

This is the clean logout: every window closes (anything unsaved in them is
lost) and the login screen comes back. `ricepilot switch --relogin` and
`rollback --relogin` offer to run exactly this after their checks, and never
anything else.

**From a TTY**, `uwsm stop` should work too: it asks your systemd user
manager, which every login of yours shares, to stop the compositor it runs.
If it says there is nothing to stop, or the graphical session does not go
away, end that login session through logind instead:

```sh
loginctl list-sessions                         # your sessions, with their TTYs
loginctl show-session <ID> -p Type -p TTY      # Type=wayland is the graphical one
loginctl terminate-session <ID>
```

Pick the graphical session, not the one you are typing in (`tty` prints
yours).

**Never** `hyprctl dispatch exit`, and never `kill` or `pkill` Hyprland: uwsm
has to stop the units it started, in order, and a compositor that disappears
under it leaves them behind.

**At the login screen**, the session menu has two Hyprland entries:

- **Hyprland (uwsm-managed)** — the normal one, `uwsm start … hyprland.desktop`.
- **Hyprland** — the plain entry, which runs `/usr/bin/start-hyprland`
  directly. It does not go through uwsm, so nothing in `~/.config/uwsm`
  (`env`, `env-hyprland`) is read — use it when the uwsm layer is what broke.
  Hyprland itself still reads `~/.config/hypr`; if that is the broken part,
  `start-hyprland` falls back to safe mode after the crash, as in
  [step 1](#1-get-somewhere-to-type). `uwsm stop` has nothing to stop in a
  session started this way; `loginctl terminate-session`, as above, ends
  it.

With SDDM's autologin and `Relogin=false`, ending the session returns you to
the login screen rather than straight back into the same session.

## What has and has not been tried

Tried, in the test suite, on throwaway trees under `target/fixtures/`:
`recover` after a crash at every step of a switch and an adopt; `rollback`;
`rescue.sh` run by a real `/bin/sh`, including a path it cannot restore;
`doctor`'s report of each problem above, including the adopt-then-rollback
state and the `mv -nT` it prints. Run by hand in the same kind of sandbox
while this document was written: step 5's whole sequence — `rescue.sh`,
setting the restored links aside, `rollback --commit`, a clean `doctor` —
with and without a link only the newer profile had, and `rescue.sh`'s
`FAILED` on a real directory and on a second run (before D73 made those
`SKIPPED` and `ok`; the script as it is now has been run by the test suite
only).

Not tried, because it would log a real user out or needs a real login: that
`uwsm stop` ends the session, from the session or from a TTY;
`loginctl terminate-session` on a uwsm session; Hyprland's safe mode and
`start-hyprland`'s relaunch (their behaviour here is from Hyprland 0.55.4's
own messages, not from a test); SDDM returning to its greeter; the two log
locations in step 2 (from uwsm's unit name as observed on the target machine
and SDDM's default configuration). ricepilot has not yet been run against a
real home directory at all.
