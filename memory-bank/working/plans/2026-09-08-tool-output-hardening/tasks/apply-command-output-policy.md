---
type: task
parent: 2026-09-08-tool-output-hardening
status: done
verified: passed
depends_on: ["migrate-machine-output"]
modifies:
  - crates/varde-code/src/main.rs
  - crates/varde-code/src/query/output.rs
  - crates/varde-code/tests/cli_output_contract.rs
creates: []
---

# Apply output policy to command payloads

Route command handlers with repository or path input through the shared output
policy. Build, extract, scan, test, and rule management must correctly report
compactness and safely relativize repository paths. Do not falsely claim output
policy metadata for commands without relevant path input.

Test approach: contract regression tests first. The shared renderer exists.

#### Out of scope

- New path inputs for skills, hooks, or watch controls.
- Altering payload semantics without repository context.

#### Verification

- assert: cargo test -p varde-code --test cli_output_contract → path policy and envelope tests pass
- assert: cargo test -p varde-code --test output_compaction → compact metadata tests pass

#### Progress

- Routed scoped path-aware commands through input-based policy rendering; contract and compaction checks pass.
