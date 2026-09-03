#!/usr/bin/env bash
# Compare varde-code symbols_in_file (warm, query-only) vs ast-grep outline.
#
# Usage: symbols_in_file.sh <repo-root> <file-path> [repo-cache-name]
#
# Prints one TSV line:
#   index_build_ms  varde_warm_query_ms  astgrep_ms
#
# repo-cache-name defaults to the basename of repo-root and is used to clear
# ~/.config/varde-code/repos/*<name>* before the cold index build, so the
# index_build_ms column is a true cold number.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_release_bin

repo_root="$1" file_path="$2" cache_name="${3:-$(basename "$1")}"

clear_varde_cache "$cache_name"

json="{\"repoRoot\":\"$repo_root\",\"filePath\":\"$file_path\"}"
cmd=("$BIN" query symbols_in_file --json "$json")

index_build_ms=$(time_ms "${cmd[@]}")
varde_warm_ms=$(min_of_3 "${cmd[@]}")
astgrep_ms=$(min_of_3 ast-grep outline "$repo_root/$file_path")

printf '%sms\t%sms\t%sms\n' "$index_build_ms" "$varde_warm_ms" "$astgrep_ms"
