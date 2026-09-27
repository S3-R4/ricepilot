#!/bin/sh
# SAFETY.md R2: no filesystem or subprocess syscall outside src/ops/.
#
# Keeping every syscall behind a named helper in one directory is what makes
# the dry-run guarantee checkable: plan.rs cannot accidentally do IO, and the
# complete set of effects ricepilot can have is the public surface of src/ops/.
set -eu

cd "$(dirname "$0")/.."

# Optional argument: the tree to scan (default: src). The test suite passes a
# fixture containing a planted violation to prove this check still bites.
TREE="${1:-src}"

PATTERN='std::fs|rustix::fs|std::os::unix::fs|std::process::Command|\bfs::|File::(open|create)|OpenOptions|read_to_string|read_dir|create_dir|hard_link|set_permissions'

# The one exemption, and it is a file, not a directory (D63).
#
# ricepilot's only delete syscall has to live where both rules allow it, and
# they meet in exactly one place: check-no-delete.sh confines deleting to
# src/gc/, and this check confines syscalls to src/ops/. So src/gc/remove.rs
# may spell `rustix::fs::unlinkat` and `rustix::fs::AtFlags` — those two
# spellings, and nothing else. They are blanked out of that one file and the
# rest of it is checked like any other: a `std::fs`, a `rustix::fs::openat`,
# a `Command` or a `read_dir` in it still fails, and so does either spelling
# in any other file, src/gc/ included. Everything else gc does to the
# filesystem goes through src/ops/.
GC_REMOVE="$TREE/gc/remove.rs"
GC_REMOVE_RE=$(printf '%s' "$GC_REMOVE" | sed 's/[][\.*^$]/\\&/g')

hits=$(grep -rnE "$PATTERN" "$TREE" --include='*.rs' \
    | grep -v "^$TREE/ops/" \
    | grep -v "^$GC_REMOVE_RE:") || hits=""

gc_hits=""
if [ -f "$GC_REMOVE" ]; then
    gc_hits=$(sed -e 's/rustix::fs::unlinkat//g' -e 's/rustix::fs::AtFlags//g' "$GC_REMOVE" \
        | grep -nE "$PATTERN" \
        | sed "s|^|$GC_REMOVE:|") || gc_hits=""
fi

if [ -n "$hits$gc_hits" ]; then
    echo "FAIL: filesystem/subprocess access found outside src/ops/ (SAFETY.md R2)" >&2
    [ -n "$hits" ] && echo "$hits" >&2
    [ -n "$gc_hits" ] && echo "$gc_hits" >&2
    exit 1
fi

echo "ok: no filesystem or subprocess access outside src/ops/"
