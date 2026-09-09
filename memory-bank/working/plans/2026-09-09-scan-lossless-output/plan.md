---
id: 2026-09-09-scan-lossless-output
title: Keep scan findings lossless
status: completed
---

# Keep scan findings lossless

## Problem

The `scan` command truncates findings after a fixed limit.
Users cannot receive every finding from a complete scan.

## Solution

Remove scan-specific result truncation.
Keep the existing output envelope and finding details.
Add a regression test exceeding the prior limit.

## Acceptance criteria

- [x] Given more than 100 scan findings, `scan` returns all findings.
- [x] The scan payload reports no finding truncation.
- [x] Existing scan output fields remain unchanged.

## Assumptions

- This change applies only to `scan` findings.
- Other command limits remain unchanged.
