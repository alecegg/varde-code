//! TS grammar loading + parse tests (task: treesitter-parse-integration-ts).

mod common;

use ast_grep_language::SupportLang;
use varde_code::parse::{language_for_path, parse_source};

#[test]
fn parse_ts_valid_fixture_loads_grammar_and_parses() {
    let src = common::fixture("ts/valid.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(
        !parsed.has_error(),
        "valid fixture must parse without errors"
    );
    assert_eq!(parsed.root.root().kind(), "program");
}

#[test]
fn parse_ts_invalid_fixture_reports_error_without_panicking() {
    let src = common::fixture("ts/invalid.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(
        parsed.has_error(),
        "invalid fixture must be reported as an error"
    );
}

#[test]
fn language_for_path_recognizes_ts_extensions() {
    assert_eq!(
        language_for_path(std::path::Path::new("a/b.ts")).unwrap(),
        SupportLang::TypeScript
    );
    assert_eq!(
        language_for_path(std::path::Path::new("a/b.mts")).unwrap(),
        SupportLang::TypeScript
    );
    assert!(language_for_path(std::path::Path::new("a/b.txt")).is_none());
}
