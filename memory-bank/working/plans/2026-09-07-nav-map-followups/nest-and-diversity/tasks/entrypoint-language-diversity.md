---
type: task
parent: 2026-09-07-nav-map-followups/nest-and-diversity
status: done
verified: passed
depends_on:
  - nest-middleware-precision
modifies:
  - crates/varde-code/src/query/nav_map.rs
creates: []
---

Retain entrypoint language diversity

Round-robin the ranked nav-map entrypoints by source extension. Preserve each
extension's ranking. Keep the original detected list for flows and subsystems.

#### Out of scope

- Reordering other nav-map sections.
- Changing entrypoint detection.

#### Verification

- assert: `cargo test -p varde-code nav_map_tests` → entrypoint round-robin and default-budget diversity tests pass.
- assert: `cargo clippy -p varde-code --lib -- -D warnings` → no lint warnings.

#### Progress

- Interleaved displayed entrypoints by source extension and verified default-budget diversity.
