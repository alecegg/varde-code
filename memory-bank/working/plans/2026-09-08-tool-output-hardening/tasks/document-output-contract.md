---
type: task
parent: 2026-09-08-tool-output-hardening
status: done
verified: passed
depends_on: ["compact-dense-results", "apply-command-output-policy"]
modifies:
  - docs/CLI.md
creates:
  - memory-bank/knowledge/reference/machine-output-contract.md
---

# Document the machine-output contract

Document the uniform envelope, error behavior, path policy, compact payload
rules, truncation fields, and the nav-map text exception.

Test approach: documentation review. The source and contract tests define truth.

#### Out of scope

- New CLI commands.
- Installing or publishing the CLI.

#### Verification

- assert: rg -n 'ok.*data.*meta|truncat|nav_map.*format text' docs/CLI.md memory-bank/knowledge/reference/machine-output-contract.md → documented contract terms are present
- retrieve: crates/varde-code/src/query/output.rs crates/varde-code/src/main.rs → documentation matches implementation

#### Progress

- Documented and source-verified the common envelope, compact policy, truncation, status rules, and nav-map text exception.
