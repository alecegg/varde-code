---
status: completed
title: "Cover web-language callable declarations and closures"
type: plan
shape: single
depends_on:
  - attribution-core
---

## Problem

JavaScript and TypeScript omit generator functions and closures from Function entities. Inner calls leak into outer findings.

## Solution

Model named generator declarations. Cover language scopes required by shared anonymous boundaries.

## Non-goals

- Create findings for anonymous callbacks.

## Constraints

- TSX must match TypeScript behavior.

## Design

### Tech choices

- Emit Functions for named generator declarations.
- Define web callable scopes for shared boundaries.

### Schema / data model

- Generator declarations carry body-spanning Function entities.

### API / interface contracts

- Outer web functions ignore anonymous closure calls.

## Decisions so far

- Anonymous callbacks remain non-reportable.

## Open Questions

- n/a — closure policy is confirmed.

## Assumptions

- n/a — regression fixtures cover every parser variant.

## Acceptance criteria

- [x] Given JavaScript or TypeScript generators
      When their bodies cross foreign slices
      Then findings point at the generator
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
- [x] Given nested web closures
      When they call foreign slices
      Then outer functions receive no leaked finding
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
