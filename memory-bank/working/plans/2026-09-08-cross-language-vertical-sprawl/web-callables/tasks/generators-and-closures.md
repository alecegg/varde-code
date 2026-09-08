---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/web-callables
status: backlog
verified: pending
depends_on: []
modifies:
  - crates/varde-code/src/extract/langs/javascript.rs
  - crates/varde-code/src/extract/langs/ts.rs
  - crates/varde-code/src/extract/langs/tsx.rs
  - crates/varde-code/src/extract/langs/mod.rs
creates: []
---

Cover web generators and anonymous closures.

Emit Functions for named generators. Emit callable boundaries for anonymous functions and arrows. Preserve TSX delegation.

#### Out of scope

- Anonymous closure findings.

#### Verification

- assert: cargo test -p varde-code --lib javascript → passes
- assert: cargo test -p varde-code --lib ts → passes
- assert: cargo test -p varde-code --lib tsx → passes
- assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes
- assert: git diff --check → passes

#### Progress

