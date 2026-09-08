//! Public output-shape tests for compact, navigable tool results.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_varde-code");
const RESULT_LIMIT: usize = 100;

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "varde-output-compaction-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp directory creates");
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("binary runs")
}

fn envelope(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout must be JSON: {err}; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn assert_truncated(data: &serde_json::Value, list: &str, total: usize) {
    let truncation = &data["guide"]["truncated"][list];
    assert_eq!(truncation["shown"], RESULT_LIMIT);
    assert_eq!(truncation["total"], total);
}

#[test]
fn extract_uses_file_table_instead_of_repeating_paths() {
    let out = run(&["extract", "tests/fixtures/ts/mixed"]);
    assert!(out.status.success(), "extract must succeed: {out:?}");
    let payload = envelope(&out);
    let data = &payload["data"];

    assert!(
        data["files"]
            .as_array()
            .is_some_and(|files| !files.is_empty())
    );
    for key in ["entities", "symbols", "diagnostics"] {
        for item in data[key].as_array().expect("output list") {
            assert!(item["file_id"].as_u64().is_some(), "{key}: {item}");
            assert!(item.get("file").is_none(), "{key}: repeated path in {item}");
        }
    }
}

#[test]
fn find_pattern_bounds_matches_and_keeps_navigation_details() {
    let dir = tempdir("find-pattern");
    let file = dir.join("many.rs");
    let source = (0..=RESULT_LIMIT)
        .map(|index| format!("alpha({index});"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&file, source).expect("fixture writes");
    let input = format!(
        r#"{{"filePath":"{}","pattern":"alpha($A)"}}"#,
        file.display()
    );

    let out = run(&["find_pattern", "--json", &input]);
    assert!(out.status.success(), "find_pattern must succeed: {out:?}");
    let payload = envelope(&out);
    assert_eq!(payload["meta"]["truncated"], true);
    let data = &payload["data"];
    let matches = data["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), RESULT_LIMIT);
    assert_truncated(data, "matches", RESULT_LIMIT + 1);
    assert!(matches.iter().all(|item| {
        item["file"].as_str().is_some()
            && item["span"]["start_line"].as_u64().is_some()
            && item["span"]["end_line"].as_u64().is_some()
    }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn batch_reports_nested_find_pattern_truncation() {
    let dir = tempdir("batch-find-pattern");
    let file = dir.join("many.rs");
    let source = (0..=RESULT_LIMIT)
        .map(|index| format!("alpha({index});"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&file, source).expect("fixture writes");
    let input = format!(
        r#"{{"calls":[{{"mode":"find_pattern","filePath":"{}","pattern":"alpha($A)"}}]}}"#,
        file.display()
    );

    let out = run(&["batch", "--json", &input]);
    assert!(out.status.success(), "batch must succeed: {out:?}");
    let payload = envelope(&out);
    assert_eq!(payload["meta"]["truncated"], true);
    assert_eq!(payload["data"][0]["meta"]["truncated"], true);
    assert_truncated(&payload["data"][0]["data"], "matches", RESULT_LIMIT + 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_bounds_results_and_keeps_rule_and_file_handles() {
    let dir = tempdir("rule-tests");
    let mut pack = String::from(
        r#"
[[rule]]
id = "many-tests"
kind = "pattern"
severity = "warning"
message = "console.log detected"
pattern = "console.log($MSG)"
"#,
    );
    for index in 0..=RESULT_LIMIT {
        writeln!(
            pack,
            r#"
[[rule.test]]
name = "case {index}"
invalid = ["console.log(\"{index}\");"]
"#,
        )
        .expect("test entry writes");
    }
    std::fs::write(dir.join("pack.toml"), pack).expect("rule pack writes");
    let input = format!(r#"{{"rulesDir":"{}"}}"#, dir.display());

    let out = run(&["test", "--json", &input]);
    let payload = envelope(&out);
    assert_eq!(payload["ok"], true, "test payload: {payload}");
    assert_eq!(payload["meta"]["truncated"], true);
    let data = &payload["data"];
    let results = data["results"].as_array().expect("results array");
    assert_eq!(results.len(), RESULT_LIMIT);
    assert_truncated(data, "results", RESULT_LIMIT + 1);
    assert!(results.iter().all(|result| {
        result["rule_id"].as_str().is_some()
            && result["test_name"].as_str().is_some()
            && result["file"].as_str().is_some()
    }));

    let _ = std::fs::remove_dir_all(&dir);
}
