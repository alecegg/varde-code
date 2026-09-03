#!/usr/bin/env bash
# Run the scan pattern-rule pipeline perf test (varde-code vs ast-grep, interleaved,
# median of 5, release mode only — see BENCHMARK.md `## scan pattern-rule pipeline`).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../../.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --release -p varde-code --test pattern_rule_perf -- --ignored --nocapture
