# Code Review: `varde-code`

**Date:** 2026-09-03
**Scope:** Full-repo review of the single `varde-code` crate (~39k LOC) — tree-sitter parse → extract → resolve → SQLite-backed query/scan.
**Method:** Three focused review passes over the parse/extract, resolve/query, and rules/scan/persist/CLI subsystems; headline findings verified against source.

## Summary

Overall the code is **well-structured and defensively written**: SQL is uniformly parameterized (no injection found anywhere), persistence is transaction-wrapped with temp-file-then-rename atomicity, char-boundary checks guard the rewrite splice path, and the combinatorially dangerous pattern matcher is memoized and budget-capped. Findings are concentrated in a few themes.

### Cross-cutting themes
1. **Unbounded recursion on adversarial/generated input** — three separate tree/graph walks can overflow the stack (uncatchable abort).
2. **TOCTOU in `scan --apply`** — the write path re-reads files independently of the parse that produced the spans.
3. **Silent `u32` truncation of byte offsets** — pervasive, latent (>4 GB files only), but unguarded.
4. **Inconsistent error posture** — some paths surface DB errors; sibling paths swallow them to defaults.

---

## Critical

### C1 — Unbounded recursion → stack overflow (process abort) on deeply nested input  ✅ FIXED (2026-09-03)
**Files:** `extract/mod.rs:50` (`walk`), `extract/symbol.rs:37`, `extract/langs/rust.rs:214`, `query/flows.rs:156` (`expand`)

All are native recursion with no depth cap. A single machine-generated or transpiled file with a deep expression tree (`((((…))))` thousands deep) or a deep linear call chain aborts the whole process — not a catchable error, directly defeating the "robust on huge/malformed files" goal.

**Fix applied:** added explicit depth caps to all four sites. The two tree walks (`walk`, `extract_symbols_inner`) and the Rust pattern collector share `extract::MAX_WALK_DEPTH` (2000) and flag `has_error` on bail; `expand` uses `MAX_FLOW_DEPTH` (1000) and renders deeper callees as leaves. All 400+ tests pass.

---

## High

### H1 — TOCTOU in `scan --apply`: spans computed against the parse, spliced into an independent re-read  ✅ FIXED (2026-09-03)
**Files:** `scan_cli.rs:126` (parse) vs `scan_cli.rs:354` (re-read + splice)

Char-boundary/range checks catch *out-of-range* stale offsets (→ `SkippedConflict`), but a file edited between parse and write whose stale offsets still land in-range silently corrupts the file.

**Fix applied:** `Finding` now carries `matched_file_state: Option<FileState>` (mtime + len), stat'd once per file at match time in `rules/pattern.rs`. `apply_rewrites` compares a fresh stat against this recorded state immediately before reading/splicing; a mismatch marks every finding in that file `SkippedConflict` and skips the write. Covered by a new regression test (`apply_skips_finding_whose_file_changed_since_it_was_matched`) that injects a deliberately stale `matched_file_state` and asserts the file is left untouched.

### H2 — TOCTOU: git-dirty gate and per-file write are not atomic  ✅ FIXED (2026-09-03)
**Files:** `scan_cli.rs:313` (batched `git status`) vs `scan_cli.rs:375` (per-file write)

A file that becomes dirty after its status probe but before its write is overwritten, destroying uncommitted work; the window scales with repo size.

**Fix applied:** `GitGate::WorkTree` now retains the work-tree root, and a new `GitGate::recheck_dirty` method re-runs a single-file `git status` immediately before each write (fails closed on a status error). This adds one subprocess call per file actually written — proportional to output, not to the candidate set, so it doesn't reintroduce the per-candidate-file cost the original batched probe (CODE-003) was designed to avoid. Covered by a new regression test (`recheck_dirty_catches_a_file_dirtied_after_the_batched_probe`) that dirties a file after the batched probe and asserts the stale `dirty()` result misses it while `recheck_dirty()` catches it.

### H3 — `query/flows.rs` N+1 SQL inside the recursive walk  ✅ FIXED (2026-09-03)
**File:** `query/flows.rs` (`expand`, `:156`, callee lookup `:175`)

`expand` issues a `callees` query + `entity_info` query per node (plus another `entity_info` per callee) — O(nodes) round-trips inside a recursive descent. This is the exact shape `entrypoints::detect` was refactored away from.

**Fix applied:** added `CallGraph::load`, which reads the full `entities`/`files` join and the full `resolved_edges` call-edge join in two statements, and builds two in-memory maps (`entity_id -> (file, name)` and `(file_id, enclosing name) -> callee ids`). `build_flows` loads it once and `expand` walks it in memory, replacing the old per-node `entity_info`/`callees` round-trips. Covered by a new regression test (`build_flows_issues_a_bounded_number_of_statements_regardless_of_chain_length`) that traces statement counts on a 25-node linear call chain and asserts the count stays below the chain length.

### H4 — `type_hierarchy` parent-walk has no cycle guard → infinite loop  ✅ FIXED (2026-09-03)
**File:** `query/graph.rs:431`

Re-queries `enclosing_function` upward with no visited set. Because the lookup is `name + file ORDER BY id LIMIT 1`, two same-file entities that mutually enclose each other by name loop forever (it only breaks on empty/`None` enclosing).

**Fix applied:** the parent-walk loop in `type_hierarchy` now tracks a `HashSet<(file, name)>` of every node visited and breaks as soon as it would revisit one, in addition to the existing empty/absent-`enclosing_function` exit. Covered by a new regression test (`cyclic_enclosing_function_terminates_instead_of_looping_forever` in `tests/query_graph_traversal.rs`) with two entities that mutually enclose each other by name — without the fix this test hangs rather than fails.

---

## Medium

### M1 — Silent `u32` truncation of byte offsets  ✅ FIXED (2026-09-03)
**Files:** `extract/mod.rs:132`, `persist.rs:892/938`, `sql.rs:131`

Files >4 GB wrap with no diagnostic. Real-world-safe but an unguarded correctness cliff — prefer a checked cast with a diagnostic.

**Fix applied:** added `model::saturating_u32`, a generic checked cast that saturates to `u32::MAX` and logs a `tracing::warn!` instead of silently wrapping. Applied at the two sites where a truncation can actually originate: `extract::span_of` (byte offsets/line/column from tree-sitter's `usize` positions) and `rules::sql`'s `kind=sql` rule handler (an untrusted line number read back as `i64` from a rule-authored query). `persist.rs:892/938`'s casts are round-trips of `i64` columns that were themselves written from `Span`'s `u32` fields, so they can't truncate independently — left unchanged.

### M2 — MinHash LSH derives all 16 hashes from one FNV base  ✅ FIXED (2026-09-03)
**File:** `extract/minhash.rs:64`

`mix_hash` is a bijection of the single base hash, so colliding shingles collide under *all* 16 "independent" functions — reduces effective entropy below the 16-hash design and degrades clone-detection recall (the feature this module exists for).

**Fix applied:** replaced the single-FNV-pass-plus-bijection scheme with 16 independent FNV-1a passes per shingle, each starting from its own seeded offset basis (`HASH_SEEDS`, derived from the same `SEED_MIX` constant but now folded into the *start* of the hash rather than post-processing a shared end result). A bijection of one value carries no more entropy than that value — invertible, so any one derived value fully determines the base hash and every other derived value; seeding the fold itself makes each function a genuinely distinct computation over the full shingle content. Covered by two new tests: `hash_seeds_are_pairwise_distinct` (sanity) and `bands_can_pick_different_winning_shingles_independently`, which asserts the 4 LSH bands disagree on which of three near-identical texts has the smallest signature — impossible under the old bijection scheme, where all bands necessarily agreed. The pre-existing `clone_detection_minhash_lsh` correctness test still passes unchanged.

### M3 — `node_is_test` uses substring `contains("test")`  ✅ FIXED (2026-09-03)
**File:** `extract/langs/mod.rs:98`

`#[cfg(feature="fastest")]`, `#[attest]`, etc. flip `is_test`, silently excluding real production functions from scan.

**Fix:** match the attribute path token, not a raw substring.
*Confirmed.*

**Fix applied:** added `attribute_is_test`, which extracts the attribute's macro path (everything before the first `(` or `=`) and checks whether its last `::`-segment is exactly `test`, or the whole path is `rstest` — instead of a raw substring search over the attribute's full source text (which also matched `(...)` arguments and string literals). Covered by three new tests, including a direct regression for the two false positives named in the finding (`#[cfg(feature = "fastest")]`, `#[attest]`).

### M4 — Divergent "type context" definitions  ✅ FIXED (2026-09-03)
**Files:** `extract/mod.rs:149` (`TYPE_KINDS`, TS/JS-centric) vs `extract/langs/rust.rs:310` (`in_rust_type_context`)

The two lists don't agree, so Rust identifiers in type position leak out as spurious `Reference` symbols.

**Fix applied:** hoisted Rust's local type-kind list to a shared `langs::rust::TYPE_KINDS`/`is_type_kind`, and made `extract::is_type_kind` take the file's language and dispatch to it for Rust (other languages keep the existing TS-centric list unchanged). Also threaded `lang` through the separate `extract::symbol::extract_symbols` walk (used by `detect_changes`/mapping diffing), which had the same bug independently since it didn't take a language parameter at all. Verified concretely: `fn f(y: &std::string::String) -> ... { ... }` leaked `std`/`string` as `Reference` symbols before the fix (their only type-context ancestors — `reference_type`, `scoped_type_identifier` — are Rust-only kinds absent from the shared list) and does not after. Covered by a new regression test, `rust_path_identifiers_in_a_bare_reference_type_are_not_references`.

### M5 — Inconsistent DB-error handling  ✅ FIXED (2026-09-03)
**Files:** `correlate.rs:100` (`TestPathLookup`), `slice.rs:919` (`record_git_head`)

`TestPathLookup` and the test-path gate swallow query errors to `(false,false)`, silently turning suppression *off* on a broken DB — while sibling `EnclosingLookup` surfaces the same errors as `ApiError`. `record_git_head` discards results with `let _ =`, risking a stale-index fast-path next run.

**Fix:** at least a `tracing::warn`; ideally propagate.

**Fix applied:** kept both call sites' existing defaulting behavior (propagating would ripple `TestPathLookup`'s signature and `record_git_head`'s caller, which the review treats as the "ideally" tier, not required) but added `tracing::warn!` on the failure path in both. `TestPathLookup::lookup` now distinguishes the expected `QueryReturnedNoRows` (an unpersisted path — silently `(false, false)` per its documented contract) from any other DB error, which now warns with the file path and underlying error before defaulting. `record_git_head`'s two `set_slice_meta_value` calls now warn individually (with a comment noting the specific correctness consequence — mtime-walk fallback vs. the CORRECTNESS-102 dirty-worktree gap) instead of discarding via `let _ =`.

### M6 — Louvain tie-break uses exact `f64 ==`  ✅ FIXED (2026-09-03)
**File:** `resolve/community.rs:121`

Order-sensitive accumulation can break the "deterministic partition" guarantee; latent today, exposed by any parallelized adjacency build. Use an epsilon.

**Fix applied:** added a `GAIN_EPSILON` (`1e-9`) constant and changed the tie-break to `total_gain > best_gain + GAIN_EPSILON` for "strictly better" and `(total_gain - best_gain).abs() <= GAIN_EPSILON` for "tied, break toward smallest community id" — so gains that are mathematically equal but differ by float-rounding ULPs (from `HashMap`-iteration-order-dependent summation) no longer pick whichever community happened to iterate first.

---

## Low

- **L1 — `Box::leak` per meta-var name in `find_pattern`** (`query/find_pattern.rs:374`). "Tiny per call" — but this crate runs as a resident daemon; over many requests with varying pattern names it leaks monotonically. ✅ FIXED (2026-09-03) — `leak` now interns through a process-wide `OnceLock<Mutex<HashMap<String, &'static str>>>`, so a given meta-var name (`FOO`, `BAR`, ...) is leaked at most once for the process's lifetime regardless of how many pattern-match requests reuse it, capping growth at the number of *distinct* names ever seen rather than the number of calls.
- **L2 — Unchecked slice indexing** on module-boundary data: `resolve.rs:289/684` (`files[id as usize]`), `resolve/clones.rs:76`, `query/graph.rs:209` (`parent[&cur]`). Safe under current contracts, hard panic if a truncated/scoped slice is ever passed — harden with `.get()`. ✅ FIXED (2026-09-03) — all four sites now use `.get()`/`Option`-based lookups: `resolve.rs:289` skips the entity with a `tracing::debug!` on an out-of-range `file_id` instead of indexing; `resolve.rs:684`'s debug-log-only read falls back to `"<unknown>"`; `resolve/clones.rs:76` and `query/graph.rs:209` both document why the lookup is currently always-hit by construction and use `.get()`/`let-else` to turn a future invariant break into a graceful skip/`None` instead of a panic.
- **L3 — `repo_lock` PID-reuse race** (`repo_lock.rs:141`): reused PID makes callers wait out the full timeout instead of reclaiming. Documented tradeoff; record as a known liveness gap. ✅ FIXED (2026-09-03) — the review's own recommended action was to record this as a known tradeoff rather than change behavior; expanded `reclaim_if_stale`'s doc comment with an explicit "Known liveness gap" section naming the PID-reuse scenario, why it's rare enough to accept (short-lived, single-machine lock; failure mode is a bounded wait, not corruption), and why a generation-token fix wasn't worth the added complexity for this crate's lock usage.
- **L4 — Silent drops:** `rules/mod.rs:257` `entries.flatten()` drops unreadable dir entries with no diagnostic, inconsistent with the loader's otherwise scrupulous skip-and-report. ✅ FIXED (2026-09-03) — **Fix applied:** `discover`/`collect_toml_files` has no per-file `Diagnostic` return channel (unlike `parse_pack_file`/`merge`), and adding one would ripple into `load_rules`'s signature for a directory-listing edge case, not the common path — so kept the existing zero-files-on-error return shape but replaced the silent `entries.flatten()` with a `filter_map` that logs a `tracing::warn!` (naming the directory and the underlying `io::Error`) for each entry that fails to read, before continuing to fold in the entries that succeeded.
- **L5 — Duplication / dead code:** near-identical `unquote` across 8 `langs/*.rs` modules (a shared helper belongs next to `strip_generic_args`); dead `hash ^= 0` (`finding.rs:74`); duplicate `if variadic` branches (`rewrite.rs:133`); `unreachable!` baking a third-party invariant into a panic (`extract/symbol.rs:86`). ✅ FIXED (2026-09-03) — **Fix applied:** (1) added a single `pub fn unquote(text: &str, allow_backtick: bool) -> String` next to `strip_generic_args` in `langs/mod.rs`; removed the 8 near-identical local copies and repointed every call site (`go.rs`, `javascript.rs`, `ts.rs` pass `true` for their backtick-string grammars; `cs.rs`, `java.rs`, `swift.rs`, `kotlin.rs`, `python.rs` pass `false`). (2) Removed the no-op `hash ^= 0` in `finding.rs::finding_id`, keeping the `hash = hash.wrapping_mul(PRIME)` that follows it (a real per-field mixing step, not dead) with a comment explaining it stands in for hashing the implicit `\0` field separator. (3) Collapsed `rewrite.rs::capture_text`'s two byte-for-byte-identical `if variadic { ... } else { ... }` branches into one, and dropped the now-unused `variadic` parameter (the call site in `substitute` no longer passes `token.variadic`). (4) Replaced the `unreachable!("ast-grep node text is always borrowed")` in `extract/symbol.rs::classify` with a `let-else` that returns `None` (this function's existing "not classifiable" contract) if the node's text is ever `Cow::Owned`, so a violated third-party invariant degrades gracefully instead of panicking.

---

## Verified correct (not findings)

- **SQL injection:** rule `query` text is trusted authored TOML; `:key` params bind via `ToSql`, never concatenated. All `format!`-built SQL interpolates only `?`-placeholder counts and hardcoded table names. Read-only rule connection enforced at `OpenFlags` level.
- **Apply-path slicing:** `splice` guards range + `is_char_boundary` before every slice → stale out-of-range spans become `SkippedConflict`, never a panic.
- **Persist atomicity:** full build writes to `db.tmp` then renames; `persist`/`persist_full_streaming` wrap schema-drop + writes + index-create in one transaction.
- **Incremental staleness:** schema-version mismatch, malformed stored hash, and empty index all force full rebuild; file-set-change eagerly rebuilds `graph_cache`.
- **Pattern matcher:** variadic backtracking is memoized (`(pi,si)` memo) and budget-capped (`MAX_BACKTRACK_STEPS`).

---

## Recommended priority

1. **C1** (recursion caps) and **H1/H2** (apply-path TOCTOU) — the only paths that abort the process or destroy user data.
2. **H3/H4** (flows N+1 + type_hierarchy cycle) — perf and hang on real large repos.
3. **M2/M3/M4** — result-quality bugs in the clone/scan features the engine exists to serve.

Everything below is cleanup and hardening. The persistence/incremental-build layer and the SQL/parameterization discipline are solid and surfaced no material findings.

**Status (2026-09-03): every finding in this report (C1, H1–H4, M1–M6, L1–L5) is fixed.** Verified via `cargo build` (clean) and a full `cargo test` run (409 lib tests + every integration binary, 0 failed, exit code 0).
