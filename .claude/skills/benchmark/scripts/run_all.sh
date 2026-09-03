#!/usr/bin/env bash
# Run every benchmark row currently tracked in BENCHMARK.md, in order, and
# print labeled TSV output for each. Read the output and transcribe the
# numbers into BENCHMARK.md's tables by hand — this script does not edit
# the file itself (measurements need a human/agent sanity check first,
# per BENCHMARK.md's "Reproduce, don't eyeball" methodology).
#
# Requires: build_release.sh has been run first (or pass --build to do it here).
# Requires: ~/source/reference-repos/{hermes-agent,oh-my-pi,repowise} and
#           ~/source/varde checked out (skips build_scale/extract rows with a
#           warning if missing).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

if [ "${1:-}" = "--build" ]; then
  ./build_release.sh
fi

echo "## find_pattern"
./find_pattern.sh "console.log (hermes-agent)" "$HOME/source/reference-repos/hermes-agent" 'console.log($MSG)' typescript ts
./find_pattern.sh "console.log (oh-my-pi)"     "$HOME/source/reference-repos/oh-my-pi"     'console.log($MSG)' typescript ts
./find_pattern.sh "console.log (repowise)"     "$HOME/source/reference-repos/repowise"     'console.log($MSG)' typescript ts
./find_pattern.sh "print (hermes-agent)"       "$HOME/source/reference-repos/hermes-agent" 'print($MSG)' python python
./find_pattern.sh "print (oh-my-pi)"           "$HOME/source/reference-repos/oh-my-pi"     'print($MSG)' python python
./find_pattern.sh "print (repowise)"           "$HOME/source/reference-repos/repowise"     'print($MSG)' python python
./find_pattern.sh "throw new Error (varde/src)" "$HOME/source/varde/src" 'throw new Error($MSG)' typescript ts

echo
echo "## symbols_in_file vs ast-grep outline"
./symbols_in_file.sh "$HOME/source/varde" src/core/subagents-lib.ts subagents-lib.ts

echo
echo "## scan pattern-rule pipeline"
./scan_pattern_rule.sh

echo
echo "## build (index) — cold/warm scale targets"
./build_scale.sh

echo
echo "## extract (parse-cost proxy)"
./extract.sh

echo
echo "## query-path freshness (ensure_fresh no-op, both changed_files paths)"
./git_perf.sh

echo
echo "## other query modes (baseline, no ast-grep equivalent)"
./other_modes.sh "$HOME/source/reference-repos/hermes-agent" hermes_bootstrap.py main
./other_modes.sh "$HOME/source/reference-repos/oh-my-pi" scripts/host-detect.ts main
./other_modes.sh "$HOME/source/reference-repos/repowise" scripts/validate_quality.py main
