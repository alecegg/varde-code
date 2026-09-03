---
type: task
parent: 2026-09-03-test-home-isolation-graph-timeout
status: backlog
verified: pending
depends_on:
  - isolate-test-home
modifies:
  - crates/varde-code/src/query
  - crates/varde-code/tests
creates: []
---

# Bound graph-count test runtime

Profile the graph-count test after HOME isolation. Remove the measured runtime
bottleneck while retaining its graph-count coverage.

#### Out of scope

- Relaxing expected graph-count assertions.

#### Verification

- assert: `cargo test -p varde-code --test query_graph_counts` → passes within 120 seconds
- assert: `cargo test --workspace` → completes within 120 seconds without HOME-derived failures

#### Progress

