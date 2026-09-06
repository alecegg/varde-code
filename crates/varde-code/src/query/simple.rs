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

/// Render a persisted `entities.kind` integer back to its snake_case string.
fn entity_kind_of(v: i64) -> String {
    crate::model::EntityKind::from_i64(v)
        .map(crate::model::EntityKind::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// The `EntityKind` discriminants that represent a *named declaration* a caller
/// would look up as a "symbol" — class/method/interface/field/parameter.
/// Declarations live in the `entities` table, not `symbols` (which holds only
/// bindings/references), so the symbol query modes union these in. The
/// occurrence/relationship kinds (call, throw, import, extends, …) are excluded,
/// as is `Export`: an `export class Foo` already yields a `Class` entity, so
/// including `Export` would only add a duplicate `Foo` row.
fn declaration_kinds() -> [i64; 5] {
    use crate::model::EntityKind::{Class, Function, Interface, Parameter, Variable};
    [
        Function.as_i64(),
        Class.as_i64(),
        Interface.as_i64(),
        Variable.as_i64(),
        Parameter.as_i64(),
    ]
}

/// Comma-joined `declaration_kinds()` for inlining into a SQL `IN (...)` list.
/// The values are trusted enum discriminants (integers), so inlining them is
/// injection-safe and avoids threading a variable-length placeholder list.
fn declaration_kinds_sql() -> String {
    declaration_kinds()
        .iter()
        .map(|k| k.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Map a `kind` query param to an entity declaration discriminant, mirroring
/// the set exposed by [`declaration_kinds`]. `None` for names that aren't
/// declaration kinds (callers then fall back to the symbol-kind interpretation).
fn entity_kind_to_i64(s: &str) -> Option<i64> {
    use crate::model::EntityKind::{Class, Function, Interface, Parameter, Variable};
    Some(
        match s {
            "function" => Function,
            "class" => Class,
            "interface" => Interface,
            "variable" => Variable,
            "parameter" => Parameter,
            _ => return None,
        }
        .as_i64(),
    )
}

/// Whether a path language matches a `filter_symbols` language predicate.
///
/// TypeScript filters intentionally include TSX files. A TSX filter remains
/// specific to TSX, while every other accepted name maps directly to its
/// `SupportLang` value.
fn matches_language_filter(
    file_language: Option<ast_grep_language::SupportLang>,
    language_filter: &str,
) -> bool {
    use ast_grep_language::SupportLang;

    match crate::parse::language_from_name(language_filter) {
        Some(SupportLang::TypeScript) => matches!(
            file_language,
            Some(SupportLang::TypeScript) | Some(SupportLang::Tsx)
        ),
        Some(language) => file_language == Some(language),
        // Preserve the existing behavior for an unrecognized predicate. The
        // caller only promises language filtering for accepted language names.
        None => true,
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

/// Declaration entities (class/method/interface/field/parameter/export) for a
/// file, shaped identically to [`query_symbols`] rows but sourced from the
/// `entities` table with the `entities.kind` rendered via [`entity_kind_of`].
fn query_entity_declarations(
    conn: &Connection,
    file_id: i64,
    body_root: Option<&std::path::Path>,
) -> std::result::Result<Vec<serde_json::Value>, ApiError> {
    let sql = format!(
        "SELECT e.kind, e.name, f.path,
                e.start_byte, e.end_byte, e.start_line, e.start_col,
                e.end_line, e.end_col
         FROM entities e
         JOIN files f ON f.id = e.file_id
         WHERE e.file_id = ?1 AND e.kind IN ({})
         ORDER BY e.id",
        declaration_kinds_sql()
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
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
            kind: entity_kind_of(k),
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

/// A file's declared symbols: its declaration entities followed by its
/// binding/reference symbols. This is the "symbols declared in a file" view
/// callers expect — declarations (from `entities`) first, then the
/// binding/reference layer (from `symbols`).
fn query_file_symbols(
    conn: &Connection,
    file_id: i64,
    body_root: Option<&std::path::Path>,
) -> std::result::Result<Vec<serde_json::Value>, ApiError> {
    let mut out = query_entity_declarations(conn, file_id, body_root)?;
    out.extend(query_symbols(conn, file_id, body_root)?);
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
    let symbols = query_file_symbols(&conn, fid, body_root(input).as_deref())?;
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
            .and_then(|fid| query_file_symbols(&conn, fid, root.as_deref()))
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

    // A `kind` filter selects the source: a declaration kind (class/function/…)
    // searches only `entities`; binding/reference searches only `symbols`. With
    // no `kind`, search both — declarations first, since a bare name lookup
    // (`get_symbol StringUtils`) wants the declaration, not a use of it.
    let want_entities = kind.is_none_or(|k| entity_kind_to_i64(k).is_some());
    let want_symbols = kind.is_none_or(|k| symbol_kind_to_i64(k).is_some());

    let mut candidates: Vec<SymbolRow> = Vec::new();
    if want_entities {
        candidates.extend(rows_by_name(
            &conn,
            RowSource::Entities,
            name,
            kind.and_then(entity_kind_to_i64),
            file_path,
        )?);
    }
    if want_symbols {
        candidates.extend(rows_by_name(
            &conn,
            RowSource::Symbols,
            name,
            kind.and_then(symbol_kind_to_i64),
            file_path,
        )?);
    }

    let Some(row) = candidates.into_iter().next() else {
        return Err(ApiError::not_found(format!("symbol {name:?}")));
    };
    symbol_json(&row, body_root(input).as_deref())
}

/// Which persisted table a lookup reads from — `entities` (declarations) or
/// `symbols` (bindings/references). They share the same 9-column span shape and
/// a `kind`/`name`/`file_id`, differing only in table name and how `kind` is
/// rendered.
#[derive(Clone, Copy)]
enum RowSource {
    Entities,
    Symbols,
}

impl RowSource {
    fn table(self) -> &'static str {
        match self {
            RowSource::Entities => "entities",
            RowSource::Symbols => "symbols",
        }
    }

    fn render_kind(self, k: i64) -> String {
        match self {
            RowSource::Entities => entity_kind_of(k),
            RowSource::Symbols => symbol_kind_of(k),
        }
    }

    /// Default `kind IN (...)` restriction when no specific kind is requested.
    /// `entities` holds many non-declaration kinds, so it restricts to the
    /// declaration set; `symbols` holds only binding/reference, so it is open.
    fn default_kind_filter(self) -> String {
        match self {
            RowSource::Entities => format!(" AND t.kind IN ({})", declaration_kinds_sql()),
            RowSource::Symbols => String::new(),
        }
    }

    /// `ORDER BY` expression that ranks a genuine *definition* ahead of a
    /// weaker match with the same name: a type/function declaration beats a
    /// variable/parameter binding, and (for symbols) a `Binding` beats a
    /// `Reference` use-site. Without this a bare `get_symbol Foo` could return
    /// a local variable or a use of `Foo` rather than the type `Foo` itself.
    fn kind_priority_sql(self) -> String {
        use crate::model::{EntityKind, SymbolKind};
        match self {
            RowSource::Entities => format!(
                "CASE t.kind WHEN {class} THEN 0 WHEN {iface} THEN 0 WHEN {func} THEN 1 \
                 WHEN {var} THEN 2 WHEN {param} THEN 3 ELSE 4 END",
                class = EntityKind::Class.as_i64(),
                iface = EntityKind::Interface.as_i64(),
                func = EntityKind::Function.as_i64(),
                var = EntityKind::Variable.as_i64(),
                param = EntityKind::Parameter.as_i64(),
            ),
            RowSource::Symbols => format!(
                "CASE t.kind WHEN {binding} THEN 0 ELSE 1 END",
                binding = SymbolKind::Binding.as_i64(),
            ),
        }
    }
}

/// Rows named `name` from `source`, optionally restricted to a specific `kind`
/// discriminant and a `file_path` suffix (component-wise, via [`matches_path`]).
fn rows_by_name(
    conn: &Connection,
    source: RowSource,
    name: &str,
    kind_int: Option<i64>,
    file_path: Option<&str>,
) -> std::result::Result<Vec<SymbolRow>, ApiError> {
    let kind_clause = match kind_int {
        Some(_) => " AND t.kind = ?2".to_string(),
        None => format!("{} AND ?2 = ?2", source.default_kind_filter()),
    };
    let sql = format!(
        "SELECT t.kind, t.name, f.path,
                t.start_byte, t.end_byte, t.start_line, t.start_col,
                t.end_line, t.end_col
         FROM {} t JOIN files f ON f.id = t.file_id
         WHERE t.name = ?1{kind_clause}
         ORDER BY {}, t.id",
        source.table(),
        source.kind_priority_sql()
    );
    // `?2` is the kind discriminant when filtering; when not, the `?2 = ?2`
    // tautology keeps a stable two-parameter binding (value is irrelevant).
    let kind_param = kind_int.unwrap_or(0);
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(rusqlite::params![name, kind_param], |r| {
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
        if let Some(fp) = file_path
            && !matches_path(&path, fp)
        {
            continue;
        }
        out.push(SymbolRow {
            kind: source.render_kind(k),
            name: n,
            path,
            sb,
            eb,
            sl,
            sc,
            el,
            ec,
        });
    }
    Ok(out)
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
        // All test files, keyed off the authoritative `is_test_path` generated
        // column (see `db.rs`) so this agrees with every other consumer of
        // "is this a test file" instead of a weaker ad-hoc heuristic.
        let mut stmt = conn
            .prepare("SELECT f.id, f.path FROM files f WHERE f.is_test_path = 1")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(db_err)?;
        let mut candidates: Vec<(i64, String)> = Vec::new();
        for row in rows {
            let (id, path) = row.map_err(db_err)?;
            candidates.push((id, path));
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

    // A `kind` filter selects the source(s): a declaration kind reads
    // `entities`, binding/reference reads `symbols`, and no kind reads both.
    let entity_kind = kind.and_then(entity_kind_to_i64);
    let symbol_kind = kind.and_then(symbol_kind_to_i64);
    let want_entities = kind.is_none() || entity_kind.is_some();
    let want_symbols = kind.is_none() || symbol_kind.is_some();

    let mut rows: Vec<SymbolRow> = Vec::new();
    if want_entities {
        rows.extend(collect_rows(
            &conn,
            RowSource::Entities,
            entity_kind,
            specific_file_id,
            min_complexity,
        )?);
    }
    if want_symbols {
        rows.extend(collect_rows(
            &conn,
            RowSource::Symbols,
            symbol_kind,
            specific_file_id,
            min_complexity,
        )?);
    }

    let mut out = Vec::new();
    for row in rows {
        if let Some(lang) = language {
            let file_lang = crate::parse::language_for_path(std::path::Path::new(&row.path));
            if !matches_language_filter(file_lang, lang) {
                continue;
            }
        }
        out.push(symbol_json(&row, None)?);
        if let Some(cap) = max_symbols
            && out.len() as i64 >= cap
        {
            break;
        }
    }
    Ok(serde_json::json!(out))
}

/// Rows from `source` matching the `filter_symbols` predicates: an optional
/// specific `kind` discriminant (else the source's default kind restriction),
/// an optional `file_id`, and an optional minimum file complexity. Ordering is
/// by row id within the source, matching the pre-union behavior.
fn collect_rows(
    conn: &Connection,
    source: RowSource,
    kind_int: Option<i64>,
    specific_file_id: Option<i64>,
    min_complexity: Option<i64>,
) -> std::result::Result<Vec<SymbolRow>, ApiError> {
    let mut sql = format!(
        "SELECT t.kind, t.name, t.start_byte, t.end_byte, t.start_line, t.start_col, t.end_line, t.end_col, f.path
         FROM {} t JOIN files f ON f.id = t.file_id WHERE 1=1",
        source.table()
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    match kind_int {
        Some(k) => {
            sql.push_str(" AND t.kind = ?");
            params.push(Box::new(k));
        }
        None => sql.push_str(&source.default_kind_filter()),
    }
    if let Some(fid) = specific_file_id {
        sql.push_str(" AND t.file_id = ?");
        params.push(Box::new(fid));
    }
    if let Some(mc) = min_complexity {
        sql.push_str(" AND COALESCE(f.complexity, 0) >= ?");
        params.push(Box::new(mc));
    }
    sql.push_str(" ORDER BY t.id");

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
                ))
            },
        )
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (k, n, sb, eb, sl, sc, el, ec, path) = row.map_err(db_err)?;
        out.push(SymbolRow {
            kind: source.render_kind(k),
            name: n,
            path,
            sb,
            eb,
            sl,
            sc,
            el,
            ec,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod build_on_read_tests {
    use super::{
        filter_symbols, find_imports, get_symbol, matches_language_filter, symbols_in_file,
    };
    use ast_grep_language::SupportLang;

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

    /// Declarations (class/method/etc.) live in the `entities` table, not
    /// `symbols`. Regression test for the bug where `symbols_in_file`,
    /// `get_symbol`, and `filter_symbols` only read `symbols` and so returned
    /// zero declarations — only references. All three must now surface the
    /// declaration a caller expects.
    #[test]
    fn symbol_modes_return_declarations_not_just_references() {
        with_isolated_home("bor", "declarations", || {
            let root = temp_root("declarations");
            let file = root.join("a.ts");
            std::fs::write(
                &file,
                "export class Widget {\n  render() { return 1; }\n}\n",
            )
            .expect("write a.ts");

            let base = serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "filePath": file.to_str().unwrap(),
            });

            // symbols_in_file surfaces the class + method declarations, each
            // rendered with its entity kind (not "reference").
            let in_file = symbols_in_file(&base).expect("builds and answers");
            let arr = in_file.as_array().expect("array");
            assert!(
                arr.iter()
                    .any(|s| s["name"] == "Widget" && s["kind"] == "class"),
                "Widget class decl present: {in_file}"
            );
            assert!(
                arr.iter()
                    .any(|s| s["name"] == "render" && s["kind"] == "function"),
                "render method decl present: {in_file}"
            );

            // get_symbol returns the declaration for a bare name lookup.
            let got = get_symbol(&serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "name": "Widget",
            }))
            .expect("get_symbol finds the declaration");
            assert_eq!(got["kind"], "class");
            assert_eq!(got["name"], "Widget");

            // filter_symbols kind:"class" now matches the declaration (was 0).
            let classes = filter_symbols(&serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "kind": "class",
            }))
            .expect("filter_symbols answers");
            assert!(
                classes
                    .as_array()
                    .expect("array")
                    .iter()
                    .any(|s| s["name"] == "Widget"),
                "class filter surfaces Widget: {classes}"
            );

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn language_filters_match_every_supported_language() {
        let cases = [
            ("rust", SupportLang::Rust),
            ("typescript", SupportLang::TypeScript),
            ("ts", SupportLang::TypeScript),
            ("tsx", SupportLang::Tsx),
            ("javascript", SupportLang::JavaScript),
            ("js", SupportLang::JavaScript),
            ("c", SupportLang::C),
            ("cpp", SupportLang::Cpp),
            ("c++", SupportLang::Cpp),
            ("cxx", SupportLang::Cpp),
            ("go", SupportLang::Go),
            ("golang", SupportLang::Go),
            ("java", SupportLang::Java),
            ("csharp", SupportLang::CSharp),
            ("cs", SupportLang::CSharp),
            ("kotlin", SupportLang::Kotlin),
            ("kt", SupportLang::Kotlin),
            ("swift", SupportLang::Swift),
            ("python", SupportLang::Python),
            ("py", SupportLang::Python),
            ("ruby", SupportLang::Ruby),
            ("rb", SupportLang::Ruby),
            ("php", SupportLang::Php),
            ("lua", SupportLang::Lua),
            ("scala", SupportLang::Scala),
            ("dart", SupportLang::Dart),
            ("elixir", SupportLang::Elixir),
            ("ex", SupportLang::Elixir),
            ("solidity", SupportLang::Solidity),
            ("sol", SupportLang::Solidity),
            ("haskell", SupportLang::Haskell),
            ("hs", SupportLang::Haskell),
            ("bash", SupportLang::Bash),
            ("sh", SupportLang::Bash),
            ("shell", SupportLang::Bash),
        ];

        for (filter, language) in cases {
            assert!(
                matches_language_filter(Some(language), filter),
                "{filter} accepts its language"
            );
            assert!(
                !matches_language_filter(Some(SupportLang::Rust), filter)
                    || language == SupportLang::Rust,
                "{filter} excludes Rust when it targets another language"
            );
        }

        assert!(matches_language_filter(
            Some(SupportLang::Tsx),
            "typescript"
        ));
        assert!(matches_language_filter(Some(SupportLang::Tsx), "ts"));
        assert!(!matches_language_filter(
            Some(SupportLang::TypeScript),
            "tsx"
        ));
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

#[cfg(test)]
mod covering_tests_tests {
    use super::*;

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "varde-covering-tests-{label}-{}-{}.sqlite",
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

    fn insert_import(conn: &Connection, from_file_id: i64, to_file_id: i64) {
        conn.execute(
            "INSERT INTO resolved_edges (from_file_id, to_file_id, kind, resolved)
             VALUES (?1, ?2, ?3, 1)",
            rusqlite::params![
                from_file_id,
                to_file_id,
                crate::resolve::EdgeKind::Import.as_i64()
            ],
        )
        .expect("insert resolved import edge");
    }

    /// Coverage now keys off the authoritative `is_test_path` generated
    /// column, so it (a) recognizes test conventions the old ad-hoc
    /// `is_test_file` substring heuristic missed — e.g. `FooTest.java`, which
    /// has no `_test`/`test_`/`tests_` marker — and (b) no longer
    /// false-positives on production files like `latest_config.rs` (the old
    /// heuristic matched its `test_` substring). Both files import the target;
    /// only the real test file should be reported as covering it.
    #[test]
    fn covering_uses_authoritative_is_test_path_column() {
        let path = temp_db_path("is-test-path");
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let target = insert_file(&conn, "src/main/java/com/acme/Service.java");
        // Real test the old heuristic missed (no `_test`/`test_` marker).
        let java_test = insert_file(&conn, "src/test/java/com/acme/ServiceTest.java");
        // Production file the old heuristic wrongly treated as a test because
        // its path contains the `test_` substring ("la-test_-config").
        let false_positive = insert_file(&conn, "src/config/latest_config.rs");

        insert_import(&conn, java_test, target);
        insert_import(&conn, false_positive, target);

        let covering = covering_tests(&conn, target).expect("covering computes");

        assert_eq!(
            covering,
            vec!["src/test/java/com/acme/ServiceTest.java".to_string()],
            "only the real test file covers the target: {covering:?}"
        );

        let _ = std::fs::remove_file(&path);
    }
}
