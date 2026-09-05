//! Integration tests for the mapping/diff query modes: map_file, map_symbol,
//! map_path, detect_changes, hotspots.

use std::path::PathBuf;

use varde_code::model::{Entity, EntityKind, ExtractOutput, FileMeta, Span};
use varde_code::persist;
use varde_code::query;
use varde_code::resolve::{self, EdgeKind, EdgeTarget, FileNode, ResolvedEdge, ResolvedGraph};

static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("varde-qmap-{}-{}-{}", tag, std::process::id(), seq));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    dir
}

fn temp_db(tag: &str) -> PathBuf {
    let dir = temp_dir(tag);
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

fn envelope(mode: &str, input: &str) -> serde_json::Value {
    let stdout = query::run_mode(mode, input);
    serde_json::from_str(&stdout).expect("envelope is JSON")
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn git_init(repo: &std::path::Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Query Test"]);
}

mod map_file_mode {
    use super::*;

    #[test]
    fn returns_persisted_node_info() {
        let db = temp_db("map-file");
        let a = fn_entity(0, "fn_a");
        let output = ExtractOutput {
            entities: vec![a],
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

        let env = envelope(
            "map_file",
            &format!(r#"{{"dbPath":"{}","filePath":"a.rs"}}"#, db.display()),
        );
        assert_eq!(env["ok"], true, "{env}");
        assert_eq!(env["data"]["path"], "a.rs");
        assert_eq!(env["data"]["complexity"], 1);
        assert_eq!(env["data"]["fan_in"], 0);
        assert_eq!(env["data"]["fan_out"], 0);
    }

    #[test]
    fn unknown_file_is_not_found() {
        let db = temp_db("map-file-nf");
        let output = ExtractOutput {
            entities: vec![fn_entity(0, "fn_a")],
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
        let env = envelope(
            "map_file",
            &format!(r#"{{"dbPath":"{}","filePath":"ghost.rs"}}"#, db.display()),
        );
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}

mod map_symbol_mode {
    use super::*;

    #[test]
    fn returns_persisted_entity_fields() {
        let db = temp_db("map-symbol");
        let mut route = fn_entity(0, "handler");
        route.kind = EntityKind::Route;
        route.method = Some("get".to_string());
        route.path = Some("/api/items".to_string());
        route.status = Some("200".to_string());
        route.body_shape = Some("json".to_string());
        route.enclosing_function = Some("outer".to_string());
        let output = ExtractOutput {
            entities: vec![route],
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

        let env = envelope(
            "map_symbol",
            &format!(r#"{{"dbPath":"{}","name":"handler"}}"#, db.display()),
        );
        assert_eq!(env["ok"], true, "{env}");
        assert_eq!(env["data"]["kind"], "route");
        assert_eq!(env["data"]["file"], "a.rs");
        assert_eq!(env["data"]["method"], "get");
        assert_eq!(env["data"]["path"], "/api/items");
        assert_eq!(env["data"]["status"], "200");
        assert_eq!(env["data"]["body_shape"], "json");
        assert_eq!(env["data"]["enclosing_function"], "outer");
    }

    #[test]
    fn unknown_symbol_is_not_found() {
        let db = temp_db("map-symbol-nf");
        let output = ExtractOutput {
            entities: vec![fn_entity(0, "fn_a")],
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
        let env = envelope(
            "map_symbol",
            &format!(r#"{{"dbPath":"{}","name":"ghost"}}"#, db.display()),
        );
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}

mod map_path_mode {
    use super::*;

    fn chain_db() -> PathBuf {
        let db = temp_db("map-path");
        let mut entities = Vec::new();
        let mut nodes = Vec::new();
        let mut files = Vec::new();
        for i in 0..3 {
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
        let edges = vec![
            ResolvedEdge {
                from: 0,
                to: EdgeTarget::File(1),
                kind: EdgeKind::Import,
                resolved: true,
                from_entity: None,
            },
            ResolvedEdge {
                from: 1,
                to: EdgeTarget::File(2),
                kind: EdgeKind::Import,
                resolved: true,
                from_entity: None,
            },
        ];
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
        .expect("persist");
        db
    }

    #[test]
    fn returns_shortest_dependency_path() {
        let db = chain_db();
        let env = envelope(
            "map_path",
            &format!(
                r#"{{"dbPath":"{}","sourceFile":"f0.rs","targetFile":"f2.rs"}}"#,
                db.display()
            ),
        );
        assert_eq!(env["ok"], true, "{env}");
        let arr = env["data"].as_array().expect("array");
        let paths: Vec<String> = arr
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(paths, vec!["f0.rs", "f1.rs", "f2.rs"]);
    }

    #[test]
    fn unreachable_target_is_empty_not_error() {
        let db = chain_db();
        let env = envelope(
            "map_path",
            &format!(
                r#"{{"dbPath":"{}","sourceFile":"f2.rs","targetFile":"f0.rs"}}"#,
                db.display()
            ),
        );
        assert_eq!(env["ok"], true);
        assert_eq!(env["data"], serde_json::json!([]));
    }

    #[test]
    fn unknown_file_is_not_found() {
        let db = chain_db();
        let env = envelope(
            "map_path",
            &format!(
                r#"{{"dbPath":"{}","sourceFile":"ghost.rs","targetFile":"f0.rs"}}"#,
                db.display()
            ),
        );
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}

mod detect_changes_mode {
    use super::*;

    const V1: &str = "fn alpha() {}\nfn beta() {}\nfn main() { alpha(); beta(); }\n";
    const V2: &str = "fn alpha(x: i32) -> i32 { x }\nfn gamma() {}\nfn main() { alpha(1); }\n";

    #[test]
    fn classifies_added_removed_modified() {
        let repo = temp_dir("dc-repo");
        git_init(&repo);
        let file = repo.join("v1.rs");
        std::fs::write(&file, V1).expect("writes");
        git(&repo, &["add", "v1.rs"]);
        git(&repo, &["commit", "-q", "-m", "before"]);

        // Persist the BEFORE state.
        let db = temp_db("dc-db");
        let abs = file.to_string_lossy().into_owned();
        let parsed = varde_code::parse::parse_source(
            &varde_code::parse::language_for_path(&file).expect("supported"),
            V1,
        );
        let result = varde_code::extract::extract(&parsed, 0);
        let output = ExtractOutput {
            entities: result.entities.clone(),
            symbols: result.symbols.clone(),
            diagnostics: vec![],
            files: vec![abs],
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

        // Apply the AFTER state (uncommitted → working_tree diff).
        std::fs::write(&file, V2).expect("writes");

        let env = envelope(
            "detect_changes",
            &format!(
                r#"{{"dbPath":"{}","repoRoot":"{}","diffMode":"working_tree"}}"#,
                db.display(),
                repo.display()
            ),
        );
        assert_eq!(env["ok"], true, "{env}");
        let arr = env["data"].as_array().expect("array");
        assert_eq!(arr.len(), 1, "one changed file: {env}");
        let entry = &arr[0];
        assert_eq!(entry["file"], "v1.rs");
        assert_eq!(entry["status"], "M");

        let symbols = entry["symbols"].as_array().expect("symbols array");
        let summarize: Vec<(String, String)> = symbols
            .iter()
            .map(|s| {
                (
                    s["name"].as_str().unwrap().to_string(),
                    s["change"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        // beta removed; gamma + x added; main modified (span moved).
        assert!(
            summarize.contains(&("beta".into(), "removed".into())),
            "{summarize:?}"
        );
        assert!(
            summarize.contains(&("gamma".into(), "added".into())),
            "{summarize:?}"
        );
        assert!(
            summarize.contains(&("x".into(), "added".into())),
            "{summarize:?}"
        );
        assert!(
            summarize.contains(&("main".into(), "modified".into())),
            "{summarize:?}"
        );
        assert!(
            !summarize.contains(&("alpha".into(), "modified".into())),
            "alpha span unchanged: {summarize:?}"
        );
    }
}

mod hotspots_mode {
    use super::*;

    #[test]
    fn orders_by_descending_complexity_times_churn() {
        let repo = temp_dir("hot-repo");
        git_init(&repo);

        // hot_a.rs: control flow (if + for) → complexity 3; committed twice.
        let a = repo.join("hot_a.rs");
        std::fs::write(
            &a,
            "pub fn hot_a() {\n    let mut s = 0;\n    for i in 0..3 { s += i; }\n    if s > 2 { s } else { 0 };\n}\n",
        )
        .expect("writes");
        git(&repo, &["add", "hot_a.rs"]);
        git(&repo, &["commit", "-q", "-m", "a1"]);
        std::fs::write(
            &a,
            "pub fn hot_a() {\n    let mut s = 0;\n    for i in 0..5 { s += i; }\n    if s > 2 { s } else { 0 };\n}\n",
        )
        .expect("writes");
        git(&repo, &["add", "hot_a.rs"]);
        git(&repo, &["commit", "-q", "-m", "a2"]);

        // hot_b.rs: no control flow → complexity 1; committed once.
        let b = repo.join("hot_b.rs");
        std::fs::write(&b, "pub fn hot_b() {}\n").expect("writes");
        git(&repo, &["add", "hot_b.rs"]);
        git(&repo, &["commit", "-q", "-m", "b1"]);

        // Extract both files and persist.
        let db = temp_db("hot-db");
        let mut entities = Vec::new();
        let mut files = Vec::new();
        for (i, path) in [&a, &b].into_iter().enumerate() {
            let abs = path.to_string_lossy().into_owned();
            let parsed = varde_code::parse::parse_source(
                &varde_code::parse::language_for_path(path).expect("supported"),
                &std::fs::read_to_string(path).expect("reads"),
            );
            let result = varde_code::extract::extract(&parsed, i as u32);
            entities.extend(result.entities);
            files.push(abs);
        }
        let output = ExtractOutput {
            entities: entities.clone(),
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
        let graph =
            resolve::resolve(&output.entities, &output.symbols, &output.files).expect("resolve");
        persist::persist(
            &db,
            std::slice::from_ref(&output),
            &graph,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("persist");

        let env = envelope("hotspots", &format!(r#"{{"dbPath":"{}"}}"#, db.display()));
        assert_eq!(env["ok"], true, "{env}");
        let arr = env["data"].as_array().expect("array");
        assert_eq!(arr.len(), 2);

        let first = &arr[0];
        let second = &arr[1];
        let a_name = first["file"].as_str().unwrap();
        let b_name = second["file"].as_str().unwrap();
        assert!(a_name.ends_with("hot_a.rs"), "hot_a first: {env}");
        assert!(b_name.ends_with("hot_b.rs"), "hot_b second: {env}");
        // hot_a: complexity 3 * churn 2 = 6; hot_b: 1 * 1 = 1.
        assert_eq!(first["complexity"], 3, "{env}");
        assert_eq!(first["churn"], 2, "{env}");
        assert_eq!(first["score"], 6, "{env}");
        assert_eq!(second["score"], 1, "{env}");
    }

    #[test]
    fn excludes_generated_or_vendored_paths_despite_high_churn() {
        let repo = temp_dir("hot-noise-repo");
        git_init(&repo);

        // A normal source file, committed once → low churn.
        let src = repo.join("real_source.rs");
        std::fs::write(&src, "pub fn real_source() {}\n").expect("writes");
        git(&repo, &["add", "real_source.rs"]);
        git(&repo, &["commit", "-q", "-m", "src1"]);

        // A file under target/ (generated/vendored), committed many times →
        // deliberately high raw churn, to prove the noise filter — not the
        // scoring algorithm — is what excludes it.
        let noisy_dir = repo.join("target");
        std::fs::create_dir_all(&noisy_dir).expect("mkdir");
        let noisy = noisy_dir.join("generated.rs");
        for i in 0..5 {
            std::fs::write(&noisy, format!("pub fn generated_{i}() {{}}\n")).expect("writes");
            git(&repo, &["add", "target/generated.rs"]);
            git(&repo, &["commit", "-q", "-m", &format!("gen{i}")]);
        }

        let db = temp_db("hot-noise-db");
        let mut entities = Vec::new();
        let mut files = Vec::new();
        for (i, path) in [&src, &noisy].into_iter().enumerate() {
            let abs = path.to_string_lossy().into_owned();
            let parsed = varde_code::parse::parse_source(
                &varde_code::parse::language_for_path(path).expect("supported"),
                &std::fs::read_to_string(path).expect("reads"),
            );
            let result = varde_code::extract::extract(&parsed, i as u32);
            entities.extend(result.entities);
            files.push(abs);
        }
        let output = ExtractOutput {
            entities: entities.clone(),
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
        let graph =
            resolve::resolve(&output.entities, &output.symbols, &output.files).expect("resolve");
        persist::persist(
            &db,
            std::slice::from_ref(&output),
            &graph,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("persist");

        let env = envelope("hotspots", &format!(r#"{{"dbPath":"{}"}}"#, db.display()));
        assert_eq!(env["ok"], true, "{env}");
        let arr = env["data"].as_array().expect("array");

        // Only the real source file should be present; the target/ file is
        // excluded despite its higher raw churn.
        assert_eq!(arr.len(), 1, "{env}");
        let only = &arr[0];
        let name = only["file"].as_str().unwrap();
        assert!(
            name.ends_with("real_source.rs"),
            "expected only real_source.rs, got: {env}"
        );
        assert!(
            !arr.iter()
                .any(|e| e["file"].as_str().unwrap_or("").contains("target")),
            "target/ file leaked into hotspots output: {env}"
        );
    }

    #[test]
    fn excludes_non_source_files_despite_high_churn() {
        let repo = temp_dir("hot-nonsource-repo");
        git_init(&repo);

        // A normal source file, committed once → low churn.
        let src = repo.join("real_source.rs");
        std::fs::write(&src, "pub fn real_source() {}\n").expect("writes");
        git(&repo, &["add", "real_source.rs"]);
        git(&repo, &["commit", "-q", "-m", "src1"]);

        // A docs file (non-source — the extractor never parses `.md`), churned
        // many times → deliberately high raw churn, to prove the non-source
        // filter excludes it rather than the scoring collapsing it.
        let doc = repo.join("README.md");
        for i in 0..5 {
            std::fs::write(&doc, format!("# heading {i}\n")).expect("writes");
            git(&repo, &["add", "README.md"]);
            git(&repo, &["commit", "-q", "-m", &format!("doc{i}")]);
        }

        let db = temp_db("hot-nonsource-db");
        // The source file carries an entity; the doc file is a bare `files`
        // row with no entity (exactly how the real build records non-source
        // files it walked but could not parse).
        let src_abs = src.to_string_lossy().into_owned();
        let doc_abs = doc.to_string_lossy().into_owned();
        let parsed = varde_code::parse::parse_source(
            &varde_code::parse::language_for_path(&src).expect("supported"),
            &std::fs::read_to_string(&src).expect("reads"),
        );
        let entities = varde_code::extract::extract(&parsed, 0).entities;
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
                2
            ],
            files: vec![src_abs, doc_abs],
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

        let env = envelope("hotspots", &format!(r#"{{"dbPath":"{}"}}"#, db.display()));
        assert_eq!(env["ok"], true, "{env}");
        let arr = env["data"].as_array().expect("array");

        // Only the real source file should be present; README.md is excluded
        // despite its higher raw churn.
        assert_eq!(arr.len(), 1, "{env}");
        assert!(
            arr[0]["file"].as_str().unwrap().ends_with("real_source.rs"),
            "expected only real_source.rs, got: {env}"
        );
        assert!(
            !arr.iter()
                .any(|e| e["file"].as_str().unwrap_or("").ends_with(".md")),
            "non-source .md file leaked into hotspots output: {env}"
        );
    }
}
