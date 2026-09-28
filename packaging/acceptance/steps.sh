#!/bin/sh
# The ricepilot acceptance run (M5 task F), inside the container, as `rice`.
#
# Every step records its command, stdout, stderr and exit code under
# ~/acceptance/NN-<id>.{cmd,out,err,rc}, is judged against the exit code(s)
# it is expected to return plus any checks that follow it, and ends up as a
# row of ~/acceptance/results.tsv. A step that is not run is recorded as
# SKIPPED with the reason. Nothing here reaches anything but this container:
# the Hyprland/hyprctl/uwsm on PATH are the logging stand-ins from fakes/.
set -u
L=$HOME/acceptance
mkdir -p "$L"
: > "$L/results.tsv"
export PATH=/usr/local/bin:/usr/bin:/bin
export XDG_RUNTIME_DIR=/run/user/1000
unset WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
H=$HOME
C=$H/.local/share/caelestia
S=$H/.local/state/ricepilot
D=$H/.local/share/ricepilot
CALLS=/var/log/fakes/calls.log
LINKS="hypr fish foot btop fastfetch uwsm spicetify starship.toml codium-flags.conf"
n=0
last=
lastf=

show() { # file, max lines
  if [ -s "$1" ]; then head -n "${2:-40}" "$1" | sed 's/^/    | /'; fi
}

# step <id> <expected rc, e.g. "0" or "0 7"> <stdin (printf %b)> <title> -- cmd...
step() {
  id=$1 want=$2 input=$3 title=$4
  shift 5
  n=$((n + 1))
  last=$id
  lastf=$L/$(printf %02d "$n")-$id
  printf '%s\n' "$*" > "$lastf.cmd"
  printf '%b' "$input" | "$@" > "$lastf.out" 2> "$lastf.err"
  rc=$?
  echo "$rc" > "$lastf.rc"
  case " $want " in *" $rc "*) v=PASS ;; *) v=FAIL ;; esac
  printf '%s\t%s\t%s\t%s\t%s\n' "$n" "$id" "$rc" "$v" "$title" >> "$L/results.tsv"
  echo
  echo "################ [$n] $id — $title"
  echo "\$ $*"
  [ -n "$input" ] && printf '  stdin: %s\n' "$(printf '%b' "$input" | tr '\n' ' ')"
  echo "  exit $rc (expected: $want) => $v"
  echo "  stdout:"; show "$lastf.out" "${SHOW:-60}"
  echo "  stderr:"; show "$lastf.err" 20
}

# check <what> -- cmd...   a condition on the state the last step left
check() {
  what=$1
  shift 2
  if "$@" > "$lastf.check.$$" 2>&1; then v=PASS; else v=FAIL; fi
  printf '%s\t%s\t-\t%s\t  check: %s\n' "$n" "$last" "$v" "$what" >> "$L/results.tsv"
  echo "  check: $what => $v"
  [ "$v" = FAIL ] && show "$lastf.check.$$" 20
  rm -f "$lastf.check.$$"
}

skip() { # id, reason
  n=$((n + 1))
  printf '%s\t%s\t-\tSKIPPED\t%s\n' "$n" "$1" "$2" >> "$L/results.tsv"
  echo
  echo "################ [$n] $1 — SKIPPED: $2"
}

links_of() { # home
  for l in $LINKS; do printf '%s -> %s\n' "$l" "$(readlink "$1/.config/$l" 2>/dev/null || echo '(not a link)')"; done
}
tree_of() { # dir: type, mode, size, mtime, path, link target, and content hash
  (cd "$1" && find . -path ./.git -prune -o -printf '%y %m %s %T@ %p -> %l\n' | LC_ALL=C sort \
    && find . -path ./.git -prune -o -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum)
}
same() { cmp -s "$1" "$2" || { diff -u "$1" "$2"; return 1; }; }
calls() { grep -c "^=== $1 " "$CALLS" 2>/dev/null || true; }
# the dests a switch into `bare` owns: hypr and fish into the bare profile
points_into() { case "$(readlink "$1")" in "$2"*) return 0 ;; *) echo "$1 -> $(readlink "$1")"; return 1 ;; esac; }

# A fake Wayland session in <dir>: a socket named wayland-1, and the systemd
# record uwsm's compositor unit leaves (what D58's session check reads).
mk_session() {
  mkdir -p "$1/systemd/units"
  chmod 700 "$1"
  python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1])' "$1/wayland-1"
  ln -sfn deadbeef "$1/systemd/units/invocation:wayland-wm@hyprland.desktop.service"
}

# Build a rice, init it (yes to every question) and add the bare profile, in <home>.
bare_profile() { # home
  mkdir -p "$1/.local/share/ricepilot/profiles/bare/hypr" "$1/.local/share/ricepilot/profiles/bare/fish"
  cat > "$1/.local/share/ricepilot/profiles/bare/profile.toml" <<'TOML'
name = "bare"
hypr_dialect = "conf"
volatile = ["**/fish_variables"]

[[path]]
dest = "~/.config/hypr"
src = "hypr"
kind = "dir-link"
activation = "relogin"

[[path]]
dest = "~/.config/fish"
src = "fish"
kind = "dir-link"
activation = "relogin"
TOML
  printf '# minimal bare hyprland.conf (synthetic)\nexec-once = placeholder-bar\ngeneral {\n    gaps_in = 0\n}\n' \
    > "$1/.local/share/ricepilot/profiles/bare/hypr/hyprland.conf"
  printf '# bare fish\n' > "$1/.local/share/ricepilot/profiles/bare/fish/config.fish"
}
fresh_home() { # home: rice + init --commit + bare, quietly, for the crash and rescue runs
  sh /acceptance/setup-rice.sh "$1" > /dev/null &&
    printf 'y\ny\ny\ny\ny\ny\ny\ny\ny\ny\n' | RICEPILOT_HOME=$1 ricepilot init --root "$1/.local/share/caelestia" --commit > "$1.init.log" 2>&1 &&
    bare_profile "$1"
}

echo "ricepilot acceptance run, $(date -u +%Y-%m-%dT%H:%M:%SZ), uid $(id -u) ($(id -un)), in $(uname -srm)"
echo "ricepilot: $(ricepilot --version)   commit: see run.sh log"
echo "stand-ins: $(command -v Hyprland) $(command -v hyprctl) $(command -v uwsm); /usr/bin has none: $(ls /usr/bin/Hyprland /usr/bin/hyprctl /usr/bin/uwsm 2>&1 | tr '\n' ' ')"
echo "shells: /bin/sh -> $(readlink -f /bin/sh), dash $(command -v dash), busybox $(command -v busybox)"

# ---------------------------------------------------------------- set-up
step setup 0 "" "build the synthetic rice (setup-rice.sh)" -- sh /acceptance/setup-rice.sh
git -C "$C" status --porcelain > "$L/git-before.txt"
links_of "$H" > "$L/links-before.txt"
tree_of "$C" > "$L/tree-before.txt"
check "7 dir links + 2 file links, into the clone" -- sh -c "grep -c ' -> $C/' '$L/links-before.txt' | grep -qx 9"
check "the clone has modified and untracked files" -- sh -c "grep -q '^ M' '$L/git-before.txt' && grep -q '^??' '$L/git-before.txt'"
echo "  git status --porcelain before:"; show "$L/git-before.txt"

# ---------------------------------------------------------------- doctor, init
step doctor-1 "0 7" "" "doctor on the untouched rice (read-only)" -- ricepilot doctor
check "doctor wrote nothing: no data or state directory" -- sh -c "! test -e '$D' && ! test -e '$S'"

step init-dry 0 "" "init (dry run)" -- ricepilot init --root "$C"
check "dry run registered nothing" -- sh -c "! test -e '$D/profiles'"

step init-commit 0 "y\ny\ny\ny\ny\ny\ny\ny\ny\ny\n" "init --commit, yes to every question over stdin" -- ricepilot init --root "$C" --commit
check "profile caelestia registered by reference" -- grep -q "^root = " "$D/profiles/caelestia/profile.toml"
check "baseline copy taken" -- test -d "$S/baseline/caelestia"
check "init moved no link" -- sh -c "links_of() { for l in $LINKS; do printf '%s -> %s\n' \$l \"\$(readlink $H/.config/\$l)\"; done; }; links_of | cmp -s - '$L/links-before.txt'"
check "uwsm link reported, not offered (denylist)" -- grep -q "uwsm" "$lastf.out"

step status-1 0 "" "status after init" -- ricepilot status
step show-caelestia 0 "" "show caelestia" -- ricepilot show caelestia

# ---------------------------------------------------------------- bare profile
bare_profile "$H"
step show-bare 0 "" "create a minimal \`bare\` profile (hypr + fish) by hand, then show bare" -- ricepilot show bare
step list 0 "" "list" -- ricepilot list
step plan-bare 0 "" "plan bare" -- ricepilot plan bare

# ---------------------------------------------------------------- switch
before_calls=$(calls Hyprland)
step switch-dry 0 "" "switch bare (dry run)" -- ricepilot switch bare
check "dry run moved no link" -- sh -c "for l in $LINKS; do printf '%s -> %s\n' \$l \"\$(readlink $H/.config/\$l)\"; done | cmp -s - '$L/links-before.txt'"
check "verify-config ran the fake Hyprland" -- test "$(calls Hyprland)" -gt "$before_calls"
check "no exec line reached Hyprland (D55)" -- sh -c "! grep -q EXEC-LINE-SURVIVED '$CALLS'"

uwsm_before=$(calls uwsm)
step switch-commit-relogin 0 "n\n" "switch bare --commit --relogin, outside any session" -- ricepilot switch bare --commit --relogin
check "hypr now points into bare" -- points_into "$H/.config/hypr" "$D/profiles/bare"
check "fish now points into bare" -- points_into "$H/.config/fish" "$D/profiles/bare"
check "file links untouched" -- sh -c "readlink $H/.config/starship.toml | grep -q caelestia && readlink $H/.config/codium-flags.conf | grep -q caelestia"
check "relogin declined; uwsm never called" -- test "$(calls uwsm)" -eq "$uwsm_before"
check "rescue.sh written and parses under dash" -- dash -n "$S/rescue.sh"
cp "$lastf.out" "$L/switch-1.out"

step verify-bare 0 "" "verify bare" -- ricepilot verify bare
step status-2 0 "" "status after switch" -- ricepilot status

# ---------------------------------------------------------------- --relogin
# A fake uwsm session in rice's runtime directory: a wayland-1 socket and the
# unit record uwsm leaves (what D58's session check reads). The stand-in uwsm
# on PATH records the argv and environment the real `uwsm stop` would get.
RT=/run/user/1000
SESSION="WAYLAND_DISPLAY=wayland-1 DBUS_SESSION_BUS_ADDRESS=unix:path=$RT/bus"
mk_session "$RT"
# 1) A yes runs the stand-in, once, with exactly `stop`.
uwsm_before=$(calls uwsm)
step rollback-relogin-yes 0 "y\n" "rollback --commit --relogin in a fake uwsm session, answered yes (stand-in uwsm)" -- \
  env $SESSION ricepilot rollback --commit --relogin
check "every --relogin check passed" -- grep -q "every check passed" "$lastf.out"
check "the stand-in uwsm was run exactly once" -- test "$(calls uwsm)" -eq $((uwsm_before + 1))
check "argv was exactly \`stop\`" -- sh -c "awk '/^=== uwsm/{f=1;next} /^=== /{f=0} f&&/^argv:/' '$CALLS' | tail -n 1 | grep -qx 'argv: stop'"
sed -n '/^=== uwsm/,/^result:/p' "$CALLS" | tail -n 20 > "$L/uwsm-call.txt"
echo "  what the stand-in uwsm received:"; show "$L/uwsm-call.txt"
# 2) The same, answered no.
step switch-relogin-no 0 "n\n" "switch bare --commit --relogin in the fake session, answered no" -- \
  env $SESSION ricepilot switch bare --commit --relogin
check "the y/N question was asked" -- grep -q "log out now with \`uwsm stop\`? \[y/N\]" "$lastf.out"
check "answered no: uwsm not run again" -- test "$(calls uwsm)" -eq $((uwsm_before + 1))
# 3) D56: inside RICEPILOT_SANDBOX the same yes reaches exec, which refuses
#    uwsm. A separate home under the sandbox root (every location must lie
#    below it), and a profile with no hypr config in it, so the switch itself
#    does not need the (also refused) Hyprland verify-config and gets as far
#    as the logout question.
SBX=$H/sbx
SH=$SBX/home
mkdir -p "$SH/.config" "$SH/.local/share/ricepilot/profiles/plain/waybar"
printf 'name = "plain"\n\n[[path]]\ndest = "~/.config/waybar"\nsrc = "waybar"\nkind = "dir-link"\nactivation = "relogin"\n' \
  > "$SH/.local/share/ricepilot/profiles/plain/profile.toml"
printf '{}\n' > "$SH/.local/share/ricepilot/profiles/plain/waybar/config"
mk_session "$SBX/run"
uwsm_before=$(calls uwsm)
step relogin-sandbox-d56 "0 2 4" "y\n" "switch plain --commit --relogin inside RICEPILOT_SANDBOX, fake session, answered yes" -- \
  env RICEPILOT_SANDBOX="$SBX" RICEPILOT_HOME="$SH" RICEPILOT_DATA_DIR="$SH/.local/share/ricepilot" \
      RICEPILOT_STATE_DIR="$SH/.local/state/ricepilot" RICEPILOT_RUNTIME_DIR="$SBX/run" \
      XDG_RUNTIME_DIR="$SBX/run" WAYLAND_DISPLAY=wayland-1 DBUS_SESSION_BUS_ADDRESS="unix:path=$SBX/run/bus" \
      ricepilot switch plain --commit --relogin
check "the switch was applied and the logout question was asked" -- sh -c "test -L '$SH/.config/waybar' && grep -q 'log out now' '$lastf.out'"
check "uwsm refused by the sandbox, never run (D56)" -- test "$(calls uwsm)" -eq "$uwsm_before"
check "the refusal names uwsm and the sandbox" -- sh -c "cat '$lastf.out' '$lastf.err' | grep 'RICEPILOT_SANDBOX' | grep -q uwsm"
skip real-logout-login "no real uwsm/Hyprland/SDDM in the container; the real \`uwsm stop\`, logout and login are left for the user's machine (the stand-in shows the argv/env it would get)"

# ---------------------------------------------------------------- back to caelestia and prove nothing changed
step rollback-commit 0 "" "rollback --commit (back to the links init found)" -- ricepilot rollback --commit
links_of "$H" > "$L/links-after.txt"
check "all 7 dir links and both file links as before (readlink)" -- same "$L/links-before.txt" "$L/links-after.txt"
step verify-caelestia 0 "" "verify caelestia (clean; the manifest init recorded, D79)" -- ricepilot verify caelestia
git -C "$C" status --porcelain > "$L/git-after.txt"
tree_of "$C" > "$L/tree-after.txt"
check "git status --porcelain identical to before" -- same "$L/git-before.txt" "$L/git-after.txt"
check "clone byte-identical: types, modes, sizes, mtimes, link targets, sha256" -- same "$L/tree-before.txt" "$L/tree-after.txt"
step rescue-cmd 0 "" "rescue (prints where rescue.sh is)" -- ricepilot rescue

# ---------------------------------------------------------------- --strict
BARE=$D/profiles/bare
printf 'set -g placeholder 1\n' > "$BARE/fish/fish_variables"
step strict-volatile 0 "" "switch bare --commit --strict, only a volatile file changed since recorded: goes ahead" -- ricepilot switch bare --commit --strict
check "hypr now points into bare" -- points_into "$H/.config/hypr" "$BARE"
step strict-rollback 0 "" "rollback --commit" -- ricepilot rollback --commit
check "links as before" -- sh -c "for l in $LINKS; do printf '%s -> %s\n' \$l \"\$(readlink $H/.config/\$l)\"; done | cmp -s - '$L/links-before.txt'"
printf '# an edit outside volatile\n' >> "$BARE/hypr/hyprland.conf"
step strict-drift 2 "" "switch bare --commit --strict after an edit outside volatile: refused" -- ricepilot switch bare --commit --strict
check "no link moved" -- sh -c "for l in $LINKS; do printf '%s -> %s\n' \$l \"\$(readlink $H/.config/\$l)\"; done | cmp -s - '$L/links-before.txt'"
check "the refusal names the changed file" -- grep -q "hypr/hyprland.conf" "$lastf.out"
step nonstrict-drift 0 "" "switch bare (dry run, no --strict): the same drift is reported, not refused" -- ricepilot switch bare
check "reported, not refused" -- grep -q "reported, not refused" "$lastf.out"

# ---------------------------------------------------------------- adopt, diff, doctor
step adopt-by-ref "2" "y\n" "adopt kitty into caelestia (by reference) is refused, R3" -- ricepilot adopt "$H/.config/kitty" --into caelestia --commit
check "kitty still a real directory" -- sh -c "test -d '$H/.config/kitty' && ! test -L '$H/.config/kitty'"
step adopt-dry 0 "" "adopt ~/.config/kitty --into bare (dry run)" -- ricepilot adopt "$H/.config/kitty" --into bare
check "dry run: kitty still a real directory" -- sh -c "test -d '$H/.config/kitty' && ! test -L '$H/.config/kitty'"
(cd "$H/.config/kitty" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) > "$L/kitty-before.txt"
step adopt-commit 0 "y\n" "adopt ~/.config/kitty --into bare --commit, answered yes" -- ricepilot adopt "$H/.config/kitty" --into bare --commit
check "kitty is now a link into bare" -- points_into "$H/.config/kitty" "$D/profiles/bare"
check "the original kitty directory is in the attic, content intact" -- sh -c "d=\$(find '$S/attic' -path '*/.config/kitty' -type d | head -n 1); test -n \"\$d\" && (cd \"\$d\" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) | cmp -s - '$L/kitty-before.txt'"
check "the link resolves to the same content" -- sh -c "(cd '$H/.config/kitty/' && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) | cmp -s - '$L/kitty-before.txt'"
(cd "$H/.config/nvim" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) > "$L/nvim-before.txt"
step capture-dry 0 "" "capture nvimrice --from ~/.config/nvim (dry run)" -- ricepilot capture nvimrice --from "$H/.config/nvim"
check "dry run: no profile made" -- sh -c "! test -e '$D/profiles/nvimrice'"
step capture-commit 0 "y\ny\n" "capture nvimrice --from ~/.config/nvim --commit" -- ricepilot capture nvimrice --from "$H/.config/nvim" --commit
check "the profile holds a copy with the same content" -- sh -c "(cd '$D/profiles/nvimrice/nvim' && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) | cmp -s - '$L/nvim-before.txt'"
check "~/.config/nvim is untouched: still a real directory, same content" -- sh -c "test -d '$H/.config/nvim' && ! test -L '$H/.config/nvim' && (cd '$H/.config/nvim' && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) | cmp -s - '$L/nvim-before.txt'"
step verify-nvimrice 0 "" "verify nvimrice (capture recorded its manifest)" -- ricepilot verify nvimrice
step diff-bare "0 6" "" "diff bare" -- ricepilot diff bare
step diff-caelestia "0 6" "" "diff caelestia" -- ricepilot diff caelestia
step doctor-2 "0 7" "" "doctor after the run" -- ricepilot doctor

# ---------------------------------------------------------------- gc
find "$S/attic" "$S/verify" -mindepth 1 -maxdepth 1 2>/dev/null | LC_ALL=C sort > "$L/gc-before.txt"
step gc-dry 0 "" "gc (dry run)" -- ricepilot gc
find "$S/attic" "$S/verify" -mindepth 1 -maxdepth 1 2>/dev/null | LC_ALL=C sort > "$L/gc-after-dry.txt"
check "dry run removed nothing" -- same "$L/gc-before.txt" "$L/gc-after-dry.txt"
check "every verify copy is recognised as ricepilot's (D78)" -- sh -c "! grep -q 'is not a name ricepilot gives' '$lastf.out'"
check "the adopted kitty original is offered only because bare holds an identical copy (D61)" -- grep -q "holds an identical copy" "$lastf.out"
step gc-wrong "0" "wrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\nwrong\n" "gc --commit, a WRONG name typed for every entry" -- ricepilot gc --commit
find "$S/attic" "$S/verify" -mindepth 1 -maxdepth 1 2>/dev/null | LC_ALL=C sort > "$L/gc-after-wrong.txt"
check "wrong names removed nothing" -- same "$L/gc-before.txt" "$L/gc-after-wrong.txt"
sed -n 's/.*type \([^ ]*\) to remove it, or anything else to keep it:.*/\1/p' "$lastf.out" > "$L/gc-names.txt"
check "at least one candidate was offered" -- test -s "$L/gc-names.txt"

# ---------------------------------------------------------------- lock contention
# A gc --commit waiting at its first typed-name question holds the lock.
mkfifo "$L/hold"
( ricepilot gc --commit < "$L/hold" > "$L/holder.out" 2>&1 ) &
holder=$!
exec 9> "$L/hold"
sleep 2
step lock-busy 5 "" "a second ricepilot while one holds the lock (gc waiting for a name)" -- ricepilot switch bare
check "the holder was really waiting at a question" -- grep -q "to remove it, or anything else to keep it" "$L/holder.out"
exec 9>&-
wait "$holder"
find "$S/attic" "$S/verify" -mindepth 1 -maxdepth 1 2>/dev/null | LC_ALL=C sort > "$L/gc-after-holder.txt"
check "the holder, given end of input, removed nothing" -- same "$L/gc-before.txt" "$L/gc-after-holder.txt"

names=$(sed 's/$/\\n/' "$L/gc-names.txt" | tr -d '\n')
step gc-right 0 "$names" "gc --commit, the right name typed for every candidate" -- ricepilot gc --commit
find "$S/attic" "$S/verify" -mindepth 1 -maxdepth 1 2>/dev/null | LC_ALL=C sort > "$L/gc-after-right.txt"
check "exactly the typed entries are gone" -- sh -c "for nm in \$(cat '$L/gc-names.txt'); do ! grep -q \"/\$nm\$\" '$L/gc-after-right.txt' || exit 1; done; test \$(wc -l < '$L/gc-after-right.txt') -eq \$(( \$(wc -l < '$L/gc-before.txt') - \$(wc -l < '$L/gc-names.txt') ))"
check "kitty still resolves to its original content" -- sh -c "(cd '$H/.config/kitty/' && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) | cmp -s - '$L/kitty-before.txt'"
links_of "$H" > "$L/links-after-gc.txt"
check "links unchanged by gc" -- same "$L/links-after.txt" "$L/links-after-gc.txt"
step doctor-3 "0 7" "" "doctor after gc" -- ricepilot doctor

# ---------------------------------------------------------------- crash injection
# Kill the real binary with SIGKILL at the k-th call of a syscall, via
# strace's fault injection, during `switch bare --commit`, in a fresh home
# per k; then `recover` must leave every destination fully old or fully new.
if strace -f -o /dev/null true 2>/dev/null; then
  CR=$H/crash
  mkdir -p "$CR"
  fresh_home "$CR/count" && RICEPILOT_HOME=$CR/count strace -f -qq -c -o "$L/crash-syscalls.txt" ricepilot switch bare --commit > /dev/null 2>&1
  echo; echo "  syscall counts of one switch bare --commit:"; grep -E 'rename|symlink|fsync|openat|mkdir' "$L/crash-syscalls.txt" | sed 's/^/    | /'
  for sc in renameat2 renameat symlinkat fsync; do
    total=$(awk -v s="$sc" '$NF==s{print $4}' "$L/crash-syscalls.txt")
    [ -n "$total" ] || continue
    k=1
    while [ "$k" -le "$total" ]; do
      h=$CR/$sc-$k
      fresh_home "$h" || { skip "crash-$sc-$k" "fresh home failed"; k=$((k + 1)); continue; }
      links_of "$h" > "$h.old"
      RICEPILOT_HOME=$h strace -f -qq -o /dev/null -e trace="$sc" -e inject="$sc:signal=KILL:when=$k" ricepilot switch bare --commit > "$h.switch.out" 2>&1
      src=$?
      links_of "$h" > "$h.mid"
      SHOW=12 step "crash-$sc-$k-recover" 0 "" "SIGKILL at $sc #$k of $total (switch exit $src), then recover --commit" -- env RICEPILOT_HOME="$h" ricepilot recover --commit
      links_of "$h" > "$h.after"
      # fully new = hypr and fish into bare, the four other owned links
      # retired to the attic (bare does not declare them), the rest as before
      sed -e "s#^hypr -> .*#hypr -> $h/.local/share/ricepilot/profiles/bare/hypr#" -e "s#^fish -> .*#fish -> $h/.local/share/ricepilot/profiles/bare/fish#" \
          -e "s#^\(foot\|btop\|fastfetch\|spicetify\) -> .*#\1 -> (not a link)#" "$h.old" > "$h.new"
      check "fully old or fully new, never mixed" -- sh -c "cmp -s '$h.after' '$h.old' && echo old || { cmp -s '$h.after' '$h.new' && echo new; } || { diff '$h.old' '$h.after'; exit 1; }"
      check "no journal left in flight" -- sh -c "! test -e '$h/.local/state/ricepilot/journal/current.toml'"
      check "ricepilot still owns the links: a switch after recover lands fully new (D77)" -- sh -c "RICEPILOT_HOME='$h' ricepilot switch bare --commit && for l in $LINKS; do printf '%s -> %s\n' \$l \"\$(readlink $h/.config/\$l 2>/dev/null || echo '(not a link)')\"; done | cmp -s - '$h.new'"
      check "and a rollback after it lands fully old" -- sh -c "RICEPILOT_HOME='$h' ricepilot rollback --commit && for l in $LINKS; do printf '%s -> %s\n' \$l \"\$(readlink $h/.config/\$l 2>/dev/null || echo '(not a link)')\"; done | cmp -s - '$h.old'"
      k=$((k + 1))
    done
  done
else
  skip crash-injection "strace cannot trace in this container"
fi

# ---------------------------------------------------------------- crash injection during adopt
# The same, killing `adopt ~/.config/kitty --into bare --commit` (answered
# yes): afterwards kitty is either still the real directory or a link into
# bare with the original in the attic, its content intact either way.
hash_of() { (cd "$1" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum); }
if strace -f -o /dev/null true 2>/dev/null; then
  AC=$H/crash-adopt
  mkdir -p "$AC"
  fresh_home "$AC/count" && printf 'y\n' | RICEPILOT_HOME=$AC/count strace -f -qq -c -o "$L/crash-adopt-syscalls.txt" ricepilot adopt "$AC/count/.config/kitty" --into bare --commit > /dev/null 2>&1
  echo; echo "  syscall counts of one adopt --commit:"; grep -E 'rename|symlink|fsync' "$L/crash-adopt-syscalls.txt" | sed 's/^/    | /'
  for sc in renameat2 renameat symlinkat fsync; do
    total=$(awk -v s="$sc" '$NF==s{print $4}' "$L/crash-adopt-syscalls.txt")
    [ -n "$total" ] || continue
    k=1
    while [ "$k" -le "$total" ]; do
      h=$AC/$sc-$k
      fresh_home "$h" || { skip "crash-adopt-$sc-$k" "fresh home failed"; k=$((k + 1)); continue; }
      hash_of "$h/.config/kitty" > "$h.kitty"
      printf 'y\n' | RICEPILOT_HOME=$h strace -f -qq -o /dev/null -e trace="$sc" -e inject="$sc:signal=KILL:when=$k" ricepilot adopt "$h/.config/kitty" --into bare --commit > "$h.adopt.out" 2>&1
      src=$?
      SHOW=12 step "crash-adopt-$sc-$k-recover" "0" "" "SIGKILL at $sc #$k of $total during adopt (exit $src), then recover --commit" -- env RICEPILOT_HOME="$h" ricepilot recover --commit
      check "kitty is the real directory or a link into bare, content intact either way" -- sh -c "
        if test -L '$h/.config/kitty'; then
          case \$(readlink '$h/.config/kitty') in '$h/.local/share/ricepilot/profiles/bare/'*) ;; *) exit 1 ;; esac
          d=\$(find '$h/.local/state/ricepilot/attic' -path '*/.config/kitty' -type d | head -n 1)
          test -n \"\$d\" && (cd \"\$d\" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) | cmp -s - '$h.kitty' && echo new
        else
          test -d '$h/.config/kitty' && (cd '$h/.config/kitty' && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) | cmp -s - '$h.kitty' && echo old
        fi"
      check "no journal left in flight" -- sh -c "! test -e '$h/.local/state/ricepilot/journal/current.toml'"
      check "a finished adopt's link is ricepilot's (status lists it, D77)" -- sh -c "! test -L '$h/.config/kitty' || RICEPILOT_HOME='$h' ricepilot status | grep -q '/.config/kitty -> '"
      k=$((k + 1))
    done
  done
else
  skip crash-adopt-injection "strace cannot trace in this container"
fi

# ---------------------------------------------------------------- rescue.sh under three shells
for sh in dash "busybox sh" bash; do
  tag=$(echo "$sh" | tr ' ' '-')
  h=$H/rescue-$tag
  fresh_home "$h"
  links_of "$h" > "$h.old"
  RICEPILOT_HOME=$h ricepilot switch bare --commit > "$h.switch.out" 2>&1
  links_of "$h" > "$h.new"
  step "rescue-$tag" 0 "" "rescue.sh under $sh restores the previous generation" -- $sh "$h/.local/state/ricepilot/rescue.sh"
  links_of "$h" > "$h.after"
  check "the switch had changed the links" -- sh -c "! cmp -s '$h.old' '$h.new'"
  check "links are the previous generation's again" -- same "$h.old" "$h.after"
  check "the displaced links went to the attic, not deleted" -- sh -c "find '$h/.local/state/ricepilot/attic' -name 'rescue-*' -type d | grep -q ."
  # The records are out of step now (README, RECOVERY.md step 5): doctor
  # says so, and its commands are the way back (D81). Follow them.
  step "rescue-$tag-doctor" 7 "" "doctor after rescue.sh ($sh): the restored links are out of step with the ledger" -- env RICEPILOT_HOME="$h" ricepilot doctor
  check "it advises set-aside + rollback, never a switch back to bare (D81)" -- sh -c "grep -q 'ricepilot rollback --commit' '$lastf.out' && ! grep -q 'switch bare --commit' '$lastf.out'"
  sed -n 's/^ *\(mv -nT .*\)$/\1/p' "$lastf.out" > "$h.moves"
  sh "$h.moves"
  step "rescue-$tag-rollback" 0 "" "the rollback --commit doctor printed" -- env RICEPILOT_HOME="$h" ricepilot rollback --commit
  links_of "$h" > "$h.recorded"
  check "links are still the previous generation's" -- same "$h.old" "$h.recorded"
  step "rescue-$tag-doctor-after" 0 "" "doctor after following its advice: healthy" -- env RICEPILOT_HOME="$h" ricepilot doctor
done

# ---------------------------------------------------------------- leave a lock file owned by rice for lock-check.sh
step lock-create 0 "" "rice takes the lock in /srv/shared-runtime (for the D74 check by rice2)" -- env RICEPILOT_RUNTIME_DIR=/srv/shared-runtime ricepilot recover
check "the lock file is rice's" -- sh -c "test \"\$(stat -c %U /srv/shared-runtime/ricepilot.lock)\" = rice"

echo
echo "================ steps.sh results"
column -t -s "$(printf '\t')" "$L/results.tsv" 2>/dev/null || cat "$L/results.tsv"
fails=$(awk -F'\t' '$4=="FAIL"' "$L/results.tsv" | wc -l)
echo "FAIL rows: $fails"
