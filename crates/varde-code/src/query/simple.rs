//! Simple lookup modes: symbols_in_file, get_symbol, tests_for_file,
//! find_imports, filter_symbols.
//!
//! All read the persisted schema; no in-memory structures. Shared contract:
//! a known file/symbol with no matches returns an empty array (not an
//! error); an unknown file/symbol returns `not_found`.

use anyhow::Result;
use rusqlite::Connection;

use crate::model::SymbolKind;

use super::{ApiError, db_err, freshen_for_mode, open_db, opt_str, req_str};

/// Render a persisted `symbols.kind` integer back to its snake_case string.
fn symbol_kind_of(v: i64) -> String {
    SymbolKind::from_i64(v)
        .map(SymbolKind::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// Parse a `kind` query param ("binding"/"reference") into the stored
/// integer discriminant, `None` for anything unrecognized (matches no rows).
fn symbol_kind_to_i64(s: &str) -> Option<i64> {
    match s {
        "binding" => Some(SymbolKind::Binding.as_i64()),
        "reference" => Some(SymbolKind::Reference.as_i64()),
        _ => None,
    }
}

/// `filePath` matching: exact path, or a path whose trailing components equal
/// the query (`a.rs` matches `/x/a.rs` but not `/x/ab.rs`).
pub fn matches_path(path: &str, query: &str) -> bool {
    path == query || path.ends_with(&format!("/{query}")) || path.ends_with(&format!("\\{query}"))
}

/// Resolve a filePath query to a persisted `files.id`.
///
/// Exact match wins; otherwise the lexicographically first file whose path
/// ends with the query (component-wise). None → `not_found`.
pub fn file_id(conn: &Connection, file_path: &str) -> std::result::Result<i64, ApiError> {
    use rusqlite::OptionalExtension;
    if let Some(id) = conn
        .query_row("SELECT id FROM files WHERE path = ?1", [file_path], |r| {
            r.get(0)
        })
        .optional()
        .map_err(db_err)?
    {
        return Ok(id);
    }
    conn.query_row(
        "SELECT id
         FROM files
         WHERE substr(path, -length(?1) - 1) = '/' || ?1
            OR substr(path, -length(?1) - 1) = char(92) || ?1
         ORDER BY path
         LIMIT 1",
        [file_path],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| ApiError::not_found(format!("file {file_path:?}")))
}

/// A persisted symbol row: kind/name/file plus its byte/line span.
struct SymbolRow {
    kind: String,
    name: String,
    path: String,
    sb: i64,
    eb: i64,
    sl: i64,
    sc: i64,
    el: i64,
    ec: i64,
}

/// One persisted symbol, shaped for the envelope. When `body_root` is set,
/// reads the symbol's byte span from the file on disk (root-joined with the
/// stored relative path) and attaches it as a `body` field; a read failure
/// (deleted/moved file, stale index) omits `body` rather than failing the
/// whole request.
fn symbol_json(
    row: &SymbolRow,
    body_root: Option<&std::path::Path>,
) -> std::result::Result<serde_json::Value, ApiError> {
    let mut json = serde_json::json!({
        "kind": row.kind,
        "name": row.name,
        "file": row.path,
        "span": {
            "start_byte": row.sb, "end_byte": row.eb,
            "start_line": row.sl, "start_col": row.sc,
            "end_line": row.el, "end_col": row.ec,
        },
    });
    if let Some(root) = body_root
        && let Some(body) = read_span(root, &row.path, row.sb, row.eb)
    {
        json["body"] = serde_json::json!(body);
    }
    Ok(json)
}

/// Read `[start_byte, end_byte)` from `root.join(rel_path)` as UTF-8 text.
/// Returns `None` on any IO/UTF-8/range failure so callers can degrade
/// gracefully instead of failing the whole query.
fn read_span(
    root: &std::path::Path,
    rel_path: &str,
    start_byte: i64,
    end_byte: i64,
) -> Option<String> {
    let bytes = std::fs::read(root.join(rel_path)).ok()?;
    let (start, end) = (
        usize::try_from(start_byte).ok()?,
        usize::try_from(end_byte).ok()?,
    );
    let slice = bytes.get(start..end)?;
    String::from_utf8(slice.to_vec()).ok()
}

fn query_symbols(
    conn: &Connection,
    file_id: i64,
    body_root: Option<&std::path::Path>,
) -> std::result::Result<Vec<serde_json::Value>, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT s.kind, s.name, f.path,
                    s.start_byte, s.end_byte, s.start_line, s.start_col,
                    s.end_line, s.end_col
             FROM symbols s
             JOIN files f ON f.id = s.file_id
             WHERE s.file_id = ?1
             ORDER BY s.id",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([file_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (k, n, path, sb, eb, sl, sc, el, ec) = row.map_err(db_err)?;
        let row = SymbolRow {
            kind: symbol_kind_of(k),
            name: n,
            path,
            sb,
            eb,
            sl,
            sc,
            el,
            ec,
        };
        out.push(symbol_json(&row, body_root)?);
    }
    Ok(out)
}

/// `includeBody=true` needs a filesystem root to resolve the stored
/// (repo-relative) file path against; `repoRoot` is that root, and also
/// covers the `dbPath`-only invocation by falling back to the db's parent.
fn body_root(input: &serde_json::Value) -> Option<std::path::PathBuf> {
    if !input
        .get("includeBody")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    if let Some(root) = input.get("repoRoot").and_then(|v| v.as_str()) {
        return Some(std::path::PathBuf::from(root));
    }
    input
        .get("dbPath")
        .and_then(|v| v.as_str())
        .and_then(|p| std::path::Path::new(p).parent())
        .map(std::path::Path::to_path_buf)
}

/// symbols_in_file — list symbols declared in a file.
///
/// Inputs: `filePath` (required), `includeBody` (optional; when true, reads
/// each symbol's byte span from disk and attaches it as `body` — best-effort,
/// omitted if the file can't be read). Output: array of symbol objects. Empty
/// array when the file exists with no symbols; `not_found` for an unknown
/// file.
///
/// Error model: all-or-nothing — any failure (unknown file, DB error) is an
/// outer `Err(ApiError)` that fails the whole call. This differs from the
/// batch [`symbols_in_files`], which reports per-file errors inline.
pub fn symbols_in_file(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let file_path = req_str(input, "filePath")?;
    freshen_for_mode("symbols_in_file", input)?;
    let conn = open_db(input)?;
    let fid = file_id(&conn, file_path)?;
    let symbols = query_symbols(&conn, fid, body_root(input).as_deref())?;
    Ok(serde_json::json!(symbols))
}

/// symbols_in_files — batch form of `symbols_in_file` over many files in one
/// call.
///
/// Inputs: `filePaths` (required array of strings), `includeBody` (optional;
/// see `symbols_in_file`). Output: an object mapping each requested path to
/// either its symbol array or `{"error": {code, message}}` — one file's
/// failure (e.g. unknown path) never fails the whole batch. Each file is
/// freshened individually, same as a per-file `symbols_in_file` call.
///
/// Error model: partial success (deliberately unlike [`symbols_in_file`]).
/// An outer `Err` is returned only for a whole-request failure (missing/
/// malformed `filePaths`, DB open). Per-file failures are NOT outer errors —
/// clients MUST inspect each entry for an `error` field rather than assuming
/// every value is a symbol array.
pub fn symbols_in_files(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let paths = input
        .get("filePaths")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::new("invalid_input", "missing array field \"filePaths\""))?;
    let file_paths = paths
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| ApiError::new("invalid_input", "filePaths entries must be strings"))
        })
        .collect::<std::result::Result<Vec<&str>, ApiError>>()?;

    for file_path in &file_paths {
        let mut per_file_input = input.clone();
        per_file_input["filePath"] = serde_json::json!(file_path);
        freshen_for_mode("symbols_in_file", &per_file_input)?;
    }

    let conn = open_db(input)?;
    let root = body_root(input);
    let mut result = serde_json::Map::new();
    for file_path in file_paths {
        let entry = match file_id(&conn, file_path)
            .and_then(|fid| query_symbols(&conn, fid, root.as_deref()))
        {
            Ok(symbols) => serde_json::json!(symbols),
            Err(e) => serde_json::json!({"error": {"code": e.code, "message": e.message}}),
        };
        result.insert(file_path.to_string(), entry);
    }
    Ok(serde_json::Value::Object(result))
}

/// get_symbol — get one symbol by name, optionally scoped by file/kind.
///
/// Inputs: `name` (required), `filePath` (optional), `kind` (optional),
/// `includeBody` (optional; see `symbols_in_file`).
/// Output: a single symbol object. `not_found` when no symbol matches.
pub fn get_symbol(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let file_path = opt_str(input, "filePath");
    freshen_for_mode("get_symbol", input)?;
    let conn = open_db(input)?;
    let name = req_str(input, "name")?;
    let kind = opt_str(input, "kind");

    // -1 never matches a stored kind (0/1), so an absent filter is a no-op —
    // same effect as the old `?2 = '' OR kind = ?2` string-sentinel pattern.
    let kind_param = kind.and_then(symbol_kind_to_i64).unwrap_or(-1);
    let kind_unfiltered = kind.is_none();

    let sql = "SELECT s.kind, s.name, f.path,
                s.start_byte, s.end_byte, s.start_line, s.start_col,
                s.end_line, s.end_col
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         WHERE s.name = ?1 AND (?2 = 1 OR s.kind = ?3)
         ORDER BY s.id";
    let mut stmt = conn.prepare(sql).map_err(db_err)?;
    let rows = stmt
        .query_map(rusqlite::params![name, kind_unfiltered, kind_param], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })
        .map_err(db_err)?;
    let mut candidates = Vec::new();
    for row in rows {
        let (k, n, path, sb, eb, sl, sc, el, ec) = row.map_err(db_err)?;
        if let Some(fp) = file_path
            && !matches_path(&path, fp)
        {
            continue;
        }
        candidates.push((k, n, path, sb, eb, sl, sc, el, ec));
    }
    let Some((k, n, path, sb, eb, sl, sc, el, ec)) = candidates.into_iter().next() else {
        return Err(ApiError::not_found(format!("symbol {name:?}")));
    };
    let row = SymbolRow {
        kind: symbol_kind_of(k),
        name: n,
        path,
        sb,
        eb,
        sl,
        sc,
        el,
        ec,
    };
    symbol_json(&row, body_root(input).as_deref())
}

/// A file is a test file when its path carries a test/spec marker:
/// `_test`/`test_` segments, `.spec`/`.test` extensions, a `tests/`
/// directory, or a `tests_` prefix.
fn is_test_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("_test")
        || lower.contains("test_")
        || lower.contains(".spec")
        || lower.contains(".test")
        || lower.contains("/tests/")
        || lower.contains("tests_")
}

/// tests_for_file — test files that (transitively) import the given file.
///
/// Inputs: `filePath` (required). Output: array of test file paths. A test
/// file is a file whose path carries a test/spec marker and which reaches the
/// target through resolved import edges. Empty array when no tests cover the
/// file; `not_found` for an unknown file.
pub fn tests_for_file(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("tests_for_file", input)?;
    let conn = open_db(input)?;
    let file_path = req_str(input, "filePath")?;
    let target = file_id(&conn, file_path)?;
    Ok(serde_json::json!(covering_tests(&conn, target)?))
}

/// Reusable snapshot backing [`covering_tests`]: the test-file candidate set
/// and the resolved import graph, loaded once. `context_pack` calls
/// `covering_tests` once per seed file — loading via [`TestCoverage::load`]
/// once up front and reusing it across seeds turns that from O(seeds *
/// repo_size) full-table rescans into a single O(repo_size) load plus a
/// cheap reverse-BFS per seed.
pub(crate) struct TestCoverage {
    candidates: Vec<(i64, String)>,
    importers: std::collections::HashMap<i64, Vec<i64>>,
}

impl TestCoverage {
    pub(crate) fn load(conn: &Connection) -> std::result::Result<Self, ApiError> {
        // All test files: path-marked files, with resolved import edges.
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path FROM files f
                 WHERE f.path LIKE '%test%' OR f.path LIKE '%spec%'",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(db_err)?;
        let mut candidates: Vec<(i64, String)> = Vec::new();
        for row in rows {
            let (id, path) = row.map_err(db_err)?;
            if is_test_file(&path) {
                candidates.push((id, path));
            }
        }

        // Resolved import graph: from -> set of to.
        let mut importers: std::collections::HashMap<i64, Vec<i64>> =
            std::collections::HashMap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT from_file_id, to_file_id FROM resolved_edges
                     WHERE kind = ?1 AND resolved = 1 AND to_file_id IS NOT NULL",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map([crate::resolve::EdgeKind::Import.as_i64()], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .map_err(db_err)?;
            for row in rows {
                let (from, to) = row.map_err(db_err)?;
                importers.entry(to).or_default().push(from);
            }
        }

        Ok(TestCoverage {
            candidates,
            importers,
        })
    }

    /// Test files that (transitively) import `target` through resolved
    /// import edges — `varde-code`'s only route to "what covers this?"
    /// without a doc corpus to search.
    pub(crate) fn covering(&self, target: i64) -> Vec<String> {
        // Reverse BFS: which files can reach `target` through import edges?
        let mut reach = std::collections::HashSet::new();
        let mut stack = vec![target];
        while let Some(node) = stack.pop() {
            if let Some(ins) = self.importers.get(&node) {
                for imp in ins {
                    if reach.insert(*imp) {
                        stack.push(*imp);
                    }
                }
            }
        }

        let mut covering: Vec<String> = self
            .candidates
            .iter()
            .filter(|(id, _)| reach.contains(id))
            .map(|(_, p)| p.clone())
            .collect();
        covering.sort();
        covering
    }
}

/// Test files that (transitively) import `target` through resolved import
/// edges. Single-target convenience wrapper around [`TestCoverage`] — used
/// by [`tests_for_file`] where only one target is ever queried. Callers that
/// need this for multiple targets against the same db (e.g. `context_pack`,
/// one call per seed file) should load a [`TestCoverage`] once and call
/// [`TestCoverage::covering`] per target instead of calling this repeatedly.
pub(crate) fn covering_tests(
    conn: &Connection,
    target: i64,
) -> std::result::Result<Vec<String>, ApiError> {
    Ok(TestCoverage::load(conn)?.covering(target))
}

/// find_imports — resolved import edges of a file.
///
/// Inputs: `filePath` (required). Output: array of `{to, resolved}` targets
/// for the file's import edges (unresolved edges included with
/// `resolved:false`). Empty array when the file has no imports; `not_found`
/// for an unknown file.
pub fn find_imports(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("find_imports", input)?;
    let conn = open_db(input)?;
    let file_path = req_str(input, "filePath")?;
    let fid = file_id(&conn, file_path)?;

    let mut stmt = conn
        .prepare(
            "SELECT e.to_file_id, e.resolved, f.path
             FROM resolved_edges e LEFT JOIN files f ON f.id = e.to_file_id
             WHERE e.kind = ?1 AND e.from_file_id = ?2 ORDER BY e.id",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            rusqlite::params![crate::resolve::EdgeKind::Import.as_i64(), fid],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (to_id, resolved, to_path) = row.map_err(db_err)?;
        out.push(serde_json::json!({
            "to": to_path.unwrap_or_default(),
            "resolved": resolved == 1,
            "to_file_id": to_id,
        }));
    }
    Ok(serde_json::json!(out))
}

/// filter_symbols — symbols filtered by the supported predicates.
///
/// Inputs (all optional): `kind` (binding|reference), `file` (path suffix),
/// `language` (derived from the file extension), `minCyclomaticComplexity`
/// (file-level), `maxSymbols` (cap). Output: array of symbol objects. Empty
/// array when nothing matches; `not_found` for an unknown `file`.
pub fn filter_symbols(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let file = opt_str(input, "file");
    freshen_for_mode("filter_symbols", input)?;
    let conn = open_db(input)?;
    let kind = opt_str(input, "kind");
    let language = opt_str(input, "language");
    let min_complexity = input
        .get("minCyclomaticComplexity")
        .and_then(|v| v.as_i64());
    let max_symbols = input.get("maxSymbols").and_then(|v| v.as_i64());

    // Resolve a specific file to its id when `file` is given. When it is not,
    // we deliberately skip the file filter entirely: an `IN (all file ids)`
    // clause is a no-op that also builds an unbounded prepared statement (one
    // placeholder per file), so we omit it rather than enumerate every file.
    let specific_file_id: Option<i64> = match file {
        Some(f) => Some(file_id(&conn, f)?),
        None => None,
    };

    let mut sql = String::from(
        "SELECT s.kind, s.name, s.start_byte, s.end_byte, s.start_line, s.start_col, s.end_line, s.end_col, f.path, f.complexity
         FROM symbols s JOIN files f ON f.id = s.file_id WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(k) = kind {
        sql.push_str(" AND s.kind = ?");
        params.push(Box::new(symbol_kind_to_i64(k).unwrap_or(-1)));
    }
    if let Some(fid) = specific_file_id {
        sql.push_str(" AND s.file_id = ?");
        params.push(Box::new(fid));
    }
    if let Some(mc) = min_complexity {
        sql.push_str(" AND COALESCE(f.complexity, 0) >= ?");
        params.push(Box::new(mc));
    }
    sql.push_str(" ORDER BY s.id");

    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p as &dyn rusqlite::ToSql)),
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, Option<i64>>(9)?,
                ))
            },
        )
        .map_err(db_err)?;

    let mut out = Vec::new();
    for row in rows {
        let (k, n, sb, eb, sl, sc, el, ec, path, _complexity) = row.map_err(db_err)?;
        if let Some(lang) = language {
            use ast_grep_language::SupportLang;
            let file_lang = crate::parse::language_for_path(std::path::Path::new(&path));
            let matches = match lang {
                "rust" => file_lang == Some(SupportLang::Rust),
                "typescript" | "ts" => {
                    matches!(
                        file_lang,
                        Some(SupportLang::TypeScript) | Some(SupportLang::Tsx)
                    )
                }
                "javascript" | "js" => file_lang == Some(SupportLang::JavaScript),
                "go" => file_lang == Some(SupportLang::Go),
                _ => true,
            };
            if !matches {
                continue;
            }
        }
        let symbol_row = SymbolRow {
            kind: symbol_kind_of(k),
            name: n,
            path,
            sb,
            eb,
            sl,
            sc,
            el,
            ec,
        };
        out.push(symbol_json(&symbol_row, None)?);
        if let Some(cap) = max_symbols
            && out.len() as i64 >= cap
        {
            break;
        }
    }
    Ok(serde_json::json!(out))
}

#[cfg(test)]
mod build_on_read_tests {
    use super::{filter_symbols, find_imports, get_symbol, symbols_in_file};

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "varde-bor-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp root creates");
        path
    }

    use crate::query::test_support::test_support::with_isolated_home;

    fn symbol_names(value: &serde_json::Value) -> Vec<String> {
        value
            .as_array()
            .expect("symbols_in_file returns an array")
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect()
    }

    /// The §0 build-on-read contract: a `symbols_in_file` call with a `repoRoot`
    /// (1) builds the index on first touch and (2) surfaces edits to the queried
    /// file WITHOUT any explicit rebuild — freshening only that file's slice.
    #[test]
    fn symbols_in_file_builds_on_miss_then_freshens_stale_file() {
        with_isolated_home("bor", "freshen", || {
            let root = temp_root("freshen");
            let file = root.join("a.ts");
            // Symbols are bindings/references; imports produce them.
            std::fs::write(&file, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");

            let input = serde_json::json!({
                "repoRoot": root.to_str().expect("utf-8 root"),
                "filePath": file.to_str().expect("utf-8 file"),
            });

            // First call: no index yet → build-on-miss, then answer.
            let first = symbols_in_file(&input).expect("first query builds and answers");
            let names = symbol_names(&first);
            assert!(names.iter().any(|n| n == "foo"), "foo present: {names:?}");
            assert!(
                !names.iter().any(|n| n == "bar"),
                "bar absent initially: {names:?}"
            );

            // Edit the file (size changes → classified stale) and re-query with NO
            // explicit rebuild.
            std::fs::write(
                &file,
                "import { foo } from \"./x\";\nimport { bar } from \"./y\";\nfoo();\nbar();\n",
            )
            .expect("rewrite a.ts");

            let second = symbols_in_file(&input).expect("second query freshens and answers");
            let names = symbol_names(&second);
            assert!(
                names.iter().any(|n| n == "bar"),
                "build-on-read surfaced bar without a manual rebuild: {names:?}"
            );

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// get_symbol is a raw-slice reader: with a repoRoot it builds on miss and
    /// surfaces edits to the queried file with no explicit rebuild.
    #[test]
    fn get_symbol_builds_on_miss_then_freshens() {
        with_isolated_home("bor", "get-symbol", || {
            let root = temp_root("get-symbol");
            let file = root.join("a.ts");
            std::fs::write(&file, "import { foo } from \"./x\";\n").expect("writes");

            let input = |name: &str| {
                serde_json::json!({
                    "repoRoot": root.to_str().unwrap(),
                    "filePath": file.to_str().unwrap(),
                    "name": name,
                })
            };

            let first = get_symbol(&input("foo")).expect("builds on miss and answers");
            assert_eq!(first["name"], "foo");

            std::fs::write(
                &file,
                "import { foo } from \"./x\";\nimport { bar } from \"./y\";\n",
            )
            .expect("rewrites");
            let second = get_symbol(&input("bar")).expect("freshens and answers");
            assert_eq!(second["name"], "bar");

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// filter_symbols reads `files.complexity`; build-on-read must rewrite it
    /// (the POC complexity gap) so a control-flow edit is reflected.
    #[test]
    fn filter_symbols_freshens_complexity() {
        with_isolated_home("bor", "filter-complexity", || {
            let root = temp_root("filter-complexity");
            let file = root.join("a.ts");
            // One `if` → complexity 2; the import produces `foo` symbols.
            std::fs::write(&file, "import { foo } from \"./x\";\nif (foo) { foo(); }\n")
                .expect("writes");

            let input = serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "file": file.to_str().unwrap(),
                "minCyclomaticComplexity": 2,
            });

            let first = filter_symbols(&input).expect("builds on miss and answers");
            assert!(
                !first.as_array().expect("array").is_empty(),
                "complexity 2 matches: {first}"
            );

            // Drop the `if` → complexity 1 → the minComplexity filter now excludes it.
            std::fs::write(&file, "import { foo } from \"./x\";\nfoo();\n").expect("rewrites");

            let second = filter_symbols(&input).expect("freshens and answers");
            assert!(
                second.as_array().expect("array").is_empty(),
                "complexity freshened down to 1: {second}"
            );

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// find_imports reads the imports slice: with a repoRoot it must reflect a
    /// newly-resolvable import without a manual rebuild — adding the target
    /// file makes a previously-unresolved import resolve on the next query.
    #[test]
    fn find_imports_freshens_when_target_file_appears() {
        with_isolated_home("bor", "find-imports", || {
            let root = temp_root("find-imports");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            // `./x` has no target yet → unresolved on first query.
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");

            let input = serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "filePath": a.to_str().unwrap(),
            });

            let first = find_imports(&input).expect("builds on miss and answers");
            let rows = first.as_array().expect("array");
            assert_eq!(rows.len(), 1, "one import edge: {first}");
            assert_eq!(
                rows[0]["resolved"].as_bool(),
                Some(false),
                "unresolved until x.ts exists: {first}"
            );

            // Add the target file; the import now resolves with no manual build.
            std::fs::write(&x, "export const foo = 1;\n").expect("write x.ts");

            let second = find_imports(&input).expect("freshens imports and answers");
            let rows = second.as_array().expect("array");
            assert_eq!(rows.len(), 1, "still one import edge: {second}");
            assert_eq!(
                rows[0]["resolved"].as_bool(),
                Some(true),
                "now resolved: {second}"
            );
            let to = rows[0]["to"].as_str().expect("to is a string");
            assert!(to.ends_with("x.ts"), "resolves to x.ts: {to}");

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }
}
