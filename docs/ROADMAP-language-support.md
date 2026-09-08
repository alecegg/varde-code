<!-- Roadmap: bringing find_pattern-only grammars to full extraction/resolution. -->

# Roadmap: full language support

## Where we are

As of the "structural search across all ast-grep grammars" change, `find_pattern`
runs against **every** grammar `ast-grep-language` links (~28). The rest of the
engine — entity/symbol extraction, the SQLite index, and every query built on it
(`symbols_in_file`, `dependencies`/`dependents`, `blast_radius`, `type_hierarchy`,
`hotspots`, `map_*`, `context_pack`, `nav_map`, `scan`) — requires a hand-written
extractor. That set now stands at **21 languages** (`parse.rs::SUPPORTED_LANGUAGES`):
TS/TSX, JavaScript, Go, Java, C#, Kotlin, Swift, Python, Rust (original 10), plus
**Ruby** (Phase 0 pilot), **PHP, C, C++** (Phase 1), **Scala, Dart, Lua, Elixir,
Solidity, Haskell** (Phase 2 — ✅ complete), and **Bash** (Phase 3 — ✅ complete).
**All Tier A + Tier B languages now have full extraction support**; only **Tier C**
(config/markup: JSON, YAML, CSS, HTML, Markdown, HCL/Terraform, Nix) remains
intentionally `find_pattern`-only.

"Full feature state" for a language means: it has an extractor, so it is indexed and
answers the full query surface — not just structural pattern matching.

## The cost model (why this is mostly extraction work)

The single most important fact for planning: **resolution, metrics, and every query
are already language-agnostic.** They operate on the interned `Entity`/`Symbol`
model by name-matching, with no per-language type-system logic.

| Area | Per-language? | Notes |
|---|---|---|
| Entity/symbol extraction (`extract/langs/<lang>.rs`) | **Yes** | The real work: map this grammar's node kinds to the shared `EntityKind` model. Existing extractors run ~339–532 LOC. |
| Dispatch arms (`extract/langs/mod.rs`) | **Yes** | 4–5 match arms: `visit`, `function_scopes`, `type_scopes`, `required_kinds`, optional `type_scope_name`. |
| Parse gate (`parse.rs`) | **Yes** | `language_for_path` extension arm + add to `SUPPORTED_LANGUAGES`. |
| Role tags (`extract/langs/role_tags.rs`) | Optional | Declarative framework rules; only for web-entrypoint semantics. |
| Test fixtures (`tests/fixtures/<lang>/`) | **Yes** | Coverage-parity harness auto-discovers the dir; fixtures must exercise each declared `REQUIRED_KIND`. |
| Resolution (`resolve.rs`, `resolve/*`) | **No** | Name/path-based; works for any language once entities/symbols exist. |
| Complexity, churn, clones, communities | **No** | ControlFlow count, git log, MinHash, modularity — all generic. |
| Queries (`query/*`) | **No** | Consume the resolved graph. |

**Implication:** once a language has a correct extractor + the parse gate, `dependencies`,
`blast_radius`, `hotspots`, complexity, churn, and clone detection light up "for free."
The one quality caveat is import resolution (see Risks).

## Scope: which languages get full support

The find_pattern-only set splits cleanly. **Not every grammar should get an extractor** —
for config/markup grammars "entities and symbols" is ill-defined and low-value, and
`find_pattern` is already the right tool.

- **Tier A — real programming languages, worth full support** (priority order by
  ecosystem demand): **Ruby, PHP, C, C++, Scala, Dart, Lua, Elixir, Solidity, Haskell**.
- **Tier B — shell**: **Bash**. Useful for entrypoint/script analysis; smaller entity
  surface (functions, calls, variables). Medium value.
- **Tier C — config/markup, stay find_pattern-only** (no extractor planned):
  **JSON, YAML, CSS, HTML, Markdown, HCL/Terraform, Nix**. Revisit only on a concrete
  request (e.g. Terraform resource graphs would be a *bespoke* extractor, not the
  generic entity model).

This roadmap covers Tier A + Bash. Tier C is explicitly out of scope; document it as
"structural search only" so the omission is intentional, not a gap.

## Per-language playbook (Definition of Done)

For each Tier A/B language `X`:

1. **Extractor** — `crates/varde-code/src/extract/langs/x.rs`
   - Declare `FUNCTION_SCOPES`, optional `TYPE_SCOPES`, and `REQUIRED_KINDS` (the subset
     of the 18 `EntityKind`s this language can express — carve out what the grammar
     lacks, e.g. no `Interface` for C).
   - Implement `visit(node, kind, ctx)` mapping tree-sitter node kinds → entities
     (functions, types, calls, imports, control flow, catch/throw, member access,
     literals, parameters, variables). Use an existing extractor of similar shape as the
     template: Go (339 LOC, procedural) for C/Bash/Lua; Python (532 LOC) for Ruby/PHP/
     Elixir; a class-based one (Java/C#/Kotlin) for Scala/Dart/Solidity.
2. **Dispatch** — add arms in `extract/langs/mod.rs` (`visit`, `function_scopes`,
   `type_scopes`, `required_kinds`; `type_scope_name` only if needed).
3. **Parse gate** — `parse.rs`: add the extension(s) to `language_for_path` and add the
   variant to `SUPPORTED_LANGUAGES`.
4. **Fixtures** — `tests/fixtures/x/` covering every `REQUIRED_KIND`, plus a
   `resolve_fixtures/x/` case with a cross-file import so `dependencies`/`dependents`
   are exercised.
5. **Role tags** (optional) — add an `X_RULES` table for that ecosystem's web frameworks
   (e.g. Rails for Ruby, Laravel for PHP) if entrypoint tagging matters.
6. **Docs** — update the README scope list and `docs/CLI.md` language coverage.

**Acceptance criteria (per language):**
- `cargo test --test coverage_parity` passes for `X` (every declared kind extracted; no
  surprise kinds).
- `extract_*` suites green.
- A `build` + `dependencies`/`dependents` smoke on the resolve fixture returns the
  expected edges.
- `find_pattern` on `X` still works (already true).
- fmt + clippy `-D warnings` clean; no `find_pattern` benchmark regression past the 2×
  ast-grep budget (extraction changes shouldn't touch that path, but re-run per AGENTS.md).

## Sequencing

**Phase 0 — de-risk with one pilot (Ruby).** Ruby is high-demand, has a clean grammar,
and is expression-oriented (forgiving to extract). Take it fully end-to-end to validate
and refine this playbook — especially the fixture/coverage-parity loop and the
import-resolution quality question. Capture anything that generalizes.

> **Phase 0 status: ✅ complete (Ruby).** Landed as a full extraction/indexing language:
> `src/extract/langs/ruby.rs` + dispatch/parse-gate wiring, `tests/fixtures/ruby/`,
> `resolve_fixtures/ruby/imports/`, and a `ruby import resolution` check in the
> resolve harness. Verified: coverage-parity green for all 13 non-carved kinds, resolve
> harness green, and an end-to-end `build` + `dependencies` CLI smoke resolves
> `require_relative` across files. Generalizable learnings for Phases 1–3:
> - **Dump the grammar first.** A ~40-line throwaway `examples/dump_<lang>.rs` that prints
>   each named node's kind + resolvable field names (`name`/`left`/`receiver`/…) is the
>   fastest way to get the node-kind mapping right; write the extractor against real output,
>   not assumptions. Delete it before committing.
> - **Map to the nearest existing kind, don't invent.** Ruby has no interface/export syntax:
>   `module` → Interface (+ `include` → Implements), Export carved out. Prefer a defensible
>   mapping onto the 18 kinds over extending the enum.
> - **Per-language required-kinds set.** Encode carve-outs in the extractor's `REQUIRED_KINDS`
>   (Ruby = 13, Export dropped); the coverage-parity harness reads it, so a language only needs
>   fixtures for what it can express. Also add the language to the harness's `required_kinds`
>   match and the resolve harness's extension match.
> - **Statement-vs-call ambiguity is normal in dynamic langs.** Where a construct is a bare
>   method call (`require`, `raise`, Sinatra `get`), branch inside the `call` arm and emit the
>   semantic kind (Import/Throw/Route) instead of a generic Call, returning early to avoid
>   double-counting.
> - **Web shape is worth including.** A narrow framework shape (Sinatra here, Flask for Python)
>   is enough to light up Route/Response and keep parity with the other languages' full kind set.
> - The `SUPPORTED_LANGUAGES` array→slice change (a cross-cutting item below) is done, so each
>   further language is a one-line append.

**Phase 1 — high-demand batch:** PHP, C, C++. (C/C++ share most node-kind mapping; do
them together.)

> **Phase 1 status: ✅ complete.** PHP (twelfth), C (thirteenth), C++ (fourteenth) all
> landed as full extraction/indexing languages, each with extractor + wiring + fixtures +
> a resolve import-resolution check, verified green (coverage-parity, resolve harness,
> per-language unit tests, and an end-to-end `build` + `dependencies` CLI smoke). Notes:
> PHP maps trait → Class and interface → Interface (it has a real `interface` keyword,
> unlike Ruby); C carves out Interface/Export/Catch/Throw/Route/Response (no exceptions,
> no classes-as-such — structs map to Class); C++ adds class inheritance (Extends) and
> exceptions (Catch/Throw) back. The **C declarator chain** (`function_definition` →
> `function_declarator`/`pointer_declarator`/`reference_declarator` → `identifier`) is the
> one real gotcha — name extraction must recurse through it. **Key cross-cutting finding (now fixed):**
> the symbol (Binding/Reference) layer was JS/TS-shaped and didn't generalize — PHP yielded
> zero symbols. Resolved by making symbol classification language-aware; see the Risks section.

**Phase 2 — remainder of Tier A:** Scala, Dart, Lua, Elixir, Solidity, Haskell — by
demand. Each is an independent, parallelizable unit of work once the playbook is proven.

> **Phase 2 status: ✅ COMPLETE — Scala ✅ (fifteenth), Dart ✅ (sixteenth), Lua ✅ (seventeenth), Elixir ✅ (eighteenth), Solidity ✅ (nineteenth), Haskell ✅ (twentieth). All six Phase-2 Tier-A languages landed. Phase 3 (Bash) has since landed too (twenty-first) — see below.**
> Scala landed as a full extraction/indexing language
> (`src/extract/langs/scala.rs`), following the class-based template
> (Java/Kotlin). `def` → Function; `class`/`case class`/`object` → Class;
> `trait` → Interface; `extends X` → Extends with each `with Y` mixin →
> Implements (generic `[…]` type args stripped — Scala uses square brackets, not
> `<>`); `val`/`var` → Variable; params → Parameter; `field_expression`
> (`x.foo`) → MemberAccess; `throw new E` → Throw, `catch { case e: E => }` →
> Catch; if/while/for/match/try/return → ControlFlow. Carve-outs: Export (no
> export declaration in Scala) and Route/Response (no single idiomatic web DSL —
> same stance as C). Imports normalize the dotted path `.` → `/`; being
> package-based they generally don't stem-match files, so most stay unresolved
> by design (accepted approximation).
>
> Dart landed next (`src/extract/langs/dart.rs`), also class-based.
> `function_signature`/`constructor_signature` → Function; `class`/`abstract
> class` → Class; `mixin` → Interface; `extends X` → Extends with each `with Y`
> mixin and each `implements Z` → Implements (generic `<…>` type args stripped);
> fields/statics/`var`/`final`/`const` → Variable; params → Parameter;
> `member_expression` (`x.foo`) → MemberAccess; `throw E(…)`/`rethrow` → Throw;
> `catch (e)`/`on T catch` → Catch; if/for/while/switch/return/break/continue/
> try → ControlFlow; `@override`-style `annotation` → Decorator. Unlike most
> languages, Dart has a genuine `export` directive, so `export 'x.dart'` →
> Export (NOT carved out); a relative `import 'x.dart'` resolves to a sibling by
> file stem. Carve-out: Route/Response only (no single idiomatic Dart web DSL —
> same stance as C/Scala).
>
> Lua landed next (`src/extract/langs/lua.rs`), procedural/dynamic (modeled on
> Go + Ruby, no class-based path). `function_declaration`/`function_definition`
> → Function (table methods `function M.m()`/`function M:m()` keep the qualified
> name); `assignment_statement` targets (local, global, multiple) → Variable;
> `parameters` → Parameter; `function_call` → Call; `dot_index_expression`
> (`t.k`) / `method_index_expression` (`t:m`) → MemberAccess; string/number/
> true/false/nil → Literal; if/elseif/else/for/while/repeat/do/return/break/
> goto/label → ControlFlow. Lua has no `throw`/`catch`/`require` keywords, so
> these are call-idioms: `require "mod"`/`require("mod")` → Import (resolves to a
> sibling by file stem); `error(…)`/`assert(…)` → Throw; `pcall`/`xpcall` →
> Catch. Carve-outs: Class AND Interface (Lua OOP is convention-based via
> tables/metatables, not expressible structurally — so Extends/Implements are
> unreachable too), Export (the module pattern is a bare `return M`, not a
> declaration), and Route/Response (no single idiomatic Lua web DSL).
>
> Elixir landed next (`src/extract/langs/elixir.rs`), call-node/dynamic (modeled
> on Ruby). In tree-sitter-elixir almost every construct is a `call` node whose
> callee is the `target` field, so `visit` matches `kind == "call"` and
> dispatches on the callee identifier. `def`/`defp`/`defmacro` → Function (name
> + Parameters from the nested function-head call); `defmodule` → Class;
> `defprotocol` → Interface; `defimpl A, for: B` → Implements; `defstruct` →
> Class; module attributes `@x v` (`unary_operator` `@`) → Variable;
> `import`/`alias`/`require`/`use` → Import; `=` match → Variable; remote
> `A.b(…)`/`x.field` (target is a `dot`) → MemberAccess + Call; local calls →
> Call; integer/float/string/atom/boolean/nil/char/charlist → Literal;
> `raise`/`throw` → Throw; `rescue`/`catch` blocks → Catch;
> `if`/`unless`/`case`/`cond`/`for`/`with`/`receive`/`try` → ControlFlow; and a
> narrow Phoenix router shape (`get "/p", Ctrl, :action`) → Route. Carve-outs:
> Export (Elixir has none — visibility is `def` vs `defp`), Response (no single
> idiomatic response DSL — same stance as C/Scala/Lua), and enclosing/owner-type
> scope-stacking (because `def` and `defmodule` share the single `call` kind and
> can't be distinguished by `node.kind()`, both scope lists are empty, so
> entities carry `enclosing_function = None`/`owner_type = None` — an accepted
> approximation for a call-node language). Imports use the shared stem matcher
> (`alias Helper` → sibling `Helper.ex`); module-based imports with no matching
> sibling file stem stay unresolved by design. Remaining Phase 2: Haskell.

> **Solidity (nineteenth).** Class-based template (Scala/Dart):
> `contract_declaration`/`library_declaration` → Class,
> `interface_declaration` → Interface, `function_definition`/
> `modifier_definition`/`constructor_definition`/`fallback_receive_definition`
> → Function. The `is Base, IFace` inheritance list has no syntactic
> base-vs-interface distinction, so the first `inheritance_specifier` → Extends
> and the rest → Implements (mirrors scala.rs's first-vs-rest rule). The
> error-check idioms `require`/`assert` and every `revert_statement` map to
> **Throw** (not Import — `require` is not a module load), with require/assert
> returning early so no duplicate Call is emitted. `import_directive` uses the
> shared stem matcher (`import "./b.sol"` → sibling `b.sol`). Export (no
> `export` keyword) and Route/Response (no web DSL) are carved out.

> **Haskell (twentieth — completes Phase 2).** The most unusual grammar in the
> set: pure functional, no OOP, no statement-level control flow, no exception
> syntax. `function` (a pattern clause `f x = …`) → Function, emitted **per
> clause** (a Haskell function is a set of pattern-matched equations, so `g 0 =
> …`/`g n = …` yield two Function nodes — the resolver tolerates repeats);
> separate `signature` nodes (`f :: …`) are NOT emitted (no phantom duplicate).
> `bind` (a no-pattern value binding, incl. `let`/`where`) → Variable.
> `data`/`newtype`/`type` → Class (no OOP class; these are the defined-type
> declarations), `class` (a **typeclass**) → Interface, `instance` → Implements
> (the typeclass). The module header's `export` entries → Export (a real,
> cleanly-exposed export list — not carved out). `apply` → Call with a
> `qualified` head (`M.lookup`) also → MemberAccess; `variable` leaves under
> `patterns` → Parameter; `integer`/`float`/`string`/`char` → Literal. No
> exception syntax, so the library idioms `error`/`throw`/`throwIO`/`ioError` →
> **Throw** and `catch`/`handle`/`try`/`bracket`/`finally` → **Catch**. No
> statement control flow, so the branch/binding *expressions* `if`/`case`/guards/
> `let … in` → **ControlFlow**. Carve-outs: Extends (no inheritance relation),
> Decorator (no annotations), Route/Response (no single web DSL). Imports
> normalize the module path `.` → `/`; being module/package-based they generally
> don't stem-match files, so most stay unresolved by design (a crafted fixture
> where the module name equals a sibling file stem exercises the RESOLVED path,
> mirroring Elixir). The symbol classifier is extended for Haskell's `variable`
> identifier leaf (it has no `identifier` node).

**Phase 3 — Bash** (Tier B) — ✅ **COMPLETE**. Full extraction support landed
(twenty-first language). Mapping: `function_definition` → Function (both `foo()`
and `function foo` forms), `variable_assignment` → Variable, `export …` → Export
(Bash has a real `export` builtin), `command` → Call, `source`/`.` → Import
(stem-matches sibling files, genuinely resolved), string/raw_string/number →
Literal, and `if`/`for`/`while`(covers `until`)/`case` + `return`/`break`/`continue`
commands → ControlFlow. Carve-outs (documented in `bash.rs`): Class/Interface/
Extends/Implements (no types), Parameter (positional `$1`/`$2`, not named),
MemberAccess (only array subscript, not a named member), Throw (no exceptions),
Catch (`trap … ERR` isn't a lexical protected region), Route/Response (no web DSL).
The symbol classifier is extended for Bash's `variable_name` leaf (used by `$x`/
`${x}` expansions; it has no `identifier` node).

Languages are independent after Phase 0, so Phases 1–3 can proceed in any order or in
parallel; the pilot is the only hard dependency.

## Cross-cutting work (not per-language)

- **Grow `SUPPORTED_LANGUAGES` cleanly.** ✅ Done in Phase 0 — switched from a fixed-size
  `[SupportLang; 10]` array to a `&[SupportLang]` slice const, so each further language is a
  one-line append with no size bump.
- **Wire role tags into queries.** Role-tag tables exist but are not yet consumed by any
  query (the "semantic entrypoint" surface). Independent of language count; do it once if
  entrypoint queries are wanted, then new languages just add a rules table.
- **Import-resolution strategies (optional quality lift).** See Risks.
- **`persist.rs` schema gates.** Adding languages should not change the schema; only bump
  `SCHEMA_VERSION` if a new `EntityKind` field or column is introduced. Most languages
  need nothing here.

## Risks & caveats

- **Import resolution is generic, not language-aware.** `match_import_target` in
  `resolve.rs` resolves imports by relative-path + file-stem matching, with no knowledge
  of `node_modules`, Python packages, Go modules, Ruby gems, PHP PSR-4, etc. So
  `dependencies`/`dependents` will be *approximate* for languages whose imports don't map
  to file paths (e.g. Ruby `require`, PHP namespaces). Extraction still makes the language
  fully queryable; tightening import resolution per-language is an **optional follow-up**,
  not a blocker. Flag this explicitly in each language's PR.
- **Grammar node-kind drift.** tree-sitter grammar versions change node kinds across
  releases; extractors are coupled to the exact grammar version in `Cargo.lock`. A grammar
  bump can silently change extraction. Coverage-parity fixtures are the guard — keep them
  representative.
- **`EntityKind` fit.** A few languages have constructs the shared 18-kind model doesn't
  name well (e.g. Elixir protocols, Rust-style traits vs interfaces). Prefer mapping to the
  closest existing kind over adding kinds; only extend `EntityKind` if multiple languages
  need it (it touches the schema and every extractor's `required_kinds`).
- **Benchmark budget.** Extraction is off the `find_pattern` hot path, but `build` time
  scales with per-file extraction cost; keep new extractors single-pass (walk once), as the
  existing ones do.
- **Symbol (Binding/Reference) layer — ✅ made language-aware.** Surfaced by the PHP work:
  `src/extract/symbol.rs` previously gated symbol classification on `identifier` nodes with a
  JS/TS-only name-position exclusion list, so **PHP produced zero symbols** (its grammar uses
  `name`/`variable_name`, never `identifier` → empty `symbols_in_file`/`get_symbol`/
  `filter_symbols`) and non-JS languages that *do* use `identifier` (Ruby/C/C++) over-produced
  (declared/callee names leaked in as references). Fixed by dispatching on language: the
  original ten keep the JS/TS classifier (`classify_jsts`) byte-for-byte, while the four newer
  languages use a language-aware identifier gate (`is_symbol_ident` — PHP adds
  `name`/`variable_name`) plus a grammar-agnostic, field-driven classifier
  (`classify_generic`) that excludes declared-name/callee/member/declarator/parameter
  positions. Resolution never consumed symbols (`resolve()` does `let _ = symbols`), so this
  is contained to the query layer; verified by the full suite + new `extract_symbols` tests.
  Remaining minor imprecision (accepted, matches the existing Python behavior on the JS path):
  simple assignment *targets* still appear as references (distinct span from their Variable
  entity, so the non-redundancy invariant holds), and `Binding`s aren't emitted on the generic
  path (imports are already `Import` entities). New Tier A languages that use their own
  identifier node kinds should extend `is_symbol_ident`.

## Rough effort

Per Tier A language: ~350–500 LOC extractor + ~10 dispatch/gate lines + ~200–400 LOC of
fixtures ≈ **0.5–1.5 days** once the pilot proves the loop. Full Tier A + Bash ≈ **2–3
weeks** of focused work, fully parallelizable after the Ruby pilot. Tier C stays
find_pattern-only (zero work, documented as intentional).
