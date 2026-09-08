---
title: CORRECTNESS findings
type: review-category
description: CORRECTNESS review findings
resource: memory-bank/working/plans/2026-09-07-nav-map-followups/nest-and-diversity/2026-09-08-nav-map-followups-review
tags:
  - review
  - correctness
timestamp: 2026-09-08T00:00:00+02:00
created_at: 2026-09-08T00:00:00+02:00
edited_at: 2026-09-08T00:00:00+02:00
---

# CORRECTNESS

## [CORRECTNESS-001] Group entrypoints by language, not extension

**Severity:** medium
**Label:** auto-fix
**Disposition:** fix
**Location:** `crates/varde-code/src/query/nav_map.rs:184`

### Summary

The round-robin groups `js`, `mjs`, `cjs`, and `jsx` separately. They are all
JavaScript in the entrypoint detector. A JavaScript-heavy monorepo can therefore
spend multiple capped positions before a genuinely different language appears.
This violates the language-diversity acceptance criterion.

### Solutions

1. **Canonicalize extension groups**
   Map JavaScript-family extensions to one JavaScript group before round-robin.
2. **Add a regression test**
   Prove JavaScript variants cannot displace a later Python entrypoint.

Fixed in `944ad50`; focused nav-map tests and Clippy pass.
