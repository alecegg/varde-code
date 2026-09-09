---
type: task
parent: 2026-09-09-scan-lossless-output
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/scan_cli.rs
  - crates/varde-code/tests/scan_cli.rs
  - crates/varde-code/src/query/find_pattern.rs
  - crates/varde-code/src/rules/pattern.rs
creates: []
---

Remove scan finding truncation

Remove the scan-only `FINDINGS_LIMIT` behavior and its metadata.
Cover more findings than the former limit through the public CLI.

#### Out of scope

- Limits in `find_pattern` or other commands.

#### Verification

- assert: cargo test -p varde-code --test scan_cli → scan integration tests pass, including more than 100 findings without truncation metadata.
- assert: cargo test -p varde-code → crate test suite passes.
- retrieve: crates/varde-code/src/scan_cli.rs → scan serializes every finding without a limit helper.

#### Progress

- Removed scan finding truncation and verified 101 public CLI findings return losslessly.
