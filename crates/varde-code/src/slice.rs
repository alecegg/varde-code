//! Per-tool sliced freshness (build-on-read).
//!
//! [`ensure_fresh`] brings the slice(s) a query mode reads up to date before
//! the mode answers, rebuilding only what changed (proportional to the delta)
//! and only for the requested slice. `build` (the command) stays the
//! always-everything-fresh operation; this module is what lets individual
//! tools answer fresh without paying for a full build.

use anyhow::Result;
use std::path::Path;

use crate::build::{FileClassification, classify_files};
use crate::persist::StoredFileState;

/// The five slices, nested `raw ⊂ imports ⊂ edges ⊂ global` plus an
/// orthogonal per-file `churn`. A tool freshens the deepest slice it reads;
/// freshening a derived slice transitively freshens the shallower ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slice {
    Raw,
    Churn,
    Imports,
    Edges,
    Global,
}

impl Slice {
    /// `slice_state` key for derived slices (raw/churn are per-file and never
    /// ledgered).
    pub fn as_str(self) -> &'static str {
        match self {
            Slice::Raw => "raw",
            Slice::Churn => "churn",
            Slice::Imports => "imports",
            Slice::Edges => "edges",
            Slice::Global => "global",
        }
    }
}

/// What subset of the repo an `ensure_fresh` call reconciles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// One file (per-file tools like `symbols_in_file`).
    File(String),
    /// The whole repo (repo-wide tools like `hotspots`).
    Repo,
}

/// Bring `slices` up to date over `scope`, then return. No-op fast path when
/// nothing in scope changed (the A1 early-exit, per slice).
///
/// Contract:
/// - Missing or schema-mismatched index → a targeted first-build for per-file
///   scopes, else a full `build` (the correct-but-full path).
/// - Raw: reparse changed/new files in scope and refresh their per-file slice
///   (bumping `files.rev`); a deleted on-disk file removes its raw rows and
///   dirties the derived slices (a file-set change can't be signaled by
///   `max(files.rev)` alone, which may drop).
/// - Churn: recompute `git log` commit counts for the changed files (folded
///   into the raw refresh, so a no-op stays warm).
/// - Imports: derived slice, rebuilt when its `built_through_rev` trails
///   `max(files.rev)` (full import re-resolve over the whole file set).
/// - Edges: rebuilt when its `built_through_rev` trails `max(files.rev)` —
///   via the scoped re-resolve (only files whose `rev` passed their per-file
///   `edges_built_rev`, plus their direct reverse-dependents, are re-resolved
///   and rewritten; fan is applied as a delta) or the full re-resolve
///   (file-set change, never-built, or a stale set too large to scope). The
///   repo-scope no-op path skips the tree walk entirely when the opt-in
///   git-status fast path is enabled (see [`changed_files`]); the walk is the
///   default (release benchmark: the git path measured 2-5x slower on
///   macOS — see the `git_perf` example).
/// - Global: full recompute of communities/clone bands/fan-community denorm
///   when `rev` advanced past its `built_through_rev` (Louvain/MinHash are
///   whole-graph, so recompute is accepted but only fires when a global
///   consumer runs and only when dirty).
///
/// Concurrency (§1.4): writes are serialized via SQLite `EXCLUSIVE` locking;
/// a `SQLITE_BUSY` loser (a concurrent freshen or a `build` holding the
/// database) retries with backoff instead of erroring — `ensure_fresh` is
/// idempotent, so retrying the whole call is safe.
pub fn ensure_fresh(slices: &[Slice], repo_root: &str, scope: &Scope) -> Result<()> {
    // Serialize the detect+rebuild critical section per repo (Task 1/2 of
    // the warm-index plan): without this, N concurrent callers on the same
    // repo (multiple agents' hooks, or a background watcher alongside them)
    // all detect the same staleness and all redo the same rebuild, fighting
    // over SQLite's EXCLUSIVE lock via `with_busy_retry` instead of the
    // loser observing the winner's rebuild already caught up and no-op'ing.
    // A crashed lock holder is reclaimed via its recorded PID rather than
    // wedging every future caller (see `repo_lock::acquire`).
    let db_path = crate::db::path::repo_db_path(Path::new(repo_root));
    let _lock = crate::repo_lock::acquire(&db_path, std::time::Duration::from_secs(30))?;
    with_busy_retry(|| ensure_fresh_once(slices, repo_root, scope))
}

fn ensure_fresh_once(slices: &[Slice], repo_root: &str, scope: &Scope) -> Result<()> {
    let t0 = std::time::Instant::now();
    let profile = std::env::var_os("VARDE_PROFILE").is_some();
    let mut rebuilt: Vec<&str> = Vec::new();

    let conn = open_or_build(repo_root, scope)?;

    // Raw reconcile: reparse changed/new files in scope (bumping `files.rev`)
    // and remove rows for files deleted on disk.
    let (changed, deleted, file_set_changed) = changed_files(&conn, repo_root, scope)?;
    if profile && (!changed.is_empty() || !deleted.is_empty()) {
        eprintln!(
            "VARDE_PROFILE ensure_fresh: raw reconcile changed={} deleted={}",
            changed.len(),
            deleted.len()
        );
    }
    for path in &changed {
        let output = crate::scan::run(path)?;
        crate::persist::refresh_file_slice(&conn, &output)?;
    }
    if !changed.is_empty() {
        rebuilt.push("raw");
    }
    if !deleted.is_empty() {
        delete_files(&conn, &deleted)?;
        rebuilt.push("deleted");
    }
    // Churn: recompute commit counts for the changed files (folded into the
    // raw refresh, so a no-op stays warm).
    if slices.contains(&Slice::Churn) && !changed.is_empty() {
        let counts = crate::churn::commit_counts_batch(&changed, Path::new(repo_root));
        crate::persist::set_churn_batch(&conn, &counts)?;
    }

    // Imports: a derived slice, rebuilt when the raw reconcile advanced `rev`
    // past its `built_through_rev` (or it was never ledgered).
    if slices.contains(&Slice::Imports) && freshen_imports(&conn)? {
        rebuilt.push("imports");
    }

    // Edges: rebuilt when stale (see [`freshen_edges`]). Runs after the raw
    // reconcile so the edge resolve reads current entities.
    if slices.contains(&Slice::Edges) && freshen_edges(&conn, file_set_changed)? {
        rebuilt.push("edges");
    }

    // Global: rebuilt when stale (see [`freshen_global`]) — the only slice
    // that still recomputes the whole graph (Louvain/MinHash are whole-graph),
    // but only when a global-slice consumer runs and only when dirty.
    if slices.contains(&Slice::Global) && freshen_global(&conn, file_set_changed)? {
        rebuilt.push("global");
    }

    if profile {
        let path = if rebuilt.is_empty() {
            "no-op".to_string()
        } else {
            rebuilt.join("+")
        };
        eprintln!(
            "VARDE_PROFILE ensure_fresh: slices={slices:?} scope={scope:?} rebuilt={path} total={:?}",
            t0.elapsed()
        );
    }
    Ok(())
}

/// Retry `op` with exponential backoff when SQLite reports `SQLITE_BUSY`
/// (another connection — a concurrent freshen or a `build` — holds the
/// EXCLUSIVE lock). Callers must be idempotent.
fn with_busy_retry<T>(mut op: impl FnMut() -> Result<T>) -> Result<T> {
    const MAX_ATTEMPTS: u32 = 6;
    const BASE_BACKOFF_MS: u64 = 25;
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) if is_sqlite_busy(&err) && attempt + 1 < MAX_ATTEMPTS => {
                last_error = Some(err);
                std::thread::sleep(std::time::Duration::from_millis(BASE_BACKOFF_MS << attempt));
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("busy retry exhausted")))
}

/// Is this error (or anything in its cause chain) a `SQLITE_BUSY`?
fn is_sqlite_busy(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .is_some_and(|e| {
                matches!(e, rusqlite::Error::SqliteFailure(ffi, _) if ffi.code == rusqlite::ErrorCode::DatabaseBusy)
            })
    })
}

/// Remove all persisted rows for `deleted` paths (raw rows via the full
/// cascade) and dirty the derived slices. A file-set change can't be signaled
/// by `max(files.rev)` alone (it may drop when a high-rev file is removed), so
/// the derived ledgers are reset to force a rebuild on the next derived-slice
/// freshen.
fn delete_files(conn: &rusqlite::Connection, deleted: &[String]) -> Result<()> {
    let file_ids = crate::persist::load_file_ids(conn)?;
    let deleted_ids: Vec<i64> = deleted
        .iter()
        .filter_map(|path| file_ids.get(path).copied())
        .collect();
    if deleted_ids.is_empty() {
        return Ok(());
    }
    // Atomic: the row deletion and the ledger invalidation must commit as a
    // unit. If the deletion committed but the invalidation did not, the next
    // freshen would see the derived ledger still marked fresh and skip the
    // rebuild — permanently serving dangling fan counts / stale communities
    // for the deleted file. One transaction (rollback-safe on the MEMORY-journal
    // `ensure_fresh` connection) closes that window.
    let tx = conn.unchecked_transaction()?;
    crate::persist::delete_deleted_files(&tx, &deleted_ids)?;
    // Drop the derived ledgers entirely (the fresheners treat a missing
    // ledger as stale). A file-set change can't be signaled by
    // `max(files.rev)` alone: a full build leaves every rev at 0, so after a
    // deletion `built=0` vs `max=0` would read as *fresh* and skip the
    // fan/community renormalization that deletion demands.
    crate::persist::invalidate_derived_slice_state(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Bring the imports slice (`resolved_edges` kind=Import) up to date. Stale
/// when `built_through_rev` trails the current `max(files.rev)` (raw rows
/// changed since it was last built) or when it has never been ledgered.
/// Correct-but-simple rebuild: full [`crate::resolve::resolve_imports`] over
/// the whole file set (targeted per-file re-resolve is a later optimization).
/// Returns `Ok(true)` when a rebuild actually ran (stale or never ledgered).
fn freshen_imports(conn: &rusqlite::Connection) -> Result<bool> {
    let max = crate::persist::max_rev(conn)?;
    let built = crate::persist::slice_built_through_rev(conn, "imports")?;
    let stale = match built {
        None => true,
        Some(b) => b < max,
    };
    if !stale {
        return Ok(false);
    }
    let state = crate::persist::query_persisted_state(conn, false)?;
    let import_edges = crate::resolve::resolve_imports(&state.entities, &state.files, None);
    crate::persist::rewrite_import_edges(conn, &state, &import_edges)?;
    crate::persist::set_slice_state(conn, "imports", max)?;
    Ok(true)
}

/// Bring the edges slice (`resolved_edges` of both kinds + `files.fan_in`/
/// `files.fan_out`) up to date. Stale when `built_through_rev` trails the
/// current `max(files.rev)` (raw rows changed since it was last built) or when
/// it has never been ledgered.
///
/// Two paths to fresh, chosen by what changed:
/// - **Full re-resolve** — when the file *set* changed (a new/deleted file can
///   newly-resolve or invalidate arbitrary imports and calls repo-wide, so the
///   reverse-dependency scope is not sufficient), the edges slice was never
///   built, or the scoped stale set exceeds [`MAX_SCOPED_FILES`] (CORRECTNESS-
///   104: SQLite's `IN`-variable limit). Re-runs the edge-layer resolve over
///   the whole persisted entity/file set, rewrites every edge row + the fan
///   denorm, and stamps `edges_built_rev = rev` for every file.
/// - **Scoped re-resolve (Cost B)** — only the files whose `rev` advanced past
///   their per-file `edges_built_rev` (the per-file ledger this function
///   stamps), plus their direct reverse-dependents (files with a resolved edge
///   INTO a stale file — a caller's edge resolution depends only on its direct
///   target's export set, never anything transitive). Emits edges only for
///   that scope ([`crate::resolve::resolve_edges_only_scoped`], still
///   resolving against the full entity table) and rewrites only those rows,
///   with fan applied as a delta inside the same transaction
///   ([`crate::persist::rewrite_edges_for_files`]).
///
/// Import edges are recomputed by the same resolve, so after this the imports
/// slice is fresh at `max` too — ledger both to avoid a redundant re-resolve on
/// the next `find_imports`.
/// Returns `Ok(true)` when a rebuild actually ran (stale or never ledgered).
fn freshen_edges(conn: &rusqlite::Connection, file_set_changed: bool) -> Result<bool> {
    let max = crate::persist::max_rev(conn)?;
    let built = crate::persist::slice_built_through_rev(conn, "edges")?;
    let stale = match built {
        None => true,
        Some(b) => b < max,
    };
    if !stale {
        return Ok(false);
    }
    let state = crate::persist::query_persisted_state(conn, false)?;

    // File-set changes (a new/deleted file) can newly-resolve or invalidate
    // arbitrary imports and calls across the whole repo, so the reverse-
    // dependency scope is not sufficient — fall back to the full re-resolve
    // (today's behavior). Same for a never-built edges slice.
    let full_path = file_set_changed || built.is_none();
    let stale_ids: Vec<i64> = if full_path {
        Vec::new()
    } else {
        let mut stmt =
            conn.prepare("SELECT id FROM files WHERE rev > edges_built_rev ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let full = |state: &crate::persist::PersistedState| -> Result<bool> {
        let (nodes, edges) = crate::resolve::resolve_edges_only(&state.entities, &state.files);
        crate::persist::rewrite_edges(conn, state, &edges, &nodes)?;
        crate::persist::set_slice_state(conn, "imports", max)?;
        crate::persist::set_slice_state(conn, "edges", max)?;
        // Full re-resolve invalidates any per-file cache patch assumption
        // (the file set may have changed), so rebuild the graph_cache from
        // scratch rather than patching it.
        //
        // Unlike the scoped path (`rewrite_edges_for_files` patches the
        // cache inside the same transaction as the `resolved_edges`
        // rewrite), the three calls above/below each run as their own
        // transaction/autocommit statement — this write is intentionally
        // best-effort/self-healing rather than atomic with the edges
        // rewrite. `write_graph_cache` stamps `rev = max_rev(conn)`, and
        // nothing here bumps `files.rev`, so an interruption between the
        // `rewrite_edges` commit and this call simply leaves the cache
        // looking stale to `read_graph_cache_if_fresh`'s rev comparison —
        // the next `Graph::load` safely falls back to a full rebuild rather
        // than serving stale data (ARCHITECTURE-001).
        crate::persist::rebuild_graph_cache_full(conn)?;
        Ok(true)
    };
    if full_path || stale_ids.len() > MAX_SCOPED_FILES {
        return full(&state);
    }

    let scope_ids = match reverse_dependent_scope(conn, &stale_ids)? {
        Some(scope) => scope,
        None => {
            // Reverse-dependents blew the scope past the cap — full path.
            return full(&state);
        }
    };

    run_scoped_edge_refresh(conn, &state, &scope_ids, max)
}

/// CORRECTNESS-104: the scoped path builds one `?` placeholder per scoped
/// file (two `IN` clauses in the caller, two more in the rewrite). SQLite
/// caps bound variables at 32766; far below that, a scoped set this large is
/// a repo-wide change anyway, and the full path is the right shape.
pub(crate) const MAX_SCOPED_FILES: usize = 500;

/// Compute the reverse-dependent scope for a base set of changed/stale file
/// ids: the base set itself, plus every file with a `resolved_edges` row
/// pointing *into* the base set (a caller's edge resolution depends only on
/// its target's export set, never anything transitive, so one hop of
/// reverse-dependents is sufficient — see [`freshen_edges`]'s file-set-
/// change comment for why this doesn't apply when the file set itself
/// changed).
///
/// Shared between [`freshen_edges`] and `build.rs::run_incremental`
/// (CODE-002) so the query and the [`MAX_SCOPED_FILES`] cap stay in exactly
/// one place. Returns `Ok(None)` when the computed scope exceeds the cap —
/// the caller should take its full-rebuild path in that case rather than
/// use the returned scope.
pub(crate) fn reverse_dependent_scope(
    conn: &rusqlite::Connection,
    base_ids: &[i64],
) -> Result<Option<std::collections::BTreeSet<i64>>> {
    let mut scope: std::collections::BTreeSet<i64> = base_ids.iter().copied().collect();
    if !base_ids.is_empty() {
        let (placeholders, params) = crate::persist::in_clause(base_ids);
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT from_file_id FROM resolved_edges
             WHERE to_file_id IN ({placeholders})"
        ))?;
        let rows = stmt.query_map(params.as_slice(), |r| r.get::<_, i64>(0))?;
        for row in rows {
            scope.insert(row?);
        }
    }
    if scope.len() > MAX_SCOPED_FILES {
        return Ok(None);
    }
    Ok(Some(scope))
}

/// Run the scoped edge-slice refresh (Cost B path) for an already-computed
/// scope of file ids: resolve edges only for that scope
/// ([`crate::resolve::resolve_edges_only_scoped`]), rewrite only those rows
/// ([`crate::persist::rewrite_edges_for_files`]), and stamp the `imports`/
/// `edges` ledger at `max`. Pure extraction from [`freshen_edges`] (CORE-
/// GRAPH-CACHE-001): the scope is supplied by the caller rather than derived
/// from the rev ledger, so callers other than `freshen_edges` (e.g. `build.rs`,
/// with its own `classify_files`-derived scope) can drive the same rewrite.
///
/// `scope_ids` may be empty — nothing to rewrite, but the ledger must still
/// advance so the next staleness check no-ops. Callers are responsible for
/// keeping `scope_ids.len()` within [`MAX_SCOPED_FILES`]; this function does
/// not fall back to the full path itself.
pub(crate) fn run_scoped_edge_refresh(
    conn: &rusqlite::Connection,
    state: &crate::persist::PersistedState,
    scope_ids: &std::collections::BTreeSet<i64>,
    max: i64,
) -> Result<bool> {
    #[cfg(test)]
    crate::SCOPED_EDGE_REFRESH_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if scope_ids.is_empty() {
        // Nothing stale per the per-file ledger, yet the global ledger says
        // stale — nothing to rewrite, but the ledger must still advance so
        // the next call's entry check no-ops.
        crate::persist::set_slice_state(conn, "imports", max)?;
        crate::persist::set_slice_state(conn, "edges", max)?;
        return Ok(true);
    }

    // Map scope DB ids back to flat file indices for the resolve emit filter
    // (O(1) via the index `query_persisted_state` already built — CODE-202).
    let flat_scope: std::collections::HashSet<u32> = scope_ids
        .iter()
        .filter_map(|db_id| state.file_index.get(db_id).copied())
        .collect();
    // One unit carrying both index spaces (ARCHITECTURE-303): the db ids the
    // rewrite deletes/inserts/stamps and the flat indices the resolve emits.
    let refresh = crate::resolve::ScopedEdgeRefresh {
        file_ids: scope_ids.iter().copied().collect(),
        flat_scope,
    };
    let edges = crate::resolve::resolve_edges_only_scoped(&state.entities, &state.files, &refresh);
    crate::persist::rewrite_edges_for_files(conn, state, &refresh, &edges)?;
    crate::persist::set_slice_state(conn, "imports", max)?;
    crate::persist::set_slice_state(conn, "edges", max)?;
    Ok(true)
}

/// Bring the global slice (`communities`, `community_members`, `clone_bands`,
/// `clone_band_members`, `files.community_id`) up to date. Stale when
/// `built_through_rev` trails the current `max(files.rev)` or when it has never
/// been ledgered.
///
/// The plan's contract: ensure the edges slice is fresh first (community
/// detection walks the same resolved edges and the community/fan denorm both
/// derive from the edge set), then do a **full** recompute of communities
/// ([`crate::resolve::community::detect`]) and clone bands
/// ([`crate::resolve::clones::detect_bands`]). Louvain/MinHash are whole-graph,
/// so full recompute is accepted — it only fires when a global-slice consumer
/// (map_file/scan) runs and only when dirty. Uses the same [`crate::resolve::resolve`]
/// path as a full build, so the global output is byte-parity with `build`.
/// Returns `Ok(true)` when a rebuild actually ran (stale or never ledgered).
fn freshen_global(conn: &rusqlite::Connection, file_set_changed: bool) -> Result<bool> {
    let max = crate::persist::max_rev(conn)?;
    let built = crate::persist::slice_built_through_rev(conn, "global")?;
    let stale = match built {
        None => true,
        Some(b) => b < max,
    };
    if !stale {
        return Ok(false);
    }
    freshen_edges(conn, file_set_changed)?;
    let state = crate::persist::query_persisted_state(conn, false)?;
    // `resolve` ignores symbols (verified: `let _ = symbols`), so an empty
    // slice is the correct input for a graph-only recompute.
    let graph = crate::resolve::resolve(&state.entities, &[], &state.files)?;
    crate::persist::rewrite_global(conn, &state, &graph)?;
    crate::persist::set_slice_state(conn, "global", max)?;
    Ok(true)
}

/// Build-on-miss only: ensure the index exists and matches the current schema,
/// without freshening any slice. Used by `detect_changes`, whose whole point is
/// to diff the persisted store against the current source — freshening the raw
/// slice to disk first would zero out that diff.
pub fn ensure_index_built(repo_root: &str) -> Result<()> {
    open_or_build(repo_root, &Scope::Repo).map(|_| ())
}

/// Dump the slice freshness ledger for a repo — the `--why` style view of
/// what each derived slice was last built through and whether it's currently
/// stale (built_through_rev trailing `max(files.rev)`), plus the `next_rev`
/// counter. Read-only: never builds or freshens. Input is the query envelope
/// (`repoRoot` resolves to the conventional path, or an explicit `dbPath`).
pub fn dump_state(input: &serde_json::Value) -> Result<serde_json::Value> {
    let db_path = match input.get("dbPath").and_then(|v| v.as_str()) {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let repo_root = input
                .get("repoRoot")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("repoRoot or dbPath required"))?;
            crate::db::path::repo_db_path(Path::new(repo_root))
        }
    };
    if !db_path.exists() {
        return Ok(serde_json::json!({
            "db_path": db_path,
            "db": "missing",
        }));
    }
    let conn = crate::db::open_read_only(&db_path)?;
    let max_rev = crate::persist::max_rev(&conn)?;
    let mut slices = serde_json::Map::new();
    for slice in ["imports", "edges", "global"] {
        let built = crate::persist::slice_built_through_rev(&conn, slice)?;
        slices.insert(
            slice.to_string(),
            serde_json::json!({
                "built_through_rev": built,
                "stale": built.is_none_or(|b| b < max_rev),
            }),
        );
    }
    let next_rev: i64 = conn
        .query_row(
            "SELECT value FROM slice_meta WHERE key = 'next_rev'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(serde_json::json!({
        "db_path": db_path,
        "max_rev": max_rev,
        "next_rev": next_rev,
        "slices": slices,
    }))
}

/// Open the repo's database, or build it when missing/schema-mismatched. A
/// per-file scope gets a minimal targeted first-build (schema + that one
/// file's raw slice) instead of a full repo build — the plan's §4.2 "missing
/// DB → targeted first-build"; everything else falls back to a full build.
fn open_or_build(repo_root: &str, scope: &Scope) -> Result<rusqlite::Connection> {
    let db_path = crate::db::path::repo_db_path(Path::new(repo_root));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !db_path.exists() {
        return build_or_open(repo_root, scope, &db_path);
    }
    // MEMORY journal: `ensure_fresh` mutates the live index in place, so its
    // transactions must roll back cleanly on error (see `db::open_incremental`).
    let conn = crate::db::open_incremental(&db_path)?;
    match crate::db::schema_version(&conn) {
        Ok(v) if v == crate::db::SCHEMA_VERSION => Ok(conn),
        Ok(_) | Err(_) => {
            // Schema mismatch on an *existing* DB means other files' rows
            // are already populated under the old schema. A targeted
            // per-file build here would rebuild the schema (dropping every
            // table) and only repopulate the requested file, leaving the
            // rest of the index falsely marked schema-fresh but empty. Only
            // a full repo build repopulates every slice, regardless of the
            // requesting scope.
            drop(conn);
            crate::build::run_with_force(repo_root, true)?;
            crate::db::open_incremental(&db_path)
        }
    }
}

/// Build the index (targeted when possible, else full) and open it.
fn build_or_open(
    repo_root: &str,
    scope: &Scope,
    db_path: &std::path::Path,
) -> Result<rusqlite::Connection> {
    if let Some(path) = targeted_build_target(scope) {
        let conn = crate::db::open_incremental(db_path)?;
        match build_file_targeted(&conn, path) {
            Ok(()) => return Ok(conn),
            Err(_) => drop(conn), // fall through to a full build on any failure
        }
    }
    crate::build::run_with_force(repo_root, true)?;
    crate::db::open_incremental(db_path)
}

/// The file to minimally first-build, or `None` when a full build is required
/// (repo-wide scope, or a per-file scope whose path doesn't exist on disk).
fn targeted_build_target(scope: &Scope) -> Option<&str> {
    match scope {
        Scope::File(path) if Path::new(path).is_file() => Some(path.as_str()),
        _ => None,
    }
}

/// Minimal first-build for one file: create the schema, parse the file, and
/// write its raw slice. Cheap for per-file tools on a never-built index.
fn build_file_targeted(conn: &rusqlite::Connection, path: &str) -> Result<()> {
    crate::db::rebuild_schema(conn)?;
    let output = crate::scan::run(path)?;
    crate::persist::refresh_file_slice(conn, &output)?;
    crate::db::create_indexes(conn)?;
    Ok(())
}

/// Files in `scope` whose raw slice is stale (changed or new on disk relative
/// to the persisted `files` rows) and files deleted on disk since the last
/// build. Returns `(changed, deleted, file_set_changed)` where
/// `file_set_changed` is true when the *set* of files changed (any new or
/// deleted file) — derived-slice rebuilds that can't be scoped by
/// reverse-dependency must treat that as a full-resolve trigger.
///
/// **Oracle contract (ARCHITECTURE-301).** For `Scope::Repo`, "what counts as
/// changed" is decided by one of two oracles, with a single documented
/// precedence:
///
/// 1. The **git fast path** ([`git_changed_files`]): `git status
///    --porcelain=v1 --untracked-files=all -z` (plus `git ls-files` for the
///    gitignore cross-check), used when `repo_root` is inside a git worktree
///    *and* the recorded build-time HEAD matches *and* the last build observed
///    a clean worktree.
/// 2. The **full walk** (`list_source_files` + `classify_files` stat-diff) —
///    used for non-git roots, any git failure, a HEAD move since the last
///    build, a dirty-at-build worktree, or a porcelain record that fails the
///    `XY path` shape.
///
/// The two oracles are mtime-vs-content based, so they diverge on a few
/// enumerated cases — each is deliberately resolved rather than silently
/// path-dependent:
/// - **Touch with no content change**: git reports clean (no-op); the walk
///   previously counted it changed and triggered a byte-identical reparse.
///   Strictly more correct — intended.
/// - **Revert to committed content after a dirty build**: invisible to git
///   status (worktree == index == HEAD); `git_clean_at_build` forces the walk
///   until a build observes a clean worktree (CORRECTNESS-102).
/// - **Newly-gitignored stored files** (e.g. `git rm --cached` + a
///   `.gitignore` entry): never reported by git status; the fast path
///   cross-checks stored files against `git ls-files` (minus ignored) and
///   drops now-ignored ones as deleted, matching the walk
///   (CORRECTNESS-103).
/// - **Staged renames**: `status.renames=false` renders them as separate
///   `D old` / `A new` records the parser handles; a record that fails the
///   `XY path` shape falls back to the walk (CORRECTNESS-101).
///
/// Cost A's git-status changed_files fast path is **opt-in**
/// (`VARDE_GIT_FAST_PATH`): a release benchmark (2026-09-03, `git_perf`
/// example) measured Apple Git spawns at 40-110ms each, and `git status`
/// scales with repo size (344ms on 4,223 files) — 2-5x slower than the
/// parallel walk (~9µs/file) at every repo size tested. The walk is the
/// default; the git path only wins where spawns are cheap (Linux) and the
/// walk dominates. Tests pin the decision via
/// [`crate::GIT_FAST_PATH_TEST_OVERRIDE`] (ignoring the env var, so CI is
/// deterministic regardless of the shell).
fn git_fast_path_enabled() -> bool {
    #[cfg(test)]
    {
        crate::GIT_FAST_PATH_TEST_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(test))]
    {
        std::env::var_os("VARDE_GIT_FAST_PATH").is_some()
    }
}

fn changed_files(
    conn: &rusqlite::Connection,
    repo_root: &str,
    scope: &Scope,
) -> Result<(Vec<String>, Vec<String>, bool)> {
    match scope {
        Scope::File(path) => {
            let stored: Vec<StoredFileState> = crate::persist::load_file_state(conn, path)?
                .into_iter()
                .collect();
            if !Path::new(path).exists() {
                // Deleted on disk: remove its rows if it was ever indexed.
                return Ok((
                    Vec::new(),
                    if stored.is_empty() {
                        Vec::new()
                    } else {
                        vec![path.clone()]
                    },
                    !stored.is_empty(),
                ));
            }
            let current = crate::scan::list_source_files(path)?;
            if current.is_empty() {
                return Ok((Vec::new(), Vec::new(), false));
            }
            let classifications = classify_files(&stored, &current);
            let stale = classifications
                .iter()
                .any(|c| !matches!(c, FileClassification::Unchanged { .. }));
            Ok(if stale {
                (
                    vec![path.clone()],
                    Vec::new(),
                    classifications.iter().any(|c| {
                        matches!(
                            c,
                            FileClassification::New { .. } | FileClassification::Deleted { .. }
                        )
                    }),
                )
            } else {
                (Vec::new(), Vec::new(), false)
            })
        }
        Scope::Repo => {
            if git_fast_path_enabled()
                && let Some(result) = git_changed_files(conn, repo_root)
            {
                return Ok(result);
            }
            let current = crate::scan::list_source_files(repo_root)?;
            let stored = crate::persist::load_file_states(conn)?;
            let mut changed = Vec::new();
            let mut deleted = Vec::new();
            let mut file_set_changed = false;
            for c in classify_files(&stored, &current) {
                match c {
                    FileClassification::Changed { path } => changed.push(path),
                    FileClassification::New { path } => {
                        changed.push(path);
                        file_set_changed = true;
                    }
                    FileClassification::Deleted { path } => {
                        deleted.push(path);
                        file_set_changed = true;
                    }
                    FileClassification::Unchanged { .. } => {}
                }
            }
            // The walk path just absorbed whatever committed content changes
            // the HEAD-move fallback was protecting against — record the
            // current HEAD so the next call can use the git fast path again.
            if let Some(head) = crate::git::run_git(&["rev-parse", "HEAD"], Path::new(repo_root)) {
                crate::persist::set_slice_meta_value(conn, "git_head", head_fingerprint(&head))?;
            }
            Ok((changed, deleted, file_set_changed))
        }
    }
}

/// The changed/new/deleted set for a repo scope from `git status`, or `None`
/// when `repo_root` isn't inside a git worktree, git fails for any reason
/// (missing binary, corrupted `.git`, permission error), HEAD moved since the
/// last recorded build, the last build captured a dirty worktree, or a
/// porcelain record fails the `XY path` shape — the caller then falls back to
/// the full `list_source_files` walk. The caller additionally gates on
/// [`git_fast_path_enabled`]: this is opt-in via `VARDE_GIT_FAST_PATH`, with
/// the walk the default.
///
/// **HEAD-move fallback is load-bearing, not a nicety:** `git status`
/// compares the worktree against the index/HEAD, so it reports *clean* for a
/// file whose content changed in a commit since the last build — the exact
/// case where varde's persisted state is stale. Comparing the current HEAD
/// against the HEAD recorded at the last build (`slice_meta` key `git_head`)
/// detects that and falls back to the mtime-based walk, which sees the
/// committed content change. Without it, a commit-then-query cycle would
/// silently answer from a stale index.
///
/// **Dirty-build gate (CORRECTNESS-102):** `git status` also compares the
/// worktree against HEAD for *uncommitted* state, so a file that was
/// uncommitted-dirty when the index was built and later reverted to its
/// committed content reports clean while the DB still serves the dirty
/// content. `record_git_head` stores `git_clean_at_build`; while it says the
/// DB was built from a dirty worktree, this returns `None` and the walk
/// (which sees the revert via mtime) runs instead.
///
/// Porcelain v1 `-z` output is `XY path\0` per entry: X is the index column,
/// Y the worktree column, both ` ` (unmodified) or a status letter (`M`
/// modified, `T` type change, `A` added, `D` deleted, `R` renamed, `C`
/// copied, `U` unmerged, `?` untracked); paths are raw, NUL-separated,
/// relative to the repo root, and `.git` internals never appear. The status
/// call runs with `-c status.renames=false` so a rename surfaces as separate
/// `D old` / `A new` records instead of the two-record `R new\0old\0` form,
/// and any record that fails the shape triggers the walk fallback rather than
/// feeding a mangled path to `scan::run` (CORRECTNESS-101). `??` (untracked)
/// and `A`/`M`/`R`/`C`/`T`/`U` map to "new/changed" (folded into `changed`);
/// `D` in either column maps to deleted; `.gitignore`d files are excluded by
/// git exactly as `ignore::WalkBuilder` already excludes them. `file_set_changed`
/// is true when any entry is untracked/new or deleted.
///
/// **Gitignore cross-check (CORRECTNESS-103):** `git status` never reports
/// *ignored* files, so a stored file that became ignored (e.g. `git rm
/// --cached` + a `.gitignore` entry) would keep its rows forever. After
/// parsing, stored `files` paths are cross-checked against the git-visible
/// set (`git ls-files` minus tracked-but-ignored, plus everything reported
/// by status); a stored file in neither, and not under `.git/`, is now
/// ignored — returned as deleted, matching what the walk would classify.
///
/// **Gate (benchmark finding, release `git_perf`):** the two `ls-files`
/// spawns cost ~130ms — more than the walk on a small repo — so the check
/// runs only when a `.gitignore` file appears in the status set. A stored
/// file can only newly become ignored if an ignore rule changed since the
/// last build: committed rule changes already fall back to the walk via the
/// HEAD-move gate above, and uncommitted ones surface the `.gitignore`
/// itself in status. `.git/info/exclude` / global `core.excludesFile` edits
/// are invisible to both status and this gate — accepted divergence (rare,
/// and any other tripped gate falls back to the walk, which classifies
/// correctly).
fn git_changed_files(
    conn: &rusqlite::Connection,
    repo_root: &str,
) -> Option<(Vec<String>, Vec<String>, bool)> {
    let head = crate::git::run_git(&["rev-parse", "HEAD"], Path::new(repo_root))?;
    let head_fp = head_fingerprint(&head);
    let stored = crate::persist::slice_meta_value(conn, "git_head").ok()?;
    if stored.is_some_and(|s| s != head_fp) {
        // HEAD moved since the last recorded build → committed content changes
        // are invisible to `git status` (worktree == HEAD); fall back to the
        // mtime walk. The caller records the new HEAD after it runs.
        return None;
    }
    if crate::persist::slice_meta_value(conn, "git_clean_at_build").ok()? == Some(0) {
        // The DB was built from a dirty worktree (CORRECTNESS-102): git
        // status compares the worktree against HEAD, so a file reverted to
        // its committed content reports *clean* while the DB still serves the
        // dirty content. The mtime walk sees the revert; force it until a
        // build observes a clean worktree (`record_git_head` flips the flag).
        return None;
    }
    // `status.renames=false` renders a rename as separate `D old` / `A new`
    // records — the parser never sees the two-record `R new\0old\0` form.
    let text = crate::git::run_git(
        &[
            "-c",
            "status.renames=false",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "-z",
        ],
        Path::new(repo_root),
    )?;
    let root = Path::new(repo_root);
    let mut changed = Vec::new();
    let mut deleted = Vec::new();
    let mut file_set_changed = false;
    let is_status_letter = |b: u8| {
        matches!(
            b,
            b' ' | b'M' | b'T' | b'A' | b'D' | b'R' | b'C' | b'U' | b'?'
        )
    };
    for entry in text.split('\0') {
        if entry.is_empty() {
            continue;
        }
        let bytes = entry.as_bytes();
        // Defensive shape check: anything that isn't exactly `XY path` means
        // the wire format surprised us (e.g. a two-record rename if
        // `status.renames` were ever re-enabled) — fall back to the walk
        // rather than feed a mangled path to `scan::run`.
        if bytes.len() < 4
            || !is_status_letter(bytes[0])
            || !is_status_letter(bytes[1])
            || bytes[2] != b' '
        {
            return None;
        }
        let (x, y) = (bytes[0], bytes[1]);
        let path = &entry[3..];
        let is_deleted = x == b'D' || y == b'D';
        let is_untracked = x == b'?' && y == b'?';
        let is_change = !is_deleted && !is_untracked && (x != b' ' || y != b' ');
        let abs = root.join(path).to_string_lossy().into_owned();
        if is_deleted {
            deleted.push(abs);
            file_set_changed = true;
        } else if is_untracked || is_change {
            changed.push(abs);
            if is_untracked {
                file_set_changed = true;
            }
        }
    }

    // Gitignore cross-check (CORRECTNESS-103): a stored file that is now
    // ignored is invisible to `git status` and would keep its rows forever.
    // A stored file is git-visible when it's tracked and not ignored
    // (`git ls-files` minus `git ls-files --ignored`), or appears in the
    // status output above; anything else is now ignored → deleted, matching
    // the walk's classification. Best-effort: any failure skips the check.
    // Gated on a `.gitignore` file showing in the status set — the only
    // uncommitted way a stored file can newly become ignored (committed
    // rule changes already fell back to the walk via the HEAD-move gate).
    // See the fn doc for the full rationale and the accepted hole.
    let ignore_relevant = changed
        .iter()
        .chain(deleted.iter())
        .any(|p| Path::new(p).file_name().is_some_and(|n| n == ".gitignore"));
    if ignore_relevant
        && let (Some(tracked), Some(tracked_ignored)) = (
            crate::git::run_git(&["ls-files", "-z"], root),
            crate::git::run_git(&["ls-files", "-ci", "--exclude-standard", "-z"], root),
        )
    {
        let mut visible: std::collections::HashSet<String> = std::collections::HashSet::new();
        visible.extend(changed.iter().cloned());
        visible.extend(deleted.iter().cloned());
        let ignored_set: std::collections::HashSet<&str> = tracked_ignored.split('\0').collect();
        for p in tracked.split('\0') {
            if !p.is_empty() && !ignored_set.contains(p) {
                visible.insert(root.join(p).to_string_lossy().into_owned());
            }
        }
        let stored_paths: Vec<String> = {
            let mut stmt = conn.prepare("SELECT path FROM files").ok()?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).ok()?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row.ok()?);
            }
            v
        };
        for stored in stored_paths {
            // `.git` internals are a pre-existing walk quirk, never source
            // files and never reported by git — leave them alone.
            if Path::new(&stored)
                .components()
                .any(|c| c.as_os_str() == ".git")
            {
                continue;
            }
            if !visible.contains(&stored) {
                deleted.push(stored);
                file_set_changed = true;
            }
        }
    }
    crate::persist::set_slice_meta_value(conn, "git_head", head_fp).ok()?;
    Some((changed, deleted, file_set_changed))
}

/// A stable integer fingerprint of a `git rev-parse HEAD` output line. Used to
/// detect "HEAD moved since the last build" without storing the full hash.
pub(crate) fn head_fingerprint(head: &str) -> i64 {
    let head = head.trim();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in head.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h as i64
}

/// Record the repo's current HEAD and worktree cleanliness in `slice_meta`
/// (`git_head` + `git_clean_at_build`). Called after a full/incremental build
/// so the git-status fast path can detect "a commit landed since the last
/// build" (HEAD move → fall back to the mtime walk) and "the build captured a
/// dirty worktree" (CORRECTNESS-102: a later revert to committed content is
/// invisible to git status → force the walk until a build sees it clean).
pub(crate) fn record_git_head(conn: &rusqlite::Connection, repo_root: &str) -> Result<()> {
    let Some(head) = crate::git::run_git(&["rev-parse", "HEAD"], Path::new(repo_root)) else {
        return Ok(());
    };
    crate::persist::set_slice_meta_value(conn, "git_head", head_fingerprint(&head))?;
    let clean = crate::git::run_git(&["status", "--porcelain=v1", "-z"], Path::new(repo_root))
        .is_some_and(|text| text.is_empty());
    // Propagated (not just logged): a swallowed failure here defaults to
    // "not forced" on the next build's fast path — indistinguishable from a
    // clean build — which reopens CORRECTNESS-102 (a later revert to
    // committed content going undetected) with no signal beyond a log line.
    crate::persist::set_slice_meta_value(conn, "git_clean_at_build", if clean { 1 } else { 0 })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "varde-slice-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp root creates");
        path
    }

    /// Pin the Cost A git-status changed_files fast path ON for the lifetime
    /// of the returned guard, restoring the prior decision on drop. All slice
    /// tests serialize on [`crate::HOME_TEST_LOCK`], so this cannot race
    /// another slice test; other modules' tests run under whatever decision
    /// is active, which is benign in both directions (they pass under both
    /// the git path and the walk).
    struct FastPathTestGuard {
        prev: bool,
    }
    impl FastPathTestGuard {
        fn on() -> Self {
            let prev =
                crate::GIT_FAST_PATH_TEST_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
            crate::GIT_FAST_PATH_TEST_OVERRIDE.store(true, std::sync::atomic::Ordering::Relaxed);
            FastPathTestGuard { prev }
        }
    }
    impl Drop for FastPathTestGuard {
        fn drop(&mut self) {
            crate::GIT_FAST_PATH_TEST_OVERRIDE
                .store(self.prev, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Run `f` with an isolated HOME-derived conventional DB path.
    fn with_isolated_home<F: FnOnce()>(label: &str, f: F) {
        crate::test_support::with_isolated_home(&format!("slice-{label}"), f);
    }

    fn db_of(root: &std::path::Path) -> std::path::PathBuf {
        crate::db::path::repo_db_path(root)
    }

    /// Read a single `files` column by path — the shared body behind
    /// `rev_of`/`complexity_of`/`churn_of`, which only differ in the column
    /// name and whether the value is nullable.
    fn files_column<T: rusqlite::types::FromSql>(
        root: &std::path::Path,
        path: &str,
        column: &str,
    ) -> T {
        let conn = crate::db::open(&db_of(root)).expect("db opens");
        conn.query_row(
            &format!("SELECT {column} FROM files WHERE path = ?1"),
            [path],
            |r| r.get(0),
        )
        .expect("column reads")
    }

    fn rev_of(root: &std::path::Path, path: &str) -> i64 {
        files_column(root, path, "rev")
    }

    fn complexity_of(root: &std::path::Path, path: &str) -> Option<i64> {
        files_column(root, path, "complexity")
    }

    fn churn_of(root: &std::path::Path, path: &str) -> Option<i64> {
        files_column(root, path, "churn")
    }

    fn build(root: &std::path::Path) {
        crate::build::run_with_force(root.to_str().expect("utf-8 root"), true)
            .expect("build succeeds");
    }

    #[test]
    fn raw_freshen_updates_complexity_and_bumps_rev() {
        with_isolated_home("complexity", || {
            let root = temp_root("complexity");
            let file = root.join("a.rs");
            let path_str = file.to_string_lossy().into_owned();
            std::fs::write(&file, "fn f(x: i32) -> i32 { if x > 0 { 1 } else { 0 } }\n")
                .expect("writes");
            build(&root);

            let c0 = complexity_of(&root, &path_str).expect("initial complexity present");
            let r0 = rev_of(&root, &path_str);
            assert!(c0 >= 2, "initial file has control flow, complexity {c0}");

            // Add an `else if` branch → one more ControlFlow entity → complexity up.
            std::fs::write(
                &file,
                "fn f(x: i32) -> i32 { if x > 0 { 1 } else if x < 0 { -1 } else { 0 } }\n",
            )
            .expect("rewrites");

            ensure_fresh(
                &[Slice::Raw],
                root.to_str().unwrap(),
                &Scope::File(path_str.clone()),
            )
            .expect("freshen succeeds");

            let c1 = complexity_of(&root, &path_str).expect("post-edit complexity present");
            let r1 = rev_of(&root, &path_str);
            assert!(
                c1 > c0,
                "complexity rises after a control-flow edit: {c0} -> {c1}"
            );
            assert!(r1 > r0, "rev bumps after a raw refresh: {r0} -> {r1}");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn no_op_freshen_leaves_rev_unchanged() {
        with_isolated_home("noop", || {
            let root = temp_root("noop");
            let file = root.join("a.rs");
            let path_str = file.to_string_lossy().into_owned();
            std::fs::write(&file, "fn a() {}\n").expect("writes");
            build(&root);

            let r0 = rev_of(&root, &path_str);
            ensure_fresh(
                &[Slice::Raw],
                root.to_str().unwrap(),
                &Scope::File(path_str.clone()),
            )
            .expect("freshen succeeds");
            let r1 = rev_of(&root, &path_str);
            assert_eq!(r0, r1, "no-op freshen must not bump rev");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn schema_mismatch_on_per_file_scope_rebuilds_every_file_not_just_one() {
        with_isolated_home("schema-mismatch", || {
            let root = temp_root("schema-mismatch");
            let a = root.join("a.rs");
            let b = root.join("b.rs");
            let a_str = a.to_string_lossy().into_owned();
            let b_str = b.to_string_lossy().into_owned();
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            std::fs::write(&b, "fn b() {}\n").expect("writes b");
            build(&root);

            // Simulate a schema bump on an existing, fully-populated DB.
            {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION + 1)
                    .expect("bump schema version");
            }

            // A per-file tool call for `a` must not leave `b`'s rows dropped.
            ensure_fresh(
                &[Slice::Raw],
                root.to_str().unwrap(),
                &Scope::File(a_str.clone()),
            )
            .expect("freshen succeeds");

            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            let b_rows: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM files WHERE path = ?1",
                    [&b_str],
                    |r| r.get(0),
                )
                .expect("count reads");
            assert_eq!(
                b_rows, 1,
                "a schema-mismatch rebuild triggered by a per-file scope must repopulate every file, not just the requested one"
            );

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn repo_scope_raw_freshen_reparses_only_changed_files() {
        with_isolated_home("repo-raw", || {
            let root = temp_root("repo-raw");
            let a = root.join("a.rs");
            let b = root.join("b.rs");
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            std::fs::write(&b, "fn b() {}\n").expect("writes b");
            build(&root);

            let a_str = a.to_string_lossy().into_owned();
            let b_str = b.to_string_lossy().into_owned();
            let r_a0 = rev_of(&root, &a_str);
            let r_b0 = rev_of(&root, &b_str);

            // Edit only `a.rs`.
            std::fs::write(&a, "fn a() { let x = 1; }\n").expect("rewrites a");

            ensure_fresh(&[Slice::Raw], root.to_str().unwrap(), &Scope::Repo).expect("freshen");

            let r_a1 = rev_of(&root, &a_str);
            let r_b1 = rev_of(&root, &b_str);
            assert!(r_a1 > r_a0, "changed file's rev bumps");
            assert_eq!(r_b0, r_b1, "unchanged file's rev is untouched");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn churn_freshen_reflects_new_commits() {
        with_isolated_home("churn", || {
            let root = temp_root("churn");
            run_git(&root, &["init", "-q"]);
            run_git(&root, &["config", "user.email", "test@example.com"]);
            run_git(&root, &["config", "user.name", "Churn Test"]);
            let file = root.join("a.rs");
            let path_str = file.to_string_lossy().into_owned();
            for i in 0..3 {
                std::fs::write(&file, format!("fn f{i}() {{}}\n")).expect("writes");
                run_git(&root, &["add", "a.rs"]);
                run_git(&root, &["commit", "-q", "-m", &format!("commit {i}")]);
            }
            build(&root);
            assert_eq!(
                churn_of(&root, &path_str),
                Some(3),
                "three commits -> churn 3"
            );

            // A 4th commit rewrites the file (mtime changes) → raw + churn freshen.
            std::fs::write(&file, "fn f3() { let x = 1; }\n").expect("writes");
            run_git(&root, &["add", "a.rs"]);
            run_git(&root, &["commit", "-q", "-m", "commit 3"]);

            ensure_fresh(
                &[Slice::Raw, Slice::Churn],
                root.to_str().unwrap(),
                &Scope::Repo,
            )
            .expect("freshen");

            assert_eq!(
                churn_of(&root, &path_str),
                Some(4),
                "churn reflects the 4th commit"
            );

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// `(from_path, to_path, kind, resolved)` for every `resolved_edges` row,
    /// sorted — the projection the graph modes traverse (path-based, so it
    /// ignores entity rowids that a re-parse reassigns).
    fn edges_of(root: &std::path::Path) -> Vec<(String, Option<String>, String, bool)> {
        let conn = crate::db::open(&db_of(root)).expect("db opens");
        let mut stmt = conn
            .prepare(
                "SELECT f.path, t.path, e.kind, e.resolved
                 FROM resolved_edges e
                 JOIN files f ON f.id = e.from_file_id
                 LEFT JOIN files t ON t.id = e.to_file_id",
            )
            .expect("edges stmt");
        let mut rows: Vec<(String, Option<String>, i64, bool)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })
            .expect("edges query")
            .map(|r| r.expect("edge row"))
            .collect();
        rows.sort();
        rows.into_iter()
            .map(|(from, to, kind, resolved)| {
                let kind_name = if kind == 0 { "Call" } else { "Import" };
                (from, to, kind_name.to_string(), resolved)
            })
            .collect()
    }

    fn fan_of(root: &std::path::Path, path: &str) -> (i64, i64) {
        let conn = crate::db::open(&db_of(root)).expect("db opens");
        conn.query_row(
            "SELECT fan_in, fan_out FROM files WHERE path = ?1",
            [path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("fan reads")
    }

    /// Phase 2 reverse-dep correctness: editing an export retargets an
    /// unchanged caller's call edge (and its fan metrics) on the next
    /// `ensure_fresh(Edges)` — no manual rebuild. `a.ts` is untouched; only
    /// `x.ts`'s export set changes, which is exactly the cross-file dependency
    /// the plan's neighborhood caveat flags.
    #[test]
    fn edges_freshen_retargets_caller_after_export_edit() {
        with_isolated_home("edges-retarget", || {
            let root = temp_root("edges-retarget");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");
            build(&root);

            let a_str = a.to_str().unwrap();
            let before = edges_of(&root);
            let a_call = before
                .iter()
                .find(|(from, _, kind, _)| from.ends_with("a.ts") && kind == "Call")
                .expect("a.ts has a call edge");
            assert!(a_call.3, "call edge resolves initially: {before:?}");
            assert!(
                a_call.1.as_deref().unwrap_or("").ends_with("x.ts"),
                "call targets x.ts: {before:?}"
            );
            // a.ts's resolved import edge (→ x.ts) and resolved call edge both
            // count: fan_out = 2.
            let (_, a_out) = fan_of(&root, a_str);
            assert_eq!(a_out, 2, "import + resolved call count toward fan_out");

            // Rename the export: foo disappears from x.ts (a.ts unchanged).
            std::fs::write(&x, "export const bar = () => 1;\n").expect("rewrite x.ts");

            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen edges succeeds");

            let after = edges_of(&root);
            let a_call = after
                .iter()
                .find(|(from, _, kind, _)| from.ends_with("a.ts") && kind == "Call")
                .expect("a.ts still has a call edge");
            assert!(!a_call.3, "call edge now unresolved: {after:?}");
            let (_, a_out) = fan_of(&root, a_str);
            assert_eq!(a_out, 1, "only the import edge counts now: {after:?}");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Phase 2 parity: after edits, the edges slice rebuilt by
    /// `ensure_fresh(Edges)` matches a full `build` on the same source — graph
    /// mode answers can't drift from the `build` command's.
    #[test]
    fn edges_freshen_matches_full_build_after_edits() {
        with_isolated_home("edges-parity", || {
            let root = temp_root("edges-parity");
            let a = root.join("a.ts");
            let b = root.join("b.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&b, "import { bar } from \"./x\";\nbar();\n").expect("write b.ts");
            std::fs::write(
                &x,
                "export const foo = () => 1;\nexport const bar = () => 2;\n",
            )
            .expect("write x.ts");
            build(&root);

            // Edit: a.ts now imports bar, and x.ts drops foo.
            std::fs::write(&a, "import { bar } from \"./x\";\nbar();\n").expect("rewrite a.ts");
            std::fs::write(&x, "export const bar = () => 2;\n").expect("rewrite x.ts");

            // Freshen just the edges slice...
            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen edges succeeds");
            let sliced = edges_of(&root);

            // ...then full-build the same source; the projections must match.
            let a_str = a.to_str().unwrap();
            let b_str = b.to_str().unwrap();
            let sliced_fans = (fan_of(&root, a_str), fan_of(&root, b_str));
            build(&root);
            let full = edges_of(&root);
            assert_eq!(sliced, full, "resolved_edges parity after edits");

            // Fan denorm parity too.
            let full_fans = (fan_of(&root, a_str), fan_of(&root, b_str));
            assert_eq!(sliced_fans, full_fans, "fan denorm parity after edits");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// `(community label per file, sorted community memberships by label,
    /// sorted clone band member entity names)` — the full projection of the
    /// global slice, keyed on labels/names rather than rowids: communities and
    /// entities are re-inserted by rebuilds with different `AUTOINCREMENT`
    /// ids (a DELETE keeps the high-water mark), so comparing ids across a
    /// freshened vs. full-built index would spuriously differ. Labels and
    /// names are deterministic given the same graph.
    type GlobalProjection = (
        Vec<(String, Option<String>)>,
        Vec<Vec<String>>,
        Vec<Vec<String>>,
    );

    fn global_projection(root: &std::path::Path) -> GlobalProjection {
        let conn = crate::db::open(&db_of(root)).expect("db opens");
        let mut file_stmt = conn
            .prepare(
                "SELECT f.path, c.label FROM files f
                 LEFT JOIN communities c ON c.id = f.community_id ORDER BY f.path",
            )
            .expect("files stmt");
        let files: Vec<(String, Option<String>)> = file_stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("files query")
            .map(|r| r.expect("row"))
            .collect();

        // Group (label, path) rows by label — the query orders by c.id, f.path.
        let mut communities: Vec<Vec<String>> = Vec::new();
        let mut current_label: Option<String> = None;
        let mut current_members: Vec<String> = Vec::new();
        let rows: Vec<(String, String)> = conn
            .prepare(
                "SELECT c.label, f.path FROM communities c
                 JOIN community_members cm ON cm.community_id = c.id
                 JOIN files f ON f.id = cm.file_id ORDER BY c.id, f.path",
            )
            .expect("communities stmt")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("communities query")
            .map(|r| r.expect("row"))
            .collect();
        for (label, path) in rows {
            if current_label.as_deref() == Some(label.as_str()) {
                current_members.push(path);
            } else {
                if current_label.is_some() {
                    communities.push(std::mem::take(&mut current_members));
                }
                current_label = Some(label);
                current_members.push(path);
            }
        }
        if current_label.is_some() {
            communities.push(current_members);
        }
        communities.sort();

        // Group (band_id, entity name) rows by band_id.
        let mut clone_members: Vec<Vec<String>> = Vec::new();
        let mut current_band: Option<i64> = None;
        let mut current_band_members: Vec<String> = Vec::new();
        let band_rows: Vec<(i64, String)> = conn
            .prepare(
                "SELECT cbm.band_id, e.name FROM clone_band_members cbm
                 JOIN entities e ON e.id = cbm.entity_id ORDER BY cbm.band_id, e.name",
            )
            .expect("clones stmt")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("clones query")
            .map(|r| r.expect("row"))
            .collect();
        for (band, name) in band_rows {
            if current_band == Some(band) {
                current_band_members.push(name);
            } else {
                if current_band.is_some() {
                    clone_members.push(std::mem::take(&mut current_band_members));
                }
                current_band = Some(band);
                current_band_members.push(name);
            }
        }
        if current_band.is_some() {
            clone_members.push(current_band_members);
        }
        clone_members.sort();
        (files, communities, clone_members)
    }

    /// Phase 3 parity: after an edit that re-wires the graph, the global slice
    /// rebuilt by `ensure_fresh(Global)` matches a full `build` — communities,
    /// community_id denorm, and clone bands can't drift from the `build`
    /// command's.
    #[test]
    fn global_freshen_matches_full_build_after_edit() {
        with_isolated_home("global-parity", || {
            let root = temp_root("global-parity");
            let a = root.join("a.ts");
            let b = root.join("b.ts");
            let x = root.join("x.ts");
            let d = root.join("d.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&b, "import { bar } from \"./x\";\nbar();\n").expect("write b.ts");
            std::fs::write(
                &x,
                "export const foo = () => 1;\nexport const bar = () => 2;\n",
            )
            .expect("write x.ts");
            // d.ts is isolated: no imports — its own community (or none).
            std::fs::write(&d, "export const baz = () => 3;\n").expect("write d.ts");
            build(&root);

            // Re-wire the graph: d.ts joins the cluster via an import to x.ts.
            std::fs::write(
                &d,
                "import { foo } from \"./x\";\nexport const baz = () => 3;\n",
            )
            .expect("rewrite d.ts");

            ensure_fresh(&[Slice::Global], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen global succeeds");
            let sliced = global_projection(&root);

            build(&root);
            let full = global_projection(&root);
            assert_eq!(sliced, full, "global slice parity after edit");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Phase 3 no-op: with nothing changed, `ensure_fresh(Global)` leaves the
    /// global slice byte-identical (the warm fast path — the plan's "global
    /// recompute ... only when dirty").
    #[test]
    fn global_noop_when_nothing_changed() {
        with_isolated_home("global-noop", || {
            let root = temp_root("global-noop");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");
            build(&root);

            let before = global_projection(&root);
            let ledger_before = crate::persist::slice_built_through_rev(
                &crate::db::open(&db_of(&root)).expect("db opens"),
                "global",
            )
            .expect("ledger reads");

            ensure_fresh(&[Slice::Global], root.to_str().unwrap(), &Scope::Repo)
                .expect("no-op freshen succeeds");
            let after = global_projection(&root);
            let ledger_after = crate::persist::slice_built_through_rev(
                &crate::db::open(&db_of(&root)).expect("db opens"),
                "global",
            )
            .expect("ledger reads");

            assert_eq!(before, after, "no-op global freshen changes nothing");
            assert_eq!(ledger_before, ledger_after, "ledger untouched by no-op");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Phase 4 §4.2: build-on-miss targeting. A per-file `ensure_fresh(Raw,
    /// File)` against a missing DB indexes only that file (no full repo build).
    #[test]
    fn file_scope_targeted_first_build_indexes_only_that_file() {
        with_isolated_home("targeted-first", || {
            let root = temp_root("targeted-first");
            let a = root.join("a.ts");
            let b = root.join("b.ts");
            std::fs::write(&a, "export const foo = () => 1;\n").expect("write a.ts");
            std::fs::write(&b, "export const bar = () => 2;\n").expect("write b.ts");
            let a_str = a.to_str().unwrap();

            // No build, no DB — a File-scope raw freshen must build only a.ts.
            ensure_fresh(
                &[Slice::Raw],
                root.to_str().unwrap(),
                &Scope::File(a_str.to_string()),
            )
            .expect("targeted first-build succeeds");
            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .expect("count reads");
            assert_eq!(count, 1, "only the requested file is indexed");
            let only: String = conn
                .query_row("SELECT path FROM files", [], |r| r.get(0))
                .expect("path reads");
            assert!(only.ends_with("a.ts"), "the indexed file is a.ts: {only}");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Phase 4 §4.3: deletion handling. A deleted on-disk file removes its raw
    /// rows and dirties the derived slices — the next `ensure_fresh(Edges)`
    /// drops its edges and renormalizes fan metrics.
    #[test]
    fn deleted_file_removes_rows_and_dirties_derived_slices() {
        with_isolated_home("deletion", || {
            let root = temp_root("deletion");
            let a = root.join("a.ts");
            let b = root.join("b.ts");
            let x = root.join("x.ts");
            std::fs::write(
                &a,
                "import { foo } from \"./b\";\nimport { bar } from \"./x\";\nfoo();\nbar();\n",
            )
            .expect("write a.ts");
            std::fs::write(&b, "export const foo = () => 1;\n").expect("write b.ts");
            std::fs::write(&x, "export const bar = () => 2;\n").expect("write x.ts");
            build(&root);

            let b_str = b.to_str().unwrap();
            assert!(
                edges_of(&root).iter().any(|(from, to, _, _)| {
                    from.ends_with("a.ts") && to.as_deref().is_some_and(|t| t.ends_with("b.ts"))
                }),
                "a.ts reaches b.ts before deletion"
            );

            // Delete b.ts on disk — no manual build.
            std::fs::remove_file(&b).expect("delete b.ts");
            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen edges after deletion succeeds");

            // b.ts's raw row is gone and nothing references it.
            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            let b_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM files WHERE path = ?1", [b_str], |r| {
                    r.get(0)
                })
                .expect("count reads");
            assert_eq!(b_count, 0, "deleted file row removed");
            assert!(
                !edges_of(&root).iter().any(|(from, to, _, _)| {
                    from.ends_with("b.ts") || to.as_deref().is_some_and(|t| t.ends_with("b.ts"))
                }),
                "no edge references the deleted file: {:?}",
                edges_of(&root)
            );

            // Fan denorm renormalized: a.ts still reaches x.ts (import+call=2).
            let (_, a_fan_out) = fan_of(&root, a.to_str().unwrap());
            assert_eq!(a_fan_out, 2, "a.ts fan_out keeps only the x.ts edges");

            // Derived slices were dirtied and rebuilt: ledgers are current.
            for slice in ["imports", "edges"] {
                let built =
                    crate::persist::slice_built_through_rev(&conn, slice).expect("ledger reads");
                let max = crate::persist::max_rev(&conn).expect("max reads");
                assert!(
                    built.is_some_and(|b| b == max),
                    "{slice} ledger rebuilt to max"
                );
            }

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Phase 4 §1.4: concurrency. Two threads racing to freshen the same DB
    /// after an edit both succeed — the `SQLITE_BUSY` loser retries — and the
    /// final state equals a single sequential freshen.
    #[test]
    fn concurrent_freshens_serialize_and_match_sequential() {
        with_isolated_home("concurrent", || {
            let root = temp_root("concurrent");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");
            build(&root);

            // Edit both files so both freshens see real work.
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\nfoo();\n")
                .expect("edit a.ts");
            std::fs::write(
                &x,
                "export const foo = () => 1;\nexport const bar = () => 2;\n",
            )
            .expect("edit x.ts");

            let repo = root.to_str().unwrap().to_string();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let mut handles = Vec::new();
            for _ in 0..2 {
                let repo = repo.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    ensure_fresh(&[Slice::Global], &repo, &Scope::Repo)
                        .expect("concurrent freshen succeeds")
                }));
            }
            for handle in handles {
                handle.join().expect("thread joins");
            }

            let after_concurrent = (
                edges_of(&root),
                fan_of(&root, a.to_str().unwrap()),
                global_projection(&root),
            );

            // Sequential reference: rebuild fresh and single-freshen once.
            crate::persist::delete_global_rows(&crate::db::open(&db_of(&root)).expect("db opens"))
                .expect("wipe for reference");
            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full rebuild");
            let after_sequential = (
                edges_of(&root),
                fan_of(&root, a.to_str().unwrap()),
                global_projection(&root),
            );
            assert_eq!(after_concurrent.0, after_sequential.0, "edge parity");
            assert_eq!(after_concurrent.1, after_sequential.1, "fan parity");
            assert_eq!(after_concurrent.2, after_sequential.2, "global parity");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Phase 4 §4.4: observability. `dump_state` reports the derived-slice
    /// ledger: fresh after a build, stale after an edit, fresh again after
    /// `ensure_fresh(Global)`, and "missing" when no index exists.
    #[test]
    fn dump_state_reports_ledger_and_staleness() {
        with_isolated_home("dump-state", || {
            let root = temp_root("dump-state");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");

            // Missing DB → "missing".
            let missing = crate::slice::dump_state(&serde_json::json!({
                "repoRoot": root.to_str().unwrap()
            }))
            .expect("dump on missing db");
            assert_eq!(missing["db"], "missing", "{missing}");

            build(&root);
            let fresh = crate::slice::dump_state(&serde_json::json!({
                "repoRoot": root.to_str().unwrap()
            }))
            .expect("dump after build");
            assert_eq!(
                fresh["db_path"].as_str().unwrap(),
                db_of(&root).to_str().unwrap()
            );
            for slice in ["imports", "edges", "global"] {
                assert_eq!(
                    fresh["slices"][slice]["stale"], false,
                    "{slice} fresh: {fresh}"
                );
            }

            // Edit the source, then freshen ONLY the raw slice (rev advances,
            // derived ledgers stay behind) → dump reports the derived slices
            // as stale. The dump itself never freshens.
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\nfoo();\n")
                .expect("edit a.ts");
            ensure_fresh(&[Slice::Raw], root.to_str().unwrap(), &Scope::Repo)
                .expect("raw-only freshen");
            let stale = crate::slice::dump_state(&serde_json::json!({
                "repoRoot": root.to_str().unwrap()
            }))
            .expect("dump after raw-only freshen");
            for slice in ["imports", "edges", "global"] {
                assert_eq!(
                    stale["slices"][slice]["stale"], true,
                    "{slice} stale: {stale}"
                );
            }

            // Freshen the derived layer → fresh again.
            ensure_fresh(&[Slice::Global], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen global");
            let refreshed = crate::slice::dump_state(&serde_json::json!({
                "repoRoot": root.to_str().unwrap()
            }))
            .expect("dump after freshen");
            for slice in ["imports", "edges", "global"] {
                assert_eq!(
                    refreshed["slices"][slice]["stale"], false,
                    "{slice} refreshed: {refreshed}"
                );
            }

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// The git-status fast path is opt-in: with no override (and no
    /// `VARDE_GIT_FAST_PATH` in the shell — tests ignore the env var), a
    /// touch-only mtime change is a *change* under the default mtime walk,
    /// where the git path would no-op it (git hashes and sees identical
    /// content). Release benchmark (2026-09-03): the git path measured 2-5x
    /// slower than the walk on this machine, so the walk is the default.
    #[test]
    fn changed_files_defaults_to_walk_without_fast_path() {
        with_isolated_home("git-default-walk", || {
            assert!(
                !git_fast_path_enabled(),
                "fast path is opt-in; the walk is the default"
            );
            let root = temp_root("git-default-walk");
            run_git(&root, &["init", "-q"]);
            run_git(&root, &["config", "user.email", "test@example.com"]);
            run_git(&root, &["config", "user.name", "Default Walk Test"]);
            let a = root.join("a.rs");
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            run_git(&root, &["add", "a.rs"]);
            run_git(&root, &["commit", "-q", "-m", "init"]);
            build(&root);

            // Touch: mtime changes, content does not.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs();
            std::fs::File::options()
                .write(true)
                .open(&a)
                .expect("opens a")
                .set_times(
                    std::fs::FileTimes::new()
                        .set_accessed(
                            std::time::SystemTime::UNIX_EPOCH
                                + std::time::Duration::from_secs(now + 10),
                        )
                        .set_modified(
                            std::time::SystemTime::UNIX_EPOCH
                                + std::time::Duration::from_secs(now + 10),
                        ),
                )
                .expect("touches a");

            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            let (changed, _, _) =
                changed_files(&conn, root.to_str().unwrap(), &Scope::Repo).expect("changed_files");
            assert!(
                changed.iter().any(|p| p.ends_with("a.rs")),
                "walk is the default and sees the mtime change: {changed:?}"
            );

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Cost A parity: the git-status fast path returns the same `(changed,
    /// deleted)` set as the full walk + `classify_files` on a git-tracked
    /// fixture, for content edits, a new (untracked) file, and a deletion.
    #[test]
    fn git_changed_files_matches_full_walk_on_tracked_fixture() {
        with_isolated_home("git-parity", || {
            let root = temp_root("git-parity");
            run_git(&root, &["init", "-q"]);
            run_git(&root, &["config", "user.email", "test@example.com"]);
            run_git(&root, &["config", "user.name", "Parity Test"]);
            let a = root.join("a.rs");
            let b = root.join("b.rs");
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            std::fs::write(&b, "fn b() {}\n").expect("writes b");
            run_git(&root, &["add", "a.rs", "b.rs"]);
            run_git(&root, &["commit", "-q", "-m", "init"]);
            build(&root);

            // Content edit + new untracked file + deletion.
            std::fs::write(&a, "fn a() { let x = 1; }\n").expect("edits a");
            std::fs::write(root.join("c.rs"), "fn c() {}\n").expect("writes c");
            std::fs::remove_file(&b).expect("deletes b");

            let (git_changed, git_deleted, git_set) = {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                git_changed_files(&conn, root.to_str().unwrap()).expect("git path succeeds")
            };

            // Reference: the full walk + mtime classify on the same tree. The
            // walk's `.hidden(false)` includes `.git/*` internals, which git
            // status never reports (git treats `.git` as non-source); the git
            // path is strictly more correct there, so the parity assertion
            // compares the source-file sets (paths outside `.git/`).
            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            let current = crate::scan::list_source_files(root.to_str().unwrap()).expect("walks");
            let stored = crate::persist::load_file_states(&conn).expect("stored");
            let mut walk_changed = Vec::new();
            let mut walk_deleted = Vec::new();
            let mut walk_set = false;
            for c in classify_files(&stored, &current) {
                let in_git_dir = match &c {
                    FileClassification::Unchanged { path }
                    | FileClassification::Changed { path }
                    | FileClassification::New { path }
                    | FileClassification::Deleted { path } => Path::new(path)
                        .components()
                        .any(|c| c.as_os_str() == ".git"),
                };
                if in_git_dir {
                    continue;
                }
                match c {
                    FileClassification::Changed { path } => walk_changed.push(path),
                    FileClassification::New { path } => {
                        walk_changed.push(path);
                        walk_set = true;
                    }
                    FileClassification::Deleted { path } => {
                        walk_deleted.push(path);
                        walk_set = true;
                    }
                    FileClassification::Unchanged { .. } => {}
                }
            }

            let mut gc = git_changed.clone();
            let mut wc = walk_changed.clone();
            gc.sort();
            wc.sort();
            let mut gd = git_deleted.clone();
            let mut wd = walk_deleted.clone();
            gd.sort();
            wd.sort();
            assert_eq!(
                gc, wc,
                "changed set parity: git {git_changed:?} vs walk {walk_changed:?}"
            );
            assert_eq!(
                gd, wd,
                "deleted set parity: git {git_deleted:?} vs walk {walk_deleted:?}"
            );
            assert_eq!(git_set, walk_set, "file-set-changed flag parity");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Cost A fallback: a non-git temp dir has no worktree, so `git status`
    /// fails and `changed_files` falls back to the full walk — a repo without
    /// git behaves exactly as before.
    #[test]
    fn changed_files_falls_back_to_walk_on_non_git_root() {
        with_isolated_home("git-fallback", || {
            let root = temp_root("git-fallback");
            let a = root.join("a.rs");
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            build(&root);

            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            std::fs::write(&a, "fn a() { let x = 1; }\n").expect("edits a");
            let (changed, deleted, set) =
                changed_files(&conn, root.to_str().unwrap(), &Scope::Repo).expect("changed_files");
            assert_eq!(changed.len(), 1, "walk path finds the edit: {changed:?}");
            assert!(changed[0].ends_with("a.rs"));
            assert!(deleted.is_empty());
            assert!(!set, "content edit only, no file-set change");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Cost A semantic note (documented in `changed_files`): git status is
    /// content-comparison-based, so a `touch`ed file with no content change is
    /// a no-op — where the mtime-based walk previously counted it as changed
    /// and triggered a byte-identical reparse.
    #[test]
    fn touch_only_edit_is_noop_under_git_path() {
        with_isolated_home("git-touch", || {
            let _fast_path = FastPathTestGuard::on();
            let root = temp_root("git-touch");
            run_git(&root, &["init", "-q"]);
            run_git(&root, &["config", "user.email", "test@example.com"]);
            run_git(&root, &["config", "user.name", "Touch Test"]);
            let a = root.join("a.rs");
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            run_git(&root, &["add", "a.rs"]);
            run_git(&root, &["commit", "-q", "-m", "init"]);
            build(&root);

            let before_rev: i64 = {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                conn.query_row("SELECT rev FROM files", [], |r| r.get(0))
                    .expect("reads rev")
            };

            // Touch: mtime changes, content does not.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs();
            std::fs::File::options()
                .write(true)
                .open(&a)
                .expect("opens a")
                .set_times(
                    std::fs::FileTimes::new()
                        .set_accessed(
                            std::time::SystemTime::UNIX_EPOCH
                                + std::time::Duration::from_secs(now + 10),
                        )
                        .set_modified(
                            std::time::SystemTime::UNIX_EPOCH
                                + std::time::Duration::from_secs(now + 10),
                        ),
                )
                .expect("touches a");

            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen succeeds");

            let after_rev: i64 = {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                conn.query_row("SELECT rev FROM files", [], |r| r.get(0))
                    .expect("reads rev")
            };
            assert_eq!(
                before_rev, after_rev,
                "touch with no content change is a no-op under the git path"
            );

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Cost B: a single-file edit on a multi-file repo re-resolves and
    /// rewrites edges ONLY for the changed file + its direct reverse-
    /// dependents — an unrelated file's edge rows keep their exact rowids
    /// (never deleted/reinserted), while the changed file's and its callers'
    /// rows are replaced. Proven by rowid identity: `resolved_edges.id` is
    /// AUTOINCREMENT, so a rewritten file's rows get new ids; an untouched
    /// file's rows keep theirs.
    #[test]
    fn edges_freshen_scoped_rewrite_touches_only_changed_and_reverse_dependents() {
        with_isolated_home("edges-scoped", || {
            let root = temp_root("edges-scoped");
            // a.ts and b.ts both import+call x.ts (both are reverse-dependents
            // of a changed x.ts); d.ts exports baz; e.ts imports d.ts so it has
            // its own edge row but is untouched by the x.ts edit.
            let a = root.join("a.ts");
            let b = root.join("b.ts");
            let x = root.join("x.ts");
            let d = root.join("d.ts");
            let e = root.join("e.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("a");
            std::fs::write(&b, "import { bar } from \"./x\";\nbar();\n").expect("b");
            std::fs::write(
                &x,
                "export const foo = () => 1;\nexport const bar = () => 2;\n",
            )
            .expect("x");
            std::fs::write(&d, "export const baz = () => 3;\n").expect("d");
            std::fs::write(&e, "import { baz } from \"./d\";\nbaz();\n").expect("e");
            build(&root);

            // Capture edge rowids grouped by from-file before the edit.
            let rowids_by_file =
                |root: &std::path::Path| -> std::collections::HashMap<String, Vec<i64>> {
                    let conn = crate::db::open(&db_of(root)).expect("db opens");
                    let mut stmt = conn
                    .prepare(
                        "SELECT f.path, e.id FROM resolved_edges e JOIN files f ON f.id = e.from_file_id ORDER BY e.id",
                    )
                    .expect("stmt");
                    let mut map: std::collections::HashMap<String, Vec<i64>> = Default::default();
                    for row in stmt
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                        .expect("query")
                    {
                        let (path, id) = row.expect("row");
                        map.entry(path).or_default().push(id);
                    }
                    map
                };
            let before = rowids_by_file(&root);
            let before_a = before.get(a.to_str().unwrap()).cloned().unwrap_or_default();
            let before_e = before.get(e.to_str().unwrap()).cloned().unwrap_or_default();
            assert!(!before_a.is_empty(), "a.ts has edges");
            assert!(!before_e.is_empty(), "e.ts has edges");

            // Edit only x.ts (rename its exports) — a.ts/b.ts/d.ts/e.ts untouched.
            std::fs::write(
                &x,
                "export const foo2 = () => 1;\nexport const bar2 = () => 2;\n",
            )
            .expect("rewrite x");
            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen edges");

            let after = rowids_by_file(&root);
            let after_e = after.get(e.to_str().unwrap()).cloned().unwrap_or_default();
            assert_eq!(
                before_e, after_e,
                "untouched e.ts edges keep their rowids (not rewritten)"
            );

            // The changed file x.ts's reverse-dependent callers (a.ts, b.ts)
            // got rewritten: their old rowids are gone.
            for (path, name) in [(&a, "a.ts"), (&b, "b.ts")] {
                let old = before
                    .get(path.to_str().unwrap())
                    .cloned()
                    .unwrap_or_default();
                let new = after
                    .get(path.to_str().unwrap())
                    .cloned()
                    .unwrap_or_default();
                assert!(
                    old.iter().all(|id| !new.contains(id)),
                    "{name} edges rewritten (old rowids dropped): old={old:?} new={new:?}"
                );
                assert_eq!(old.len(), new.len(), "{name} edge count preserved");
            }

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Cost B: the scoped rewrite's fan_in/fan_out matches a full rebuild, for
    /// both the changed file and its edge targets.
    #[test]
    fn edges_freshen_scoped_fan_matches_full_build() {
        with_isolated_home("edges-scoped-fan", || {
            let root = temp_root("edges-scoped-fan");
            let a = root.join("a.ts");
            let b = root.join("b.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("a");
            std::fs::write(&b, "import { bar } from \"./x\";\nbar();\n").expect("b");
            std::fs::write(
                &x,
                "export const foo = () => 1;\nexport const bar = () => 2;\n",
            )
            .expect("x");
            build(&root);

            // Edit only x.ts: foo -> foo2 (a.ts's call edge retargets/unresolves).
            std::fs::write(
                &x,
                "export const foo2 = () => 1;\nexport const bar = () => 2;\n",
            )
            .expect("rewrite x");
            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen edges");
            let sliced_fans = (
                fan_of(&root, a.to_str().unwrap()),
                fan_of(&root, b.to_str().unwrap()),
                fan_of(&root, x.to_str().unwrap()),
            );

            build(&root);
            let full_fans = (
                fan_of(&root, a.to_str().unwrap()),
                fan_of(&root, b.to_str().unwrap()),
                fan_of(&root, x.to_str().unwrap()),
            );
            assert_eq!(sliced_fans, full_fans, "scoped fan parity vs full rebuild");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Cost B: the scoped rewrite stamps `files.edges_built_rev` for the
    /// files it recomputed, and leaves untouched files' stamp alone.
    #[test]
    fn edges_freshen_stamps_edges_built_rev_only_for_rescoped_files() {
        with_isolated_home("edges-built-rev", || {
            let root = temp_root("edges-built-rev");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            let d = root.join("d.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("a");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("x");
            std::fs::write(&d, "export const baz = () => 3;\n").expect("d");
            build(&root);

            std::fs::write(&x, "export const foo2 = () => 1;\n").expect("rewrite x");
            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen edges");

            let built_rev = |root: &std::path::Path, path: &str| -> i64 {
                let conn = crate::db::open(&db_of(root)).expect("db opens");
                conn.query_row(
                    "SELECT edges_built_rev FROM files WHERE path = ?1",
                    [path],
                    |r| r.get(0),
                )
                .expect("reads edges_built_rev")
            };
            // x.ts was re-resolved → its stamp equals its own rev (>= 1).
            assert!(built_rev(&root, x.to_str().unwrap()) >= 1, "x.ts stamped");
            // d.ts was untouched → stamp still 0.
            assert_eq!(built_rev(&root, d.to_str().unwrap()), 0, "d.ts not stamped");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// CORRECTNESS-101 + CODE-204.1: a staged rename (`git mv`) must not
    /// misparse under the git-status fast path — `-c status.renames=false`
    /// renders it as separate `D old` / `A new` records, so the old path lands
    /// in `deleted` (its rows get removed) and the new path in `changed`,
    /// with `file_set_changed` set.
    #[test]
    fn git_changed_files_staged_rename_marks_old_deleted_new_changed() {
        with_isolated_home("git-rename", || {
            let root = temp_root("git-rename");
            run_git(&root, &["init", "-q"]);
            run_git(&root, &["config", "user.email", "test@example.com"]);
            run_git(&root, &["config", "user.name", "Rename Test"]);
            let a = root.join("a.rs");
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            run_git(&root, &["add", "a.rs"]);
            run_git(&root, &["commit", "-q", "-m", "init"]);
            build(&root);

            // Staged rename: a.rs -> renamed.rs (still in the index as a
            // deletion + addition, not yet committed).
            run_git(&root, &["mv", "a.rs", "renamed.rs"]);

            let (changed, deleted, set) = {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                git_changed_files(&conn, root.to_str().unwrap()).expect("git path succeeds")
            };
            assert!(
                changed.iter().any(|p| p.ends_with("renamed.rs")),
                "new path in changed: {changed:?}"
            );
            assert!(
                deleted.iter().any(|p| p.ends_with("a.rs")),
                "old path in deleted: {deleted:?}"
            );
            assert!(set, "a rename is a file-set change");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// CORRECTNESS-102: the DB built from a dirty worktree must keep using
    /// the walk (which sees mtimes) until a build observes a clean worktree —
    /// a revert to committed content is invisible to `git status` but must
    /// still reparse.
    #[test]
    fn revert_to_committed_after_dirty_build_uses_walk() {
        with_isolated_home("git-revert", || {
            let _fast_path = FastPathTestGuard::on();
            let root = temp_root("git-revert");
            run_git(&root, &["init", "-q"]);
            run_git(&root, &["config", "user.email", "test@example.com"]);
            run_git(&root, &["config", "user.name", "Revert Test"]);
            let a = root.join("a.rs");
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            run_git(&root, &["add", "a.rs"]);
            run_git(&root, &["commit", "-q", "-m", "init"]);

            // Build while the worktree is dirty (uncommitted edit), then
            // revert the edit to its committed content.
            std::fs::write(&a, "fn a() { let x = 1; }\n").expect("dirty edit");
            build(&root);
            run_git(&root, &["checkout", "--", "a.rs"]);

            // The blind spot, documented: git status reports *clean* — only
            // the mtime walk can see the revert.
            let status =
                crate::git::run_git(&["status", "--porcelain=v1", "-z"], &root).expect("git runs");
            assert!(status.is_empty(), "git status is clean after the revert");

            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            let (changed, _, _) =
                changed_files(&conn, root.to_str().unwrap(), &Scope::Repo).expect("changed_files");
            assert!(
                changed.iter().any(|p| p.ends_with("a.rs")),
                "walk path sees the revert: {changed:?}"
            );

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// CORRECTNESS-103: a stored file that became gitignored is invisible to
    /// `git status`, so the fast path cross-checks stored files against
    /// `git ls-files` (minus tracked-but-ignored) and drops it as deleted —
    /// matching what the walk would classify.
    #[test]
    fn newly_gitignored_stored_file_deleted_by_fast_path() {
        with_isolated_home("git-ignore", || {
            let _fast_path = FastPathTestGuard::on();
            let root = temp_root("git-ignore");
            run_git(&root, &["init", "-q"]);
            run_git(&root, &["config", "user.email", "test@example.com"]);
            run_git(&root, &["config", "user.name", "Ignore Test"]);
            let a = root.join("a.rs");
            let b = root.join("b.rs");
            std::fs::write(&a, "fn a() {}\n").expect("writes a");
            std::fs::write(&b, "fn b() {}\n").expect("writes b");
            run_git(&root, &["add", "a.rs", "b.rs"]);
            run_git(&root, &["commit", "-q", "-m", "init"]);
            build(&root);

            // b.rs is tracked but now matches a `.gitignore` entry — git
            // status still reports nothing for it (unchanged), and the walk
            // would drop it because the ignore crate excludes gitignored
            // files regardless of tracking.
            std::fs::write(root.join(".gitignore"), "b.rs\n").expect("gitignore");

            // Fast path (direct): b.rs must be classified deleted.
            let (changed, deleted, set) = {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                git_changed_files(&conn, root.to_str().unwrap()).expect("git path succeeds")
            };
            assert!(
                deleted.iter().any(|p| p.ends_with("b.rs")),
                "fast path drops now-ignored b.rs: {deleted:?}"
            );
            assert!(set, "dropping a stored file is a file-set change");
            assert!(
                changed.iter().any(|p| p.ends_with(".gitignore")),
                "the .gitignore itself is new: {changed:?}"
            );

            // Walk path agrees (forced via the dirty-build gate): the ignore
            // crate excludes b.rs, so classify marks it Deleted too.
            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            crate::persist::set_slice_meta_value(&conn, "git_clean_at_build", 0).expect("flag set");
            let (_, deleted_walk, set_walk) =
                changed_files(&conn, root.to_str().unwrap(), &Scope::Repo).expect("changed_files");
            assert!(
                deleted_walk.iter().any(|p| p.ends_with("b.rs")),
                "walk path drops b.rs too: {deleted_walk:?}"
            );
            assert!(set_walk, "walk path also flags the file-set change");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// CORRECTNESS-104: a stale set larger than the scoped-path cap falls
    /// back to the full re-resolve instead of erroring — proven by a
    /// non-stale file's edge rows being rewritten (only the full path deletes
    /// and reinserts every edge).
    #[test]
    fn scoped_path_falls_back_to_full_rewrite_on_large_stale_set() {
        with_isolated_home("edges-cap", || {
            let root = temp_root("edges-cap");
            // 510 files: f000 imports f001 (so it has an edge row); f002..
            // f509 are standalone. After the SQL rev bump, 508 files are
            // stale — past the MAX_SCOPED_FILES = 500 cap.
            let mut paths = Vec::new();
            for i in 0..510 {
                let p = root.join(format!("f{i:03}.ts"));
                if i == 0 {
                    std::fs::write(&p, "import { v1 } from \"./f001\";\nv1();\n").expect("f000");
                } else {
                    std::fs::write(&p, format!("export const v{i} = {i};\n")).expect("file");
                }
                paths.push(p);
            }
            build(&root);

            // Force 508 files stale (f000/f001 stay fresh) via the per-file
            // ledger, without touching the worktree.
            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            conn.execute("UPDATE files SET rev = rev + 1", [])
                .expect("bump revs");
            conn.execute(
                "UPDATE files SET rev = 0 WHERE path LIKE '%/f000.ts' OR path LIKE '%/f001.ts'",
                [],
            )
            .expect("keep f000/f001 fresh");
            drop(conn);

            let rowid_before: i64 = {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                conn.query_row(
                    "SELECT e.id FROM resolved_edges e JOIN files f ON f.id = e.from_file_id
                     WHERE f.path LIKE '%/f000.ts' ORDER BY e.id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .expect("f000 edge rowid")
            };

            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("large stale set falls back to the full path, no error");

            let rowid_after: i64 = {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                conn.query_row(
                    "SELECT e.id FROM resolved_edges e JOIN files f ON f.id = e.from_file_id
                     WHERE f.path LIKE '%/f000.ts' ORDER BY e.id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .expect("f000 edge rowid")
            };
            assert_ne!(
                rowid_before, rowid_after,
                "full path rewrote f000's edge rows (new rowids) — the cap fired"
            );

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// CODE-204.2: drive a real query mode (`dependencies`) end-to-end through
    /// the FRESHNESS table + scoped `freshen_edges`. A single-file edit is
    /// reflected in the answer, and an unrelated file's edge rows keep their
    /// exact rowids (proving the scoped path ran inside the query, not just
    /// under a direct `ensure_fresh` call).
    #[test]
    fn dependencies_through_run_mode_uses_scoped_freshen() {
        with_isolated_home("e2e-scoped", || {
            let root = temp_root("e2e-scoped");
            let a = root.join("a.ts");
            let b = root.join("b.ts");
            let x = root.join("x.ts");
            let d = root.join("d.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("a");
            std::fs::write(&b, "import { baz } from \"./d\";\nbaz();\n").expect("b");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("x");
            std::fs::write(&d, "export const baz = () => 3;\n").expect("d");
            build(&root);

            let rowids_of = |root: &std::path::Path, path: &str| -> Vec<i64> {
                let conn = crate::db::open(&db_of(root)).expect("db opens");
                let mut stmt = conn
                    .prepare(
                        "SELECT e.id FROM resolved_edges e JOIN files f ON f.id = e.from_file_id
                         WHERE f.path = ?1 ORDER BY e.id",
                    )
                    .expect("stmt");
                stmt.query_map([path], |r| r.get(0))
                    .expect("query")
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .expect("rows")
            };
            let b_before = rowids_of(&root, b.to_str().unwrap());
            assert!(!b_before.is_empty(), "b.ts has edges");
            let a_before = rowids_of(&root, a.to_str().unwrap());
            assert!(!a_before.is_empty(), "a.ts has edges");

            // Edit x.ts so it gains its own dependency on d.ts: the
            // `dependencies` answer for a.ts grows a.ts -> x.ts -> d.ts.
            std::fs::write(
                &x,
                "import { baz } from \"./d\";\nbaz();\nexport const foo = () => 1;\n",
            )
            .expect("rewrite x");

            let input = serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "filePath": a.to_str().unwrap(),
            });
            let out = crate::query::run_mode("dependencies", &input.to_string());
            let parsed: serde_json::Value =
                serde_json::from_str(&out).expect("valid JSON envelope");
            assert_eq!(parsed["ok"], true, "dependencies ok: {out}");
            let files: Vec<String> = parsed["data"]
                .as_array()
                .expect("dependencies data array")
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            assert!(
                files.iter().any(|p| p.ends_with("x.ts")),
                "a.ts still depends on x.ts: {files:?}"
            );
            assert!(
                files.iter().any(|p| p.ends_with("d.ts")),
                "x.ts's new dependency on d.ts is in the answer: {files:?}"
            );
            assert!(
                !files.iter().any(|p| p.ends_with("b.ts")),
                "unrelated b.ts not in a.ts's dependencies: {files:?}"
            );

            // The scoped path ran inside run_mode: b.ts (unrelated) kept its
            // edge rowids, while a.ts (a reverse-dependent of the edited x.ts)
            // was rewritten.
            let b_after = rowids_of(&root, b.to_str().unwrap());
            assert_eq!(
                b_before, b_after,
                "unrelated b.ts edges untouched by the scoped freshen"
            );
            let a_after = rowids_of(&root, a.to_str().unwrap());
            assert!(
                a_before.iter().all(|id| !a_after.contains(id)),
                "a.ts (reverse-dependent) rewritten: {a_before:?} -> {a_after:?}"
            );

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A scoped edge refresh (rev-only edit, no file added/removed) must
    /// patch an already-cached `graph_cache` blob using the
    /// `patch_graph_cache_scoped` diff rather than leaving it stale, and the
    /// patched result must be indistinguishable from a fresh SQL reload.
    #[test]
    fn scoped_refresh_patches_graph_cache_to_match_full_reload() {
        with_isolated_home("cache-scoped", || {
            let root = temp_root("cache-scoped");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");
            build(&root);

            // Seed graph_cache directly (the eager full-build cache write is
            // a sibling task not yet wired into `build`), so the scoped path
            // below has an existing cache to patch.
            {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                crate::persist::rebuild_graph_cache_full(&conn).expect("seed cache");
            }

            // Rename the export: foo disappears from x.ts. This is a
            // rev-only edit (no file added/removed), so `freshen_edges`
            // takes the scoped path and `run_scoped_edge_refresh` patches
            // the cache in place instead of rebuilding it.
            std::fs::write(&x, "export const bar = () => 1;\n").expect("rewrite x.ts");

            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen edges succeeds");

            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            let patched = crate::persist::read_graph_cache(&conn)
                .expect("cache reads")
                .expect("cache row present after scoped patch");
            let fresh = crate::query::graph::Graph::load(&conn).expect("graph loads from sql");
            assert_eq!(patched, fresh, "patched cache matches fresh SQL reload");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// The full edge re-resolve path (file set changed) must rebuild
    /// `graph_cache` from scratch: the blob/rev row is present afterward and
    /// deserializes to a `Graph` matching a fresh full derivation.
    #[test]
    fn full_path_edge_refresh_rebuilds_graph_cache() {
        with_isolated_home("cache-full", || {
            let root = temp_root("cache-full");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");
            build(&root);

            let rev_before: i64 = {
                let conn = crate::db::open(&db_of(&root)).expect("db opens");
                conn.query_row("SELECT rev FROM graph_cache WHERE id = 1", [], |r| r.get(0))
                    .expect(
                        "build() now eagerly writes graph_cache (eager-cache-write-on-full-build), \
                         so a row exists immediately after the initial full build",
                    )
            };

            // Add a new file: the file *set* changed, so `freshen_edges`
            // takes the full re-resolve path (`full_path` in
            // `freshen_edges`), which must rebuild the cache from scratch.
            let y = root.join("y.ts");
            std::fs::write(&y, "export const qux = () => 2;\n").expect("write y.ts");
            std::fs::write(
                &a,
                "import { foo } from \"./x\";\nimport { qux } from \"./y\";\nfoo();\nqux();\n",
            )
            .expect("rewrite a.ts");

            ensure_fresh(&[Slice::Edges], root.to_str().unwrap(), &Scope::Repo)
                .expect("freshen edges succeeds");

            let conn = crate::db::open(&db_of(&root)).expect("db opens");
            let cached = crate::persist::read_graph_cache(&conn)
                .expect("cache reads")
                .expect("cache row present after full-path rebuild");
            let rev_after: i64 = conn
                .query_row("SELECT rev FROM graph_cache WHERE id = 1", [], |r| r.get(0))
                .expect("rev present");
            let ledger_rev = crate::persist::max_rev(&conn).expect("max_rev reads");
            assert_eq!(
                rev_after, ledger_rev,
                "cache rev is stamped with the current ledger rev (max_rev), \
                 not an independent counter — this is what lets Graph::load's \
                 cache-first check compare against the same value freshen_edges uses"
            );
            assert!(
                rev_after > rev_before,
                "the new-file full re-resolve must bump the cache's rev past the \
                 eager write build() already made"
            );
            let fresh = crate::query::graph::Graph::load(&conn).expect("graph loads from sql");
            assert_eq!(cached, fresh, "full-path cache matches fresh SQL reload");

            let _ = std::fs::remove_file(db_of(&root));
            let _ = std::fs::remove_dir_all(&root);
        });
    }
}
