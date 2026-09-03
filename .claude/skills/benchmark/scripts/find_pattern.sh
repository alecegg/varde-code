#!/usr/bin/env bash
# Compare varde-code find_pattern vs ast-grep run for one pattern/repo/lang.
#
# Usage: find_pattern.sh <label> <path> <pattern> <ast-grep-lang> <varde-lang>
#
# Prints one TSV line:
#   label  varde_cold_ms  varde_warm_ms  astgrep_ms  varde_matches  astgrep_matches
#
# varde-code find_pattern has no persisted index, so "cold" is just first-touch
# (page cache not warmed for this repo this session) — same command as warm,
# run once before the min-of-3 warm loop, matching BENCHMARK.md's convention.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_release_bin

label="$1" path="$2" pattern="$3" ag_lang="$4" varde_lang="$5"

varde_json="{\"path\":\"$path\",\"pattern\":$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$pattern"),\"language\":\"$varde_lang\"}"

varde_cmd=("$BIN" find_pattern --json "$varde_json")
astgrep_cmd=(ast-grep run -p "$pattern" -l "$ag_lang" "$path" --json=compact)

varde_cold=$(time_ms "${varde_cmd[@]}")
varde_warm=$(min_of_3 "${varde_cmd[@]}")
astgrep_ms=$(min_of_3 "${astgrep_cmd[@]}")

varde_matches=$("${varde_cmd[@]}" | python3 -c 'import json,sys;print(len(json.load(sys.stdin).get("data") or []))')
astgrep_matches=$("${astgrep_cmd[@]}" | python3 -c 'import json,sys;print(len(json.load(sys.stdin)))')

printf '%s\t%sms\t%sms\t%sms\t%s\t%s\n' \
  "$label" "$varde_cold" "$varde_warm" "$astgrep_ms" "$varde_matches" "$astgrep_matches"
