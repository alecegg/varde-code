---
id: 2026-09-07-nav-map-followups
title: Nav-map framework and release follow-ups
status: ready
shape: group
---

# Nav-map framework and release follow-ups

## Problem

The nav-map audit exposed inaccurate NestJS entrypoints and unbalanced
mixed-language orientation. Reference coverage needs regression fixtures.
The installed binary must match current source behavior.

## Solution

Correct Nest role classification, balance selected entrypoints across detected
language surfaces, add reference-repository regression coverage, and refresh
the installed CLI through the chosen release path.

## Design

- Nest: classify `@Injectable()` services without treating them as middleware.
- Selection: sort candidates by current ranking, then round-robin by source extension.
- Regression: test reference-like framework fixtures at the default budget.
- Release: update the local installation or publish a versioned release.

## Acceptance criteria

- [ ] Nest services are excluded from nav-map entrypoints.
- [ ] Mixed-language maps retain one entrypoint per detected source language when the cap permits.
- [ ] Framework fixtures cover NestJS, Spring, and ASP.NET route detection at the default budget.
- [ ] The installed CLI is rebuilt locally from the merged source.

## Decisions so far

- Use section-first truncation before framework selection.
- Use source-language round-robin for entrypoint diversity.
- Use `cargo install --path crates/varde-code` for the local release path.

## Open Questions

None.

## Assumptions

- Reference fixtures can remain compact and in-repository — affects: test design — confidence: high.
- Source extension is a stable language discriminator — affects: selection — confidence: high.
