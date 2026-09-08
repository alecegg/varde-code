---
id: 2026-09-08-tool-output-hardening
title: Harden machine-readable tool output
status: completed
---

# Harden machine-readable tool output

## Problem

Tool payloads vary in density and detail. Large results can repeat data.
Callers need stable, concise, actionable JSON across every tool.

## Solution

Harden every machine-readable output boundary. Standardize the envelope as
`{ ok, data, meta }`, make failures structured, relativize known repository
paths, and compact dense result shapes without hiding dropped data. The
explicit `nav_map --format text` view remains human-readable.

## Acceptance criteria

- [x] Every machine-readable command emits `{ ok, data, meta }`.
- [x] Failed machine-readable commands emit structured error data.
- [x] Large payloads avoid needless repeated fields.
- [x] Any truncation is explicit and preserves navigation details.
- [x] Contract tests cover success, failure, and compact output behavior.

## Assumptions

- The CLI is pre-alpha. Output schema changes are acceptable.
- `nav_map --format text` remains a deliberate human-readable exception.
