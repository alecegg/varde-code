---
id: 2026-09-09-isp-auto-property-accessors
title: Exclude C# auto-property accessors from ISP stubs
status: completed
---

# Exclude C# auto-property accessors from ISP stubs

## Problem

Bodyless C# auto-property accessors are extracted as functions.
The ISP rule mistakes them for empty method stubs.

## Solution

Exclude bodyless auto-property accessors from function extraction.
Keep explicit property accessor bodies visible as functions.
Add an end-to-end C# scan regression.

## Acceptance criteria

- [x] A class with auto-properties does not trigger `solid-isp` from generated accessors.
- [x] Explicit C# accessor bodies remain function entities.
- [x] Existing C# scan behavior remains covered.
