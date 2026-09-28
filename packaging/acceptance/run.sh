#!/bin/sh
# Host side of the ricepilot acceptance run (M5 task F).
#
#   sh packaging/acceptance/run.sh           stage, build, run, collect logs
#   sh packaging/acceptance/run.sh build     stage and build the image only
#
# What reaches the image: `git archive HEAD` of this repository (committed
# files only: no .git, no target/, no untracked notes) plus this directory.
# What the container gets at run time: nothing from the host. No bind mount,
# no --privileged, no host network, no docker socket; --network none; it runs
# as the unprivileged user `rice` (uid 1000), with `rice2` (uid 1001) used
# through `docker exec --user` for the lock-owner check.
#
# Logs land in target/acceptance/<UTC timestamp>/ and are not committed.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=$repo/target/acceptance
ctx=$out/ctx
image=ricepilot-acceptance:latest
base=archlinux:latest@sha256:f3691b4dde62ba4c4b6f0ae2c1fbf28e8c0c8c4b9a35c7e06dc1f70e21aa29f6

stage() {
  mkdir -p "$out"
  if [ -e "$ctx" ]; then mv "$ctx" "$out/ctx.old.$$"; fi
  mkdir -p "$ctx/fakes"
  git -C "$repo" archive --format=tar -o "$ctx/ricepilot.tar" HEAD
  cp "$here/Dockerfile" "$here/.dockerignore" "$here/setup-rice.sh" \
     "$here/steps.sh" "$here/lock-check.sh" "$ctx/"
  cp "$here/fakes/Hyprland" "$here/fakes/hyprctl" "$here/fakes/uwsm" "$ctx/fakes/"
  # The staged tree must not carry anything that was never committed.
  if tar -tf "$ctx/ricepilot.tar" | grep -Eq '^(target/|\.git/|agentprompt\.md$|NEXT-AGENTS\.md$)'; then
    echo "run.sh: the staged archive contains something it must not" >&2
    exit 2
  fi
}

build() {
  docker build --build-arg "BASE=$base" -t "$image" "$ctx"
}

run() {
  ts=$(date -u +%Y%m%dT%H%M%SZ)
  logs=$out/$ts
  mkdir -p "$logs"
  name=ricepilot-acceptance-$ts
  git -C "$repo" rev-parse HEAD > "$logs/commit"
  docker image inspect --format '{{.Id}}' "$image" > "$logs/image-id"
  echo "$base" > "$logs/base"
  docker run -d --name "$name" --network none --user rice \
    --cap-drop ALL --security-opt no-new-privileges \
    "$image" sleep infinity > /dev/null
  set +e
  docker exec -i --user rice "$name" sh /acceptance/steps.sh 2>&1 | tee "$logs/steps.log"
  docker exec -i --user rice2 "$name" sh /acceptance/lock-check.sh 2>&1 | tee "$logs/lock-check.log"
  set -e
  docker cp "$name:/home/rice/acceptance" "$logs/rice" > /dev/null
  docker cp "$name:/var/log/fakes/calls.log" "$logs/fake-calls.log" > /dev/null 2>&1 || true
  docker rm -f "$name" > /dev/null
  echo "run.sh: logs in $logs"
}

case "${1:-all}" in
  build) stage; build ;;
  all) stage; build; run ;;
  *) echo "usage: run.sh [build]" >&2; exit 2 ;;
esac
