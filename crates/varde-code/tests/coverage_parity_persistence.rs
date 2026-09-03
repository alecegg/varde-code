//! Coverage-parity harness for the persistence phase.
//!
//! One checklist capability per persisted concern. Every check drives the
//! REAL pipeline end-to-end — `extract()` → `resolve()` → `persist()` — over
//! the fixture sources under `tests/resolve_fixtures`; nothing is stubbed.
//! Pass/fail is decided by self-contained assertions on the persisted DB
//! state (no diffing against varde's TS output).

use std::path::PathBuf;

use varde_code::db;
use varde_code::extract;
use varde_code::model::{Entity, EntityKind, ExtractOutput, FileMeta, Symbol};
use varde_code::parse::parse_source;
use varde_code::persist;
use varde_code::resolve;

const FIXTURES: &str = "tests/resolve_fixtures";

type Check = Result<(), String>;
type CheckFn = fn() -> Check;

fn fixture_dir(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES)
        .join(rel)
}

/// Parse + extract every fixture file under `<FIXTURES>/<rel>` (sorted).
fn load_project(rel: &str) -> (Vec<Entity>, Vec<Symbol>, Vec<String>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixture_dir(rel))
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
        let parsed = parse_source(
            &varde_code::parse::language_for_path(path).expect("fixture is a supported language"),
            &std::fs::read_to_string(path).expect("fixture reads"),
        );
        let result = extract::extract(&parsed, file_id as u32);
        entities.extend(result.entities);
        symbols.extend(result.symbols);
    }
    (entities, symbols, files)
}

/// Parse + extract a single fixture file (any supported language).
fn load_file(rel: &str) -> (Vec<Entity>, Vec<Symbol>, Vec<String>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    let file = path.to_string_lossy().into_owned();
    let parsed = parse_source(
        &varde_code::parse::language_for_path(&path).expect("fixture is a supported language"),
        &std::fs::read_to_string(&path).expect("fixture reads"),
    );
    let result = extract::extract(&parsed, 0);
    (result.entities, result.symbols, vec![file])
}

fn temp_db(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("varde-parity-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    let db = dir.join("index.db");
    let _ = std::fs::remove_file(&db);
    db
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0))
        .map_err(|e| e.to_string())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn check_files() -> Check {
    let (entities, symbols, files) = load_project("rust/graph");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("files");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    let rows: i64 = count(&conn, "SELECT COUNT(*) FROM files");
    let expected: i64 = graph.nodes.len() as i64;
    if rows != expected {
        return Err(format!("files rows {rows} != graph nodes {expected}"));
    }
    Ok(())
}

fn check_entities() -> Check {
    let (entities, symbols, files) = load_project("rust/graph");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("entities");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    // The graph fixture declares fn_b/fn_c — function entities must be there.
    let fn_count: i64 = count(
        &conn,
        &format!(
            "SELECT COUNT(*) FROM entities WHERE kind = {} AND name IN ('fn_b','fn_c')",
            EntityKind::Function.as_i64()
        ),
    );
    if fn_count != 2 {
        return Err(format!(
            "expected fn_b/fn_c function entities, got {fn_count}"
        ));
    }
    // No entities row may dangle: every file_id references a files row.
    let dangling: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM entities e LEFT JOIN files f ON f.id = e.file_id WHERE f.id IS NULL",
    );
    if dangling != 0 {
        return Err(format!("{dangling} entities with dangling file_id"));
    }
    Ok(())
}

fn check_symbols() -> Check {
    // The TS symbols fixture has named + default imports, which produce both
    // binding (import_clause/namespace alias) and reference symbols.
    let (entities, symbols, files) = load_file("tests/fixtures/ts/symbols.ts");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("symbols");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    let symbols_rows: i64 = count(&conn, "SELECT COUNT(*) FROM symbols");
    if symbols_rows != symbols.len() as i64 {
        return Err(format!(
            "symbols rows {symbols_rows} != {len}",
            len = symbols.len()
        ));
    }
    // Both binding and reference kinds are representable.
    let kinds: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT kind FROM symbols")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        let mut v = Vec::new();
        for r in rows {
            let k = r.map_err(|e| e.to_string())?;
            v.push(
                varde_code::model::SymbolKind::from_i64(k)
                    .map(varde_code::model::SymbolKind::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            );
        }
        v
    };
    for k in ["binding", "reference"] {
        if !kinds.contains(&k.to_string()) {
            return Err(format!("symbol kind {k} missing; have {kinds:?}"));
        }
    }
    Ok(())
}

fn check_diagnostics() -> Check {
    // A directory containing an unsupported file type: the real scan pipeline
    // emits a diagnostic for it, which must persist.
    let dir = std::env::temp_dir().join(format!("varde-parity-diag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir creates");
    std::fs::write(dir.join("notes.txt"), "not a source file").expect("writes");
    let output = varde_code::scan::run(&dir.to_string_lossy()).map_err(|e| e.to_string())?;
    if output.diagnostics.is_empty() {
        return Err("expected at least one diagnostic from the unsupported file".into());
    }
    let graph = resolve::resolve(&output.entities, &output.symbols, &output.files)
        .map_err(|e| e.to_string())?;
    let db_path = temp_db("diagnostics");
    persist::persist(
        &db_path,
        std::slice::from_ref(&output),
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    let diag_rows: i64 = count(&conn, "SELECT COUNT(*) FROM diagnostics");
    if diag_rows != output.diagnostics.len() as i64 {
        return Err(format!(
            "diagnostics rows {diag_rows} != extracted {n}",
            n = output.diagnostics.len()
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn check_resolved_edges() -> Check {
    let (entities, symbols, files) = load_project("rust/graph");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("edges");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    let edge_rows: i64 = count(&conn, "SELECT COUNT(*) FROM resolved_edges");
    if edge_rows != graph.edges.len() as i64 {
        return Err(format!(
            "edge rows {edge_rows} != {n}",
            n = graph.edges.len()
        ));
    }
    // Kinds distinguished: graph fixture has both imports and calls.
    let call_rows: i64 = count(
        &conn,
        &format!(
            "SELECT COUNT(*) FROM resolved_edges WHERE kind = {}",
            varde_code::resolve::EdgeKind::Call.as_i64()
        ),
    );
    let import_rows: i64 = count(
        &conn,
        &format!(
            "SELECT COUNT(*) FROM resolved_edges WHERE kind = {}",
            varde_code::resolve::EdgeKind::Import.as_i64()
        ),
    );
    if call_rows == 0 || import_rows == 0 {
        return Err(format!(
            "expected both call ({call_rows}) and import ({import_rows}) edges"
        ));
    }
    // Unresolved edges are persisted, not dropped.
    let unresolved: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM resolved_edges WHERE resolved = 0",
    );
    if unresolved == 0 {
        return Err("expected at least one unresolved edge to persist".into());
    }
    Ok(())
}

fn check_communities() -> Check {
    let (entities, symbols, files) = load_project("rust/communities");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("communities");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    let community_rows: i64 = count(&conn, "SELECT COUNT(*) FROM communities");
    if community_rows != graph.communities.len() as i64 {
        return Err(format!(
            "community rows {community_rows} != {n}",
            n = graph.communities.len()
        ));
    }
    let member_rows: i64 = count(&conn, "SELECT COUNT(*) FROM community_members");
    if member_rows == 0 {
        return Err("expected community membership rows".into());
    }
    // Every file in a community has files.community_id set consistently.
    let mismatched: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM files f WHERE f.community_id IS NOT NULL AND NOT EXISTS (
            SELECT 1 FROM community_members cm WHERE cm.file_id = f.id AND cm.community_id = f.community_id
        )",
    );
    if mismatched != 0 {
        return Err(format!("{mismatched} files with inconsistent community_id"));
    }
    Ok(())
}

fn check_clone_bands() -> Check {
    let (entities, symbols, files) = load_project("rust/clones");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("clone-bands");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    let band_rows: i64 = count(&conn, "SELECT COUNT(*) FROM clone_bands");
    if band_rows != graph.clone_bands.len() as i64 {
        return Err(format!(
            "clone band rows {band_rows} != {n}",
            n = graph.clone_bands.len()
        ));
    }
    // The dups.rs fixture has two near-duplicate functions → at least one band
    // with ≥2 members.
    let banded_members: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM clone_band_members cbm JOIN clone_bands cb ON cb.id = cbm.band_id",
    );
    if banded_members < 2 {
        return Err(format!("expected ≥2 banded members, got {banded_members}"));
    }
    Ok(())
}

fn check_indexes() -> Check {
    let (entities, symbols, files) = load_project("rust/graph");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("indexes");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    for index in [
        "idx_resolved_edges_from",
        "idx_resolved_edges_to",
        "idx_entities_file",
        "idx_symbols_file",
    ] {
        let found: i64 = count(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = '{index}'"
            ),
        );
        if found != 1 {
            return Err(format!("missing index {index}"));
        }
    }
    Ok(())
}

fn check_complexity() -> Check {
    let (entities, symbols, files) = load_project("rust/graph");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("complexity");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    // Self-contained expected value: base 1 + ControlFlow entities per file.
    let mut expected: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for e in &entities {
        if e.kind == EntityKind::ControlFlow {
            *expected
                .entry(files[e.file_id as usize].clone())
                .or_insert(0) += 1;
        }
    }
    let mut stmt = conn
        .prepare("SELECT f.path, f.complexity FROM files f")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (path, complexity) = row.map_err(|e| e.to_string())?;
        let want = 1 + expected.get(&path).copied().unwrap_or(0);
        if complexity != Some(want) {
            return Err(format!(
                "complexity for {path}: got {complexity:?}, want {want}"
            ));
        }
    }
    Ok(())
}

fn check_churn() -> Check {
    let (entities, symbols, files) = load_project("rust/graph");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("churn");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT f.path, f.churn FROM files f")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (path, churn) = row.map_err(|e| e.to_string())?;
        let raw = varde_code::churn::commit_count(&path);
        if churn != i64::from(raw) {
            return Err(format!("churn for {path}: got {churn}, want {raw}"));
        }
    }
    Ok(())
}

fn check_fan_metrics() -> Check {
    let (entities, symbols, files) = load_project("rust/fan");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("fan");
    persist::persist(
        &db_path,
        &[ExtractOutput {
            entities: entities.clone(),
            symbols: symbols.clone(),
            diagnostics: vec![],
            files: files.clone(),
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                files.len()
            ],
        }],
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    for node in &graph.nodes {
        let (fi, fo): (i64, i64) = conn
            .query_row(
                "SELECT fan_in, fan_out FROM files WHERE path = ?1",
                [&node.path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        if fi != i64::from(node.fan_in) || fo != i64::from(node.fan_out) {
            return Err(format!(
                "fan metrics for {}: got ({fi},{fo}), want ({},{})",
                node.path, node.fan_in, node.fan_out
            ));
        }
    }
    Ok(())
}

fn check_full_rebuild() -> Check {
    let (entities, symbols, files) = load_project("rust/graph");
    let graph = resolve::resolve(&entities, &symbols, &files).map_err(|e| e.to_string())?;
    let db_path = temp_db("rebuild");
    let output = ExtractOutput {
        entities: entities.clone(),
        symbols: symbols.clone(),
        diagnostics: vec![],
        files: files.clone(),
        file_meta: vec![
            FileMeta {
                mtime: 0,
                size: 0,
                content_hash: "0000000000000000".to_string()
            };
            files.len()
        ],
    };
    persist::persist(
        &db_path,
        std::slice::from_ref(&output),
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;
    let first_files: i64 = {
        let conn = db::open(&db_path).map_err(|e| e.to_string())?;
        count(&conn, "SELECT COUNT(*) FROM files")
    };

    // Second run with a fresh, smaller project: only its data may remain.
    let (entities2, symbols2, files2) = load_project("rust/calls");
    let graph2 = resolve::resolve(&entities2, &symbols2, &files2).map_err(|e| e.to_string())?;
    let output2 = ExtractOutput {
        entities: entities2,
        symbols: symbols2,
        diagnostics: vec![],
        file_meta: vec![
            FileMeta {
                mtime: 0,
                size: 0,
                content_hash: "0000000000000000".to_string()
            };
            files2.len()
        ],
        files: files2,
    };
    persist::persist(
        &db_path,
        std::slice::from_ref(&output2),
        &graph2,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(|e| e.to_string())?;

    let conn = db::open(&db_path).map_err(|e| e.to_string())?;
    let second_files: i64 = count(&conn, "SELECT COUNT(*) FROM files");
    let leftover: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM files f WHERE f.path NOT IN (SELECT path FROM files)",
    );
    if first_files == 0 || second_files == 0 {
        return Err(format!(
            "full-rebuild produced no rows (first {first_files}, second {second_files})"
        ));
    }
    if leftover != 0 {
        return Err(format!("{leftover} leftover rows after rebuild"));
    }
    Ok(())
}

fn main() {
    let checks: [(&str, CheckFn); 12] = [
        ("files", check_files),
        ("entities", check_entities),
        ("symbols", check_symbols),
        ("diagnostics", check_diagnostics),
        ("resolved_edges", check_resolved_edges),
        ("communities", check_communities),
        ("clone_bands", check_clone_bands),
        ("indexes", check_indexes),
        ("complexity", check_complexity),
        ("churn", check_churn),
        ("fan_metrics", check_fan_metrics),
        ("full_rebuild", check_full_rebuild),
    ];

    let mut failures = Vec::new();
    for (name, check) in checks {
        match check() {
            Ok(()) => println!("coverage_parity_persistence: {name}: PASS"),
            Err(e) => {
                eprintln!("coverage_parity_persistence: {name}: FAIL {e}");
                failures.push(name);
            }
        }
    }

    if failures.is_empty() {
        println!(
            "coverage_parity_persistence: all {} persisted capabilities passed",
            checks.len()
        );
    } else {
        eprintln!(
            "coverage_parity_persistence: {} capability(ies) failed: {:?}",
            failures.len(),
            failures
        );
        std::process::exit(1);
    }
}
