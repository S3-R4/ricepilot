# ricepilot

Switch a Linux desktop between complete rice profiles by atomically
re-pointing directory symlinks.

**Status: pre-alpha, milestone M4.** Do not point it at a real `$HOME`: it has
never been run against one, and the supervised acceptance run is M5.

Working: `list`, `show`, `status`, `plan`, `switch`, `rollback`, `recover`,
`rescue`, `verify`, `capture`, `adopt`, `init`. Every mutating command is
dry-run by default and requires `--commit`; `init` and `adopt` additionally
ask about each path, with no flag that skips the asking.

Not built yet: `doctor`, `diff`, `gc`, `switch --relogin` and
`switch --strict` (M5). Each exits 3 and names its milestone rather than
pretending to succeed.

The overriding requirement is that ricepilot must never break the system it
runs on and never modify a profile's contents. The rules that follow from that
are in [`docs/SAFETY.md`](docs/SAFETY.md); what ricepilot deliberately refuses
to do is in [`docs/NOT-POSSIBLE.md`](docs/NOT-POSSIBLE.md); how it works is in
[`docs/DESIGN.md`](docs/DESIGN.md).
