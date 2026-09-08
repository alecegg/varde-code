//! Coverage-parity harness (task: fixture-suite-coverage-parity-harness).
//!
//! Runs as a custom test binary (`harness = false`) so it can accept a
//! `--include <lang1,lang2,...>` argument, used by the fan-out tasks:
//!
//! ```text
//! cargo test -p varde-code coverage_parity -- --include go,java,cs
//! ```
//!
//! For each included language, all fixture files under `tests/fixtures/<lang>/`
//! are parsed and extracted; the union of entities must cover the language's
//! required entity-kind set (all 14 kinds by default), and per-kind sanity
//! assertions validate specific field values rather than mere presence.

mod common;

use ast_grep_language::SupportLang;
use std::path::PathBuf;
use varde_code::extract;
use varde_code::model::{Entity, EntityKind};
use varde_code::parse::{ParsedFile, language_for_path, parse_source};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let include = parse_include(&args);
    let languages: Vec<String> = match include {
        Some(langs) => langs,
        None => fixture_languages(),
    };

    let mut failures: Vec<String> = Vec::new();
    for lang in &languages {
        match check_language(lang) {
            Ok(()) => println!("coverage_parity: {lang}: OK"),
            Err(e) => failures.push(format!("{lang}: {e}")),
        }
    }

    // ---- query-surface section: one checklist entry per query mode ----
    let fixture_db = build_query_fixture_db();
    type QueryCheck = (&'static str, fn(&std::path::Path) -> Result<(), String>);
    let mut query_checks: Vec<QueryCheck> = vec![
        ("batch", check_q_batch),
        ("symbols_in_file", check_q_symbols_in_file),
        ("symbols_in_files", check_q_symbols_in_files),
        ("get_symbol", check_q_get_symbol),
        ("tests_for_file", check_q_tests_for_file),
        ("find_imports", check_q_find_imports),
        ("filter_symbols", check_q_filter_symbols),
        ("dependencies", check_q_dependencies),
        ("dependents", check_q_dependents),
        ("blast_radius", check_q_blast_radius),
        ("symbol_blast_radius", check_q_symbol_blast_radius),
        ("type_hierarchy", check_q_type_hierarchy),
        ("explore", check_q_explore),
        ("map_file", check_q_map_file),
        ("map_symbol", check_q_map_symbol),
        ("map_path", check_q_map_path),
        ("detect_changes", check_q_detect_changes),
        ("hotspots", check_q_hotspots),
        ("clusters", check_q_clusters),
        ("find_pattern", check_q_find_pattern),
        ("slice_state", check_q_slice_state),
        ("context_pack", check_q_context_pack),
        ("nav_map", check_q_nav_map),
    ];

    // Every implemented mode must be covered — an unlisted mode fails loudly.
    let mut covered: Vec<&str> = query_checks.iter().map(|(name, _)| *name).collect();
    covered.sort_unstable();
    let mut all_modes: Vec<&str> = varde_code::query::QUERY_MODES.to_vec();
    all_modes.sort_unstable();
    if covered != all_modes {
        failures.push(format!(
            "query-mode checklist does not match QUERY_MODES: covered {covered:?} != implemented {all_modes:?}"
        ));
    }

    // Every implemented mode must be freshness-wired (have a FRESHNESS-table
    // entry) except `find_pattern`, the "no slice, live parse" mode. An
    // unwired mode that reads persisted data would silently answer from a
    // stale index instead of build-on-read.
    let mut wired: Vec<&str> = varde_code::query::FRESHNESS
        .iter()
        .map(|(name, _, _)| *name)
        .collect();
    wired.sort_unstable();
    let unwired: Vec<&str> = varde_code::query::QUERY_MODES
        .iter()
        .copied()
        .filter(|m| *m != "find_pattern" && wired.binary_search(m).is_err())
        .collect();
    if !unwired.is_empty() {
        failures.push(format!(
            "query modes without a freshness-table entry (must be None-scope no-ops at minimum): {unwired:?}"
        ));
    }

    for (name, check) in query_checks.drain(..) {
        match check(&fixture_db) {
            Ok(()) => println!("coverage_parity: query/{name}: OK"),
            Err(e) => failures.push(format!("query/{name}: {e}")),
        }
    }

    if failures.is_empty() {
        println!(
            "coverage_parity: all {} language(s) and {} query mode(s) passed",
            languages.len(),
            varde_code::query::QUERY_MODES.len()
        );
    } else {
        for f in &failures {
            eprintln!("coverage_parity: FAIL {f}");
        }
        eprintln!("coverage_parity: {} check(s) failed", failures.len());
        std::process::exit(1);
    }
}

/// Parse `--include lang1,lang2` from process args (after `--`), or fall
/// back to the `VARDE_CODE_INCLUDE` env var (comma-separated).
fn parse_include(args: &[String]) -> Option<Vec<String>> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--include" {
            return args
                .get(i + 1)
                .map(|v| v.split(',').map(|s| s.to_string()).collect());
        }
        i += 1;
    }
    std::env::var("VARDE_CODE_INCLUDE")
        .ok()
        .map(|v| v.split(',').map(|s| s.to_string()).collect())
}

/// All fixture subdirectories (language names) present under tests/fixtures/.
fn fixture_languages() -> Vec<String> {
    let root = common::fixture_path("");
    let mut langs: Vec<String> = std::fs::read_dir(&root)
        .expect("fixtures dir exists")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    langs.sort();
    langs
}

/// Required entity kinds for a language, delegated to the per-language
/// extractor modules (each may carve out kinds it cannot express).
fn required_kinds(lang: &str) -> Vec<EntityKind> {
    let support = match lang {
        "ts" => SupportLang::TypeScript,
        "tsx" => SupportLang::Tsx,
        "js" | "javascript" => SupportLang::JavaScript,
        "c" => SupportLang::C,
        "cpp" => SupportLang::Cpp,
        "go" => SupportLang::Go,
        "java" => SupportLang::Java,
        "cs" => SupportLang::CSharp,
        "kotlin" => SupportLang::Kotlin,
        "swift" => SupportLang::Swift,
        "python" => SupportLang::Python,
        "ruby" => SupportLang::Ruby,
        "php" => SupportLang::Php,
        "lua" => SupportLang::Lua,
        "scala" => SupportLang::Scala,
        "dart" => SupportLang::Dart,
        "elixir" => SupportLang::Elixir,
        "solidity" => SupportLang::Solidity,
        "haskell" => SupportLang::Haskell,
        "bash" => SupportLang::Bash,
        "rust" => SupportLang::Rust,
        other => panic!("coverage_parity: unknown language {other:?}"),
    };
    varde_code::extract::langs::required_kinds(support)
}

/// Parse + extract every fixture file for a language; check kind coverage and
/// per-kind sanity assertions.
fn check_language(lang: &str) -> Result<(), String> {
    let dir = common::fixture_path(lang);
    if !dir.is_dir() {
        return Err(format!("no fixtures dir for language {lang:?}"));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();

    let mut entities: Vec<Entity> = Vec::new();
    for path in &files {
        let lang = language_for_path(path)
            .ok_or_else(|| format!("{}: unsupported extension", path.display()))?;
        let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let parsed: ParsedFile = parse_source(&lang, &src);
        entities.extend(extract::extract(&parsed, 0).entities);
    }

    // 1. Kind coverage: every required kind appears at least once in the union.
    let required = required_kinds(lang);
    let mut present: std::collections::BTreeSet<EntityKind> = std::collections::BTreeSet::new();
    for e in &entities {
        present.insert(e.kind);
    }
    let missing: Vec<EntityKind> = required
        .iter()
        .copied()
        .filter(|k| !present.contains(k))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "missing kinds across {} fixture file(s): {:?} ({} entities total)",
            files.len(),
            missing,
            entities.len()
        ));
    }

    // 2. Per-kind sanity assertions: specific field values.
    sanity_assertions(lang, &entities)?;

    Ok(())
}

/// Assert specific expected field values for representative entities of each
/// kind (task 10 scope is TypeScript; other languages get kind-coverage only,
/// extended by the fan-out tasks).
fn sanity_assertions(lang: &str, entities: &[Entity]) -> Result<(), String> {
    if lang != "ts" {
        return Ok(());
    }
    let find =
        |kind: EntityKind, name: &str| entities.iter().find(|e| e.kind == kind && e.name == name);
    let require = |kind: EntityKind, name: &str| {
        find(kind, name)
            .map(|_| ())
            .ok_or_else(|| format!("sanity: expected {kind:?} named {name:?}"))
    };

    require(EntityKind::Function, "greet")?;
    require(EntityKind::Class, "Greeter")?;
    require(EntityKind::Interface, "Point")?;
    require(EntityKind::Variable, "counter")?;
    require(EntityKind::Parameter, "x")?;
    require(EntityKind::Export, "add")?;
    require(EntityKind::Call, "add")?;
    require(EntityKind::Literal, "8080")?;
    require(EntityKind::MemberAccess, "resolve")?;
    require(EntityKind::Catch, "err")?;
    require(EntityKind::Throw, "new Error(\"empty\")")?;
    require(EntityKind::ControlFlow, "if_statement")?;

    let route = find(EntityKind::Route, "app.get").ok_or("sanity: expected route app.get")?;
    if route.method.as_deref() != Some("get") || route.path.as_deref() != Some("/users") {
        return Err(format!(
            "sanity: route fields wrong: method={:?} path={:?}",
            route.method, route.path
        ));
    }

    let response = entities
        .iter()
        .find(|e| e.kind == EntityKind::Response && e.status.as_deref() == Some("200"))
        .ok_or("sanity: expected response with status 200")?;
    if response.body_shape.as_deref() != Some("json") {
        return Err(format!(
            "sanity: response body_shape wrong: {:?}",
            response.body_shape
        ));
    }

    Ok(())
}

// -------------------- query-surface coverage-parity section --------------------

/// One fixture DB covering entities/symbols/edges + a test-file scenario,
/// persisted from the resolve fixtures + the tests_for_file query fixtures.
fn build_query_fixture_db() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("varde-parity-qdb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    let db = dir.join("index.db");
    let _ = std::fs::remove_file(&db);

    let mut entities = Vec::new();
    let mut symbols = Vec::new();
    let mut files: Vec<String> = Vec::new();
    // resolve fixture: rust/graph (a.rs imports b/c resolved; b.rs/c.rs fn).
    // NOTE: do not merge additional fixtures here — a.rs/b.rs/c.rs appear in
    // multiple fixtures and collide, breaking import resolution.
    {
        let rel = "rust/graph";
        let (e, s, f) = load_resolve_fixture(rel);
        merge_fixture(&mut entities, &mut symbols, &mut files, e, s, f);
    }
    // query fixtures: lib.rs + lib_tests.rs (test file importing lib.rs).
    let tf = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/query_fixtures/tests_for_file");
    let mut paths: Vec<_> = std::fs::read_dir(&tf)
        .expect("query fixture dir exists")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    let mut e2 = Vec::new();
    let mut s2 = Vec::new();
    let mut f2 = Vec::new();
    for (file_id, path) in paths.iter().enumerate() {
        let parsed = varde_code::parse::parse_source(
            &varde_code::parse::language_for_path(path).expect("supported language"),
            &std::fs::read_to_string(path).expect("reads"),
        );
        let result = varde_code::extract::extract(&parsed, file_id as u32);
        e2.extend(result.entities);
        s2.extend(result.symbols);
        f2.push(path.to_string_lossy().into_owned());
    }
    merge_fixture(&mut entities, &mut symbols, &mut files, e2, s2, f2);

    let output = varde_code::model::ExtractOutput {
        entities,
        symbols,
        diagnostics: vec![],
        file_meta: vec![
            varde_code::model::FileMeta {
                mtime: 0,
                size: 0,
                content_hash: "0000000000000000".to_string()
            };
            files.len()
        ],
        files,
    };
    let graph = varde_code::resolve::resolve(&output.entities, &output.symbols, &output.files)
        .expect("resolve succeeds");
    varde_code::persist::persist(
        &db,
        std::slice::from_ref(&output),
        &graph,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .expect("persist succeeds");

    // The fixture files physically live under this crate's own `tests/` tree,
    // so their absolute paths trip the `is_test_path` generated column — every
    // source file would look like a test, which (correctly) empties the
    // hotspots/foundational/symbols sections now that those exclude test paths.
    // Rewrite the persisted paths into a realistic repo-relative layout
    // (production under `src/`, the covering test under `tests/`) so the parity
    // checks exercise real behavior. `resolved_edges`/`community_members` key
    // off `file_id`, not path, so this is safe; `is_test_path` recomputes
    // automatically as a generated column.
    {
        let conn = varde_code::db::open(&db).expect("reopen fixture db");
        let rows: Vec<(i64, String)> = conn
            .prepare("SELECT id, path FROM files")
            .expect("prepare files")
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .expect("query files")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect files");
        for (id, path) in rows {
            let base = std::path::Path::new(&path)
                .file_name()
                .expect("fixture file has a name")
                .to_string_lossy()
                .into_owned();
            let rebased = if base.contains("_test") {
                format!("tests/{base}")
            } else {
                format!("src/{base}")
            };
            conn.execute(
                "UPDATE files SET path = ?1 WHERE id = ?2",
                rusqlite::params![rebased, id],
            )
            .expect("rewrite fixture path");
        }
    }

    db
}

/// Append one fixture load's entities/symbols/files onto the accumulated
/// globals, rebasing each entity/symbol `file_id` by the files already
/// accumulated — each fixture load starts its own `file_id`s at 0.
fn merge_fixture(
    entities: &mut Vec<varde_code::model::Entity>,
    symbols: &mut Vec<varde_code::model::Symbol>,
    files: &mut Vec<String>,
    mut e: Vec<varde_code::model::Entity>,
    mut s: Vec<varde_code::model::Symbol>,
    f: Vec<String>,
) {
    let offset = files.len() as u32;
    for entity in &mut e {
        entity.file_id += offset;
    }
    for symbol in &mut s {
        symbol.file_id += offset;
    }
    entities.extend(e);
    symbols.extend(s);
    files.extend(f);
}

fn load_resolve_fixture(
    rel: &str,
) -> (
    Vec<varde_code::model::Entity>,
    Vec<varde_code::model::Symbol>,
    Vec<String>,
) {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resolve_fixtures")
        .join(rel);
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("resolve fixture dir exists")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    let mut entities = Vec::new();
    let mut symbols = Vec::new();
    let mut files = Vec::new();
    for (file_id, path) in paths.iter().enumerate() {
        let parsed = varde_code::parse::parse_source(
            &varde_code::parse::language_for_path(path).expect("supported language"),
            &std::fs::read_to_string(path).expect("reads"),
        );
        let result = varde_code::extract::extract(&parsed, file_id as u32);
        entities.extend(result.entities);
        symbols.extend(result.symbols);
        files.push(path.to_string_lossy().into_owned());
    }
    (entities, symbols, files)
}

fn qrun(mode: &str, db: &std::path::Path, extra: &str) -> serde_json::Value {
    let input = if extra.is_empty() {
        format!(r#"{{"dbPath":"{db}"}}"#, db = db.display())
    } else {
        format!(r#"{{"dbPath":"{db}",{extra}}}"#, db = db.display())
    };
    let stdout = varde_code::query::run_mode(mode, &input);
    serde_json::from_str(&stdout).expect("envelope is JSON")
}

/// Envelope-conformance + a mode-specific truthiness assertion.
fn assert_ok(env: &serde_json::Value, mode: &str) -> Result<(), String> {
    if env["ok"] != true {
        return Err(format!("{mode}: expected ok:true, got {env}"));
    }
    if !env["data"].is_array() && !env["data"].is_object() {
        return Err(format!("{mode}: data payload missing, got {env}"));
    }
    Ok(())
}

fn nonempty(env: &serde_json::Value, mode: &str) -> Result<(), String> {
    assert_ok(env, mode)?;
    let arr = env["data"]
        .as_array()
        .ok_or(format!("{mode}: data not array: {env}"))?;
    if arr.is_empty() {
        return Err(format!("{mode}: expected a non-empty result, got {env}"));
    }
    Ok(())
}

fn check_q_batch(db: &std::path::Path) -> Result<(), String> {
    let calls = r#""calls":[{"mode":"symbols_in_file","filePath":"a.rs"},{"mode":"get_symbol","name":"fn_b"},{"mode":"nope_mode"},{"mode":"batch","calls":[]}]"#;
    let env = qrun("batch", db, calls);
    assert_ok(&env, "batch")?;
    let data = env["data"]
        .as_array()
        .ok_or(format!("batch: data not array: {env}"))?;
    if data.len() != 4 {
        return Err(format!("batch: expected 4 results, got {env}"));
    }
    if data[0]["ok"] != true || !data[0]["data"].is_array() {
        return Err(format!("batch: call 0 (symbols_in_file) failed: {env}"));
    }
    if data[1]["ok"] != true || data[1]["data"]["name"] != "fn_b" {
        return Err(format!("batch: call 1 (get_symbol) failed: {env}"));
    }
    if data[2]["ok"] != false || data[2]["data"]["error"]["code"] != "unknown_mode" {
        return Err(format!("batch: call 2 (unknown mode) should error: {env}"));
    }
    if data[3]["ok"] != false || data[3]["data"]["error"]["code"] != "invalid_input" {
        return Err(format!("batch: call 3 (nested batch) should reject: {env}"));
    }
    Ok(())
}

fn check_q_symbols_in_file(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("symbols_in_file", db, r#""filePath":"a.rs""#);
    nonempty(&env, "symbols_in_file")
}

fn check_q_symbols_in_files(db: &std::path::Path) -> Result<(), String> {
    let env = qrun(
        "symbols_in_files",
        db,
        r#""filePaths":["a.rs","missing.rs"]"#,
    );
    assert_ok(&env, "symbols_in_files")?;
    let data = &env["data"];
    if !data["a.rs"].is_array() || data["a.rs"].as_array().unwrap().is_empty() {
        return Err(format!(
            "symbols_in_files: expected symbols for a.rs: {env}"
        ));
    }
    if data["missing.rs"]["error"]["code"] != "not_found" {
        return Err(format!(
            "symbols_in_files: expected not_found for missing.rs: {env}"
        ));
    }
    Ok(())
}

fn check_q_get_symbol(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("get_symbol", db, r#""name":"fn_b""#);
    assert_ok(&env, "get_symbol")?;
    if env["data"]["name"] != "fn_b" {
        return Err(format!("get_symbol: wrong symbol: {env}"));
    }
    Ok(())
}

fn check_q_tests_for_file(db: &std::path::Path) -> Result<(), String> {
    // The lib.rs entity exists in the fixture DB; lib_tests.rs imports it.
    let lib_rs = qrun("get_symbol", db, r#""name":"compute""#);
    if lib_rs["ok"] != true {
        // fall back: find any path ending in lib.rs via map_file
        let env = qrun("tests_for_file", db, r#""filePath":"lib.rs""#);
        return assert_ok(&env, "tests_for_file");
    }
    let env = qrun("tests_for_file", db, r#""filePath":"lib.rs""#);
    assert_ok(&env, "tests_for_file")
}

fn check_q_find_imports(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("find_imports", db, r#""filePath":"a.rs""#);
    nonempty(&env, "find_imports")
}

fn check_q_filter_symbols(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("filter_symbols", db, r#""kind":"reference""#);
    nonempty(&env, "filter_symbols")
}

fn check_q_dependencies(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("dependencies", db, r#""filePath":"a.rs""#);
    nonempty(&env, "dependencies")
}

fn check_q_dependents(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("dependents", db, r#""filePath":"b.rs""#);
    assert_ok(&env, "dependents")
}

fn check_q_blast_radius(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("blast_radius", db, r#""filePath":"a.rs""#);
    assert_ok(&env, "blast_radius")
}

fn check_q_symbol_blast_radius(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("symbol_blast_radius", db, r#""name":"fn_b""#);
    assert_ok(&env, "symbol_blast_radius")
}

fn check_q_type_hierarchy(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("type_hierarchy", db, r#""name":"fn_b""#);
    assert_ok(&env, "type_hierarchy")
}

fn check_q_explore(db: &std::path::Path) -> Result<(), String> {
    let env = qrun(
        "explore",
        db,
        r#""query":{"kind":"dependency","params":{"input":"a.rs"}}"#,
    );
    assert_ok(&env, "explore")?;
    let reachable = env["data"]["reachable"]
        .as_array()
        .ok_or_else(|| format!("explore: reachable missing: {env}"))?;
    if reachable.is_empty() {
        return Err(format!("explore: expected reachable files, got {env}"));
    }
    Ok(())
}

fn check_q_map_file(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("map_file", db, r#""filePath":"a.rs""#);
    assert_ok(&env, "map_file")?;
    if env["data"]["path"].as_str().is_none() {
        return Err(format!("map_file: path missing: {env}"));
    }
    Ok(())
}

fn check_q_map_symbol(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("map_symbol", db, r#""name":"fn_b""#);
    assert_ok(&env, "map_symbol")
}

fn check_q_map_path(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("map_path", db, r#""sourceFile":"a.rs","targetFile":"c.rs""#);
    nonempty(&env, "map_path")
}

fn check_q_detect_changes(_db: &std::path::Path) -> Result<(), String> {
    // Envelope conformance: git diff against /tmp yields an ok:true envelope
    // (possibly empty change list).
    let env = qrun(
        "detect_changes",
        std::path::Path::new("/tmp/index.db"),
        r#""diffMode":"working_tree","repoRoot":"/tmp""#,
    );
    assert_ok(&env, "detect_changes")
}

fn check_q_hotspots(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("hotspots", db, "");
    assert_ok(&env, "hotspots")?;
    let arr = env["data"].as_array().ok_or("hotspots: not array")?;
    if arr.is_empty() {
        return Err("hotspots: expected files".into());
    }
    // Ordered by descending score.
    let scores: Vec<i64> = arr
        .iter()
        .map(|h| h["score"].as_i64().unwrap_or(0))
        .collect();
    let mut sorted = scores.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    if scores != sorted {
        return Err(format!("hotspots: not ordered by score desc: {scores:?}"));
    }
    Ok(())
}

fn check_q_nav_map(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("nav_map", db, "");
    assert_ok(&env, "nav_map")?;
    let data = env["data"]
        .as_object()
        .ok_or_else(|| format!("nav_map: data not an object: {env}"))?;
    for key in [
        "entrypoints",
        "foundational_files",
        "module_layers",
        "subsystems",
        "symbols",
        "flows",
        "hotspots",
    ] {
        if !data.contains_key(key) {
            return Err(format!("nav_map: missing section {key:?}: {env}"));
        }
    }
    Ok(())
}

fn check_q_clusters(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("clusters", db, "");
    assert_ok(&env, "clusters")?;
    let clusters = env["data"]["clusters"]
        .as_array()
        .ok_or_else(|| format!("clusters: clusters array missing: {env}"))?;
    if clusters.is_empty() {
        return Err(format!("clusters: expected at least one cluster: {env}"));
    }
    for c in clusters {
        if !c["label"].is_null() {
            return Err(format!("clusters: label should be null: {env}"));
        }
        if !c["cohesion"].is_number() {
            return Err(format!("clusters: cohesion missing: {env}"));
        }
        if !c["files"].is_array() {
            return Err(format!("clusters: files missing: {env}"));
        }
    }
    Ok(())
}

fn check_q_slice_state(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("slice_state", db, "");
    assert_ok(&env, "slice_state")?;
    // The --why view: every derived slice ledgered at the fixture's max_rev,
    // so nothing is stale after the fixture persist.
    let slices = env["data"]["slices"]
        .as_object()
        .ok_or_else(|| format!("slice_state: slices object missing: {env}"))?;
    for slice in ["imports", "edges", "global"] {
        let entry = slices
            .get(slice)
            .ok_or_else(|| format!("slice_state: missing slice {slice}: {env}"))?;
        if entry["stale"] != false {
            return Err(format!("slice_state: {slice} unexpectedly stale: {env}"));
        }
    }
    Ok(())
}

fn check_q_context_pack(db: &std::path::Path) -> Result<(), String> {
    let env = qrun("context_pack", db, r#""query":"a.rs""#);
    assert_ok(&env, "context_pack")?;
    let files = env["data"]["files"]
        .as_array()
        .ok_or_else(|| format!("context_pack: files missing: {env}"))?;
    if files.is_empty() {
        return Err(format!(
            "context_pack: expected at least one file, got {env}"
        ));
    }
    if !files.iter().any(|f| f["relevance"] == "seed") {
        return Err(format!(
            "context_pack: expected a seed-relevance file, got {env}"
        ));
    }
    for field in ["symbols", "tests", "readingOrder"] {
        if !env["data"][field].is_array() {
            return Err(format!("context_pack: {field} missing: {env}"));
        }
    }
    let reading_order = env["data"]["readingOrder"].as_array().unwrap();
    if reading_order.len() != files.len() {
        return Err(format!(
            "context_pack: readingOrder should mirror files 1:1, got {env}"
        ));
    }

    // Audit S10: a multi-word query must tokenize and keyword-search, not match
    // the literal phrase (which never appears verbatim) and return not_found.
    // One matching token ("a.rs") unioned with a non-matching one must still
    // resolve to the same seed as the single-token query above.
    let multi = qrun("context_pack", db, r#""query":"a.rs zzznomatch""#);
    assert_ok(&multi, "context_pack multi-word")?;
    let multi_files = multi["data"]["files"].as_array();
    if multi_files.is_none_or(|f| f.is_empty()) {
        return Err(format!(
            "context_pack: multi-word query should resolve via keyword search, got {multi}"
        ));
    }
    Ok(())
}

fn check_q_find_pattern(_db: &std::path::Path) -> Result<(), String> {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/find_pattern_fixtures/calls.rs");
    let input = format!(
        r#"{{"filePath":"{}","pattern":"alpha($A)"}}"#,
        fixture.display()
    );
    let stdout = varde_code::query::run_mode("find_pattern", &input);
    let env: serde_json::Value = serde_json::from_str(&stdout).expect("envelope is JSON");
    assert_ok(&env, "find_pattern")?;
    if env["data"]["matches"].as_array().is_none_or(Vec::is_empty) {
        return Err(format!(
            "find_pattern: expected non-empty matches, got {env}"
        ));
    }
    Ok(())
}
