---
status: draft
title: "Fix vertical slice sprawl across all languages"
type: plan
shape: group
---

## Problem

The rule now attributes calls by declaration span. Several extractors omit callable entities or assign header-only spans. This hides real sprawl or assigns nested calls to an outer function.

## Solution

Every supported callable construct will either have a containing Function entity or be intentionally excluded from outer attribution. The rule will report the smallest named source declaration with useful location data.

## Non-goals

- Improve call resolution beyond existing extractor capabilities.
- Change rule thresholds or foreign-slice semantics.
- Report anonymous closures as standalone findings without a stable label.

## Constraints

- Preserve C# declaration identity fixes already verified in its worktree.
- Keep findings deterministic across supported language parsers.
- Cover each repaired syntax with extractor and rule regressions.
- Public surface: builtin rule output and source spans.
- Affected languages: C#, Dart, JavaScript, TypeScript, TSX, Kotlin, Swift, Java, Go, Rust, C++, Python, and Haskell.

## Design

### Tech choices

- Function entity coverage — model named callables directly.
- Anonymous closure handling — exclude closure-contained calls from outer functions.
- Declaration spans — include each callable body.
- Rule attribution — use function identity and containment only.
- C# integration — import commits `a93866e` and `5c69763` first.

### Schema / data model

- EntityKind::Function: one entity per reportable callable declaration.
- Anonymous callable span: nested exclusion boundary without a reportable Function entity.
- Function span: includes the whole executable body.

### API / interface contracts

- vertical_slice_sprawl finding: source span identifies one callable declaration.
- Calls inside nested named functions: attributed to the nested function.
- Calls inside anonymous closures: excluded from the outer finding.

## Decisions so far

- Cross-language scope → repair every audited language family.
- Finding identity → declaration span and entity identity.
- Anonymous closures → exclude outer attribution without synthetic findings.
- C# integration → import the two verified worktree commits first.
- Affected language set → repair every confirmed audited extractor gap.
- UI surface → none.

## Open Questions

- n/a — source inspection resolved the integration and scope boundaries.

## Assumptions

- n/a — grammar support will be verified by focused fixtures.

## Acceptance criteria

- n/a — child plans carry executable acceptance criteria.
