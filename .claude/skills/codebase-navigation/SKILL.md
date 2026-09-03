---
name: codebase-navigation
description: >
  TRIGGER: Use the `varde-code` CLI itself (this repo's own tool) to navigate,
  search, and orient in a codebase — instead of raw `grep`/`find`/reading
  files blind. Covers session-start orientation (`report nav_map`), keyword-driven
  context gathering (`report context_pack`), symbol/dependency lookups, structural
  pattern search (`query find_pattern`), and blast-radius/impact analysis before a
  change. Use when asked to explore, orient in, or find something in a repo
  that has (or can have) a `varde-code` index, or when the user asks "how do
  I use varde-code to find X".
  SKIP: Skip for simple one-off text lookups where `grep`/`Read` is already
  fast enough (e.g. a literal string that's obviously in one file), for
  authoring scan rules (see `rule-authoring`), or for benchmarking
  (`benchmark`).
  Example phrases: "use varde-code to find where X is defined", "what
  depends on this file", "explore this codebase with the CLI", "what would
  break if I change this".
allowed-tools: Bash
compatibility: >
  All `query` subcommands accept `repoRoot` (extracts fresh in memory,
  slower, no prior build needed) or `dbPath`/an already-built repo (fast,
  reads the persisted index). `graph explore`, `query symbols_in_file`, `graph dependencies`,
  `graph dependents`, `report hotspots`, `graph blast_radius`, etc. need a prior `varde-code
  build` to answer from the persisted index — run `build` once per repo
  first unless just doing a single `report context_pack`/`query find_pattern` call with
  `repoRoot`. Every JSON arg is one flag: `--json '{...}'`, not shell-quoted
  key=value pairs.
---

## Core idea

`varde-code` is a query engine over a repo's parsed AST/symbol/dependency
graph. Prefer it over blind `grep`/`find`/reading files when the question is
structural — "what calls this", "what would this change affect", "where is
this symbol declared" — because it answers from real parse/resolve data, not
text matching.

Every subcommand takes `--json '<object>'`. Discover the exact input shape
per command with `varde-code <command> --help` if this skill's summary is
insufficient — don't guess at field names.

## Setup: build once per repo

Most query commands (`graph explore`, `query symbols_in_file`, `graph dependencies`,
`graph dependents`, `report hotspots`, `graph blast_radius`, `graph type_hierarchy`, `query filter_symbols`,
`query map_symbol`, `query map_file`, `query tests_for_file`, `report clusters`, `report detect_changes`)
read from a persisted SQLite index and need it built first:

```bash
varde-code build --repo-root <repo>
```

Writes to `~/.config/varde-code/repos/<name>-<hash>/index.db`. Full rebuild
every run — safe to re-run after changes, but re-run when the repo has
changed and you need fresh answers (or use `varde-code watch` to keep an
index warm in the background for a repo you're working in over a session).

Two commands work standalone without a prior build — they extract in memory
on demand (slower per-call, no setup): `report context_pack` and `query find_pattern`
(when scoped to a `file`). Reach for these for a single ad hoc lookup; reach
for `build` + the persisted-index commands when doing several lookups in the
same repo.

## Orientation workflow (start here in an unfamiliar repo)

1. **`report nav_map`** — session-start orientation: entrypoints, foundational
   files, module layers, subsystems, hotspots, derived from the persisted
   index.
   ```bash
   varde-code report nav_map --json '{"repoRoot":"<repo>"}' --format text
   ```
   Use `--format text` for a human-readable render; JSON is canonical if you
   need to process it further. This is the same map a session-start hook may
   already have surfaced — check before re-running.

2. **`report context_pack`** — keyword-driven bundle: files/symbols matching a
   query term (substring/exact against paths, dir names, symbol names) plus
   their one-hop graph neighborhood and covering tests, ranked with a
   suggested reading order.
   ```bash
   varde-code report context_pack --json '{"repoRoot":"<repo>","query":"<keyword>"}'
   ```
   Works without a prior `build` (extracts on the fly). This is the "I want
   to start reading about X" command — start here for a feature/concept
   name, not a raw `grep`.

3. **`report hotspots`** / **`report clusters`** — `report hotspots` ranks files by
   complexity + churn (where risk concentrates); `report clusters` partitions the
   dependency graph into densely-interconnected file groups (rough module
   boundaries when the repo has none documented).

## Symbol and dependency lookups

- **`query symbols_in_file`** — what's declared in a file: `{ repoRoot|dbPath,
  filePath, includeBody? }`.
- **`query get_symbol`** / **`query map_symbol`** — find one symbol by name (+ optional
  `filePath`/`kind` to disambiguate): `{ repoRoot|dbPath, name, filePath?,
  kind?, includeBody? }`.
- **`query filter_symbols`** — bulk filter by `kind`/`tags`/`language`/`file`, for
  "list all X" queries (e.g. all `struct`s, all `test` symbols).
- **`graph dependencies`** — what a file imports/depends on; **`graph dependents`** —
  what depends on a file (the reverse edge). Both take `filePath` +
  optional `direction`/`maxDepth`.
- **`graph explore`** — graph walk from a seed file *or* symbol name (resolves
  file path first, falls back to symbol match). `direction` is `outgoing`
  (default) / `incoming` / `both`.
  ```bash
  varde-code graph explore --json '{"repoRoot":"<repo>","query":{"params":{"input":"<file-or-symbol>","direction":"incoming","maxItems":20}}}'
  ```
- **`graph blast_radius`** / **`graph symbol_blast_radius`** — everything transitively
  reachable from a file/symbol. Run this **before** changing a
  widely-imported file to see the full impact surface, not just direct
  dependents.
- **`query tests_for_file`** — which test files cover a given file, so you know
  what to run/update after an edit.
- **`graph type_hierarchy`** — supertype/subtype chain for a symbol.
- **`graph map_path`** — dependency path between two specific files (how does A
  reach B).
- **`report detect_changes`** — symbols changed between two git states (diff-aware,
  not text diff — reports which *symbols* changed).

## Structural pattern search

**`query find_pattern`** — AST pattern match using `$VAR`/`$$$VAR` meta-variables,
not regex/text. Use this instead of `grep` when the target is a *shape*
(a specific call form, a specific declaration), not a literal string:

```bash
varde-code query find_pattern --json '{"repoRoot":"<repo>","pattern":"$OBJ.unwrap()"}'
```

Scope to one `file` for a single-file structural check without needing a
prior `build`; omit `file` to search the whole indexed repo (needs
`dbPath`/prior `build` for repo-wide DB-backed search — check `--help` for
current scoping rules if unsure).

## Batch multiple queries in one call

**`query batch`** — run several query modes in a single invocation instead of N
separate CLI calls when you already know you need several lookups (e.g.
`query symbols_in_file` for 5 files, or a mix of modes):

```bash
varde-code query batch --json '{"repoRoot":"<repo>","calls":[{"mode":"symbols_in_file","filePath":"a.rs"},{"mode":"dependents","filePath":"a.rs"}]}'
```

Prefer this over sequential single-mode calls when the queries are known
upfront — one process/DB-open instead of several.

## Gotchas

- JSON goes in a single `--json '<object>'` flag — always valid JSON, no
  shell-side key=value parsing.
- `repoRoot` triggers a fresh in-memory extract (correctness over speed, no
  build needed); `dbPath` (or an already-built `repoRoot`) reads the
  persisted index (fast, but only as fresh as the last `build`). If results
  look stale after edits, re-run `build`.
- `report nav_map`/`report hotspots`/`report clusters`/etc. all read the *persisted* index — they
  will not reflect uncommitted changes until you `build` again (or run
  `watch` to keep it warm automatically).
- `graph explore`'s `input` resolves as a file path first, only falling back to a
  symbol-name match (exact, then substring) if no file matches — an
  ambiguous bare name can silently match the wrong thing if a same-named
  file exists. Prefer a full/relative file path when you have one.
- `scan`/`rules test`/`rules list`/`rules seed` are for lint rules, not
  navigation — see the `rule-authoring` skill instead.
- Every command supports `--help` for its exact current JSON shape; this
  skill summarizes common fields but the CLI `--help` output is the source
  of truth if they diverge.
