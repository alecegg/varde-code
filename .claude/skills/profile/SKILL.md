## What this is

Perf-tuning loop for individual varde-code query modes ("tools"). Where the
`benchmark` skill tracks the canonical ast-grep-comparison numbers in
`BENCHMARK.md`, this skill is for the iterate-fast work of finding *which*
tool needs tuning next and hammering on just that one while you fix it —
without re-running the whole suite each time.

Grew out of manually root-causing `nav_map`'s disproportionate hermes-agent
slowness (missing SQLite index on `resolved_edges` entity-id columns —
7-15x cost vs. the other reference repos despite only ~1.3-2x more files).
`rank.sh` below is that investigation, scripted, so the next scaling
anomaly doesn't require redoing it by hand.

## Workflow

1. **Build once.** `../benchmark/scripts/build_release.sh`.
2. **Rank every tool.** `scripts/rank.sh` runs every query mode from
   `other_modes.sh` (see the `benchmark` skill) across all three reference
   repos, then prints two tables:
   - warm_ms descending (what's slowest in absolute terms)
   - a scaling-flag table: for each mode, `time_ratio` (hermes-agent warm_ms
     / repowise warm_ms) vs `file_ratio` (hermes-agent file count / repowise
     file count). Anything where `time_ratio > 2 * file_ratio` is flagged
     `SUPERLINEAR` — that's a scan-cost or missing-index smell, not just
     "this repo is bigger."
   Pick the worst offender (highest warm_ms, or flagged SUPERLINEAR) as the
   next thing to tune.
3. **Loop on one tool.** `scripts/watch.sh <mode> <repo-root> <json-args>`
   times a single mode's cold+warm cost and diffs it against the previous
   call to the same mode (cached in `$TMPDIR/varde-profile-watch/`). Re-run
   this after each edit-rebuild cycle:
   ```
   # edit code, then:
   cargo build --release -p varde-code && scripts/watch.sh nav_map \
     "$HOME/source/reference-repos/hermes-agent" '{"repoRoot":"...","..."}'
   ```
   First call in a session has no baseline to diff against — that's
   expected, just the starting point.
4. **Root-cause a slow query.** varde-code query modes are read-only SQL
   over the on-disk SQLite db at `~/.config/varde-code/repos/<name>/`. If a
   mode is disproportionately slow:
   - Check for a missing index: `EXPLAIN QUERY PLAN` the SQL in the
     relevant `src/query/*.rs` file against the repo's db (path from
     `clear_varde_cache`'s glob target) — a `SCAN` on a large table where
     you expected a `SEARCH ... USING INDEX` is the signature of what
     caused the nav_map regression.
   - Check for repeated `conn.prepare(...)` (re-parses SQL text every call)
     vs `conn.prepare_cached(...)` inside a hot loop (DFS, per-candidate
     classification, etc).
   - Check for redundant duplicate lookups of the same row inside a single
     call path (e.g. a helper called twice per iteration when once would
     do).
5. **Confirm and hand off.** Once a fix lands, re-run `rank.sh` to confirm
   the flag clears and no other mode regressed, then follow the
   `benchmark` skill's workflow to update `BENCHMARK.md` with the new
   canonical numbers — this skill's scripts are for the tuning loop, not
   the recorded-measurement file.

## Tuning `build` specifically

`build` (indexing) isn't a query mode — different CLI shape (`varde-code
build --repo-root <path>`, no `--json`), and it's a single operation, not
N modes to rank against each other, so it's outside `rank.sh`/`watch.sh`.
Use `scripts/watch_build.sh <repo-root>` instead: same tight loop, just for
`build`.
```
# edit code, then:
cargo build --release -p varde-code && scripts/watch_build.sh \
  "$HOME/source/reference-repos/hermes-agent"
```
For the initial "is `build` even worth tuning right now" read, use
`../benchmark/scripts/build_scale.sh` (all 3 reference repos in one pass) —
`build`'s canonical numbers already live in BENCHMARK.md's "Scale build
targets" section under the `benchmark` skill, including the open target of
getting hermes-agent's cold build under 3s.

## Scripts

All under `scripts/`, all source `../../benchmark/scripts/lib.sh` for the
`time_ms` / `min_of_3` / `clear_varde_cache` helpers (no need to duplicate
them — this skill assumes `benchmark` is present alongside it).

| Script | Purpose |
|---|---|
| `rank.sh [repo1-root file1 symbol1 [repo2-root file2 symbol2 [repo3-root file3 symbol3]]]` | Profile every mode across N reference repos (defaults to the same hermes-agent/oh-my-pi/repowise triples as `other_modes.sh`), print ranked + scaling-flag tables. |
| `watch.sh <mode> <repo-root> <json-args> [cache-name]` | Cold/warm-time one mode once, diff against the last `watch.sh` call for that exact (mode, repo, json) key. Meant to be re-run in a tight loop while tuning. |
| `watch_build.sh <repo-root> [cache-name]` | Same idea as `watch.sh`, but for `build` (indexing) instead of a query mode. |
| `analyze.py <data-file>` | Ranking/scaling-flag report `rank.sh` pipes its collected data through. Not usually invoked directly. |

## Notes

- These scripts print to stdout for you to read and reason about — they
  don't edit `BENCHMARK.md` or any other tracked file. `watch.sh`'s
  baseline cache lives in `$TMPDIR`, not the repo, so it doesn't leave
  droppings to clean up or commit.
- If `~/source/reference-repos/{hermes-agent,oh-my-pi,repowise}` aren't
  checked out, `rank.sh` skips with a warning on stderr rather than
  failing outright — report which repos were skipped.
