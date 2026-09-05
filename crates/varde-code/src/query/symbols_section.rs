//! Symbols (dependency graph) fan-in leaderboard.
//!
//! A standalone computable unit for the nav-map "Dependency graph (symbols)"
//! section (see
//! `memory-bank/working/plans/2026-09-03-nav-map-draft/plan.md`, "Sections
//! (output shape)" → "Dependency graph (symbols)"): a cross-file symbol
//! fan-in leaderboard, noise-filtered, with symbols already surfaced by the
//! Entrypoints section excluded so the two sections never duplicate a
//! symbol.
//!
//! Fan-in counting is NOT reimplemented here. It reuses the same
//! `resolved_edges`-counting shape [`crate::query::entrypoints`]'s
//! `is_bootstrap_by_fan_asymmetry` already uses (`COUNT(*) FROM
//! resolved_edges WHERE resolved = 1 AND to_entity_id = ?`) — the only
//! existing per-symbol fan-in primitive in this codebase (`files.fan_in`,
//! read by [`super::foundational_files`], is file-granularity and not
//! reusable here). This module aggregates that same join with `GROUP BY`
//! instead of querying it per-entity, restricts it to edges crossing a file
//! boundary (`resolved_edges.from_file_id != entities.file_id`) to match
//! "cross-file" fan-in, and adds two filtering steps on top: dropping
//! generated/vendored paths via
//! [`super::noise_filter::is_generated_or_vendored_path`], and excluding any
//! symbol whose `entity_id` also appears in
//! [`crate::query::entrypoints::detect`]'s output.
//!
//! Out of scope (per the task): changing the fan-in counting logic itself
//! beyond the noise-filter and entrypoint-dedup steps. This module is not
//! yet wired into any dispatcher/CLI mode — that's the later
//! `nav-map-dispatch-cli` task.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::model::EntityKind;
use crate::resolve::EdgeKind;

#[cfg(test)]
use super::entrypoints;
use super::noise_filter::is_generated_or_vendored_path;
use super::{ApiError, db_err};

/// Compute the cross-file symbol fan-in leaderboard.
///
/// Reads `Function`/`Class` entities and their cross-file resolved
/// `Call`-edge fan-in count (see module docs for the exact reused join),
/// excludes symbols defined in test files (via the authoritative
/// `is_test_path` generated column, matching [`entrypoints::detect`] and the
/// foundational-files leaderboard), drops any path
/// [`is_generated_or_vendored_path`] flags as
/// generated/vendored, drops any entity also present in
/// [`entrypoints::detect`]'s output, sorts descending by fan-in (ties broken
/// by path then symbol name for determinism), and caps the result at
/// `limit` entries (`None` = unbounded). Each entry is `{"file", "symbol",
/// "owner", "count"}`, where `owner` is the method's owning type (from
/// `entities.owner_type`) or `null` for a module/top-level function.
pub fn leaderboard(
    conn: &Connection,
    entrypoint_ids: &HashSet<i64>,
    limit: Option<usize>,
) -> Result<serde_json::Value, ApiError> {
    let call_kind = EdgeKind::Call.as_i64();
    let mut stmt = conn
        .prepare(
            "SELECT e.id, f.path, e.name, e.owner_type, COUNT(re.id) AS fan_in
             FROM entities e
             JOIN files f ON f.id = e.file_id
             JOIN resolved_edges re
                 ON re.to_entity_id = e.id
                AND re.resolved = 1
                AND re.kind = ?1
                AND re.from_file_id != e.file_id
             WHERE e.kind IN (?2, ?3) AND f.is_test_path = 0
             GROUP BY e.id
             HAVING COUNT(re.id) > 0
             ORDER BY fan_in DESC, f.path ASC, e.name ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                call_kind,
                EntityKind::Function.as_i64(),
                EntityKind::Class.as_i64()
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .map_err(db_err)?;

    let mut entries = Vec::new();
    for row in rows {
        let (entity_id, path, name, owner_type, fan_in) = row.map_err(db_err)?;
        if is_generated_or_vendored_path(&path) {
            continue;
        }
        if entrypoint_ids.contains(&entity_id) {
            continue;
        }
        // `owner` disambiguates common method names (`on` -> owner `EventBus`);
        // `null` for module/top-level functions and for languages that do not
        // yet record `owner_type`.
        entries.push(serde_json::json!({
            "file": path,
            "symbol": name,
            "owner": owner_type,
            "count": fan_in,
        }));
    }
    if let Some(limit) = limit {
        entries.truncate(limit);
    }
    Ok(serde_json::json!(entries))
}

#[cfg(test)]
mod symbols_section_tests {
    use super::*;

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "varde-symbols-section-{label}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ))
    }

    fn insert_file(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO files (path) VALUES (?1)",
            rusqlite::params![path],
        )
        .expect("insert file row");
        conn.last_insert_rowid()
    }

    fn insert_function(conn: &Connection, file_id: i64, name: &str) -> i64 {
        conn.execute(
            "INSERT INTO entities (kind, name, file_id, start_byte, end_byte, start_line, start_col, end_line, end_col)
             VALUES (?1, ?2, ?3, 0, 0, 0, 0, 0, 0)",
            rusqlite::params![EntityKind::Function.as_i64(), name, file_id],
        )
        .expect("insert function entity");
        conn.last_insert_rowid()
    }

    fn insert_call_edge(
        conn: &Connection,
        from_file_id: i64,
        to_file_id: i64,
        from_entity_id: i64,
        to_entity_id: i64,
    ) {
        conn.execute(
            "INSERT INTO resolved_edges (from_file_id, to_file_id, kind, resolved, from_entity_id, to_entity_id)
             VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            rusqlite::params![from_file_id, to_file_id, EdgeKind::Call.as_i64(), from_entity_id, to_entity_id],
        )
        .expect("insert resolved call edge");
    }

    /// AC1: a symbol that is also a semantic entrypoint (a route-handler
    /// role-tagged function, per `entrypoints::detect`) is absent from the
    /// symbols-section leaderboard even though it has cross-file fan-in.
    #[test]
    fn symbols_section_excludes_entrypoint_symbols() {
        let path = temp_db_path("entrypoint-dedup");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let handler_file = insert_file(&conn, "src/routes/users.ts");
        let caller_file = insert_file(&conn, "src/server.ts");
        let plain_file = insert_file(&conn, "src/util.ts");

        // Role-tagged route handler (matches TS_RULES): should be picked up
        // by entrypoints::detect and therefore excluded here.
        let handler_id = insert_function(&conn, handler_file, "handleUsers");
        conn.execute(
            "INSERT INTO entities (kind, name, file_id, start_byte, end_byte, start_line, start_col, end_line, end_col, enclosing_function)
             VALUES (?1, 'Get', ?2, 0, 0, 0, 0, 0, 0, 'handleUsers')",
            rusqlite::params![EntityKind::Decorator.as_i64(), handler_file],
        )
        .expect("insert decorator entity");

        // Plain helper function, not an entrypoint, called cross-file.
        let helper_id = insert_function(&conn, plain_file, "helper");

        let caller_id = insert_function(&conn, caller_file, "main");
        insert_call_edge(&conn, caller_file, handler_file, caller_id, handler_id);
        insert_call_edge(&conn, caller_file, plain_file, caller_id, helper_id);

        let entrypoint_ids: HashSet<i64> = entrypoints::detect(&conn)
            .expect("detect")
            .into_iter()
            .map(|e| e.entity_id)
            .collect();
        let result = leaderboard(&conn, &entrypoint_ids, None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert!(
            !entries.iter().any(|e| e["symbol"] == "handleUsers"),
            "entrypoint symbol handleUsers must be excluded: {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e["symbol"] == "helper"),
            "non-entrypoint symbol helper should remain: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// AC2: a symbol defined only in `dist/bundle.js` is excluded from the
    /// leaderboard by noise-filtering, even with cross-file fan-in.
    #[test]
    fn symbols_section_excludes_generated_vendored_symbols() {
        let path = temp_db_path("noise-filter");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let vendored_file = insert_file(&conn, "dist/bundle.js");
        let real_file = insert_file(&conn, "src/core.ts");
        let caller_file = insert_file(&conn, "src/main.ts");

        let vendored_id = insert_function(&conn, vendored_file, "bundled");
        let real_id = insert_function(&conn, real_file, "core");
        let caller_id = insert_function(&conn, caller_file, "main");

        insert_call_edge(&conn, caller_file, vendored_file, caller_id, vendored_id);
        insert_call_edge(&conn, caller_file, real_file, caller_id, real_id);

        let result = leaderboard(&conn, &HashSet::new(), None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert!(
            !entries.iter().any(|e| e["symbol"] == "bundled"),
            "symbol defined only in dist/bundle.js must be excluded: {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e["symbol"] == "core"),
            "symbol defined in src/core.ts should remain: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A symbol defined in a test file is excluded from the leaderboard even
    /// with cross-file fan-in, via the `is_test_path` column — a test helper
    /// called from many other tests should not rank as a top production
    /// symbol.
    #[test]
    fn symbols_section_excludes_test_file_symbols() {
        let path = temp_db_path("test-paths");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let test_file = insert_file(&conn, "src/util.test.ts");
        let real_file = insert_file(&conn, "src/core.ts");
        let caller_file = insert_file(&conn, "src/main.ts");

        let helper_id = insert_function(&conn, test_file, "testHelper");
        let core_id = insert_function(&conn, real_file, "core");
        let caller_id = insert_function(&conn, caller_file, "main");

        insert_call_edge(&conn, caller_file, test_file, caller_id, helper_id);
        insert_call_edge(&conn, caller_file, real_file, caller_id, core_id);

        let result = leaderboard(&conn, &HashSet::new(), None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert!(
            !entries.iter().any(|e| e["symbol"] == "testHelper"),
            "symbol defined in src/util.test.ts must be excluded: {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e["symbol"] == "core"),
            "non-test symbol core should remain: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A method carrying `owner_type` reports it as `owner`; a top-level
    /// function reports `owner: null`.
    #[test]
    fn symbols_section_reports_owner_type_when_present() {
        let path = temp_db_path("owner-type");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let bus_file = insert_file(&conn, "src/event_bus.ts");
        let util_file = insert_file(&conn, "src/util.ts");
        let caller_file = insert_file(&conn, "src/main.ts");

        // A method `on` owned by `EventBus`, and a free function `helper`.
        conn.execute(
            "INSERT INTO entities (kind, name, file_id, start_byte, end_byte, start_line, start_col, end_line, end_col, owner_type)
             VALUES (?1, 'on', ?2, 0, 0, 0, 0, 0, 0, 'EventBus')",
            rusqlite::params![EntityKind::Function.as_i64(), bus_file],
        )
        .expect("insert method entity");
        let on_id = conn.last_insert_rowid();
        let helper_id = insert_function(&conn, util_file, "helper");
        let caller_id = insert_function(&conn, caller_file, "main");
        insert_call_edge(&conn, caller_file, bus_file, caller_id, on_id);
        insert_call_edge(&conn, caller_file, util_file, caller_id, helper_id);

        let result = leaderboard(&conn, &HashSet::new(), None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        let on = entries
            .iter()
            .find(|e| e["symbol"] == "on")
            .expect("method on present");
        assert_eq!(
            on["owner"], "EventBus",
            "method owner must be reported: {entries:?}"
        );
        let helper = entries
            .iter()
            .find(|e| e["symbol"] == "helper")
            .expect("helper present");
        assert!(
            helper["owner"].is_null(),
            "top-level function owner must be null: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Leaderboard sorts descending by fan-in and respects `limit`.
    #[test]
    fn symbols_section_sorts_and_limits() {
        let path = temp_db_path("sort-limit");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let a_file = insert_file(&conn, "src/a.ts");
        let b_file = insert_file(&conn, "src/b.ts");
        let c_file = insert_file(&conn, "src/c.ts");
        let caller_file = insert_file(&conn, "src/callers.ts");

        let a_id = insert_function(&conn, a_file, "a");
        let b_id = insert_function(&conn, b_file, "b");
        let c_id = insert_function(&conn, c_file, "c");

        for i in 0..2 {
            let caller_id = insert_function(&conn, caller_file, &format!("caller_a_{i}"));
            insert_call_edge(&conn, caller_file, a_file, caller_id, a_id);
        }
        for i in 0..9 {
            let caller_id = insert_function(&conn, caller_file, &format!("caller_b_{i}"));
            insert_call_edge(&conn, caller_file, b_file, caller_id, b_id);
        }
        for i in 0..5 {
            let caller_id = insert_function(&conn, caller_file, &format!("caller_c_{i}"));
            insert_call_edge(&conn, caller_file, c_file, caller_id, c_id);
        }

        let result = leaderboard(&conn, &HashSet::new(), Some(2)).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert_eq!(
            entries.len(),
            2,
            "limit truncates to 2 entries: {entries:?}"
        );
        assert_eq!(entries[0]["symbol"], "b");
        assert_eq!(entries[1]["symbol"], "c");

        let _ = std::fs::remove_file(&path);
    }
}
