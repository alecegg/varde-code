//! Graph-traversal query modes: dependencies, dependents, blast_radius,
//! symbol_blast_radius, type_hierarchy, explore.
//!
//! Performance contract: the whole resolved-edge adjacency is loaded in a
//! constant number of SQL statements (two: files + resolved edges), and all
//! traversal happens in memory with visited-set BFS, so statement count is
//! O(1) in the number of nodes and cyclic input always terminates.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, HashSet, VecDeque};

use super::{ApiError, db_err, freshen_for_mode, open_db, opt_str, req_str};
use crate::query::simple::{file_id, matches_path};

/// In-memory adjacency snapshot of the persisted resolved graph.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct Graph {
    /// files.id -> path
    paths: HashMap<i64, String>,
    /// files.id -> resolved outgoing targets
    out: HashMap<i64, Vec<i64>>,
    /// files.id -> resolved incoming sources
    inc: HashMap<i64, Vec<i64>>,
}

impl Graph {
    /// Load the full adjacency, cache-first.
    ///
    /// Tries the `graph_cache` row first: if its stored `rev` matches the
    /// current ledger value ([`crate::persist::max_rev`]) and the blob
    /// decodes cleanly, it's returned directly with no SQL scan. On a miss —
    /// no row, a stale/mismatched `rev`, or a decode failure — this
    /// falls through to the full SQL-scan rebuild below unchanged, then
    /// writes a fresh cache afterward so the next call hits the fast path
    /// (self-healing). A decode/version mismatch is never a hard error; it's
    /// treated identically to a missing row.
    ///
    /// Concurrency: this runs on a query connection with **no repo lock held**
    /// (`freshen` released it before the mode called us) and possibly while a
    /// build commits on another connection. The miss-path rebuild and the
    /// self-heal write are therefore wrapped in one deferred transaction so
    /// the `files`/`resolved_edges` reads *and* the `rev` [`write_graph_cache`]
    /// stamps all come from a single snapshot. Without this, a build that
    /// bumped `max_rev` between the edge read and the stamp would make us tag
    /// an older (or torn) graph with the *new* `rev`, which a later
    /// `read_graph_cache_if_fresh` would wrongly accept as fresh and serve as
    /// stale results until the next `rev` change. If the snapshot can't be
    /// promoted to a write at commit (another writer moved the DB on), the
    /// self-heal is skipped — best-effort — and the snapshot-consistent graph
    /// is still returned.
    pub fn load(conn: &Connection) -> std::result::Result<Self, ApiError> {
        if let Some(graph) = crate::persist::read_graph_cache_if_fresh(conn).map_err(other_err)? {
            #[cfg(test)]
            crate::GRAPH_CACHE_HIT_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(graph);
        }
        // Deferred transaction pins one snapshot for both the rebuild reads and
        // the `rev` stamp (see the concurrency note above). Never re-enters
        // `Graph::load` — `load_uncached` reads SQL directly — so no recursion.
        let tx = conn.unchecked_transaction().map_err(db_err)?;
        let graph = Self::load_uncached(&tx)?;
        // Self-healing, best-effort: a write failure (e.g. the snapshot lost a
        // race to a concurrent build) must not fail a query that already has a
        // correct in-memory `Graph`. Stamping happens inside the snapshot, so
        // the cached `rev` always matches the cached blob.
        let _ = crate::persist::write_graph_cache(&tx, &graph);
        let _ = tx.commit();
        Ok(graph)
    }

    /// Full SQL-scan rebuild, in a constant number of statements. Never
    /// consults or writes the cache — the cache-first/self-healing logic
    /// lives in [`Graph::load`]. `pub(crate)` so
    /// [`crate::persist::rebuild_graph_cache_full`] can force a fresh
    /// from-SQL rebuild without re-entering `Graph::load`'s cache check.
    pub(crate) fn load_uncached(conn: &Connection) -> std::result::Result<Self, ApiError> {
        let mut paths = HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT id, path FROM files").map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map_err(db_err)?;
            for row in rows {
                let (id, path) = row.map_err(db_err)?;
                paths.insert(id, path);
            }
        }

        let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut inc: HashMap<i64, Vec<i64>> = HashMap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT from_file_id, to_file_id FROM resolved_edges
                     WHERE resolved = 1 AND to_file_id IS NOT NULL",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
                .map_err(db_err)?;
            for row in rows {
                let (from, to) = row.map_err(db_err)?;
                out.entry(from).or_default().push(to);
                inc.entry(to).or_default().push(from);
            }
        }
        Ok(Graph { paths, out, inc })
    }

    /// Remove and return `from`'s full outgoing adjacency list (empty if
    /// absent). Used by the scoped cache-patch path (`persist::
    /// patch_graph_cache_scoped`) to discard a rescoped file's stale out
    /// edges before re-inserting the freshly resolved ones.
    pub(crate) fn take_out(&mut self, from: i64) -> Vec<i64> {
        self.out.remove(&from).unwrap_or_default()
    }

    /// Remove every occurrence of `from` from `to`'s incoming backlink list
    /// (the counterpart of [`Graph::take_out`] on the reverse map), dropping
    /// the entry entirely once empty so a stale key doesn't linger.
    pub(crate) fn remove_inc(&mut self, to: i64, from: i64) {
        if let Some(list) = self.inc.get_mut(&to) {
            list.retain(|&x| x != from);
            if list.is_empty() {
                self.inc.remove(&to);
            }
        }
    }

    /// Record a freshly resolved `from -> to` edge in both adjacency maps.
    pub(crate) fn push_edge(&mut self, from: i64, to: i64) {
        self.out.entry(from).or_default().push(to);
        self.inc.entry(to).or_default().push(from);
    }

    /// Forward BFS (things this file depends on / reaches).
    fn forward(&self, start: i64, max_depth: Option<usize>) -> Vec<i64> {
        self.bfs(start, true, max_depth)
    }

    /// Reverse BFS (files that depend on / reach this file).
    fn reverse(&self, start: i64, max_depth: Option<usize>) -> Vec<i64> {
        self.bfs(start, false, max_depth)
    }

    fn bfs(&self, start: i64, forward: bool, max_depth: Option<usize>) -> Vec<i64> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back((start, 0usize));
        visited.insert(start);
        while let Some((node, depth)) = queue.pop_front() {
            if let Some(limit) = max_depth
                && depth >= limit
            {
                continue;
            }
            let neighbors = if forward {
                self.out.get(&node)
            } else {
                self.inc.get(&node)
            };
            if let Some(neighbors) = neighbors {
                for next in neighbors {
                    if visited.insert(*next) {
                        queue.push_back((*next, depth + 1));
                    }
                }
            }
        }
        let mut result: Vec<i64> = visited.into_iter().filter(|id| *id != start).collect();
        result.sort();
        result
    }

    /// Immediate (one-hop) neighbors of `id` in either direction — its direct
    /// dependencies and dependents, deduplicated. Used by `context_pack` to
    /// expand a seed set without a full BFS.
    pub fn neighbors(&self, id: i64) -> Vec<i64> {
        let mut out: HashSet<i64> = HashSet::new();
        if let Some(deps) = self.out.get(&id) {
            out.extend(deps.iter().copied());
        }
        if let Some(deps) = self.inc.get(&id) {
            out.extend(deps.iter().copied());
        }
        out.remove(&id);
        out.into_iter().collect()
    }

    pub fn paths_of(&self, ids: &[i64]) -> Vec<String> {
        let mut out: Vec<String> = ids
            .iter()
            .filter_map(|id| self.paths.get(id).cloned())
            .collect();
        out.sort();
        out
    }

    /// Shortest dependency path from `from` to `to` (forward edges), or
    /// `None` when `to` is unreachable from `from`.
    pub fn shortest_path(&self, from: i64, to: i64) -> Option<Vec<i64>> {
        use std::collections::VecDeque;
        let mut parent: HashMap<i64, i64> = HashMap::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(from);
        visited.insert(from);
        while let Some(node) = queue.pop_front() {
            if node == to {
                break;
            }
            if let Some(neighbors) = self.out.get(&node) {
                for next in neighbors {
                    if visited.insert(*next) {
                        parent.insert(*next, node);
                        queue.push_back(*next);
                    }
                }
            }
        }
        if !visited.contains(&to) {
            return None;
        }
        let mut path = vec![to];
        let mut cur = to;
        while cur != from {
            // `cur` is always a node the BFS above reached via `parent`
            // (the loop only advances to values inserted alongside a
            // `visited` entry), so this always hits — `.get()` instead of
            // indexing avoids a panic path if that invariant is ever broken.
            let prev = *parent.get(&cur)?;
            path.push(prev);
            cur = prev;
        }
        path.reverse();
        Some(path)
    }
}

/// Maps a `graph_cache` read failure ([`crate::persist::
/// read_graph_cache_if_fresh`]'s outer `Result`, e.g. a SQL error reading the
/// `graph_cache`/`slice_meta` rows) to an `ApiError`. Note this is distinct
/// from a decode/stale-rev/missing-row miss, which that function already
/// folds into `Ok(None)` rather than an error.
fn other_err(e: anyhow::Error) -> ApiError {
    ApiError::new("db_error", e.to_string())
}

fn opt_depth(input: &serde_json::Value) -> Option<usize> {
    input
        .get("maxDepth")
        .and_then(|v| v.as_u64())
        .map(|d| d as usize)
}

/// dependencies — files reachable from a file through resolved edges.
///
/// Inputs: `filePath` (required), `direction` (`outgoing` default |
/// `incoming`), `maxDepth` (optional). Output: array of reachable file
/// paths. Terminates on cycles (visited-set BFS); statement count is O(1) in
/// the number of nodes. `not_found` for an unknown file.
pub fn dependencies(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("dependencies", input)?;
    let conn = open_db(input)?;
    let file_path = req_str(input, "filePath")?;
    let direction = opt_str(input, "direction").unwrap_or("outgoing");
    let graph = Graph::load(&conn)?;
    let fid = file_id(&conn, file_path)?;
    let ids = match direction {
        "incoming" => graph.reverse(fid, opt_depth(input)),
        _ => graph.forward(fid, opt_depth(input)),
    };
    Ok(serde_json::json!(graph.paths_of(&ids)))
}

/// dependents — files that depend on a file (reverse traversal).
///
/// Inputs: `filePath` (required), `maxDepth` (optional). Output: array of
/// dependent file paths.
pub fn dependents(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("dependents", input)?;
    let conn = open_db(input)?;
    let file_path = req_str(input, "filePath")?;
    let graph = Graph::load(&conn)?;
    let fid = file_id(&conn, file_path)?;
    let ids = graph.reverse(fid, opt_depth(input));
    Ok(serde_json::json!(graph.paths_of(&ids)))
}

/// blast_radius — full impact set of a file.
///
/// Inputs: `filePath` (required). Output: array of file paths — the union of
/// everything the file reaches and everything that reaches it (both
/// directions), excluding the file itself.
pub fn blast_radius(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("blast_radius", input)?;
    let conn = open_db(input)?;
    let file_path = req_str(input, "filePath")?;
    let graph = Graph::load(&conn)?;
    let fid = file_id(&conn, file_path)?;
    let mut both: Vec<i64> = graph.forward(fid, None);
    both.extend(graph.reverse(fid, None));
    both.sort();
    both.dedup();
    Ok(serde_json::json!(graph.paths_of(&both)))
}

/// symbol_blast_radius — impact set of a symbol's declaring file.
///
/// Inputs: `name` (required). Output: `{declaring_file, blast_radius:[...]}`.
/// `not_found` when no entity carries the name.
pub fn symbol_blast_radius(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("symbol_blast_radius", input)?;
    let conn = open_db(input)?;
    let name = req_str(input, "name")?;
    let graph = Graph::load(&conn)?;

    let file_id_of_entity = conn
        .query_row(
            "SELECT file_id FROM entities WHERE name = ?1 ORDER BY id LIMIT 1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|_| ApiError::not_found(format!("symbol {name:?}")))?;

    let declaring = graph
        .paths
        .get(&file_id_of_entity)
        .cloned()
        .unwrap_or_default();
    let mut both: Vec<i64> = graph.forward(file_id_of_entity, None);
    both.extend(graph.reverse(file_id_of_entity, None));
    both.sort();
    both.dedup();
    Ok(serde_json::json!({
        "declaring_file": declaring,
        "blast_radius": graph.paths_of(&both),
    }))
}

/// type_hierarchy — declaration context chain of a type.
///
/// Inputs: `name` (required), `filePath` (optional). Output:
/// `{name, kind, file, span, enclosing_function}` — the persisted containment
/// hierarchy available in the schema (entity → enclosing function → file).
/// `not_found` when no matching entity exists.
/// A single `entities JOIN files` row shared by [`type_hierarchy`]'s initial
/// lookup and its parent-walk loop.
struct EntityRow {
    kind: String,
    name: String,
    file_id: i64,
    start_byte: i64,
    end_byte: i64,
    start_line: i64,
    start_col: i64,
    end_line: i64,
    end_col: i64,
    enclosing_function: Option<String>,
    path: String,
}

impl EntityRow {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "kind": self.kind,
            "file": self.path,
            "file_id": self.file_id,
            "span": {
                "start_byte": self.start_byte, "end_byte": self.end_byte,
                "start_line": self.start_line, "start_col": self.start_col,
                "end_line": self.end_line, "end_col": self.end_col,
            },
            "enclosing_function": self.enclosing_function,
        })
    }
}

const ENTITY_ROW_COLUMNS: &str = "e.kind, e.name, e.file_id, e.start_byte, e.end_byte, e.start_line, e.start_col, e.end_line, e.end_col, e.enclosing_function, f.path";

fn map_entity_row(r: &rusqlite::Row) -> rusqlite::Result<EntityRow> {
    let kind: i64 = r.get(0)?;
    Ok(EntityRow {
        kind: crate::model::EntityKind::from_i64(kind)
            .map(crate::model::EntityKind::as_str)
            .unwrap_or("unknown")
            .to_string(),
        name: r.get(1)?,
        file_id: r.get(2)?,
        start_byte: r.get(3)?,
        end_byte: r.get(4)?,
        start_line: r.get(5)?,
        start_col: r.get(6)?,
        end_line: r.get(7)?,
        end_col: r.get(8)?,
        enclosing_function: r.get(9)?,
        path: r.get(10)?,
    })
}

/// Look up a single `entities JOIN files` row by name, optionally scoped to a
/// declaring file path. Returns the first match ordered by entity id.
fn entity_row(
    conn: &rusqlite::Connection,
    name: &str,
    file_filter: Option<&str>,
) -> rusqlite::Result<Option<EntityRow>> {
    match file_filter {
        Some(path) => {
            let sql = format!(
                "SELECT {ENTITY_ROW_COLUMNS} FROM entities e JOIN files f ON f.id = e.file_id \
                 WHERE e.name = ?1 AND f.path = ?2 ORDER BY e.id LIMIT 1"
            );
            conn.query_row(&sql, rusqlite::params![name, path], map_entity_row)
                .optional()
        }
        None => {
            let sql = format!(
                "SELECT {ENTITY_ROW_COLUMNS} FROM entities e JOIN files f ON f.id = e.file_id \
                 WHERE e.name = ?1 ORDER BY e.id LIMIT 1"
            );
            conn.query_row(&sql, [name], map_entity_row).optional()
        }
    }
}

pub fn type_hierarchy(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("type_hierarchy", input)?;
    let conn = open_db(input)?;
    let name = req_str(input, "name")?;
    let file_path = opt_str(input, "filePath");

    let sql = format!(
        "SELECT {ENTITY_ROW_COLUMNS} FROM entities e JOIN files f ON f.id = e.file_id \
         WHERE e.name = ?1 ORDER BY e.id"
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt.query_map([name], map_entity_row).map_err(db_err)?;
    let mut matches = Vec::new();
    for row in rows {
        let row = row.map_err(db_err)?;
        if let Some(fp) = file_path
            && !matches_path(&row.path, fp)
        {
            continue;
        }
        matches.push(row.to_json());
    }
    if matches.is_empty() {
        return Err(ApiError::not_found(format!("type {name:?}")));
    }
    // Hierarchy: the type itself plus the enclosing chain, parent-first.
    // `seen` guards against a cycle: `entity_row`'s `name + file ORDER BY id
    // LIMIT 1` lookup means two same-file entities that mutually enclose
    // each other by name would otherwise loop forever (the only other exit
    // is an empty/absent `enclosing_function`).
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current = matches[0].clone();
    loop {
        let current_name = current["name"].as_str().unwrap_or("").to_string();
        let current_file = current["file"].as_str().unwrap_or("").to_string();
        if !seen.insert((current_file.clone(), current_name)) {
            break;
        }
        chain.push(current.clone());
        let enclosing = current["enclosing_function"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if enclosing.is_empty() {
            break;
        }
        let parent = entity_row(&conn, &enclosing, Some(&current_file))
            .map_err(db_err)
            .ok()
            .flatten();
        let Some(parent) = parent else { break };
        current = parent.to_json();
    }
    chain.reverse();
    Ok(serde_json::json!({
        "symbol": matches[0],
        "hierarchy": chain,
    }))
}

/// Resolve a free-text `explore` seed to one or more `files.id`s.
///
/// Tries an exact/suffix file-path match first ([`file_id`]'s existing
/// semantics). On a miss, falls back to matching the input against symbol
/// names (exact match preferred; otherwise any symbol whose name contains
/// the input, case-insensitively) and returns the distinct set of files
/// those symbols are declared in — this is the loose, cyano-`ownership`-style
/// resolution the query-modes table's `explore` row promises but the
/// original implementation didn't provide (a bare seed had to already be an
/// exact/suffix file path). Returns the matched file ids plus a tag
/// describing how they were resolved, or `not_found` if nothing matches
/// either way.
fn resolve_seeds(
    conn: &Connection,
    input: &str,
) -> std::result::Result<(Vec<i64>, &'static str), ApiError> {
    if let Ok(id) = file_id(conn, input) {
        return Ok((vec![id], "file"));
    }

    let mut stmt = conn
        .prepare("SELECT DISTINCT file_id FROM symbols WHERE name = ?1")
        .map_err(db_err)?;
    let exact: Vec<i64> = stmt
        .query_map([input], |r| r.get(0))
        .map_err(db_err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db_err)?;
    if !exact.is_empty() {
        return Ok((exact, "symbol_exact"));
    }

    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT file_id FROM symbols WHERE lower(name) LIKE '%' || lower(?1) || '%'",
        )
        .map_err(db_err)?;
    let partial: Vec<i64> = stmt
        .query_map([input], |r| r.get(0))
        .map_err(db_err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db_err)?;
    if !partial.is_empty() {
        return Ok((partial, "symbol_partial"));
    }

    Err(ApiError::not_found(format!(
        "no file or symbol matching {input:?}"
    )))
}

/// explore — neighborhood exploration from a seed file or symbol.
///
/// Inputs: `query` object with `params: {input, direction?, maxItems?}`.
/// `input` is resolved as a file path first, falling back to a symbol-name
/// match (exact, then substring) when no file matches — see
/// [`resolve_seeds`]. `direction` is `"outgoing"` (default — files the seed
/// depends on), `"incoming"` (files depending on the seed), or `"both"`.
/// Output: `{input, resolvedVia, seedFiles, reachable:[...]}` — files
/// reachable from the seed(s) through resolved edges, capped at `maxItems`.
pub fn explore(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("explore", input)?;
    let conn = open_db(input)?;
    let query = input
        .get("query")
        .ok_or_else(|| ApiError::new("invalid_input", "missing query field"))?;
    let params = query.get("params").unwrap_or(query);
    let seed = params
        .get("input")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::new("invalid_input", "query.params.input is required"))?;
    let max_items = params
        .get("maxItems")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let direction = params
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("outgoing");

    let graph = Graph::load(&conn)?;
    let (seed_ids, resolved_via) = resolve_seeds(&conn, seed)?;

    let mut visited: HashSet<i64> = HashSet::new();
    for &fid in &seed_ids {
        let ids: Vec<i64> = match direction {
            "incoming" => graph.reverse(fid, None),
            "both" => {
                let mut both = graph.forward(fid, None);
                both.extend(graph.reverse(fid, None));
                both
            }
            _ => graph.forward(fid, None),
        };
        visited.extend(ids);
    }
    for fid in &seed_ids {
        visited.remove(fid);
    }
    let mut reachable: Vec<i64> = visited.into_iter().collect();
    reachable.sort();

    let mut reachable = graph.paths_of(&reachable);
    if let Some(cap) = max_items {
        reachable.truncate(cap);
    }
    Ok(serde_json::json!({
        "input": seed,
        "resolvedVia": resolved_via,
        "seedFiles": graph.paths_of(&seed_ids),
        "reachable": reachable,
    }))
}

#[cfg(test)]
mod serialization_tests {
    use super::*;

    /// `Graph` is cached as a `postcard`-encoded blob (`graph_cache` table);
    /// a round trip through `to_allocvec`/`from_bytes` must reproduce the
    /// original struct exactly, or the cache would silently corrupt future
    /// reads.
    #[test]
    fn postcard_round_trip_preserves_graph() {
        let mut paths = HashMap::new();
        paths.insert(1i64, "a.ts".to_string());
        paths.insert(2i64, "b.ts".to_string());
        paths.insert(3i64, "c.ts".to_string());

        let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
        out.insert(1, vec![2, 3]);
        out.insert(2, vec![3]);

        let mut inc: HashMap<i64, Vec<i64>> = HashMap::new();
        inc.insert(2, vec![1]);
        inc.insert(3, vec![1, 2]);

        let graph = Graph { paths, out, inc };

        let encoded = postcard::to_allocvec(&graph).expect("graph encodes");
        let decoded: Graph = postcard::from_bytes(&encoded).expect("graph decodes");

        assert_eq!(decoded, graph, "round-tripped graph matches original");
    }
}

#[cfg(test)]
mod build_on_read_tests {
    use super::*;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "varde-graph-bor-{label}-{}-{}",
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

    /// The § Phase-2 build-on-read contract for graph modes: a `dependencies`
    /// call with a `repoRoot` builds on first touch, reflects edits (an import
    /// retarget moves the dependency set) with no explicit rebuild, and its
    /// answer is byte-identical to a fresh full `build` + re-query — the
    /// parity guarantee the plan requires before the targeted neighborhood
    /// path can replace the correct-but-full staging.
    #[test]
    fn dependencies_self_freshens_and_matches_full_build() {
        with_isolated_home("graph-bor", "deps-freshen", || {
            let root = temp_root("deps-freshen");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            let y = root.join("y.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");
            std::fs::write(&y, "export const foo = () => 2;\n").expect("write y.ts");

            let a_str = a.to_str().unwrap();
            let input = serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "filePath": a_str,
            });

            // First call: build-on-miss, then answer. a.ts reaches x.ts.
            let first = dependencies(&input).expect("first query builds and answers");
            let first_paths: Vec<&str> = first
                .as_array()
                .expect("dependencies returns an array")
                .iter()
                .filter_map(|p| p.as_str())
                .collect();
            assert!(
                first_paths.iter().any(|p| p.ends_with("x.ts")),
                "x.ts reachable initially: {first_paths:?}"
            );

            // Retarget a.ts's import to y.ts, with NO manual rebuild.
            std::fs::write(&a, "import { foo } from \"./y\";\nfoo();\n").expect("rewrite a.ts");

            let second = dependencies(&input).expect("freshens edges and answers");
            let second_paths: Vec<&str> = second
                .as_array()
                .expect("dependencies returns an array")
                .iter()
                .filter_map(|p| p.as_str())
                .collect();
            assert!(
                second_paths.iter().any(|p| p.ends_with("y.ts")),
                "y.ts reachable after edit: {second_paths:?}"
            );
            assert!(
                !second_paths.iter().any(|p| p.ends_with("x.ts")),
                "x.ts no longer reachable: {second_paths:?}"
            );

            // Parity: force a full build and re-query — answers must match.
            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let full = dependencies(&input).expect("full-build query answers");
            assert_eq!(second, full, "dependencies parity after full build");

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// AC1: once the `graph_cache` row is fresh (its `rev` matches
    /// [`crate::persist::max_rev`]), a subsequent `Graph::load` call — driven
    /// here through the `dependencies` mode, one of the 8 graph-traversal
    /// modes — deserializes the cache directly instead of re-running the
    /// full SQL-scan rebuild. Proven via the [`crate::GRAPH_CACHE_HIT_CALLS`]
    /// counter's before/after delta (racy against unrelated parallel tests
    /// that also drive `Graph::load`, per its own doc comment — run this
    /// under a module-scoped filter, e.g. `cargo test -p varde-code
    /// query::graph::`).
    #[test]
    fn cache_hit_path_used_when_fresh() {
        with_isolated_home("graph-bor", "cache-hit", || {
            let root = temp_root("cache-hit");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");

            let input = serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "filePath": a.to_str().unwrap(),
            });

            // First call: build-on-miss triggers a fresh full build
            // (`build::run_with_force`), which now eagerly writes
            // `graph_cache` as part of that same build (see
            // `eager-cache-write-on-full-build`) — so by the time
            // `Graph::load` runs inside this same call, the cache is
            // already fresh and this call hits it too.
            let before_first =
                crate::GRAPH_CACHE_HIT_CALLS.load(std::sync::atomic::Ordering::Relaxed);
            let first = dependencies(&input).expect("first query builds and answers");
            let after_first =
                crate::GRAPH_CACHE_HIT_CALLS.load(std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                after_first,
                before_first + 1,
                "first call hits the cache the eager full build already wrote"
            );

            // Second call with no intervening edit: the cache is still fresh
            // (rev == current max_rev), so this call must hit the cache path
            // too.
            let before_second = after_first;
            let second = dependencies(&input).expect("second query hits the cache");
            let after_second =
                crate::GRAPH_CACHE_HIT_CALLS.load(std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                after_second,
                before_second + 1,
                "second call hits the fresh cache exactly once"
            );
            assert_eq!(
                first, second,
                "cache hit answers identically to the SQL rebuild"
            );

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// AC2: a missing, stale-rev, or decode-failing `graph_cache` row
    /// must never surface as an error — `Graph::load` falls back to the full
    /// SQL-scan rebuild and rewrites a fresh cache afterward (self-healing),
    /// so the *next* call hits the fast path again.
    #[test]
    fn corrupt_cache_falls_back_safely() {
        with_isolated_home("graph-bor", "cache-corrupt", || {
            let root = temp_root("cache-corrupt");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");

            let input = serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "filePath": a.to_str().unwrap(),
            });

            // Seed a graph_cache row, then corrupt its blob while keeping
            // `rev` matching the current ledger — a decode failure, not a
            // stale-rev miss.
            let _ = dependencies(&input).expect("seed the cache");
            let db = crate::db::path::repo_db_path(&root);
            let conn = rusqlite::Connection::open(&db).expect("open db");
            conn.execute(
                "UPDATE graph_cache SET blob = ?1 WHERE id = 1",
                rusqlite::params![vec![0xFFu8; 8]],
            )
            .expect("corrupt the cached blob");

            // Must not error, and must still answer correctly (falls back to
            // the full SQL-scan rebuild).
            let recovered = dependencies(&input).expect("corrupt cache falls back, no hard error");
            let recovered_paths: Vec<&str> = recovered
                .as_array()
                .expect("dependencies returns an array")
                .iter()
                .filter_map(|p| p.as_str())
                .collect();
            assert!(
                recovered_paths.iter().any(|p| p.ends_with("x.ts")),
                "x.ts still reachable after corrupt-cache fallback: {recovered_paths:?}"
            );

            // Self-healing: the fallback rewrote a fresh, decodable cache.
            let restored = crate::persist::read_graph_cache_if_fresh(&conn)
                .expect("cache read after fallback")
                .expect("fallback rewrote a fresh, decodable graph_cache row");
            let _ = restored;

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }
}
