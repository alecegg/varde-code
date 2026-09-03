---
type: plan
title: Isolate test homes and bound graph-test runtime
status: active
goal: Make the workspace test suite deterministic and complete within its review time budget.
authoring_commit: ad85378
depends_on: []
---

# Isolate test homes and bound graph-test runtime

## Problem

Unit tests mutate process-global `HOME` without one shared guard. The graph
test also exceeds the review runner's time limit.

## Solution

Centralize test-only HOME override and restoration. Then identify and remove
the graph-test runtime bottleneck without weakening its asserted behavior.

## Acceptance criteria

- [ ] Given concurrent unit tests, when they override HOME, then every override is serialized and restored after its test.
- [ ] Given the graph-count test, when it runs in the workspace suite, then it completes within the 120-second review budget.
- [ ] Given the workspace test suite, when it runs, then it completes without HOME-derived configuration failures.
