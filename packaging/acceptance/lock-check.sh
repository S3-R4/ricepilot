#!/bin/sh
# The D74 lock-owner check, inside the container, as `rice2` (uid 1001),
# after steps.sh has left /srv/shared-runtime/ricepilot.lock owned by `rice`.
# Results are appended to rice's results table through a file rice2 can
# write: /srv/shared-runtime is mode 1777.
set -u
export PATH=/usr/local/bin:/usr/bin:/bin
export XDG_RUNTIME_DIR=/run/user/1001
L=/srv/shared-runtime/lock-check
mkdir -p "$L"
: > "$L/results.tsv"
n=100

run() { # id, expected rc, title, cmd...
  id=$1 want=$2 title=$3
  shift 3
  n=$((n + 1))
  f=$L/$n-$id
  printf '%s\n' "$*" > "$f.cmd"
  timeout 20 "$@" > "$f.out" 2> "$f.err" < /dev/null
  rc=$?
  echo "$rc" > "$f.rc"
  case " $want " in *" $rc "*) v=PASS ;; *) v=FAIL ;; esac
  printf '%s\t%s\t%s\t%s\t%s\n' "$n" "$id" "$rc" "$v" "$title" >> "$L/results.tsv"
  echo
  echo "################ [$n] $id — $title"
  echo "\$ $*"
  echo "  exit $rc (expected: $want) => $v"
  echo "  stdout:"; sed 's/^/    | /' "$f.out" | head -n 20
  echo "  stderr:"; sed 's/^/    | /' "$f.err" | head -n 20
}
check() { # what, cmd...
  what=$1
  shift
  if "$@" > /dev/null 2>&1; then v=PASS; else v=FAIL; fi
  printf '%s\t%s\t-\t%s\t  check: %s\n' "$n" "$id" "$v" "$what" >> "$L/results.tsv"
  echo "  check: $what => $v"
}

echo "lock-check.sh as uid $(id -u) ($(id -un))"
ls -l /srv/shared-runtime/ricepilot.lock

run lock-foreign-owner 2 "rice2 against a lock file owned by rice (D74)" \
  env RICEPILOT_RUNTIME_DIR=/srv/shared-runtime ricepilot recover
check "the refusal says the lock must be a file you own" grep -q "own" "$f.err"
check "the lock file is still rice's" sh -c 'test "$(stat -c %U /srv/shared-runtime/ricepilot.lock)" = rice'

R2=/srv/shared-runtime/rice2-rt
mkdir -p "$R2"
chmod 700 "$R2"
ln -s "$HOME/lock-target" "$R2/ricepilot.lock"
run lock-symlink 2 "rice2, the lock's name is a symlink to a file that does not exist (D74)" \
  env RICEPILOT_RUNTIME_DIR="$R2" ricepilot recover
check "the symlink's target was not created" sh -c "! test -e '$HOME/lock-target'"

R3=/srv/shared-runtime/rice2-fifo
mkdir -p "$R3"
chmod 700 "$R3"
mkfifo "$R3/ricepilot.lock"
run lock-fifo 2 "rice2, the lock's name is a fifo: refused, and no hang (D74)" \
  env RICEPILOT_RUNTIME_DIR="$R3" ricepilot recover

run lock-own 0 "rice2 with its own runtime directory: the lock is taken" ricepilot recover
check "rice2's lock file is rice2's, mode 600" sh -c 'test "$(stat -c %U:%a /run/user/1001/ricepilot.lock)" = rice2:600'

echo
echo "================ lock-check.sh results"
cat "$L/results.tsv"
