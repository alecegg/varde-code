---
title: CORRECTNESS findings
type: review-category
description: CORRECTNESS review findings
resource: memory-bank/working/plans/2026-09-08-tool-output-hardening/2026-09-08-output-contract-review
tags: [review, correctness]
timestamp: 2026-09-08T00:00:00+02:00
created_at: 2026-09-08T00:00:00+02:00
edited_at: 2026-09-08T00:00:00+02:00
---

# CORRECTNESS

## [CORRECTNESS-001] Build file lists bypass path compaction

**Severity:** high
**Label:** auto-fix
**Disposition:** fix
**Location:** `crates/varde-code/src/main.rs:1135`

### Summary

`changedFiles` and `changedFilesSample` are absent from the path-field
allowlist. Build can therefore emit absolute repository paths while reporting
`meta.compact: true`.

### Solutions

1. Add both changed-file fields to the scoped path policy.
2. Cover absolute-root build output in the command contract test.

## [CORRECTNESS-002] A new envelope test contradicts dbPath policy

**Severity:** high
**Label:** auto-fix
**Disposition:** fix
**Location:** `crates/varde-code/tests/query_envelope.rs:97`

### Summary

The test expects compact output from a dbPath-only call. The documented
contract leaves such calls unchanged, so the new assertion fails.

### Solutions

1. Expect `meta.compact: false` for dbPath-only input.
2. Add `repoRoot` when testing path compaction.

## [CORRECTNESS-003] Batch hides nested truncation

**Severity:** medium
**Label:** auto-fix
**Disposition:** fix
**Location:** `crates/varde-code/src/query/output.rs:84`

### Summary

Batch metadata only checks its top-level guide. A truncated child result leaves
the outer `meta.truncated` false, despite an incomplete aggregate result.

### Solutions

1. Aggregate child truncation into batch metadata.
2. Add a nested find-pattern regression test.

## [CORRECTNESS-004] Batch child failures have a different shape

**Severity:** medium
**Label:** auto-fix
**Disposition:** fix
**Location:** `crates/varde-code/src/query/mod.rs:516`

### Summary

Batch failure rows place `error` beside `ok`. The contract requires structured
errors in `data.error` and metadata on each machine result.

### Solutions

1. Render every batch child through the shared envelope.
2. Add a mixed-success batch contract test.
