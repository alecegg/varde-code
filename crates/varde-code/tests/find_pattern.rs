//! Integration tests for the hand-rolled find_pattern matcher: `$VAR`
//! single-node capture, `$$$VAR` variadic capture, and no-match semantics.

use std::path::{Path, PathBuf};

use varde_code::query;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/find_pattern_fixtures/calls.rs")
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/find_pattern_fixtures")
}

fn run_file(file: &Path, pattern: &str, extra: &str) -> serde_json::Value {
    let input = format!(
        r#"{{"filePath":"{}","pattern":"{}"{extra}}}"#,
        file.display(),
        pattern
    );
    let stdout = query::run_mode("find_pattern", &input);
    serde_json::from_str(&stdout).expect("envelope is JSON")
}

fn run(pattern: &str, extra: &str) -> serde_json::Value {
    let file = fixture();
    let input = format!(
        r#"{{"filePath":"{}","pattern":"{}"{extra}}}"#,
        file.display(),
        pattern
    );
    let stdout = query::run_mode("find_pattern", &input);
    serde_json::from_str(&stdout).expect("envelope is JSON")
}

fn matches_of(env: &serde_json::Value) -> Vec<String> {
    env["data"]["matches"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn single_node_capture_matches_all_calls_with_one_arg() {
    let env = run("alpha($A)", "");
    assert_eq!(env["ok"], true, "{env}");
    let texts = matches_of(&env);
    assert_eq!(texts, vec!["alpha(1)", "alpha(2)"], "{env}");

    // Capture binding: $A holds the single argument node.
    let captures = &env["data"]["matches"][0]["captures"];
    assert_eq!(captures["A"]["kind"], "integer_literal", "{env}");
    assert_eq!(captures["A"]["text"], "1", "{env}");
}

#[test]
fn variadic_capture_matches_any_argument_count() {
    let env = run("alpha($$$ARGS)", "");
    assert_eq!(env["ok"], true, "{env}");
    let texts = matches_of(&env);
    // alpha(1), alpha(2), alpha(3, 4) — three calls with 0..N args.
    assert_eq!(texts.len(), 3, "{env}");
    assert!(texts.contains(&"alpha(3, 4)".to_string()));

    // The variadic capture binds an array of argument nodes.
    let three_four = env["data"]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["text"] == "alpha(3, 4)")
        .expect("match exists");
    // Trivia (the `,` separator) is stripped from variadic captures, same
    // as ast-grep's default "Smart" match strictness — $$$ARGS binds only
    // the argument nodes, not the punctuation between them.
    let args = three_four["captures"]["ARGS"]
        .as_array()
        .expect("array capture");
    assert_eq!(
        args.len(),
        2,
        "integer, integer (comma stripped as trivia): {env}"
    );
    assert_eq!(args[0]["text"], "3");
    assert_eq!(args[1]["text"], "4");
}

#[test]
fn no_match_is_empty_not_error() {
    let env = run("gamma($A)", "");
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["data"], serde_json::json!({ "matches": [] }));
}

#[test]
fn callee_name_is_respected() {
    // beta(1) must NOT match the alpha pattern.
    let env = run("alpha($A)", "");
    let texts = matches_of(&env);
    assert!(!texts.iter().any(|t| t.starts_with("beta")), "{texts:?}");
}

#[test]
fn invalid_pattern_is_an_error_not_a_panic() {
    let env = run("fn main( $A", "");
    assert_eq!(env["ok"], false, "{env}");
    assert_eq!(env["data"]["error"]["code"], "invalid_pattern");
}

#[test]
fn unknown_file_is_an_error() {
    let input = r#"{"filePath":"/nonexistent/ghost.rs","pattern":"alpha($A)"}"#.to_string();
    let stdout = query::run_mode("find_pattern", &input);
    let env: serde_json::Value = serde_json::from_str(&stdout).expect("envelope is JSON");
    assert_eq!(env["ok"], false);
    assert_eq!(env["data"]["error"]["code"], "file_error");
}

// find_pattern searches every language `ast-grep` links, not just the
// extraction-supported subset — a linked grammar is all structural search
// needs. Ruby and C++ are not extraction languages.

#[test]
fn non_extraction_language_matches_via_extension_inference() {
    // No explicit `language`: inferred from the `.rb` extension.
    let env = run_file(&fixtures_dir().join("calls.rb"), "alpha($A)", "");
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(matches_of(&env), vec!["alpha(1)", "alpha(2)"], "{env}");
}

#[test]
fn c_family_bare_fragment_wraps_and_matches() {
    // A bare call fragment isn't valid standalone C++ (nor a valid statement
    // without a terminator); it must be wrapped in a function body with a `;`
    // to parse. Also exercises an `ast-grep` alias (`c++` → Cpp).
    let env = run_file(
        &fixtures_dir().join("calls.cpp"),
        "alpha($A)",
        r#","language":"c++""#,
    );
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(matches_of(&env), vec!["alpha(1)", "alpha(2)"], "{env}");
}

#[test]
fn directory_walk_filters_to_the_requested_language() {
    // The fixtures dir holds calls.{rs,rb,cpp}; a Ruby directory search (via
    // `path`, not `filePath`) must walk only the `.rb` file and ignore the
    // others — proving the walk's per-file language filter spans the full set.
    let input = format!(
        r#"{{"path":"{}","pattern":"alpha($A)","language":"ruby"}}"#,
        fixtures_dir().display()
    );
    let env: serde_json::Value =
        serde_json::from_str(&query::run_mode("find_pattern", &input)).expect("envelope is JSON");
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(matches_of(&env), vec!["alpha(1)", "alpha(2)"], "{env}");
}

#[test]
fn unsupported_language_name_is_an_error() {
    let env = run_file(
        &fixtures_dir().join("calls.rb"),
        "alpha($A)",
        r#","language":"cobol""#,
    );
    assert_eq!(env["ok"], false, "{env}");
    assert_eq!(env["data"]["error"]["code"], "invalid_input", "{env}");
}
