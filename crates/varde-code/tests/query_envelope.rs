//! CLI envelope contract tests: the uniform `{"ok",...}` JSON shape across
//! all query subcommands, and the `--help` registration of all 17 modes.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_varde-code");

/// All 17 query-mode subcommand names, exactly as registered in `--help`.
const MODES: [&str; 17] = [
    "symbols_in_file",
    "get_symbol",
    "dependencies",
    "dependents",
    "tests_for_file",
    "hotspots",
    "map_file",
    "map_symbol",
    "map_path",
    "explore",
    "blast_radius",
    "symbol_blast_radius",
    "detect_changes",
    "find_imports",
    "type_hierarchy",
    "filter_symbols",
    "find_pattern",
];

fn run(args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).output().expect("binary runs");
    assert!(
        out.status.success(),
        "command {args:?} exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn help_lists_all_17_mode_subcommands() {
    let help = run(&["--help"]);
    for mode in MODES {
        assert!(
            help.contains(mode),
            "--help must list the {mode:?} subcommand; got:\n{help}"
        );
    }
}

#[test]
fn success_is_ok_true_with_data() {
    // A real persisted database: extract+resolve+persist one fixture file.
    let db_dir = std::env::temp_dir().join(format!("varde-qenv-ok-{}", std::process::id()));
    std::fs::create_dir_all(&db_dir).expect("temp dir creates");
    let db = db_dir.join("index.db");
    let fixture = format!(
        "{}/tests/resolve_fixtures/rust/graph/a.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    let source = std::fs::read_to_string(&fixture).expect("fixture reads");
    let parsed = varde_code::parse::parse_source(
        &varde_code::parse::language_for_path(std::path::Path::new(&fixture))
            .expect("supported language"),
        &source,
    );
    let result = varde_code::extract::extract(&parsed, 0);
    let output = varde_code::model::ExtractOutput {
        entities: result.entities.clone(),
        symbols: result.symbols.clone(),
        diagnostics: vec![],
        files: vec![fixture.clone()],
        file_meta: vec![
            varde_code::model::FileMeta {
                mtime: 0,
                size: 0,
                content_hash: "0000000000000000".to_string()
            };
            1
        ],
    };
    let graph = varde_code::resolve::resolve(&output.entities, &output.symbols, &output.files)
        .expect("resolve");
    varde_code::persist::persist(
        &db,
        std::slice::from_ref(&output),
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .expect("persist");

    let json = format!(r#"{{"dbPath":"{}","filePath":"a.rs"}}"#, db.display());
    let stdout = run(&["symbols_in_file", "--json", &json]);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["ok"], true, "stdout: {stdout}");
    assert!(value.get("data").is_some(), "data key present: {stdout}");
    assert!(
        value.get("error").is_none(),
        "no error key on success: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&db_dir);
}

#[test]
fn malformed_input_is_ok_false_with_error() {
    let stdout = run(&["symbols_in_file", "--json", "this is not json"]);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["ok"], false, "stdout: {stdout}");
    assert_eq!(value["error"]["code"], "invalid_input", "stdout: {stdout}");
    assert!(value["error"]["message"].is_string(), "stdout: {stdout}");
    assert!(
        value.get("data").is_none(),
        "no data key on failure: {stdout}"
    );
}

#[test]
fn missing_required_field_is_ok_false_with_error() {
    let db_dir = std::env::temp_dir().join(format!("varde-qenv-miss-{}", std::process::id()));
    std::fs::create_dir_all(&db_dir).expect("temp dir creates");
    let db = db_dir.join("index.db");
    // Missing filePath.
    let json = format!(r#"{{"dbPath":"{}"}}"#, db.display());
    let stdout = run(&["symbols_in_file", "--json", &json]);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["ok"], false, "stdout: {stdout}");
    assert_eq!(value["error"]["code"], "invalid_input", "stdout: {stdout}");
    let _ = std::fs::remove_dir_all(&db_dir);
}
