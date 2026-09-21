#!/bin/sh
# SAFETY.md R2: no delete primitive exists outside src/gc/.
#
# ricepilot displaces things by renaming them into the attic. The ability to
# remove a file or directory is confined to one directory, which is small
# enough to audit by eye and never runs implicitly.
#
# This is a textual check and it deliberately does not exempt comments: if the
# word appears outside src/gc/, that is worth a human look.
set -eu

cd "$(dirname "$0")/.."

# Optional argument: the tree to scan (default: src). The test suite passes a
# fixture containing a planted violation to prove this check still bites.
TREE="${1:-src}"

PATTERN='remove_file|remove_dir|unlink|rmdir|rmtree|\brm[[:space:]]+-[a-zA-Z]|O_TRUNC|set_len'

if hits=$(grep -rnE "$PATTERN" "$TREE" --include='*.rs' | grep -v "^$TREE/gc/"); then
    echo "FAIL: delete primitive found outside src/gc/ (SAFETY.md R2)" >&2
    echo "$hits" >&2
    exit 1
fi

echo "ok: no delete primitive outside src/gc/"
