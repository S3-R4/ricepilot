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

if hits=$(grep -rnE "$PATTERN" "$TREE" --include='*.rs' | grep -v "^$TREE/ops/"); then
    echo "FAIL: filesystem/subprocess access found outside src/ops/ (SAFETY.md R2)" >&2
    echo "$hits" >&2
    exit 1
fi

echo "ok: no filesystem or subprocess access outside src/ops/"
