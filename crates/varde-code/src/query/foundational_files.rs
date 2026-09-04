//! Foundational-files fan-in leaderboard.
//!
//! A standalone computable unit for the nav-map "Foundational files" section
//! (see `memory-bank/working/plans/2026-09-03-nav-map-draft/plan.md`,
//! "Sections (output shape)" → "Foundational files"): files ranked by
//! fan-in, noise-filtered, each entry decorated with a one-line "why".
//!
//! Fan-in counting is NOT reimplemented here. `files.fan_in` is already
//! maintained incrementally by `persist.rs` off `resolved_edges` (see
//! `persist::write_fan_in_fan_out` and its scoped-rewrite counterparts), and
//! is the same column the existing `map_file` mode
//! ([`crate::query::mapping::map_file`]) surfaces per-file. The graph
//! module's `dependents` mode ([`crate::query::graph::dependents`]) answers
//! a different question — the *transitive* set of files reachable via
//! reverse BFS, deduplicated to distinct file paths — not a per-file import
//! count, so it isn't the right primitive to reuse for "imported N times".
//! This module reads the same precomputed `files.fan_in` column directly,
//! adding only noise-filtering, `why`-string generation, and the
//! leaderboard sort/limit on top.
//!
//! Out of scope (per the task): edge listing (dependency-graph edges belong
//! to the future "flows" section) and deduping against the entrypoints list
//! (foundational files and entrypoints are deliberately separate
//! categories). This module is not yet wired into any dispatcher/CLI mode —
//! that's the later `nav-map-dispatch-cli` task.

use rusqlite::Connection;

use super::noise_filter::is_generated_or_vendored_path;
use super::{ApiError, db_err};

/// Compute the foundational-files fan-in leaderboard.
///
/// Reads `files.path`/`files.fan_in` (already-maintained fan-in counts, see
/// module docs), excludes test files (via the authoritative `is_test_path`
/// generated column, so a heavily-imported test helper never ranks as
/// foundational — matching [`crate::query::entrypoints`]), drops any path
/// [`is_generated_or_vendored_path`] flags as
/// generated/vendored, sorts descending by fan-in (ties broken by path for
/// determinism), and caps the result at `limit` entries (`None` =
/// unbounded). Each entry is `{"file", "count", "why"}`; `why` is always a
/// non-empty one-line string.
pub fn leaderboard(conn: &Connection, limit: Option<usize>) -> Result<serde_json::Value, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT path, fan_in FROM files
             WHERE fan_in > 0 AND is_test_path = 0
             ORDER BY fan_in DESC, path ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(db_err)?;

    let mut entries = Vec::new();
    for row in rows {
        let (path, fan_in) = row.map_err(db_err)?;
        if is_generated_or_vendored_path(&path) {
            continue;
        }
        let why = format!(
            "imported/referenced by {fan_in} other file{plural} in the repo",
            plural = if fan_in == 1 { "" } else { "s" }
        );
        entries.push(serde_json::json!({
            "file": path,
            "count": fan_in,
            "why": why,
        }));
    }
    if let Some(limit) = limit {
        entries.truncate(limit);
    }
    Ok(serde_json::json!(entries))
}

#[cfg(test)]
mod foundational_files_section_tests {
    use super::*;

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "varde-foundational-files-{label}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ))
    }

    fn insert_file(conn: &Connection, path: &str, fan_in: i64) {
        conn.execute(
            "INSERT INTO files (path, fan_in) VALUES (?1, ?2)",
            rusqlite::params![path, fan_in],
        )
        .expect("insert file row");
    }

    /// AC1: `node_modules/dep.js` (fan_in 10) is filtered out as
    /// vendored/noise, while `src/core.rs` (fan_in 5) survives with its
    /// exact count.
    #[test]
    fn foundational_files_section_excludes_vendored_and_reports_count() {
        let path = temp_db_path("basic");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        insert_file(&conn, "node_modules/dep.js", 10);
        insert_file(&conn, "src/core.rs", 5);

        let result = leaderboard(&conn, None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        let core_entry = entries
            .iter()
            .find(|e| e["file"] == "src/core.rs")
            .expect("src/core.rs present in leaderboard");
        assert_eq!(
            core_entry["count"], 5,
            "src/core.rs count matches fan_in: {core_entry}"
        );

        assert!(
            !entries.iter().any(|e| e["file"] == "node_modules/dep.js"),
            "node_modules/dep.js must be excluded as vendored noise: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// AC2: every surviving leaderboard entry carries a non-empty `why`.
    #[test]
    fn foundational_files_section_every_entry_has_nonempty_why() {
        let path = temp_db_path("why");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        insert_file(&conn, "src/core.rs", 5);
        insert_file(&conn, "src/util.rs", 1);
        insert_file(&conn, "dist/bundle.js", 20);

        let result = leaderboard(&conn, None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert!(
            !entries.is_empty(),
            "leaderboard should have surviving entries"
        );
        for entry in entries {
            let why = entry["why"].as_str().unwrap_or("");
            assert!(!why.is_empty(), "entry missing non-empty why: {entry}");
        }
        assert!(
            !entries.iter().any(|e| e["file"] == "dist/bundle.js"),
            "dist/bundle.js must be excluded as generated noise: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Test files are excluded even with high fan-in: `src/util_test.go`
    /// (fan_in 10, a Go test file per the `is_test_path` column) must not
    /// rank as foundational, while `src/core.rs` (fan_in 5) survives.
    #[test]
    fn foundational_files_section_excludes_test_files() {
        let path = temp_db_path("test-paths");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        insert_file(&conn, "src/util_test.go", 10);
        insert_file(&conn, "src/core.rs", 5);

        let result = leaderboard(&conn, None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert!(
            !entries.iter().any(|e| e["file"] == "src/util_test.go"),
            "test file src/util_test.go must be excluded: {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e["file"] == "src/core.rs"),
            "non-test src/core.rs should remain: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Leaderboard sorts descending by fan-in and respects `limit`.
    #[test]
    fn foundational_files_section_sorts_and_limits() {
        let path = temp_db_path("sort-limit");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        insert_file(&conn, "src/a.rs", 2);
        insert_file(&conn, "src/b.rs", 9);
        insert_file(&conn, "src/c.rs", 5);

        let result = leaderboard(&conn, Some(2)).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert_eq!(
            entries.len(),
            2,
            "limit truncates to 2 entries: {entries:?}"
        );
        assert_eq!(entries[0]["file"], "src/b.rs");
        assert_eq!(entries[1]["file"], "src/c.rs");

        let _ = std::fs::remove_file(&path);
    }
}
