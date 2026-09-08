---
type: task
parent: 2026-09-08-tool-output-hardening
status: done
verified: passed
depends_on: ["apply-command-output-policy"]
modifies:
  - crates/varde-code/src/query/find_pattern.rs
  - crates/varde-code/src/scan_cli.rs
  - crates/varde-code/src/test_cli.rs
  - crates/varde-code/src/rules/pattern.rs
  - crates/varde-code/src/rules/test_runner.rs
creates: []
---

# Align internal tests with the output contract

Update stale test-only envelope assertions. Preserve the implemented machine
output contract. The full package test suite must pass.

#### Out of scope

- Changing query, scan, or rule semantics.

#### Verification

- assert: cargo test -p varde-code --quiet → all package tests pass

#### Progress

- Aligned internal find-pattern consumers and stale envelope assertions; full package suite passes.
