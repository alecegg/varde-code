---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/jvm-swift-callables
status: backlog
verified: pending
depends_on:
  - kotlin-accessors
modifies:
  - crates/varde-code/src/extract/langs/swift.rs
  - crates/varde-code/src/extract/langs/mod.rs
creates: []
---

Emit Functions for Swift computed-property and subscript accessors.

#### Out of scope

- Swift resolution changes.

#### Verification

- assert: cargo test -p varde-code --lib swift → passes
- assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes
- assert: git diff --check → passes

#### Progress

