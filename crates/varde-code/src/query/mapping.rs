//! Mapping/diff query modes: map_file, map_symbol, map_path, detect_changes,
//! hotspots.
//!
//! - `hotspots` ranks files by descending complexity + churn (the repo's
//!   established risk-hotspot convention).
//! - `detect_changes` mirrors the existing `code_query` `detect_changes`
//!   contract: a `diffMode: staged|working_tree|range` input; changed files
//!   come from git, and symbols are classified added/removed/modified by
//!   diffing the persisted store against the current source.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use super::{ApiError, db_err, freshen_for_mode, open_db, opt_str, req_str};
use crate::query::graph::Graph;
use crate::query::noise_filter::{is_generated_or_vendored_path, is_non_source_path};
use crate::query::simple::{file_id, matches_path};

/// Distinct dependent-file and dependency-file counts for a single file id.
///
/// `files.fan_in`/`files.fan_out` count resolved *edges* — every import plus
/// every cross-file call site — so one file that references a target many
/// times inflates both. That over-counts "how many files depend on / are
/// depended on by this one", the same mismatch [`super::foundational_files`]
/// corrects for its leaderboard. These are the honest distinct-file counts:
/// how many *other* files have at least one resolved edge into (`fan_in`) or
/// out of (`fan_out`) the target, deduped by the far-end file id and excluding
/// self-edges. Reads the same `resolved_edges` set the fan columns are built
/// from.
fn distinct_fan_files(conn: &Connection, fid: i64) -> Result<(i64, i64), ApiError> {
    let dependents: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT from_file_id) FROM resolved_edges
             WHERE resolved = 1 AND to_file_id = ?1 AND from_file_id != to_file_id",
            [fid],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    let dependencies: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT to_file_id) FROM resolved_edges
             WHERE resolved = 1 AND from_file_id = ?1 AND to_file_id IS NOT NULL
               AND from_file_id != to_file_id",
            [fid],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    Ok((dependents, dependencies))
}

/// map_file — persisted node info for a file.
///
/// Inputs: `filePath` (required). Output: `{path, complexity, churn,
/// fan_in, fan_out, fan_in_files, fan_out_files, community_id,
/// community_label}`. `fan_in`/`fan_out` are resolved-edge (reference) totals;
/// `fan_in_files`/`fan_out_files` are the distinct *file* counts (how many
/// other files depend on / are depended on by this one) — the intuitive "fan"
/// answer, matching the distinct-dependent counting in
/// [`super::foundational_files`]. `not_found` for an unknown file.
pub fn map_file(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("map_file", input)?;
    let conn = open_db(input)?;
    let file_path = req_str(input, "filePath")?;
    let fid = file_id(&conn, file_path)?;
    let row: (String, Option<i64>, Option<i64>, i64, i64, Option<i64>) = conn
        .query_row(
            "SELECT f.path, f.complexity, f.churn, f.fan_in, f.fan_out, f.community_id
             FROM files f WHERE f.id = ?1",
            [fid],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .map_err(db_err)?;
    let (fan_in_files, fan_out_files) = distinct_fan_files(&conn, fid)?;
    let community_label: Option<String> = if let Some(cid) = row.5 {
        conn.query_row("SELECT label FROM communities WHERE id = ?1", [cid], |r| {
            r.get(0)
        })
        .map_err(db_err)
        .ok()
    } else {
        None
    };
    Ok(serde_json::json!({
        "path": row.0,
        "complexity": row.1,
        "churn": row.2,
        "fan_in": row.3,
        "fan_out": row.4,
        "fan_in_files": fan_in_files,
        "fan_out_files": fan_out_files,
        "community_id": row.5,
        "community_label": community_label,
    }))
}

/// map_symbol — persisted entity info for a symbol.
///
/// Inputs: `name` (required), `sourceFile` (optional). Output: the entity's
/// persisted fields. `not_found` when no entity matches.
pub fn map_symbol(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("map_symbol", input)?;
    let conn = open_db(input)?;
    let name = req_str(input, "name")?;
    let source_file = opt_str(input, "sourceFile");

    let sql = "SELECT e.kind, e.name, e.file_id, e.start_byte, e.end_byte, e.start_line, e.start_col, e.end_line, e.end_col,
               e.enclosing_function, e.method, e.path, e.status, e.body_shape, f.path
               FROM entities e JOIN files f ON f.id = e.file_id
               WHERE e.name = ?1 ORDER BY e.id";
    let mut stmt = conn.prepare(sql).map_err(db_err)?;
    let rows = stmt
        .query_map([name], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, String>(14)?,
            ))
        })
        .map_err(db_err)?;
    let mut matches = Vec::new();
    for row in rows {
        let (kind, n, fid, sb, eb, sl, sc, el, ec, enclosing, method, path, status, body, file) =
            row.map_err(db_err)?;
        if let Some(sf) = source_file
            && !matches_path(&file, sf)
        {
            continue;
        }
        let kind = crate::model::EntityKind::from_i64(kind)
            .map(crate::model::EntityKind::as_str)
            .unwrap_or("unknown");
        matches.push(serde_json::json!({
            "name": n,
            "kind": kind,
            "file": file,
            "file_id": fid,
            "span": {
                "start_byte": sb, "end_byte": eb,
                "start_line": sl, "start_col": sc,
                "end_line": el, "end_col": ec,
            },
            "enclosing_function": enclosing,
            "method": method,
            "path": path,
            "status": status,
            "body_shape": body,
        }));
    }
    matches
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::not_found(format!("symbol {name:?}")))
}

/// map_path — shortest dependency path between two files.
///
/// Inputs: `sourceFile`, `targetFile` (required), `maxDepth` (optional cap).
/// Output: array of file paths from source to target (inclusive), or an
/// empty array when the target is unreachable. `not_found` for an unknown
/// file.
pub fn map_path(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("map_path", input)?;
    let conn = open_db(input)?;
    let source = req_str(input, "sourceFile")?;
    let target = req_str(input, "targetFile")?;
    let graph = Graph::load(&conn)?;
    let from = file_id(&conn, source)?;
    let to = file_id(&conn, target)?;

    let max_depth = input.get("maxDepth").and_then(|v| v.as_u64());
    let mut path = graph.shortest_path(from, to);
    if let Some(p) = &mut path
        && let Some(limit) = max_depth
    {
        p.truncate(limit as usize + 1); // include the source node
    }
    match path {
        Some(p) => Ok(serde_json::json!(graph.paths_of(&p))),
        None => Ok(serde_json::json!([])),
    }
}

/// git diff --name-status output: (status, path) pairs.
fn git_changed_files(
    diff_mode: &str,
    range: Option<&str>,
    cwd: &std::path::Path,
) -> Vec<(String, String)> {
    let args: Vec<&str> = match diff_mode {
        "staged" => vec!["diff", "--cached", "--name-status"],
        "range" => {
            let mut a = vec!["diff", "--name-status"];
            if let Some(r) = range {
                a.push(r);
            }
            a
        }
        _ => {
            // working_tree (default)
            vec!["diff", "--name-status"]
        }
    };
    let Some(text) = crate::git::run_git(&args, cwd) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let status = parts.next()?.trim().to_string();
            let path = parts.next()?.trim().to_string();
            if status.is_empty() || path.is_empty() {
                return None;
            }
            Some((status, path))
        })
        .collect()
}

/// Persisted symbol signatures: name + kind + span, keyed per file.
fn persisted_symbols(
    conn: &Connection,
    file_id: i64,
) -> std::result::Result<Vec<(String, String, i64, i64)>, ApiError> {
    let mut stmt = conn
        .prepare("SELECT name, kind, start_byte, end_byte FROM symbols WHERE file_id = ?1")
        .map_err(db_err)?;
    let rows = stmt
        .query_map([file_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (name, kind, sb, eb) = row.map_err(db_err)?;
        let kind = crate::model::SymbolKind::from_i64(kind)
            .map(crate::model::SymbolKind::as_str)
            .unwrap_or("unknown")
            .to_string();
        out.push((name, kind, sb, eb));
    }
    Ok(out)
}

/// detect_changes — classify symbol changes for files changed in git.
///
/// Inputs: `diffMode` (`staged` | `working_tree` | `range`, default
/// `working_tree`), `range` (required when `diffMode: range`). Output: array
/// per changed file: `{file, status, symbols:[{name, kind, change}]}` where
/// `change` is `added`, `removed`, or `modified` (diffing the persisted
/// store against the current source).
pub fn detect_changes(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let diff_mode = opt_str(input, "diffMode").unwrap_or("working_tree");
    let range = opt_str(input, "range");

    // Repo root: prefer repoRoot, else the db's parent chain's repo.
    let repo_root = match input.get("repoRoot").and_then(|v| v.as_str()) {
        Some(r) => std::path::PathBuf::from(r),
        None => {
            let db_path = input
                .get("dbPath")
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            repo_root_of(&db_path).unwrap_or_default()
        }
    };

    // Build-on-miss only (not raw-freshen): `detect_changes` diffs the
    // persisted store against the current source, so freshening the raw slice
    // to disk first would zero out the very diff it reports. It only needs the
    // index to exist so it doesn't error on an unpersisted repo — and only when
    // the caller gave a `repoRoot` (not an explicit `dbPath`, which points at a
    // specific index we must read as-is, never rebuild).
    if opt_str(input, "dbPath").is_none()
        && let Some(root) = opt_str(input, "repoRoot")
    {
        crate::slice::ensure_index_built(root)
            .map_err(|e| ApiError::new("build_error", format!("{e}")))?;
    }

    let conn = open_db(input)?;
    let changed = git_changed_files(diff_mode, range, &repo_root);
    if repo_root.as_os_str().is_empty() {
        return Err(ApiError::new(
            "invalid_input",
            "detect_changes needs repoRoot (or a dbPath inside a git repo)",
        ));
    }

    let mut out = Vec::new();
    for (status, rel) in changed {
        let abs = repo_root.join(&rel);
        // Symbols currently on disk (re-extract the changed file).
        let current: Vec<(String, String, i64, i64)> = extract_symbol_signatures(&abs);
        // Symbols persisted for the file (best-effort: the file may not be
        // in the store yet).
        let persisted: Vec<(String, String, i64, i64)> =
            match file_id(&conn, &abs.to_string_lossy()) {
                Ok(fid) => persisted_symbols(&conn, fid).unwrap_or_default(),
                Err(_) => Vec::new(),
            };

        let changes = classify_symbol_changes(&current, &persisted);
        out.push(serde_json::json!({
            "file": rel,
            "status": status,
            "symbols": changes,
        }));
    }
    Ok(serde_json::json!(out))
}

fn classify_symbol_changes(
    current: &[(String, String, i64, i64)],
    persisted: &[(String, String, i64, i64)],
) -> Vec<serde_json::Value> {
    let mut persisted_by_name = HashMap::with_capacity(persisted.len());
    for (name, _kind, start_byte, end_byte) in persisted {
        // Keep the first persisted span for duplicate names.
        persisted_by_name
            .entry(name.as_str())
            .or_insert((*start_byte, *end_byte));
    }
    let current_names: HashSet<&str> = current
        .iter()
        .map(|(name, _kind, _start_byte, _end_byte)| name.as_str())
        .collect();

    let mut changes = Vec::new();
    // added: in current, not persisted
    for (name, kind, _sb, _eb) in current {
        if !persisted_by_name.contains_key(name.as_str()) {
            changes.push(serde_json::json!({"name": name, "kind": kind, "change": "added"}));
        }
    }
    // removed: persisted, not in current
    for (name, kind, _sb, _eb) in persisted {
        if !current_names.contains(name.as_str()) {
            changes.push(serde_json::json!({"name": name, "kind": kind, "change": "removed"}));
        }
    }
    // modified: in both but span differs
    for (name, kind, sb, eb) in current {
        if let Some((psb, peb)) = persisted_by_name.get(name.as_str())
            && (psb != sb || peb != eb)
        {
            changes.push(serde_json::json!({"name": name, "kind": kind, "change": "modified"}));
        }
    }
    changes.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    changes
}

/// Re-extract symbol signatures (name, kind, start_byte, end_byte) from a
/// source file on disk. Empty on any parse/extraction failure.
fn extract_symbol_signatures(path: &std::path::Path) -> Vec<(String, String, i64, i64)> {
    let Some(lang) = crate::parse::language_for_path(path) else {
        return Vec::new();
    };
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let parsed = crate::parse::parse_source(&lang, &source);
    // Symbol-only walk (skips the entity walk) — file_id is irrelevant here,
    // only name/kind/span are read below.
    let mut symbols = Vec::new();
    let has_error =
        crate::extract::symbol::extract_symbols(&parsed.root.root(), lang, 0, &mut symbols);
    if has_error {
        return Vec::new();
    }
    symbols
        .into_iter()
        .map(|s| {
            (
                s.name,
                match s.kind {
                    crate::model::SymbolKind::Binding => "binding".to_string(),
                    crate::model::SymbolKind::Reference => "reference".to_string(),
                },
                i64::from(s.span.start_byte),
                i64::from(s.span.end_byte),
            )
        })
        .collect()
}

/// Walk up from a db path looking for the enclosing git repository root.
fn repo_root_of(db_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = db_path.parent()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// hotspots — files ranked by descending `complexity * churn`.
///
/// Inputs: none (repoRoot/dbPath only). Output: array of
/// `{file, complexity, churn, score}` sorted by `score` (`complexity * churn`,
/// or `complexity` alone when churn does not vary across files) descending,
/// ties broken by path. The standalone mode is unbounded — the full ranked
/// list; nav_map's section caps it (see [`hotspots_on`]'s `limit`).
pub fn hotspots(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("hotspots", input)?;
    let conn = open_db(input)?;
    hotspots_on(&conn, None)
}

/// Core hotspots computation over an already-open connection — the seam
/// [`super::nav_map::nav_map`] calls directly so its `hotspots` section
/// reads the same connection/snapshot as every other section, instead of
/// [`hotspots`] opening (and re-freshening) a second one.
///
/// `limit` caps the result to the top-N by score (`None` = unbounded). The
/// standalone `hotspots` mode passes `None`; nav_map passes a cap so its
/// orientation summary stays bounded on large repos.
pub fn hotspots_on(conn: &Connection, limit: Option<usize>) -> Result<serde_json::Value, ApiError> {
    let mut stmt = conn
        .prepare("SELECT f.path, f.complexity, f.churn FROM files f WHERE f.is_test_path = 0")
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })
        .map_err(db_err)?;
    let mut files: Vec<(String, i64, i64)> = Vec::new();
    for row in rows {
        let (path, complexity, churn) = row.map_err(db_err)?;
        if is_generated_or_vendored_path(&path) || is_non_source_path(&path) {
            continue;
        }
        files.push((path, complexity.unwrap_or(0), churn.unwrap_or(0)));
    }
    // Multiplicative score (Tornhill): a hotspot is a file that is *both*
    // complex and frequently changed, so a complex-but-stable file (churn 0)
    // scores 0 and a churny-but-simple file scores low. An additive
    // `complexity + churn` is dominated by whichever term is larger — with
    // complexity in the hundreds and churn a handful, churn barely moved the
    // ranking, collapsing it to a plain complexity sort.
    //
    // Fallback: churn is only a ranking signal when it *varies* across files.
    // Two degenerate cases produce a single distinct churn value and make the
    // multiplicative score misleading: a non-git repo / empty churn window
    // (every churn 0), and a shallow clone or single-commit window (every
    // churn 1 — observed as all 917 files scoring `complexity * 1` on a
    // reference repo). In both, `complexity * k` just rescales complexity (or
    // zeroes it), so rank by complexity alone — the best signal still
    // available — rather than presenting a churn-weighted score that carries no
    // information.
    let churn_varies = {
        let mut seen = std::collections::HashSet::new();
        files.iter().for_each(|(_, _, ch)| {
            seen.insert(*ch);
        });
        seen.len() > 1
    };
    let mut hotspots: Vec<(String, i64, i64, i64)> = files
        .into_iter()
        .map(|(path, c, ch)| {
            let score = if churn_varies { c * ch } else { c };
            (path, c, ch, score)
        })
        .collect();
    hotspots.sort_by(|a, b| {
        b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)) // score desc, path asc
    });
    if let Some(limit) = limit {
        hotspots.truncate(limit);
    }
    Ok(serde_json::json!(
        hotspots
            .into_iter()
            .map(|(file, complexity, churn, score)| serde_json::json!({
                "file": file,
                "complexity": complexity,
                "churn": churn,
                "score": score,
            }))
            .collect::<Vec<_>>()
    ))
}

/// clusters — community-detection partition over the resolution graph.
///
/// Surfaces the Louvain communities already computed and persisted by the
/// global slice ([`crate::resolve::community::detect`], `communities` /
/// `community_members` tables) as partitions rather than point-to-point
/// traversals — no new analysis, just a read path over existing state.
///
/// Inputs: `minSize?` (drop clusters below N files — **defaults to 2**, since a
/// lone file is a caller artifact, not a domain; pass `1` to include
/// singletons), `minCohesion?` (drop clusters whose internal-edge fraction is
/// below this 0.0..=1.0 floor — the code-graph signal for "is this a real
/// semantic group or a random caller set"; defaults to `0.0`), `maxClusters?`
/// (cap output, ranked by cohesion desc then size desc), `seedPath?` (return
/// only the cluster containing this file, unfiltered). Output: `{"clusters":
/// [{id, files, label, cohesion}, ...]}`. `label` is always `null` — naming a
/// cluster ("auth", "billing") is a judgment call left to the caller.
/// `cohesion` is the fraction of edges touching the cluster that stay inside it
/// (0.0..=1.0, 0.0 for an edgeless singleton); a low score means the boundary
/// is likely an artifact rather than a real domain. The `minSize`/`minCohesion`
/// defaults exist because on real repos most communities are singletons — e.g.
/// a 2116-file .NET repo yields 858 communities, 583 (68%) of them lone files
/// that are all cohesion 0.0 — so an unfiltered partition buries the real
/// multi-file domains under caller-artifact noise.
pub fn clusters(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("clusters", input)?;
    let conn = open_db(input)?;
    // A lone file is not a semantic group; default the floor to 2 so callers
    // get real (multi-file) domains without opting in. `minSize: 1` restores
    // the full partition.
    let min_size = input
        .get("minSize")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(2);
    let min_cohesion = input
        .get("minCohesion")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let max_clusters = input
        .get("maxClusters")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let seed_path = opt_str(input, "seedPath");

    // file_id -> community_id, for the single edge-scan cohesion pass below.
    let mut community_of: HashMap<i64, i64> = HashMap::new();
    // community_id -> member (file_id, path) pairs.
    let mut members: HashMap<i64, Vec<(i64, String)>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT cm.community_id, f.id, f.path
                 FROM community_members cm JOIN files f ON f.id = cm.file_id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db_err)?;
        for row in rows {
            let (cid, fid, path) = row.map_err(db_err)?;
            community_of.insert(fid, cid);
            members.entry(cid).or_default().push((fid, path));
        }
    }

    // One pass over resolved edges: tally internal/external edge counts per
    // community for cohesion scoring.
    let mut internal: HashMap<i64, u64> = HashMap::new();
    let mut external: HashMap<i64, u64> = HashMap::new();
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
            let (Some(&cf), Some(&ct)) = (community_of.get(&from), community_of.get(&to)) else {
                continue;
            };
            if cf == ct {
                *internal.entry(cf).or_default() += 1;
            } else {
                *external.entry(cf).or_default() += 1;
                *external.entry(ct).or_default() += 1;
            }
        }
    }

    let build_cluster = |cid: i64, m: &[(i64, String)]| -> serde_json::Value {
        let ins = internal.get(&cid).copied().unwrap_or(0);
        let ext = external.get(&cid).copied().unwrap_or(0);
        let cohesion = if ins + ext == 0 {
            0.0
        } else {
            ins as f64 / (ins + ext) as f64
        };
        let mut paths: Vec<String> = m.iter().map(|(_, p)| p.clone()).collect();
        paths.sort();
        serde_json::json!({
            "id": cid,
            "files": paths,
            "label": serde_json::Value::Null,
            "cohesion": cohesion,
        })
    };

    if let Some(seed) = seed_path {
        let fid = file_id(&conn, seed)?;
        let Some(&cid) = community_of.get(&fid) else {
            return Ok(serde_json::json!({ "clusters": [] }));
        };
        return Ok(serde_json::json!({ "clusters": [build_cluster(cid, &members[&cid])] }));
    }

    let mut result: Vec<serde_json::Value> = members
        .iter()
        .filter(|(_, m)| m.len() >= min_size)
        .map(|(cid, m)| build_cluster(*cid, m))
        .filter(|c| c["cohesion"].as_f64().unwrap_or(0.0) >= min_cohesion)
        .collect();

    result.sort_by(|a, b| {
        let ca = a["cohesion"].as_f64().unwrap_or(0.0);
        let cb = b["cohesion"].as_f64().unwrap_or(0.0);
        cb.partial_cmp(&ca)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let sa = a["files"].as_array().map(|v| v.len()).unwrap_or(0);
                let sb = b["files"].as_array().map(|v| v.len()).unwrap_or(0);
                sb.cmp(&sa)
            })
    });
    if let Some(cap) = max_clusters {
        result.truncate(cap);
    }
    Ok(serde_json::json!({ "clusters": result }))
}

/// context_pack — keyword-driven context bundle over the code graph, no doc
/// corpus involved.
///
/// Inputs: `query` (required keyword/phrase). Resolution is purely
/// structural — no semantic search, no embeddings: file/directory paths and
/// symbol names are exact/substring matched (the same tiering `explore`'s
/// `input` field uses) to build a seed set, then each seed's immediate
/// `dependencies`/`dependents` (one hop, via the resolved-edge graph) are
/// pulled in as neighbors. `not_found` when nothing matches.
///
/// Output: `{"files": [{path, relevance}], "symbols": [{name, filePath,
/// kind}], "tests": [{path, coversFile}], "readingOrder": [path, ...]}`.
/// `relevance` is `"seed"` (direct match) or `"neighbor"` (one-hop
/// pull-in); within each tier, files rank by `hotspots`-style
/// complexity+churn score (richest first) so noisy low-relevance neighbors
/// sink to the bottom. `readingOrder` is just that ranked path list — no
/// separate ranking logic. `tests` reuses `tests_for_file`'s heuristic
/// per seed file (neighbors aren't probed for coverage, to keep the query
/// count bounded).
///
/// What's missing vs. a doc-backed pack: no knowledge-bundle field — there's
/// no doc corpus here to search, so "why does this code exist" isn't
/// answerable from this mode; it only tells you what code is related.
pub fn context_pack(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    freshen_for_mode("context_pack", input)?;
    let conn = open_db(input)?;
    let query = req_str(input, "query")?;

    // Split the query into whitespace-delimited keywords so a multi-word query
    // ("http router") does keyword search instead of matching the literal phrase
    // — the phrase never appears verbatim in a path or symbol name, so the whole
    // pack returned `not_found`. Each token seeds independently and the matches
    // are unioned. A single-word query yields one token and behaves exactly as
    // before. An all-whitespace query falls back to the raw string (→ no match →
    // the same `not_found` as before).
    let tokens: Vec<&str> = {
        let split: Vec<&str> = query.split_whitespace().collect();
        if split.is_empty() { vec![query] } else { split }
    };

    // Step 1: file/directory-path substring match (case-insensitive; a path
    // already contains its directory components, so no separate dir query).
    let mut stmt = conn
        .prepare("SELECT id FROM files WHERE lower(path) LIKE '%' || lower(?1) || '%'")
        .map_err(db_err)?;
    let mut seed_ids: HashSet<i64> = HashSet::new();
    for tok in &tokens {
        for id in stmt
            .query_map([tok], |r| r.get::<_, i64>(0))
            .map_err(db_err)?
        {
            seed_ids.insert(id.map_err(db_err)?);
        }
    }

    // Step 1 (symbols): exact name match wins; substring fallback only when the
    // exact pass finds nothing for *any* token — same tiering as `explore`'s
    // `resolve_seeds`, applied per token then unioned.
    let mut stmt = conn
        .prepare("SELECT name, kind, file_id FROM entities WHERE name = ?1 ORDER BY id")
        .map_err(db_err)?;
    let mut symbol_rows: Vec<(String, i64, i64)> = Vec::new();
    for tok in &tokens {
        for row in stmt
            .query_map([tok], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(db_err)?
        {
            symbol_rows.push(row.map_err(db_err)?);
        }
    }
    if symbol_rows.is_empty() {
        let mut stmt = conn
            .prepare(
                "SELECT name, kind, file_id FROM entities
                 WHERE lower(name) LIKE '%' || lower(?1) || '%' ORDER BY id LIMIT 200",
            )
            .map_err(db_err)?;
        for tok in &tokens {
            for row in stmt
                .query_map([tok], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(db_err)?
            {
                symbol_rows.push(row.map_err(db_err)?);
            }
        }
    }

    // Distinct tokens can match the same symbol (e.g. "http"/"handler" both
    // substring-match `httpHandler`); dedup so it appears once, keeping first
    // occurrence (which preserves the by-id order for a single-token query).
    {
        let mut seen: HashSet<(String, i64, i64)> = HashSet::new();
        symbol_rows.retain(|row| seen.insert(row.clone()));
    }

    // Step 2: every direct match becomes a seed.
    for (_, _, fid) in &symbol_rows {
        seed_ids.insert(*fid);
    }
    if seed_ids.is_empty() {
        return Err(ApiError::not_found(format!(
            "no file, directory, or symbol matching {query:?}"
        )));
    }

    // Step 3: expand — each seed's immediate dependencies/dependents.
    let graph = Graph::load(&conn)?;
    let mut neighbor_ids: HashSet<i64> = HashSet::new();
    for &fid in &seed_ids {
        for n in graph.neighbors(fid) {
            if !seed_ids.contains(&n) {
                neighbor_ids.insert(n);
            }
        }
    }

    // Step 4: rank — seed tier before neighbor tier; within a tier, by
    // hotspots-style complexity+churn score desc, then path asc.
    let mut stmt = conn
        .prepare("SELECT id, COALESCE(complexity, 0) + COALESCE(churn, 0) FROM files")
        .map_err(db_err)?;
    let scores: HashMap<i64, i64> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .map_err(db_err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db_err)?;

    let rank = |ids: &HashSet<i64>| -> Vec<(i64, String)> {
        let mut ranked: Vec<(i64, String)> = ids
            .iter()
            .filter_map(|&id| graph.paths_of(&[id]).into_iter().next().map(|p| (id, p)))
            .collect();
        ranked.sort_by(|a, b| {
            let sa = scores.get(&a.0).copied().unwrap_or(0);
            let sb = scores.get(&b.0).copied().unwrap_or(0);
            sb.cmp(&sa).then_with(|| a.1.cmp(&b.1))
        });
        ranked
    };
    let seed_ranked = rank(&seed_ids);
    let neighbor_ranked = rank(&neighbor_ids);

    let mut files = Vec::with_capacity(seed_ranked.len() + neighbor_ranked.len());
    let mut reading_order = Vec::with_capacity(files.capacity());
    for (_, path) in seed_ranked.iter().chain(neighbor_ranked.iter()) {
        reading_order.push(path.clone());
    }
    for (path, relevance) in seed_ranked
        .iter()
        .map(|(_, p)| (p, "seed"))
        .chain(neighbor_ranked.iter().map(|(_, p)| (p, "neighbor")))
    {
        files.push(serde_json::json!({ "path": path, "relevance": relevance }));
    }

    let symbols: Vec<serde_json::Value> = symbol_rows
        .iter()
        .filter_map(|(name, kind, fid)| {
            graph.paths_of(&[*fid]).into_iter().next().map(|file_path| {
                serde_json::json!({
                    "name": name,
                    "filePath": file_path,
                    "kind": crate::model::EntityKind::from_i64(*kind)
                        .map(crate::model::EntityKind::as_str)
                        .unwrap_or("unknown"),
                })
            })
        })
        .collect();

    // `tests` reuses `tests_for_file`'s heuristic per seed file only —
    // neighbors aren't probed, to keep the query count bounded. Loaded once
    // and reused across seeds (not `covering_tests` per seed) so this stays
    // O(repo_size) instead of O(seeds * repo_size).
    let coverage = crate::query::simple::TestCoverage::load(&conn)?;
    let mut tests = Vec::new();
    for (fid, path) in &seed_ranked {
        for t in coverage.covering(*fid) {
            tests.push(serde_json::json!({ "path": t, "coversFile": path }));
        }
    }

    Ok(serde_json::json!({
        "files": files,
        "symbols": symbols,
        "tests": tests,
        "readingOrder": reading_order,
    }))
}

#[cfg(test)]
mod tests {
    use super::classify_symbol_changes;

    fn pairs(changes: &[serde_json::Value]) -> Vec<(String, String)> {
        changes
            .iter()
            .map(|change| {
                (
                    change["name"].as_str().unwrap().to_string(),
                    change["change"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn preserves_duplicate_name_outputs() {
        let current = vec![
            ("duplicate".into(), "binding".into(), 1, 2),
            ("duplicate".into(), "binding".into(), 3, 4),
            ("added".into(), "binding".into(), 5, 6),
        ];
        let persisted = vec![("removed".into(), "binding".into(), 7, 8)];

        let changes = classify_symbol_changes(&current, &persisted);

        assert_eq!(
            pairs(&changes),
            vec![
                ("added".into(), "added".into()),
                ("duplicate".into(), "added".into()),
                ("duplicate".into(), "added".into()),
                ("removed".into(), "removed".into()),
            ]
        );
    }

    #[test]
    fn uses_first_persisted_span_for_duplicate_names() {
        let current = vec![("duplicate".into(), "binding".into(), 1, 2)];
        let persisted = vec![
            ("duplicate".into(), "binding".into(), 9, 10),
            ("duplicate".into(), "binding".into(), 1, 2),
        ];

        let changes = classify_symbol_changes(&current, &persisted);

        assert_eq!(
            pairs(&changes),
            vec![("duplicate".into(), "modified".into())]
        );
    }

    /// Real schema in-memory for exercising [`super::hotspots_on`] directly.
    /// Uses the production DDL (not a hand-rolled partial table) so generated
    /// columns like `is_test_path` behave exactly as they do in a real db.
    fn files_db(rows: &[(&str, i64, i64)]) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::db::schema_ddl()).expect("schema");
        for (path, complexity, churn) in rows {
            conn.execute(
                "INSERT INTO files (path, complexity, churn) VALUES (?1, ?2, ?3)",
                rusqlite::params![path, complexity, churn],
            )
            .expect("insert");
        }
        conn
    }

    fn hotspot_scores(out: &serde_json::Value) -> Vec<(String, i64)> {
        out.as_array()
            .expect("array")
            .iter()
            .map(|h| {
                (
                    h["file"].as_str().unwrap().to_string(),
                    h["score"].as_i64().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn hotspots_score_is_multiplicative() {
        // A complex-but-stable file (churn 0) must NOT outrank a moderately
        // complex, frequently-changed one — the whole point of a hotspot. Under
        // the old additive `complexity + churn`, the churn=0 file (300) would
        // beat the churn=3 file (100+3=103); multiplicatively it scores 0.
        let conn = files_db(&[
            ("src/complex_stable.rs", 300, 0),
            ("src/complex_churny.rs", 100, 3),
            ("src/simple_churny.rs", 10, 5),
        ]);
        let scores = hotspot_scores(&super::hotspots_on(&conn, None).expect("hotspots"));
        assert_eq!(
            scores,
            vec![
                ("src/complex_churny.rs".to_string(), 300), // 100 * 3
                ("src/simple_churny.rs".to_string(), 50),   // 10 * 5
                ("src/complex_stable.rs".to_string(), 0),   // 300 * 0
            ]
        );
    }

    #[test]
    fn hotspots_falls_back_to_complexity_when_no_churn() {
        // Non-git repo / empty churn window: every churn is 0, so a pure
        // multiplicative score would zero out and collapse to path order.
        // Fall back to ranking by complexity so the mode stays useful.
        let conn = files_db(&[("src/a.rs", 30, 0), ("src/b.rs", 200, 0)]);
        let scores = hotspot_scores(&super::hotspots_on(&conn, None).expect("hotspots"));
        assert_eq!(
            scores,
            vec![("src/b.rs".to_string(), 200), ("src/a.rs".to_string(), 30)]
        );
    }

    #[test]
    fn hotspots_falls_back_to_complexity_when_churn_is_uniform() {
        // Shallow clone / single-commit window: every file has the same
        // non-zero churn (here 1), so churn carries no ranking signal. A
        // multiplicative score would just rescale complexity while presenting
        // a churn-weighted number; rank by complexity instead.
        let conn = files_db(&[
            ("src/a.rs", 30, 1),
            ("src/b.rs", 200, 1),
            ("src/c.rs", 90, 1),
        ]);
        let scores = hotspot_scores(&super::hotspots_on(&conn, None).expect("hotspots"));
        assert_eq!(
            scores,
            vec![
                ("src/b.rs".to_string(), 200),
                ("src/c.rs".to_string(), 90),
                ("src/a.rs".to_string(), 30),
            ],
            "uniform churn must fall back to complexity ranking, not complexity*churn: {scores:?}"
        );
    }

    #[test]
    fn hotspots_respects_limit() {
        // Varying churn so the multiplicative score is in effect; limit caps
        // the result to the top-N by score.
        let conn = files_db(&[
            ("src/a.rs", 10, 2),  // 20
            ("src/b.rs", 100, 5), // 500
            ("src/c.rs", 40, 4),  // 160
            ("src/d.rs", 5, 1),   // 5
        ]);
        let scores = hotspot_scores(&super::hotspots_on(&conn, Some(2)).expect("hotspots"));
        assert_eq!(
            scores,
            vec![("src/b.rs".to_string(), 500), ("src/c.rs".to_string(), 160),],
            "limit must truncate to the top-2 by score: {scores:?}"
        );
    }

    /// In-memory DB with the real schema, seeded with a community partition
    /// and resolved edges, for exercising [`super::clusters_on`]-style filtering
    /// through the public `clusters` entrypoint (via `dbPath`).
    fn clusters_db() -> (rusqlite::Connection, std::path::PathBuf) {
        // clusters() opens by dbPath, so persist to a temp file.
        let path = std::env::temp_dir().join(format!(
            "varde-clusters-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let conn = crate::db::open_or_rebuild(&path).expect("schema");
        (conn, path)
    }

    fn add_file(conn: &rusqlite::Connection, path: &str) -> i64 {
        conn.execute("INSERT INTO files (path) VALUES (?1)", [path])
            .expect("insert file");
        conn.last_insert_rowid()
    }

    fn add_member(conn: &rusqlite::Connection, community_id: i64, file_id: i64) {
        conn.execute(
            "INSERT INTO community_members (community_id, file_id) VALUES (?1, ?2)",
            rusqlite::params![community_id, file_id],
        )
        .expect("insert member");
    }

    fn add_edge(conn: &rusqlite::Connection, from: i64, to: i64) {
        conn.execute(
            "INSERT INTO resolved_edges (from_file_id, to_file_id, kind, resolved)
             VALUES (?1, ?2, 0, 1)",
            rusqlite::params![from, to],
        )
        .expect("insert edge");
    }

    fn cluster_files(out: &serde_json::Value) -> Vec<Vec<String>> {
        out["clusters"]
            .as_array()
            .expect("clusters array")
            .iter()
            .map(|c| {
                c["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f.as_str().unwrap().to_string())
                    .collect()
            })
            .collect()
    }

    /// The dominant real-repo noise (68% of communities on a 2116-file .NET
    /// repo) is singleton communities — one file, always cohesion 0.0. By
    /// default `clusters` drops them: a lone file is a caller artifact, not a
    /// semantic group. `minSize: 1` restores them.
    #[test]
    fn clusters_drops_singleton_communities_by_default() {
        let (conn, path) = clusters_db();
        // Community 1: a real 2-file group with an internal edge.
        let a = add_file(&conn, "src/auth/login.rs");
        let b = add_file(&conn, "src/auth/session.rs");
        add_member(&conn, 1, a);
        add_member(&conn, 1, b);
        add_edge(&conn, a, b);
        // Community 2: a lone file (singleton) — cohesion 0.0.
        let c = add_file(&conn, "src/util/orphan.rs");
        add_member(&conn, 2, c);
        drop(conn);

        let input = serde_json::json!({ "dbPath": path.to_str().unwrap() });
        let out = super::clusters(&input).expect("clusters");
        let files = cluster_files(&out);
        assert_eq!(
            files,
            vec![vec![
                "src/auth/login.rs".to_string(),
                "src/auth/session.rs".to_string()
            ]],
            "singleton dropped by default: {out}"
        );

        // Opt back in with minSize: 1.
        let input_all = serde_json::json!({ "dbPath": path.to_str().unwrap(), "minSize": 1 });
        let out_all = super::clusters(&input_all).expect("clusters minSize=1");
        assert_eq!(
            cluster_files(&out_all).len(),
            2,
            "minSize:1 restores the singleton: {out_all}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// `minCohesion` prunes loosely-bound "random caller" groups — a multi-file
    /// community whose edges mostly leave it (low internal fraction) — while
    /// keeping tightly-knit domains.
    #[test]
    fn clusters_min_cohesion_prunes_loose_groups() {
        let (conn, path) = clusters_db();
        // Tight community 1: two files, one internal edge, no edges leaving it —
        // cohesion 1.0. Survives the floor.
        let a = add_file(&conn, "src/core/a.rs");
        let b = add_file(&conn, "src/core/b.rs");
        add_member(&conn, 1, a);
        add_member(&conn, 1, b);
        add_edge(&conn, a, b);
        // Loose community 2: one internal edge (c->d) but three edges out to a
        // separate sink community 3 — internal 1, external 3, cohesion 0.25.
        // (Its external edges target community 3, not community 1, so community
        // 1 stays at cohesion 1.0.)
        let c = add_file(&conn, "src/misc/c.rs");
        let d = add_file(&conn, "src/misc/d.rs");
        add_member(&conn, 2, c);
        add_member(&conn, 2, d);
        let e = add_file(&conn, "src/sink/e.rs");
        let f = add_file(&conn, "src/sink/f.rs");
        add_member(&conn, 3, e);
        add_member(&conn, 3, f);
        add_edge(&conn, e, f); // community 3 internal
        add_edge(&conn, c, d); // community 2 internal
        add_edge(&conn, c, e);
        add_edge(&conn, d, e);
        add_edge(&conn, d, f);
        drop(conn);

        let input = serde_json::json!({ "dbPath": path.to_str().unwrap(), "minCohesion": 0.5 });
        let out = super::clusters(&input).expect("clusters minCohesion");
        let files = cluster_files(&out);
        assert_eq!(
            files,
            vec![vec![
                "src/core/a.rs".to_string(),
                "src/core/b.rs".to_string()
            ]],
            "only the tight group clears the cohesion floor: {out}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hotspots_exclude_test_files() {
        // A complex, churny test file (`src/core.test.ts`) must not surface as
        // a hotspot — hotspots orient toward production code, matching the
        // other nav-map surfacing tools' `is_test_path` exclusion. Two
        // production files with differing churn keep the multiplicative score
        // in effect (churn varies), so this isolates the exclusion behavior.
        let conn = files_db(&[
            ("src/core.ts", 100, 4),
            ("src/other.ts", 50, 2),
            ("src/core.test.ts", 200, 9),
        ]);
        let scores = hotspot_scores(&super::hotspots_on(&conn, None).expect("hotspots"));
        assert_eq!(
            scores,
            vec![
                ("src/core.ts".to_string(), 400),  // 100 * 4
                ("src/other.ts".to_string(), 100), // 50 * 2
            ],
            "test file must be excluded from hotspots: {scores:?}"
        );
    }
}

/// Build-on-read contract for the sole per-file global-slice consumer: a
/// `map_file` call with a `repoRoot` builds on first touch (no stale/missing
/// DB errors), reflects an edit that re-wires the graph with no explicit
/// rebuild, and its answer is identical to a fresh full `build` + re-query.
#[cfg(test)]
mod build_on_read_tests {
    use super::*;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "varde-mapping-bor-{label}-{}-{}",
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

    #[test]
    fn map_file_builds_on_miss_then_reflects_edits_and_matches_full_build() {
        with_isolated_home("mapping-bor", "mapfile-freshen", || {
            let root = temp_root("mapfile-freshen");
            let a = root.join("a.ts");
            let x = root.join("x.ts");
            std::fs::write(&a, "import { foo } from \"./x\";\nfoo();\n").expect("write a.ts");
            std::fs::write(&x, "export const foo = () => 1;\n").expect("write x.ts");

            let x_str = x.to_str().unwrap();
            let input = serde_json::json!({
                "repoRoot": root.to_str().unwrap(),
                "filePath": x_str,
            });

            // First call: build-on-miss — the global slice (community_id) is
            // computed, the fan denorm is present. No stale/missing DB error.
            let first = map_file(&input).expect("first map_file builds and answers");
            assert!(
                first["community_id"].is_null() || first["community_id"].is_number(),
                "global computed on miss: {first}"
            );
            // x.ts's resolved import edge AND call edge both count (fan_in = 2).
            assert_eq!(
                first["fan_in"], 2,
                "x.ts fan_in counts a.ts's import + call: {first}"
            );
            // ...but only ONE distinct file (a.ts) depends on x.ts — the "fan"
            // answer callers actually want, not the raw reference total.
            assert_eq!(
                first["fan_in_files"], 1,
                "x.ts fan_in_files dedups a.ts's two edges to one file: {first}"
            );
            // x.ts depends on no other file, so fan_out_files is 0.
            assert_eq!(
                first["fan_out_files"], 0,
                "x.ts has no outgoing file dependencies: {first}"
            );

            // Retarget a.ts away from x.ts (add y.ts); no manual rebuild.
            let y = root.join("y.ts");
            std::fs::write(&y, "export const foo = () => 2;\n").expect("write y.ts");
            std::fs::write(&a, "import { foo } from \"./y\";\nfoo();\n").expect("rewrite a.ts");

            let second = map_file(&input).expect("freshens global and answers");
            assert_eq!(
                second["fan_in"], 0,
                "x.ts fan_in drops after retarget: {second}"
            );

            // Parity: a full build + re-query answers identically on every
            // semantic field. (`community_id` is a rowid — communities are
            // re-inserted by rebuilds with a different AUTOINCREMENT
            // high-water mark, so the raw id legitimately differs; the label
            // is the deterministic projection.)
            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let full = map_file(&input).expect("full-build query answers");
            let mut second_semantic = second.clone();
            second_semantic
                .as_object_mut()
                .unwrap()
                .remove("community_id");
            let mut full_semantic = full.clone();
            full_semantic
                .as_object_mut()
                .unwrap()
                .remove("community_id");
            assert_eq!(
                second_semantic, full_semantic,
                "map_file parity after full build"
            );
            assert_eq!(
                second["community_label"], full["community_label"],
                "community label parity: {second} vs {full}"
            );

            let db = crate::db::path::repo_db_path(&root);
            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }
}
