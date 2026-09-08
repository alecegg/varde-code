---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/dart-callables
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/extract/langs/dart.rs
creates: []
---

Emit body-spanning Dart Function entities.

Cover functions, constructors, factories, getters, and setters. Preserve stable names. Add extractor and vertical-sprawl regressions.

#### Out of scope

- Dart call resolution.

#### Verification

- assert: cargo test -p varde-code --lib dart → passes
- assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes
- assert: git diff --check → passes

#### Progress

- Dart callable entities now span executable declaration bodies.
