---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/attribution-core
status: backlog
verified: pending
depends_on: []
modifies:
  - crates/varde-code/src/model.rs
  - crates/varde-code/src/extract/entity.rs
  - crates/varde-code/src/persist.rs
creates: []
---

Persist non-reportable callable boundaries.

Add a stable entity representation for anonymous callable spans. Preserve existing kind discriminants. Do not create standalone findings.

#### Out of scope

- Language-specific scope mapping.
- Rule SQL changes.

#### Verification

- assert: cargo test -p varde-code --lib model persist → passes
- assert: git diff --check → passes

#### Progress

