---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/jvm-swift-callables
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/extract/langs/kotlin.rs
creates: []
---

Emit Functions for Kotlin property accessors.

Cover getters and setters with stable names and body spans.

#### Out of scope

- Kotlin resolution changes.

#### Verification

- assert: cargo test -p varde-code --lib kotlin → passes
- assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes
- assert: git diff --check → passes

#### Progress

- Emitted body-spanning getter and setter Functions with stable property names.
