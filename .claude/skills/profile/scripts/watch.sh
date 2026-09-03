#!/usr/bin/env bash
# Time a single query mode (cold + warm) and diff against the previous
# watch.sh call for the same (mode, repo-root, json-args) key. Meant to be
# re-run after each edit/rebuild while tuning one specific tool — rank.sh
# tells you *which* mode to look at, this is the tight loop once you're
# looking at it.
#
# Usage: watch.sh <mode> <repo-root> <json-args> [cache-name]
#
# Baselines are cached in $TMPDIR/varde-profile-watch/, keyed by a hash of
# (mode, repo-root, json-args), so switching modes/repos mid-session
# doesn't clobber another tool's baseline. Delete that directory to reset.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source "../../benchmark/scripts/lib.sh"
require_release_bin

mode="$1" repo_root="$2" json="$3" cache_name="${4:-$(basename "$2")}"

state_dir="${TMPDIR:-/tmp}/varde-profile-watch"
mkdir -p "$state_dir"
key=$(printf '%s' "$mode|$repo_root|$json" | shasum -a 1 | cut -d' ' -f1)
state_file="$state_dir/$key"

clear_varde_cache "$cache_name"
cmd=("$BIN" "$mode" --json "$json")
cold=$(time_ms "${cmd[@]}")
warm=$(min_of_3 "${cmd[@]}")

echo "mode:  $mode"
echo "repo:  $repo_root"
echo "cold:  ${cold}ms"
echo "warm:  ${warm}ms"

if [ -f "$state_file" ]; then
  prev_cold=$(cut -f1 "$state_file")
  prev_warm=$(cut -f2 "$state_file")
  cold_delta=$((cold - prev_cold))
  warm_delta=$((warm - prev_warm))
  echo "---"
  echo "prev cold: ${prev_cold}ms (delta ${cold_delta}ms)"
  echo "prev warm: ${prev_warm}ms (delta ${warm_delta}ms)"
else
  echo "---"
  echo "(no previous baseline for this mode/repo/json — this run is now the baseline)"
fi

printf '%s\t%s\n' "$cold" "$warm" > "$state_file"
