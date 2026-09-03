#!/usr/bin/env bash
# Time a `build` (indexing) run for one repo (cold + warm) and diff against
# the previous watch_build.sh call for that repo. Same tight tuning-loop
# idea as watch.sh, but for `build` instead of a query mode — `build` isn't
# one of rank.sh's ranked modes (different CLI shape, and there's nothing to
# rank it against, just one thing to iterate on), so it gets its own script.
#
# Usage: watch_build.sh <repo-root> [cache-name]
#
# Baselines are cached in $TMPDIR/varde-profile-watch/, keyed by a hash of
# repo-root, so switching repos mid-session doesn't clobber another repo's
# baseline. Delete that directory to reset. Shares the same state dir as
# watch.sh but distinct keys (build vs query-mode calls never collide).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source "../../benchmark/scripts/lib.sh"
require_release_bin

repo_root="$1" cache_name="${2:-$(basename "$1")}"

state_dir="${TMPDIR:-/tmp}/varde-profile-watch"
mkdir -p "$state_dir"
key=$(printf 'build|%s' "$repo_root" | shasum -a 1 | cut -d' ' -f1)
state_file="$state_dir/$key"

clear_varde_cache "$cache_name"
cold=$(time_ms "$BIN" build --repo-root "$repo_root")
warm=$(time_ms "$BIN" build --repo-root "$repo_root")

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
  echo "(no previous baseline for this repo — this run is now the baseline)"
fi

printf '%s\t%s\n' "$cold" "$warm" > "$state_file"
