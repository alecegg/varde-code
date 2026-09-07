//! Query surface: varde-code's read-side access to the persisted SQLite
//! store (`sqlite-persistence`'s schema).
//!
//! 20 query modes, one function per mode, all sharing one JSON envelope
//! contract:
//!
//! - success: `{"ok": true, "data": <mode payload>}`
//! - failure: `{"ok": false, "error": {"code": "<stable machine-readable code>",
//!   "message": "<human-readable message>"}}`
//!
//! Every mode except `find_pattern` reads directly from the persisted schema;
//! nothing re-derives in-memory structures. This module owns the envelope and
//! the mode dispatcher; the mode implementations live in the submodules.

use anyhow::Result;
use rusqlite::Connection;

/// A stable machine-readable error code paired with a human message.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// NotFound error for a missing/unknown symbol or file.
    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new("not_found", format!("not found: {what}"))
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

/// Render a mode result as the JSON envelope string.
///
/// `Ok(data)` → `{"ok":true,"data":...}`; `Err(err)` →
/// `{"ok":false,"error":{"code":...,"message":...}}`. This is the single
/// serialization point every mode flows through.
pub fn render(result: Result<serde_json::Value, ApiError>) -> String {
    match result {
        Ok(data) => serde_json::json!({ "ok": true, "data": data }).to_string(),
        Err(err) => serde_json::json!({
            "ok": false,
            "error": { "code": err.code, "message": err.message }
        })
        .to_string(),
    }
}

/// Process-wide SQL statement trace hook, applied to every connection
/// `open_db` creates. Diagnostics/test support (e.g. verifying that graph
/// traversals issue a constant number of statements).
static TRACE_HOOK: std::sync::Mutex<Option<fn(&str)>> = std::sync::Mutex::new(None);

/// Install (or clear) the SQL statement trace hook applied by [`open_db`].
pub fn set_trace_hook(hook: Option<fn(&str)>) {
    *TRACE_HOOK.lock().expect("trace hook lock") = hook;
}

/// Open the query database, resolving `dbPath` (explicit) or `repoRoot`
/// (via `db::path::repo_db_path`) from the mode's input object.
///
/// Missing database → a not_found-style error so the CLI never panics on an
/// unpersisted repo.
pub fn open_db(input: &serde_json::Value) -> std::result::Result<Connection, ApiError> {
    let path = match input.get("dbPath").and_then(|v| v.as_str()) {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let root = input
                .get("repoRoot")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ApiError::new("invalid_input", "missing repoRoot or dbPath"))?;
            crate::db::path::repo_db_path(std::path::Path::new(root))
        }
    };
    let mut conn =
        Connection::open(&path).map_err(|e| ApiError::new("db_error", format!("{e}")))?;
    if let Some(hook) = *TRACE_HOOK.lock().expect("trace hook lock") {
        conn.trace(Some(hook));
    }
    Ok(conn)
}

/// Map a `rusqlite::Error` to the standard `db_error` [`ApiError`]. Shared by
/// every query submodule that talks to SQLite directly.
pub(crate) fn db_err(e: rusqlite::Error) -> ApiError {
    ApiError::new("db_error", e.to_string())
}

/// Required string field from a mode input object.
pub fn req_str<'a>(
    input: &'a serde_json::Value,
    field: &str,
) -> std::result::Result<&'a str, ApiError> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::new("invalid_input", format!("missing string field {field:?}")))
}

/// Optional string field from a mode input object.
pub fn opt_str<'a>(input: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    input.get(field).and_then(|v| v.as_str())
}

/// Every implemented query mode, in the canonical order the CLI registers
/// them. The coverage-parity harness requires its checklist to cover exactly
/// this set — an unlisted mode fails loudly.
pub const QUERY_MODES: [&str; 23] = [
    "batch",
    "symbols_in_file",
    "symbols_in_files",
    "get_symbol",
    "tests_for_file",
    "find_imports",
    "filter_symbols",
    "dependencies",
    "dependents",
    "blast_radius",
    "symbol_blast_radius",
    "type_hierarchy",
    "explore",
    "map_file",
    "map_symbol",
    "map_path",
    "detect_changes",
    "hotspots",
    "clusters",
    "find_pattern",
    "slice_state",
    "context_pack",
    "nav_map",
];

/// A scope resolver for one query mode: given the mode input, the scope to
/// freshen over, or `None` to skip freshening entirely.
type ScopeResolver = fn(&serde_json::Value) -> Option<crate::slice::Scope>;

/// One [`FRESHNESS`] row: mode name, the slices it reads, and how to resolve
/// the freshen scope from the mode input.
type FreshnessEntry = (&'static str, &'static [crate::slice::Slice], ScopeResolver);

/// Mode → slice freshness mapping, the single place a query mode's
/// build-on-read wiring lives (SLICED_FRESHNESS_PLAN.md §1.3). `run_mode`
/// consults this table once, before dispatching to the mode function; the
/// scope resolver returns `None` to skip freshening (the dbPath-only / no-
/// `repoRoot` case, and the never-freshened modes below).
///
/// `find_pattern` is deliberately absent — it is the "no slice, live parse"
/// mode and is explicitly exempted by the coverage-parity harness.
/// `detect_changes` and `slice_state` are present as no-op entries so a new
/// mode must consciously decide its freshness story (the harness requires a
/// table entry for every mode except `find_pattern`).
pub const FRESHNESS: &[FreshnessEntry] = &[
    // raw: per-file structural slice (symbols_in_file / get_symbol /
    // filter_symbols reparse only the requested file when a path is given).
    (
        "symbols_in_file",
        &[crate::slice::Slice::Raw],
        raw_file_scope,
    ),
    // symbols_in_files freshens each requested file individually (looping
    // `freshen_for_mode("symbols_in_file", ...)` per path inside the mode
    // function itself, since one Scope can only name one file) rather than
    // through this table's resolver — never_scope here is a no-op placeholder
    // so the coverage-parity harness sees every mode consciously wired.
    ("symbols_in_files", &[crate::slice::Slice::Raw], never_scope),
    (
        "get_symbol",
        &[crate::slice::Slice::Raw],
        raw_file_or_repo_scope,
    ),
    (
        "filter_symbols",
        &[crate::slice::Slice::Raw],
        raw_file_or_repo_scope,
    ),
    // raw + churn: hotspots scores complexity + churn over the whole repo.
    (
        "hotspots",
        &[crate::slice::Slice::Raw, crate::slice::Slice::Churn],
        repo_scope,
    ),
    // imports: resolved import edges (tests_for_file / find_imports filter to
    // Import kind only; resolution needs the whole file set).
    (
        "tests_for_file",
        &[crate::slice::Slice::Imports],
        repo_scope,
    ),
    ("find_imports", &[crate::slice::Slice::Imports], repo_scope),
    // edges: graph traversals load all resolved edges with no kind filter.
    ("dependencies", &[crate::slice::Slice::Edges], repo_scope),
    ("dependents", &[crate::slice::Slice::Edges], repo_scope),
    ("blast_radius", &[crate::slice::Slice::Edges], repo_scope),
    (
        "symbol_blast_radius",
        &[crate::slice::Slice::Edges],
        repo_scope,
    ),
    ("type_hierarchy", &[crate::slice::Slice::Edges], repo_scope),
    ("explore", &[crate::slice::Slice::Edges], repo_scope),
    ("map_symbol", &[crate::slice::Slice::Edges], repo_scope),
    ("map_path", &[crate::slice::Slice::Edges], repo_scope),
    // global: map_file reads community_id + fan.
    ("map_file", &[crate::slice::Slice::Global], repo_scope),
    // global: clusters reads the persisted communities/community_members
    // partition (+ resolved edges for cohesion).
    ("clusters", &[crate::slice::Slice::Global], repo_scope),
    // scan (rules engine): reads global (communities, clone bands) + churn.
    (
        "scan",
        &[crate::slice::Slice::Global, crate::slice::Slice::Churn],
        repo_scope,
    ),
    // edges + churn: context_pack seeds off files/entities, expands one hop
    // over resolved edges, and ranks with the same complexity+churn score
    // hotspots uses.
    (
        "context_pack",
        &[crate::slice::Slice::Edges, crate::slice::Slice::Churn],
        repo_scope,
    ),
    // Never freshened (deliberate): detect_changes diffs the persisted store
    // against current source, so freshening raw to disk first would zero out
    // the very diff it reports; slice_state is a diagnostic dump.
    ("detect_changes", &[], never_scope),
    ("slice_state", &[], never_scope),
    // batch has no slices of its own — each dispatched call freshens itself
    // via its own FRESHNESS entry when it runs.
    ("batch", &[], never_scope),
    // nav_map spans every other section's data source (raw entities for
    // entrypoints, edges for module_layers/symbols/flows, global for
    // subsystems, raw+churn for hotspots) — freshen the union.
    (
        "nav_map",
        &[
            crate::slice::Slice::Raw,
            crate::slice::Slice::Edges,
            crate::slice::Slice::Global,
            crate::slice::Slice::Churn,
        ],
        repo_scope,
    ),
];

/// `Scope::Repo` when a `repoRoot` is present; `None` otherwise (dbPath-only
/// call — nothing to freshen against).
fn repo_scope(input: &serde_json::Value) -> Option<crate::slice::Scope> {
    opt_str(input, "repoRoot").map(|_| crate::slice::Scope::Repo)
}

/// `Scope::File(path)` when a `repoRoot` is present; `None` otherwise.
fn raw_file_scope(input: &serde_json::Value) -> Option<crate::slice::Scope> {
    opt_str(input, "repoRoot")?;
    opt_str(input, "filePath").map(|p| crate::slice::Scope::File(p.to_string()))
}

/// `Scope::File(path)` when a path is given, else `Scope::Repo`, but only
/// when a `repoRoot` is present (dbPath-only calls skip freshening).
fn raw_file_or_repo_scope(input: &serde_json::Value) -> Option<crate::slice::Scope> {
    opt_str(input, "repoRoot")?;
    match opt_str(input, "filePath").or_else(|| opt_str(input, "file")) {
        Some(p) => Some(crate::slice::Scope::File(p.to_string())),
        None => Some(crate::slice::Scope::Repo),
    }
}

/// A never-freshening scope resolver (modes that read as-is by design).
fn never_scope(_input: &serde_json::Value) -> Option<crate::slice::Scope> {
    None
}

thread_local! {
    /// Within a [`batch`] call, the set of freshen plans (repo_root + slices +
    /// scope) already brought up to date this batch. `None` outside a batch, so
    /// standalone queries never memoize and behave exactly as before. Lets a
    /// batch of repeated same-scope modes (e.g. five graph traversals, all
    /// `Edges`@`Repo`) pay the repo-walk change-detection once instead of once
    /// per call — the batch is a single filesystem snapshot. Thread-local, so a
    /// mode that spawns worker threads is unaffected; `batch` dispatches its
    /// calls sequentially on the one thread that owns this memo.
    static BATCH_FRESHEN_MEMO: std::cell::RefCell<Option<std::collections::HashSet<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// RAII guard that enables batch freshen memoization for its lifetime and
/// clears it on drop (including on panic / early return). Batches never nest
/// (`batch` rejects a nested `batch` call), so a flat set/clear is sufficient.
///
/// The memo is an intra-batch dedup optimization only, not a transactional
/// record: it is discarded unconditionally on drop, so a batch that errors or
/// panics partway leaves nothing behind and a retry re-freshens every slice
/// from scratch. Freshening is idempotent, so this re-work is safe, just not
/// free. The memo never spans batches.
struct BatchFreshenScope;

/// Test-only counter of freshen runs that actually executed `ensure_fresh`
/// (memo misses). Racy against any other freshen-driving test, so the batch
/// memo test reads a before/after delta under [`crate::HOME_TEST_LOCK`].
#[cfg(test)]
pub(crate) static FRESHEN_RUN_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

impl BatchFreshenScope {
    fn enter() -> Self {
        BATCH_FRESHEN_MEMO.with(|m| *m.borrow_mut() = Some(std::collections::HashSet::new()));
        BatchFreshenScope
    }
}

impl Drop for BatchFreshenScope {
    fn drop(&mut self) {
        BATCH_FRESHEN_MEMO.with(|m| *m.borrow_mut() = None);
    }
}

/// Stable key for a freshen plan in the batch memo — `repo_root`, the slice
/// set (in table order), and the scope. Uses control bytes as separators so a
/// path can't collide with the structural delimiters.
fn freshen_memo_key(
    slices: &[crate::slice::Slice],
    repo_root: &str,
    scope: &crate::slice::Scope,
) -> String {
    let slice_part = slices
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let scope_part = match scope {
        crate::slice::Scope::File(p) => p.as_str(),
        crate::slice::Scope::Repo => "\u{2}repo",
    };
    format!("{repo_root}\u{1}{slice_part}\u{1}{scope_part}")
}

/// Freshen the slice(s) a query mode reads, per the [`FRESHNESS`] table.
///
/// No-op when the mode is absent (find_pattern: live parse), the resolver
/// returns `None` (dbPath-only call, or a never-freshened mode), or the
/// freshen itself is a no-op. Error mapping matches the old per-call-site
/// `build_error` envelope.
///
/// Inside a [`batch`], a plan already freshened earlier in the same batch is
/// skipped (see [`BATCH_FRESHEN_MEMO`]). The memo records a plan only on
/// *success*, so a failed freshen never suppresses a later same-plan call's
/// retry — that call re-attempts and surfaces its own error exactly as a
/// standalone call would.
pub fn freshen_for_mode(
    mode: &str,
    input: &serde_json::Value,
) -> std::result::Result<(), ApiError> {
    let Some((_, slices, resolve_scope)) = FRESHNESS.iter().find(|(m, _, _)| *m == mode) else {
        return Ok(());
    };
    let Some(repo_root) = opt_str(input, "repoRoot") else {
        return Ok(());
    };
    let Some(scope) = resolve_scope(input) else {
        return Ok(());
    };

    let key = freshen_memo_key(slices, repo_root, &scope);
    let already_fresh =
        BATCH_FRESHEN_MEMO.with(|m| m.borrow().as_ref().is_some_and(|set| set.contains(&key)));
    if already_fresh {
        return Ok(());
    }

    // Test-only counter of *actual* freshen runs (the expensive repo-walk
    // detection): a memo hit returns above without incrementing, so a batch
    // regression test can assert each distinct plan freshens exactly once.
    #[cfg(test)]
    FRESHEN_RUN_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    crate::slice::ensure_fresh(slices, repo_root, &scope)
        .map_err(|e| ApiError::new("build_error", format!("{e}")))?;

    BATCH_FRESHEN_MEMO.with(|m| {
        if let Some(set) = m.borrow_mut().as_mut() {
            set.insert(key);
        }
    });
    Ok(())
}

/// Run one query mode against a JSON input string, returning the envelope.
///
/// The mode is dispatched by name; unknown modes and malformed inputs render
/// as error envelopes (never a process-level failure).
pub fn run_mode(mode: &str, input: &str) -> String {
    let value: serde_json::Value = match serde_json::from_str(input) {
        Ok(v) => v,
        Err(e) => {
            return render(Err(ApiError::new(
                "invalid_input",
                format!("input is not valid JSON: {e}"),
            )));
        }
    };
    render(dispatch_mode(mode, &value))
}

/// Dispatch one mode by name against an already-parsed input object.
///
/// The single routing table shared by [`run_mode`] (top-level CLI/API entry)
/// and [`batch`] (per-call routing inside a batch request) — adding a mode
/// here wires it into both.
fn dispatch_mode(mode: &str, value: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    dispatch_mode_inner(mode, value).map(|mut data| {
        // Single output boundary for every mode (and, via `batch`, each of its
        // sub-calls): repo-relative paths (F5) + line-only spans (F6). Both
        // transforms are idempotent, so a `batch` payload seeing this twice —
        // once per sub-call with that call's own `repoRoot`, once for the
        // aggregate — is harmless.
        crate::query::output::postprocess(&mut data, value);
        data
    })
}

fn dispatch_mode_inner(
    mode: &str,
    value: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    match mode {
        "symbols_in_file" => crate::query::simple::symbols_in_file(value),
        "symbols_in_files" => crate::query::simple::symbols_in_files(value),
        "get_symbol" => crate::query::simple::get_symbol(value),
        "tests_for_file" => crate::query::simple::tests_for_file(value),
        "find_imports" => crate::query::simple::find_imports(value),
        "filter_symbols" => crate::query::simple::filter_symbols(value),
        "dependencies" => crate::query::graph::dependencies(value),
        "dependents" => crate::query::graph::dependents(value),
        "blast_radius" => crate::query::graph::blast_radius(value),
        "symbol_blast_radius" => crate::query::graph::symbol_blast_radius(value),
        "type_hierarchy" => crate::query::graph::type_hierarchy(value),
        "explore" => crate::query::graph::explore(value),
        "map_file" => crate::query::mapping::map_file(value),
        "map_symbol" => crate::query::mapping::map_symbol(value),
        "map_path" => crate::query::mapping::map_path(value),
        "detect_changes" => crate::query::mapping::detect_changes(value),
        "hotspots" => crate::query::mapping::hotspots(value),
        "clusters" => crate::query::mapping::clusters(value),
        "context_pack" => crate::query::mapping::context_pack(value),
        "nav_map" => crate::query::nav_map::nav_map(value),
        "find_pattern" => crate::query::find_pattern::find_pattern(value),
        "slice_state" => slice_state(value),
        "batch" => batch(value),
        other => Err(ApiError::new(
            "unknown_mode",
            format!("unknown query mode {other:?}"),
        )),
    }
}

/// `--why` style slice freshness dump for a repo (see [`crate::slice::dump_state`]).
fn slice_state(value: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    crate::slice::dump_state(value).map_err(|e| ApiError::new("db_error", format!("{e}")))
}

/// batch — run several query modes in one call, sharing one JSON envelope.
///
/// Inputs: `calls` (required array of `{mode, ...mode-specific fields}`
/// objects). Each call inherits the batch's `repoRoot`/`dbPath` when it
/// doesn't specify its own. `mode: "batch"` may not nest. Output: an array,
/// one entry per call in order, each `{"mode", "ok", "data"}` or
/// `{"mode", "ok": false, "error"}` — one call's failure never fails the
/// batch. Calls dispatch through the same [`dispatch_mode`] every mode function
/// calls its own `freshen_for_mode` from, but for the duration of the batch a
/// freshen plan (slices + scope) is brought up to date at most once: the batch
/// is a single filesystem snapshot, so five graph traversals pay the
/// repo-walk change-detection once, not five times (see [`BatchFreshenScope`]
/// / [`freshen_for_mode`]).
fn batch(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let calls = input
        .get("calls")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::new("invalid_input", "missing array field \"calls\""))?;

    // Memoize freshen plans across this batch's calls (cleared on drop).
    let _freshen_scope = BatchFreshenScope::enter();

    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(mode) = call.get("mode").and_then(|v| v.as_str()) else {
            results.push(serde_json::json!({
                "ok": false,
                "error": {"code": "invalid_input", "message": "batch call missing string field \"mode\""},
            }));
            continue;
        };
        if mode == "batch" {
            results.push(serde_json::json!({
                "mode": mode, "ok": false,
                "error": {"code": "invalid_input", "message": "batch calls cannot nest \"batch\""},
            }));
            continue;
        }
        let merged = merge_repo_context(input, call);
        results.push(match dispatch_mode(mode, &merged) {
            Ok(data) => serde_json::json!({"mode": mode, "ok": true, "data": data}),
            Err(e) => serde_json::json!({"mode": mode, "ok": false, "error": {"code": e.code, "message": e.message}}),
        });
    }
    Ok(serde_json::Value::Array(results))
}

/// A batch call's own `repoRoot`/`dbPath` wins; otherwise it inherits the
/// batch request's, so callers don't have to repeat it on every call.
fn merge_repo_context(input: &serde_json::Value, call: &serde_json::Value) -> serde_json::Value {
    let mut merged = call.clone();
    if merged.get("repoRoot").is_none() && merged.get("dbPath").is_none() {
        if let Some(root) = input.get("repoRoot") {
            merged["repoRoot"] = root.clone();
        } else if let Some(db) = input.get("dbPath") {
            merged["dbPath"] = db.clone();
        }
    }
    merged
}

// Mode submodules (implemented by subsequent tasks).
pub mod entrypoints;
pub mod find_pattern;
pub mod flows;
pub mod foundational_files;
pub mod graph;
pub mod mapping;
pub mod module_layers;
pub mod nav_map;
pub mod noise_filter;
pub mod output;
pub mod simple;
pub mod subsystems;
pub mod symbols_section;
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod batch_freshen_tests {
    use super::*;
    use crate::query::test_support::test_support::with_isolated_home;
    use std::sync::atomic::Ordering;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "varde-query-batch-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp root creates");
        path
    }

    /// A batch freshens each distinct plan (slices + scope) exactly once, not
    /// once per call: three `Edges`@`Repo` graph calls + one `Raw`@`File` call
    /// must run `ensure_fresh` twice total. Contrast: the same calls dispatched
    /// standalone freshen once each.
    #[test]
    fn batch_freshens_each_plan_once() {
        with_isolated_home("query-batch", "freshen-once", || {
            let root = temp_root("freshen-once");
            std::fs::write(root.join("a.py"), "import b\n\ndef fa():\n    return 1\n")
                .expect("write a.py");
            std::fs::write(root.join("b.py"), "def fb():\n    return 2\n").expect("write b.py");
            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");

            let rr = root.to_str().unwrap();
            let a = root.join("a.py").to_str().unwrap().to_string();
            let b = root.join("b.py").to_str().unwrap().to_string();
            let calls = serde_json::json!({
                "repoRoot": rr,
                "calls": [
                    {"mode": "blast_radius", "filePath": a},        // Edges@Repo
                    {"mode": "dependents", "filePath": b},          // Edges@Repo (memoized)
                    {"mode": "symbols_in_file", "filePath": a},     // Raw@File(a) — distinct plan
                    {"mode": "blast_radius", "filePath": b},        // Edges@Repo (memoized)
                ],
            });

            let before = FRESHEN_RUN_CALLS.load(Ordering::Relaxed);
            let out = batch(&calls).expect("batch runs");
            let batch_runs = FRESHEN_RUN_CALLS.load(Ordering::Relaxed) - before;
            assert_eq!(
                batch_runs, 2,
                "batch must freshen the two distinct plans (Edges@Repo, Raw@File) once each"
            );
            assert_eq!(
                out.as_array().map(Vec::len),
                Some(4),
                "all four calls dispatched"
            );
            assert!(
                out.as_array()
                    .unwrap()
                    .iter()
                    .all(|r| r["ok"].as_bool() == Some(true)),
                "every call ok: {out}"
            );

            // Standalone (no batch memo): each call freshens on its own.
            let before = FRESHEN_RUN_CALLS.load(Ordering::Relaxed);
            for call in calls["calls"].as_array().unwrap() {
                let mut merged = call.clone();
                merged["repoRoot"] = serde_json::json!(rr);
                let _ = dispatch_mode(call["mode"].as_str().unwrap(), &merged);
            }
            let standalone_runs = FRESHEN_RUN_CALLS.load(Ordering::Relaxed) - before;
            assert_eq!(
                standalone_runs, 4,
                "standalone dispatch freshens once per call (no memo)"
            );

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }
}
