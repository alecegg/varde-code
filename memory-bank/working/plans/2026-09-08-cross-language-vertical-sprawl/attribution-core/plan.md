---
status: completed
title: "Establish declaration-safe vertical-sprawl attribution"
type: plan
shape: single
depends_on: []
---

## Problem

The rule merged same-named C# declarations. It also lacks a boundary for anonymous callable bodies.

## Solution

Use callable entity spans as function identity. Exclude nested callable spans from outer findings. Import the verified C# changes.

## Non-goals

- Resolve previously unresolved call edges.

## Constraints

- Preserve exact rule thresholds.
- Keep C# accessor findings named and stable.

## Design

### Tech choices

- Import C# commits `a93866e` and `5c69763`.
- Represent anonymous callable boundaries without standalone findings.
- Apply boundaries through shared extractor scope handling.

### Schema / data model

- Function entities own reportable declarations.
- Callable-boundary entities exclude inner calls from outer spans.

### API / interface contracts

- One finding maps to one declaration span.

## Decisions so far

- C# source commits are the verified starting point.

## Open Questions

- n/a — user confirmed full repair scope.

## Assumptions

- n/a — focused regressions define the required behavior.

## Acceptance criteria

- [x] Given same-named C# declarations
      When each crosses different foreign slices
      Then findings remain declaration-specific
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
- [x] Given an anonymous nested callable
      When it calls foreign slices
      Then its outer declaration receives no leaked finding
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
- [x] Given C# accessors
      When extraction runs
      Then their Function spans include accessor bodies
      (assert: cargo test -p varde-code --lib cs → passes)
