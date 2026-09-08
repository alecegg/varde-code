---
status: completed
title: "Cover Java, Kotlin, and Swift callable bodies"
type: plan
shape: single
depends_on:
  - attribution-core
---

## Problem

Several named accessor forms emit no Function entity. Java compact record constructors also lack coverage.

## Solution

Emit body-spanning Functions for each reportable accessor and compact constructor.

## Non-goals

- Change property or subscript variable extraction.

## Constraints

- Preserve existing names for ordinary functions.

## Design

### Tech choices

- Kotlin: property getters and setters.
- Swift: computed-property and subscript accessors.
- Java: compact record constructors.

### Schema / data model

- Each reportable accessor owns a Function entity.

### API / interface contracts

- Findings identify the accessor or constructor declaration.

## Decisions so far

- Accessor bodies are reportable named callables.

## Open Questions

- n/a — audited syntax defines the scope.

## Assumptions

- n/a — fixtures validate names and spans.

## Acceptance criteria

- [x] Given Kotlin property accessors
      When their bodies cross foreign slices
      Then findings identify the accessor
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
- [x] Given Swift accessors and subscripts
      When their bodies cross foreign slices
      Then findings identify the accessor
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
- [x] Given Java compact record constructors
      When their bodies cross foreign slices
      Then findings identify the constructor
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
