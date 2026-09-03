//! Unparseable-file error-handling tests (task: unparseable-file-error-handling).
//! Skip + report per file, continue the run, exit 0 unless a fatal error.

mod common;

use ast_grep_language::SupportLang;
use varde_code::model::EntityKind;
use varde_code::parse::parse_source;
use varde_code::scan;

const MIXED_DIR: &str = "tests/fixtures/ts/mixed";

#[test]
fn unparseable_files_skip_and_report_keep_valid_entities() {
    let output = scan::run(MIXED_DIR).expect("mixed dir scan succeeds");

    // The valid file's entities must appear in the output.
    let hello = output
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Function && e.name == "hello")
        .expect("valid.ts function entity present");
    assert!(output.files[hello.file_id as usize].contains("valid.ts"));

    // A diagnostic entry must exist for each bad file.
    let diag_files: Vec<&str> = output
        .diagnostics
        .iter()
        .map(|d| output.files[d.file_id as usize].as_str())
        .collect();
    for bad in ["broken.ts", "binary.ts", "blob.bin"] {
        assert!(
            diag_files.iter().any(|f| f.contains(bad)),
            "missing diagnostic for {bad}; got {diag_files:?}"
        );
    }
}

#[test]
fn unparseable_files_mixed_dir_exits_zero() {
    let bin = env!("CARGO_BIN_EXE_varde-code");
    let out = std::process::Command::new(bin)
        .args(["extract", MIXED_DIR])
        .output()
        .expect("run extract");
    assert!(
        out.status.success(),
        "mixed dir must exit 0 despite per-file errors: {:?}",
        out.status
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(value["diagnostics"].as_array().unwrap().len(), 3);
    assert!(!value["entities"].as_array().unwrap().is_empty());
}

#[test]
fn unparseable_files_missing_root_is_fatal_nonzero() {
    let bin = env!("CARGO_BIN_EXE_varde-code");
    let out = std::process::Command::new(bin)
        .args(["extract", "tests/fixtures/ts/does-not-exist-xyz"])
        .output()
        .expect("run extract");
    assert!(
        !out.status.success(),
        "missing root must be fatal with non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.trim().is_empty(),
        "fatal error must be reported on stderr"
    );
}

/// Sanity: the fixture files in the mixed dir parse/extract as expected.
#[test]
fn unparseable_files_fixture_sanity() {
    let src = common::fixture("ts/mixed/valid.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(!parsed.has_error());

    let broken = common::fixture("ts/mixed/broken.ts");
    let parsed_broken = parse_source(&SupportLang::TypeScript, &broken);
    assert!(parsed_broken.has_error());
}
