---
type: task
parent: 2026-09-03-test-home-isolation-graph-timeout
status: done
verified: passed
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

- Profiled `query_graph_counts`: warm runtime was 0.10s, so no graph change was justified; workspace tests passed in 16.81s after HOME recovery.
