---
type: task
parent: 2026-09-08-tool-output-hardening
status: done
verified: passed
depends_on: ["output-contract-foundation"]
modifies:
  - crates/varde-code/src/main.rs
  - crates/varde-code/src/scan_cli.rs
  - crates/varde-code/src/test_cli.rs
  - crates/varde-code/src/watch.rs
  - crates/varde-code/tests/cli_smoke.rs
creates:
  - crates/varde-code/tests/cli_output_contract.rs
---

# Migrate every machine command to the shared envelope

Route build, extract, scan, test, rule management, skill management, hook
management, and watch control commands through the shared renderer. Preserve
each command's exit-status contract. Keep nav-map text output human-readable.

Test approach: process-level smoke tests first. Command handlers own stdout.

#### Out of scope

- Changing each command's domain payload contents.
- Long-running human watch logs.

#### Verification

- assert: cargo test -p varde-code --test cli_output_contract → all command families use the contract
- assert: cargo test -p varde-code --test cli_smoke → CLI help behavior passes

#### Progress

- Routed non-query machine commands through the shared envelope and verified process-level output contracts.
