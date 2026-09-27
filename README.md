# ricepilot

Switch a Linux desktop between complete rice profiles by atomically
re-pointing directory symlinks.

**Status: pre-alpha, milestone M5 in progress.** Do not point it at a real `$HOME`: it has
never been run against one, and the supervised acceptance run is M5.

Working: `list`, `show`, `status`, `plan`, `switch` (with `--strict` and
`--relogin`), `rollback` (with `--relogin`), `recover`, `rescue`, `verify`,
`capture`, `adopt`, `init`, `doctor`, `diff`, `gc`. Every mutating command is
dry-run by default and requires `--commit`; `init` and `adopt` additionally ask
about each path, `--relogin` asks before it logs you out with `uwsm stop`, and
`gc` — the only command that removes anything — asks you to type each entry's
name back, with no flag that skips the asking. `gc` keeps a directory `adopt`
moved into the attic unless a profile holds an identical copy of it and the
path it came from is ricepilot's link again.

The overriding requirement is that ricepilot must never break the system it
runs on and never modify a profile's contents. The rules that follow from that
are in [`docs/SAFETY.md`](docs/SAFETY.md); what ricepilot deliberately refuses
to do is in [`docs/NOT-POSSIBLE.md`](docs/NOT-POSSIBLE.md); how it works is in
[`docs/DESIGN.md`](docs/DESIGN.md).
