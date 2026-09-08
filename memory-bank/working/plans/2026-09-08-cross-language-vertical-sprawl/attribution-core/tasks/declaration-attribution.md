---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/attribution-core
status: backlog
verified: pending
depends_on:
  - callable-boundaries
modifies:
  - crates/varde-code/src/extract/langs/cs.rs
  - crates/varde-code/src/extract/langs/mod.rs
  - crates/varde-code/src/extract/mod.rs
  - crates/varde-code/src/rules/builtin/vertical_slice_sprawl.toml
  - crates/varde-code/src/rules/sql.rs
creates: []
---

Import declaration-safe C# attribution.

Cherry-pick commits `a93866e` and `5c69763`. Extend rule exclusion to nested callable boundaries. Preserve named nested function findings and thresholds.

#### Out of scope

- New C# call resolution rules.
- Other language syntax coverage.

#### Verification

- assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes
- assert: cargo test -p varde-code --lib cs → passes
- assert: git diff --check → passes

#### Progress

