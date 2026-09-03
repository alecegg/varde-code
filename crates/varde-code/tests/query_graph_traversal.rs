//! Integration tests for the graph-traversal query modes: dependencies,
//! dependents, blast_radius, symbol_blast_radius, type_hierarchy, explore.
//!
//! Fixtures are built two ways: real resolved fixture projects
//! (extract → resolve → persist) and synthetic graphs (constructed
//! `ExtractOutput`/`ResolvedGraph` persisted directly) for cycle and
//! scale cases.

use std::path::PathBuf;

use varde_code::model::{Entity, EntityKind, ExtractOutput, FileMeta, Span, Symbol, SymbolKind};
use varde_code::persist;
use varde_code::query;
use varde_code::resolve::{self, EdgeKind, EdgeTarget, FileNode, ResolvedEdge, ResolvedGraph};

static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn temp_db(tag: &str) -> PathBuf {
    // Unique dir per call: parallel tests sharing a tag must not race.
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "varde-qgraph-{}-{}-{}",
        tag,
        std::process::id(),
        seq
    ));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    let db = dir.join("index.db");
    let _ = std::fs::remove_file(&db);
    db
}

fn span() -> Span {
    Span {
        start_byte: 0,
        end_byte: 1,
        start_line: 1,
        start_col: 0,
        end_line: 1,
        end_col: 1,
    }
}

fn fn_entity(file_id: u32, name: &str) -> Entity {
    Entity {
        kind: EntityKind::Function,
        name: name.to_string(),
        file_id,
        span: span(),
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

/// Persist a synthetic graph: files 0..n with edges given as (from, to)
/// pairs (import kind, resolved).
fn persist_synthetic(tag: &str, n: usize, edges: &[(usize, usize)]) -> PathBuf {
    let db = temp_db(tag);
    let mut entities = Vec::new();
    let mut nodes = Vec::new();
    let mut files = Vec::new();
    for i in 0..n {
        let file = format!("f{i}.rs");
        entities.push(fn_entity(i as u32, &format!("fn{i}")));
        nodes.push(FileNode {
            path: file.clone(),
            community_id: None,
            fan_in: 0,
            fan_out: 0,
        });
        files.push(file);
    }
    let resolved: Vec<ResolvedEdge> = edges
        .iter()
        .map(|(from, to)| ResolvedEdge {
            from: *from as u32,
            to: EdgeTarget::File(*to as u32),
            kind: EdgeKind::Import,
            resolved: true,
            from_entity: None,
        })
        .collect();
    let graph = ResolvedGraph {
        nodes,
        edges: resolved,
        communities: vec![],
        clone_bands: vec![],
    };
    let output = ExtractOutput {
        entities,
        symbols: vec![],
        diagnostics: vec![],
        file_meta: vec![
            FileMeta {
                mtime: 0,
                size: 0,
                content_hash: "0000000000000000".to_string()
            };
            files.len()
        ],
        files,
    };
    persist::persist(
        &db,
        std::slice::from_ref(&output),
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .expect("persist succeeds");
    db
}

/// Like [`persist_synthetic`], but also persists one binding symbol per
/// file (`sym{i}`) into the `symbols` table, for `explore`'s symbol-name
/// seed-resolution fallback.
fn persist_synthetic_with_symbols(tag: &str, n: usize, edges: &[(usize, usize)]) -> PathBuf {
    let db = temp_db(tag);
    let mut entities = Vec::new();
    let mut symbols = Vec::new();
    let mut nodes = Vec::new();
    let mut files = Vec::new();
    for i in 0..n {
        let file = format!("f{i}.rs");
        entities.push(fn_entity(i as u32, &format!("fn{i}")));
        symbols.push(Symbol {
            kind: SymbolKind::Binding,
            name: format!("sym{i}"),
            file_id: i as u32,
            span: span(),
        });
        nodes.push(FileNode {
            path: file.clone(),
            community_id: None,
            fan_in: 0,
            fan_out: 0,
        });
        files.push(file);
    }
    let resolved: Vec<ResolvedEdge> = edges
        .iter()
        .map(|(from, to)| ResolvedEdge {
            from: *from as u32,
            to: EdgeTarget::File(*to as u32),
            kind: EdgeKind::Import,
            resolved: true,
            from_entity: None,
        })
        .collect();
    let graph = ResolvedGraph {
        nodes,
        edges: resolved,
        communities: vec![],
        clone_bands: vec![],
    };
    let output = ExtractOutput {
        entities,
        symbols,
        diagnostics: vec![],
        file_meta: vec![
            FileMeta {
                mtime: 0,
                size: 0,
                content_hash: "0000000000000000".to_string()
            };
            files.len()
        ],
        files,
    };
    persist::persist(
        &db,
        std::slice::from_ref(&output),
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .expect("persist succeeds");
    db
}

/// Run a mode against a db path; returns the envelope.
fn envelope(mode: &str, db_path: &std::path::Path, extra: &str) -> serde_json::Value {
    let input = format!(r#"{{"dbPath":"{db}",{extra}}}"#, db = db_path.display());
    let stdout = query::run_mode(mode, &input);
    serde_json::from_str(&stdout).expect("envelope is JSON")
}

fn data(mode: &str, db_path: &std::path::Path, extra: &str) -> serde_json::Value {
    envelope(mode, db_path, extra)["data"].clone()
}

fn paths_array(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .expect("data is an array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

mod cycle_termination {
    use super::*;

    /// a→b→c→a: every traversal must terminate with a finite result.
    #[test]
    fn cyclic_graph_terminates_for_all_traversals() {
        let db = persist_synthetic("cycle", 3, &[(0, 1), (1, 2), (2, 0)]);
        for mode in ["dependencies", "dependents", "blast_radius"] {
            let env = envelope(mode, &db, r#""filePath":"f0.rs""#);
            assert_eq!(env["ok"], true, "{mode} terminated: {env}");
            let arr = env["data"].as_array().expect("array");
            assert!(
                !arr.is_empty(),
                "{mode} on a cycle returns a finite non-empty set"
            );
            // f0 reaches f1 and f2 via the cycle — both must appear, no dupes.
            let files = paths_array(&env["data"]);
            assert_eq!(files.len(), 2, "{mode} result: {files:?}");
        }
    }
}

mod dependencies_mode {
    use super::*;

    fn fan_db() -> PathBuf {
        // a→b, a→c, b→c (resolved imports).
        persist_synthetic("fan", 3, &[(0, 1), (0, 2), (1, 2)])
    }

    #[test]
    fn outgoing_matches_expected_traversal() {
        let db = fan_db();
        let result = data("dependencies", &db, r#""filePath":"f0.rs""#);
        assert_eq!(paths_array(&result), vec!["f1.rs", "f2.rs"]);
    }

    #[test]
    fn incoming_direction_supported() {
        let db = fan_db();
        let result = data(
            "dependencies",
            &db,
            r#""filePath":"f2.rs","direction":"incoming""#,
        );
        assert_eq!(paths_array(&result), vec!["f0.rs", "f1.rs"]);
    }

    #[test]
    fn max_depth_limits_traversal() {
        // Chain f0→f1→f2: depth 1 reaches f1 only, depth 2 reaches f1+f2.
        let db = persist_synthetic("fan-depth", 3, &[(0, 1), (1, 2)]);
        let result = data("dependencies", &db, r#""filePath":"f0.rs","maxDepth":1"#);
        assert_eq!(paths_array(&result), vec!["f1.rs"]);
        let full = data("dependencies", &db, r#""filePath":"f0.rs""#);
        assert_eq!(paths_array(&full), vec!["f1.rs", "f2.rs"]);
    }
}

mod dependents_mode {
    use super::*;

    #[test]
    fn reverse_traversal_matches_expected() {
        let db = persist_synthetic("dep", 3, &[(0, 1), (0, 2), (1, 2)]);
        let result = data("dependents", &db, r#""filePath":"f2.rs""#);
        assert_eq!(paths_array(&result), vec!["f0.rs", "f1.rs"]);
    }

    #[test]
    fn leaf_has_no_dependents() {
        // f1 depends on f0, so nothing depends on f1.
        let db = persist_synthetic("dep-leaf", 2, &[(1, 0)]);
        let env = envelope("dependents", &db, r#""filePath":"f1.rs""#);
        assert_eq!(env["ok"], true);
        assert_eq!(env["data"], serde_json::json!([]));
    }
}

mod blast_radius_mode {
    use super::*;

    #[test]
    fn union_of_both_directions() {
        let db = persist_synthetic("br", 4, &[(0, 1), (1, 2), (3, 1)]);
        // f1 reaches f2; f0 and f3 reach f1 → impact set {f0, f2, f3}.
        let result = data("blast_radius", &db, r#""filePath":"f1.rs""#);
        assert_eq!(paths_array(&result), vec!["f0.rs", "f2.rs", "f3.rs"]);
    }
}

mod symbol_blast_radius_mode {
    use super::*;

    #[test]
    fn declares_impact_of_entitys_file() {
        let db = persist_synthetic("sbr", 3, &[(0, 1), (1, 2)]);
        let result = data("symbol_blast_radius", &db, r#""name":"fn1""#);
        assert_eq!(result["declaring_file"], "f1.rs");
        assert_eq!(paths_array(&result["blast_radius"]), vec!["f0.rs", "f2.rs"]);
    }

    #[test]
    fn unknown_symbol_is_not_found() {
        let db = persist_synthetic("sbr-nf", 2, &[(0, 1)]);
        let env = envelope("symbol_blast_radius", &db, r#""name":"ghost_fn""#);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}

mod type_hierarchy_mode {
    use super::*;

    #[test]
    fn returns_declaration_context_chain() {
        let db = temp_db("th");
        // A class entity inside an enclosing function.
        let mut class = fn_entity(0, "Thing");
        class.kind = EntityKind::Class;
        class.enclosing_function = Some("outer".to_string());
        let output = ExtractOutput {
            entities: vec![fn_entity(0, "outer"), class],
            symbols: vec![],
            diagnostics: vec![],
            files: vec!["a.rs".to_string()],
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                1
            ],
        };
        let graph =
            resolve::resolve(&output.entities, &output.symbols, &output.files).expect("resolve");
        persist::persist(
            &db,
            std::slice::from_ref(&output),
            &graph,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("persist");

        let result = data("type_hierarchy", &db, r#""name":"Thing","filePath":"a.rs""#);
        assert_eq!(result["symbol"]["name"], "Thing");
        assert_eq!(result["symbol"]["kind"], "class");
        assert_eq!(result["symbol"]["enclosing_function"], "outer");
        let chain = result["hierarchy"].as_array().expect("hierarchy array");
        assert_eq!(chain.len(), 2, "entity + enclosing function: {result}");
    }

    #[test]
    fn unknown_type_is_not_found() {
        let db = persist_synthetic("th-nf", 2, &[(0, 1)]);
        let env = envelope("type_hierarchy", &db, r#""name":"Ghost""#);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }

    /// Regression (review fix H4): two same-file entities that mutually
    /// enclose each other by name (`A.enclosing_function == "B"`,
    /// `B.enclosing_function == "A"`) must not loop forever — the
    /// `name + file ORDER BY id LIMIT 1` parent lookup has no other exit
    /// besides an empty/absent `enclosing_function`, so the walk needs its
    /// own cycle guard. Without it this test hangs rather than fails.
    #[test]
    fn cyclic_enclosing_function_terminates_instead_of_looping_forever() {
        let db = temp_db("th-cycle");
        let mut a = fn_entity(0, "A");
        a.kind = EntityKind::Class;
        a.enclosing_function = Some("B".to_string());
        let mut b = fn_entity(0, "B");
        b.kind = EntityKind::Class;
        b.enclosing_function = Some("A".to_string());
        let output = ExtractOutput {
            entities: vec![a, b],
            symbols: vec![],
            diagnostics: vec![],
            files: vec!["a.rs".to_string()],
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                1
            ],
        };
        let graph =
            resolve::resolve(&output.entities, &output.symbols, &output.files).expect("resolve");
        persist::persist(
            &db,
            std::slice::from_ref(&output),
            &graph,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("persist");

        let result = data("type_hierarchy", &db, r#""name":"A","filePath":"a.rs""#);
        let chain = result["hierarchy"].as_array().expect("hierarchy array");
        assert_eq!(
            chain.len(),
            2,
            "cycle must terminate once both A and B are seen: {result}"
        );
    }
}

mod explore_mode {
    use super::*;

    #[test]
    fn explores_neighborhood_from_seed() {
        let db = persist_synthetic("explore", 3, &[(0, 1), (0, 2)]);
        let result = data(
            "explore",
            &db,
            r#""query":{"kind":"dependency","params":{"input":"f0.rs"}}"#,
        );
        assert_eq!(result["input"], "f0.rs");
        assert_eq!(paths_array(&result["reachable"]), vec!["f1.rs", "f2.rs"]);
    }

    #[test]
    fn max_items_caps_results() {
        let db = persist_synthetic("explore-cap", 4, &[(0, 1), (0, 2), (0, 3)]);
        let result = data(
            "explore",
            &db,
            r#""query":{"kind":"dependency","params":{"input":"f0.rs","maxItems":1}}"#,
        );
        assert_eq!(paths_array(&result["reachable"]).len(), 1);
    }

    #[test]
    fn falls_back_to_symbol_name_when_seed_is_not_a_file() {
        let db = persist_synthetic_with_symbols("explore-symbol", 3, &[(0, 1), (0, 2)]);
        let result = data("explore", &db, r#""query":{"params":{"input":"sym0"}}"#);
        assert_eq!(result["resolvedVia"], "symbol_exact");
        assert_eq!(paths_array(&result["seedFiles"]), vec!["f0.rs"]);
        assert_eq!(paths_array(&result["reachable"]), vec!["f1.rs", "f2.rs"]);
    }

    #[test]
    fn falls_back_to_partial_symbol_match() {
        let db = persist_synthetic_with_symbols("explore-symbol-partial", 3, &[(0, 1), (0, 2)]);
        let result = data("explore", &db, r#""query":{"params":{"input":"YM0"}}"#);
        assert_eq!(result["resolvedVia"], "symbol_partial");
        assert_eq!(paths_array(&result["seedFiles"]), vec!["f0.rs"]);
    }

    #[test]
    fn incoming_direction_walks_reverse_edges() {
        let db = persist_synthetic("explore-incoming", 3, &[(0, 1), (0, 2)]);
        let result = data(
            "explore",
            &db,
            r#""query":{"params":{"input":"f1.rs","direction":"incoming"}}"#,
        );
        assert_eq!(paths_array(&result["reachable"]), vec!["f0.rs"]);
    }

    #[test]
    fn both_direction_unions_forward_and_reverse() {
        // f0 -> f1 -> f2: from f1, "both" reaches f0 (incoming) and f2 (outgoing).
        let db = persist_synthetic("explore-both", 3, &[(0, 1), (1, 2)]);
        let result = data(
            "explore",
            &db,
            r#""query":{"params":{"input":"f1.rs","direction":"both"}}"#,
        );
        assert_eq!(paths_array(&result["reachable"]), vec!["f0.rs", "f2.rs"]);
    }

    #[test]
    fn unresolvable_seed_is_not_found() {
        let db = persist_synthetic("explore-miss", 2, &[(0, 1)]);
        let env = envelope(
            "explore",
            &db,
            r#""query":{"params":{"input":"nonexistent"}}"#,
        );
        assert_eq!(env["ok"], false, "{env}");
    }
}
