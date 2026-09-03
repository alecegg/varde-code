---
type: friction-item
source: varde-review
status: open
signal: obstacle
created: 2026-09-03
head_sha: ad85378
session_label: review-whole-repo
---

# Sandbox configuration lock blocks test completion

An isolated repository review ran `cargo test -p varde-code --tests`.
It reached 456 passing tests, then failed remaining tests because the sandbox
could not create the configured user configuration lock directory. The initial
test runner also exceeded its 120-second limit during `query_graph_counts`.

This prevents a clean repository-wide test verdict during review. The test
harness should support a writable, per-test configuration directory without
relying on the user configuration location.
