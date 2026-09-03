//! Module dependency layers: module-to-module edges derived from file-level
//! resolved edges, plus cycle detection over the resulting directed module
//! graph.
//!
//! Design (per `memory-bank/working/plans/2026-09-03-nav-map-draft/plan.md`,
//! "Module dependency layers"):
//! - A "module" is a file's parent directory (the smallest unit that groups
//!   files into a cohesive unit without requiring extra config).
//! - A module-to-module edge A -> B is only kept when at least 3 distinct
//!   files cross that boundary (i.e. at least 3 distinct (from_file, to_file)
//!   pairs with from_file in A and to_file in B). Two crossing files is noise
//!   — a single import doesn't establish an architectural dependency.
//! - This is explicitly NOT a declared/enforced layer order. The only
//!   violation signal this module produces is cycle detection over the
//!   resulting module-edge graph: cycles are considered architecturally
//!   risky regardless of what order modules "should" depend in.
//! - Generated/vendored files are excluded before any of the above, via
//!   [`crate::query::noise_filter::is_generated_or_vendored_path`].
//!
//! This module is a standalone computable unit: it operates on plain
//! `(from_path, to_path)` file-edge pairs, not directly on a DB connection,
//! so it's trivially unit-testable with fixtures and can be wired into a
//! DB-backed caller (a later task) by feeding it `resolved_edges` rows.

use std::collections::{HashMap, HashSet};

use crate::query::noise_filter::is_generated_or_vendored_path;

/// Minimum number of distinct crossing files required for a module-to-module
/// edge to be kept. Two crossing files is treated as noise, not an
/// architectural dependency.
pub const MIN_CROSSING_FILES: usize = 3;

/// A directed module-to-module edge that survived the crossing-file
/// threshold.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModuleEdge {
    pub from_module: String,
    pub to_module: String,
    /// Number of distinct (from_file, to_file) pairs that crossed this
    /// module boundary.
    pub crossing_files: usize,
}

/// Full result of a module-layers computation: the kept edges plus any
/// cycles found in the resulting directed graph.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ModuleLayers {
    pub edges: Vec<ModuleEdge>,
    /// Each inner Vec is one cycle, as a list of module names in graph
    /// traversal order (first element repeats implicitly at the end).
    pub cycles: Vec<Vec<String>>,
}

/// Derive a file's "module" as its parent directory. Files with no parent
/// (bare filenames at repo root) fall into the root module `""`.
fn module_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _file)) => dir.to_string(),
        None => String::new(),
    }
}

/// Compute module-to-module edges (threshold-filtered) and cycles from raw
/// file-level edges `(from_path, to_path)`.
///
/// Generated/vendored paths (on either side) are excluded before module
/// derivation. Self-edges (a module depending on itself) never produce a
/// `ModuleEdge` — they can't participate in a cross-module cycle anyway.
pub fn compute_module_layers<'a, I>(file_edges: I) -> ModuleLayers
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    // module_pair -> distinct (from_file, to_file) pairs seen.
    let mut crossings: HashMap<(String, String), HashSet<(String, String)>> = HashMap::new();

    for (from_path, to_path) in file_edges {
        if is_generated_or_vendored_path(from_path) || is_generated_or_vendored_path(to_path) {
            continue;
        }
        let from_module = module_of(from_path);
        let to_module = module_of(to_path);
        if from_module == to_module {
            continue;
        }
        crossings
            .entry((from_module, to_module))
            .or_default()
            .insert((from_path.to_string(), to_path.to_string()));
    }

    let mut edges: Vec<ModuleEdge> = crossings
        .into_iter()
        .filter_map(|((from_module, to_module), files)| {
            if files.len() >= MIN_CROSSING_FILES {
                Some(ModuleEdge {
                    from_module,
                    to_module,
                    crossing_files: files.len(),
                })
            } else {
                None
            }
        })
        .collect();
    // Deterministic ordering for stable output/tests.
    edges.sort_by(|a, b| (&a.from_module, &a.to_module).cmp(&(&b.from_module, &b.to_module)));

    let cycles = detect_cycles(&edges);

    ModuleLayers { edges, cycles }
}

/// DFS-based cycle detection (find_strongly_connected-style, but simplified
/// to "find one representative simple cycle per SCC with >1 node") over the
/// module-edge graph. Small graphs are expected here (module count, not file
/// count), so a straightforward DFS with a recursion stack is simplest and
/// correct — no need for full Tarjan bookkeeping.
fn detect_cycles(edges: &[ModuleEdge]) -> Vec<Vec<String>> {
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut nodes: Vec<&str> = Vec::new();
    let mut seen_nodes: HashSet<&str> = HashSet::new();
    for edge in edges {
        adjacency
            .entry(edge.from_module.as_str())
            .or_default()
            .push(edge.to_module.as_str());
        for n in [edge.from_module.as_str(), edge.to_module.as_str()] {
            if seen_nodes.insert(n) {
                nodes.push(n);
            }
        }
    }
    // Deterministic traversal order.
    nodes.sort();
    for adj in adjacency.values_mut() {
        adj.sort();
    }

    let mut cycles: Vec<Vec<String>> = Vec::new();
    let mut reported_sets: HashSet<Vec<String>> = HashSet::new();
    let mut visited: HashSet<&str> = HashSet::new();

    for &start in &nodes {
        if visited.contains(start) {
            continue;
        }
        let mut stack: Vec<&str> = Vec::new();
        let mut on_stack: HashSet<&str> = HashSet::new();
        dfs(
            start,
            &adjacency,
            &mut visited,
            &mut stack,
            &mut on_stack,
            &mut cycles,
            &mut reported_sets,
        );
    }

    cycles
}

fn dfs<'a>(
    node: &'a str,
    adjacency: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
    stack: &mut Vec<&'a str>,
    on_stack: &mut HashSet<&'a str>,
    cycles: &mut Vec<Vec<String>>,
    reported_sets: &mut HashSet<Vec<String>>,
) {
    visited.insert(node);
    stack.push(node);
    on_stack.insert(node);

    if let Some(neighbors) = adjacency.get(node) {
        for &next in neighbors {
            if on_stack.contains(next) {
                // Found a cycle: the portion of the stack from `next`'s
                // first occurrence to the top.
                let start_idx = stack.iter().position(|&n| n == next).unwrap();
                let ordered: Vec<String> =
                    stack[start_idx..].iter().map(|s| s.to_string()).collect();
                // Dedup identical cycles reported from different entry
                // points by comparing their sorted node sets.
                let mut key = ordered.clone();
                key.sort();
                if reported_sets.insert(key) {
                    cycles.push(ordered);
                }
            } else if !visited.contains(next) {
                dfs(
                    next,
                    adjacency,
                    visited,
                    stack,
                    on_stack,
                    cycles,
                    reported_sets,
                );
            }
        }
    }

    stack.pop();
    on_stack.remove(node);
}

#[cfg(test)]
mod module_layers_cycles {
    use super::*;

    /// AC1: two modules with only 2 files crossing the boundary produce no
    /// edge in the output.
    #[test]
    fn edge_below_threshold_is_dropped() {
        let file_edges = vec![
            ("mod_a/a1.rs", "mod_b/b1.rs"),
            ("mod_a/a2.rs", "mod_b/b2.rs"),
        ];
        let result = compute_module_layers(file_edges);
        assert!(
            result.edges.is_empty(),
            "expected no edges below the {}-file crossing threshold, got {:?}",
            MIN_CROSSING_FILES,
            result.edges
        );
        assert!(result.cycles.is_empty());
    }

    /// AC1 (positive companion): once a third crossing file appears, the
    /// edge does show up, confirming the threshold boundary itself.
    #[test]
    fn edge_at_threshold_is_kept() {
        let file_edges = vec![
            ("mod_a/a1.rs", "mod_b/b1.rs"),
            ("mod_a/a2.rs", "mod_b/b2.rs"),
            ("mod_a/a3.rs", "mod_b/b3.rs"),
        ];
        let result = compute_module_layers(file_edges);
        assert_eq!(
            result.edges,
            vec![ModuleEdge {
                from_module: "mod_a".to_string(),
                to_module: "mod_b".to_string(),
                crossing_files: 3,
            }]
        );
    }

    /// AC2: modules A -> B -> C -> A, each pair with >=3 crossing files,
    /// must be flagged as a cycle containing exactly {A, B, C}.
    #[test]
    fn three_module_cycle_is_detected() {
        let file_edges = vec![
            ("mod_a/a1.rs", "mod_b/b1.rs"),
            ("mod_a/a2.rs", "mod_b/b2.rs"),
            ("mod_a/a3.rs", "mod_b/b3.rs"),
            ("mod_b/b1.rs", "mod_c/c1.rs"),
            ("mod_b/b2.rs", "mod_c/c2.rs"),
            ("mod_b/b3.rs", "mod_c/c3.rs"),
            ("mod_c/c1.rs", "mod_a/a1.rs"),
            ("mod_c/c2.rs", "mod_a/a2.rs"),
            ("mod_c/c3.rs", "mod_a/a3.rs"),
        ];
        let result = compute_module_layers(file_edges);
        assert_eq!(
            result.edges.len(),
            3,
            "expected all three boundary edges kept: {:?}",
            result.edges
        );

        assert_eq!(
            result.cycles.len(),
            1,
            "expected exactly one cycle: {:?}",
            result.cycles
        );
        let mut found: Vec<String> = result.cycles[0].clone();
        found.sort();
        assert_eq!(
            found,
            vec![
                "mod_a".to_string(),
                "mod_b".to_string(),
                "mod_c".to_string()
            ]
        );
    }

    /// Generated/vendored files must be excluded before module-edge
    /// counting, so a boundary that only "crosses" via vendored files
    /// should not produce an edge even with >=3 crossing files.
    #[test]
    fn vendored_files_are_excluded() {
        let file_edges = vec![
            ("mod_a/node_modules/a1.rs", "mod_b/b1.rs"),
            ("mod_a/node_modules/a2.rs", "mod_b/b2.rs"),
            ("mod_a/node_modules/a3.rs", "mod_b/b3.rs"),
        ];
        let result = compute_module_layers(file_edges);
        assert!(
            result.edges.is_empty(),
            "expected vendored crossings to be filtered out, got {:?}",
            result.edges
        );
    }

    /// A module never produces a self-edge even with many same-module
    /// "edges" (e.g. intra-module calls) — only cross-module boundaries are
    /// candidates for a `ModuleEdge`.
    #[test]
    fn same_module_edges_are_not_boundary_crossings() {
        let file_edges = vec![
            ("mod_a/a1.rs", "mod_a/a2.rs"),
            ("mod_a/a2.rs", "mod_a/a3.rs"),
            ("mod_a/a3.rs", "mod_a/a1.rs"),
        ];
        let result = compute_module_layers(file_edges);
        assert!(result.edges.is_empty());
        assert!(result.cycles.is_empty());
    }
}
