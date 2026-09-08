---
type: task
parent: 2026-09-08-cross-language-vertical-sprawl/native-functional-closures
status: backlog
verified: pending
depends_on:
  - closure-boundaries
modifies:
  - crates/varde-code/src/extract/langs/php.rs
  - crates/varde-code/src/extract/langs/ruby.rs
  - crates/varde-code/src/extract/langs/lua.rs
  - crates/varde-code/src/extract/langs/scala.rs
  - crates/varde-code/src/extract/langs/elixir.rs
creates: []
---

Contain remaining anonymous closure calls.

Add CallableBoundary emitters for PHP, Ruby, Lua, Scala, and Elixir grammar forms. Preserve named function attribution.

#### Out of scope

- Anonymous closure findings.

#### Verification

- assert: cargo test -p varde-code --lib php → passes
- assert: cargo test -p varde-code --lib ruby → passes
- assert: cargo test -p varde-code --lib lua → passes
- assert: cargo test -p varde-code --lib scala → passes
- assert: cargo test -p varde-code --lib elixir → passes
- assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes
- assert: git diff --check → passes

#### Progress

