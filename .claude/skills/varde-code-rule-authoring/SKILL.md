---
name: varde-code-rule-authoring
description: >
  TRIGGER: Walk through creating a custom varde-code scan rule (kind=pattern
  or kind=sql) with the user — collaboratively pick the rule type, table/
  column and pattern syntax, fill in every TOML field, add self-tests, and
  validate. Use when the user wants a new lint/scan rule, wants to customize
  a seeded built-in rule, or asks "how do I write a rule for X".
  SKIP: Skip when the request is about running an existing rule (`scan`),
  seeding built-ins (`rules seed`), or changing application code instead of
  authoring a rule.
  Example phrases: "add a custom rule that flags X", "write a rule for
  circular deps", "help me build a scan rule".
allowed-tools: Bash Read Write Edit
compatibility: >
  kind=sql rules query the persisted index — `varde-code build` must have
  run at least once for `scan`/manual query testing to work. `varde-code
  test` (self-tests) needs no prior build.
---

## Workflow

This skill is instructional: it walks through the format collaboratively with the user rather than silently generating a file. Ask, don't assume — thresholds, severity, and pattern shape are all judgment calls the user should confirm.

1. **Understand the check.** Ask the user what they want flagged, with a concrete example of code that SHOULD and should NOT match. Vague requests ("catch bad error handling") need a concrete before/after example before you can pick a rule type.

2. **Choose the rule kind.** See `references/RULE-FORMAT.md` "Choosing pattern vs sql" for the decision criteria. Summary:
   - **`pattern`** — the check is a syntactic shape in one file/AST node (a specific call, a specific declaration form, a specific literal assignment). Runs per-file at scan time via the ast-grep-style matcher.
   - **`sql`** — the check is about aggregates, thresholds, or relationships across the persisted index (complexity, churn, fan-in/out, import graphs, cross-file joins). Runs as a read-only query against the built DB.
   - If the request needs both a structural shape AND a cross-file relationship, prefer `sql` joining `entities`/`resolved_edges` — the SQL engine has the full graph; the pattern engine only sees one file at a time.

3. **For `pattern` rules** — draft the `pattern` string and `languages` list. Read `references/RULE-FORMAT.md` "Pattern syntax" for `$VAR`/`$$$VAR` capture syntax, `constraints` (per-capture regex), and optional `rewrite` template. Look at `crates/varde-code/src/rules/builtin/hardcoded_credential_literal.toml` for a worked multi-pattern, multi-constraint example.

4. **For `sql` rules** — identify which table(s)/columns the check needs. Read `references/RULE-FORMAT.md` "SQL surface" for the full schema (tables, columns, `entities.kind`/`resolved_edges.kind` integer codes) and query conventions (`file` + `line` columns required in the SELECT, `:threshold_name` binds to `thresholds`/`strings`). Look at `crates/varde-code/src/rules/builtin/complexity.toml` (aggregation) and `circular_import.toml` (self-join) for worked examples.

5. **Fill in every required and relevant optional field.** Full field reference (types, required/optional, semantics) in `references/RULE-FORMAT.md` "Rule fields". Required: `id`, `kind`, `severity`, `message`. Strongly recommended: `name`, `description`, `remediation`. Confirm `severity` (`error`/`warning`/`info`) and any `thresholds`/`strings` defaults with the user — these are the knobs a repo/user override would typically tune.

6. **Write `[[test]]` self-tests.** Every rule should ship at least one positive and one negative case. Format differs by kind — see `references/RULE-FORMAT.md` "Test entries". Pattern rules use `valid`/`invalid` snippet lists (plus `expect_rewrite` if `rewrite` is set); SQL rules use an inline `[test.fixture]` file tree plus `expect_rows`.

7. **Pick the target file and scope.**
   - New standalone custom rule → one file, either `<repo_root>/.varde-code/rules/<id>.toml` (repo-scoped, this project only) or `~/.config/varde-code/rules/<id>.toml` (user-scoped, applies everywhere). Ask which the user wants if unclear — default to repo-scoped for anything tied to this codebase's conventions.
   - Customizing a seeded built-in → the user should run `varde-code rules seed --json '{"repoRoot":"<repo>"}'` first (writes editable copies of every built-in pack into `.varde-code/rules/`), then edit the seeded file with the same `id` in place. A same-id file in repo/user scope silently overrides the embedded built-in — no separate "override" mechanism needed.

8. **Validate.**
   ```bash
   varde-code rules test --json '{"rulesDir":"<dir-containing-the-toml>"}'
   ```
   Exits non-zero on any failing `[[test]]` case; fix and re-run until clean. Then confirm the rule actually loads and shows correct provenance:
   ```bash
   varde-code rules list --json '{"repoRoot":"<repo_root>"}'
   ```
   Check the new/edited rule's `"source"` field (`custom`, `override`, or `builtin`) matches expectations.

9. **Dry-run against real code (optional but recommended for `sql` rules).** Run `varde-code build --json '{"repoRoot":"<repo>"}'` then `varde-code scan --json '{"repoRoot":"<repo>"}'` and check the new rule's findings look right — no false positives on the repo's own code, catches the cases the user described in step 1.

## Gotchas

- `pattern` rules are language-agnostic by default (match every file whose parse succeeds) unless `languages` is set — set it explicitly for anything language-specific, or a JS-shaped pattern will silently no-op (never crash) on Python files.
- Regex `constraints` use Rust `regex` — no lookaheads/lookbehinds. Anchor with `^...$` or matches become substring searches, not whole-capture matches (see the credential-literal built-in's comment block for why this matters for false positives).
- SQL rule queries must alias a `file` column (source path) and a `line` column (1-indexed) in the SELECT — these are what `Finding` rendering expects. `1 AS line` is fine when the check has no natural line (e.g. whole-file aggregates).
- `thresholds`/`strings` keys bind as named SQL parameters `:key` — a threshold declared but never referenced in `query` is dead; a `:name` referenced in `query` but missing from `thresholds`/`strings` fails at scan time, not load time.
- `fix`/`rewrite` are different: `fix` is free-text remediation guidance (always informational). `rewrite` is a live meta-variable template applied when `scan --apply` runs (pattern rules only) — only set it if the fix is safe to auto-apply, and validate every `$VAR` in it appears in `pattern` (the loader rejects `rewrite` referencing an unknown capture).
- One rule pack file can hold multiple `[[rule]]` entries — group closely related checks (e.g. multiple pattern shapes for the same concept, like the credential-literal assignment vs. declaration split) in one file rather than one-rule-per-file, matching the built-in packs' convention.
