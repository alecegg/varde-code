#!/usr/bin/env bash
# Shared helpers for benchmark scripts. Source this, don't run it directly.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BIN="$REPO_ROOT/target/release/varde-code"
REF_REPOS="$HOME/source/reference-repos"

require_release_bin() {
  if [ ! -x "$BIN" ]; then
    echo "ERROR: release binary not found at $BIN — run build_release.sh first" >&2
    exit 1
  fi
}

# now_ms — current time in milliseconds (portable: macOS `date` lacks %N).
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# time_ms CMD... — wall-clock a command in milliseconds, discard stdout.
time_ms() {
  local start end
  start=$(now_ms)
  "$@" >/dev/null 2>&1
  end=$(now_ms)
  echo $((end - start))
}

# min_of_3 CMD... — run a command 3x, print the minimum wall-clock ms.
min_of_3() {
  local a b c m
  a=$(time_ms "$@")
  b=$(time_ms "$@")
  c=$(time_ms "$@")
  m=$a
  [ "$b" -lt "$m" ] && m=$b
  [ "$c" -lt "$m" ] && m=$c
  echo "$m"
}

clear_varde_cache() {
  local repo_name="$1"
  rm -rf "$HOME/.config/varde-code/repos/"*"$repo_name"*
}
