//! Cross-file resolution and graph algorithms.
//!
//! Consumes `parsing-extraction`'s handoff structure (flat `Vec<Entity>` /
//! `Vec<Symbol>` per file, no pre-built indices) and produces the resolved
//! graph: file nodes, call/import edges, communities, and clone bands —
//! all in memory. Persistence belongs to the `sqlite-persistence` phase.
//!
//! Entry point: [`resolve`].

pub mod clones;
pub mod community;
pub mod graph;

use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

use crate::model::{Entity, Symbol};

/// Index of a file node in `ResolvedGraph::nodes` (deterministic order:
/// sorted by path).
pub type FileId = u32;

/// What a resolved edge points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum EdgeTarget {
    /// Target file id (import edges).
    File(FileId),
    /// Target entity id (call edges — the entity invoked).
    Entity(u32),
    /// No resolvable target (unresolved edges).
    Unknown,
}

/// Kind of a resolved edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Call,
    Import,
    /// A class/interface `extends` relationship in the resolved type
    /// hierarchy (superset addition; not yet built by resolve.rs).
    Extends,
    /// A class `implements` relationship in the resolved type hierarchy
    /// (superset addition; not yet built by resolve.rs).
    Implements,
}

impl EdgeKind {
    pub fn as_i64(self) -> i64 {
        match self {
            EdgeKind::Call => 0,
            EdgeKind::Import => 1,
            EdgeKind::Extends => 2,
            EdgeKind::Implements => 3,
        }
    }

    pub fn from_i64(v: i64) -> Option<Self> {
        Some(match v {
            0 => EdgeKind::Call,
            1 => EdgeKind::Import,
            2 => EdgeKind::Extends,
            3 => EdgeKind::Implements,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Call => "call",
            EdgeKind::Import => "import",
            EdgeKind::Extends => "extends",
            EdgeKind::Implements => "implements",
        }
    }
}

/// One file in the resolved graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileNode {
    pub path: String,
    /// Community id assigned by community detection (`None` before it runs).
    pub community_id: Option<u32>,
    /// Incoming resolved-edge count (imports targeting this file, calls
    /// invoking an entity declared in this file).
    pub fan_in: u32,
    /// Outgoing resolved-edge count.
    pub fan_out: u32,
}

/// One call or import reference between files (resolved or dangling).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedEdge {
    /// File id of the referencing file.
    pub from: FileId,
    /// Resolved target (`EdgeTarget::File` for imports, `EdgeTarget::Entity`
    /// for calls) or `EdgeTarget::Unknown` when unresolved.
    pub to: EdgeTarget,
    pub kind: EdgeKind,
    pub resolved: bool,
    /// Entity index (into the same flat entity slice `EdgeTarget::Entity`
    /// indexes into) of the specific call-site/import-statement entity that
    /// produced this edge. For `EdgeKind::Import` this is the Import entity
    /// itself, letting rules join back to it (e.g. `circular_import.toml`
    /// checks `entities.body_shape = 'type_only'` to exclude
    /// `TYPE_CHECKING`/`import type`-guarded imports from the runtime
    /// import graph). Lets rules aggregate per-function (via a self-join
    /// back to this entity's `enclosing_function`) instead of only per-file
    /// — see
    /// `vertical-slice-sprawl` in rules/builtin.
    pub from_entity: Option<u32>,
}

/// A community: a densely-connected group of files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Community {
    pub id: u32,
    /// Member file paths.
    pub members: Vec<String>,
}

/// A clone band: a group of near-duplicate entities (member entity ids).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloneBand {
    pub id: u32,
    pub members: Vec<u32>,
}

/// Result of running resolution over extracted entities/symbols.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedGraph {
    pub nodes: Vec<FileNode>,
    pub edges: Vec<ResolvedEdge>,
    pub communities: Vec<Community>,
    pub clone_bands: Vec<CloneBand>,
}

/// Resolve extracted entities/symbols into a cross-file graph.
///
/// Deterministic: node order is sorted by path; edge/band order follows
/// input order. Unresolved references are left dangling (`resolved == false`)
/// and reported via `tracing` diagnostics — the run always continues.
pub fn resolve(entities: &[Entity], symbols: &[Symbol], files: &[String]) -> Result<ResolvedGraph> {
    let _ = symbols;
    let resolve_span = tracing::info_span!("resolve");
    let _guard = resolve_span.enter();
    let profile = std::env::var_os("VARDE_PROFILE").is_some();
    let t = std::time::Instant::now();
    macro_rules! checkpoint {
        ($label:expr) => {
            if profile {
                eprintln!(
                    "VARDE_PROFILE resolve: {} done at {:?}",
                    $label,
                    t.elapsed()
                );
            }
        };
    }

    // `entity.file_id`/`files` are already aligned: scan assigns file ids
    // from the same sorted, deduped file list this table is built from, so
    // no path->id map needs rebuilding here (previously the dominant cost of
    // this stage, run on top of an identical dedup scan already done once
    // upstream).
    let mut nodes = graph::build_nodes(files);
    checkpoint!("build_nodes");

    let mut edges = {
        let _span = tracing::info_span!("import_resolution").entered();
        resolve_imports(entities, files, None)
    };
    checkpoint!("import_resolution");
    edges.extend({
        let _span = tracing::info_span!("call_resolution").entered();
        resolve_calls(entities, &edges, files, None)
    });
    checkpoint!("call_resolution");
    edges.extend({
        let _span = tracing::info_span!("type_hierarchy_resolution").entered();
        resolve_type_hierarchy(entities, None)
    });
    checkpoint!("type_hierarchy_resolution");
    {
        let _span = tracing::info_span!("graph_construction").entered();
        graph::compute_fan_metrics(&mut nodes, &edges, entities);
    }
    checkpoint!("graph_construction");
    let communities = {
        let _span = tracing::info_span!("community_detection").entered();
        community::detect(&mut nodes, &edges, entities)
    };
    checkpoint!("community_detection");
    let clone_bands = {
        let _span = tracing::info_span!("clone_detection").entered();
        clones::detect_bands(entities)
    };
    checkpoint!("clone_detection");

    Ok(ResolvedGraph {
        nodes,
        edges,
        communities,
        clone_bands,
    })
}

/// Resolve only the edge layer of the graph — import edges + call edges plus
/// node fan-in/fan-out — skipping the global layer (communities, clone bands)
/// that the `edges` slice doesn't consume. Same inputs and alignment rules as
/// [`resolve`]; used by `slice::ensure_fresh(Edges)` to rebuild the edges
/// slice on read without paying for Louvain/MinHash.
pub(crate) fn resolve_edges_only(
    entities: &[Entity],
    files: &[String],
) -> (Vec<FileNode>, Vec<ResolvedEdge>) {
    let mut nodes = graph::build_nodes(files);
    let mut edges = resolve_imports(entities, files, None);
    edges.extend(resolve_calls(entities, &edges, files, None));
    edges.extend(resolve_type_hierarchy(entities, None));
    graph::compute_fan_metrics(&mut nodes, &edges, entities);
    (nodes, edges)
}

/// The scope of one scoped edge refresh, produced once by
/// `slice::freshen_edges` and consumed by [`resolve_edges_only_scoped`] (the
/// flat file-index emit set) and `persist::rewrite_edges_for_files` (the db
/// file ids for delete/insert/stamp). Carries both index spaces as one unit
/// so the two can't be mixed at the call sites (the boundary where index-
/// space mixups would otherwise live).
pub(crate) struct ScopedEdgeRefresh {
    /// Persisted `files.id` values of the scoped files.
    pub file_ids: Vec<i64>,
    /// Flat file indices (positions in the persisted `files` order) of the
    /// same files — the resolve emit filter.
    pub flat_scope: std::collections::HashSet<u32>,
}

/// Resolve only the edge layer — import edges + call edges — emitting edges
/// **only for the files in `refresh.flat_scope`**, while still building target
/// indexes from the *full* entity table (a scoped caller can resolve to a
/// target in any file, in or out of scope). Fan metrics are not computed
/// here: the caller (`slice::freshen_edges`' scoped path) applies fan as a
/// delta against the existing `files.fan_*` values, so this returns the edge
/// list only.
pub(crate) fn resolve_edges_only_scoped(
    entities: &[Entity],
    files: &[String],
    refresh: &ScopedEdgeRefresh,
) -> Vec<ResolvedEdge> {
    let emit = &refresh.flat_scope;
    let mut edges = resolve_imports(entities, files, Some(emit));
    edges.extend(resolve_calls(entities, &edges, files, Some(emit)));
    edges.extend(resolve_type_hierarchy(entities, Some(emit)));
    edges
}

/// Resolve every `Import` entity to its target file.
///
/// Matching strategy (same-language only): the normalized specifier is tried
/// whole against file stems, then with trailing path segments dropped one at
/// a time (so `use crate::b::helper;` matches `b.rs`, and `import ... from
/// './util'` matches `util.ts`). Relative paths are resolved against the
/// importing file's directory first. Ambiguous matches (multiple candidates)
/// stay unresolved.
pub(crate) fn resolve_imports(
    entities: &[Entity],
    files: &[String],
    emit: Option<&std::collections::HashSet<u32>>,
) -> Vec<ResolvedEdge> {
    let by_path: std::collections::HashMap<&str, u32> = files
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i as u32))
        .collect();
    let stem_index = build_stem_index(files);

    let mut edges = Vec::new();
    for (i, entity) in entities.iter().enumerate() {
        if entity.kind != crate::model::EntityKind::Import {
            continue;
        }
        if emit.is_some_and(|s| !s.contains(&entity.file_id)) {
            continue;
        }
        let from_id = entity.file_id;
        let Some(from_file) = files.get(from_id as usize) else {
            // `entity.file_id` should always be a valid index into `files`
            // (both come from the same extraction pass); skip rather than
            // panic if a caller ever passes a truncated/scoped slice.
            tracing::debug!(
                file_id = from_id,
                "import entity references an out-of-range file id; skipping"
            );
            continue;
        };
        let target = match_import_target(&entity.name, from_file, &by_path, &stem_index);
        if target.is_none() {
            // debug, not warn: on a large repo this fires per-unresolved-import
            // (often tens of thousands of times) — warn-level volume made
            // `build`'s log output balloon to 100+MB and dominate wall time
            // writing it.
            tracing::debug!(
                file = %from_file,
                kind = "import",
                specifier = %entity.name,
                "unresolved reference"
            );
        }
        edges.push(ResolvedEdge {
            from: from_id,
            to: match target {
                Some(t) => EdgeTarget::File(t),
                None => EdgeTarget::Unknown,
            },
            kind: EdgeKind::Import,
            resolved: target.is_some(),
            from_entity: Some(i as u32),
        });
    }
    edges
}

/// `stem -> (file id, language)` index, used by [`match_import_target`] to
/// avoid a linear scan of every file per import per path segment. Files are
/// grouped by stem only (not stem+language) because an importer with no
/// resolvable language (`from_lang == None`) matches candidates of any
/// language — matching the pre-index behavior exactly.
type StemIndex =
    std::collections::HashMap<String, Vec<(u32, Option<ast_grep_language::SupportLang>)>>;

/// Build the stem index once per resolve pass (not once per import).
fn build_stem_index(files: &[String]) -> StemIndex {
    let mut index: StemIndex = std::collections::HashMap::new();
    for (i, p) in files.iter().enumerate() {
        let path = std::path::Path::new(p);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let lang = crate::parse::language_for_path(path);
        index.entry(stem).or_default().push((i as u32, lang));
    }
    index
}

/// Find the file an import specifier points at, or `None`.
fn match_import_target(
    spec: &str,
    from_file: &str,
    by_path: &std::collections::HashMap<&str, u32>,
    stem_index: &StemIndex,
) -> Option<u32> {
    // 1. Relative-path resolution against the importing file's directory,
    //    using the unquoted specifier BEFORE prefix stripping (so `../x`
    //    and `./x` resolve from the importing file, not globally).
    if let Some(rel) = resolve_relative(spec, from_file)
        && let Some(&pos) = by_path.get(rel.as_str())
    {
        return Some(pos);
    }

    let norm = normalize_spec(spec);
    if norm.is_empty() {
        return None;
    }

    // 2. Segment-wise stem matching: try the full path first, then drop
    //    trailing segments one at a time. A single same-language candidate
    //    wins; ambiguity stays unresolved (deterministic).
    let segments = split_keep_segments(&norm);
    let from_lang = crate::parse::language_for_path(std::path::Path::new(from_file));
    for keep in (1..=segments.len()).rev() {
        let last = &segments[keep - 1];
        let matches: Vec<u32> = stem_index
            .get(last.as_str())
            .into_iter()
            .flatten()
            .filter(|(_, lang)| from_lang.is_none() || *lang == from_lang)
            .map(|(i, _)| *i)
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
        if matches.len() > 1 {
            // ambiguous — deterministic unresolved
            return None;
        }
    }
    None
}

fn split_keep_segments(s: &str) -> Vec<String> {
    s.split([':', '/'])
        .filter(|x| !x.is_empty())
        .map(|x| x.to_string())
        .collect()
}
fn normalize_spec(spec: &str) -> String {
    let mut t = spec.trim().to_string();
    // strip quotes
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' && b[t.len() - 1] == b'"') || (b[0] == b'\'' && b[t.len() - 1] == b'\'') {
            t = t[1..t.len() - 1].to_string();
        }
    }
    loop {
        let prev = t.clone();
        for prefix in ["./", "../", "crate::", "self::", "super::"] {
            if let Some(stripped) = t.strip_prefix(prefix) {
                t = stripped.to_string();
            }
        }
        if t == prev {
            break;
        }
    }
    // strip a trailing file extension
    if let Some((head, ext)) = t.rsplit_once('.')
        && ext.len() <= 4
        && !head.is_empty()
        && !ext.contains('/')
        && !ext.contains(':')
    {
        t = head.to_string();
    }
    t
}

/// Resolve a (quote-stripped) specifier as a relative path against the
/// importing file's directory, returning the candidate path if plausible.
fn resolve_relative(spec: &str, from_file: &str) -> Option<String> {
    let t = spec.trim();
    let unquoted = if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' && b[t.len() - 1] == b'"') || (b[0] == b'\'' && b[t.len() - 1] == b'\'') {
            &t[1..t.len() - 1]
        } else {
            t
        }
    } else {
        t
    };
    if !unquoted.contains('/') {
        return None;
    }
    let dir = std::path::Path::new(from_file).parent()?;
    Some(dir.join(unquoted).to_string_lossy().into_owned())
}

#[cfg(test)]
mod resolved_graph_scaffold {
    use super::*;

    #[test]
    fn empty_input_returns_empty_graph() {
        let graph = resolve(&[], &[], &[]).expect("resolve succeeds on empty input");
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
        assert!(graph.communities.is_empty());
        assert!(graph.clone_bands.is_empty());
    }
}

#[cfg(test)]
mod test_util {
    use super::*;
    use crate::extract;
    use crate::parse::parse_file;

    /// Parse + extract every fixture file under
    /// `tests/resolve_fixtures/<rel>` (non-recursive, sorted).
    pub fn load_project(rel: &str) -> (Vec<Entity>, Vec<Symbol>, Vec<String>) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/resolve_fixtures")
            .join(rel);
        let mut paths: Vec<_> = std::fs::read_dir(&root)
            .expect("resolve fixture dir exists")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        paths.sort();
        let files: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut entities = Vec::new();
        let mut symbols = Vec::new();
        for (file_id, path) in paths.iter().enumerate() {
            let parsed = parse_file(path)
                .expect("parse_file ok")
                .expect("fixture is a supported language");
            let result = extract::extract(&parsed, file_id as u32);
            entities.extend(result.entities);
            symbols.extend(result.symbols);
        }
        (entities, symbols, files)
    }

    /// File id of the node whose path ends with `suffix`.
    pub fn file_id(graph: &ResolvedGraph, suffix: &str) -> u32 {
        graph
            .nodes
            .iter()
            .position(|n| n.path.ends_with(suffix))
            .expect("node exists") as u32
    }
}

#[cfg(test)]
mod import_resolution {
    use super::test_util::{file_id, load_project};
    use super::*;

    #[test]
    fn same_language_import_resolves() {
        let (entities, symbols, files) = load_project("rust/imports");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let a = file_id(&graph, "/a.rs");
        let b = file_id(&graph, "/b.rs");

        let import_edges: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import && e.resolved)
            .collect();
        assert_eq!(import_edges.len(), 1, "exactly one resolved import edge");
        let edge = import_edges[0];
        assert_eq!(edge.from, a);
        assert_eq!(edge.to, EdgeTarget::File(b));
    }

    #[test]
    fn dangling_import_unresolved() {
        let (entities, symbols, files) = load_project("rust/imports");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let c = file_id(&graph, "/c.rs");

        let unresolved: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import && !e.resolved)
            .collect();
        assert_eq!(unresolved.len(), 1, "exactly one unresolved import edge");
        assert_eq!(unresolved[0].from, c);
        assert_eq!(unresolved[0].to, EdgeTarget::Unknown);
    }
}

/// Resolve call entities to their invoked targets.
///
/// Pass 1 (same-file): match the callee against Function/Class/Interface
/// entities in the same file — exact name first, then last-path-segment.
/// Pass 2 (cross-file): for calls still unresolved, match against callable
/// or exported entities of files imported by the calling file. A call
/// resolves only when exactly one (module, entity) candidate matches;
/// ambiguous or absent matches stay unresolved.
/// Exact and normalized name indexes of callable entities, with name keys
/// borrowed from `entities` (no `String` clones).
struct CallableIndex<'a> {
    exact: HashMap<&'a str, usize>,
    normalized: HashMap<&'a str, usize>,
}

/// Exported-name index keyed by file id, with name keys borrowed from
/// `entities` (no `String` clones).
struct ExportIndex<'a> {
    exact: HashMap<u32, HashMap<&'a str, u32>>,
    normalized: HashMap<u32, HashMap<&'a str, Vec<u32>>>,
}

/// Build the callable (same-file) and export (cross-file) name indexes in a
/// single pass over entities.
///
/// The two previous functions each walked all entities — cloning `e.name` for
/// the exact key and allocating a normalized-key `String` (`callee_key(..)
/// .to_owned()`), ~2–4 M allocations per build. `callee_key` already returns a
/// `&str` borrowed from the entity, which outlives both indexes, so the keys
/// can be borrowed directly and the two passes merged into one.
fn build_call_and_export_indexes<'a>(
    entities: &'a [Entity],
) -> (HashMap<u32, CallableIndex<'a>>, ExportIndex<'a>) {
    let mut callables: HashMap<u32, CallableIndex<'a>> = HashMap::new();
    let mut exports = ExportIndex {
        exact: HashMap::new(),
        normalized: HashMap::new(),
    };

    for (i, e) in entities.iter().enumerate() {
        let callable = matches!(
            e.kind,
            crate::model::EntityKind::Function
                | crate::model::EntityKind::Class
                | crate::model::EntityKind::Interface
        );
        if callable {
            let index = callables.entry(e.file_id).or_insert_with(|| CallableIndex {
                exact: HashMap::new(),
                normalized: HashMap::new(),
            });
            index.exact.entry(e.name.as_str()).or_insert(i);
            index.normalized.entry(callee_key(&e.name)).or_insert(i);
        }

        if callable || e.kind == crate::model::EntityKind::Export {
            let index = exports.exact.entry(e.file_id).or_default();
            let entity_id = *index.entry(e.name.as_str()).or_insert(i as u32);
            exports
                .normalized
                .entry(e.file_id)
                .or_default()
                .entry(callee_key(&e.name))
                .or_default()
                .push(entity_id);
        }
    }

    (callables, exports)
}

/// Build from file id -> target file ids of resolved import edges.
fn build_import_targets(import_edges: &[ResolvedEdge]) -> std::collections::HashMap<u32, Vec<u32>> {
    use std::collections::HashMap;

    let mut imports_by: HashMap<u32, Vec<u32>> = HashMap::new();
    for edge in import_edges {
        if edge.kind == EdgeKind::Import
            && edge.resolved
            && let EdgeTarget::File(to) = edge.to
        {
            imports_by.entry(edge.from).or_default().push(to);
        }
    }
    imports_by
}

fn resolve_calls(
    entities: &[Entity],
    import_edges: &[ResolvedEdge],
    files: &[String],
    emit: Option<&std::collections::HashSet<u32>>,
) -> Vec<ResolvedEdge> {
    use rayon::prelude::*;

    let (callables, exports) = build_call_and_export_indexes(entities);
    let imports_by = build_import_targets(import_edges);

    // Call entity indices, in entity order. Parallelizing over these and
    // collecting via indexed `map` preserves the deterministic edge order.
    let call_indices: Vec<usize> = entities
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == crate::model::EntityKind::Call)
        .filter(|(_, e)| emit.is_none_or(|s| s.contains(&e.file_id)))
        .map(|(i, _)| i)
        .collect();

    call_indices
        .par_iter()
        .map(|&i| {
            let e = &entities[i];
            let from = e.file_id;
            let key = callee_key(&e.name);

            // Pass 1: same-file.
            let same = callables.get(&e.file_id).and_then(|index| {
                index
                    .exact
                    .get(key)
                    .or_else(|| index.normalized.get(key))
                    .copied()
                    .map(|idx| idx as u32)
            });

            // Pass 2: cross-file via resolved imports.
            let cross = if same.is_none() {
                cross_file_call_target(&imports_by, &exports, from, key)
            } else {
                None
            };

            let target = same.or(cross);
            if target.is_none() {
                // debug, not warn — see the import-resolution
                // unresolved-reference comment above; calls are the
                // higher-volume case. Log line order may vary under
                // parallelism (stderr only).
                tracing::debug!(
                    file = %files.get(e.file_id as usize).map(String::as_str).unwrap_or("<unknown>"),
                    callee = %e.name,
                    "unresolved reference"
                );
            }
            ResolvedEdge {
                from,
                to: target.map_or(EdgeTarget::Unknown, EdgeTarget::Entity),
                kind: EdgeKind::Call,
                resolved: target.is_some(),
                from_entity: Some(i as u32),
            }
        })
        .collect()
}
/// Cross-file call resolution: find the single (module, entity) candidate
/// across the calling file's resolved imports whose exported name matches
/// `key` (exact, then last-segment). Deterministic: exported names are
/// visited in sorted order; more than one match (across modules or via
/// same-file name collisions) stays unresolved.
fn cross_file_call_target<'a>(
    imports_by: &std::collections::HashMap<u32, Vec<u32>>,
    exports: &ExportIndex<'a>,
    from: u32,
    key: &str,
) -> Option<u32> {
    // Order never affects the result: a single match wins regardless of
    // visitation order, and ambiguity (more than one distinct target) always
    // resolves to `None`. Track the first distinct target in an `Option`
    // accumulator and early-exit on a second distinct target, avoiding a
    // per-call `HashSet` allocation.
    let mut first: Option<u32> = None;
    if let Some(targets) = imports_by.get(&from) {
        for t in targets {
            if let Some(idx) = exports.exact.get(t).and_then(|names| names.get(key)) {
                match first {
                    Some(seen) if seen != *idx => return None,
                    None => first = Some(*idx),
                    _ => {}
                }
            }
            if let Some(indices) = exports.normalized.get(t).and_then(|names| names.get(key)) {
                for &idx in indices {
                    match first {
                        Some(seen) if seen != idx => return None,
                        None => first = Some(idx),
                        _ => {}
                    }
                }
            }
        }
    }
    first
}

/// Resolve `Extends`/`Implements` entities into type-hierarchy edges.
///
/// Mirrors [`resolve_imports`]'s shape: each `Extends`/`Implements` entity
/// carries the raw supertype/interface name pre-resolution on its `name`
/// field (same convention as `Import.name` holding the raw specifier), and
/// resolution either finds a matching `Class`/`Interface` entity anywhere in
/// the repo (`resolved = true`, `to = EdgeTarget::Entity`) or leaves the edge
/// dangling (`resolved = false`, `to = EdgeTarget::Unknown`) exactly like an
/// unresolved import — the raw name stays readable on the source entity
/// either way, so nothing is lost when the supertype is external.
///
/// The owning class/interface (the edge's `from_entity`) is looked up via
/// `entity.enclosing_function`: extractors set it to the name of the
/// class/interface the `Extends`/`Implements` entity belongs to, reusing the
/// existing "name of the nearest enclosing named construct" field (today
/// populated for control-flow/error entities with the enclosing *function*;
/// here the enclosing construct is the class/interface declaration itself)
/// rather than adding a new field.
pub(crate) fn resolve_type_hierarchy(
    entities: &[Entity],
    emit: Option<&std::collections::HashSet<u32>>,
) -> Vec<ResolvedEdge> {
    // Global name -> entity index of Class/Interface entities. A name that
    // resolves to more than one entity anywhere in the repo is ambiguous and
    // stays unresolved (`None`), matching the deterministic-ambiguity
    // convention used elsewhere in this module.
    let mut by_name: HashMap<&str, Option<u32>> = HashMap::new();
    for (i, e) in entities.iter().enumerate() {
        if matches!(
            e.kind,
            crate::model::EntityKind::Class | crate::model::EntityKind::Interface
        ) {
            by_name
                .entry(e.name.as_str())
                .and_modify(|v| *v = None)
                .or_insert(Some(i as u32));
        }
    }

    // (file_id, class/interface name) -> entity index, so finding the owner
    // of an Extends/Implements entity below is an O(1) lookup instead of a
    // full linear scan over every entity in the repo. On a name collision
    // within the same file, keep the first — matches the old scan's
    // first-match `position()` semantics.
    let mut owner_by_file_and_name: HashMap<(u32, &str), u32> = HashMap::new();
    for (i, e) in entities.iter().enumerate() {
        if matches!(
            e.kind,
            crate::model::EntityKind::Class | crate::model::EntityKind::Interface
        ) {
            owner_by_file_and_name
                .entry((e.file_id, e.name.as_str()))
                .or_insert(i as u32);
        }
    }

    let mut edges = Vec::new();
    for entity in entities {
        let kind = match entity.kind {
            crate::model::EntityKind::Extends => EdgeKind::Extends,
            crate::model::EntityKind::Implements => EdgeKind::Implements,
            _ => continue,
        };
        if emit.is_some_and(|s| !s.contains(&entity.file_id)) {
            continue;
        }

        let from_entity = entity.enclosing_function.as_deref().and_then(|owner_name| {
            owner_by_file_and_name
                .get(&(entity.file_id, owner_name))
                .copied()
        });

        let target = by_name.get(entity.name.as_str()).copied().flatten();
        if target.is_none() {
            tracing::debug!(
                file_id = entity.file_id,
                kind = kind.as_str(),
                supertype = %entity.name,
                "unresolved reference"
            );
        }

        edges.push(ResolvedEdge {
            from: entity.file_id,
            to: target.map_or(EdgeTarget::Unknown, EdgeTarget::Entity),
            kind,
            resolved: target.is_some(),
            from_entity,
        });
    }
    edges
}

/// Normalize a callee name for matching: drop a trailing `()`, then take the
/// last path segment (`util::do_thing` -> `do_thing`, `obj.method` ->
/// `method`, `new Foo` -> `Foo`).
fn callee_key(name: &str) -> &str {
    let t = name.trim();
    let t = t.strip_suffix("()").map(str::trim).unwrap_or(t);
    t.rsplit(['.', ':', ' ']).next().unwrap_or(t)
}

#[cfg(test)]
mod call_resolution_same_file {
    use super::test_util::{file_id, load_project};
    use super::*;

    #[test]
    fn local_call_resolves() {
        let (entities, symbols, files) = load_project("rust/calls");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let app = file_id(&graph, "/app.rs");
        let local_fn = entities
            .iter()
            .position(|e| e.kind == crate::model::EntityKind::Function && e.name == "local_fn")
            .expect("local_fn entity exists") as u32;

        let call_edges: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Call)
            .collect();
        assert_eq!(
            call_edges.len(),
            2,
            "two call edges (one resolved, one not)"
        );
        let resolved: Vec<&&ResolvedEdge> = call_edges.iter().filter(|e| e.resolved).collect();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].from, app);
        assert_eq!(resolved[0].to, EdgeTarget::Entity(local_fn));
    }

    #[test]
    fn undefined_symbol_unresolved() {
        let (entities, symbols, files) = load_project("rust/calls");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let app = file_id(&graph, "/app.rs");

        let unresolved: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Call && !e.resolved)
            .collect();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].from, app);
        assert_eq!(unresolved[0].to, EdgeTarget::Unknown);
    }
}

#[cfg(test)]
mod call_resolution_cross_file {
    use super::test_util::{file_id, load_project};
    use super::*;

    #[test]
    fn imported_module_call_resolves() {
        let (entities, symbols, files) = load_project("rust/imports");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let a = file_id(&graph, "/a.rs");
        let helper = entities
            .iter()
            .position(|e| e.kind == crate::model::EntityKind::Function && e.name == "helper")
            .expect("helper entity exists in b.rs") as u32;

        let call_edges: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Call)
            .collect();
        assert_eq!(call_edges.len(), 1, "a.rs has one call");
        assert!(call_edges[0].resolved);
        assert_eq!(call_edges[0].from, a);
        assert_eq!(call_edges[0].to, EdgeTarget::Entity(helper));
    }

    #[test]
    fn unresolved_after_both_passes() {
        let (entities, symbols, files) = load_project("rust/calls");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let app = file_id(&graph, "/app.rs");

        let unresolved: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Call && !e.resolved)
            .collect();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].from, app);
        assert_eq!(unresolved[0].to, EdgeTarget::Unknown);
    }
}

#[cfg(test)]
mod tracing_capture {
    use std::sync::{Arc, Mutex};

    /// Subscriber capturing event text and span names into a shared Vec.
    pub struct CaptureSubscriber {
        pub events: Arc<Mutex<Vec<String>>>,
    }

    impl tracing::Subscriber for CaptureSubscriber {
        fn enabled(&self, _m: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn register_callsite(
            &self,
            _m: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }
        fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            self.events
                .lock()
                .unwrap()
                .push(format!("span:{}", attrs.metadata().name()));
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _s: &tracing::span::Id, _v: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _s: &tracing::span::Id, _f: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut text = format!("{}", event.metadata().level());
            event.record(&mut CaptureVisitor(&mut text));
            self.events.lock().unwrap().push(text);
        }
        fn enter(&self, _s: &tracing::span::Id) {}
        fn exit(&self, _s: &tracing::span::Id) {}
    }

    struct CaptureVisitor<'a>(&'a mut String);
    impl tracing::field::Visit for CaptureVisitor<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!(" {}={:?}", field.name(), value));
        }
    }

    /// Process-wide capture buffer. All tracing events and span creations in
    /// the test process land here once the global subscriber is installed.
    ///
    /// Why a single global buffer instead of per-test scoped subscribers:
    /// tracing's `SCOPED_COUNT` fast path is process-wide — while any other
    /// test's scoped guard is active the count is nonzero, and the moment it
    /// drops to zero a per-thread scoped subscriber is silently bypassed via
    /// the global fallback. A process-wide always-interested global default
    /// has no such window. Tests key their assertions on fixture-specific
    /// content (file paths in event fields) to stay isolated.
    static BUFFER: std::sync::LazyLock<Arc<Mutex<Vec<String>>>> =
        std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

    static INSTALL: std::sync::Once = std::sync::Once::new();
    pub fn global_capture() -> Arc<Mutex<Vec<String>>> {
        INSTALL.call_once(|| {
            let subscriber = CaptureSubscriber {
                events: BUFFER.clone(),
            };
            let _ = tracing::subscriber::set_global_default(subscriber);
            tracing::callsite::rebuild_interest_cache();
        });
        BUFFER.clone()
    }
}

#[cfg(test)]
mod unresolved_diagnostics {
    use super::test_util::load_project;
    use super::tracing_capture::global_capture;
    use super::*;

    #[test]
    fn unresolved_emits_diagnostic() {
        let (entities, symbols, files) = load_project("rust/calls");
        let events = global_capture();

        let graph =
            resolve(&entities, &symbols, &files).expect("resolve succeeds without panicking");

        // The undefined call is left dangling.
        let unresolved: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Call && !e.resolved)
            .collect();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].to, EdgeTarget::Unknown);

        // A diagnostic tracing event for this fixture's undefined call was
        // captured (keyed on the fixture file path to stay isolated from
        // other tests' diagnostics).
        let captured = events.lock().unwrap();
        assert!(
            captured.iter().any(|e| {
                e.contains("unresolved") && e.contains("call") && e.contains("rust/calls/app.rs")
            }),
            "expected an unresolved-reference diagnostic for app.rs, got: {captured:?}"
        );
    }
}

#[cfg(test)]
mod tracing_logging_wiring {
    use super::test_util::load_project;
    use super::tracing_capture::global_capture;
    use super::*;

    #[test]
    fn captures_span_per_stage() {
        let (entities, symbols, files) = load_project("rust/graph");
        let events = global_capture();

        let _ = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let captured = events.lock().unwrap();
        let spans: Vec<&str> = captured
            .iter()
            .filter_map(|e| e.strip_prefix("span:"))
            .collect();
        for stage in [
            "resolve",
            "import_resolution",
            "call_resolution",
            "graph_construction",
            "community_detection",
            "clone_detection",
        ] {
            assert!(
                spans.contains(&stage),
                "expected span {stage:?} for the stage, captured spans: {spans:?}"
            );
        }
    }
}

#[cfg(test)]
mod dependency_graph_construction {
    use super::test_util::{file_id, load_project};
    use super::*;

    #[test]
    fn multi_file_node_and_edge_counts() {
        let (entities, symbols, files) = load_project("rust/graph");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let a = file_id(&graph, "/a.rs");
        let b = file_id(&graph, "/b.rs");
        let c = file_id(&graph, "/c.rs");

        // 3 nodes, one per fixture file.
        assert_eq!(graph.nodes.len(), 3);

        // 6 edges: 3 imports (2 resolved, 1 dangling) + 3 calls
        // (2 resolved cross-file, 1 dangling).
        assert_eq!(graph.edges.len(), 6);

        let summarize = |e: &ResolvedEdge| (e.from, e.to, e.kind, e.resolved);
        let edges: Vec<_> = graph.edges.iter().map(summarize).collect();

        // Imports: a->b, a->c resolved; a->? dangling.
        assert!(edges.contains(&(a, EdgeTarget::File(b), EdgeKind::Import, true)));
        assert!(edges.contains(&(a, EdgeTarget::File(c), EdgeKind::Import, true)));
        assert!(edges.contains(&(a, EdgeTarget::Unknown, EdgeKind::Import, false)));

        // Calls: a calls fn_b (b), fn_c (c); ghost() dangling.
        let fn_b = entities
            .iter()
            .position(|e| e.kind == crate::model::EntityKind::Function && e.name == "fn_b")
            .expect("fn_b entity") as u32;
        let fn_c = entities
            .iter()
            .position(|e| e.kind == crate::model::EntityKind::Function && e.name == "fn_c")
            .expect("fn_c entity") as u32;
        assert!(edges.contains(&(a, EdgeTarget::Entity(fn_b), EdgeKind::Call, true)));
        assert!(edges.contains(&(a, EdgeTarget::Entity(fn_c), EdgeKind::Call, true)));
        assert!(edges.contains(&(a, EdgeTarget::Unknown, EdgeKind::Call, false)));
    }
}

#[cfg(test)]
mod fan_in_fan_out_metrics {
    use super::test_util::{file_id, load_project};
    use super::*;

    #[test]
    fn three_file_known_topology_counts() {
        // Topology: a -> b, a -> c, b -> c (all resolved imports).
        let (entities, symbols, files) = load_project("rust/fan");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let a = file_id(&graph, "/a.rs");
        let b = file_id(&graph, "/b.rs");
        let c = file_id(&graph, "/c.rs");

        // Only the three resolved import edges count.
        let resolved = graph.edges.iter().filter(|e| e.resolved).count();
        assert_eq!(resolved, 3);

        let node = |id: u32| &graph.nodes[id as usize];
        assert_eq!(node(a).fan_out, 2);
        assert_eq!(node(a).fan_in, 0);
        assert_eq!(node(b).fan_out, 1);
        assert_eq!(node(b).fan_in, 1);
        assert_eq!(node(c).fan_out, 0);
        assert_eq!(node(c).fan_in, 2);
    }
}

#[cfg(test)]
mod type_hierarchy_resolution {
    use super::*;
    use crate::model::{Entity, EntityKind, Span};

    /// Hand-built entity fixture: zero span, no file/owner metadata beyond
    /// what each test needs to set explicitly.
    fn entity(kind: EntityKind, name: &str, file_id: u32) -> Entity {
        Entity {
            kind,
            name: name.to_string(),
            file_id,
            span: Span {
                start_byte: 0,
                end_byte: 0,
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
            },
            enclosing_function: None,
            method: None,
            path: None,
            status: None,
            body_shape: None,
            body_minhash: None,
            is_async: None,
            is_test: false,
            owner_type: None,
        }
    }

    #[test]
    fn resolvable_supertype_produces_resolved_extends_edge() {
        // class Base {} in file 0; class Sub extends Base in file 1.
        let base = entity(EntityKind::Class, "Base", 0);
        let sub = entity(EntityKind::Class, "Sub", 1);
        let mut extends = entity(EntityKind::Extends, "Base", 1);
        extends.enclosing_function = Some("Sub".to_string());
        let entities = vec![base, sub, extends];

        let edges = resolve_type_hierarchy(&entities, None);

        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert!(edge.resolved);
        assert_eq!(edge.kind, EdgeKind::Extends);
        assert_eq!(edge.from, 1);
        assert_eq!(
            edge.from_entity,
            Some(1),
            "from_entity is the Sub class entity"
        );
        assert_eq!(
            edge.to,
            EdgeTarget::Entity(0),
            "to is the Base class entity"
        );
    }

    #[test]
    fn unresolvable_supertype_produces_unresolved_edge_with_raw_name_preserved() {
        // class Sub extends ExternalFramework::Base (never defined in-repo).
        let sub = entity(EntityKind::Class, "Sub", 0);
        let mut extends = entity(EntityKind::Extends, "ExternalBase", 0);
        extends.enclosing_function = Some("Sub".to_string());
        let entities = vec![sub, extends];

        let edges = resolve_type_hierarchy(&entities, None);

        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert!(!edge.resolved);
        assert_eq!(edge.kind, EdgeKind::Extends);
        assert_eq!(edge.to, EdgeTarget::Unknown, "to_entity_id stays NULL");
        assert_eq!(
            edge.from_entity,
            Some(0),
            "from_entity is still the Sub class entity"
        );
        // Raw supertype name preserved on the source (Extends) entity's
        // `name` field regardless of resolution outcome.
        assert_eq!(entities[1].name, "ExternalBase");
    }

    #[test]
    fn resolvable_interface_produces_resolved_implements_edge() {
        let iface = entity(EntityKind::Interface, "Comparable", 0);
        let class = entity(EntityKind::Class, "Widget", 1);
        let mut implements = entity(EntityKind::Implements, "Comparable", 1);
        implements.enclosing_function = Some("Widget".to_string());
        let entities = vec![iface, class, implements];

        let edges = resolve_type_hierarchy(&entities, None);

        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert!(edge.resolved);
        assert_eq!(edge.kind, EdgeKind::Implements);
        assert_eq!(edge.from_entity, Some(1));
        assert_eq!(edge.to, EdgeTarget::Entity(0));
    }

    #[test]
    fn ambiguous_supertype_name_stays_unresolved() {
        // Two distinct classes named "Base" in different files: ambiguous.
        let base1 = entity(EntityKind::Class, "Base", 0);
        let base2 = entity(EntityKind::Class, "Base", 1);
        let sub = entity(EntityKind::Class, "Sub", 2);
        let mut extends = entity(EntityKind::Extends, "Base", 2);
        extends.enclosing_function = Some("Sub".to_string());
        let entities = vec![base1, base2, sub, extends];

        let edges = resolve_type_hierarchy(&entities, None);

        assert_eq!(edges.len(), 1);
        assert!(!edges[0].resolved);
        assert_eq!(edges[0].to, EdgeTarget::Unknown);
    }

    #[test]
    fn emit_scope_filters_by_file_id() {
        let base = entity(EntityKind::Class, "Base", 0);
        let sub_in = entity(EntityKind::Class, "SubIn", 1);
        let sub_out = entity(EntityKind::Class, "SubOut", 2);
        let mut extends_in = entity(EntityKind::Extends, "Base", 1);
        extends_in.enclosing_function = Some("SubIn".to_string());
        let mut extends_out = entity(EntityKind::Extends, "Base", 2);
        extends_out.enclosing_function = Some("SubOut".to_string());
        let entities = vec![base, sub_in, sub_out, extends_in, extends_out];

        let scope: std::collections::HashSet<u32> = [1].into_iter().collect();
        let edges = resolve_type_hierarchy(&entities, Some(&scope));

        assert_eq!(edges.len(), 1, "only the file-1 Extends entity is emitted");
        assert_eq!(edges[0].from, 1);
    }

    #[test]
    fn missing_owner_class_still_produces_edge_with_from_entity_none() {
        // CODE-004: an Extends entity whose enclosing_function doesn't match
        // any Class/Interface in the file (owner not found) is still pushed
        // as an edge, with `from_entity: None` rather than being dropped.
        let base = entity(EntityKind::Class, "Base", 0);
        let mut extends = entity(EntityKind::Extends, "Base", 0);
        extends.enclosing_function = Some("NoSuchOwner".to_string());
        let entities = vec![base, extends];

        let edges = resolve_type_hierarchy(&entities, None);

        assert_eq!(
            edges.len(),
            1,
            "edge is still produced despite missing owner"
        );
        let edge = &edges[0];
        assert_eq!(edge.from_entity, None, "owner class not found in file");
        assert!(edge.resolved, "supertype itself still resolves");
        assert_eq!(edge.to, EdgeTarget::Entity(0));
    }

    #[test]
    fn full_resolve_pipeline_produces_implements_edge() {
        // Integration-level check (CODE-001 / ARCHITECTURE-001): a real
        // `resolve()` call over an extracted `impl Trait for Type` fixture
        // must include the Implements edge in its final output, not just the
        // unit-level `resolve_type_hierarchy()` calls above.
        let (entities, symbols, files) = super::test_util::load_project("rust/type_hierarchy");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let implements_edges: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(
            implements_edges.len(),
            1,
            "resolve() must run resolve_type_hierarchy and include its edges"
        );
        assert!(
            implements_edges[0].resolved,
            "Greet trait is defined in-repo"
        );
    }
}

#[cfg(test)]
mod community_detection_louvain {
    use super::test_util::load_project;
    use super::*;

    #[test]
    fn two_cluster_partition_matches_expected() {
        let (entities, symbols, files) = load_project("rust/communities");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        // Two disjoint 3-file clusters joined by one weak edge.
        assert_eq!(graph.communities.len(), 2);

        let cluster1: Vec<&str> = ["a.rs", "b.rs", "c.rs"].to_vec();
        let cluster2: Vec<&str> = ["d.rs", "e.rs", "f.rs"].to_vec();

        for community in &graph.communities {
            let members: Vec<&str> = community
                .members
                .iter()
                .map(|m| m.rsplit('/').next().unwrap())
                .collect();
            let matches1 = members.iter().all(|m| cluster1.contains(m)) && members.len() == 3;
            let matches2 = members.iter().all(|m| cluster2.contains(m)) && members.len() == 3;
            assert!(
                matches1 || matches2,
                "community {community:?} must be exactly one cluster"
            );
        }
    }

    #[test]
    fn community_id_matches_members() {
        let (entities, symbols, files) = load_project("rust/communities");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        for node in &graph.nodes {
            let cid = node.community_id.expect("community assigned");
            let community = graph
                .communities
                .iter()
                .find(|c| c.id == cid)
                .expect("community id exists");
            assert!(
                community.members.contains(&node.path),
                "{} must be a member of community {cid}",
                node.path
            );
        }
    }
}
