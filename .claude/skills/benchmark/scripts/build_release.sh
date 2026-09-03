#!/usr/bin/env bash
# Build the release binary all other benchmark scripts depend on.
# Run this first, once, before any other script in this skill.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../../.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p varde-code
