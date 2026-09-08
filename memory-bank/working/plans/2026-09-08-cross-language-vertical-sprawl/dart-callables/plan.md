---
status: completed
title: "Cover Dart callable declaration bodies"
type: plan
shape: single
depends_on:
  - attribution-core
---

## Problem

Dart Function entities cover signatures only. Their body calls cannot trigger the rule.

## Solution

Emit body-spanning Function entities for functions, constructors, factories, getters, and setters.

## Non-goals

- Extend Dart call resolution.

## Constraints

- Keep callable names stable and useful.

## Design

### Tech choices

- Use enclosing declaration spans that include executable bodies.

### Schema / data model

- Each Dart named callable emits one body-spanning Function entity.

### API / interface contracts

- Dart findings point at the owning callable declaration.

## Decisions so far

- Header-only signature spans are invalid rule anchors.

## Open Questions

- n/a — audited syntax defines the scope.

## Assumptions

- n/a — fixtures verify grammar node selection.

## Acceptance criteria

- [x] Given Dart functions and constructors
      When their bodies call foreign slices
      Then findings point at each owning declaration
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
- [x] Given Dart getters, setters, and factories
      When extraction runs
      Then Function spans contain their body calls
      (assert: cargo test -p varde-code --lib dart → passes)
