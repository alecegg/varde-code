---
title: CODE findings
type: review-category
description: CODE review findings
resource: memory-bank/working/reviews/2026-09-03-perf-find-pattern-literal-atom-prefilter-full-repo
tags:
  - review
  - code
timestamp: 2026-09-03T00:00:00+02:00
created_at: 2026-09-03T00:00:00+02:00
edited_at: 2026-09-03T12:00:00+02:00
---

# CODE

## [CODE-001] Pattern scans map most languages to Rust

**Severity:** high
**Label:** triage
**Disposition:** fix
**Location:** `crates/varde-code/src/rules/pattern.rs:333`

### Summary

The pattern-rule language mapper falls back to `rust` for supported languages
outside its ten explicit arms. A PHP, Ruby, C++, or Bash rule therefore scans
Rust files and misses its intended language files.

### Solutions

1. **Map every supported language explicitly**
   Use the canonical language mapping for each supported rule language.
2. **Add parameterized language tests**
   Assert a scoped rule examines files for every supported language.

## [CODE-002] Empty pattern-language lists scan no files

**Severity:** medium
**Label:** triage
**Disposition:** fix
**Location:** `crates/varde-code/src/rules/pattern.rs:223`

### Summary

The public `Rule.languages` contract defines an absent or empty list as all
languages. `Some(vec![])` instead produces no scans and no diagnostic.

### Solutions

1. **Normalize empty lists**
   Treat an empty list as the complete supported-language set.
2. **Add a contract regression test**
   Verify omitted and empty language lists yield identical matches.

## [CODE-003] Relative imports lose their local target

**Severity:** high
**Label:** triage
**Disposition:** fix
**Location:** `crates/varde-code/src/resolve.rs:356`

### Summary

Relative imports are checked only as literal paths. The resolver neither
normalizes `..` nor infers extensions before globally matching stems. Two
same-language `x.ts` files can make `./x` incorrectly resolve as unknown.

### Solutions

1. **Resolve relative candidates before global fallback**
   Normalize components and test directory-plus-stem candidates.
2. **Add duplicate-stem fixtures**
   Cover `./x` and `../x` imports with duplicate target filenames.
