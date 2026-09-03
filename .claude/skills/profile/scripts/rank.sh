#!/usr/bin/env bash
# Profile every query mode (same set as benchmark/scripts/other_modes.sh)
# across reference repos and rank them: slowest-first, plus a scaling-flag
# table that catches "disproportionately slow on the bigger repo" bugs
# like the nav_map/resolved_edges-index regression this skill was built
# to catch faster next time.
#
# Usage: rank.sh [repo-root file symbol]...
#   Any number of (repo-root, file-path, symbol-name) triples. Defaults to
#   the three BENCHMARK.md reference repos if none are given.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source "../../benchmark/scripts/lib.sh"
require_release_bin

MODES=(
  get_symbol filter_symbols hotspots tests_for_file find_imports
  dependencies dependents blast_radius symbol_blast_radius type_hierarchy
  explore map_symbol map_path map_file symbols_in_files clusters
  detect_changes context_pack nav_map
)

if [ "$#" -eq 0 ]; then
  set -- \
    "$HOME/source/reference-repos/hermes-agent" hermes_bootstrap.py main \
    "$HOME/source/reference-repos/oh-my-pi" scripts/host-detect.ts main \
    "$HOME/source/reference-repos/repowise" scripts/validate_quality.py main
fi

data_file="$(mktemp "${TMPDIR:-/tmp}/varde-profile-rank.XXXXXX")"
trap 'rm -f "$data_file"' EXIT

json_for_mode() {
  local mode="$1" repo_root="$2" file_path="$3" symbol_name="$4"
  case "$mode" in
    get_symbol|symbol_blast_radius|type_hierarchy|map_symbol)
      echo "{\"repoRoot\":\"$repo_root\",\"name\":\"$symbol_name\"}" ;;
    filter_symbols)
      echo "{\"repoRoot\":\"$repo_root\",\"file\":\"$file_path\"}" ;;
    hotspots|clusters|detect_changes)
      echo "{\"repoRoot\":\"$repo_root\"}" ;;
    tests_for_file|find_imports|dependencies|dependents|blast_radius|map_file)
      echo "{\"repoRoot\":\"$repo_root\",\"filePath\":\"$file_path\"}" ;;
    explore)
      echo "{\"repoRoot\":\"$repo_root\",\"query\":{\"params\":{\"input\":\"$file_path\"}}}" ;;
    map_path)
      echo "{\"repoRoot\":\"$repo_root\",\"sourceFile\":\"$file_path\",\"targetFile\":\"$file_path\"}" ;;
    symbols_in_files)
      echo "{\"repoRoot\":\"$repo_root\",\"filePaths\":[\"$file_path\"]}" ;;
    context_pack)
      echo "{\"repoRoot\":\"$repo_root\",\"query\":\"$symbol_name\"}" ;;
    nav_map)
      echo "{\"repoRoot\":\"$repo_root\"}" ;;
  esac
}

while [ "$#" -gt 0 ]; do
  repo_root="$1" file_path="$2" symbol_name="$3"
  shift 3
  if [ ! -d "$repo_root" ]; then
    echo "SKIP: $repo_root not checked out" >&2
    continue
  fi
  repo_name="$(basename "$repo_root")"
  file_count=$(git -C "$repo_root" ls-files 2>/dev/null | wc -l | tr -d ' ')
  echo "profiling $repo_name ($file_count files)..." >&2
  for mode in "${MODES[@]}"; do
    json=$(json_for_mode "$mode" "$repo_root" "$file_path" "$symbol_name")
    clear_varde_cache "$repo_name"
    cmd=("$BIN" "$mode" --json "$json")
    cold=$(time_ms "${cmd[@]}")
    warm=$(min_of_3 "${cmd[@]}")
    printf '%s\t%s\t%s\t%s\t%s\n' "$mode" "$repo_name" "$file_count" "$cold" "$warm" >> "$data_file"
  done
done

python3 analyze.py "$data_file"
