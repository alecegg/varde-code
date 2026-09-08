<!-- docs:v1 {"specs":{"cli":"crates/varde-code/src/cli.rs"}} -->

# CLI

The `varde-code` binary is a library plus a single CLI: one subcommand per query
mode, plus `extract`, `build`, `scan`, `test`, the `rules_*` / `skills_*` /
`hooks` management commands, `slice_state`, and `watch`.
Global flag: `-v`/`--verbose` (only applies when `RUST_LOG` is unset).

## Conventions

Every machine-readable command prints one JSON envelope to stdout. Query modes
take a single `--json '<object>'` argument; commands with flat flags still use
the same output envelope:

```json
{"ok": true, "data": {"...": "payload"}, "meta": {"compact": true, "truncated": false}}
{"ok": false, "data": {"error": {"code": "not_found", "message": "not found: ..."}}, "meta": {"compact": false, "truncated": false}}
```

`ok` is the operation result. Payloads always use `data`.
Failures use `data.error.code` and `data.error.message`.
`meta.compact` reports the active compacting policy.
`meta.truncated` reports declared omissions. Query errors remain
process-successful, so callers inspect `ok`. CI gates can return nonzero after
printing their envelope. See
[`machine-output-contract.md`](../memory-bank/knowledge/reference/machine-output-contract.md)
for the full contract.

The JSON object always accepts `repoRoot` or `dbPath` to locate the index,
plus mode-specific fields (documented per command below).

Two output-shaping fields are accepted by every query mode and by `scan`:

- **`absolutePaths?`** (default `false`) — file paths in the output are emitted
  repo-relative (the absolute `repoRoot` prefix is stripped) so the prefix
  isn't re-stated on every path. Set `true` to keep absolute paths. Relative
  paths round-trip: a relative `filePath` fed back into another mode still
  resolves (paths are matched by suffix). No effect on `dbPath`-only calls
  (no `repoRoot` to strip) or on paths outside the repo.
- **`includeSpanDetail?`** (default `false`) — a `span` carries only
  `start_line`/`end_line` by default; set `true` to also include the
  `start_byte`/`end_byte`/`start_col`/`end_col` fields.

`nav_map` JSON uses the same envelope and compacting rules.
`nav_map --format text` is the deliberate text renderer. Tools should use JSON.

## Build / index

- **`extract PATH`** — Extract entities and symbols from a file or directory
  tree; prints JSON to stdout. Does not touch the database.
- **`build --repo-root ROOT [--force] [--changed-files]`** — Extract, resolve,
  and persist a repo's full index. Full rebuild every run (`--force` skips
  incremental detection). Writes to
  `~/.config/varde-code/repos/<name>-<hash>/index.db`. Query subcommands read
  from here (or freshen it incrementally on demand). The JSON result reports
  `changedFilesCount` plus a small `changedFilesSample`; pass `--changed-files`
  to include the full `changedFiles` array instead (on a full build that is
  every reparsed path in the repo).

## Query modes

Each takes `--json '<object>'`; fields shown are in addition to
`repoRoot`/`dbPath`.

| Command | Extra JSON fields | Description |
|---|---|---|
| `batch` | `calls: [{mode, ...}]` | Run several query modes in one call, sharing `repoRoot`/`dbPath` |
| `symbols_in_file` | `filePath`, `includeBody?`, `includeReferences?` | List symbols declared in a file. Returns declarations + bindings by default; `reference`-kind symbols (call sites/usages, 80–95% of rows) are excluded unless `includeReferences: true` |
| `symbols_in_files` | `filePaths`, `includeBody?`, `includeReferences?` | Batch form of `symbols_in_file` over many files; maps each path to its symbols or `{error}` |
| `get_symbol` | `name`, `filePath?`, `kind?`, `includeBody?` | Get one symbol by name |
| `dependencies` | `filePath`, `direction?`, `maxDepth?` | Files a file depends on |
| `dependents` | `filePath`, `maxDepth?` | Files depending on a file |
| `tests_for_file` | `filePath` | Test files covering a file |
| `hotspots` | — | Risk hotspots ranked by complexity × churn (falls back to complexity alone when no file has churn) |
| `clusters` | `minSize?`, `maxClusters?`, `seedPath?` | Community-detection (Louvain) partition of the resolution graph into densely-interconnected file clusters; each `{id, files, label, cohesion}` (`label` always `null`, `cohesion` is the fraction of touching edges kept inside). `seedPath` returns only the cluster containing that file |
| `context_pack` | `query` | Keyword-driven context bundle: files/symbols whose paths, directory names, or symbol names match `query` (exact/substring), plus their one-hop dependency neighbors, ranked by complexity+churn, with covering tests and a `readingOrder`. Structural only — no doc corpus, no semantic search |
| `nav_map` | `maxTokensEstimate?` (plus `--format json\|text`) | Session-start repo orientation map: entrypoints, foundational files, module layers, subsystems, symbols, flows, and hotspots assembled from the persisted index. Trimmed to a total token budget (default 8000, override with `maxTokensEstimate`) spent section-by-section in priority order so it stays fixed-cost regardless of repo size; a `guide.truncated` block reports `{shown, total, more}` per trimmed section and names the follow-up that returns the full data. The `symbols` leaderboard ranks by **caller breadth** (distinct calling files, reported as `callers`) rather than raw call count, and drops low-orientation accessor/stdlib names (`getName`, `push`, `ConfigureAwait`, …). The `flows` section lists only genuine multi-node call trees — single-node trees that merely restate an entrypoint are omitted. Orientation sections (`foundational_files`, `symbols`, `entrypoints`) exclude front-end asset code (JS/TS/CSS under `assets/`), and `foundational_files` ranks pure data classes (all-accessor/boilerplate methods) below real modules. `entrypoints` covers both annotation-based handlers and call-based routes (Express, Slim, Phoenix, Laravel, Ktor, net/http, …). JSON is canonical; `--format text` renders the same data as plain text |
| `map_file` | `filePath` | Map a file to its persisted node info |
| `map_symbol` | `name`, `sourceFile?` | Map a symbol to its persisted entity |
| `map_path` | `sourceFile`, `targetFile`, `maxDepth?` | Dependency path between two files |
| `explore` | `query: {params: {input, direction?, maxItems?}}` | Explore the dependency graph from a seed; `input` resolves as a file path, falling back to a symbol-name match (exact, then substring) if no file matches; `direction` is `outgoing` (default), `incoming`, or `both` |
| `blast_radius` | `filePath` | All files transitively reachable from a file |
| `symbol_blast_radius` | `name`, `kind?` | All files reachable from a symbol |
| `detect_changes` | `diffMode`, `range?` | Symbols changed between git states |
| `find_imports` | `filePath` | Resolved import edges of a file |
| `type_hierarchy` | `name?`, `filePath?` | Type hierarchy (extends/implements) |
| `filter_symbols` | `kind?`, `tags?`, `language?`, `file?`, ... | Filter symbols by kind/tags/language/etc. |
| `find_pattern` | `pattern`, `filePath?` (`file?` alias), `path?`, `language?`, `inside?`, `has?`, `precedes?`, `follows?` | Find AST nodes matching a `$VAR`/`$$$VAR` pattern, optionally `$VAR:kind`-constrained and filtered by `{kind}` ancestor/descendant/sibling relations (live parse, no DB). Works on **every** `ast-grep`-linked grammar (not just the twenty-one indexed languages); `language` accepts `ast-grep` aliases (`c++`, `py`, `rb`, …) and is inferred from the extension for a single `filePath`. A directory `path` requires an explicit `language`. |

## Scan / rules

- **`scan --json '{repoRoot, output?, severityThreshold?}' [--apply] [--force]`**
  — Run the repo's rule packs (built-in + user + repo scope) against the
  persisted index and emit findings. Requires a fresh prior `build`. Exits
  non-zero when findings exist at/above the severity threshold, so it can gate
  CI. `--apply` writes `rewrite` templates to matched files (default is
  read-only); `--force` allows writing to files with uncommitted git changes
  (otherwise skipped as `skipped-dirty`). Output is `{findings, diagnostics,
  rules}`: each finding is `{id, rule_id, severity, location, evidence,
  message?, certainty?, agent_instructions?}`, and the `rules` legend maps each
  fired `rule_id` to its `{message, remediation}` once rather than repeating that
  static text on every finding (a finding carries an inline `message` only when
  rule interpolation changed it from the template). `duplicate-code-clone`
  findings are collapsed one-per-clone-band: a single finding per band with
  `evidence: {band, members: [{file, startLine, endLine}, …]}` instead of one
  finding per member.
- **`test --json '{rulesDir?}'`** — Run every rule's `[[test]]` entries through
  the pattern/SQL test runners and report pass/fail. Self-contained: no DB and
  no prior `build` required. `rulesDir` scopes discovery to a single directory
  of rule-pack TOML files (no user/repo merge, no built-ins); absent it,
  discovery mirrors `scan`'s rule loading from the current directory. Exits
  non-zero when any test fails, so it can gate CI.

## Rules management

Materialize or undo the built-in rule packs as editable TOML so they can be
customized directly (instead of only overridden by id). All three take
`--json '{repoRoot}'` and never require a DB.

- **`rules_list --json '{repoRoot}'`** — List the rules that would run for a
  repo (built-in + user + repo scope), with override provenance. Never scans.
- **`rules_seed --json '{repoRoot}' [--user] [--force]`** — Copy the built-in
  rule packs into the repo's `.varde-code/rules/` (default) or, with `--user`,
  `~/.config/varde-code/rules/`. Existing files are left untouched unless
  `--force`.
- **`rules_remove --json '{repoRoot}' [--user] [--force]`** — Undo a
  `rules_seed`: delete previously seeded built-in files from the repo (default)
  or user (`--user`) rules dir. Only files matching a shipped built-in are
  removed; custom rule files are left alone. A seeded file edited since seeding
  is skipped unless `--force` (which discards those local edits).

## Skills

Install or remove the agent skills bundled in the binary (rule authoring,
rule-scan triage, codebase navigation). These use flat flags, not `--json`, and
never require a DB. Install targets match each harness's global discovery path:
Claude (`~/.claude/skills`), Codex (`~/.agents/skills`), OpenCode
(`~/.config/opencode/skills`), and Pi (`~/.pi/agent/skills`).

- **`skills_list`** — List bundled packs, files, and harness destinations.
  No filesystem writes.
- **`skills_install [--agent A...] [--dir DIR] [--force]`** — Install every
  pack for Claude, Codex, OpenCode, and Pi by default. Limit targets with
  repeatable or comma-separated `--agent` values. `--dir` overrides every
  selected target, for a project-local shared installation or testing.
- **`skills_remove [--agent A...] [--dir DIR] [--force]`** — Undo
  `skills_install` for the selected targets. Only shipped skill-pack files are
  touched. Locally edited files are skipped unless `--force`.

## Hooks

Install or remove session-start hooks for supported agent harnesses (`claude`,
`codex`, `opencode`, `pi`) that shell out to `nav_map` at session start. Same
idea as `skills`, but installs a hook instead of skill files. Flat flags, no
`--json`.

- **`hooks list`** — List the 4 supported agent hook targets and the
  directory/file each installs to. No filesystem writes.
- **`hooks install [--agent A...] [--force] [--dir DIR]`** — Install
  session-start hooks for the given agents (default: all 4). `--agent` is
  repeatable (`--agent claude --agent codex`) or comma-separated
  (`--agent claude,codex`). Existing entries are left untouched unless
  `--force`. `--dir` overrides the install target (primarily for testing);
  without it each agent resolves its real per-OS default (`~/.claude/`,
  `~/.codex/`, `~/.config/opencode/`, `~/.pi/agent/extensions/`).
- **`hooks remove [--agent A...] [--force] [--dir DIR]`** — Undo
  `hooks install`. Whole-file targets (opencode, pi) are deleted; merge targets
  (claude, codex) have only this tool's injected entry removed, never the rest
  of the shared config. A target edited since install is skipped unless
  `--force`.

## Diagnostics

- **`slice_state --json '{repoRoot}'`** — Dump the slice freshness ledger for a
  repo: what each derived slice was last built through and whether it's stale.
  Read-only, never builds or freshens.

## Background watcher

- **`watch [--repo REPO...] [--config PATH] [--debounce-ms N]`** — Long-lived
  foreground process that proactively keeps one or more repos' indexes warm by
  reacting to filesystem events (debounced, default 750ms) and running the
  same incremental `ensure_fresh` path queries use on demand. Purely a latency
  optimization: every query still self-verifies freshness independently, so a
  dead or lagging watcher never produces a stale answer, only a slower one.
  Backgrounding the process itself (launchd/systemd/nohup) is the caller's
  job. `--config` points at a `watch.toml` (defaults to
  `~/.config/varde-code/watch.toml` if present and no `--repo` given) that can
  list `repos`/`parent_dirs`; explicit `--repo` flags combine with it.
- **`watch --list`** — List every running (or stale-locked) watcher instance
  as JSON and exit.
- **`watch --stop [--repo REPO...] [--config PATH]`** — Stop the watcher for
  the repo set given via `--repo`/`--config` (SIGTERM if live; always clears
  its lock) and exit.
- **`watch --stop-all`** — Stop every running watcher instance and exit.
</content>
