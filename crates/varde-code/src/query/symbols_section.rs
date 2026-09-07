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
//! Ranking is by **caller breadth** (distinct caller files), not raw
//! call-edge count, and low-orientation accessor/stdlib names are dropped
//! (audit F8 — see [`leaderboard`] and [`is_low_orientation_symbol`]). Wired
//! into the nav-map `symbols` section (`query::nav_map`).

use std::collections::HashSet;

use rusqlite::Connection;

use crate::model::EntityKind;
use crate::resolve::EdgeKind;

#[cfg(test)]
use super::entrypoints;
use super::noise_filter::{is_frontend_asset_path, is_generated_or_vendored_path};
use super::{ApiError, db_err};

/// Compute the cross-file symbol fan-in leaderboard.
///
/// Reads `Function`/`Class` entities and ranks them by **caller breadth** —
/// the number of *distinct files* that call them across a file boundary
/// (audit F8), not the raw call-edge count. Breadth is a far better proxy for
/// architectural centrality: an accessor called 50× from one module scores 1,
/// while a core interface touched from 8 files scores 8. Raw edge count is
/// kept only as a deterministic tie-breaker.
///
/// Excludes symbols defined in test files (via the authoritative
/// `is_test_path` generated column, matching [`entrypoints::detect`] and the
/// foundational-files leaderboard), drops any path
/// [`is_generated_or_vendored_path`] flags as generated/vendored, drops any
/// entity also present in [`entrypoints::detect`]'s output, and drops
/// low-orientation names ([`is_low_orientation_symbol`] — accessors and
/// stdlib/framework boilerplate like `setName`/`push`/`ConfigureAwait` that
/// rank high but teach nothing about the repo's architecture). Sorts
/// descending by breadth (ties broken by raw count, then path, then symbol
/// name for determinism) and caps the result at `limit` entries (`None` =
/// unbounded). Each entry is `{"file", "symbol", "owner", "callers"}`, where
/// `callers` is the distinct-caller-file breadth and `owner` is the method's
/// owning type (from `entities.owner_type`) or `null` for a module/top-level
/// function.
pub fn leaderboard(
    conn: &Connection,
    entrypoint_ids: &HashSet<i64>,
    limit: Option<usize>,
) -> Result<serde_json::Value, ApiError> {
    let call_kind = EdgeKind::Call.as_i64();
    let mut stmt = conn
        .prepare(
            "SELECT e.id, f.path, e.name, e.owner_type,
                    COUNT(DISTINCT re.from_file_id) AS breadth,
                    COUNT(re.id) AS total
             FROM entities e
             JOIN files f ON f.id = e.file_id
             JOIN resolved_edges re
                 ON re.to_entity_id = e.id
                AND re.resolved = 1
                AND re.kind = ?1
                AND re.from_file_id != e.file_id
             WHERE e.kind IN (?2, ?3) AND f.is_test_path = 0
             GROUP BY e.id
             HAVING COUNT(DISTINCT re.from_file_id) > 0
             ORDER BY breadth DESC, total DESC, f.path ASC, e.name ASC",
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
        let (entity_id, path, name, owner_type, breadth) = row.map_err(db_err)?;
        if is_generated_or_vendored_path(&path) || is_frontend_asset_path(&path) {
            continue;
        }
        if entrypoint_ids.contains(&entity_id) {
            continue;
        }
        if is_low_orientation_symbol(&name) {
            continue;
        }
        // `owner` disambiguates common method names (`on` -> owner `EventBus`);
        // `null` for module/top-level functions and for languages that do not
        // yet record `owner_type`.
        entries.push(serde_json::json!({
            "file": path,
            "symbol": name,
            "owner": owner_type,
            "callers": breadth,
        }));
    }
    if let Some(limit) = limit {
        entries.truncate(limit);
    }
    Ok(serde_json::json!(entries))
}

/// Accessor / stdlib / framework names that top a raw fan-in board but carry
/// ~zero orientation value — an agent learns nothing about a repo's
/// architecture from `setName` or `push` (audit F8). Two matchers:
///
/// - **Accessors**: the bare forms `get`/`set`/`is`/`has`, and the
///   `getName`/`setValue`/`isReady`/`has_next` prefix pattern (prefix followed
///   by an uppercase letter or `_`, so `issue`/`hash`/`setup` are *not*
///   caught).
/// - **Stopwords**: a curated set of container/stdlib method and
///   language-sentinel names (case-insensitive), drawn from the names actually
///   observed topping the leaderboard across the audit corpus.
fn is_low_orientation_symbol(name: &str) -> bool {
    if is_accessor(name) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    LOW_ORIENTATION_NAMES.contains(&lower.as_str())
}

/// `get`/`set`/`is`/`has` accessor detection — see [`is_low_orientation_symbol`].
/// Shared with [`super::foundational_files`]'s data-class heuristic (audit F9).
pub(crate) fn is_accessor(name: &str) -> bool {
    for prefix in ["get", "set", "is", "has"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            match rest.chars().next() {
                None => return true, // bare `get`/`set`/`is`/`has`
                Some(c) if c == '_' || c.is_ascii_uppercase() => return true,
                _ => {}
            }
        }
    }
    false
}

/// Case-insensitive stopword set for [`is_low_orientation_symbol`]. Kept
/// deliberately focused on unambiguous container/stdlib methods and language
/// sentinels (Rust trait methods, C++/Ruby/JS container ops, C# awaitable and
/// constant boilerplate) rather than domain-plausible verbs like
/// `add`/`find`/`run`, which can legitimately be a repo's core interface.
const LOW_ORIENTATION_NAMES: &[&str] = &[
    "new",
    "push",
    "pop",
    "size",
    "len",
    "length",
    "begin",
    "end",
    "clone",
    "merge",
    "dig",
    "unwrap",
    "default",
    "hash",
    "into",
    "from",
    "as_str",
    "as_ref",
    "as_mut",
    "to_string",
    "tostring",
    "configureawait",
    "true",
    "false",
    "none",
    "null",
    "notnull",
    "nil",
];

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

    /// Ranking is by caller *breadth* (distinct caller files), not raw call
    /// count: a symbol called 3× from 3 different files outranks one called
    /// 10× from a single file (audit F8).
    #[test]
    fn symbols_section_ranks_by_caller_breadth_not_raw_count() {
        let path = temp_db_path("breadth-rank");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let wide_file = insert_file(&conn, "src/wide.ts");
        let hot_file = insert_file(&conn, "src/hot.ts");
        let wide_id = insert_function(&conn, wide_file, "wideUse");
        let hot_id = insert_function(&conn, hot_file, "hotLoop");

        // wideUse: one call from each of 3 distinct caller files -> breadth 3.
        for i in 0..3 {
            let caller_file = insert_file(&conn, &format!("src/caller_w{i}.ts"));
            let caller_id = insert_function(&conn, caller_file, &format!("cw{i}"));
            insert_call_edge(&conn, caller_file, wide_file, caller_id, wide_id);
        }
        // hotLoop: 10 calls, all from a single caller file -> breadth 1.
        let hot_caller = insert_file(&conn, "src/hot_caller.ts");
        for i in 0..10 {
            let caller_id = insert_function(&conn, hot_caller, &format!("ch{i}"));
            insert_call_edge(&conn, hot_caller, hot_file, caller_id, hot_id);
        }

        let result = leaderboard(&conn, &HashSet::new(), None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert_eq!(
            entries[0]["symbol"], "wideUse",
            "breadth (3 caller files) must outrank raw count (10 calls, 1 file): {entries:?}"
        );
        assert_eq!(entries[0]["callers"], 3);
        let hot = entries
            .iter()
            .find(|e| e["symbol"] == "hotLoop")
            .expect("hotLoop present");
        assert_eq!(
            hot["callers"], 1,
            "single-caller-file breadth is 1: {hot:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Accessor and stdlib/framework boilerplate names are dropped from the
    /// leaderboard even with real cross-file breadth; a domain symbol is kept
    /// (audit F8).
    #[test]
    fn symbols_section_excludes_low_orientation_names() {
        let path = temp_db_path("low-orientation");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let target_file = insert_file(&conn, "src/model.ts");
        let setter_id = insert_function(&conn, target_file, "setName"); // accessor
        let push_id = insert_function(&conn, target_file, "push"); // stopword
        let domain_id = insert_function(&conn, target_file, "reconcileLedger"); // kept

        // Give each real cross-file breadth so only the name filter can drop them.
        for (i, callee) in [setter_id, push_id, domain_id].into_iter().enumerate() {
            let caller_file = insert_file(&conn, &format!("src/c{i}.ts"));
            let caller_id = insert_function(&conn, caller_file, &format!("caller{i}"));
            insert_call_edge(&conn, caller_file, target_file, caller_id, callee);
        }

        let result = leaderboard(&conn, &HashSet::new(), None).expect("leaderboard computes");
        let entries = result.as_array().expect("leaderboard is an array");

        assert!(
            !entries.iter().any(|e| e["symbol"] == "setName"),
            "accessor setName must be excluded: {entries:?}"
        );
        assert!(
            !entries.iter().any(|e| e["symbol"] == "push"),
            "stdlib name push must be excluded: {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e["symbol"] == "reconcileLedger"),
            "domain symbol reconcileLedger must remain: {entries:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Accessor detection: prefixes only fire on `getX`/`is_x`/bare forms, not
    /// on words that merely start with those letters.
    #[test]
    fn accessor_detection_is_precise() {
        for yes in [
            "get", "set", "is", "has", "getName", "setValue", "is_ready", "hasNext",
        ] {
            assert!(is_accessor(yes), "{yes} should be an accessor");
        }
        for no in ["issue", "hash", "setup", "getaway", "reconcile", "index"] {
            assert!(!is_accessor(no), "{no} should NOT be an accessor");
        }
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
