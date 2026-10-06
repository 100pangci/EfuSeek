#!/usr/bin/env bash
set -euo pipefail

binary=${1:-target/debug/efuseek}
version=$(cargo metadata --locked --offline --no-deps --format-version 1 |
  jq -r '.packages[] | select(.name == "efuseek") | .version')
root=$(mktemp -d "${TMPDIR:-/tmp}/efuseek-smoke.XXXXXX")
export XDG_CONFIG_HOME="$root/config" XDG_CACHE_HOME="$root/cache"
export XDG_DATA_HOME="$root/data" XDG_STATE_HOME="$root/state" XDG_RUNTIME_DIR="$root/runtime"
export GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=none
unset WAYLAND_DISPLAY
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

"$binary" > "$root/application.log" 2>&1 &
pid=$!
cleanup() { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }
trap cleanup EXIT
window=$(timeout 20s xdotool search --sync --onlyvisible --name 'EfuSeek v' | head -n 1)
title=$(xdotool getwindowname "$window")
[[ "$title" == "EfuSeek v$version" ]]
for _ in {1..30}; do
  [[ -f "$XDG_CONFIG_HOME/efuseek/config.toml" ]] && break
  sleep 0.1
done
cmp config.default.toml "$XDG_CONFIG_HOME/efuseek/config.toml"
kill -0 "$pid"
printf 'Native window verified: %s\n' "$title"
