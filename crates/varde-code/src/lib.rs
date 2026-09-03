//! varde-code: code-intelligence core.
//!
//! Multi-language tree-sitter parsing and entity/symbol extraction, exposed
//! as a library and a small CLI (`varde-code extract <path>`).

pub mod build;
pub mod churn;
pub mod cli;
pub mod complexity;
pub mod db;
pub mod extract;
pub mod git;
pub mod hooks;
pub mod model;
pub mod parse;
pub mod persist;
pub mod query;
pub mod repo_lock;
pub mod resolve;
pub mod rules;
pub mod scan;
pub mod scan_cli;
pub mod skills;
pub mod slice;
pub mod test_cli;
pub mod watch;

/// Shared guard for tests that mutate the process-global `HOME` (which drives
/// [`crate::db::path::repo_db_path`]). Every test that redirects `HOME` must
/// hold this lock so concurrent test threads don't interleave set/restore.
///
/// Not `#[cfg(test)]`-gated: `rules::test_runner::run_sql_rule_tests` (a
/// non-test, library-facing entry point used by the `test` CLI subcommand)
/// also takes this lock while it builds+indexes a fixture, so that its
/// `HOME` reads can't land mid-swap against a `#[test]` in the same binary
/// that's temporarily redirecting `HOME`. The mutex is a no-op outside of
/// test binaries (nothing else ever contends it), so this costs nothing in
/// production.
pub(crate) static HOME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Test-only pin for the Cost A git-status changed_files fast path
/// ([`crate::slice::git_fast_path_enabled`]): tests ignore the
/// `VARDE_GIT_FAST_PATH` env var entirely and decide via this flag, so CI is
/// deterministic regardless of the developer's shell. All slice tests
/// serialize on [`HOME_TEST_LOCK`], so it cannot race another slice test.
#[cfg(test)]
pub(crate) static GIT_FAST_PATH_TEST_OVERRIDE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Test-only call counter for [`crate::slice::run_scoped_edge_refresh`]
/// (CORE-GRAPH-CACHE-001's shared scoped edge engine), incremented on every
/// invocation regardless of caller (`slice::freshen_edges` or
/// `build::run_incremental`). Lets a test prove the scoped path — as opposed
/// to a full re-derivation — actually ran, since neither `rev` numbering nor
/// row counts alone distinguish the two paths in every fixture shape. Reads
/// racily against any other test that also drives the scoped path, so
/// callers should compare a before/after delta and run under a module-scoped
/// filter (e.g. `cargo test build::`) rather than the whole suite at once.
#[cfg(test)]
pub(crate) static SCOPED_EDGE_REFRESH_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Serializes every `build::` test that reads [`SCOPED_EDGE_REFRESH_CALLS`]
/// (directly, or indirectly by driving `run_incremental` down a path that
/// calls the shared scoped edge engine) so cargo's default parallel test
/// threads can't interleave increments from one test into another's
/// before/after delta.
#[cfg(test)]
pub(crate) static SCOPED_EDGE_REFRESH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Test-only call counter for the `graph_cache` fast path in
/// [`crate::query::graph::Graph::load`] (wire-graph-load-cache-first),
/// incremented every time `Graph::load` returns a cache hit (fresh-rev row,
/// decoded successfully) instead of running the full SQL-scan rebuild. Lets a
/// test prove the cache path — as opposed to the full rebuild — actually ran.
/// Reads racily against any other test that also drives `Graph::load`, so
/// callers should compare a before/after delta.
#[cfg(test)]
pub(crate) static GRAPH_CACHE_HIT_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
