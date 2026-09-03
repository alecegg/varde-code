//! Relational-context correlation: look up the persisted entity whose span
//! encloses a pattern match's file+byte-span.
//!
//! Pattern rules get relational context (e.g. "is this match inside an async
//! function") from persisted `entities` rows rather than a live tree-walk —
//! the enclosing function/method row's structural-fact columns (`is_async`,
//! future additions) are read from the DB. The narrowest containing row wins
//! so nested functions/closures resolve to their innermost ancestor.

use crate::model::EntityKind;
use crate::query::ApiError;
use rusqlite::Connection;

/// The structural facts read from the enclosing function/method entity row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnclosingFact {
    /// The enclosing entity's name.
    pub name: String,
    /// The `is_async` structural fact from that row.
    pub is_async: Option<bool>,
}

/// A reusable enclosing-entity lookup: the SQL statement is prepared once
/// and executed per match, so a scan with hundreds of matches pays one
/// compile instead of one per lookup (the pattern-rule perf budget).
pub struct EnclosingLookup<'conn> {
    stmt: rusqlite::Statement<'conn>,
}

impl<'conn> EnclosingLookup<'conn> {
    pub fn new(conn: &'conn Connection) -> Result<Self, ApiError> {
        let stmt = conn
            .prepare(
                "SELECT e.name, e.is_async
                 FROM entities e
                 JOIN files f ON f.id = e.file_id
                 WHERE f.path = ?1
                   AND e.kind = ?2
                   AND e.start_byte <= ?3
                   AND e.end_byte >= ?4
                 ORDER BY (e.end_byte - e.start_byte) ASC
                 LIMIT 1",
            )
            .map_err(|e| ApiError::new("db_error", format!("{e}")))?;
        Ok(Self { stmt })
    }

    /// Look up the narrowest persisted function/method entity whose span
    /// contains `[start_byte, end_byte]` in `file_path`.
    ///
    /// `file_path` must match the normalized path form `persist` wrote (the
    /// scan-produced path string — absolute when the repo root was absolute)
    /// or the lookup silently returns `None`. A file with no rows, or no
    /// containing function row, yields `Ok(None)` — never a panic.
    pub fn lookup(
        &mut self,
        file_path: &str,
        start_byte: u32,
        end_byte: u32,
    ) -> Result<Option<EnclosingFact>, ApiError> {
        let function_kind = EntityKind::Function.as_i64();
        let mut rows = self
            .stmt
            .query_map(
                rusqlite::params![file_path, function_kind, start_byte as i64, end_byte as i64],
                |row| {
                    Ok(EnclosingFact {
                        name: row.get(0)?,
                        is_async: row.get(1)?,
                    })
                },
            )
            .map_err(|e| ApiError::new("db_error", format!("{e}")))?;

        match rows.next() {
            Some(Ok(fact)) => Ok(Some(fact)),
            Some(Err(e)) => Err(ApiError::new("db_error", format!("{e}"))),
            None => Ok(None),
        }
    }
}

/// Caches `files.is_test_path`/`files.is_tooling_path` by path so a pattern
/// rule with hundreds of matches in the same handful of files pays one
/// query per distinct path, not one per match.
pub struct TestPathLookup<'conn> {
    conn: &'conn Connection,
    cache: std::collections::HashMap<String, (bool, bool)>,
}

impl<'conn> TestPathLookup<'conn> {
    pub fn new(conn: &'conn Connection) -> Self {
        Self {
            conn,
            cache: std::collections::HashMap::new(),
        }
    }

    fn lookup(&mut self, file_path: &str) -> (bool, bool) {
        if let Some(cached) = self.cache.get(file_path) {
            return *cached;
        }
        let flags: (bool, bool) = self
            .conn
            .query_row(
                "SELECT is_test_path, is_tooling_path FROM files WHERE path = ?1",
                [file_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|err| {
                // Absent-row is the expected case for an unpersisted path
                // (treated as not-a-test-path per this method's contract);
                // any other error means the DB read itself is broken, and
                // suppression silently turning *off* is the only symptom —
                // warn so an operator has a signal to go on.
                if !matches!(err, rusqlite::Error::QueryReturnedNoRows) {
                    tracing::warn!(%err, file = %file_path, "failed to read is_test_path/is_tooling_path; treating as neither");
                }
                (false, false)
            });
        self.cache.insert(file_path.to_string(), flags);
        flags
    }

    /// Whether `file_path` is under a test/fixture convention, per the
    /// `files.is_test_path` generated column. An unpersisted path (no
    /// matching `files` row) is treated as not-a-test-path.
    pub fn is_test_path(&mut self, file_path: &str) -> Result<bool, ApiError> {
        Ok(self.lookup(file_path).0)
    }

    /// Whether `file_path` is non-shipped tooling (benchmarks, dev/build
    /// scripts, docs generators), per the `files.is_tooling_path` generated
    /// column. An unpersisted path is treated as not-tooling.
    pub fn is_tooling_path(&mut self, file_path: &str) -> Result<bool, ApiError> {
        Ok(self.lookup(file_path).1)
    }
}

/// One-shot convenience wrapper over [`EnclosingLookup`] (prepares a fresh
/// statement per call — prefer the lookup when scanning many matches).
pub fn enclosing_function_entity(
    conn: &Connection,
    file_path: &str,
    start_byte: u32,
    end_byte: u32,
) -> Result<Option<EnclosingFact>, ApiError> {
    EnclosingLookup::new(conn)?.lookup(file_path, start_byte, end_byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(tag: &str) -> (std::path::PathBuf, rusqlite::Connection) {
        let dir =
            std::env::temp_dir().join(format!("varde-correlate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("index.db");
        let conn = crate::db::open_or_rebuild(&db_path).expect("db opens");
        (db_path, conn)
    }

    /// Insert one file (once) + one function entity row.
    fn insert_function(
        conn: &Connection,
        file: &str,
        name: &str,
        start: i64,
        end: i64,
        is_async: Option<bool>,
    ) {
        let file_id: i64 =
            match conn.query_row("SELECT id FROM files WHERE path = ?1", [file], |r| r.get(0)) {
                Ok(id) => id,
                Err(_) => {
                    conn.execute("INSERT INTO files (path) VALUES (?1)", [file])
                        .expect("file row inserts");
                    conn.last_insert_rowid()
                }
            };
        conn.execute(
            "INSERT INTO entities (kind, name, file_id, start_byte, end_byte, start_line, start_col, end_line, end_col, is_async)
             VALUES (0, ?1, ?2, ?3, ?4, 1, 0, 1, 0, ?5)",
            rusqlite::params![name, file_id, start, end, is_async],
        )
        .expect("entity row inserts");
    }

    #[test]
    fn enclosing_async_function_reports_is_async_true() {
        let (dir, conn) = temp_db("async");
        insert_function(&conn, "a.rs", "outer", 0, 100, Some(true));
        let fact = enclosing_function_entity(&conn, "a.rs", 50, 55)
            .expect("lookup succeeds")
            .expect("enclosing row found");
        assert_eq!(fact.name, "outer");
        assert_eq!(fact.is_async, Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enclosing_sync_function_reports_is_async_false() {
        let (dir, conn) = temp_db("sync");
        insert_function(&conn, "a.rs", "render", 10, 40, Some(false));
        let fact = enclosing_function_entity(&conn, "a.rs", 20, 25)
            .expect("lookup succeeds")
            .expect("enclosing row found");
        assert_eq!(fact.name, "render");
        assert_eq!(fact.is_async, Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn span_outside_any_function_yields_none() {
        let (dir, conn) = temp_db("outside");
        insert_function(&conn, "a.rs", "render", 10, 40, Some(false));
        assert_eq!(
            enclosing_function_entity(&conn, "a.rs", 200, 210).expect("lookup succeeds"),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_functions_resolve_to_narrowest_entity() {
        let (dir, conn) = temp_db("nested");
        insert_function(&conn, "a.rs", "outer", 0, 100, Some(true));
        insert_function(&conn, "a.rs", "inner", 40, 60, Some(false));
        let fact = enclosing_function_entity(&conn, "a.rs", 50, 55)
            .expect("lookup succeeds")
            .expect("enclosing row found");
        assert_eq!(fact.name, "inner", "innermost enclosing entity wins");
        assert_eq!(fact.is_async, Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absent_file_yields_none_without_panicking() {
        let (dir, conn) = temp_db("absent");
        insert_function(&conn, "a.rs", "render", 10, 40, Some(false));
        assert_eq!(
            enclosing_function_entity(&conn, "missing.rs", 20, 25).expect("lookup succeeds"),
            None,
            "a file with no rows fails cleanly, not a panic"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
