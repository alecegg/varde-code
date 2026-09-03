---
name: benchmark
description: >
  TRIGGER: Re-run the varde-code vs ast-grep benchmark suite and update
  BENCHMARK.md with fresh numbers. Use when the user asks to rerun/refresh
  benchmarks, re-benchmark a mode after a perf change, or check whether the
  10%-of-ast-grep goal is closer.
  SKIP: Skip for one-off ad hoc timing checks unrelated to BENCHMARK.md's
  tracked rows — just run the command directly instead.
  Example phrases: "rerun all the benchmarks" or "re-benchmark find_pattern
  on hermes-agent".
allowed-tools: Bash Read Edit
compatibility: "Requires a release build (cargo build --release -p varde-code), ast-grep on PATH, and ~/source/reference-repos + ~/source/varde checked out for the scale-repo rows."
---

## What this is

`BENCHMARK.md` (repo root) is the single canonical measurement file for
varde-code's speed/quality vs `ast-grep`. Each row is reproducible from a
command in that file. This skill scripts those commands so re-running the
suite doesn't require re-deriving invocations by hand each time.

## Workflow

1. **Build once.** `scripts/build_release.sh` — `cargo build --release
   -p varde-code`. All other scripts depend on `target/release/varde-code`
   and will error out with a clear message if it's missing.
2. **Run the suite.** `scripts/run_all.sh` runs every row currently tracked
   in `BENCHMARK.md`, in table order, printing labeled TSV/text output
   section by section. Pass `--build` to fold step 1 in.
   - To re-run a single section (e.g. after a perf change to one mode),
     invoke that section's script directly instead — see "Scripts" below.
3. **Read the output, transcribe by hand.** Do not script the BENCHMARK.md
   edit. Per the file's own methodology ("Reproduce, don't eyeball"), sanity
   check each number against the previous round (does the delta make sense
   given what changed?) before writing it in. Update:
   - the relevant `Results` / scale-target table row(s)
   - the `(2026-09-03, ...)` — style date header on the section you touched
   - the `Open work` checklist if a target was newly hit or newly missed
4. **Note skips.** If a reference repo or `~/source/varde` isn't checked out
   locally, the corresponding script/row is skipped with a `SKIP` message on
   stderr — report which rows were skipped rather than silently omitting
   them from the summary.

## Scripts

All under `scripts/`, all read args positionally (see each script's header
comment for exact usage) and all print machine-parseable TSV so results can
be read back without re-deriving parsing logic:

| Script | Section in BENCHMARK.md |
|---|---|
| `build_release.sh` | (prerequisite, not a row) |
| `find_pattern.sh <label> <path> <pattern> <ag-lang> <varde-lang>` | `find_pattern` |
| `symbols_in_file.sh <repo-root> <file-path> [cache-name]` | `symbols_in_file` / `filter_symbols` / `find_imports` vs `outline` |
| `scan_pattern_rule.sh` | `scan` pattern-rule pipeline |
| `build_scale.sh [repo ...]` | Scale build targets |
| `extract.sh [path]` | `extract` |
| `git_perf.sh` | Query-path freshness (`ensure_fresh` no-op cost) |
| `other_modes.sh <repo-root> <file-path> <symbol-name> [cache-name]` | Other query modes (baseline, no ast-grep equivalent) |
| `run_all.sh [--build]` | Runs all of the above, in `BENCHMARK.md` order |
| `lib.sh` | Shared helpers (sourced, not run directly) |

## Adding a new tracked row

When a new query mode gets an ast-grep-comparable benchmark row (or an
existing one gets a new repo/pattern), add a script (or a new invocation of
an existing parametrized script) here first, then add the row to
`BENCHMARK.md`'s `Overlap inventory` and `Results` tables — keep the two in
sync so `run_all.sh` never silently drifts from what the file claims to
track.
