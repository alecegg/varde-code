# Agent notes for varde-code

## Performance constraint: parity with ast-grep

Any query mode that overlaps functionality already provided by `ast-grep` (the
upstream CLI/crate this project depends on for grammar loading and, for
`find_pattern`, pattern-matching semantics) must run in **no more than 2x** the
wall-clock time of the equivalent `ast-grep` invocation on the same input.

- Benchmark on the same target (same repo/dir, same file set) and the same
  pattern/query, run warm (repeat 3x, take the stable number) — see
  `2026-09-03` session notes for the methodology used to first measure this
  (cold vs. warm cache separated; `ast-grep` installed via
  `cargo install ast-grep` for a same-version baseline against the
  `ast-grep-core`/`ast-grep-language` versions varde-code depends on).
- If a change regresses a query past the 2x threshold, that's a blocking
  finding, not a nice-to-have — fix it before merging, don't defer it.
- Fixed as of 2026-09-03: `find_pattern`'s speed gap vs. `ast-grep run` (was
  ~5x slower, now at parity ~1x). Root cause wasn't the hand-rolled matcher
  (`match_node`/`match_sequence` in `find_pattern.rs`) — three targeted
  matcher fixes plus phase-level timing (`VARDE_PROFILE=1` env var) ruled
  that out. The actual cause: the directory-walk loop in `find_pattern`
  processed files sequentially, while `ast-grep`'s CLI walks directories via
  `ignore::WalkParallel` across threads. Fixed by parallelizing the per-file
  work with `rayon::par_iter` (new `rayon` dependency). A trailing-comma
  match bug that caused a 101-vs-105 match-count gap was also fixed (now
  105/105). See `BENCHMARK.md`.
- Fixed as of 2026-09-03: `varde-code build`'s churn perf bug. It was
  spawning one `git log -- <file>` subprocess per file (2m19s to index ~40
  files against `~/source/varde`). Replaced with `churn::commit_counts_batch`
  — one `git log --name-only` walk per git repo root, counts kept in memory.
  Same ~40-file target now takes ~1.9s wall (~74x faster). `build` was then
  benchmarked cold/end-to-end against `ast-grep outline` — fails the 2x
  budget (~10-40x slower) but judged not a blocking regression due to a
  genuine scope mismatch (full indexing pipeline vs. single-shot parse); see
  `BENCHMARK.md` for the full reasoning.
