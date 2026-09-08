---
title: CORRECTNESS findings
type: review-category
description: CORRECTNESS review findings
resource: memory-bank/working/reviews/2026-09-07-build-nav-map-section-budget-01d736d
tags:
  - review
  - correctness
timestamp: 2026-09-07T00:00:00+02:00
created_at: 2026-09-07T00:00:00+02:00
edited_at: 2026-09-07T00:00:00+02:00
---

# CORRECTNESS

## [CORRECTNESS-001] Cycle payload can starve every reserved section

**Severity:** high
**Label:** triage
**Disposition:** fix
**Location:** `crates/varde-code/src/query/nav_map.rs:302`

### Summary

`fixed_cost` includes the complete, capped `module_layers.cycles` payload and
is subtracted before the first-item reserve. A map with twenty long cycles can
exhaust a valid tight `maxTokensEstimate`, setting `section_reserve` to zero.
All populated top-level sections and module edges are then truncated, defeating
the new section-preservation behavior.

### Solutions

1. **Budget cycles independently**
   Keep only the cycles that fit a bounded module-layer allowance before
   calculating reserves for the other orientation sections.
2. **Reserve sections before cycle payload**
   Allocate the first-item shares before charging optional cycle details.

## [CORRECTNESS-002] Fixed map overhead exceeds small budgets

**Severity:** medium
**Label:** triage
**Disposition:** fix
**Location:** `crates/varde-code/src/query/nav_map.rs:302`

### Summary

The budgeter charges only `module_layers` fixed cost. It never reserves tokens
for the six top-level section keys, their empty arrays, or `guide`. Therefore a
small `maxTokensEstimate` can return a structurally non-empty map whose
estimated size already exceeds the requested budget, even after every item is
trimmed. This violates the task requirement that the entire rendered map stay
within `maxTokensEstimate`.

### Solutions

1. **Charge baseline envelope cost first**
   Estimate the map with all variable arrays empty and subtract that cost before
   allocating section items.
2. **Define the minimum viable budget**
   If the structural baseline exceeds the requested budget, return the smallest
   valid map with an explicit documented lower-bound behavior.
