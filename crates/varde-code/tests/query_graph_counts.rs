//! SQL-statement-count verification for graph traversals.
//!
//! This file holds exactly ONE test so the process-wide rusqlite trace hook
//! counts only this test's statements (no parallel-test interference). The
//! O(1)-statement contract: `dependencies`/`dependents`/`blast_radius` must
//! issue the same number of SQL statements on a 100-node graph as on a
//! 10-node graph — adjacency is loaded once, traversal is in memory.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use varde_code::model::{Entity, EntityKind, ExtractOutput, FileMeta, Span};
use varde_code::persist;
use varde_code::query;
use varde_code::resolve::{EdgeKind, EdgeTarget, FileNode, ResolvedEdge, ResolvedGraph};

static STMT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Count query statements only. Transaction-control statements (the
/// `BEGIN`/`COMMIT` framing the graph-cache self-heal wraps its read + write
/// in) are fixed O(1) overhead that is orthogonal to the fan-out contract
/// this test guards, so they are not counted.
fn trace_hook(sql: &str) {
    let head = sql.trim_start();
    let is_txn_control = ["BEGIN", "COMMIT", "ROLLBACK", "SAVEPOINT", "RELEASE"]
        .iter()
        .any(|kw| head.len() >= kw.len() && head[..kw.len()].eq_ignore_ascii_case(kw));
    if !is_txn_control {
        STMT_COUNT.fetch_add(1, Ordering::SeqCst);
    }
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

/// Persist a chain graph 0→1→…→(n-1) and return its db path.
fn persist_chain(n: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("varde-qcounts-{}-{}", n, std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    let db = dir.join("index.db");
    let _ = std::fs::remove_file(&db);

    let mut entities = Vec::new();
    let mut nodes = Vec::new();
    let mut files = Vec::new();
    for i in 0..n {
        let file = format!("c{i}.rs");
        entities.push(Entity {
            kind: EntityKind::Function,
            name: format!("fn{i}"),
            file_id: i as u32,
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
        });
        nodes.push(FileNode {
            path: file.clone(),
            community_id: None,
            fan_in: 0,
            fan_out: 0,
        });
        files.push(file);
    }
    let edges: Vec<ResolvedEdge> = (0..n - 1)
        .map(|i| ResolvedEdge {
            from: i as u32,
            to: EdgeTarget::File(i as u32 + 1),
            kind: EdgeKind::Import,
            resolved: true,
            from_entity: None,
        })
        .collect();
    let graph = ResolvedGraph {
        nodes,
        edges,
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

fn count_statements(mode: &str, db: &std::path::Path) -> usize {
    STMT_COUNT.store(0, Ordering::SeqCst);
    varde_code::query::set_trace_hook(Some(trace_hook));
    let input = format!(r#"{{"dbPath":"{}","filePath":"c0.rs"}}"#, db.display());
    let stdout = query::run_mode(mode, &input);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("envelope is JSON");
    assert_eq!(value["ok"], true, "mode {mode} succeeded: {stdout}");
    STMT_COUNT.load(Ordering::SeqCst)
}

#[test]
fn statement_count_is_constant_in_graph_size() {
    varde_code::query::set_trace_hook(Some(trace_hook));

    let db10 = persist_chain(10);
    let db100 = persist_chain(100);

    for mode in ["dependencies", "dependents", "blast_radius"] {
        let n10 = count_statements(mode, &db10);
        let n100 = count_statements(mode, &db100);
        assert!(
            n10 <= 6,
            "{mode} on 10 nodes executed {n10} statements (expected O(1))"
        );
        assert_eq!(
            n10, n100,
            "{mode} statement count must not grow with N: {n10} vs {n100}"
        );
    }

    varde_code::query::set_trace_hook(None);
    let _ = std::fs::remove_dir_all(db10.parent().unwrap());
    let _ = std::fs::remove_dir_all(db100.parent().unwrap());
}
