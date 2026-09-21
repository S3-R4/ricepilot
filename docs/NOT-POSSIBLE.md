# NOT POSSIBLE — capabilities ricepilot declines, and why

"Not possible" is a first-class outcome ([SAFETY.md](SAFETY.md) R5). This file
is the complete catalogue. Every entry has an anchor that error messages link
to, so a refusal is always traceable to a written reason.

Each entry states what is declined, why, and what to do instead.

## live-apply

**Declined:** applying a profile switch to the running session.

A full rice spans Hyprland, a shell, a bar, a notification daemon, a theme
engine and a dozen apps that read config only at startup. Half of them can be
reloaded, half cannot, and the ones that cannot include the compositor's
input configuration. A partial live apply is exactly the "half-applied state"
R5 forbids.

**Instead:** a switch is an on-disk relink that takes effect at the next
login. `switch --relogin` will run `uwsm stop` after a y/N confirmation.
An opt-in `--live` for the quickshell layer only, with a dead-man
auto-rollback, is a v2 candidate.

## unowned-path

**Declined:** acting on any path that fails the three-part ownership
predicate in [SAFETY.md](SAFETY.md#the-ownership-predicate).

If ricepilot did not create a link, it cannot know what depends on it, and it
has no record of what the path looked like before. Overwriting it would be
unrecoverable in the only sense that matters: the user would not know what was
lost.

**Instead:** `adopt` the path explicitly, per path, with confirmation.

## real-dir-at-managed-dest

**Declined:** `switch` when a managed destination is a real directory rather
than our link.

This is the signature of an installer having run: caelestia's `install.fish`
and `caelestia-cli install|update` convert links back into real directories.
The directory's contents are whatever the installer wrote, and silently
re-linking would strand them.

**Instead:** `doctor` reports it; `adopt` it deliberately, or restore the link
by hand once you know what the directory contains.

## file-activation

**Declined:** activating individual files (`kind = "file-copy"`) in v1.

v1 activates directories only. File-level deployment needs a copy ledger and a
three-state divergence check (unchanged / changed-by-us / changed-by-them) to
avoid silently discarding a user edit. That is a design in its own right.

**Instead:** v1.1. In the meantime, file links that already exist are
observed and reported but not managed.

## deleting-anything

**Declined:** removing any file or directory outside `gc`, ever.

**Instead:** displaced objects are renamed into
`~/.local/state/ricepilot/attic/<ts>/`. `gc` reclaims the space later, after
itemising exactly what it would remove and requiring the operator to type the
attic directory's name back.

This reaches the user most visibly in `rollback`. A switch onto a destination
that was *absent* creates a link there with a single `symlinkat`, and undoing
that would mean making the path absent again — a removal. So the link is
**retired**: renamed into the attic, leaving the destination empty. The
observable effect is the one the user expected (nothing at that path), plus a
line saying where the link went. `rollback` names every retired path rather
than letting the user assume a deletion happened
([DECISIONS.md](DECISIONS.md) D36).

## cross-device

**Declined:** any operation that would need to rename across an `st_dev`
boundary.

`rename(2)` fails with `EXDEV`, and the "obvious" workaround — copy then
delete — is both a delete (R2) and non-atomic (R5). On the target machine `/`,
`/home`, `/tmp` and `/var/tmp` are all distinct devices.

**Instead:** everything ricepilot renames lives under `/home`. A mismatch is a
pre-flight refusal with both device numbers named.

## installing-packages

**Declined:** installing, removing or upgrading packages.

A profile may declare `requires = [...]`. ricepilot checks with `pacman -Q`
and prints the exact `paru -S --needed …` command. It never runs it. Package
management is a root-privileged, system-wide, hard-to-reverse operation, and
it is not what a dotfile switcher is for.

## running-rice-installers

**Declined:** invoking `caelestia install|update`, `install.fish`, or any
other rice installer.

`install.fish` does `rm -rf` on overwrite. `caelestia-cli install|update` uses
`rmtree` and can delete `~/.local/share/caelestia` outright through its
legacy-migration path. Calling either would hand away every guarantee in
[SAFETY.md](SAFETY.md) in one line.

## editing-profile-content

**Declined:** templating, `sed`-ing, or otherwise writing into a profile.

**Instead:** ricepilot links profiles into place unmodified. Applications
write into them at runtime — that is expected, and those paths are declared
`volatile` and excluded from hashing. Paths a theme engine rewrites are
declared `generated` and are never linked at all.

## snapshotting

**Declined:** taking a btrfs snapshot before a switch.

Rootless snapshots fail with `EPERM`, and snapper on the target machine has
only a `root` config — `/home`, the only subvolume that matters here, is
unsnapshotted.

**Instead:** `doctor` prints the root-only
`snapper -c home create-config /home` command for the user to run. ricepilot
never runs it, and never runs `sudo`.

## denylisted-dest

**Declined:** managing session, credential or browser state — the denylist in
[DESIGN.md §9](DESIGN.md#9-denylist-v1-hard).

These either bootstrap the session itself (so breaking them costs the user
their login), or hold secrets, or are written continuously by a running
process that would not notice the swap.

## verify-config-as-proof

**Declined:** treating `Hyprland --verify-config` exit 0 as "this config
works".

It parses twice and forks every `exec =` line on the second pass, so it can
only be run at all on a sanitised scratch copy. Exit 0 means the syntax
parsed. It does not mean the session will come up.

**Instead:** the recovery ladder in [DESIGN.md §7](DESIGN.md#7-failure-and-recovery-ladder).

## repairing-foreign-state

**Declined:** fixing anything `doctor` finds.

`doctor` is read-only by construction. It reports the `hypr-session`
`SESSION_DIR` patch, the unsnapshotted `/home`, the running theme daemons —
and prints commands rather than running them. A health check that mutates is
a health check nobody can safely run.

## not-yet-implemented

A command that exists in the CLI surface but whose milestone has not landed
exits `NotPossible` (3) and names the milestone that will bring it.

This is deliberately not a silent no-op and not a stub that pretends to
succeed. `switch --commit` returning 0 without switching anything would be
the single most dangerous thing a partially-built version of this tool could
do: the user would log out expecting a new profile and be told nothing was
wrong. The exit code and the message say plainly which half of the tool
exists.
