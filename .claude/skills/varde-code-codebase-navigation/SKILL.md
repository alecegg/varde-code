---
name: varde-code-codebase-navigation
description: Navigate and learn unfamiliar codebases with the varde-code CLI. Use when an agent needs repository orientation, feature context, symbol or dependency lookup, structural search, impact analysis, or test discovery. Prefer it for structural questions such as where code is defined, what calls it, what depends on it, and what a change can affect. Skip simple literal lookups in a known file and scan-rule authoring.
---

# Navigate a codebase

Use `varde-code` for parsed symbols, resolved dependencies, and AST shapes.
Use ordinary file reads after it identifies useful targets.

## Start a navigation session

Set the repository root once. Build the index before several queries.

```sh
varde-code build --repo-root <repo>
varde-code nav_map --json '{"repoRoot":"<repo>"}' --format text
```

`nav_map` returns entrypoints, foundational files, module layers, subsystems,
important symbols, flows, and hotspots. Start there in an unfamiliar repo.

Find a feature or concept next:

```sh
varde-code context_pack --json '{"repoRoot":"<repo>","query":"<feature-or-concept>"}'
```

Use `readingOrder`, matched symbols, graph neighbors, and tests to decide what
to read. Do not treat this as semantic documentation search.

## Follow the question

- Inspect declarations in a known file with `symbols_in_file`.
- Locate a declaration with `get_symbol`. Add `filePath` or `kind` if needed.
- Trace imports with `find_imports`.
- Trace direct relationships with `dependencies` or `dependents`.
- Explore a local graph with `explore`.
- Check transitive change impact with `blast_radius` or `symbol_blast_radius`.
- Find affected tests with `tests_for_file`.
- Inspect inheritance with `type_hierarchy`.
- Compare two files with `map_path`.
- Identify risky code with `hotspots`.

Examples:

```sh
varde-code symbols_in_file --json '{"repoRoot":"<repo>","filePath":"src/server.rs"}'
varde-code get_symbol --json '{"repoRoot":"<repo>","name":"handle_request","filePath":"src/server.rs"}'
varde-code dependents --json '{"repoRoot":"<repo>","filePath":"src/server.rs"}'
varde-code explore --json '{"repoRoot":"<repo>","query":{"params":{"input":"handle_request","direction":"both","maxItems":20}}}'
varde-code tests_for_file --json '{"repoRoot":"<repo>","filePath":"src/server.rs"}'
```

Use repo-relative paths from prior results. They round-trip into later calls.

## Search code structure

Use `find_pattern` for syntax shapes, not literal text. It live-parses files
and never needs an index.

```sh
varde-code find_pattern --json '{"filePath":"src/server.rs","pattern":"$RESULT.unwrap()"}'
varde-code find_pattern --json '{"path":"src","language":"rust","pattern":"$FUNC($$$ARGS)"}'
```

Use `$VAR` for one captured node. Use `$$$VAR` for many nodes. Give a directory
search an explicit `language`. Run `varde-code find_pattern --help` before
using relation filters or kind constraints.

## Keep queries efficient

Pass `repoRoot` to let indexed query modes freshen relevant data automatically.
Run `build` before a broad session to make that first query predictable. Use
`batch` when several lookups are already known:

```sh
varde-code batch --json '{"repoRoot":"<repo>","calls":[{"mode":"symbols_in_file","filePath":"src/server.rs"},{"mode":"dependents","filePath":"src/server.rs"}]}'
```

Read `{"ok":true,"data":...}` from stdout. Handle
`{"ok":false,"error":...}` before assuming a query found nothing. Pass
`includeBody: true` only when declaration bodies are necessary. Pass
`includeReferences: true` only when usages are required.

Use `varde-code <command> --help` for the current JSON shape. Do not invent
field names or reuse stale command syntax.
