#!/usr/bin/env bash
# Query-path freshness cost: ensure_fresh no-op, default walk vs opt-in
# git-status fast path, on a 201-file synthetic fixture. Wraps the existing
# `examples/git_perf.rs` (both paths, one process, min-of-3, prints ms/call).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../../.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo run --release -p varde-code --example git_perf
