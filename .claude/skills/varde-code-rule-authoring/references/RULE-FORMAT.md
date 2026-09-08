# varde-code rule pack format

Source of truth: `crates/varde-code/src/rules/mod.rs` (`Rule`/`TestCase` structs), `pattern.rs`, `sql.rs`. This doc summarizes it for rule authoring; if it and the code disagree, the code wins.

A rule pack is a TOML file with one or more `[[rule]]` array-of-tables entries. Each rule has a `kind = "pattern" | "sql"` discriminator; unrecognized extra fields are ignored (permissive), and a rule missing a required field is skipped with a diagnostic rather than failing the whole file.

## Choosing pattern vs sql

| | `pattern` | `sql` |
|---|---|---|
| Scope | one file / one AST match at a time | whole persisted index, joins across files |
| Good for | a specific call/declaration/literal shape | thresholds, aggregates, graph relationships (imports, complexity, churn, clones) |
| Needs a prior `build`? | no (matches source directly) | yes (queries the DB `build` populates) |
| Cross-file checks? | no | yes |

## Rule fields

Required (rule is skipped + diagnosed if any is missing):

- `id` (string) — stable identifier. Used for cross-scope override matching (a repo/user rule with the same `id` as a built-in silently replaces it) and for `rules list`'s dedup.
- `kind` (`"pattern"` | `"sql"`)
- `severity` (`"error"` | `"warning"` | `"info"`, case-insensitive) — `error` > `warning` > `info` for threshold comparisons (e.g. `scan`'s `severityThreshold` gate).
- `message` (string) — the finding's headline text.

Recommended (optional, no validation, but every built-in sets these):

- `name` (string) — short human title.
- `description` (string) — one/two sentences on what's being checked and why.
- `remediation` (string) — how to fix a finding.

Kind-specific payload (exactly one required, matching `kind`):

- `pattern` (string) — ast-grep-style pattern, required when `kind = "pattern"`.
- `query` (string) — SQL SELECT, required when `kind = "sql"`.

Kind-agnostic optional fields:

- `thresholds` (table of string → float) — SQL only. Each key becomes a `:key` named parameter bound into `query`.
- `strings` (table of string → string) — SQL only, same binding mechanism as `thresholds` but for string parameters.
- `constraints` (table of string → string) — pattern only. Per-capture regex: `capture name -> regex`. A capture present in the map but absent from the match, or that fails the regex, drops the match. Rust `regex` crate — no lookaheads/lookbehinds.
- `fix` (string) — free-text suggested-fix guidance. Purely informational, never applied automatically, valid on either kind.
- `rewrite` (string) — pattern only. A meta-variable template (`$VAR`/`$$$VAR`) applied to matched spans when `scan --apply` runs. Every `$VAR`/`$$$VAR` in `rewrite` must also appear in `pattern` — the loader validates this and rejects the rule (with a diagnostic) if not.
- `languages` (array of strings) — pattern only. `SupportLang`-resolvable names (e.g. `"javascript"`, `"typescript"`, `"tsx"`, `"python"`, `"rust"`, `"go"`...). Omitted/empty = run against every file whose parse succeeds (language-agnostic).
- `test` — see "Test entries" below. Inert to `scan`; only consumed by `varde-code rules test`.

## Pattern syntax

The pattern engine reuses `find_pattern`'s ast-grep-style matcher (same engine as the `find_pattern` scan-benchmark mode).

- `$VAR` captures a single AST node.
- `$$$VAR` captures a variadic list of nodes (e.g. call arguments, statement lists).
- Everything else in the pattern string is matched literally against AST structure (not text) — declaration keywords like `const`/`let` are stripped as trivia by the matcher, so `const $KEY = $VAL` also matches `let` declarators.
- Constraints apply per-capture, checked with `regex::is_match` (unanchored) — always anchor with `^...$` unless you deliberately want substring matching.

Worked example (`crates/varde-code/src/rules/builtin/hardcoded_credential_literal.toml`): two `[[rule]]` entries in one pack because one `pattern` string can't express both `$KEY = $VAL` (bare assignment) and `const $KEY = $VAL` (declaration) — same `constraints` reused across both.

## SQL surface

`kind = "sql"` queries run read-only against the persisted index built by `varde-code build`. The SELECT must alias a `file` column (source path) and a `line` column (1-indexed); use `1 AS line` for whole-file/no-natural-line checks.

### Tables

**`files`** — one row per indexed file.
| column | type | notes |
|---|---|---|
| `id` | INTEGER PK | |
| `path` | TEXT | repo-relative source path |
| `mtime`, `size` | INTEGER | |
| `content_hash` | TEXT | |
| `complexity` | INTEGER | `1 + count(ControlFlow entities)` in the file |
| `churn` | INTEGER | commit-touch count (see `git`-derived churn signal) |
| `fan_in`, `fan_out` | INTEGER | denormalized edge counts |
| `community_id` | INTEGER | clustering group, FK-ish to `communities.id` |

**`entities`** — one row per extracted language construct (functions, classes, calls, literals, control-flow nodes, etc.)
| column | type | notes |
|---|---|---|
| `id` | INTEGER PK | |
| `kind` | INTEGER | see `EntityKind` codes below |
| `name` | TEXT | |
| `file_id` | INTEGER | FK to `files.id` |
| `start_byte`/`end_byte`, `start_line`/`start_col`, `end_line`/`end_col` | INTEGER | |
| `enclosing_function` | TEXT, nullable | bare name of the narrowest enclosing named function — **not** an entity id, so same-named functions in one file collide (see `function-complexity-hotspot`'s known-limitation comment) |
| `method`, `path`, `status`, `body_shape` | TEXT, nullable | kind-specific payload (e.g. `Route`/`Response` entities) |
| `body_minhash` | BLOB, nullable | clone-detection signature |
| `is_async` | BOOLEAN, nullable | |

**`entities.kind` codes** (`EntityKind::as_i64`, append-only — never reorder):
`0` function, `1` class, `2` interface, `3` variable, `4` parameter, `5` export, `6` call, `7` literal, `8` member_access, `9` import, `10` catch, `11` throw, `12` control_flow, `13` route, `14` response, `15` extends, `16` implements, `17` decorator.

**`symbols`** — lighter-weight symbol table (id, kind, name, file_id, span). Same `start_*`/`end_*` shape as `entities`, no `enclosing_function`/kind-specific payload columns.

**`resolved_edges`** — one row per resolved relationship between files/entities.
| column | type | notes |
|---|---|---|
| `id` | INTEGER PK | |
| `from_file_id`, `to_file_id` | INTEGER (`to_file_id` nullable) | |
| `kind` | INTEGER | see `EdgeKind` codes below |
| `resolved` | INTEGER (bool) | `1` if the edge target was actually resolved to a file, `0` for unresolved (external/unresolvable import etc.) — filter on `resolved = 1` for real graph traversal |
| `from_entity_id`, `to_entity_id` | INTEGER, nullable | entity-level endpoints when known |

**`resolved_edges.kind` codes** (`EdgeKind::as_i64`): `0` call, `1` import, `2` extends, `3` implements.

**`diagnostics`** — file_id, message, severity (parse/extraction diagnostics, not scan findings).

**`communities`** / **`community_members`** — clustering groups and file membership.

**`clone_bands`** / **`clone_band_members`** — near-duplicate-code groups and entity membership (drives `duplicate-code-clone`).

Indexes exist on `resolved_edges(from_file_id/to_file_id/from_entity_id/to_entity_id)` and `entities`/`symbols`(`file_id`, `name`) — filtering/joining on those columns is cheap; anything else is a full scan.

### Query conventions

- Bind thresholds/strings as `:key` — every `thresholds`/`strings` key must appear in `query` as `:key` or it's inert; every `:key` in `query` must have a matching `thresholds`/`strings` entry or the query fails to bind at scan time.
- `JOIN files f ON f.id = <table>.file_id` to surface `path` as the `file` column.
- Self-joins on `resolved_edges` are the pattern for graph checks (see `circular_import.toml`): join the table to itself on swapped `from`/`to`, dedupe symmetric pairs with `e1.from_file_id < e1.to_file_id`.
- `GROUP BY ... HAVING` for per-entity aggregates (see `function-complexity-hotspot` in `complexity.toml`) — `MIN(start_line)` picks a representative line when grouping collapses multiple rows.

## Test entries

`[[test]]` blocks are self-tests validated by `varde-code rules test`, inert to `scan`. Every entry needs `name`; the rest is kind-specific.

**Pattern rules:**
```toml
[[test]]
name = "flags a plain assignment"
invalid = ["api_key = \"sk_live_1234567890abcdef\""]   # snippets that MUST match
valid = ["api_key = os.environ[\"API_KEY\"]"]           # snippets that must NOT match
```
If the rule sets `rewrite`, add `expect_rewrite` (map of input snippet → expected output after the rewrite template is applied):
```toml
[test.expect_rewrite]
"old_call(x)" = "new_call(x)"
```

**SQL rules:** an inline fixture file tree is written to a temp dir, indexed, and the rule's query run against it; results are compared as an order-insensitive multiset of rows.
```toml
[[test]]
name = "flags a file over the complexity threshold"
[test.fixture]
"big.py" = "if a:\n if b:\n  if c:\n   pass\n"
[[test.expect_rows]]
file = "big.py"
line = 1
```

## Scopes and precedence

- **Built-in** — embedded in the `varde-code` binary (`crates/varde-code/src/rules/builtin/*.toml`), lowest priority.
- **User** — `$VARDE_USER_RULES_DIR` or `~/.config/varde-code/rules/`, applies across every repo.
- **Repo** — `<repo_root>/.varde-code/rules/`, applies to this repo only, highest priority.

Merge is by `id`: repo wins over user wins over built-in, silently (no diagnostic) — this is the intended way to disable/replace a default. `varde-code rules seed` materializes the built-ins as editable files in either scope so they can be edited in place instead of fully re-authored.
