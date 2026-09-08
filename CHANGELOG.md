# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **`nav_map` entrypoints now carry the HTTP method and path for
  annotation/decorator frameworks.** Previously only call-based routes
  (Gin/Ktor/C# minimal-API) surfaced as `"GET /users"`; a FastAPI/Spring/ASP.NET
  MVC/Kotlin-Spring/NestJS handler surfaced only by function name because the
  route decorator's verb+path was discarded at extraction. Extractors now stamp
  `method`+`path` onto route `Decorator` entities (verb from the decorator name
  via `http_verb_for_annotation`; path from its string argument), and
  `entrypoints::detect` combines the class-level base path (`@RequestMapping`,
  `[Route]`, `@Controller`) with the method-level path — e.g. `getUser` now
  reports `GET /users/{id}`. The `symbol` stays the handler name (so it remains a
  valid `explore` target and flow-tree root); the verb+path are added as
  structured `method`/`path` fields and rendered as `getUser  (GET /users/{id})`
  in the text nav_map. Tests: `route_method_and_path_captured_for_annotation_handlers`,
  `semantic_entrypoint_nestjs_controller_routes`.

### Fixed
- **The repo-wide call fallback no longer over-converges on test doubles or
  library receivers.** The single-definition fallback (namespace-import
  languages: C#/Java/Kotlin/Scala/Swift) bound production callers to the wrong
  target in three ways, now all closed: (1) a callee defined *only* in a test
  file was "unique across the repo," so every production caller of that name
  resolved to the test double (one run had a single test `next` capturing 82
  production call sites) — test-file definitions are now excluded from the
  uniqueness index (a name defined once in production and once in a test also
  flips from ambiguous to the correct production def); (2) a call on a receiver
  positively typed as a *library* type (`_repo.FindOne()` where `_repo`'s type
  is not in the repo) bound to a coincidentally same-named in-repo method — the
  fallback is now vetoed for known-external receivers, while unknown-type
  receivers (inherited `obj.getId()`) still resolve; (3) a production caller
  could still reach a test-file definition through the *type-directed* pass when
  the only in-repo subtype of an external base was a unit-test fake
  (`TimeProvider`'s sole in-repo `GetUtcNow` being a test `FixedUtcTimeProvider`)
  — a production caller is now never allowed to resolve to a test-file
  definition (caller-aware: test callers still reach test code). Verified on
  semantic-kernel (643K entities): production→test call edges dropped to zero
  with resolution otherwise unchanged. New pure-path `db::path_is_test` mirrors
  the `files.is_test_path` SQL column for pre-persist resolution (parity-tested).
  Tests: `repo_wide_fallback_excludes_test_file_definitions`,
  `repo_wide_fallback_vetoes_library_receiver_calls`,
  `production_caller_does_not_resolve_to_test_definition`,
  `path_is_test_matches_sql_generated_column`.
- **NestJS entrypoints are no longer empty; TS class/method decorators are
  captured.** The TS extractor dropped a decorator on an *exported* class (it
  hangs off the wrapping `export_statement`, not the `class_declaration`) and
  never handled *method* decorators (a preceding sibling in the class body), so
  a NestJS `@Controller`/`@Get` controller produced no `Decorator` entities and
  no entrypoints. Both placements are now swept, so NestJS controllers and their
  actions surface as route handlers (with verb+path, per Added). Tests:
  `nestjs_exported_class_and_method_decorators_captured`,
  `semantic_entrypoint_nestjs_controller_routes`.
- **Rails-engine / namespaced controllers are now entrypoints.** Role-tag rules
  matched only the exact base classes `ApplicationController`/`ActionController::{Base,API}`,
  missing Devise engine controllers (`< Devise::SessionsController`) and
  app-specific bases (`< Admin::BaseController`, `< Api::BaseController`). A
  `*Controller` base-class *suffix* rule (new `*`-prefix mechanism in
  `match_role_tag`) now catches any class extending something ending in
  `Controller`. Tests: `semantic_entrypoint_rails_engine_and_namespaced_controllers`,
  `ruby_controller_suffix_rule_matches_engine_and_namespaced_bases`.
- **TypeScript `symbols_in_file` now includes class fields.** Class
  `public_field_definition`s and interface `property_signature`s (a NestJS
  entity/DTO's `@Column` properties) were dropped, so a TS type's data model was
  invisible (C#/Python captured fields fine). They are now emitted as `Variable`
  entities owned by their type. Test:
  `ts_class_fields_and_interface_properties_captured`.
- **Python receiver-qualified module calls now resolve.** A `crud.authenticate()`
  call where `crud` is a module the file imported (`from app import crud`,
  `import crud`) went unresolved — `from pkg import mod` targets the package, and
  the last-segment cross-file pass dropped `users.create()`/`items.create()` as
  ambiguous when `create` was defined in several sibling modules. The Python
  extractor now records each import's local binding name on `Import.owner_type`,
  and a new receiver-aware resolution pass maps `recv.method()` to the sibling
  module whose file stem is `recv` and that defines `method` (unambiguous within
  one file). Purely additive — the import graph is unchanged. Test:
  `python_receiver_qualified_module_calls_resolve` (+ `python/module_calls` fixture).
- **Scan: Ruby mixins no longer trigger interface SOLID rules.** Ruby `module`
  maps to `Interface` and `include` to `Implements`, but Ruby has no interface
  construct — `module`/`include` is mixin composition. The interface-contract
  rules misfired: `solid-lsp` flagged every `include` as an unimplemented
  contract, `too-many-interfaces` counted included mixins, and `fat-interface`
  flagged large helper modules. `.rb` files are now excluded from the
  interface-implementation rules (`solid-lsp`, `solid-isp`, `too-many-interfaces`)
  and Ruby *modules* (not classes) from `fat-interface` — a Ruby fat *class* is
  still flagged. Test: `ruby_mixins_do_not_trigger_interface_solid_rules`.
- **Minified/generated bundles are no longer indexed as source.** A checked-in
  minified/bundled file (a webpack bundle, a protoc stub) is machine output, not
  source an agent navigates — but indexing one let it dominate the graph. The
  cross-language audit found a single 490 KB `chat.js` (webpack, *not* named
  `*.min.js`) supplying 74% of a Kotlin repo's entities and 93% of its call
  edges, pushing real `.kt` files out of `foundational_files`/`symbols`/
  `hotspots` and the module graph. Extraction now skips a file whose CONTENT
  reads as minified/generated — a `Code generated … DO NOT EDIT` marker in the
  header, or predominantly very long lines (`noise_filter::is_minified_source`,
  applied in `scan::process_file`). The file stays tracked (hash recorded,
  incremental unaffected) but contributes no entities/symbols. Measured: the
  Kotlin sample repo dropped 90,603 → 19,360 entities and its foundational files
  became real `.kt` modules. Tests: `is_minified_source_detects_generated_and_minified_content`.
- **Scan no longer fires on more classes of generated files.** The
  generated/vendored path filter (which suppresses both scan findings and nav
  orientation) gained protoc Go (`*.pb.go`), the `*.gen.*` codegen convention
  (OpenAPI clients, etc.), and Alembic/DB migration revisions
  (`alembic/versions/`, `migrations/versions/`). The cross-language audit found
  `duplicate-code-clone` false positives on all three (Go `*.pb.go`, a TS
  `sdk.gen.ts`, Python migration files). Verified: 0 findings now land on any of
  these on the audited Go/Python repos. Test:
  `true_for_cross_language_audit_generated_files`.
- **Call resolution: Swift object-protocol methods no longer invent edges.**
  Extending the C# object-protocol fix to Swift (also a repo-wide-fallback
  language): a `hash(into:)` (Hashable), `encode(to:)` (Encodable), or an
  Equatable/Comparable operator (`==`, `<`, …) defined exactly once in a repo was
  captured by the single-definition fallback for every matching call, even on an
  unrelated receiver type (confirmed with a fixture: `widget.hash(into:)` wrongly
  resolving to an unrelated `Report.hash`). `resolve::build_repo_wide_unique_index`
  now skips these Swift protocol-requirement names alongside the C#/Java set.
  Test: `swift_object_protocol_method_is_not_captured_by_repo_wide_fallback` (+
  `swift/object_methods` fixture).
- **`nav_map` subsystem names are now unique.** Directory-based naming is not
  injective, so distinct Louvain communities sharing a dominant directory all
  rendered with the same bare name (the ASP.NET Core audit repo produced five
  separate `CatalogItemEndpoints` subsystems, plus repeated `Models`/`Interfaces`)
  — indistinguishable for orientation. `subsystems::name_clusters` now qualifies
  every collision with a representative member stem
  (`CatalogItemEndpoints/BaseRequest`, `.../DuplicateException`) and, in the rare
  case that is still not unique, the community id. Non-colliding names are left
  bare. Tests: `subsystems_clustering_disambiguates_colliding_directory_names`,
  `subsystems_clustering_leaves_unique_names_unqualified`.
- **`nav_map` no longer lists empty controller classes as entrypoints.** A class
  becomes an entrypoint via a type-level signal (`extends ControllerBase`,
  `[ApiController]`), but a controller with no action methods serves no routes —
  eShopOnWeb's `BaseApiController` (an empty `{ }` body commented "No longer
  used") was surfaced purely on its base class, bloating the highest-priority
  section. `entrypoints::detect` now gates class candidates on owning at least
  one method (a method carries its class name in `owner_type`). Method
  candidates are unaffected. Test:
  `semantic_entrypoint_empty_controller_class_is_not_an_entrypoint`.
- **`nav_map` flows no longer duplicate overloaded handlers.** A C# MVC GET/POST
  action pair (`EnableAuthenticator()` + `EnableAuthenticator(model)`) is two
  distinct entities sharing one (file, symbol), so it produced two near-identical
  flow trees that read as noise and wasted the section's small item budget. The
  flows assembly now keeps only the largest tree per (file, symbol). On
  eShopOnWeb the flows section went from repeated `EnableAuthenticator`/
  `ChangePassword` entries to five distinct call trees.
- **Call resolution no longer invents edges for object-protocol methods.** The
  repo-wide single-definition fallback (C#/Java/Kotlin/Scala/Swift) resolved a
  call to the sole in-repo definition of a name — but `ToString`/`Equals`/
  `GetHashCode` (and the Java `toString`/`equals`/`hashCode`, `close`, ...) have
  their canonical definition on the framework base class, not in the repo, so a
  single in-repo override captured *every* such call. On eShopOnWeb every
  `.ToString()` resolved to `ErrorDetails.ToString`, injecting a false edge into
  flows and the call graph. `resolve::build_repo_wide_unique_index` now skips
  these object-protocol names (an override is still reachable via the
  type-directed pass when the receiver type is known). Verified: 0 resolved call
  edges target any `ToString` on eShopOnWeb (was capturing all of them). Test:
  `csharp_object_protocol_method_is_not_captured_by_repo_wide_fallback`.
- **`nav_map` module_layers no longer starves to empty on real repos.** The
  architectural layering view (module dependency graph + cycles) is spent last
  in the token-budget priority order, and on any non-trivial repo the
  higher-priority sections exhausted the whole budget before it was reached —
  so `module_layers.edges` came back empty (`shown: 0`) on essentially every
  mainstream repo, silently dropping the single most useful architectural
  artifact. Found while auditing a clean-architecture ASP.NET Core API
  (eShopOnWeb): 20 real module edges computed, 0 shown. Fixed by reserving a
  small up-front token allotment (`MODULE_LAYERS_RESERVE_TOKENS`, capped at ¼ of
  the total budget so a tiny `maxTokensEstimate` isn't dominated) and trimming
  the edge list to fit that allotment plus any leftover — replacing the old
  all-or-nothing drop. Module edges are individually tiny (two directory paths +
  a count), so the reservation surfaces the top ~18 edges at the default budget
  without meaningfully shrinking the other sections (eShopOnWeb: 0→18 edges,
  total ~7.1K tokens; at `maxTokensEstimate:1000`, 6 edges still show). Edge
  truncation is now reported honestly via `guide.truncated["module_layers.edges"]`
  with the real surviving-edge count. Test:
  `module_layers_edges_survive_a_tight_budget`.

### Changed
- **`nav_map` now has a token budget (audit F1).** nav_map is injected at
  session start, so it must be fixed-cost, not proportional to repo size —
  unbudgeted it reached ~140K tokens on a mainstream C# repo (and ~70K on
  crewAI), enough to blow a context window on its own. The assembled map is now
  trimmed to a total token budget spent section-by-section in a fixed priority
  order (`entrypoints` → `foundational_files` → `subsystems` → `symbols` →
  `hotspots` → `module_layers` → `flows`), with a hard per-section item cap, an
  8-member cap per subsystem (surplus reported inline as `membersOmitted`), and
  edge/cycle caps on `module_layers`. The budget defaults to 8000 tokens and is
  overridable via a new `maxTokensEstimate` input (mirroring `context_pack`).
  Every cut is self-describing: a new `guide.truncated` block reports
  `{shown, total, more}` per trimmed section, where `more` names the follow-up
  that returns the full data — a dedicated query mode where one exists
  (`hotspots`, `filter_symbols`), otherwise re-running nav_map with a larger
  `maxTokensEstimate`. Measured: C# 140K→~9.5K, crewAI 70K→~9.4K tokens. The
  `--format text` renderer surfaces the same truncation summary.
- **`scan` no longer re-inlines per-rule static text on every finding (audit
  F2).** The identical `message`+`remediation` a rule emits were repeated on
  every one of its findings — on the C# repo the duplicate-code-clone
  message+remediation (~180 B) repeated across ~6,555 findings ≈ 1.3 MB of pure
  repetition. They are now hoisted into a one-per-rule `rules` legend
  (`{rule_id: {message, remediation}}`) stated once; `remediation` (always
  static) is dropped from every finding, and `message` is dropped from a finding
  only when it still equals the rule template (interpolated SQL `{column}`
  messages such as `fat-interface`'s "declares 16 methods" stay inline —
  lossless). The always-null `certainty` and `agent_instructions` fields are now
  omitted from findings that don't set them (null on ~100% previously). Measured
  lossless reduction: varde-code scan 74K→42K tokens (−43%).
- **`scan` collapses clone bands into one finding each (audit F3).** The
  `duplicate-code-clone` rule emitted one finding per band *member* — 58–95% of
  all findings on real repos — each carrying only its own `evidence.label` and
  location, never naming the other members, so a reader couldn't act on one
  without re-deriving the band. Each band is now a single finding whose
  `evidence` is `{band, members: [{file, startLine, endLine}, …]}`. On
  varde-code this cut findings 590→155 (clone findings 556→120 bands) and, with
  F2, scan output 74K→23K tokens (−69% total) — and every clone finding is now
  self-contained and actionable.
- **`symbols_in_file` / `symbols_in_files` return declarations by default, not
  reference noise (audit F4).** `reference`-kind symbols (call sites / usages)
  were 80–95% of a file's symbol rows — on `nav_map.rs`, 307 of 358 entries —
  drowning the declarations a caller surveying a file actually wants. They are
  now excluded by default (declarations + bindings remain); pass
  `includeReferences: true` to restore them. Measured: `symbols_in_file` on
  `nav_map.rs` 358→78 rows (~78% fewer tokens), with the full 616-row view still
  available on request.
- **All tool output is now repo-relative and line-only-span by default (audit
  F5 + F6).** The MCP schema requires an absolute `repoRoot` and the index
  stores each file exactly as the walker yielded it, so every emitted path
  re-stated the absolute prefix (~200× in a single `nav_map`) and every `span`
  shipped six fields (byte + line + col) where the line pair almost always
  suffices. A single post-processing pass at the query/scan serialization
  boundary now (a) strips the `repoRoot` prefix from every path — relative
  paths round-trip because inputs already resolve a `filePath` by suffix match,
  and out-of-repo paths like `dbPath` are untouched — and (b) drops the
  `start_byte`/`end_byte`/`start_col`/`end_col` fields from every span, keeping
  `start_line`/`end_line`. Opt back into the old shapes per call with
  `absolutePaths: true` and `includeSpanDetail: true` respectively (the byte
  offsets an `--apply` rewrite needs are read from the in-memory finding before
  this runs, so trimming the JSON never affects splicing). Measured on
  varde-code: `nav_map` 19.5K→14.4K bytes from F5 alone (−26%), on top of the F1
  budget.
- **`nav_map` drops dead `flows` and sharpens the `symbols` leaderboard (audit
  F7 + F8).** `flows` was empty on most repos and, where present, a wrapper
  around each entrypoint with an empty `children` array — restating the
  `entrypoints` section at up to ~130 KB for ~0 marginal information (real call
  trees are sparse because call resolution rarely produces outgoing edges for a
  detected handler). Single-node flow trees are now omitted, so the section
  carries only genuine multi-node call trees. Separately, the `symbols` "core
  interfaces by fan-in" leaderboard ranked by raw call-edge count, which put
  getters/setters and stdlib methods (`push`, `get`, `as_str`, `setName`,
  `size`, `ConfigureAwait`, …) on top — high-frequency but zero
  orientation-value. It now ranks by **caller breadth** (the number of distinct
  files that call a symbol across a file boundary, reported as `callers`
  replacing `count`), with raw count kept only as a tie-breaker, and excludes
  low-orientation names (the `get`/`set`/`is`/`has` accessor pattern plus a
  curated stdlib/framework stopword set). On varde-code the leaderboard now
  leads with `parse_source`, `resolve`, `persist`, `language_for_path` rather
  than container methods.
- **nav_map orientation sections exclude front-end asset code (audit F9).** An
  Elixir/Phoenix repo surfaced its bundled `assets/js/phoenix/*.js` client as
  the top `foundational_files` and `symbols`, hiding every `.ex` controller. A
  new `is_frontend_asset_path` drops JS/TS/CSS-family files under an `assets/`
  directory from the three orientation sections (`foundational_files`,
  `symbols`, `entrypoints`) only — the files stay fully indexed, queryable, and
  scanned.
- **`foundational_files` sinks data classes below real modules (audit F9).** A
  JPA `@Entity`/DTO like `Person` or `BaseEntity` is depended on by many files
  (high breadth) but teaches nothing about architecture. A file whose methods
  are *all* trivial (accessors + `equals`/`hashCode`/`toString`/builder
  boilerplate) is now ranked after real modules of comparable fan-in instead of
  topping the list.
- **`build` no longer dumps the full changed-file list by default (audit
  F11).** The JSON result now carries `changedFilesCount` plus a small
  `changedFilesSample`; the full `changedFiles` array (every reparsed path in
  the repo on a full build) is opt-in via a new `--changed-files` flag.

### Fixed
- **Swift produced zero symbols and empty fan-in (audit F9).** Swift's
  expression identifier leaf is `simple_identifier`, not `identifier`, so the
  symbol classifier — gating on `identifier` — emitted no symbols for any
  `.swift` file (empty `symbols_in_file`/`get_symbol`/`filter_symbols`). And
  Swift has no cross-file `import` statements (same-module symbols are
  implicitly visible), so call resolution produced no cross-file edges, leaving
  nav_map's fan-in `symbols` leaderboard and `foundational_files` empty. Fixed
  both: Swift is now classified via the generic (field-driven) path, and it
  joins the repo-wide single-definition call fallback (previously C#/Java/
  Kotlin/Scala only) so a call to a uniquely-named function/type resolves to
  its definition anywhere in the module (ambiguous names stay unresolved — no
  false edge).
- **Call-based HTTP routes for Slim (PHP) and Phoenix (Elixir) were not
  detected (audit F10).** Phoenix routes (`get "/users", Ctrl, :index`) were
  already extracted but `.ex`/`.exs` was missing from the call-based-route
  gate, so they never surfaced as entrypoints — fixed. Slim routes
  (`$app->get('/users', $handler)`) are method calls, not the `Route::get`
  static form Laravel uses, and had no extractor support — added, guarded
  against PSR-11 container `->get('service')` false positives by requiring a
  route-object receiver (`$app`/`$router`/`$group`) and a slash-path argument.
  (Express `app.get`/`router.get` already worked; a routeless CLI like repomix
  correctly reports zero.)

### Added
- **Bash** (shell, Tier B) is now a full extraction/indexing language (the
  twenty-first), not just `find_pattern`-only — this completes **Phase 3**, the
  final phase, so **all Tier A + Tier B languages now have full extraction
  support** (only Tier C config/markup grammars stay `find_pattern`-only). A
  hand-written extractor (`src/extract/langs/bash.rs`) maps tree-sitter-bash to
  the shared entity model with a deliberately smaller surface:
  `function_definition` → Function (both the POSIX `foo() { … }` and the
  `function foo { … }` forms); `variable_assignment` (`x=1`, incl. `local`/
  `declare`/`readonly` declarations) → Variable; `export FOO[=…]` → **Export**
  (Bash has a real `export` builtin — the exported name is not also double-counted
  as a Variable); a `command` → Call, with `source X` / `. X` branching to
  **Import** (the file argument stem-matches sibling files, genuinely resolved);
  `string`/`raw_string`/`number` → Literal; and `if`/`elif`/`else`/`for`/`while`
  (the grammar reuses `while_statement` for `until`)/`case` plus the
  `return`/`break`/`continue` builtins → ControlFlow. Carve-outs (documented in
  `bash.rs`): Class/Interface/Extends/Implements (Bash has no types), Parameter
  (arguments are positional `$1`/`$2`, not named declarations), MemberAccess (only
  array subscripting `${arr[0]}`, not a named member), Throw (no exceptions) and
  Catch (`trap … ERR` registers a handler by string, not a lexical protected
  region), and Route/Response (no web DSL). `.sh`/`.bash`/`.zsh`/`.ksh`/`.bats`
  are added to the parse gate, `SUPPORTED_LANGUAGES`, and the symbol classifier
  (Bash's identifier leaf is `variable_name`, used by `$x`/`${x}` expansions; it
  has no `identifier` node).
- **Haskell** is now a full extraction/indexing language (the twentieth), not
  just `find_pattern`-only — this completes **Phase 2** (all of Scala, Dart, Lua,
  Elixir, Solidity, Haskell landed). A hand-written extractor
  (`src/extract/langs/haskell.rs`) maps tree-sitter-haskell to the shared entity
  model: `function` (a pattern clause, `f x = …`) → Function (emitted per clause,
  since a Haskell function is a set of pattern-matched equations); `bind` (a
  no-pattern value binding, incl. `let`/`where`) → Variable; `data`/`newtype`/
  `type` declarations → Class (Haskell has no OOP class — these are its
  defined-type declarations); `class` (a **typeclass**) → Interface; `instance`
  → Implements (referencing the typeclass); the module header's `export` entries
  (`module Foo (a, b) where`) → Export (a real, cleanly-exposed export list);
  `import Data.List` / `import qualified Data.Map as M` → Import (module path
  normalized to a `/`-separated spec; module-based, so it only stem-matches a
  file whose name equals the module — an accepted approximation); `apply`
  (function application) → Call, with a `qualified` head (`M.lookup`) also →
  MemberAccess; `variable` leaves under `patterns` → Parameter; `integer`/
  `float`/`string`/`char` → Literal. Haskell has no exception *syntax*, so the
  library idioms `error`/`throw`/`throwIO`/`ioError` → **Throw** and
  `catch`/`handle`/`try`/`bracket`/`finally` → **Catch** (best-effort). Haskell
  has no statement-level control flow, so the branch/binding *expressions*
  `if`/`case`/guards/`let … in` → **ControlFlow**. Type signatures (`f :: …`,
  separate `signature` nodes) are not emitted (no phantom Function duplicate).
  Extends (no inheritance), Decorator (no annotations), and Route/Response (no
  single web DSL) are carved out. `.hs` is added to the parse gate,
  `SUPPORTED_LANGUAGES`, and the symbol classifier (Haskell's identifier leaf is
  `variable`, not `identifier`).
- **Solidity** is now a full extraction/indexing language (the nineteenth), not
  just `find_pattern`-only. A hand-written extractor
  (`src/extract/langs/solidity.rs`) maps tree-sitter-solidity to the shared
  entity model, following the class-based template (Scala/Dart):
  `function_definition`/`modifier_definition`/`constructor_definition`/
  `fallback_receive_definition` → Function, `contract_declaration`/
  `library_declaration` → Class, `interface_declaration` → Interface, the `is`
  list → Extends (first) + Implements (rest), state/local variable declarations
  → Variable, `parameter` → Parameter, `call_expression`/`emit_statement` →
  Call, `member_expression` → MemberAccess, `import_directive` → Import (the
  unquoted path resolves to a sibling `.sol` file by stem). The error-check
  idioms `require(...)`/`assert(...)` and every `revert_statement`
  (`revert("msg")`, `revert CustomError(...)`) map to **Throw** (not Import,
  not a plain Call); `try/catch` → Catch/ControlFlow. Export (no `export`
  keyword) and Route/Response (no web DSL) are carved out. `.sol` is added to
  the parse gate, `SUPPORTED_LANGUAGES`, and the symbol classifier.
- **Elixir** is now a full extraction/indexing language (the eighteenth), not
  just `find_pattern`-only. A hand-written extractor
  (`src/extract/langs/elixir.rs`) maps tree-sitter-elixir to the shared entity
  model, following the call-node/dynamic template (Ruby). In tree-sitter-elixir
  almost every construct is a `call` node whose callee is the `target` field, so
  `visit` matches `kind == "call"` and dispatches on the callee identifier:
  `def`/`defp`/`defmacro` → Function (name + Parameters from the nested
  function-head call), `defmodule` → Class, `defprotocol` → Interface,
  `defimpl A, for: B` → Implements, `defstruct` → Class, module attributes
  `@x v` (`unary_operator` `@`) → Variable, `import`/`alias`/`require`/`use` →
  Import, `=` match → Variable, remote `A.b(...)`/`x.field` → MemberAccess+Call,
  local calls → Call, literals (integer/float/string/atom/boolean/nil/char/
  charlist) → Literal, `raise`/`throw` → Throw, `rescue`/`catch` blocks → Catch,
  `if`/`unless`/`case`/`cond`/`for`/`with`/`receive`/`try` → ControlFlow, and a
  narrow Phoenix router shape (`get "/p", Ctrl, :action`) → Route. Carve-outs:
  Export (Elixir has none — visibility is `def` vs `defp`), Response (no single
  idiomatic response DSL — same stance as C/Scala/Lua), and enclosing-function/
  owner-type scope-stacking (since `def`/`defmodule` share the single `call`
  kind and can't be distinguished by kind, both scope lists are empty; entities
  carry `enclosing_function = None`/`owner_type = None`). Import resolution uses
  the shared stem matcher (`alias Helper` → sibling `Helper.ex`); module-based
  imports that don't match a sibling file stem stay unresolved by design.
  `.ex`/`.exs` are parse-gated to Elixir. Joins the
  Ruby/PHP/C/C++/Scala/Dart/Lua pilots for full multi-language extraction.
- **Lua** is now a full extraction/indexing language (the seventeenth), not just
  `find_pattern`-only. A hand-written extractor (`src/extract/langs/lua.rs`) maps
  tree-sitter-lua to the shared entity model, following the procedural/dynamic
  template (Go + Ruby, no class-based path): `function_declaration`/
  `function_definition` → Function (table methods `function M.m()`/`function
  M:m()` keep the qualified name), `assignment_statement` targets (local/global/
  multiple) → Variable, `parameters` → Parameter, `function_call` → Call,
  `dot_index_expression` (`t.k`) / `method_index_expression` (`t:m`) →
  MemberAccess, string/number/true/false/nil → Literal, if/elseif/else/for/while/
  repeat/do/return/break/goto/label → ControlFlow. Lua has no throw/catch/require
  *keywords*, so these are call-idioms: `require "mod"`/`require("mod")` → Import
  (resolves to a sibling by file stem), `error(…)`/`assert(…)` → Throw, and
  `pcall`/`xpcall` → Catch. Carve-outs: Class AND Interface (Lua OOP is
  convention-based via tables/metatables, not expressible structurally — so
  Extends/Implements are unreachable too), Export (the module pattern is a bare
  `return M`, not a declaration), and Route/Response (no single idiomatic Lua web
  DSL). So `build`, `symbols_in_file`, `dependencies`/`dependents`, `hotspots`,
  complexity, churn, and clone detection now cover `.lua` files. As with the
  other non-Rust extractors, the symbol layer is best-effort. Follows the
  Ruby/PHP/C/C++/Scala/Dart pilots for
  [docs/ROADMAP-language-support.md](docs/ROADMAP-language-support.md).
- **Dart** is now a full extraction/indexing language (the sixteenth), not just
  `find_pattern`-only. A hand-written extractor (`src/extract/langs/dart.rs`)
  maps tree-sitter-dart to the shared entity model: `function_signature`/
  `constructor_signature` → Function, `class`/`abstract class` → Class, `mixin`
  → Interface, `extends X` → Extends with each `with Y` mixin and each
  `implements Z` → Implements (generic `<…>` type args stripped), fields/
  statics/`var`/`final`/`const` → Variable, params → Parameter,
  `call_expression` → Call, `member_expression` (`x.foo`) → MemberAccess,
  literals → Literal, `throw E(…)`/`rethrow` → Throw, `catch (e)`/`on T catch` →
  Catch, if/for/while/switch/return/break/continue/try → ControlFlow, and
  `@override`-style `annotation` → Decorator. Unlike most languages in the model,
  Dart has a genuine `export` directive, so `export 'x.dart'` → Export (NOT
  carved out); `import 'x.dart'` → Import with the unquoted URI, so a relative
  sibling import resolves via the shared file-stem matcher. Carve-outs:
  Route/Response (no single idiomatic Dart web DSL — same stance as C/Scala). So
  `build`, `symbols_in_file`, `dependencies`/`dependents`, `hotspots`,
  `type_hierarchy`, complexity, churn, and clone detection now cover `.dart`
  files. As with the other non-Rust extractors, the symbol layer is best-effort.
  Follows the Ruby/PHP/C/C++/Scala pilots for
  [docs/ROADMAP-language-support.md](docs/ROADMAP-language-support.md).
- **Scala** is now a full extraction/indexing language (the fifteenth), not just
  `find_pattern`-only. A hand-written extractor (`src/extract/langs/scala.rs`)
  maps tree-sitter-scala to the shared entity model: `def` → Function,
  `class`/`case class`/`object` → Class, `trait` → Interface, `extends X` →
  Extends with each `with Y` mixin → Implements (generic `[…]` type args
  stripped), `val`/`var` → Variable, params → Parameter, `call_expression` →
  Call, `field_expression` (`x.foo`) → MemberAccess, `import a.b.C` → Import
  (dotted path normalized `.` → `/`, `{…}` selectors / `._` wildcards stripped),
  literals → Literal, `throw new E` → Throw, `catch { case e: E => }` → Catch,
  and if/while/for/match/try/return → ControlFlow. Carve-outs: Export (Scala has
  no export declaration) and Route/Response (no single idiomatic web DSL — same
  stance as C). So `build`, `symbols_in_file`, `dependencies`/`dependents`,
  `hotspots`, `type_hierarchy`, complexity, churn, and clone detection now cover
  `.scala`/`.sc`/`.sbt` files. Scala imports are package-based and in general do
  NOT stem-match files, so most imports stay unresolved by design; an import
  whose trailing segment happens to match a sibling file stem resolves via the
  shared stem matcher. As with the other non-Rust extractors, the symbol layer
  is best-effort. Follows the Ruby/PHP/C/C++ pilots for
  [docs/ROADMAP-language-support.md](docs/ROADMAP-language-support.md).
- **C** and **C++** are now full extraction/indexing languages (the thirteenth
  and fourteenth), not just `find_pattern`-only. Hand-written extractors
  (`src/extract/langs/c.rs`, `src/extract/langs/cpp.rs`, C++ reusing C's
  declarator-unwrap) map tree-sitter-c/-cpp to the shared entity model:
  `function_definition` → Function (name peeled out of the nested
  `function_declarator`/`pointer_declarator` chain), `struct`/`union`/`enum`/
  `typedef` → Class (C++ adds `class` with `base_class_clause` → Extends),
  declarations/parameters → Variable/Parameter, `#include` → imports (C++ also
  maps `using`, `::` normalized → `/`), `call_expression` → Call (C++ `new` →
  Call named after the type), `field_expression` (`.`/`->`) → MemberAccess,
  and if/for/while/do/switch/case/return/break/continue/goto/`?:` →
  ControlFlow. C++ additionally maps `try`/`catch`/`throw` → ControlFlow/Catch/
  Throw. Carve-outs: both drop Interface/Export/Route/Response (no in-grammar
  construct); C additionally drops Catch/Throw (no exceptions). So `build`,
  `symbols_in_file`, `dependencies`/`dependents`, `hotspots`, `type_hierarchy`,
  complexity, churn, and clone detection now cover `.c`/`.h` (C) and
  `.cpp`/`.cc`/`.cxx`/`.hpp`/… (C++) files; `.h` is treated as C by convention.
  `#include "x.h"` resolves to sibling files via the shared stem matcher;
  angle-bracket system includes and `using` namespaces stay unresolved by
  design. As with the other non-Rust extractors, the symbol layer is
  best-effort — see the cross-cutting note in
  [docs/ROADMAP-language-support.md](docs/ROADMAP-language-support.md). Follows
  the Ruby/PHP pilots.
- **PHP** is now a full extraction/indexing language (the twelfth), not just
  `find_pattern`-only. A hand-written extractor
  (`src/extract/langs/php.rs`) maps tree-sitter-php to the shared entity
  model: `function`/`method` → functions, `class` → Class (with
  `extends` → Extends, `implements` → Implements), `interface` → Interface,
  `trait` → Class (it carries implementation), `use`/`require`/`include` →
  imports (namespaces normalized `\` → `/`), `catch`/`throw` → Catch/Throw,
  and a narrow Laravel shape (`Route::get('/x', …)` → Route, `view`/`json`/
  `response()->json`/… → Response). So `build`, `symbols_in_file`,
  `dependencies`/`dependents`, `hotspots`, `type_hierarchy`, complexity, churn,
  and clone detection now cover `.php` files. `require`/`include` resolve to
  sibling files via the shared stem matcher; Composer/PSR-4 autoload resolution
  is a future refinement. Follows the Ruby pilot for
  [docs/ROADMAP-language-support.md](docs/ROADMAP-language-support.md).
- **Ruby** is now a full extraction/indexing language (the eleventh), not just
  `find_pattern`-only. A hand-written extractor
  (`src/extract/langs/ruby.rs`) maps tree-sitter-ruby to the shared entity
  model: methods/singleton methods → functions, `class` → Class (with
  `< Base` → Extends), `module` → Interface (with `include`/`prepend`/`extend`
  → Implements), `require`/`require_relative` → imports, `rescue`/`raise` →
  Catch/Throw, and a narrow Sinatra shape (`get "/x" do … end` → Route,
  `json`/`erb`/`redirect`/… → Response). So `build`, `symbols_in_file`,
  `dependencies`/`dependents`, `hotspots`, `type_hierarchy`, complexity, churn,
  and clone detection now cover `.rb` files. `require_relative` resolves to
  sibling files via the shared stem matcher; gem/`$LOAD_PATH` resolution is a
  future refinement. This is the pilot for
  [docs/ROADMAP-language-support.md](docs/ROADMAP-language-support.md).
- `find_pattern` now runs structural search against **every** grammar
  `ast-grep-language` links (Ruby, C/C++, PHP, Scala, Solidity, Lua, HCL/Terraform,
  Bash, and more), not just the ten extraction-indexed languages. The `language`
  field accepts `ast-grep`'s own aliases (e.g. `c++`, `py`, `rb`), and directory
  walks / single-file inference resolve the full extension set. Entity/symbol
  indexing (`build`, `symbols_in_file`, resolution, etc.) is unchanged — still the
  ten languages with hand-written extractors. Bare statement/expression fragments
  are wrapped per-language so patterns like `foo($A)` parse in brace-family
  languages that require a statement terminator.
- `THIRD-PARTY-LICENSES.md`, an attribution bundle for the statically-linked
  dependencies (every bundled tree-sitter grammar included), generated from
  `Cargo.lock` by `scripts/gen-third-party-licenses.py` and shipped inside each
  release tarball to satisfy the MIT/BSD/ISC/Apache-2.0 binary-redistribution
  notices. A CI `licenses` job (`--check`) fails the build on any dependency
  license outside the permissive allow-list (no copyleft), so a future
  copyleft/unlicensed dependency is caught before release.

### Fixed
- **`scan` findings from SQL rules now carry a precise byte/line/column span**
  instead of a line-only span with zeroed byte/column. The audit flagged this as
  "C/C++ zeroed scan spans", but the root cause was language-agnostic: entity-
  anchored SQL rules (`duplicate-code-clone`, `function-complexity-hotspot`,
  `vertical-slice-sprawl`, `circular-import`) selected only `start_line AS line`,
  so `start_byte`/`end_byte`/`start_col`/`end_line`/`end_col` defaulted to 0 and
  the span collapsed to a single line — C/C++ repos merely surfaced it because
  they are dominated by SQL-rule findings (pattern rules, which already carry
  full spans, fire less there). Each rule now selects its anchor entity's full
  span (`circular-import` additionally points at the offending import statement
  rather than line 1). Whole-file rules (`file-complexity-hotspot`,
  `low-fan-in-high-fan-out-file`) keep their honest line-1 anchor — a byte range
  would be fabricated for a file-level metric. Example: a C++ clone finding went
  from `L75:0-L75:0 byte 0-0` to `L75-L83 byte 2104-2442`.
- **`nav_map` entrypoints now include language process mains.** Detection covered
  web-framework handlers/routes but excluded the `main`/`Main` a program actually
  starts at (they were filtered as bootstrap). A new `detect_process_mains` pass
  surfaces them as a distinct `process_main` role — Rust/Go/C/C++/Java/Kotlin
  `main`, C# `Main` — matched structurally by name **and** language (a C# helper
  named `main` or a Rust method named `Main` is not mistaken for one), with
  test/generated/scaffold paths excluded. Process mains are flow roots, so they
  also seed the flow trees (a CLI/binary's call graph is now navigable from its
  entry). Example: c-inih went from 0 entrypoints to its 6 example/tool `main`s.
- **Cross-module import resolution now follows re-exports and package/module
  boundaries** (`dependents`/`blast_radius`/dependency graph). The audit found the
  resolver only stem-matched an import's trailing segment against file basenames,
  so three whole idioms went unresolved and left the graph disconnected:
  (1) **package/directory imports** — Python `from pkg import X`, a bare
  directory `index.*` import, or Lua's `require "pkg"` — now resolve to the
  package's index file (`__init__.py`/`index.ts`/`init.lua`) via a new
  package-directory index; (2) **multi-segment module paths** — `from a.b.c
  import X` (and `use crate::a::b`) — now match on the *full trailing path
  suffix* (`a/b/c` → the one `.../a/b/c.py`) instead of colliding on the bare
  `c` stem across every same-named file; and (3) **namespaced module aliases** —
  Elixir `alias/import/use Foo.Bar` — now resolve to the file that declares
  `defmodule Foo.Bar` via a module-declaration index, independent of Elixir's
  snake_case file naming; and (4) **Go package imports** — a Go import names a
  *directory* of `.go` files, not a single file, so it is resolved through
  `go.mod`'s `module` path (mapping the import-path prefix to the package
  directory) and emits one import edge per file in the target package
  (`_test.go` files excluded — they are not part of the importable surface).
  Lua's dotted `require "a.b.c"` module paths are also normalized to slash form
  at extraction so they stop being truncated as a file extension. Effect
  (verified end-to-end): a Python package's re-exported leaf
  (`crewai/agent/core.py`) went from **0 → 641** transitive dependents once the
  `consumer → __init__.py → core.py` chain connects; Elixir
  (`Phoenix.Controller` 20, `Phoenix.Router` 12), Lua (`busted/init.lua` 16,
  `busted/block.lua` 18), and Go (go-cobra root-package files now report their 11
  `doc/`+external-test dependents, was 0) dependency edges, previously near-empty,
  now resolve. All resolution stays deterministic and single-match-only for the
  file-targeted cases (ambiguous specifiers stay unresolved — no invented edges);
  Go resolves via the module-path prefix so a stdlib import (`fmt`, `os`) never
  mis-matches a local file.
- **`type_hierarchy` now returns the real inheritance graph** instead of the
  lexical enclosing-scope chain. A cross-language accuracy audit found the mode
  walked `enclosing_function` containment (so a top-level type reported only
  `[self]`, and a bare name could resolve to a call-site or a local variable —
  even one in a different language, e.g. Rust `Shape` matching a Haskell
  `Shape`). It now (1) resolves the seed to an actual class/interface
  *declaration* preferentially, scoped to the seed's language; (2) walks the
  `Extends`/`Implements` edge set both ways, returning `supertypes` (transitive
  parents) and `subtypes` (transitive implementers/subclasses); and (3) keeps a
  backward-compatible `hierarchy` field, now the inheritance ancestry
  (parent-first, ending in the seed). E.g. `IChatCompletionService` returns 18
  implementers (was 0), Java `Owner` returns `Person → BaseEntity → Serializable`
  (was self only), and Python `BaseLLM` returns 24 subclasses (was 1).
- **`fat-interface` / `solid-lsp` / `solid-isp` no longer inflate method counts
  by cross-file name collision.** The member-count subqueries matched every
  method whose `owner_type` *name* equalled the type's name across the whole
  repo, so N sample files each defining a `class MenuPlugin` summed into one
  bogus count (a 2-method class reported as "declares 102 methods" — a 122/122
  false-positive rate on `semantic-kernel`). `fat-interface` now counts only
  methods nested in the declaration's own file+byte span; the SOLID rules scope
  their class-side counts to the implementing type's file. On `semantic-kernel`
  `fat-interface` drops from 122 findings to 82, all with accurate per-declaration
  counts. Covered by a new regression test.
- **C/C++ no longer emit reserved keywords as entities or reference symbols.**
  Under preprocessor confusion or tree-sitter error recovery, `if (...)` was
  captured as a `function`/`call` named `if`, and `class`/`template`/`typename`/
  `struct`/`const` flooded the reference list (663 keyword symbols in one fmt
  header). A reserved word can never be a valid identifier, so it is now dropped
  at the C/C++ push sites, in the symbol classifier, and via a language-scoped
  backstop in `ExtractCtx::push`. Scoped enums (`enum class Color`) still keep
  their real type name. Covered by new regression tests.
- **`get_symbol` prefers definitions over weaker same-named matches.** Candidate
  ordering now ranks a type/function declaration above a variable/parameter
  binding, and a `Binding` above a `Reference`, so a bare lookup returns the
  declaration rather than a use of it.
- **Blank-name entities and symbols are no longer emitted.** Unnamed constructs
  and error recovery (Ruby `class << self`, Haskell type operators like `:>`, JS
  `export default function(){}`, Lua table-literal method closures) produced
  `name:""` rows that could never match a lookup, flooded `symbols_in_file`, and
  inflated scan counts (22 blank symbols in one Ruby file, 25 in one Haskell
  file, all-anonymous Lua object methods). A single post-extraction filter drops
  every blank-named declaration/reference row; control-flow and error markers
  (`ControlFlow`/`Catch`/`Throw`) are exempt so a bare `rescue` or re-`raise`
  stays queryable. Verified on ruby-sinatra/haskell-servant/lua-busted (blank
  counts 22/25/n → 0/0/0); covered by new regression tests.
- **`context_pack` handles multi-word queries.** A `query` like `"router
  controller"` was matched as one literal phrase against paths and symbol names;
  since no path/name contains the phrase verbatim it returned `not_found`. The
  query is now split into whitespace-delimited keywords, each seeded
  independently and unioned (matches deduped). Single-word queries are
  unchanged. Verified on elixir-phoenix (`"router controller"`: not_found → 49
  files / 5 symbols); covered by a new parity check.
- **Declaration coverage greatly expanded — many constructs were extracted but
  never surfaced, or never captured at all.** Two root causes:
  - *Persistence dropped `Variable`/`Parameter`.* The persistence layer dropped
    those kinds on the assumption nothing read them back, but `declaration_kinds`
    (behind `symbols_in_file`/`get_symbol`/`filter_symbols`) lists both — so every
    field, property, constant, and parameter was invisible to exactly the queries
    meant to return them. They are now persisted (DB size effectively unchanged;
    the dropped bulk is `Literal`/`MemberAccess`). This alone restored Java
    `@Column` fields, PHP typed properties, TS module `const`s, and C# properties.
  - *Extractors dropped whole declaration forms.* Added capture, mapped to the
    closest existing kind so they are queryable without a schema change:
    TS `type` aliases (→interface) and `enum` (→class); C# `record`/`struct`/
    `enum` types (→class) and properties (→variable); Go named func/defined/alias
    types (→class); Kotlin `object`/`companion object` (→class); Ruby
    `attr_accessor`/`reader`/`writer` (→variable, owned by the class); Solidity
    `event`/`error` (→function); C `#define` macros (object-like→variable,
    function-like→function) and header function prototypes (→function); Dart
    factory constructors and getters/setters (→function); Scala `given`
    (→variable) and `type` members (→interface); Haskell `type family`
    (→interface). Members carry their owning type via `owner_type`. Verified
    end-to-end (extract→build→query) on the audit repos; covered by per-language
    regression tests.
- **`scan` no longer reports findings on generated/vendored files.** Bundled/
  minified JS assets, committed tree-sitter `parser.c`, and dependency trees were
  linted like source, so complexity/clone/lint rules fired on code the user can't
  fix — 87% of one repo's findings (1359/1958) cited `priv/static/phoenix.*.js`
  bundles, and 25 complexity findings cited a generated `grammars/*/parser.c`.
  `noise_filter::is_generated_or_vendored_path` now also recognizes `vendor`/
  `third_party`/`bower_components`/`.next`/`.nuxt`/`grammars` directories, the
  `priv/static/` compiled-asset tree, and `*.min.js`/`*.bundle.js` filenames; the
  scan engine drops findings on such files after both rule engines merge (so it
  covers pattern AND SQL rules). Verified on elixir-phoenix (1958 → 599 findings,
  0 on generated files; the 1359 dropped are logged, not silent). The broadened
  detection also de-noises the nav `hotspots`/`foundational_files` sections, which
  share this predicate.

## [0.1.0] - 2026-09-03

Initial standalone port of varde-code: multi-language parsing (tree-sitter), entity/symbol
extraction and resolution, a SQLite-backed query engine, and a pattern-matching scan/rules engine,
exposed via the `varde-code` CLI. See [README.md](README.md) for scope and
[docs/CLI.md](docs/CLI.md) / [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

### Added
- MIT `LICENSE` file.
- CI workflow: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`
  on push/PR to `main`, plus a macOS/Linux test matrix, an MSRV (`1.88.0`) build gate, a
  `cargo audit` dependency-vulnerability check, and a `cargo fmt --check` formatting gate.
- Release workflow publishes tagged (`v*`) builds as GitHub Release assets (macOS arm64/x86_64,
  Linux x86_64/arm64), not just workflow artifacts.
- README install instructions (prebuilt binaries or `cargo install --path crates/varde-code`).
- The per-repo advisory lock now guards the in-place incremental build as well (previously only
  query-triggered freshens took it), so a concurrent `build` and query on the same repo serialize
  instead of interleaving partial writes.

### Changed
- README status bumped from Pre-alpha to Beta.
- `Cargo.lock` is now tracked in git, as is standard for a binary crate.
- The incremental/freshen write path opens with a `MEMORY` rollback journal so a mid-write error
  rolls back cleanly instead of splicing a half-applied delta into the live index; file deletion
  and derived-ledger invalidation now commit as a single transaction.

### Fixed
- Flaky `rules::test_runner::sql_tests` failures under concurrent `cargo test --workspace` runs,
  caused by a fixture build reading the process-global `HOME` env var without holding the lock
  other tests use when redirecting it.
- A `#[should_panic]` persistence test relied on a `debug_assert!`, so it failed under
  `cargo test --release`; it is now gated on `debug_assertions`.

### Removed
- Stale root-level investigation/plan docs (`BUILD_PERSISTENCE_INVESTIGATION.md`,
  `PERFORMANCE_AUDIT.md`, `SLICED_FRESHNESS_PLAN.md`, `WARM_PATH_FRESHNESS_INVESTIGATION.md`).
</content>
</invoke>
