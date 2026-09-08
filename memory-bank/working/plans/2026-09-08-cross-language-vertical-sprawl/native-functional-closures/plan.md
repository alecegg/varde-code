---
status: completed
title: "Contain anonymous closures across remaining languages"
type: plan
shape: single
depends_on:
  - attribution-core
---

## Problem

Anonymous callable bodies can leak calls into an enclosing Function finding across remaining language extractors.

## Solution

Mark every supported anonymous callable span as a non-reportable boundary.

## Non-goals

- Produce standalone findings for anonymous closures.

## Constraints

- Preserve named nested function attribution.

## Design

### Tech choices

- Audit every remaining language for anonymous callable syntax.
- Go: function literals.
- Rust: closure expressions.
- C++: lambda expressions.
- Python: lambda expressions.
- Haskell: local value bindings.
- Cover PHP, Ruby, Scala, Elixir, Java, Kotlin, and Swift when their grammar exposes anonymous callables.

### Schema / data model

- Anonymous callable spans exclude calls from containing Functions.
- Callable-boundary fixture table covers every supported closure syntax.

### API / interface contracts

- No outer finding includes inner anonymous callable calls.

## Decisions so far

- Closure findings need stable names, so anonymous closures remain excluded.

## Open Questions

- n/a — closure policy is confirmed.

## Assumptions

- n/a — language fixtures verify each boundary.

## Acceptance criteria

- [x] Given the callable-boundary fixture table
      When every fixture calls foreign slices
      Then outer functions receive no leaked finding
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
- [x] Given Haskell local value bindings
      When they call foreign slices
      Then enclosing Functions receive no leaked finding
      (assert: cargo test -p varde-code --lib vertical_slice_sprawl → passes)
