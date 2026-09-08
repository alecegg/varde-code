//! Integration tests for the `extract` CLI JSON contract.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_varde-code"))
}

#[test]
fn extract_existing_path_prints_well_formed_json() {
    let out = bin().args(["extract", "."]).output().expect("run extract");
    assert!(out.status.success(), "exit code must be 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Well-formed JSON that jq can parse.
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout parses as JSON");
    assert!(value.is_object());
    assert_eq!(value["ok"], true);
    assert!(value["data"]["entities"].is_array());
    assert!(value["data"]["symbols"].is_array());
    assert!(value["data"]["diagnostics"].is_array());
    assert!(value["meta"].is_object());
}

#[test]
fn extract_non_source_file_reports_info_diagnostic() {
    // A non-source file produces no entities/symbols and an info diagnostic,
    // with the run still exiting 0.
    let out = bin()
        .args(["extract", "./Cargo.toml"])
        .output()
        .expect("run extract");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["entities"].as_array().unwrap().len(), 0);
    assert_eq!(value["data"]["symbols"].as_array().unwrap().len(), 0);
    let diags = value["data"]["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["severity"], "info");
    assert_eq!(diags[0]["file_id"], 0);
    assert_eq!(value["data"]["files"][0], "Cargo.toml");
}
