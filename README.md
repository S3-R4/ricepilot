# ricepilot

Switch a Linux desktop between complete rice profiles by atomically
re-pointing directory symlinks.

**Status: pre-alpha, milestone M0 (scaffold).** Nothing is implemented yet;
every subcommand exits 3. Do not point it at a real `$HOME`.

The overriding requirement is that ricepilot must never break the system it
runs on and never modify a profile's contents. The rules that follow from that
are in [`docs/SAFETY.md`](docs/SAFETY.md); what ricepilot deliberately refuses
to do is in [`docs/NOT-POSSIBLE.md`](docs/NOT-POSSIBLE.md); how it works is in
[`docs/DESIGN.md`](docs/DESIGN.md).
