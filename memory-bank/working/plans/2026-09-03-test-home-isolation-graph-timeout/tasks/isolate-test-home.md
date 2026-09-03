---
type: task
parent: 2026-09-03-test-home-isolation-graph-timeout
status: done
verified: passed
depends_on: []
modifies:
  - crates/varde-code/src/build.rs
  - crates/varde-code/src/query/test_support.rs
  - crates/varde-code/src/scan_cli.rs
  - crates/varde-code/src/slice.rs
  - crates/varde-code/src/watch.rs
  - crates/varde-code/tests/scan_cli.rs
creates: []
---

# Isolate test HOME overrides

Centralize the test-only HOME override behind one crate-wide lock and RAII
restoration. Replace direct test mutations in each affected module.

#### Out of scope

- Changing production config-path behavior.

#### Verification

- assert: `cargo test -p varde-code --lib` → all unit tests pass without HOME-related failures
- retrieve: test-only HOME helper and all former direct HOME writes → one shared isolation mechanism

#### Progress

- Centralized test HOME mutation behind an RAII guard; all 506 library tests pass.
