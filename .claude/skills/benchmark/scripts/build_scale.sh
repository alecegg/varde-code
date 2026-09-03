#!/usr/bin/env bash
# Cold/warm `build` timing on the 3 scale repos in ~/source/reference-repos.
# Requires the repos to exist locally (see BENCHMARK.md "Scale build targets").
#
# Usage: build_scale.sh [repo ...]   (defaults to all 3)
#
# Prints one TSV line per repo:
#   repo  cold_ms  warm_ms
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_release_bin

if [ $# -gt 0 ]; then repos=("$@"); else repos=(hermes-agent oh-my-pi repowise); fi

for repo in "${repos[@]}"; do
  repo_path="$REF_REPOS/$repo"
  if [ ! -d "$repo_path" ]; then
    echo "SKIP $repo: not found at $repo_path" >&2
    continue
  fi
  clear_varde_cache "$repo"
  cold_ms=$(time_ms "$BIN" build --repo-root "$repo_path")
  warm_ms=$(time_ms "$BIN" build --repo-root "$repo_path")
  printf '%s\t%sms\t%sms\n' "$repo" "$cold_ms" "$warm_ms"
done
