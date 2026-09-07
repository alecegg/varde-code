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
//! This module ranks by the precomputed `files.fan_in` column directly, and
//! for the `why` string additionally computes each file's distinct
//! dependent-file count (a single `GROUP BY` over `resolved_edges`, deduped by
//! `from_file_id`) — because `fan_in` is a reference/edge total, not a file
//! count. On top of that it adds noise-filtering, `why`-string generation, and
//! the leaderboard sort/limit.
//!
//! Out of scope (per the task): edge listing (dependency-graph edges belong
//! to the future "flows" section) and deduping against the entrypoints list
//! (foundational files and entrypoints are deliberately separate
//! categories). This module is not yet wired into any dispatcher/CLI mode —
//! that's the later `nav-map-dispatch-cli` task.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use super::noise_filter::{is_frontend_asset_path, is_generated_or_vendored_path};
use super::symbols_section::is_accessor;
use super::{ApiError, db_err};

/// Distinct dependent-file count per target file id.
///
/// `files.fan_in` counts resolved *edges* into a file — every import plus
/// every cross-file call site — so it over-counts "how many files depend on
/// this" whenever one file references a target many times (observed:
/// `llm.py` with `fan_in` 1171 in a ~730-file repo). This is the honest
/// distinct-file count for the `why` string: how many *other* files have at
/// least one resolved edge into the target. It reads the same `resolved_edges`
/// set `fan_in` is built from (`to_file_id` is populated for both import and
/// call edges — see `persist.rs`), deduped by `from_file_id` and excluding
/// self-edges.
fn distinct_dependents_by_file(conn: &Connection) -> Result<HashMap<i64, i64>, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT to_file_id, COUNT(DISTINCT from_file_id)
             FROM resolved_edges
             WHERE resolved = 1 AND to_file_id IS NOT NULL AND from_file_id != to_file_id
             GROUP BY to_file_id",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .map_err(db_err)?;
    let mut map = HashMap::new();
    for row in rows {
        let (file_id, dependents) = row.map_err(db_err)?;
        map.insert(file_id, dependents);
    }
    Ok(map)
}

/// Standard value-type boilerplate method names (besides accessors) that a
/// data class carries — case-insensitive. `equals`/`hashCode`/`toString` are
/// the Java/Kotlin trio; `compareTo`/`clone`/`copy` and the builder/factory
/// idioms (`builder`/`of`/`valueOf`/`with`) round out records and DTOs.
const DATA_CLASS_BOILERPLATE: &[&str] = &[
    "equals",
    "hashcode",
    "tostring",
    "compareto",
    "clone",
    "copy",
    "builder",
    "of",
    "valueof",
    "with",
    "deconstruct",
];

/// True when `name` is a trivial data-holder method: an accessor
/// ([`is_accessor`]) or standard value-type boilerplate
/// ([`DATA_CLASS_BOILERPLATE`]).
fn is_trivial_data_method(name: &str) -> bool {
    is_accessor(name) || DATA_CLASS_BOILERPLATE.contains(&name.to_ascii_lowercase().as_str())
}

/// File ids whose defined methods are *all* trivial data-holder methods —
/// i.e. a data class (a JPA `@Entity`, a POJO/record, a DTO). Such a file is
/// heavily depended on but low orientation value: an agent learns nothing
/// about a service's architecture from `Person` or `BaseEntity` (audit F9).
/// Only files with at least one `Function` entity are considered, and every
/// one must be trivial — a single non-accessor method (real behavior) means
/// the file is not a pure data holder and is left ranked on its merits.
fn data_class_file_ids(conn: &Connection) -> Result<HashSet<i64>, ApiError> {
    let mut stmt = conn
        .prepare("SELECT file_id, name FROM entities WHERE kind = ?1")
        .map_err(db_err)?;
    let rows = stmt
        .query_map([crate::model::EntityKind::Function.as_i64()], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_err)?;
    // file_id -> "every method seen so far is trivial". A key exists only for
    // files that define at least one method.
    let mut all_trivial: HashMap<i64, bool> = HashMap::new();
    for row in rows {
        let (file_id, name) = row.map_err(db_err)?;
        let entry = all_trivial.entry(file_id).or_insert(true);
        *entry = *entry && is_trivial_data_method(&name);
    }
    Ok(all_trivial
        .into_iter()
        .filter_map(|(file_id, trivial)| trivial.then_some(file_id))
        .collect())
}

/// Compute the foundational-files fan-in leaderboard.
///
/// Reads `files.path`/`files.fan_in` (already-maintained fan-in counts, see
/// module docs), excludes test files (via the authoritative `is_test_path`
/// generated column, so a heavily-imported test helper never ranks as
/// foundational — matching [`crate::query::entrypoints`]), drops any path
/// [`is_generated_or_vendored_path`] flags as generated/vendored, ranks
/// descending by **distinct dependent files** (ties broken by `fan_in`
/// descending, then path for determinism), and caps the result at `limit`
/// entries (`None` = unbounded).
///
/// Distinct dependents — not `fan_in` — is the ranking key because breadth of
/// dependence is the "how foundational is this file" signal: a core module
/// imported once by 143 files is more foundational than a hot utility called
/// 1171 times from a handful of callers. `fan_in` (a reference/edge total) can
/// wildly over-state centrality when one file references a target many times,
/// so it only breaks ties here. Each entry is `{"file", "count", "dependents",
/// "why"}`: `count` is `fan_in` (total resolved references, kept for context),
/// `dependents` is the distinct count of other files that reference it (the
/// ranking key), and `why` is always a non-empty one-line string built from
/// both.
pub fn leaderboard(conn: &Connection, limit: Option<usize>) -> Result<serde_json::Value, ApiError> {
    let dependents_by_file = distinct_dependents_by_file(conn)?;
    let data_classes = data_class_file_ids(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, path, fan_in FROM files
             WHERE fan_in > 0 AND is_test_path = 0",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(db_err)?;

    // (is_data_class, dependents, fan_in, path) — collected first so the
    // leaderboard can be ranked by signals SQL doesn't have to hand: distinct
    // dependents (the GROUP-BY map above) and the data-class flag (a per-file
    // method-name heuristic). `is_data_class` is the *primary* discriminator so
    // heavily-depended-on data holders (a JPA `@Entity`, a DTO) sink below real
    // modules of comparable fan-in instead of topping the list (audit F9).
    let mut ranked: Vec<(bool, i64, i64, String)> = Vec::new();
    for row in rows {
        let (file_id, path, fan_in) = row.map_err(db_err)?;
        if is_generated_or_vendored_path(&path) || is_frontend_asset_path(&path) {
            continue;
        }
        let dependents = dependents_by_file.get(&file_id).copied().unwrap_or(0);
        ranked.push((data_classes.contains(&file_id), dependents, fan_in, path));
    }
    // Rank: non-data-class first, then distinct dependents desc, then fan_in
    // desc, then path asc. `false < true`, so ascending on the flag puts real
    // modules ahead of data classes.
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    if let Some(limit) = limit {
        ranked.truncate(limit);
    }

    let entries: Vec<serde_json::Value> = ranked
        .into_iter()
        .map(|(_is_data_class, dependents, fan_in, path)| {
            // Describe the distinct dependent-file count (the real "how central
            // is this file" signal), and only add the raw reference total when
            // it differs — so a broadly-imported core module and a hot utility
            // called many times from a few files read differently instead of
            // sharing one boilerplate line.
            let why = if dependents == fan_in {
                format!(
                    "depended on by {dependents} other file{p} in the repo",
                    p = if dependents == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "depended on by {dependents} other file{p} ({fan_in} references total) in the repo",
                    p = if dependents == 1 { "" } else { "s" }
                )
            };
            serde_json::json!({
                "file": path,
                "count": fan_in,
                "dependents": dependents,
                "why": why,
            })
        })
        .collect();
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

    fn insert_file_id(conn: &Connection, path: &str, fan_in: i64) -> i64 {
        conn.execute(
            "INSERT INTO files (path, fan_in) VALUES (?1, ?2)",
            rusqlite::params![path, fan_in],
        )
        .expect("insert file row");
        conn.last_insert_rowid()
    }

    fn insert_resolved_edge(conn: &Connection, from_file_id: i64, to_file_id: i64) {
        conn.execute(
            "INSERT INTO resolved_edges (from_file_id, to_file_id, kind, resolved)
             VALUES (?1, ?2, 0, 1)",
            rusqlite::params![from_file_id, to_file_id],
        )
        .expect("insert resolved edge");
    }

    /// The `why`/`dependents` report DISTINCT dependent files, not the raw
    /// `fan_in` reference count: a core file referenced 5 times from only 2
    /// other files is "depended on by 2 other files (5 references total)", not
    /// "5 files".
    #[test]
    fn foundational_files_section_reports_distinct_dependents_not_reference_count() {
        let path = temp_db_path("distinct-dependents");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        // core.rs has fan_in 5 (5 resolved references) but only 2 distinct
        // dependent files (a.rs references it 3×, b.rs 2×).
        let core = insert_file_id(&conn, "src/core.rs", 5);
        let a = insert_file_id(&conn, "src/a.rs", 0);
        let b = insert_file_id(&conn, "src/b.rs", 0);
        for _ in 0..3 {
            insert_resolved_edge(&conn, a, core);
        }
        for _ in 0..2 {
            insert_resolved_edge(&conn, b, core);
        }

        let result = leaderboard(&conn, None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");
        let core_entry = entries
            .iter()
            .find(|e| e["file"] == "src/core.rs")
            .expect("core present");

        assert_eq!(core_entry["count"], 5, "count stays fan_in: {core_entry}");
        assert_eq!(
            core_entry["dependents"], 2,
            "dependents is the distinct file count: {core_entry}"
        );
        let why = core_entry["why"].as_str().unwrap();
        assert!(
            why.contains("2 other files") && why.contains("5 references total"),
            "why must report distinct files and the reference total: {why}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Ranking key is distinct dependents, NOT the raw `fan_in` reference
    /// total: `base.rs` depended on by 3 distinct files (fan_in 3) outranks
    /// `hot.rs` referenced 100 times but only from 1 file (fan_in 100),
    /// because breadth of dependence is the centrality signal, not call volume.
    #[test]
    fn foundational_files_section_ranks_by_distinct_dependents_not_fan_in() {
        let path = temp_db_path("rank-by-dependents");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        // hot.rs: fan_in 100 but all from a single caller.
        let hot = insert_file_id(&conn, "src/hot.rs", 100);
        // base.rs: fan_in 3, one edge from each of 3 distinct files.
        let base = insert_file_id(&conn, "src/base.rs", 3);
        let caller = insert_file_id(&conn, "src/caller.rs", 0);
        let a = insert_file_id(&conn, "src/a.rs", 0);
        let b = insert_file_id(&conn, "src/b.rs", 0);
        for _ in 0..100 {
            insert_resolved_edge(&conn, caller, hot);
        }
        insert_resolved_edge(&conn, caller, base);
        insert_resolved_edge(&conn, a, base);
        insert_resolved_edge(&conn, b, base);

        let result = leaderboard(&conn, None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert_eq!(
            entries[0]["file"], "src/base.rs",
            "file with more distinct dependents ranks first: {entries:?}"
        );
        assert_eq!(entries[0]["dependents"], 3);
        assert_eq!(entries[1]["file"], "src/hot.rs");
        assert_eq!(entries[1]["dependents"], 1);

        let _ = std::fs::remove_file(&path);
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

    fn insert_function(conn: &Connection, file_id: i64, name: &str) {
        conn.execute(
            "INSERT INTO entities (kind, name, file_id, start_byte, end_byte, start_line, start_col, end_line, end_col)
             VALUES (?1, ?2, ?3, 0, 0, 0, 0, 0, 0)",
            rusqlite::params![crate::model::EntityKind::Function.as_i64(), name, file_id],
        )
        .expect("insert function entity");
    }

    /// A data-class file (all methods are accessors/boilerplate) sinks below a
    /// real module in the leaderboard even when it is depended on by *more*
    /// files — a JPA `@Entity` shouldn't top the foundational list (audit F9).
    #[test]
    fn foundational_files_section_sinks_data_classes_below_real_modules() {
        let path = temp_db_path("data-class-sink");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        // Person: a data class (getters/setters + equals), depended on by 3.
        let person = insert_file_id(&conn, "src/Person.java", 3);
        insert_function(&conn, person, "getName");
        insert_function(&conn, person, "setName");
        insert_function(&conn, person, "equals");
        // OrderService: real behavior, depended on by only 2.
        let svc = insert_file_id(&conn, "src/OrderService.java", 2);
        insert_function(&conn, svc, "placeOrder");
        insert_function(&conn, svc, "getName"); // a mix, but not all-trivial

        for i in 0..3 {
            let dep = insert_file_id(&conn, &format!("src/p{i}.java"), 0);
            insert_resolved_edge(&conn, dep, person);
        }
        for i in 0..2 {
            let dep = insert_file_id(&conn, &format!("src/s{i}.java"), 0);
            insert_resolved_edge(&conn, dep, svc);
        }

        let result = leaderboard(&conn, None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");
        let svc_pos = entries
            .iter()
            .position(|e| e["file"] == "src/OrderService.java")
            .expect("service present");
        let person_pos = entries
            .iter()
            .position(|e| e["file"] == "src/Person.java")
            .expect("person present");
        assert!(
            svc_pos < person_pos,
            "real module must outrank the more-depended-on data class: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn data_class_detection_requires_all_methods_trivial() {
        assert!(is_trivial_data_method("getName"));
        assert!(is_trivial_data_method("setId"));
        assert!(is_trivial_data_method("equals"));
        assert!(is_trivial_data_method("toString"));
        assert!(!is_trivial_data_method("placeOrder"));
        assert!(!is_trivial_data_method("reconcile"));
    }
}
