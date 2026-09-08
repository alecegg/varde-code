# Built-in rule pack — scoping

Goal: a small, purely deterministic set of built-in rules shipped with varde-code. "Deterministic" here means: given an unchanged repo, the rule produces the same findings on every run — no heuristics, no scoring, no LLM judgment. Every rule below is either a direct `ast-grep` pattern match or a plain SQL query against the persisted intelligence DB.

Kept intentionally short. Cut anything that's (a) already well-covered by existing linters (ESLint, Clippy, etc.) or (b) high-noise/low-signal (style nits, TODO comments, `==` vs `===`). Preference given to rules that use data unique to varde's persisted DB (churn, complexity, clones) since that's the differentiator — generic AST pattern rules are commodity.

Schema referenced below (from `crates/varde-code/src/db.rs`):
- `files(id, path, mtime, size, content_hash, complexity, churn, fan_in, fan_out, community_id)` — complexity/churn are **file-level aggregates**, not per-function.
- `entities(id, kind, name, file_id, start_byte, end_byte, start_line, start_col, end_line, end_col, enclosing_function, method, path, status, body_shape, body_minhash, is_async)`
- `clone_bands(id, label)` / `clone_band_members(band_id, entity_id)`
- `EntityKind`: `Function, Class, Interface, Variable, Parameter, Export, Call, Literal, MemberAccess, Import, Catch, Throw, ControlFlow, Route, Response`
- No exported/visibility column exists — `Export` is only visible as an EntityKind row, so a "dead code" / "orphan export" rule isn't cleanly expressible yet and is out of scope for this pack.

Rule TOML shape (confirmed from `crates/varde-code/src/rules/mod.rs`):
```toml
[[rule]]
id = "..."
kind = "sql" | "pattern"
severity = "error" | "warning" | "info"
message = "..."
query = "..."            # sql only
thresholds = { k = 1.0 } # sql only, f64 :param binds
strings = { k = "..." }  # sql only, String :param binds
pattern = "..."          # pattern only
constraints = { CAP = "regex" } # pattern only, per-capture
languages = ["typescript", "go"] # pattern only, default = all 10 supported
```

---

## SQL rules (DB-backed, varde-differentiated)

### 1-3. `complexity` rule set — **shipped** ✅ (`rules/builtin/complexity.toml`)
Three rules sharing one signal (cyclomatic complexity, `1 + count(ControlFlow entities)`) at three granularities, shipped as one pack:

- **`file-complexity-hotspot`** — `SELECT path AS file, 1 AS line FROM files WHERE complexity > :max_complexity` (`max_complexity = 50.0`). Single-column, single-table, zero ambiguity — complexity is already computed and persisted. **Risk:** a file-level aggregate can hide a hotspot inside one function of an otherwise-fine file — that gap is exactly what `function-complexity-hotspot` (below) closes.
- **`churn-complexity-hotspot`** — `SELECT path AS file, 1 AS line FROM files WHERE complexity > :max_complexity AND churn > :min_churn` (`max_complexity = 50.0`, `min_churn = 20.0`). The actual risk signal — complex *and* frequently changed code is where bugs concentrate; neither alone is as strong. Most varde-differentiated rule in the pack; no standard linter can express it. **Risk:** none structurally, purely threshold-tuning.
- **`function-complexity-hotspot`** — `SELECT f.path AS file, MIN(fn.start_line) AS line, fn.name, 1 + COUNT(cf.id) AS complexity FROM entities fn JOIN files f ON f.id = fn.file_id LEFT JOIN entities cf ON cf.file_id = fn.file_id AND cf.enclosing_function = fn.name AND cf.kind = 12 WHERE fn.kind = 0 GROUP BY f.id, fn.name HAVING 1 + COUNT(cf.id) > :max_function_complexity` (`max_function_complexity = 15.0`). No new extraction or schema work — `entities.enclosing_function` already names the narrowest enclosing function for every entity extracted inside it, so a per-function complexity count is a pure SQL aggregation over existing columns. **Why:** catches a single complex function buried in an otherwise-simple file, which the two file-level rules above structurally can't. **Risk:** `enclosing_function` is a bare name, not an entity id — two same-named functions in one file (overloads, or a top-level function shadowed by a same-named nested one) share a `GROUP BY` bucket and their control-flow counts merge; `line` for the merged bucket is reported via `MIN()` arbitrarily. This can mis-attribute complexity between the colliding pair but never drops real complexity from the count. Accepted for a first pass — true per-entity linkage would need `enclosing_function` to store an id instead of a name.

### 3. `duplicate-code-clone` — **shipped** ✅
- **Query shape:** `SELECT f.path AS file, e.start_line AS line, cb.label FROM clone_band_members cbm JOIN entities e ON e.id = cbm.entity_id JOIN files f ON f.id = e.file_id JOIN clone_bands cb ON cb.id = cbm.band_id WHERE cb.id IN (SELECT band_id FROM clone_band_members GROUP BY band_id HAVING COUNT(*) >= :min_band_size)` — one finding per member of each qualifying band, band label as evidence.
- **Sketch fix:** the original sketch self-joined `clone_band_members` (cbm2) and counted `COUNT(*)` over the join — that counts N² rows per band, so a 2-member band scored 4 and would pass a `min_band_size` of 3. The shipped query filters bands by member count in a subquery instead.
- **Threshold:** `min_band_size = 3.0` — confirmed conservative: a band forms when ≥2 entities share one 4-row LSH band of a 16-row MinHash signature (shingle size 5). At Jaccard ~0.5 the per-band collision probability is only ~23%, so a pair can still be incidental; three members sharing a band is a strong triplication signal. Bands of two are deliberately not flagged.
- **Why:** clone detection already runs (`resolve::clones`) and is persisted; surfacing bands above a size threshold is a pure aggregation, no new analysis needed.
- **Risk:** low at this threshold — a 3-member band requires three bodies with ≥0.5 shingle-similarity; short/boilerplate bodies (< 5 tokens) never get a signature, so getters and empty handlers can't form bands. Repo-scope overrides remain the escape hatch for generated-code directories.

### 4. `low-fan-in-high-fan-out-file` — **shipped** ✅ (`rules/builtin/low_fan_in_high_fan_out_file.toml`)
- **Shipped query:** `SELECT path AS file, 1 AS line FROM files WHERE fan_in <= :max_fan_in AND fan_out >= :min_fan_out` — exactly the doc sketch.
- **Severity `info`, not warning** — the doc's fallback for the entry-point risk. Info findings never fail a scan at the default threshold, so the rule is safe default-on without a path-exclusion convention.
- **Thresholds calibrated against real repos:** `max_fan_in = 2`, `min_fan_out = 15` (inclusive bounds). On code-rails (6,050 indexed files) this flags exactly **4 files (0.07%)** — three heavy integration tests (`foreign-data-focused-function.test.ts` 0/20, `boolean-argument-controls-flow.test.ts` 0/16, `git-history.test.ts` 0/15) plus `rules/design/index.ts` 0/16, a barrel re-export no one imports. On varde-code (2,574 files): **0 files**. Entry points were *not* the dominant FP class — `cli.ts` has fan-in 51, so real hubs importers depend on are naturally excluded; test suites are the structural shape (they import everything, nothing imports them), and those are genuine signals (integration-heavy tests, dead barrel exports).
- **Risk:** low at these thresholds; if `*.test.ts` noise appears, a path-exclusion convention is the documented follow-up, and info severity keeps it safe meanwhile.

### 9. `circular-import` — **shipped** ✅ (`rules/builtin/circular_import.toml`)
- **Query shape:** self-join on `resolved_edges` — `A imports B AND B imports A` (`kind = 1` = import, `resolved = 1`), deduped to one row per cycle via `e1.from_file_id < e1.to_file_id`.
- **Why:** sourced from a comparison against the [fallow](../reference-repos/fallow) reference repo's `CircularDependency`/`ReExportCycle` finding kinds — a direct import cycle makes initialization order load-order-dependent and, in some module systems, exposes a partially-populated module to one side. No new extraction or schema work: `resolved_edges` already persists every resolved import edge.
- **Scope:** direct (2-file) cycles only. Verified against this repo's own indexed fixtures — the query correctly found 3 real cycles in `resolve_fixtures/rust/communities/`. Transitive N-file cycles would need a recursive CTE with a depth guard to avoid path explosion on dense graphs; deferred, not needed for the common case.
- **Risk:** low — a direct cycle either exists in the resolved graph or it doesn't, no threshold to mis-tune.

### Considered from the fallow comparison, not shipped
- **`unresolved-import`** (fallow: `UnresolvedImport`) — varde already computes this internally (`resolved_edges WHERE kind=1 AND resolved=0`), but `resolved_edges` has no link back to the specific import entity or its specifier text, only file-to-file. That matters because `resolve_imports` marks *every* import unresolved, including legitimate third-party package imports (`react`, `lodash`) — the resolver only tries same-repo files, and its own code comment notes unresolved imports fire "tens of thousands of times" on a large repo. Shippable only after `resolved_edges` (or a new table) carries the import specifier or entity id, so the rule can filter to relative-path imports only.
- **`unused-file`** (fallow: `UnusedFile`) — `files.fan_in = 0` is a trivial query, but fallow's version relies on entry-point heuristics (package.json `main`/`bin`, CLI detection, dynamic-import awareness) varde doesn't have. Without that, every CLI entry point, test file, and type-declaration file false-positives. Not worth shipping default-on.
- Everything else in fallow's ~40 finding kinds (framework-specific React/Vue/Svelte/Angular findings, pnpm/package-manager findings, CSS findings, inline-suppression-comment tracking) requires semantic domains varde's language-agnostic, manifest-free extraction model doesn't cover — out of scope by design, not an oversight.

---

## Pattern rules (ast-grep, low-noise)

### 5. `empty-catch-block` — **shipped** ✅
- **Shipped as three rules** (the pattern engine carries one `pattern` per rule and each family's syntax differs): `empty-catch-block` (`try { $$$A } catch ($ERR) { }`, `typescript, tsx, javascript`), `empty-except-block` (`try:\n    $$$A\nexcept $ERR: pass`, `python`), `empty-catch-block-swift` (`do { $$$A } catch { }`, `swift`). Go has no direct equivalent (errors aren't exceptions).
- **Matcher limitation:** Java/C#/Kotlin have try/catch semantics but the hand-rolled pattern matcher cannot parse a catch-clause-shaped pattern in those grammars (the pattern wrapper only supports statement-level snippets; a bare `catch`/`except` clause is not a statement). They are excluded until the matcher grows a true pattern grammar.
- **Why:** swallowed errors are close to unambiguously bad; near-zero false-positive rate.
- **Risk:** legitimate empty catches exist (intentional suppression) — handled for free by the escape hatch: a comment inside the block body makes it non-empty (comments are AST children), so `catch (e) { /* intentional */ }` and a commented `except ...: pass` are **not** flagged. Truly empty handlers are. Python's `except $ERR: pass` also flags `except X as e: pass` (the `as`-clause is captured by `$ERR`) but not `except: pass` without a type expression.

### 6. `eval-usage` — **shipped** ✅
- **Pattern:** `eval($$$ARGS)` scoped to `javascript, typescript, tsx, python` — the callee must be a bare `eval` identifier, so `obj.eval(x)` does not match. Python's `exec` (statement, not a call) is deliberately left out; it would need its own pattern.
- **Why:** deterministic security smell, essentially never legitimate in application code.
- **Risk:** near-zero. Test frameworks or bundler-internal code occasionally use `eval` legitimately — repo-scope rule overrides can exclude specific paths if this proves noisy in practice.

### 7. `debug-statement-strict` — **shipped** ✅ (as a three-rule pack, `rules/builtin/debug_statement_strict.toml`)
- **Shipped rules:** `debug-macro-strict` (`dbg!($$$ARGS)`, rust), `debug-statement-strict` (`debugger;`, javascript/typescript/tsx), `console-log-strict` (`console.log($$$ARGS)`, javascript/typescript/tsx). Three rules because a pattern-kind rule carries one `pattern` and the shapes are per-language.
- **Narrowed set:** the `print`/`fmt.Println` half is deliberately **not** shipped — in CLI tools and scripts those are legitimate program output. The doc's two-rule recommendation became a two-family decision: strict half in, print-style half out entirely.
- **Why:** the `dbg!`/`debugger`/`console.log` half is very high-confidence, default-on safe.
- **Empirics:** `dbg!` requires a bare `dbg` callee (`println!` never matches); `console.log` matches only `.log` — `console.warn`/`console.error` do not; `debugger;` matches the statement in functions and class methods. Verified via the `find_pattern` CLI.

### 8. `hardcoded-credential-literal` — **shipped** ✅ (two-rule pack, `rules/builtin/hardcoded_credential_literal.toml`)
- **Shipped rules:** `hardcoded-credential-literal` (`$KEY = $VAL`, python/javascript/typescript/tsx) and `hardcoded-credential-declaration` (`const $KEY = $VAL`, javascript/typescript/tsx — the const pattern also covers `let`, since declaration keywords are anonymous tokens stripped as trivia). Two rules because the shapes differ (assignment vs. declaration).
- **Constraints (anchored — the engine applies regexes with unanchored `is_match`, so the rules anchor themselves):**
  - `KEY = ^(?i)(api[_-]?key|access[_-]?key|client[_-]?secret|secret|password|passwd|credential|(access|auth|refresh|id|session)[_-]?token|token)$` — bare identifiers only, so `config.api_key = ...` (member LHS) and `client_id = ...` never fire.
  - `VAL = ^["'][A-Za-z0-9_\-+/=]*[0-9][A-Za-z0-9_\-+/=]{15,}["']$` — quoted, ≥16 chars, at least one digit with 15+ chars after it, base64-ish alphabet. The digit requirement kills placeholders (`"your-api-key-here"`, `"changeme"`, `"correcthorsebatterystaple"`) without lookahead support (Rust `regex` crate). **Tuning lesson:** a first draft used `{15}[0-9]` (digit at exactly position 15), which wrongly dropped alternating hex like `9f8d7c6b...` — the digit must be allowed anywhere, hence `[*][0-9]{15,}`.
- **Matcher fix this rule required:** `$KEY = $VAL` exposed a `find_pattern` bug — `meta_of` misdetected any node whose text starts *and* ends with marker delimiters, so the assignment root (`__varde_meta_s_KEY__ = __varde_meta_s_VAL__`) was treated as one giant meta variable that matched the whole program. Fixed by requiring meta markers to be leaf nodes (`query/find_pattern.rs`, regression test `two_meta_variables_in_one_statement_bind_separately`).
- **Real-repo noise check:** probed `code-quality-rules` + `code-rails` (656 TS/JS files) and `camera` + `varde` Python (~1561 assignments) with the exact patterns + constraints — **zero hits** (no FPs; these repos contain no credential-named keys, positive cases only in fixtures).
- **Risk:** now low — env refs and non-literal values fail the VAL shape, placeholders fail the digit rule, member assignments fail the anchored KEY. Severity warning; repo-scope overrides remain the escape hatch for generated code.

---

## Recommended ship order

1. ✅ `complexity` rule set (#1-3: `churn-complexity-hotspot`, `file-complexity-hotspot`, `function-complexity-hotspot`) — most differentiated, cheapest to validate; merged into one pack since all three derive the same underlying signal
3. ✅ `empty-catch-block` (#5), `eval-usage` (#6) — lowest false-positive pattern rules
4. ✅ `duplicate-code-clone` (#3) — threshold confirmed conservative (3+ members sharing an LSH band)
5. ✅ `debug-statement-strict` (#7) — narrowed set shipped; print/fmt.Println variants left out entirely
6. ✅ `hardcoded-credential-literal` (#8) — value-regex tuned against real repos (zero hits), two-rule pack
7. ✅ `low-fan-in-high-fan-out-file` (#4) — shipped as info severity with calibrated thresholds
8. ✅ `circular-import` (#9) — added after a comparison against the fallow reference repo's finding-kind inventory

Not scoped (deferred): dead-code/unused-export detection, `unresolved-import`, `unused-file` — see "Considered from the fallow comparison, not shipped" above for the specific schema/heuristic gaps blocking each.
