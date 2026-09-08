---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/native-functional-closures
status: backlog
verified: pending
depends_on: []
modifies:
  - crates/varde-code/src/extract/langs/go.rs
  - crates/varde-code/src/extract/langs/rust.rs
  - crates/varde-code/src/extract/langs/cpp.rs
  - crates/varde-code/src/extract/langs/python.rs
  - crates/varde-code/src/extract/langs/haskell.rs
  - crates/varde-code/src/extract/langs/mod.rs
creates: []
---

Contain anonymous closures across native and functional languages.

Emit CallableBoundary entities for Go literals, Rust closures, C++ lambdas, Python lambdas, and Haskell local bindings. Audit additional supported language closure scopes while preserving named function attribution.

#### Out of scope

- Anonymous closure findings.
- Call resolution changes.

#### Verification

- assert: cargo test -p varde-code --lib go → passes
- assert: cargo test -p varde-code --lib rust → passes
- assert: cargo test -p varde-code --lib cpp → passes
- assert: cargo test -p varde-code --lib python → passes
- assert: cargo test -p varde-code --lib haskell → passes
- assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes
- assert: git diff --check → passes

#### Progress

