//! Integration tests for the five simple-lookup query modes:
//! symbols_in_file, get_symbol, tests_for_file, find_imports, filter_symbols.
//!
//! Each case persists a real fixture project (extract → resolve → persist)
//! and queries the resulting database.

use std::path::PathBuf;

use varde_code::extract;
use varde_code::model::{Entity, ExtractOutput, FileMeta, Symbol};
use varde_code::parse::parse_source;
use varde_code::persist;
use varde_code::query;
use varde_code::resolve;

const FIXTURES: &str = "tests/resolve_fixtures";

fn fixture_dir(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES)
        .join(rel)
}

fn load_project(rel: &str) -> (Vec<Entity>, Vec<Symbol>, Vec<String>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixture_dir(rel))
        .expect("fixture dir exists")
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
            &varde_code::parse::language_for_path(path).expect("supported language"),
            &std::fs::read_to_string(path).expect("reads"),
        );
        let result = extract::extract(&parsed, file_id as u32);
        entities.extend(result.entities);
        symbols.extend(result.symbols);
    }
    (entities, symbols, files)
}

fn load_query_fixture(rel: &str) -> (Vec<Entity>, Vec<Symbol>, Vec<String>) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/query_fixtures")
        .join(rel);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("query fixture dir exists")
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
            &varde_code::parse::language_for_path(path).expect("supported language"),
            &std::fs::read_to_string(path).expect("reads"),
        );
        let result = extract::extract(&parsed, file_id as u32);
        entities.extend(result.entities);
        symbols.extend(result.symbols);
    }
    (entities, symbols, files)
}

fn temp_db(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("varde-qsimple-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    let db = dir.join("index.db");
    let _ = std::fs::remove_file(&db);
    db
}

fn persist_project(
    tag: &str,
    entities: Vec<Entity>,
    symbols: Vec<Symbol>,
    files: Vec<String>,
) -> PathBuf {
    let db_path = temp_db(tag);
    let graph = resolve::resolve(&entities, &symbols, &files).expect("resolve succeeds");
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
        &db_path,
        std::slice::from_ref(&output),
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .expect("persist succeeds");
    db_path
}

/// Run a query mode against a db path with extra input fields; returns the
/// full envelope.
fn envelope(mode: &str, db_path: &std::path::Path, extra: &str) -> serde_json::Value {
    let input = format!(r#"{{"dbPath":"{db}",{extra}}}"#, db = db_path.display());
    let stdout = query::run_mode(mode, &input);
    serde_json::from_str(&stdout).expect("envelope is JSON")
}

fn data(mode: &str, db_path: &std::path::Path, extra: &str) -> serde_json::Value {
    envelope(mode, db_path, extra)["data"].clone()
}

mod symbols_in_file {
    use super::*;

    #[test]
    fn returns_exactly_the_expected_symbols() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("sif", entities.clone(), symbols.clone(), files.clone());
        // Full path of a.rs.
        let a_rs = files
            .iter()
            .find(|f| f.ends_with("/a.rs"))
            .cloned()
            .expect("a.rs entity exists");
        let a_rs_id = files.iter().position(|f| f == &a_rs).unwrap() as u32;

        let result = data("symbols_in_file", &db, &format!(r#""filePath":"{a_rs}""#));
        let arr = result.as_array().expect("data is an array");
        // Every symbol from a.rs in the loaded project is listed.
        let expected: Vec<&Symbol> = symbols.iter().filter(|s| s.file_id == a_rs_id).collect();
        assert_eq!(arr.len(), expected.len(), "result: {result}");
        for item in arr {
            assert!(item["name"].is_string());
            assert!(item["kind"].is_string());
            assert_eq!(item["file"], a_rs);
            assert!(item["span"]["start_line"].is_number());
        }
        let names: Vec<String> = arr
            .iter()
            .map(|i| i["name"].as_str().unwrap().to_string())
            .collect();
        for sym in &expected {
            assert!(names.contains(&sym.name), "missing symbol {}", sym.name);
        }
    }

    #[test]
    fn suffix_path_matching_works() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project(
            "sif-suffix",
            entities.clone(),
            symbols.clone(),
            files.clone(),
        );
        let result = data("symbols_in_file", &db, r#""filePath":"a.rs""#);
        assert!(
            result.as_array().is_some_and(|a| !a.is_empty()),
            "suffix query must find /.../a.rs: {result}"
        );
    }

    #[test]
    fn suffix_path_matching_selects_lexicographically_first_path() {
        let (entities, symbols, mut files) = load_project("rust/graph");
        let a_rs_index = files.iter().position(|f| f.ends_with("/a.rs")).unwrap();
        let other_index = files.iter().position(|f| f.ends_with("/b.rs")).unwrap();
        files[a_rs_index] = "/z/a.rs".to_string();
        files[other_index] = "/a/a.rs".to_string();

        let db = persist_project("sif-suffix-order", entities, symbols, files);
        let result = data("symbols_in_file", &db, r#""filePath":"a.rs""#);
        let arr = result.as_array().expect("data is an array");
        assert!(
            !arr.is_empty(),
            "lexical suffix match must find a file: {result}"
        );
        assert!(
            arr.iter().all(|item| item["file"] == "/a/a.rs"),
            "result: {result}"
        );
    }

    #[test]
    fn suffix_path_matching_requires_component_boundary() {
        let (entities, symbols, mut files) = load_project("rust/graph");
        let a_rs_index = files.iter().position(|f| f.ends_with("/a.rs")).unwrap();
        files[a_rs_index] = "/root/ab.rs".to_string();

        let db = persist_project("sif-suffix-boundary", entities, symbols, files);
        let env = envelope("symbols_in_file", &db, r#""filePath":"a.rs""#);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }

    #[test]
    fn known_file_with_no_symbols_returns_empty_not_error() {
        let db = temp_db("sif-empty");
        // A file that exists only via a diagnostic → files row, zero symbols.
        let output = ExtractOutput {
            entities: vec![],
            symbols: vec![],
            diagnostics: vec![varde_code::model::Diagnostic {
                file_id: 0,
                message: "unsupported".to_string(),
                severity: "info".to_string(),
            }],
            files: vec!["lonely.txt".to_string()],
            file_meta: vec![
                FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0000000000000000".to_string()
                };
                1
            ],
        };
        let graph = resolve::resolve(&[], &[], &[]).expect("resolve empty");
        persist::persist(
            &db,
            std::slice::from_ref(&output),
            &graph,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("persist");

        let env = envelope("symbols_in_file", &db, r#""filePath":"lonely.txt""#);
        assert_eq!(env["ok"], true);
        assert_eq!(env["data"], serde_json::json!([]));
    }

    #[test]
    fn unknown_file_is_not_found() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("sif-nf", entities.clone(), symbols.clone(), files.clone());
        let env = envelope("symbols_in_file", &db, r#""filePath":"ghost.rs""#);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}

mod get_symbol {
    use super::*;

    #[test]
    fn returns_persisted_fields_for_known_symbol() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("gs", entities.clone(), symbols.clone(), files.clone());
        let a_rs = files
            .iter()
            .find(|f| f.ends_with("/a.rs"))
            .cloned()
            .expect("a.rs exists");
        let a_rs_id = files.iter().position(|f| f == &a_rs).unwrap() as u32;
        let sym = symbols
            .iter()
            .find(|s| s.file_id == a_rs_id)
            .expect("a.rs symbol");

        let result = data(
            "get_symbol",
            &db,
            &format!(r#""name":"{}","filePath":"a.rs""#, sym.name),
        );
        assert_eq!(result["name"], sym.name);
        assert_eq!(result["kind"], "reference"); // rust fixtures produce references
        assert_eq!(result["file"], a_rs);
        assert_eq!(result["span"]["start_line"], sym.span.start_line);
        assert_eq!(result["span"]["end_col"], sym.span.end_col);
    }

    #[test]
    fn kind_scope_filters() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("gs-kind", entities.clone(), symbols.clone(), files.clone());
        let a_rs = files
            .iter()
            .find(|f| f.ends_with("/a.rs"))
            .cloned()
            .expect("a.rs exists");
        let a_rs_id = files.iter().position(|f| f == &a_rs).unwrap() as u32;
        let sym = symbols
            .iter()
            .find(|s| s.file_id == a_rs_id)
            .expect("symbol");

        // Wrong kind → not found.
        let env = envelope(
            "get_symbol",
            &db,
            &format!(r#""name":"{}","kind":"binding""#, sym.name),
        );
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }

    #[test]
    fn unknown_symbol_is_not_found() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("gs-nf", entities.clone(), symbols.clone(), files.clone());
        let env = envelope("get_symbol", &db, r#""name":"ghost_symbol""#);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}

mod tests_for_file {
    use super::*;

    #[test]
    fn returns_test_files_importing_the_target() {
        let (entities, symbols, files) = load_query_fixture("tests_for_file");
        let db = persist_project("tff", entities.clone(), symbols.clone(), files.clone());
        let lib_rs = files
            .iter()
            .find(|f| f.ends_with("/lib.rs"))
            .cloned()
            .expect("lib.rs exists");

        let result = data("tests_for_file", &db, &format!(r#""filePath":"{lib_rs}""#));
        let arr = result.as_array().expect("array");
        assert_eq!(arr.len(), 1, "one test file covers lib.rs: {result}");
        assert!(
            arr[0].as_str().unwrap().ends_with("lib_tests.rs"),
            "got: {result}"
        );
    }

    #[test]
    fn no_covering_tests_is_empty_not_error() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project(
            "tff-empty",
            entities.clone(),
            symbols.clone(),
            files.clone(),
        );
        let a_rs = files
            .iter()
            .find(|f| f.ends_with("/a.rs"))
            .cloned()
            .expect("a.rs exists");
        let env = envelope("tests_for_file", &db, &format!(r#""filePath":"{a_rs}""#));
        assert_eq!(env["ok"], true);
        assert_eq!(env["data"], serde_json::json!([]));
    }

    #[test]
    fn unknown_file_is_not_found() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("tff-nf", entities.clone(), symbols.clone(), files.clone());
        let env = envelope("tests_for_file", &db, r#""filePath":"ghost.rs""#);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}

mod find_imports {
    use super::*;

    #[test]
    fn lists_resolved_and_unresolved_import_edges() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("fi", entities.clone(), symbols.clone(), files.clone());
        let a_rs = files
            .iter()
            .find(|f| f.ends_with("/a.rs"))
            .cloned()
            .expect("a.rs exists");

        let result = data("find_imports", &db, &format!(r#""filePath":"{a_rs}""#));
        let arr = result.as_array().expect("array");
        // a.rs: use b::fn_b, use c::fn_c, use missing::thing → 3 import edges.
        assert_eq!(arr.len(), 3, "result: {result}");
        let resolved: Vec<&serde_json::Value> =
            arr.iter().filter(|e| e["resolved"] == true).collect();
        let unresolved: Vec<&serde_json::Value> =
            arr.iter().filter(|e| e["resolved"] == false).collect();
        assert_eq!(resolved.len(), 2);
        assert_eq!(unresolved.len(), 1);
        assert!(
            arr.iter()
                .any(|e| e["to"].as_str().unwrap().ends_with("b.rs"))
        );
        assert!(
            arr.iter()
                .any(|e| e["to"].as_str().unwrap().ends_with("c.rs"))
        );
    }

    #[test]
    fn unknown_file_is_not_found() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("fi-nf", entities.clone(), symbols.clone(), files.clone());
        let env = envelope("find_imports", &db, r#""filePath":"ghost.rs""#);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}

mod filter_symbols {
    use super::*;

    #[test]
    fn filters_by_file_and_caps_results() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("fs", entities.clone(), symbols.clone(), files.clone());
        let result = data("filter_symbols", &db, r#""file":"a.rs","maxSymbols":1"#);
        let arr = result.as_array().expect("array");
        assert_eq!(arr.len(), 1, "maxSymbols cap applies: {result}");
        assert!(arr[0]["file"].as_str().unwrap().ends_with("a.rs"));
    }

    #[test]
    fn no_matches_is_empty_not_error() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("fs-empty", entities.clone(), symbols.clone(), files.clone());
        // kind=binding does not exist in the rust graph fixture.
        let env = envelope("filter_symbols", &db, r#""kind":"binding""#);
        assert_eq!(env["ok"], true);
        assert_eq!(env["data"], serde_json::json!([]));
    }

    #[test]
    fn unknown_file_is_not_found() {
        let (entities, symbols, files) = load_project("rust/graph");
        let db = persist_project("fs-nf", entities.clone(), symbols.clone(), files.clone());
        let env = envelope("filter_symbols", &db, r#""file":"ghost.rs""#);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "not_found");
    }
}
