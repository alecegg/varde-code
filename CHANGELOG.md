# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
