---
type: task
parent: 2026-09-08-tool-output-hardening
status: done
verified: passed
depends_on: ["migrate-machine-output"]
modifies:
  - crates/varde-code/src/main.rs
  - crates/varde-code/src/model.rs
  - crates/varde-code/src/query/find_pattern.rs
  - crates/varde-code/src/scan_cli.rs
  - crates/varde-code/src/test_cli.rs
creates:
  - crates/varde-code/tests/output_compaction.rs
---

# Compact dense results with explicit summaries

Remove repeated file-path data from dense extract output. Bound high-volume
find-pattern, scan, and rule-test result lists. Report shown, total, and
truncated metadata. Retain enough IDs and spans for callers to navigate.

Test approach: failing output-shape tests first. These payloads are public.

#### Out of scope

- Semantic ranking changes.
- Pagination storage or resumable cursors.

#### Verification

- assert: cargo test -p varde-code --test output_compaction → compact payload and truncation tests pass
- assert: cargo test -p varde-code --test find_pattern → pattern results retain expected matches
- assert: cargo test -p varde-code --test rule_pack_loading → rule behavior remains covered

#### Progress

- Capped find-pattern, scan, and rule-test lists with explicit summaries; extract now uses its file table.
