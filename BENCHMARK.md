# Benchmark: varde-code vs. ast-grep

Tracks speed/quality for every varde-code query mode that overlaps
functionality `ast-grep` already provides. Updated whenever a covered mode
changes or a new overlapping mode ships. This file holds a single canonical
set of current measurements — no optimization history or narrative. Past
optimization write-ups live in git history if needed.

## Goal

**At least within 10% of ast-grep's runtime, per tool, per repo**, for every
overlapping capability — and where varde-code's persisted architecture allows,
**beat** it. This replaces the prior "2x budget" target.

`find_pattern` now *beats* ast-grep on every tested row (see Results) via a
**literal-atom prefilter**: it never parses a file that provably cannot
match. That is the general lever a persist-and-serve tool has over a
live-parse tool — avoid work, don't just do the same work faster. Racing a
hand-rolled matcher to *parity* with ast-grep on a full parse was the wrong
framing; the win came from parsing fewer files, not from a faster matcher.

The `scan` pattern-rule pipeline is the remaining overlapping mode still
slower than ast-grep (1.56x). Because it matches a single target (not a tree),
the prefilter's skip-files win does not apply; its options are matcher-level
(adopt `ast_grep_core`'s own `Pattern`, which varde-code already links, in
place of the hand-rolled matcher) — see Open work.

Feature adjustment or removal is on the table if a feature's complexity/cost
turns out not to be worth it, but this is a last resort — the default is to
hit the target without cutting scope.

The existing cold/warm scale-build targets (`hermes-agent` cold < 3s, warm <
250ms; see "Scale build targets" below) remain in effect alongside this goal,
not replaced by it.

## Results (2026-09-03, release build, ast-grep 0.45.1)

Min-of-3 for warm/find_pattern rows; cold rows are single-run (cache cleared
via `rm -rf ~/.config/varde-code/repos/*<repo>*` before each). Full repro
commands in each mode's section below.

> **`find_pattern` refreshed 2026-09-03** (min-of-5, ast-grep 0.45.1). Two
> changes, both preserving 100% match parity on every row: (1) the directory
> search now uses `ignore`'s parallel walker (`WalkParallel`), overlapping the
> walk with per-file parse; (2) a **literal-atom prefilter** skips parsing any
> file that lacks one of the pattern's mandatory literal tokens (e.g.
> `console` *and* `log` for `console.log($MSG)`) — conservative by
> construction (the matcher requires exact-text leaves, so a missing atom
> means the file provably cannot match), which on these patterns skips
> **80–99%** of files from tree-sitter. Net result: **`find_pattern` now
> beats ast-grep on every tested row** (ratio 0.22–0.97x), because a
> persist-and-serve tool need not parse files that cannot match. Still
> index-free — the prefilter reads each file to substring-check it; a
> persisted `token→files` index would eliminate those reads too (see Open
> work). Cold (first-touch) not re-measured (not reliably reproducible
> mid-session; see note below the table).

| Tool | Repo | varde cold | varde warm | ast-grep | Ratio (warm) | Beats ast-grep? |
|---|---|---|---|---|---|---|
| `find_pattern` (TS `console.log($MSG)`) | hermes-agent | 1.280s | 0.075s | 0.130s | 0.58x | Yes ✓ |
| `find_pattern` (TS `console.log($MSG)`) | oh-my-pi | 1.251s | 0.117s | 0.522s | 0.22x | Yes ✓ |
| `find_pattern` (TS `console.log($MSG)`) | repowise | 0.156s | 0.032s | 0.042s | 0.76x | Yes ✓ |
| `find_pattern` (Python `print($MSG)`) | hermes-agent | 1.325s | 0.588s | 0.787s | 0.75x | Yes ✓ |
| `find_pattern` (Python `print($MSG)`) | oh-my-pi | 0.146s | 0.078s | 0.081s | 0.97x | ~tie |
| `find_pattern` (Python `print($MSG)`) | repowise | 0.394s | 0.084s | 0.236s | 0.36x | Yes ✓ |
| `scan` pattern-rule pipeline (`Ok($VALUE)`, Rust) | varde-code crate | — | 0.039s | 0.025s | 1.56x | No (1.56x slower) |
| `build` (index) | hermes-agent | 6.70s | 0.04s | n/a | n/a | n/a |
| `build` (index) | oh-my-pi | 3.72s | 0.03s | n/a | n/a | n/a |
| `build` (index) | repowise | 1.31s | 0.02s | n/a | n/a | n/a |
| `extract` (parse-cost proxy) | varde-code crate | — | 0.277s | n/a | n/a | n/a (no ast-grep equivalent) |

`find_pattern` has no persisted index, so its "cold" column is first-touch
(no OS page-cache warm-up for that repo this session) rather than a cleared
SQLite cache.

**`find_pattern` now beats ast-grep on every tested row** (0.22–0.97x) via
the literal-atom prefilter above — the goal for this mode is exceeded, not
just met. The `scan` pattern-rule pipeline still runs 1.56x slower: it reuses
the same matcher but targets a single file/crate, so the prefilter (which
wins by *skipping* files across a tree) does not help its single-target
timing — closing that gap is a separate matcher-level question (see Open
work).

**Skipped this round:** the `throw new Error($MSG)` / varde/src `find_pattern`
row, the `symbols_in_file` vs. `ast-grep outline` row, and the `extract`
target-file row all depend on `~/source/varde` being checked out locally,
which it is not in this environment — no numbers for these until that repo
is available again.

## Other query modes (baseline, no ast-grep equivalent)

The 18 modes below have no `ast-grep` counterpart at all (symbol lookup,
dependency graphs, hotspots, clustering — `ast-grep` doesn't do any of
these), so they are not judged against the 10% goal. These are absolute
cold/warm timing baselines captured so future perf work on these modes has
something to diff against, not a comparison to any other tool. Cold = single
run immediately after `rm -rf ~/.config/varde-code/repos/*<repo>*` (first
call rebuilds whichever slice(s) that mode reads, per the `FRESHNESS` table
in `crates/varde-code/src/query/mod.rs`); warm = min-of-3 subsequent calls.
Reproduce with `.claude/skills/benchmark/scripts/other_modes.sh <repo-root>
<file-path> <symbol-name>`.

Representative inputs: `hermes_bootstrap.py` / `main` (hermes-agent),
`scripts/host-detect.ts` / `main` (oh-my-pi), `scripts/validate_quality.py`
/ `main` (repowise) — `main` resolves to at least one symbol row in all
three repos (kind doesn't matter for these modes, just that `name` matches;
`context_pack` reuses it as the keyword query).

`symbols_in_files`, `clusters`, `detect_changes`, `context_pack`, and
`nav_map` were added to this table and to `other_modes.sh` this round —
they existed in the codebase but had no benchmark coverage previously.

Measured 2026-09-03, release build:

| Mode | Repo | Cold (ms) | Warm (ms) |
|---|---|---|---|
| `get_symbol` | hermes-agent | 13043 | 73 |
| `get_symbol` | oh-my-pi | 7960 | 60 |
| `get_symbol` | repowise | 1688 | 47 |
| `filter_symbols` | hermes-agent | 11841 | 21 |
| `filter_symbols` | oh-my-pi | 7532 | 21 |
| `filter_symbols` | repowise | 1579 | 22 |
| `hotspots` | hermes-agent | 11190 | 79 |
| `hotspots` | oh-my-pi | 7857 | 66 |
| `hotspots` | repowise | 1620 | 50 |
| `tests_for_file` | hermes-agent | 11307 | 84 |
| `tests_for_file` | oh-my-pi | 7885 | 71 |
| `tests_for_file` | repowise | 1700 | 54 |
| `find_imports` | hermes-agent | 12049 | 74 |
| `find_imports` | oh-my-pi | 7640 | 61 |
| `find_imports` | repowise | 1669 | 49 |
| `dependencies` | hermes-agent | 11431 | 74 |
| `dependencies` | oh-my-pi | 7809 | 63 |
| `dependencies` | repowise | 1651 | 48 |
| `dependents` | hermes-agent | 11781 | 77 |
| `dependents` | oh-my-pi | 7827 | 63 |
| `dependents` | repowise | 1676 | 50 |
| `blast_radius` | hermes-agent | 12490 | 77 |
| `blast_radius` | oh-my-pi | 7822 | 63 |
| `blast_radius` | repowise | 1613 | 49 |
| `symbol_blast_radius` | hermes-agent | 12764 | 75 |
| `symbol_blast_radius` | oh-my-pi | 7629 | 62 |
| `symbol_blast_radius` | repowise | 1617 | 50 |
| `type_hierarchy` | hermes-agent | 11404 | 71 |
| `type_hierarchy` | oh-my-pi | 7771 | 59 |
| `type_hierarchy` | repowise | 1664 | 48 |
| `explore` | hermes-agent | 11791 | 77 |
| `explore` | oh-my-pi | 7895 | 63 |
| `explore` | repowise | 1703 | 49 |
| `map_symbol` | hermes-agent | 11904 | 73 |
| `map_symbol` | oh-my-pi | 7905 | 60 |
| `map_symbol` | repowise | 1644 | 47 |
| `map_path` | hermes-agent | 11487 | 79 |
| `map_path` | oh-my-pi | 7578 | 64 |
| `map_path` | repowise | 1664 | 50 |
| `map_file` | hermes-agent | 11662 | 74 |
| `map_file` | oh-my-pi | 7855 | 61 |
| `map_file` | repowise | 1617 | 48 |
| `symbols_in_files` | hermes-agent | 14593 | 22 |
| `symbols_in_files` | oh-my-pi | 10022 | 26 |
| `symbols_in_files` | repowise | 1980 | 24 |
| `clusters` | hermes-agent | 13659 | 102 |
| `clusters` | oh-my-pi | 11079 | 95 |
| `clusters` | repowise | 1949 | 64 |
| `detect_changes` | hermes-agent | 13069 | 36 |
| `detect_changes` | oh-my-pi | 9713 | 40 |
| `detect_changes` | repowise | 1892 | 34 |
| `context_pack` | hermes-agent | 16847 | 2966 |
| `context_pack` | oh-my-pi | 10838 | 1770 |
| `context_pack` | repowise | 2079 | 214 |
| `nav_map` | hermes-agent | 22489 | 10271 |
| `nav_map` | oh-my-pi | 14132 | 3431 |
| `nav_map` | repowise | 4727 | 1189 |

No modes were skipped — all 18 resolved cleanly against the chosen
representative inputs on all 3 repos.

Cold numbers track repo scale directly (hermes-agent 6,078 files > oh-my-pi
4,869 > repowise 3,111) and land in the same ~1.6-13s band regardless of
mode, since cold cost is dominated by the one-time slice build (raw/churn/
imports/edges/global), not the query logic on top of it. Warm numbers show
a per-mode split: `filter_symbols` is the cheapest warm query across all
three repos (21-22ms, a single indexed lookup); most of the rest cluster in
a 47-84ms band.

`symbols_in_files`, `clusters`, and `detect_changes` fit the same fast-warm
pattern as the other 13 modes. `context_pack` and `nav_map` do not: both
recompute their result on every call rather than serving from a cache, so
their "warm" numbers are not a steady-state query cost the way the other 16
modes' are — `context_pack`'s cost tracks query fan-out (0.2-3s warm,
scaling with repo size), and `nav_map` recomputes a full repo orientation
map per call every time (1.2-3.4s warm on oh-my-pi/repowise, 10.3s on
hermes-agent). `nav_map` previously showed a much sharper, disproportionate
jump on hermes-agent (63s warm, 7-15x the other two repos) traced to a
missing SQLite index: `resolved_edges` had indexes on `from_file_id`/
`to_file_id` but not `from_entity_id`/`to_entity_id`, so the per-entity
call-graph lookups `flows::callees` (once per DFS node) and
`entrypoints::is_bootstrap_by_fan_asymmetry` (once per candidate entity) ran
as full table scans — quadratic in entity/edge count, and hermes-agent has
disproportionately more of both relative to its file count than the other
two repos. Fixed in `crates/varde-code/src/db.rs`
(`idx_resolved_edges_from_entity`/`idx_resolved_edges_to_entity`,
`SCHEMA_VERSION` 6→7) plus statement caching in `flows.rs`/`entrypoints.rs`;
hermes-agent's `nav_map` warm time dropped 63.1s → 10.3s (6.1x). `nav_map`
remains the most expensive mode in this table (it still recomputes fully
every call, unlike the other 16 modes) but no longer scales
disproportionately with repo size.

## Query-path freshness (`ensure_fresh` no-op cost)

Every repo-scope query pays an `ensure_fresh` no-op check before answering
(the sliced-freshness plan's build-on-read layer). These costs ride on top
of every repo-scope query's warm time in the Results table above.

Measured 2026-09-03, release build; 201-file fixture via
`cargo run --release -p varde-code --example git_perf` (min-of-3 internally,
20 calls per path, reported as average ms/call):

| changed_files path | 201-file fixture (per no-op call) |
|---|---|
| default walk | 12.76 ms (20 calls) |
| opt-in git-status (`VARDE_GIT_FAST_PATH=1`) | 14.41 ms (20 calls) |

At 201 files the two paths are statistically indistinguishable — both sit
under the machine's noise floor (subprocess spawn vs walk jitter). The
git-status fast path is **opt-in** (`VARDE_GIT_FAST_PATH`) with the walk the
default (commit `53003c7`) since it only wins where git spawns are cheap and
the walk dominates (very large repos).

## Scale build targets

Large-repo `build` scaling checks use these three repos from
`~/source/reference-repos`:

| Repo | Source files (ts/tsx/js/jsx/py/rs/go) |
|---|---|
| `hermes-agent` | 6,078 |
| `oh-my-pi` | 4,869 |
| `repowise` | 3,111 |

**Targets:** `hermes-agent` cold build **< 3s**, warm (incremental) build
**< 250ms**. `oh-my-pi` and `repowise` are secondary regression checks
(proportionally faster).

**Current (2026-09-03, streaming build + first three fixes):**

| Repo | Cold (real) | Warm (real) | vs. cold target | vs. warm target |
|---|---|---|---|---|
| `hermes-agent` | 4.41s | 0.065s | 1.5x over | **met** |
| `oh-my-pi` | 3.03s | 0.052s | — | — |
| `repowise` | 1.22s | 0.040s | — | — |

> These absolute cold numbers are the last **clean-session** baseline. A
> 2026-09-03 re-run (fixes four and five below) could not re-baseline them
> reliably: sustained compile+benchmark load left the machine thermally
> throttling, and the *same* HEAD binary varied 5.99s→6.96s cold on
> `hermes-agent` run-to-run — wider than the fixes' own effect. The two newest
> fixes' impact is therefore reported from controlled small-repo runs and a
> same-session A/B, not re-stated as new absolutes here.

Warm consistently clears the < 250ms target. Cold has come down across five
fixes, all found by profiling individual `build` phases with `VARDE_PROFILE=1`
(see the `profile` skill). The first two were quadratic-work / allocation bugs
(`hermes-agent` 12.37s → 6.70s across them). The third overlaps parsing with
persistence for a further ~12–14% (`hermes-agent` 5.09s → 4.41s, `oh-my-pi`
3.53s → 3.03s, `repowise` 1.39s → 1.22s — same-session batch vs streaming,
min-of-3 cold). The fourth and fifth (2026-09-03) trim per-node walk cost and
the post-commit cache rebuild:

- **`resolve_type_hierarchy`'s `from_entity` lookup** was an O(entities)
  linear scan (`entities.iter().position(...)`) per `Extends`/`Implements`
  entity, effectively O(N×M) over the whole repo. Replaced with a
  precomputed `(file_id, name) -> entity index` `HashMap`, dropping
  `type_hierarchy_resolution` itself by 28-36x (6.34s→221ms on hermes-agent,
  4.18s→117ms on oh-my-pi).
- **`scan`'s sequential merge loop** (`output.entities.extend(...)`) grew an
  unreserved `Vec` by doubling as ~8,700 files' worth of entities/symbols
  merged in — at repo scale the last few doublings each copied a
  multi-million-element vector. Added `reserve_exact` up front from
  per-file counts already known after the parallel parse step, cutting the
  merge step from ~486ms to ~310ms on hermes-agent.
- **Overlapping parse with persistence** (the streaming full build,
  `persist::persist_full_streaming`). The full build ran as two sequential
  phases — parse+extract the whole repo, then insert every row. It now
  pipelines them: a parser thread `parse_chunk`s the file list in order while
  the main thread inserts the previous chunk's rows through the exact same
  `write_*` primitives the incremental delta path uses (so the two paths never
  diverge in how a row is written). Parsing is the long pole (~⅔ of build; see
  the CPU profile below), so the raw-row insert cost now hides behind it —
  ~12–14% cold-build speedup, byte-identical index (asserted by the
  `streaming_matches_batch_build` test), warm/incremental path unchanged.
- **Per-node `node.kind()` dedup** (`extract::walk`, threaded through all 11
  language visitors). The extract walk is the largest single slice of scan CPU
  — 55–63% of parse+extract, measured directly with the new
  `examples/parse_vs_walk` profiler (single-threaded parse vs. parse+walk on
  gson/ripgrep/zod/axios). `node.kind()` is an FFI call returning a `Cow<str>`,
  and the walk consulted it ~5× per node (identifier check, function-scope
  test, type-scope test, type-kind test, plus each visitor's own top-level
  `match node.kind()`). It is now materialized once per node and threaded as a
  borrowed `&str` into `langs::visit` and every visitor, so each node resolves
  its kind exactly once. Pure call-dedup — byte-identical output; walk time
  −10–13% (gson −10%, ripgrep −12%, zod −13%; controlled min-of-many). Diluted
  at the build level because parse+extract is already rayon-parallel and
  overlapped with inserts (see the streaming bullet).
- **`graph_cache` warmed in-transaction** (`persist::persist_full_streaming`).
  `run_full` used to reopen the just-committed DB and run a full from-SQL
  `Graph::load_uncached` rebuild to warm `graph_cache` — re-deriving the exact
  adjacency the streaming resolve had already produced, against a cold page
  cache (~15–17% of a cold build, the largest untouched post-parse phase). The
  warm (and the built-at git HEAD record) now happen inside the build
  transaction from the warm just-written `files`/`resolved_edges` rows, and the
  post-commit reopen is gone. Still routed through
  `rebuild_graph_cache_full`/`load_uncached`, so the full and incremental builds
  warm the cache the same way (no divergence) and the blob stays byte-identical
  to a from-SQL rebuild — the invariant the
  `deleting_non_max_rev_file_refreshes_graph_cache` test asserts. Only the
  reopen + cold-scan overhead is eliminable (the SQL scan + bincode encode is
  inherent work that *moved* rather than vanished): −3–5% cold on medium repos
  (zod −5.2%, gson −3.1%; controlled warmed min-of-6).

**Investigated and ruled out** (each tested empirically, not just reasoned
about, then reverted since none measured a win):
- `cache_size`/`temp_store=MEMORY` pragmas on the theory that SQLite's
  default ~2MB cache forces `CREATE INDEX`'s sort to spill to disk at
  multi-million-row scale — made index build *slower* (~1.5s → ~1.9-2s).
- `INSERT_BATCH` 500 → 2000 (fewer, larger multi-row `INSERT` statements) —
  flat, within run-to-run noise.
- Normalizing `entities`/`symbols.name` into a separate `strings` table
  with an integer FK (dedup + smaller/faster-sorting indexes) — real
  possible win on paper, but ruled out on cost/benefit: only 2 of the 8
  indexes built are name indexes, so the bound on the gain is a few hundred
  ms at most, while nearly every one of the 19 query modes filters on
  `name` and would need its SQL rewritten to resolve name → name_id first —
  disproportionate blast radius for the likely payoff.

**Where the remaining time goes** (`samply` CPU profile of a hermes-agent
cold build, leaf-frame self-time, own-code binary only):

| category | share |
|---|---|
| tree-sitter (parsing) | 67.1% |
| other/libc/system | 15.4% |
| varde-code own code | 12.7% |
| sqlite/rusqlite | 4.8% |

Two-thirds of all sampled time is inside tree-sitter's C parser — the
actual cost of lexing/parsing source into a syntax tree, already fully
parallelized across every core via `rayon`. The 12.7% that's varde-code's
own code has no single dominant hotspot: `core::str::from_utf8` (5.6%, one
necessary UTF-8 validation pass per file), `extract::entity::entity` (4.5%,
per-`Function` entity construction including the MinHash body signature),
and small (~1% each) `persist` write functions. No further O(N×M)-style bug
or missing-reservation pattern found.

**Verdict: with parsing and persistence now overlapped, the remaining
cold-build cost is dominated by tree-sitter parsing itself.** The first two
fixes were genuine bugs (unintentional quadratic work); the streaming build
was a structural win — running two phases concurrently instead of back to
back — rather than tuning. Past that, the cost is inherent to the features as
specified: tree-sitter parsing (67% of samples, already fully parallel),
per-function clone-detection signatures, per-file UTF-8 validation, SQLite
persistence. Because the raw-row insert now hides behind parsing, persistence
has largely left the critical path; closing the remaining ~1.5x gap to the
< 3s target would mean cutting a shipped feature's parse/extract cost (e.g.
skip/lazy MinHash, swap the parser), not tuning existing code — a different
kind of decision than this section's scope.

Repro:

```
export PATH="$HOME/.cargo/bin:$PATH"
BIN=target/release/varde-code
for repo in hermes-agent oh-my-pi repowise; do
  rm -rf ~/.config/varde-code/repos/*"$repo"*
  /usr/bin/time -p "$BIN" build --repo-root "/Users/alec/source/reference-repos/$repo"
  /usr/bin/time -p "$BIN" build --repo-root "/Users/alec/source/reference-repos/$repo"  # warm, no --force
done
```

No ast-grep equivalent exists for `build` at this scale (ast-grep has no
persisted-index concept) — this is a scaling/regression check against
varde-code's own baseline, not an ast-grep comparison.

## Methodology

- **Same version baseline.** ast-grep installed via `brew install ast-grep`
  (0.45.1) for real-CLI comparisons; varde-code built with `cargo build
  --release` (release binary only — debug builds inflate tree-sitter parse
  cost ~4x and are not representative).
- **Same target, same query** for both tools; `find_pattern` matches
  ast-grep's default "smart" strictness (trivia-skipping), so no pattern
  translation is needed.
- **Cold vs. warm, separated**, never averaged. Cold = first run, persisted
  index/cache cleared first. Warm = repeat 3x, report the stable (min)
  number.
- **Quality, not just speed.** Match/entity/symbol counts are checked for
  parity on every row; any delta is investigated before treated as noise.
- **Reproduce, don't eyeball.** Every row is re-runnable from the commands in
  its section.

## Overlap inventory

`ast-grep`'s full command surface: `run` (search/rewrite), `scan` (rule-based
lint/rewrite), `test`, `new`, `lsp`, `outline` (symbol/import/export
structure), `completions`.

| ast-grep command | Overlaps varde-code mode | In scope? |
|---|---|---|
| `run -p <pattern>` | `find_pattern` | Yes |
| `outline` | `symbols_in_file`, `symbols_in_files`, `filter_symbols`, `find_imports`, `build` | Yes |
| (parsing, cost proxy) | `extract` | Yes, imperfect proxy |
| `run --rewrite` | none — `find_pattern` is match-only | No |
| `scan` (YAML lint) | none — no rule/lint engine | No |
| `test`, `new`, `lsp`, `completions` | none | No |

Modes with no ast-grep equivalent at all (graph/persistence-backed):
`dependencies`, `dependents`, `tests_for_file`, `hotspots`, `map_file`,
`map_symbol`, `map_path`, `explore`, `blast_radius`, `symbol_blast_radius`,
`detect_changes`, `type_hierarchy`, `get_symbol`, `clusters`, `context_pack`,
`nav_map`. Out of scope for the 10% goal (no equivalent to compare against).
`symbols_in_files` has an `outline` equivalent in spirit (batch variant of
`symbols_in_file`) but isn't separately benchmarked against it — see
"Other query modes" above for its baseline instead.

## `extract`

Not a strict ast-grep overlap — closest proxy is ast-grep's parse cost
(`extract` = parse + tree-sitter walk + entity/symbol construction). No
directly comparable `ast-grep` number; tracked as a parse-cost proxy only.

**Target:** `~/source/varde/src` (474 files) when available; falls back to
the `varde-code` crate's own source otherwise.

```
varde-code extract <path>   # release build
```

## `find_pattern`

**Patterns:** `console.log($MSG)` (TS, 3 scale repos), `print($MSG)`
(Python, 3 scale repos).

```
ast-grep run -p '<pattern>' -l <lang> <path> --json=compact
varde-code find_pattern --json '{"path":"<path>","pattern":"<pattern>","language":"<lang>"}'
```

Match counts are at parity (100%) across all tested repos/patterns. As of
2026-09-03, `find_pattern` **beats** ast-grep on every tested row (see
Results). Two mechanisms, both output-identical:

1. **Parallel walk** (`ignore::WalkParallel`) — overlaps directory traversal
   with per-file parse/match instead of the old collect-all-paths → sort →
   parallel-match pipeline, which serialized the whole walk as a barrier
   before the first parse.
2. **Literal-atom prefilter** (`mandatory_atoms` in
   `crates/varde-code/src/query/find_pattern.rs`) — extracts the literal
   identifier/keyword/number tokens the pattern requires verbatim (e.g.
   `console`, `log` for `console.log($MSG)`; empty for a purely meta-var
   pattern like `$A = $B`), then skips parsing any file whose bytes lack one.
   Conservative by construction: the matcher requires exact-text leaves, so a
   file missing an atom cannot match — the filter only ever *over*-selects
   (wastes a parse), never drops a real match. On the tested patterns it skips
   80–99% of files from tree-sitter, which is the entire win: a persisted tool
   does not parse files that cannot match. Still index-free — the prefilter
   reads each file to substring-check it. Correctness is covered by
   `prefilter_changes_timing_not_results`, `atomless_pattern_still_searches_whole_tree`,
   and the `mandatory_atoms_*` unit tests.

## `symbols_in_file` / `filter_symbols` / `find_imports` vs. `ast-grep outline`

Architecturally different: these modes query a persisted SQLite index;
`ast-grep outline` does a single-shot live parse with no index. Two distinct
comparisons apply — warm/query-only (steady-state cost of asking twice) and
cold/end-to-end (build-index-then-query vs. one-shot parse). Only
warm/query-only is a fair apples-to-apples comparison; cold/end-to-end
compares a persisted index build against a one-shot parse and is tracked
separately rather than judged against the 10% goal.

```
ast-grep outline <file>
varde-code symbols_in_file --json '{"repoRoot":"...","filePath":"<file>"}'
```

## `scan` pattern-rule pipeline

```
cargo test --release -p varde-code --test pattern_rule_perf -- --ignored --nocapture
```

Times the full rule pipeline — find_pattern matching + constraints +
enclosing-entity correlation + Finding conversion — against `ast-grep run` on
the same target, 5 interleaved runs each side, median. Must run in release
mode and interleave both sides per run to avoid machine-drift skew (see
Methodology).

## Open work

- [x] Close the `find_pattern` gap — **done 2026-09-03, now beats ast-grep**
      (0.22–0.97x across all 3 scale repos, both languages). The parallel walk
      removed the walk/sort barrier; the literal-atom prefilter then skips
      parsing 80–99% of files that cannot match. Match parity preserved. See
      the `find_pattern` section above.
- [ ] **Optional follow-on — persisted `token→files` prefilter index.** The
      current prefilter is index-free: it still *reads* every file to
      substring-check it (hermes-agent TS: reads 1412 files, parses 23; the
      1389 unparsed reads are the only remaining cost on skipped files). A
      persisted token index would answer the prefilter without the read (and
      could skip the directory walk entirely, jumping straight to candidate
      files). Deferred, not scheduled: the read is the *cheap* half (parse was
      the expensive half, already eliminated), and persisting a token index
      adds build cost + a freshness-correctness surface (a stale index → missed
      matches) that the always-correct query-time filter avoids. Build only if
      profiling a real workload shows the reads bottleneck.
- [ ] Close the `scan` pattern-rule pipeline gap (1.56x slower). Unlike
      `find_pattern`, `scan` matches a single target, so the file-skipping
      prefilter does not apply — this is a matcher-level gap. varde-code
      already links `ast-grep-core` (same version as the ast-grep CLI), which
      exposes an optimized `Pattern`/`Matcher`; adopting it in place of the
      hand-rolled matcher would likely land at parity and delete the
      hand-rolled matcher's maintenance/correctness burden. Correctness-
      sensitive (the matcher backs both `find_pattern` and `scan`) — port
      behind the existing test suite.
- [x] Investigated `build` cold on `hermes-agent`-scale repos — improved
      12.37s to 6.70s (1.8x) by fixing two genuine algorithmic bugs
      (`resolve_type_hierarchy`'s O(N×M) owner lookup;
      `scan`'s unreserved-`Vec` merge). Still 2.2x over the < 3s target,
      but a `samply` CPU profile shows 67% of remaining time is inside
      tree-sitter's parser (inherent, already fully parallelized) — no
      further bug found. Closing the rest means cutting a feature's cost
      (parser swap, lazy/optional MinHash), not more tuning; see "Scale
      build targets" above for the full investigation and what was tried
      and ruled out.
- [ ] Re-checkout `~/source/varde` (or pick a new representative target) so
      the `throw new Error($MSG)`, `symbols_in_file`, and `extract` rows
      that depend on it can be re-measured.
- [ ] Re-benchmark `symbols_in_file`/`filter_symbols`/`find_imports`
      individually (currently only `filter_symbols` has a warm number in
      the "Other query modes" table) to confirm all three hold the 10%
      goal, not just `filter_symbols`.
- [x] Investigate `nav_map`'s 63s warm time on `hermes-agent` (7-15x its
      cost on the other two scale repos, disproportionate to their file
      count difference) — root cause was a missing index on
      `resolved_edges(from_entity_id/to_entity_id)`; fixed, hermes-agent warm
      time dropped to 10.3s. `nav_map` still recomputes fully on every call
      (unlike the other 16 "Other query modes" rows) — that remains
      unaddressed, just no longer disproportionate to repo size.
