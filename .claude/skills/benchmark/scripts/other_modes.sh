#!/usr/bin/env bash
# Cold/warm absolute baselines for query modes that have no ast-grep
# equivalent (symbol lookup, dependency graphs, hotspots, etc). Not a
# ratio-vs-ast-grep comparison — see BENCHMARK.md's "Other query modes"
# section.
#
# Usage: other_modes.sh <repo-root> <file-path> <symbol-name> [cache-name]
#
# <file-path>: representative source file (relative to repo-root), used for
#   the filePath-based modes (tests_for_file, find_imports, dependencies,
#   dependents, blast_radius, map_file, map_path source==target, explore
#   seed).
# <symbol-name>: a symbol name that resolves in this repo (any binding or
#   reference — get_symbol/symbol_blast_radius/type_hierarchy/map_symbol
#   don't care about function vs. class, just that `name` matches a row),
#   used for get_symbol, filter_symbols (via file), symbol_blast_radius,
#   type_hierarchy, map_symbol, context_pack (as the keyword query).
#
# Representative choices used in BENCHMARK.md:
#   hermes-agent  hermes_bootstrap.py   main
#   oh-my-pi      scripts/host-detect.ts main
#   repowise      scripts/validate_quality.py main
#
# For each mode below, prints one TSV line:
#   mode  cold_ms  warm_ms
#
# cold_ms = single run right after clearing the varde-code cache for this
# repo (first call rebuilds whichever slice(s) that mode reads — see
# FRESHNESS table in crates/varde-code/src/query/mod.rs).
# warm_ms = min-of-3 subsequent calls.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_release_bin

repo_root="$1" file_path="$2" symbol_name="$3" cache_name="${4:-$(basename "$1")}"

mode_group() {
  case "$1" in
    dependencies|dependents|find_imports|explore|blast_radius|symbol_blast_radius|type_hierarchy|map_path)
      echo graph ;;
    hotspots|clusters|context_pack|nav_map|detect_changes)
      echo report ;;
    *)
      echo query ;;
  esac
}

run_mode() {
  local mode="$1" json="$2" group
  group=$(mode_group "$mode")
  clear_varde_cache "$cache_name"
  local cmd=("$BIN" "$group" "$mode" --json "$json")
  local cold warm
  cold=$(time_ms "${cmd[@]}")
  warm=$(min_of_3 "${cmd[@]}")
  printf '%s\t%sms\t%sms\n' "$mode" "$cold" "$warm"
}

run_mode get_symbol "{\"repoRoot\":\"$repo_root\",\"name\":\"$symbol_name\"}"
run_mode filter_symbols "{\"repoRoot\":\"$repo_root\",\"file\":\"$file_path\"}"
run_mode hotspots "{\"repoRoot\":\"$repo_root\"}"
run_mode tests_for_file "{\"repoRoot\":\"$repo_root\",\"filePath\":\"$file_path\"}"
run_mode find_imports "{\"repoRoot\":\"$repo_root\",\"filePath\":\"$file_path\"}"
run_mode dependencies "{\"repoRoot\":\"$repo_root\",\"filePath\":\"$file_path\"}"
run_mode dependents "{\"repoRoot\":\"$repo_root\",\"filePath\":\"$file_path\"}"
run_mode blast_radius "{\"repoRoot\":\"$repo_root\",\"filePath\":\"$file_path\"}"
run_mode symbol_blast_radius "{\"repoRoot\":\"$repo_root\",\"name\":\"$symbol_name\"}"
run_mode type_hierarchy "{\"repoRoot\":\"$repo_root\",\"name\":\"$symbol_name\"}"
run_mode explore "{\"repoRoot\":\"$repo_root\",\"query\":{\"params\":{\"input\":\"$file_path\"}}}"
run_mode map_symbol "{\"repoRoot\":\"$repo_root\",\"name\":\"$symbol_name\"}"
run_mode map_path "{\"repoRoot\":\"$repo_root\",\"sourceFile\":\"$file_path\",\"targetFile\":\"$file_path\"}"
run_mode map_file "{\"repoRoot\":\"$repo_root\",\"filePath\":\"$file_path\"}"
run_mode symbols_in_files "{\"repoRoot\":\"$repo_root\",\"filePaths\":[\"$file_path\"]}"
run_mode clusters "{\"repoRoot\":\"$repo_root\"}"
run_mode detect_changes "{\"repoRoot\":\"$repo_root\"}"
run_mode context_pack "{\"repoRoot\":\"$repo_root\",\"query\":\"$symbol_name\"}"
run_mode nav_map "{\"repoRoot\":\"$repo_root\"}"
