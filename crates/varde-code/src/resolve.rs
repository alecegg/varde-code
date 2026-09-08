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
use crate::parse::language_for_path;

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
    let relative_path_index = build_relative_path_index(files);
    let path_suffix_index = build_path_suffix_index(files);
    let package_index = build_package_index(files);
    let file_lang: Vec<Option<ast_grep_language::SupportLang>> = files
        .iter()
        .map(|p| crate::parse::language_for_path(std::path::Path::new(p)))
        .collect();
    let module_index = build_module_def_index(entities, &file_lang);
    // Go packages are directories, mapped through go.mod's module path — a
    // resolution model distinct from the file-stem matcher below. Only paid for
    // when the repo actually has a go.mod.
    let go_modules = discover_go_modules(files);
    let go_pkg_index = if go_modules.is_empty() {
        std::collections::HashMap::new()
    } else {
        build_go_package_index(files)
    };
    let indexes = ImportIndexes {
        by_path: &by_path,
        stem: &stem_index,
        relative: &relative_path_index,
        path_suffix: &path_suffix_index,
        package: &package_index,
        module: &module_index,
    };

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
        // Go imports name a package (a directory of `.go` files) via go.mod's
        // module path, not a single file — resolve them separately, emitting one
        // edge per file in the target package. Skip the file-stem matcher for Go
        // so a stdlib import (`fmt`, `os`) can't accidentally stem-match a local
        // file.
        if file_lang.get(from_id as usize).copied().flatten()
            == Some(ast_grep_language::SupportLang::Go)
        {
            match resolve_go_import(&entity.name, from_id, &go_modules, &go_pkg_index) {
                Some(targets) => {
                    for t in targets {
                        edges.push(ResolvedEdge {
                            from: from_id,
                            to: EdgeTarget::File(t),
                            kind: EdgeKind::Import,
                            resolved: true,
                            from_entity: Some(i as u32),
                        });
                    }
                }
                None => {
                    tracing::debug!(
                        file = %from_file,
                        kind = "import",
                        specifier = %entity.name,
                        "unresolved reference"
                    );
                    edges.push(ResolvedEdge {
                        from: from_id,
                        to: EdgeTarget::Unknown,
                        kind: EdgeKind::Import,
                        resolved: false,
                        from_entity: Some(i as u32),
                    });
                }
            }
            continue;
        }
        let target = match_import_target(&entity.name, from_file, &indexes);
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

/// Normalized, extensionless file path -> (file id, language). This serves
/// relative imports such as `./x` without making unrelated same-stem files
/// compete in the global fallback index.
type RelativePathIndex =
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

fn build_relative_path_index(files: &[String]) -> RelativePathIndex {
    let mut index: RelativePathIndex = std::collections::HashMap::new();
    for (i, file) in files.iter().enumerate() {
        let path = normalize_path(std::path::Path::new(file));
        let stem_path = path.with_extension("");
        let lang = crate::parse::language_for_path(&path);
        index
            .entry(stem_path.to_string_lossy().into_owned())
            .or_default()
            .push((i as u32, lang));
    }
    index
}

/// Filenames that make their containing *directory* an importable package: a
/// directory import (`from pkg import X`, `import a.b`, `require "a.b"`)
/// resolves to this index file rather than a same-named file. Python packages
/// use `__init__.py`; Node/TS resolve a bare directory import to `index.*`;
/// Lua's `require` finds a directory's `init.lua`.
const PACKAGE_INDEX_FILES: &[&str] = &[
    "__init__.py",
    "index.js",
    "index.jsx",
    "index.mjs",
    "index.cjs",
    "index.ts",
    "index.tsx",
    "init.lua",
];

/// `directory-path-suffix -> [(index-file id, language)]`. For a package index
/// file at `a/b/__init__.py`, every suffix of its directory (`a/b`, then `b`)
/// maps to it, so both a fully-qualified `import a.b` and a bare
/// `from b import X` can match — longest suffix tried first for specificity,
/// ambiguity left unresolved (same deterministic rule as the stem index).
type PackageIndex =
    std::collections::HashMap<String, Vec<(u32, Option<ast_grep_language::SupportLang>)>>;

/// `module-name -> [(declaring-file id, language)]` for namespaced module
/// declarations (Elixir `defmodule Foo.Bar` emits a `Class` whose name is the
/// full dotted path). Lets an `alias`/`import`/`use Foo.Bar` resolve to the
/// file that declares the module even though Elixir's snake_case file names
/// never stem-match a PascalCase module path. Only dotted names are indexed: a
/// bare `Foo` collides with every same-named class across the repo, while a
/// dotted `Foo.Bar` is specific enough to key on.
type ModuleDefIndex =
    std::collections::HashMap<String, Vec<(u32, Option<ast_grep_language::SupportLang>)>>;

/// `path-suffix -> [(file id, language)]` over every file's trailing path
/// segments (extensionless), bounded to the last [`MAX_PATH_SUFFIX`] segments.
/// Lets a multi-segment module path (`from a.b.c import X`, `use crate::a::b`)
/// resolve on its full trailing path — `crewai/agent/core` picks the one
/// `.../crewai/agent/core.py` — instead of colliding on the bare `core` stem.
type PathSuffixIndex =
    std::collections::HashMap<String, Vec<(u32, Option<ast_grep_language::SupportLang>)>>;

/// Cap on how many trailing segments the path-suffix index stores per file (and
/// the longest spec suffix tried). Module specifiers rarely need more than a
/// few trailing segments to disambiguate; the cap keeps the index bounded on
/// deep trees.
const MAX_PATH_SUFFIX: usize = 6;

/// Build the path-suffix index once per resolve pass.
fn build_path_suffix_index(files: &[String]) -> PathSuffixIndex {
    let mut index: PathSuffixIndex = std::collections::HashMap::new();
    for (i, p) in files.iter().enumerate() {
        let path = std::path::Path::new(p);
        let lang = crate::parse::language_for_path(path);
        let norm = normalize_path(path).with_extension("");
        let comps: Vec<String> = norm
            .components()
            .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
            .collect();
        let n = comps.len();
        let lo = n.saturating_sub(MAX_PATH_SUFFIX);
        for start in lo..n {
            let key = comps[start..].join("/");
            if key.is_empty() {
                continue;
            }
            index.entry(key).or_default().push((i as u32, lang));
        }
    }
    index
}

/// Build the package-index (directory imports) once per resolve pass.
fn build_package_index(files: &[String]) -> PackageIndex {
    let mut index: PackageIndex = std::collections::HashMap::new();
    for (i, p) in files.iter().enumerate() {
        let path = std::path::Path::new(p);
        let is_index = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| PACKAGE_INDEX_FILES.contains(&n));
        if !is_index {
            continue;
        }
        let Some(dir) = path.parent() else { continue };
        let lang = crate::parse::language_for_path(path);
        let dir_norm = normalize_path(dir);
        let comps: Vec<String> = dir_norm
            .components()
            .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
            .collect();
        for start in 0..comps.len() {
            let key = comps[start..].join("/");
            if key.is_empty() {
                continue;
            }
            index.entry(key).or_default().push((i as u32, lang));
        }
    }
    index
}

/// Build the namespaced-module-declaration index once per resolve pass.
fn build_module_def_index(
    entities: &[Entity],
    file_lang: &[Option<ast_grep_language::SupportLang>],
) -> ModuleDefIndex {
    let mut index: ModuleDefIndex = std::collections::HashMap::new();
    for e in entities {
        if !matches!(
            e.kind,
            crate::model::EntityKind::Class | crate::model::EntityKind::Interface
        ) {
            continue;
        }
        if !e.name.contains('.') {
            continue;
        }
        let lang = file_lang.get(e.file_id as usize).copied().flatten();
        index
            .entry(e.name.clone())
            .or_default()
            .push((e.file_id, lang));
    }
    index
}

/// Strip a single pair of matching surrounding quotes, if present.
fn strip_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' && b[t.len() - 1] == b'"') || (b[0] == b'\'' && b[t.len() - 1] == b'\'') {
            return &t[1..t.len() - 1];
        }
    }
    t
}

/// A Go module discovered from a `go.mod` in the file set: its declared module
/// path and the (normalized) directory that path maps to — the `go.mod`'s
/// parent. A Go import names a *package* (a directory of `.go` files); mapping
/// the module-path prefix to its root directory is what turns an import path
/// (`github.com/x/y/pkg`) into the repo directory holding that package.
struct GoModule {
    module_path: String,
    root_dir: String,
}

/// Extract the module path from `go.mod` contents: the token after a leading
/// `module` directive. Ignores comments/blank lines and any trailing
/// comment/quotes on the directive.
fn parse_go_module_path(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("module")
            && rest.starts_with(char::is_whitespace)
        {
            let token = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches('"');
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// Discover Go modules by reading every `go.mod` in `files` (they are indexed
/// as non-source rows). Unreadable files (e.g. synthetic test paths) and
/// go.mods without a `module` line are skipped. Sorted longest-module-path
/// first so a nested module wins over an enclosing one.
fn discover_go_modules(files: &[String]) -> Vec<GoModule> {
    let mut mods = Vec::new();
    for p in files {
        let path = std::path::Path::new(p);
        if path.file_name().and_then(|n| n.to_str()) != Some("go.mod") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(module_path) = parse_go_module_path(&contents) else {
            continue;
        };
        let Some(dir) = path.parent() else { continue };
        mods.push(GoModule {
            module_path,
            root_dir: normalize_path(dir).to_string_lossy().into_owned(),
        });
    }
    mods.sort_by_key(|m| std::cmp::Reverse(m.module_path.len()));
    mods
}

/// `normalized-directory -> [Go file ids in that directory]`. A Go package is a
/// directory of `.go` files, so this is the layout Go import resolution targets.
/// `_test.go` files are excluded: they are not part of the package's importable
/// surface (a production `import` never compiles them), so linking an importer
/// to them would invent spurious edges.
fn build_go_package_index(files: &[String]) -> std::collections::HashMap<String, Vec<u32>> {
    let mut index: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
    for (i, p) in files.iter().enumerate() {
        let path = std::path::Path::new(p);
        if crate::parse::language_for_path(path) != Some(ast_grep_language::SupportLang::Go) {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with("_test.go"))
        {
            continue;
        }
        let Some(dir) = path.parent() else { continue };
        let key = normalize_path(dir).to_string_lossy().into_owned();
        index.entry(key).or_default().push(i as u32);
    }
    index
}

/// Resolve a Go import specifier to the file ids of the target package's `.go`
/// files, or `None` when it names an external/stdlib package (no module prefix
/// matches) or the package isn't in the file set. A Go import depends on the
/// *whole* package, so it resolves to every file in the target directory (one
/// import edge each); `from_id` is excluded so a file never imports itself.
fn resolve_go_import(
    spec: &str,
    from_id: u32,
    modules: &[GoModule],
    pkg_index: &std::collections::HashMap<String, Vec<u32>>,
) -> Option<Vec<u32>> {
    let import_path = strip_quotes(spec);
    for m in modules {
        let rel = if import_path == m.module_path {
            ""
        } else if let Some(rest) = import_path.strip_prefix(&m.module_path) {
            // Require a `/` boundary so module `github.com/x/y` does not swallow
            // an unrelated `github.com/x/ya`.
            match rest.strip_prefix('/') {
                Some(sub) => sub,
                None => continue,
            }
        } else {
            continue;
        };
        let target_dir = if rel.is_empty() {
            m.root_dir.clone()
        } else {
            normalize_path(std::path::Path::new(&format!("{}/{}", m.root_dir, rel)))
                .to_string_lossy()
                .into_owned()
        };
        // The module prefix matched: the import is internal. Resolve to the
        // package's files if present, else stay unresolved — never fall through
        // to a shorter module prefix that would mis-target.
        return pkg_index.get(&target_dir).and_then(|ids| {
            let targets: Vec<u32> = ids.iter().copied().filter(|&id| id != from_id).collect();
            (!targets.is_empty()).then_some(targets)
        });
    }
    None
}

/// The set of per-pass indexes [`match_import_target`] consults, built once in
/// [`resolve_imports`]. Bundled so the matcher takes one reference instead of
/// six positional arguments.
struct ImportIndexes<'a> {
    by_path: &'a std::collections::HashMap<&'a str, u32>,
    stem: &'a StemIndex,
    relative: &'a RelativePathIndex,
    path_suffix: &'a PathSuffixIndex,
    package: &'a PackageIndex,
    module: &'a ModuleDefIndex,
}

/// Find the file an import specifier points at, or `None`.
fn match_import_target(spec: &str, from_file: &str, idx: &ImportIndexes<'_>) -> Option<u32> {
    let from_lang = crate::parse::language_for_path(std::path::Path::new(from_file));

    // 1. Relative-path resolution against the importing file's directory,
    //    using the unquoted specifier BEFORE prefix stripping (so `../x`
    //    and `./x` resolve from the importing file, not globally).
    if let Some(rel) = resolve_relative(spec, from_file) {
        if let Some(&pos) = idx.by_path.get(rel.as_str()) {
            return Some(pos);
        }

        let relative_stem = normalize_path(std::path::Path::new(&rel)).with_extension("");
        let matches: Vec<u32> = idx
            .relative
            .get(relative_stem.to_string_lossy().as_ref())
            .into_iter()
            .flatten()
            .filter(|(_, lang)| from_lang.is_none() || *lang == from_lang)
            .map(|(i, _)| *i)
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
    }

    // 2. Namespaced module-name resolution (e.g. Elixir `alias Foo.Bar`): the
    //    raw, quote-stripped specifier names a module declared somewhere in the
    //    repo. Uses the RAW spec — `normalize_spec` below would strip the
    //    trailing `.Bar` as if it were a file extension — and requires a single
    //    same-language declaring file.
    let raw = strip_quotes(spec);
    if raw.contains('.') {
        let matches: Vec<u32> = idx
            .module
            .get(raw)
            .into_iter()
            .flatten()
            .filter(|(_, lang)| from_lang.is_none() || *lang == from_lang)
            .map(|(i, _)| *i)
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
    }

    let norm = normalize_spec(spec);
    if norm.is_empty() {
        return None;
    }

    let segments = split_keep_segments(&norm);

    // 3. Full-path-suffix matching: match the specifier's trailing segments
    //    against whole file-path suffixes, longest first, so a multi-segment
    //    module path resolves the *specific* file (`crewai/agent/core` ->
    //    `.../crewai/agent/core.py`) instead of colliding on the bare `core`
    //    stem. Only a unique same-language match resolves; an ambiguous suffix
    //    is skipped (a shorter suffix or the stem step below may still decide),
    //    never producing a wrong edge.
    let n = segments.len();
    let max = n.min(MAX_PATH_SUFFIX);
    for len in (2..=max).rev() {
        let key = segments[n - len..].join("/");
        let matches: Vec<u32> = idx
            .path_suffix
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|(_, lang)| from_lang.is_none() || *lang == from_lang)
            .map(|(i, _)| *i)
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
    }

    // 4. Segment-wise stem matching: try the full path first, then drop
    //    trailing segments one at a time. A single same-language candidate
    //    wins; ambiguity stays unresolved (deterministic).
    for keep in (1..=segments.len()).rev() {
        let last = &segments[keep - 1];
        let matches: Vec<u32> = idx
            .stem
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

    // 5. Package-directory resolution: the specifier names a package/module
    //    directory whose index file (`__init__.py`, `index.ts`, `init.lua`) is
    //    the imported module. Try the full segment path first, then drop
    //    leading segments (so `import a.b.c` matches `.../a/b/c/__init__.py`
    //    and a bare `from pkg import X` matches `.../pkg/__init__.py`).
    for start in 0..segments.len() {
        let key = segments[start..].join("/");
        let matches: Vec<u32> = idx
            .package
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|(_, lang)| from_lang.is_none() || *lang == from_lang)
            .map(|(i, _)| *i)
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
        if matches.len() > 1 {
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
    if !(unquoted.starts_with("./") || unquoted.starts_with("../")) {
        return None;
    }
    let dir = std::path::Path::new(from_file).parent()?;
    Some(
        normalize_path(&dir.join(unquoted))
            .to_string_lossy()
            .into_owned(),
    )
}

/// Collapse `.` and `..` components without consulting the filesystem.
fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;

    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
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

    /// Parse + extract every fixture file under `resolve_fixtures/<rel>`
    /// (recursive, sorted). The fixtures live *outside* the crate's `tests/`
    /// directory on purpose: their absolute paths must not match
    /// [`crate::db::path_is_test`], or the repo-wide call fallback would treat
    /// every fixture definition as test code and refuse to resolve through it.
    pub fn load_project(rel: &str) -> (Vec<Entity>, Vec<Symbol>, Vec<String>) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resolve_fixtures")
            .join(rel);
        let mut paths = Vec::new();
        collect_paths(&root, &mut paths);
        paths.sort();
        let files: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut entities = Vec::new();
        let mut symbols = Vec::new();
        for (file_id, path) in paths.iter().enumerate() {
            // Non-source files (e.g. a `go.mod` that drives Go package
            // resolution) still get a `files` slot — mirroring production, where
            // the build walks every file — but carry no entities. Only
            // parseable source files are extracted.
            let Some(parsed) = parse_file(path).expect("parse_file ok") else {
                continue;
            };
            let result = extract::extract(&parsed, file_id as u32);
            entities.extend(result.entities);
            symbols.extend(result.symbols);
        }
        (entities, symbols, files)
    }

    fn collect_paths(root: &std::path::Path, paths: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(root)
            .expect("resolve fixture dir exists")
            .filter_map(|entry| entry.ok())
        {
            let path = entry.path();
            if path.is_dir() {
                collect_paths(&path, paths);
            } else if path.is_file() {
                paths.push(path);
            }
        }
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

    #[test]
    fn relative_imports_resolve_before_duplicate_global_stems() {
        let (entities, symbols, files) = load_project("typescript/relative_imports");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let sibling_importer = file_id(&graph, "/sibling/importer.ts");
        let sibling_target = file_id(&graph, "/sibling/x.ts");
        let parent_importer = file_id(&graph, "/parent/child/importer.ts");
        let parent_target = file_id(&graph, "/parent/x.ts");

        let import_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Import)
            .collect();
        assert_eq!(import_edges.len(), 2);
        assert!(import_edges.iter().any(|edge| {
            edge.from == sibling_importer
                && edge.to == EdgeTarget::File(sibling_target)
                && edge.resolved
        }));
        assert!(import_edges.iter().any(|edge| {
            edge.from == parent_importer
                && edge.to == EdgeTarget::File(parent_target)
                && edge.resolved
        }));
    }

    /// S6: a package re-export. `app.py` does `from pkg import Thing`; `pkg` is
    /// a directory whose `__init__.py` re-exports `Thing` from the `core`
    /// submodule. The bare package import must resolve to `pkg/__init__.py`
    /// (package-directory resolution), and the `__init__.py`'s `from .core
    /// import Thing` must resolve to `pkg/core.py` — together they connect
    /// `app.py` to `core.py` transitively, which is what the dependents graph
    /// walks. Before the fix `from pkg import Thing` stem-matched nothing
    /// (`pkg`'s only file stem is `__init__`), so every consumer was dangling.
    #[test]
    fn python_package_reexport_resolves_through_init() {
        let (entities, symbols, files) = load_project("python/package_reexport");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let app = file_id(&graph, "/app.py");
        let init = file_id(&graph, "/pkg/__init__.py");
        let core = file_id(&graph, "/pkg/core.py");

        let resolved: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import && e.resolved)
            .collect();
        assert!(
            resolved
                .iter()
                .any(|e| e.from == app && e.to == EdgeTarget::File(init)),
            "`from pkg import Thing` must resolve to pkg/__init__.py: {resolved:?}"
        );
        assert!(
            resolved
                .iter()
                .any(|e| e.from == init && e.to == EdgeTarget::File(core)),
            "`from .core import Thing` must resolve to pkg/core.py: {resolved:?}"
        );
    }

    /// S6: Elixir `alias MyApp.Repo` in `user.ex` resolves to `repo.ex` (which
    /// declares `defmodule MyApp.Repo`) via the module-declaration index — the
    /// PascalCase module path never stem-matches the snake_case file name.
    #[test]
    fn elixir_alias_resolves_via_module_name() {
        let (entities, symbols, files) = load_project("elixir/aliases");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let user = file_id(&graph, "/user.ex");
        let repo = file_id(&graph, "/repo.ex");

        let resolved: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import && e.resolved)
            .collect();
        assert!(
            resolved
                .iter()
                .any(|e| e.from == user && e.to == EdgeTarget::File(repo)),
            "`alias MyApp.Repo` must resolve to repo.ex: {resolved:?}"
        );
    }

    /// S6: a Go import names a *package* (a directory of `.go` files) via
    /// go.mod's module path. `cmd/main.go` imports the module-root package
    /// (`example.com/app` -> `app.go`) and a subpackage (`example.com/app/sub`
    /// -> `sub/helper.go`); both resolve. The stdlib `fmt` import names no
    /// module prefix, so it stays unresolved (no invented edge).
    #[test]
    fn go_module_and_subpackage_imports_resolve() {
        let (entities, symbols, files) = load_project("go/package_import");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let main = file_id(&graph, "/cmd/main.go");
        let root = file_id(&graph, "/package_import/app.go");
        let helper = file_id(&graph, "/sub/helper.go");

        let resolved: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import && e.resolved)
            .collect();
        assert!(
            resolved
                .iter()
                .any(|e| e.from == main && e.to == EdgeTarget::File(root)),
            "root-package import must resolve cmd/main.go -> app.go: {resolved:?}"
        );
        assert!(
            resolved
                .iter()
                .any(|e| e.from == main && e.to == EdgeTarget::File(helper)),
            "subpackage import must resolve cmd/main.go -> sub/helper.go: {resolved:?}"
        );
        // `fmt` is stdlib: it names no internal module prefix, so it must not
        // resolve to any file.
        let unresolved_from_main = graph
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Import && e.from == main && !e.resolved);
        assert!(
            unresolved_from_main,
            "the stdlib `fmt` import must stay unresolved: {:?}",
            graph.edges
        );
    }

    /// S6: Lua `require "foo.bar"` (dotted module path) resolves to
    /// `foo/bar.lua`, and `require "util"` (a directory package) resolves to
    /// `util/init.lua`.
    #[test]
    fn lua_dotted_and_package_requires_resolve() {
        let (entities, symbols, files) = load_project("lua/nested_require");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");
        let main = file_id(&graph, "/main.lua");
        let bar = file_id(&graph, "/foo/bar.lua");
        let init = file_id(&graph, "/util/init.lua");

        let resolved: Vec<&ResolvedEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import && e.resolved)
            .collect();
        assert!(
            resolved
                .iter()
                .any(|e| e.from == main && e.to == EdgeTarget::File(bar)),
            "`require \"foo.bar\"` must resolve to foo/bar.lua: {resolved:?}"
        );
        assert!(
            resolved
                .iter()
                .any(|e| e.from == main && e.to == EdgeTarget::File(init)),
            "`require \"util\"` must resolve to util/init.lua: {resolved:?}"
        );
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

/// Languages whose imports name a namespace/package rather than a file: a
/// `using`/`import`/`package` reference never resolves to a file-import edge,
/// and a call to a type or method in the same namespace needs no import at
/// all — so the cross-file [`cross_file_call_target`] pass structurally can't
/// fire for them and a definition in a sibling file is unreachable by name.
/// These fall back to the repo-wide single-definition index (Pass 3).
fn resolves_imports_by_namespace(lang: ast_grep_language::SupportLang) -> bool {
    use ast_grep_language::SupportLang::*;
    matches!(lang, CSharp | Java | Kotlin | Scala)
}

/// Languages that need the repo-wide single-definition call fallback (Pass 3
/// in [`resolve_calls`]): either their imports name a namespace rather than a
/// file ([`resolves_imports_by_namespace`]), or they have no cross-file import
/// statements at all because same-module symbols are implicitly visible
/// (Swift — every file in a module sees every other without an `import`). In
/// both cases the path-based cross-file import pass can't produce call edges,
/// so resolution falls back to the unique repo-wide definition of the callee
/// name (ambiguous names stay unresolved — no false edge). Audit F9: without
/// this, Swift produced zero resolved cross-file edges, so nav_map's fan-in
/// `symbols` leaderboard and `foundational_files` were empty.
fn resolves_calls_by_repo_wide_fallback(lang: ast_grep_language::SupportLang) -> bool {
    use ast_grep_language::SupportLang::*;
    resolves_imports_by_namespace(lang) || matches!(lang, Swift)
}

/// Repo-wide "single definition" index for the languages in
/// [`resolves_imports_by_namespace`]. Maps a normalized callee key to the sole
/// callable entity of that name across the whole repo, or `None` when the name
/// is defined more than once — an ambiguous name is left unresolved, exactly
/// like the cross-file pass's single-candidate rule, so this never invents a
/// false edge for a common method name (`ToString`, `Get`, overloads, ...).
///
/// Constructors (a method whose name equals its `owner_type`) are skipped so
/// they don't collide with their own class's entry: `new Foo()` normalizes to
/// `Foo`, which must resolve to the `Foo` *class* entity, not be knocked out
/// as ambiguous by the same-named constructor method (Finding #2).
/// Object-protocol method names whose real definition lives on a framework
/// base class outside the repo (`System.Object`, `java.lang.Object`, ...), so
/// an in-repo override must never win the repo-wide single-definition fallback
/// (see [`build_repo_wide_unique_index`]). Covers the C# (PascalCase) and
/// Java/Kotlin/Scala (camelCase) spellings.
fn is_ubiquitous_object_method(name: &str) -> bool {
    matches!(
        name,
        // C# / .NET (System.Object + IDisposable/IComparable/ICloneable)
        "ToString"
            | "Equals"
            | "GetHashCode"
            | "GetType"
            | "Clone"
            | "CompareTo"
            | "Dispose"
            | "DisposeAsync"
            | "Finalize"
            | "MemberwiseClone"
            // Java / Kotlin / Scala (java.lang.Object + Comparable/AutoCloseable)
            | "toString"
            | "equals"
            | "hashCode"
            | "clone"
            | "compareTo"
            | "finalize"
            | "close"
            // Swift protocol requirements whose canonical definition is the
            // compiler-synthesized/stdlib one: `hash(into:)` (Hashable),
            // `encode(to:)` (Encodable), and the Equatable/Comparable operators.
            // A single in-repo override otherwise captured every matching call
            // (e.g. `widget.hash(into:)` resolving to an unrelated `Report.hash`).
            | "hash"
            | "encode"
            | "=="
            | "!="
            | "<"
            | "<="
            | ">"
            | ">="
    )
}

fn build_repo_wide_unique_index<'a>(
    entities: &'a [Entity],
    test_file: &[bool],
) -> HashMap<&'a str, Option<u32>> {
    let mut index: HashMap<&str, Option<u32>> = HashMap::new();
    for (i, e) in entities.iter().enumerate() {
        // Definitions living in test/fixture files never participate in the
        // repo-wide uniqueness index. Without this, a name defined *only* in a
        // test double (e.g. a test-double `next` or a mock repository method)
        // is "unique across the repo" and the fallback binds every production
        // caller of that name to the test definition — one observed case had a
        // single test `next` capturing 82 production call sites. Excluding test
        // defs is strictly a precision gain: a name defined once in production
        // and once in a test flips from ambiguous (unresolved) to the correct
        // production definition, and a test-only name is left unresolved rather
        // than injecting a production→test false edge. `test_file` is the
        // per-file-id `db::path_is_test` mirror (resolution runs pre-persist).
        if test_file.get(e.file_id as usize).copied().unwrap_or(false) {
            continue;
        }
        let callable = matches!(
            e.kind,
            crate::model::EntityKind::Function
                | crate::model::EntityKind::Class
                | crate::model::EntityKind::Interface
        );
        if !callable {
            continue;
        }
        // Skip constructors: they duplicate the class name and would otherwise
        // make every constructed type ambiguous with its own `new` target.
        if e.kind == crate::model::EntityKind::Function
            && e.owner_type.as_deref() == Some(e.name.as_str())
        {
            continue;
        }
        // Skip object-protocol methods (`ToString`/`Equals`/`GetHashCode`,
        // `toString`/`equals`/`hashCode`, ...). Their canonical definition lives
        // on the framework base class (`System.Object`, `java.lang.Object`),
        // not in the repo, so the "defined exactly once" test the uniqueness
        // index relies on is systematically wrong for them: a single in-repo
        // override captures every `.ToString()` call in the codebase. On
        // eShopOnWeb this resolved every `ToString()` to `ErrorDetails.ToString`,
        // injecting a false call edge into flows and the call graph. Leave them
        // unresolved (an override is still reachable via type-directed Pass 4
        // when the receiver type is known).
        if is_ubiquitous_object_method(&e.name) {
            continue;
        }
        index
            .entry(callee_key(&e.name))
            // A second definition of the same name makes it ambiguous.
            .and_modify(|slot| *slot = None)
            .or_insert(Some(i as u32));
    }
    index
}

/// Type-directed call-resolution context for namespace-import languages
/// (Pass 4). Recovers the static type of a call's receiver from the `TypeRef`
/// entities the extractor emits for typed fields/params/locals, then looks the
/// method up among that type's members — including members declared on a
/// concrete class reached through an `implements`/`extends` edge, so the DI
/// idiom `_svc.Do()` where `_svc : IFoo` resolves to `Foo.Do`. Resolves only
/// when the receiver type yields exactly one matching method, so an ambiguous
/// hierarchy stays unresolved rather than guessing.
struct TypeResolveCtx<'a> {
    /// (file id, variable name) -> declared type name.
    var_type: HashMap<(u32, &'a str), &'a str>,
    /// type name -> concrete subtypes (classes that `implements`/`extends` it).
    subtypes_of: HashMap<&'a str, Vec<&'a str>>,
    /// (owning type name, normalized method key) -> method entity indices.
    method_index: HashMap<(&'a str, &'a str), Vec<u32>>,
    /// All Class/Interface names, for a `Type.StaticMethod()` receiver.
    type_names: std::collections::HashSet<&'a str>,
}

impl<'a> TypeResolveCtx<'a> {
    fn build(entities: &'a [Entity]) -> Self {
        use crate::model::EntityKind::*;
        let mut var_type = HashMap::new();
        let mut subtypes_of: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut type_names = std::collections::HashSet::new();
        let mut interface_names = std::collections::HashSet::new();
        for e in entities {
            match e.kind {
                TypeRef => {
                    if let Some(var) = e.enclosing_function.as_deref() {
                        var_type.insert((e.file_id, var), e.name.as_str());
                    }
                }
                Extends | Implements => {
                    if let Some(sub) = e.enclosing_function.as_deref() {
                        subtypes_of.entry(e.name.as_str()).or_default().push(sub);
                    }
                }
                Class => {
                    type_names.insert(e.name.as_str());
                }
                Interface => {
                    type_names.insert(e.name.as_str());
                    interface_names.insert(e.name.as_str());
                }
                _ => {}
            }
        }
        // Method index is a second pass: a method whose owner is an interface
        // is an abstract *declaration*, not a call target — the implementing
        // class's override is. Indexing both would make every interface-typed
        // receiver with a single implementer look ambiguous. Skip them so the
        // implementation wins.
        let mut method_index: HashMap<(&str, &str), Vec<u32>> = HashMap::new();
        for (i, e) in entities.iter().enumerate() {
            if e.kind == Function
                && let Some(owner) = e.owner_type.as_deref()
                && !interface_names.contains(owner)
            {
                method_index
                    .entry((owner, callee_key(&e.name)))
                    .or_default()
                    .push(i as u32);
            }
        }
        TypeResolveCtx {
            var_type,
            subtypes_of,
            method_index,
            type_names,
        }
    }

    /// Resolve a `receiver.method` call from file `from` to a single method
    /// entity, or `None` if the receiver type is unknown or the match is
    /// absent/ambiguous.
    fn resolve(&self, from: u32, call_name: &str) -> Option<u32> {
        let recv = call_receiver_var(call_name)?;
        let method_key = callee_key(call_name);

        // Candidate owning types: the receiver's declared type and its
        // concrete subtypes; or, for a `Type.StaticMethod()` form, the
        // receiver treated as a type name directly.
        let mut owners: Vec<&str> = Vec::new();
        if let Some(&ty) = self.var_type.get(&(from, recv)) {
            owners.push(ty);
            if let Some(subs) = self.subtypes_of.get(ty) {
                owners.extend(subs.iter().copied());
            }
        }
        if self.type_names.contains(recv) {
            owners.push(recv);
            if let Some(subs) = self.subtypes_of.get(recv) {
                owners.extend(subs.iter().copied());
            }
        }
        if owners.is_empty() {
            return None;
        }

        // Require exactly one distinct target across all candidate owners.
        let mut found: Option<u32> = None;
        for owner in owners {
            if let Some(cands) = self.method_index.get(&(owner, method_key)) {
                for &idx in cands {
                    match found {
                        Some(seen) if seen != idx => return None,
                        None => found = Some(idx),
                        _ => {}
                    }
                }
            }
        }
        found
    }

    /// True when the call is `recv.method` and `recv` is a *variable* whose
    /// declared type is known and is not defined in this repo — positive
    /// evidence the receiver is a library/framework object (an injected ORM
    /// repository, an SDK client, ...). Used to veto the repo-wide name-unique
    /// fallback (Pass 3): a call on a library receiver must not bind to a
    /// coincidentally same-named in-repo definition (`_repo.FindOne()` ->
    /// some unrelated `FindOne`). Since an external receiver's real method
    /// cannot be the in-repo one, vetoing only ever drops a false edge — never
    /// a correct one.
    ///
    /// Deliberately conservative: it fires only when the type is *known and
    /// external*. A receiver with no recorded type (the common case, e.g. an
    /// inherited `obj.getId()` whose variable the extractor didn't type) is
    /// left to the fallback, so those keep resolving. A receiver typed as an
    /// in-repo class/interface is handled by the type-directed pass instead.
    fn receiver_is_known_external(&self, from: u32, call_name: &str) -> bool {
        let Some(recv) = call_receiver_var(call_name) else {
            return false;
        };
        match self.var_type.get(&(from, recv)) {
            Some(&ty) => !self.type_names.contains(ty),
            None => false,
        }
    }
}

/// The receiver variable of a member-access call name: the simple identifier
/// immediately before the final `.method` segment (`this._svc.Do` -> `_svc`,
/// `_svc.Do` -> `_svc`). `None` when the call has no member-access receiver.
fn call_receiver_var(name: &str) -> Option<&str> {
    let t = name.trim();
    let t = t.strip_suffix("()").map(str::trim).unwrap_or(t);
    let dot = t.rfind('.')?;
    let recv = t[..dot].trim_end();
    Some(
        recv.rsplit(['.', ' ', '(', ')'])
            .next()
            .unwrap_or(recv)
            .trim(),
    )
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
    // Per-file-id test/fixture flag (the pure-path `db::path_is_test` mirror,
    // since resolution runs pre-persist). Drives two guards: test-file defs are
    // kept out of the repo-wide uniqueness index, and a production caller is
    // never allowed to resolve to a test-file definition (see the post-filter
    // below).
    let test_file: Vec<bool> = files.iter().map(|p| crate::db::path_is_test(p)).collect();
    let repo_wide = build_repo_wide_unique_index(entities, &test_file);
    let type_ctx = TypeResolveCtx::build(entities);
    // Receiver-aware module-binding resolution (Python `import x` /
    // `from pkg import x`): per file, the local module-binding names the
    // extractor stashed on `Import.owner_type`; and a stem→files index so a
    // call `x.method()` resolves to the sibling module `x` that defines it.
    let module_bindings = build_module_bindings(entities);
    let stem_files = build_stem_to_files(files);

    // Files whose language needs the repo-wide single-definition fallback
    // because the path-based cross-file import pass can't fire: imports name a
    // namespace, not a file (C#/Java/Kotlin/Scala), or there are no cross-file
    // import statements at all because same-module symbols are implicitly
    // visible (Swift). Both fall back to the repo-wide single-definition index
    // (Pass 3 below). Precomputed per file id so the per-call closure is a
    // cheap lookup, not a path re-parse.
    let namespace_import_file: Vec<bool> = files
        .iter()
        .map(|p| {
            language_for_path(std::path::Path::new(p))
                .is_some_and(resolves_calls_by_repo_wide_fallback)
        })
        .collect();

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

            // Pass 2.5: receiver-aware module-binding call. A qualified call
            // `recv.method()` where `recv` is a module the file imported
            // (`import recv` / `from pkg import recv`) resolves to the sibling
            // module `recv` that defines `method` — disambiguating cases the
            // last-segment cross-file pass drops as ambiguous (`users.create`
            // vs `items.create`). Only fires when Pass 1/2 found nothing.
            let recv = if same.is_none() && cross.is_none() {
                resolve_receiver_module_call(
                    &e.name,
                    from,
                    &module_bindings,
                    &stem_files,
                    &exports,
                )
            } else {
                None
            };

            // Pass 3: repo-wide single-definition fallback, only for files
            // whose imports name namespaces rather than files (C#), where
            // Pass 2 cannot fire. Resolves only when the callee name has
            // exactly one definition in the whole repo — ambiguous names stay
            // unresolved, so no false edge is invented.
            let namespace_import = namespace_import_file
                .get(from as usize)
                .copied()
                .unwrap_or(false);
            let repo = if same.is_none()
                && cross.is_none()
                && recv.is_none()
                && namespace_import
                // A call on a receiver positively typed as a library object must
                // not bind to a coincidentally same-named in-repo definition
                // (`_repo.FindOne()` -> unrelated `FindOne`). Unknown-type
                // receivers still fall through, so inherited `obj.getId()`
                // resolves as before.
                && !type_ctx.receiver_is_known_external(from, &e.name)
            {
                repo_wide.get(key).copied().flatten()
            } else {
                None
            };

            // Pass 4: type-directed resolution. When the name-uniqueness
            // fallback can't decide (an ambiguous method name), use the
            // receiver's declared type to pick the one matching method.
            let typed = if same.is_none()
                && cross.is_none()
                && recv.is_none()
                && repo.is_none()
                && namespace_import
            {
                type_ctx.resolve(from, &e.name)
            } else {
                None
            };

            let target = same.or(cross).or(recv).or(repo).or(typed);
            // A production caller must never resolve to a definition that lives
            // in a test/fixture file: production code depending on test code is
            // almost always a coincidental name/type collision (e.g. the
            // type-directed pass reaching a test-only subtype override of an
            // external base — `TimeProvider`'s only in-repo `GetUtcNow` being a
            // unit-test fake), never a real edge. Caller-aware so a test caller
            // still resolves to test code; only ever drops a false edge.
            let caller_is_test = test_file.get(from as usize).copied().unwrap_or(false);
            let target = target.filter(|&t| {
                caller_is_test
                    || !test_file
                        .get(entities[t as usize].file_id as usize)
                        .copied()
                        .unwrap_or(false)
            });
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

/// Per-file set of local module-binding names — the receiver a
/// `recv.method()` call uses — recorded on `Import.owner_type` by the Python
/// extractor (`import x as y` -> `y`; `from pkg import a, b` -> `a`,`b`).
/// Other languages leave `owner_type` unset on imports, so this map is empty
/// for them and the receiver-aware pass never fires.
fn build_module_bindings(entities: &[Entity]) -> HashMap<u32, std::collections::HashSet<String>> {
    let mut map: HashMap<u32, std::collections::HashSet<String>> = HashMap::new();
    for e in entities {
        if e.kind == crate::model::EntityKind::Import
            && let Some(bindings) = e.owner_type.as_deref()
        {
            let set = map.entry(e.file_id).or_default();
            for name in bindings.split(',') {
                let n = name.trim();
                if !n.is_empty() {
                    set.insert(n.to_string());
                }
            }
        }
    }
    map
}

/// File path stem -> file ids, so a call whose receiver names a module can be
/// mapped to that module's file(s) (`crud` -> `app/crud.py`). A stem shared by
/// more than one file yields multiple candidates; the caller resolves only
/// when exactly one candidate defines the called method.
fn build_stem_to_files(files: &[String]) -> HashMap<String, Vec<u32>> {
    let mut map: HashMap<String, Vec<u32>> = HashMap::new();
    for (i, p) in files.iter().enumerate() {
        if let Some(stem) = std::path::Path::new(p).file_stem().and_then(|s| s.to_str()) {
            map.entry(stem.to_string()).or_default().push(i as u32);
        }
    }
    map
}

/// Resolve a receiver-qualified call `recv.method()` to the module `recv`
/// binds. `recv` must be a module binding of the calling file (guards against
/// resolving arbitrary `obj.method()`); the target is the file whose path stem
/// equals `recv` and that exports `method`. Ambiguity (more than one such
/// file/definition) stays unresolved, so no false edge is invented.
fn resolve_receiver_module_call(
    call_name: &str,
    from: u32,
    module_bindings: &HashMap<u32, std::collections::HashSet<String>>,
    stem_files: &HashMap<String, Vec<u32>>,
    exports: &ExportIndex<'_>,
) -> Option<u32> {
    let trimmed = call_name.trim();
    let trimmed = trimmed.strip_suffix("()").map(str::trim).unwrap_or(trimmed);
    let (recv_path, method) = trimmed.rsplit_once('.')?;
    let recv = recv_path
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(recv_path)
        .trim();
    if recv.is_empty() || method.is_empty() {
        return None;
    }
    if !module_bindings.get(&from).is_some_and(|s| s.contains(recv)) {
        return None;
    }
    let method_key = callee_key(method);
    let mut found: Option<u32> = None;
    for &fid in stem_files.get(recv)? {
        if let Some(indices) = exports.normalized.get(&fid).and_then(|m| m.get(method_key)) {
            for &idx in indices {
                match found {
                    Some(seen) if seen != idx => return None,
                    None => found = Some(idx),
                    _ => {}
                }
            }
        }
    }
    found
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

    /// Pass 2.5 (receiver-aware module binding, Python): a `crud.authenticate()`
    /// call where `crud` is bound by `from app import crud` resolves to the
    /// sibling module `app/crud.py`, and `users.create()`/`items.create()`
    /// (both an ambiguous bare `create`) each resolve to the module their
    /// receiver names — a disambiguation the last-segment cross-file pass
    /// cannot make.
    #[test]
    fn python_receiver_qualified_module_calls_resolve() {
        let (entities, symbols, files) = load_project("python/module_calls");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let entity_at = |target: &EdgeTarget| -> (&str, u32) {
            match target {
                EdgeTarget::Entity(idx) => (entities[*idx as usize].name.as_str(), *idx),
                _ => panic!("expected entity target"),
            }
        };
        let call_target = |call_name: &str| -> String {
            let e = entities
                .iter()
                .position(|e| e.kind == crate::model::EntityKind::Call && e.name == call_name)
                .unwrap_or_else(|| panic!("call {call_name} exists")) as u32;
            let edge = graph
                .edges
                .iter()
                .find(|ed| ed.kind == EdgeKind::Call && ed.from_entity == Some(e))
                .unwrap_or_else(|| panic!("edge for {call_name} exists"));
            assert!(edge.resolved, "{call_name} must resolve: {edge:?}");
            let (name, idx) = entity_at(&edge.to);
            let file = &files[entities[idx as usize].file_id as usize];
            format!("{name}@{file}")
        };

        assert!(
            call_target("crud.authenticate").ends_with("app/crud.py"),
            "crud.authenticate -> app/crud.py"
        );
        assert!(call_target("crud.get_user").ends_with("app/crud.py"));
        assert!(
            call_target("users.create").ends_with("svc/users.py"),
            "users.create -> svc/users.py"
        );
        assert!(
            call_target("items.create").ends_with("svc/items.py"),
            "items.create -> svc/items.py (receiver disambiguates the ambiguous `create`)"
        );
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

    /// Pass 3 (C# repo-wide fallback): `using` directives name namespaces, not
    /// files, so a call to a method defined in a sibling file has no import
    /// edge to resolve through. When the method name is defined exactly once
    /// across the repo it must resolve to that single definition; when it is
    /// defined more than once it must stay unresolved (no invented edge).
    #[test]
    fn csharp_call_resolves_via_repo_wide_unique_definition() {
        let (entities, symbols, files) = load_project("csharp/calls");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let do_unique = entities
            .iter()
            .position(|e| e.kind == crate::model::EntityKind::Function && e.name == "DoUnique")
            .expect("DoUnique defined") as u32;

        let call_edge = |callee: &str| -> ResolvedEdge {
            graph
                .edges
                .iter()
                .find(|e| {
                    e.kind == EdgeKind::Call
                        && e.from_entity
                            .is_some_and(|fi| entities[fi as usize].name == callee)
                })
                .cloned()
                .unwrap_or_else(|| panic!("call site `{callee}` present"))
        };

        // Unique across the repo → resolves to the sole definition.
        let unique = call_edge("_svc.DoUnique");
        assert!(unique.resolved, "unique cross-file C# call must resolve");
        assert_eq!(unique.to, EdgeTarget::Entity(do_unique));

        // Defined in both Service.cs and Other.cs, so the repo-wide fallback
        // (Pass 3) can't decide — but `_svc` is a `WidgetService`, so the
        // type-directed pass (Pass 4) resolves it to `WidgetService.DoAmbiguous`,
        // not `OtherService`'s.
        let widget_do_ambiguous = entities
            .iter()
            .position(|e| {
                e.kind == crate::model::EntityKind::Function
                    && e.name == "DoAmbiguous"
                    && e.owner_type.as_deref() == Some("WidgetService")
            })
            .expect("WidgetService.DoAmbiguous") as u32;
        let ambiguous = call_edge("_svc.DoAmbiguous");
        assert!(
            ambiguous.resolved,
            "receiver type must disambiguate the call: {ambiguous:?}"
        );
        assert_eq!(ambiguous.to, EdgeTarget::Entity(widget_do_ambiguous));

        // `new WidgetService()` must resolve to the WidgetService *class*
        // entity, even though the class declares an explicit constructor of
        // the same name (Finding #2: constructors are skipped in the repo-wide
        // index so they don't make the type ambiguous with its own `new`).
        let widget_class = entities
            .iter()
            .position(|e| e.kind == crate::model::EntityKind::Class && e.name == "WidgetService")
            .expect("WidgetService class") as u32;
        let ctor_call = call_edge("WidgetService");
        assert!(
            ctor_call.resolved,
            "constructor call must resolve to the class: {ctor_call:?}"
        );
        assert_eq!(ctor_call.to, EdgeTarget::Entity(widget_class));
    }

    /// Object-protocol methods (`ToString`, `Equals`, ...) whose real
    /// definition lives on `System.Object` must never be captured by the
    /// repo-wide single-definition fallback: a lone in-repo override
    /// (`Report.ToString`) previously swallowed every `.ToString()` call in the
    /// codebase (`item.ToString()` where `item` is a plain `object`), a false
    /// edge that polluted flows and the call graph.
    #[test]
    fn csharp_object_protocol_method_is_not_captured_by_repo_wide_fallback() {
        let (entities, symbols, files) = load_project("csharp/object_methods");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let report_tostring = entities
            .iter()
            .position(|e| {
                e.kind == crate::model::EntityKind::Function
                    && e.name == "ToString"
                    && e.owner_type.as_deref() == Some("Report")
            })
            .expect("Report.ToString defined") as u32;

        // No resolved Call edge may target the in-repo ToString override.
        let captured = graph.edges.iter().any(|e| {
            e.kind == EdgeKind::Call && e.resolved && e.to == EdgeTarget::Entity(report_tostring)
        });
        assert!(
            !captured,
            "object-protocol override must not capture foreign .ToString() calls: {:?}",
            graph
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Call && e.to == EdgeTarget::Entity(report_tostring))
                .collect::<Vec<_>>()
        );
    }

    /// A callee defined *only* in a test/fixture file must not be reachable
    /// through the repo-wide single-definition fallback: it is "unique across
    /// the repo" but binding a production caller to a test double is a false
    /// edge (one observed run had a single test `next` capturing 82 production
    /// call sites). The exclusion is by path (`db::path_is_test`) since
    /// resolution runs pre-persist. A method that *is* uniquely defined in
    /// production still resolves — the exclusion must not over-fire.
    #[test]
    fn repo_wide_fallback_excludes_test_file_definitions() {
        let (entities, symbols, files) = load_project("csharp/test_double_exclusion");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let call_edge = |name: &str| -> &ResolvedEdge {
            let call_idx = entities
                .iter()
                .position(|e| e.kind == crate::model::EntityKind::Call && e.name == name)
                .unwrap_or_else(|| panic!("call `{name}` present"))
                as u32;
            graph
                .edges
                .iter()
                .find(|e| e.kind == EdgeKind::Call && e.from_entity == Some(call_idx))
                .unwrap_or_else(|| panic!("call edge for `{name}` present"))
        };

        // Positive control: `Compute` is defined once, in production, so the
        // repo-wide fallback resolves it (the exclusion must not over-fire).
        let compute = entities
            .iter()
            .position(|e| {
                e.kind == crate::model::EntityKind::Function
                    && e.name == "Compute"
                    && e.owner_type.as_deref() == Some("Helper")
            })
            .expect("production Helper.Compute defined") as u32;
        let compute_call = call_edge("Compute");
        assert!(
            compute_call.resolved && compute_call.to == EdgeTarget::Entity(compute),
            "a uniquely-in-production callee must still resolve: {compute_call:?}"
        );

        // The regression: `RunOnce` is defined only in the `.Tests/` double, so
        // the fallback must leave the production call unresolved rather than
        // binding it to the test fake.
        let run_once = call_edge("RunOnce");
        assert!(
            !run_once.resolved,
            "a test-only callee must not capture a production caller: {run_once:?}"
        );
    }

    /// A call on a receiver whose declared type is a *library* type (not
    /// defined in the repo) must not bind to a coincidentally same-named
    /// in-repo method via the repo-wide fallback (`_repo.FindOne()` where
    /// `_repo : IWidgetRepository` is external). The veto is targeted: the
    /// *same* method name called through an *in-repo*-typed receiver
    /// (`_catalog : Catalog`) still resolves via the type-directed pass.
    #[test]
    fn repo_wide_fallback_vetoes_library_receiver_calls() {
        let (entities, symbols, files) = load_project("csharp/library_receiver");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let catalog_findone = entities
            .iter()
            .position(|e| {
                e.kind == crate::model::EntityKind::Function
                    && e.name == "FindOne"
                    && e.owner_type.as_deref() == Some("Catalog")
            })
            .expect("Catalog.FindOne defined") as u32;

        let call_edge_via = |recv: &str| -> &ResolvedEdge {
            let idx = entities
                .iter()
                .position(|e| {
                    e.kind == crate::model::EntityKind::Call
                        && e.name.contains("FindOne")
                        && e.name.contains(recv)
                })
                .unwrap_or_else(|| panic!("call via `{recv}` present"))
                as u32;
            graph
                .edges
                .iter()
                .find(|e| e.kind == EdgeKind::Call && e.from_entity == Some(idx))
                .unwrap_or_else(|| panic!("call edge via `{recv}` present"))
        };

        // Library receiver: the fallback is vetoed, so the call does not bind
        // to the in-repo Catalog.FindOne (it stays unresolved — no false edge).
        let external = call_edge_via("_repo");
        assert!(
            !(external.resolved && external.to == EdgeTarget::Entity(catalog_findone)),
            "library-receiver call must not bind to the coincidental in-repo method: {external:?}"
        );

        // In-repo-typed receiver: the type-directed pass resolves it to the
        // real method — the veto must not suppress this.
        let internal = call_edge_via("_catalog");
        assert!(
            internal.resolved && internal.to == EdgeTarget::Entity(catalog_findone),
            "in-repo-typed receiver must still resolve to the matching method: {internal:?}"
        );
    }

    /// A production caller must not resolve to a definition in a test file,
    /// even when the *type-directed* pass reaches it: here `_dep : ExternalBase`
    /// (an external base) has one in-repo subtype, a unit-test fake, so Pass 4
    /// would bind `_dep.Fetch()` to the test `FakeDep.Fetch`. The production→test
    /// guard drops that edge. (Observed on semantic-kernel: `TimeProvider`'s only
    /// in-repo `GetUtcNow` was a test `FixedUtcTimeProvider`.)
    #[test]
    fn production_caller_does_not_resolve_to_test_definition() {
        let (entities, symbols, files) = load_project("csharp/prod_to_test_type_dispatch");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let fake_fetch = entities
            .iter()
            .position(|e| {
                e.kind == crate::model::EntityKind::Function
                    && e.name == "Fetch"
                    && e.owner_type.as_deref() == Some("FakeDep")
            })
            .expect("test FakeDep.Fetch defined") as u32;

        // No resolved call edge may target the test-file method.
        let captured = graph.edges.iter().any(|e| {
            e.kind == EdgeKind::Call && e.resolved && e.to == EdgeTarget::Entity(fake_fetch)
        });
        assert!(
            !captured,
            "a production call must not resolve to a test-file definition: {:?}",
            graph
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Call && e.to == EdgeTarget::Entity(fake_fetch))
                .collect::<Vec<_>>()
        );
    }

    /// Swift companion to the C# object-protocol test: `hash(into:)` (a
    /// `Hashable` requirement) defined exactly once in the repo must not be
    /// captured by the repo-wide fallback for a call on an unrelated type.
    #[test]
    fn swift_object_protocol_method_is_not_captured_by_repo_wide_fallback() {
        let (entities, symbols, files) = load_project("swift/object_methods");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let report_hash = entities
            .iter()
            .position(|e| {
                e.kind == crate::model::EntityKind::Function
                    && e.name == "hash"
                    && e.owner_type.as_deref() == Some("Report")
            })
            .expect("Report.hash defined") as u32;

        let captured = graph.edges.iter().any(|e| {
            e.kind == EdgeKind::Call && e.resolved && e.to == EdgeTarget::Entity(report_hash)
        });
        assert!(
            !captured,
            "Swift Hashable.hash override must not capture foreign .hash(into:) calls"
        );
    }

    /// Pass 4 (type-directed): an *ambiguous* method name (`Process` defined on
    /// both `AirShipping` and `SeaShipping`) that the repo-wide uniqueness
    /// fallback can't decide is resolved by the receiver's declared type —
    /// `_air : AirShipping` picks `AirShipping.Process`.
    #[test]
    fn csharp_ambiguous_method_resolves_by_concrete_receiver_type() {
        let (entities, symbols, files) = load_project("csharp/typed_dispatch");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let air_process = entities
            .iter()
            .position(|e| {
                e.kind == crate::model::EntityKind::Function
                    && e.name == "Process"
                    && e.owner_type.as_deref() == Some("AirShipping")
            })
            .expect("AirShipping.Process") as u32;

        let call = graph
            .edges
            .iter()
            .find(|e| {
                e.kind == EdgeKind::Call
                    && e.from_entity
                        .is_some_and(|fi| entities[fi as usize].name == "_air.Process")
            })
            .expect("_air.Process call site");
        assert!(
            call.resolved,
            "ambiguous method must resolve via receiver type"
        );
        assert_eq!(
            call.to,
            EdgeTarget::Entity(air_process),
            "must resolve to AirShipping.Process, not SeaShipping.Process"
        );

        // Safety: an interface-typed receiver with *two* implementers is
        // genuinely ambiguous — `_ship.Process()` (`_ship : IShipping`) must
        // stay unresolved rather than guess Air vs Sea.
        let iface_call = graph
            .edges
            .iter()
            .find(|e| {
                e.kind == EdgeKind::Call
                    && e.from_entity
                        .is_some_and(|fi| entities[fi as usize].name == "_ship.Process")
            })
            .expect("_ship.Process call site");
        assert!(
            !iface_call.resolved,
            "multi-implementer interface dispatch must stay unresolved: {iface_call:?}"
        );
    }

    /// Pass 4 (interface DI): a field typed as an interface with a single
    /// implementer resolves to the implementer's method — the interface's own
    /// abstract declaration is not a call target.
    #[test]
    fn csharp_interface_field_resolves_to_single_implementer() {
        let (entities, symbols, files) = load_project("csharp/interface_di");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let impl_method = entities
            .iter()
            .position(|e| {
                e.kind == crate::model::EntityKind::Function
                    && e.name == "ListUsers"
                    && e.owner_type.as_deref() == Some("UserService")
            })
            .expect("UserService.ListUsers") as u32;

        let call = graph
            .edges
            .iter()
            .find(|e| {
                e.kind == EdgeKind::Call
                    && e.from_entity
                        .is_some_and(|fi| entities[fi as usize].name == "_svc.ListUsers")
            })
            .expect("_svc.ListUsers call site");
        assert!(
            call.resolved,
            "interface-typed receiver must resolve to impl"
        );
        assert_eq!(call.to, EdgeTarget::Entity(impl_method));
    }

    /// Pass 3 extends to Java (same namespace-not-file import model as C#): a
    /// same-package method call needs no `import`, so it has no import edge to
    /// resolve through and must fall back to the repo-wide unique definition.
    #[test]
    fn java_same_package_call_resolves_via_repo_wide_unique_definition() {
        let (entities, symbols, files) = load_project("java/calls");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let place_order = entities
            .iter()
            .position(|e| e.kind == crate::model::EntityKind::Function && e.name == "placeOrder")
            .expect("placeOrder defined") as u32;

        let resolved_to_place_order = graph.edges.iter().any(|e| {
            e.kind == EdgeKind::Call && e.resolved && e.to == EdgeTarget::Entity(place_order)
        });
        assert!(
            resolved_to_place_order,
            "svc.placeOrder() must resolve cross-file via the repo-wide fallback: {:?}",
            graph
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Call)
                .collect::<Vec<_>>()
        );
    }

    /// Pass 3 extends to Swift (audit F9): same-module files see each other
    /// with no `import`, so a cross-file method call has no import edge to
    /// resolve through and must fall back to the repo-wide unique definition.
    /// Without this, Swift produced zero resolved cross-file edges, leaving
    /// nav_map's fan-in `symbols` leaderboard and `foundational_files` empty.
    #[test]
    fn swift_same_module_call_resolves_via_repo_wide_unique_definition() {
        let (entities, symbols, files) = load_project("swift/calls");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let place_order = entities
            .iter()
            .position(|e| e.kind == crate::model::EntityKind::Function && e.name == "placeOrder")
            .expect("placeOrder defined") as u32;

        let resolved_to_place_order = graph.edges.iter().any(|e| {
            e.kind == EdgeKind::Call && e.resolved && e.to == EdgeTarget::Entity(place_order)
        });
        assert!(
            resolved_to_place_order,
            "svc.placeOrder() must resolve cross-file via the repo-wide fallback: {:?}",
            graph
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Call)
                .collect::<Vec<_>>()
        );
    }

    /// Gate check: the repo-wide fallback is scoped to namespace-import /
    /// implicit-visibility languages. A Rust call to a repo-unique function
    /// that the caller never `use`s must stay unresolved — Rust resolves
    /// cross-file through import edges, and applying the fallback to it would
    /// invent edges the language's own visibility rules forbid.
    #[test]
    fn non_csharp_language_does_not_use_repo_wide_fallback() {
        let (entities, symbols, files) = load_project("rust/unimported_unique");
        let graph = resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let helper_call = graph
            .edges
            .iter()
            .find(|e| {
                e.kind == EdgeKind::Call
                    && e.from_entity
                        .is_some_and(|fi| entities[fi as usize].name == "unique_helper")
            })
            .expect("unique_helper call site present");
        assert!(
            !helper_call.resolved,
            "Rust call must stay unresolved without an import (fallback is C#-only): {helper_call:?}"
        );
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
