---
title: CORRECTNESS findings
type: review-category
description: CORRECTNESS review findings
resource: memory-bank/working/reviews/2026-09-03-perf-find-pattern-literal-atom-prefilter-full-repo
tags:
  - review
  - correctness
timestamp: 2026-09-03T00:00:00+02:00
created_at: 2026-09-03T00:00:00+02:00
edited_at: 2026-09-03T12:00:00+02:00
---

# CORRECTNESS

## [CORRECTNESS-001] Concurrent full builds share one temporary database

**Severity:** high
**Label:** triage
**Disposition:** fix
**Location:** `crates/varde-code/src/build.rs:96`

### Summary

Forced and initial builds bypass `repo_lock` while sharing `index.db.tmp`.
Concurrent full builds can remove, write, or rename each other's temporary
database. A build can fail or publish another writer's index.

### Solutions

1. **Serialize full builds**
   Hold the repository lock across the full-build temporary-file lifecycle.
2. **Add a concurrency regression test**
   Exercise concurrent forced and initial builds against one repository.

## [CORRECTNESS-002] String contents can disable rule findings

**Severity:** high
**Label:** triage
**Disposition:** fix
**Location:** `crates/varde-code/src/rules/suppress.rs:84`

### Summary

Suppression detection accepts either directive marker anywhere in a raw line.
A string literal containing `varde-ignore-file` therefore suppresses every
finding for that file. The documented contract requires inline comments.

### Solutions

1. **Recognize real comments only**
   Parse comments or validate language-specific comment prefixes first.
2. **Add string-literal coverage**
   Verify directive text inside a string cannot suppress findings.

## [CORRECTNESS-003] Symbol language filters accept unrelated languages

**Severity:** medium
**Label:** triage
**Disposition:** fix
**Location:** `crates/varde-code/src/query/simple.rs:571`

### Summary

`filter_symbols` checks only Rust, TypeScript, JavaScript, and Go. Every
other language value matches all symbols. Valid filters like `python` or
`php` therefore return unrelated languages despite the API's predicate claim.

### Solutions

1. **Map every supported language**
   Apply the filter to every `language_from_name` result.
2. **Add cross-language tests**
   Assert each accepted language returns only matching symbols.
