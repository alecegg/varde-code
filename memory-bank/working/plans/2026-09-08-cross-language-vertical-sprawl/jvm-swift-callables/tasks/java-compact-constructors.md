---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/jvm-swift-callables
status: done
verified: passed
depends_on:
  - swift-accessors
modifies:
  - crates/varde-code/src/extract/langs/java.rs
creates: []
---

Emit Functions for Java compact record constructors.

#### Out of scope

- Java resolution changes.

#### Verification

- assert: cargo test -p varde-code --lib java → passes
- assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes
- assert: git diff --check → passes

#### Progress

- Emitted named Functions for compact record constructors.
- Verified Java extraction and vertical sprawl tests.
