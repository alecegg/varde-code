//! Pattern-rule `[[test]]` execution: runs a rule's declared `valid`,
//! `invalid`, and `expect_rewrite` fixtures through the same matching
//! pipeline `scan`'s `run_pattern_rules` uses, and reports pass/fail per
//! `[[test]]` entry.
//!
//! Matching itself is never reimplemented here: every snippet goes through
//! `query::find_pattern::find_pattern` (the same matcher `scan` calls) and,
//! when the rule declares `constraints`, `pattern::apply_constraints` (the
//! same per-capture regex filter `run_pattern_rules` applies before
//! counting). `expect_rewrite` checks call `rewrite::substitute` (the exact
//! function `scan --apply` uses) against the match's captures.

use crate::query::ApiError;
use crate::rules::{Rule, RuleKind, pattern, rewrite};
use std::sync::atomic::{AtomicU64, Ordering};

/// Outcome of one `[[test]]` entry. `detail` is populated only on failure
/// and literally contains both the expected and actual values (snippet
/// text / expected output vs. match count / actual rewritten output) so a
/// caller can embed it verbatim with no reformatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    pub rule_id: String,
    pub test_name: String,
    pub pass: bool,
    pub detail: Option<String>,
}

fn fail(rule: &Rule, name: &str, detail: String) -> TestResult {
    TestResult {
        rule_id: rule.id.clone(),
        test_name: name.to_string(),
        pass: false,
        detail: Some(detail),
    }
}

fn pass(rule: &Rule, name: &str) -> TestResult {
    TestResult {
        rule_id: rule.id.clone(),
        test_name: name.to_string(),
        pass: true,
        detail: None,
    }
}

/// Run every `[[test]]` entry declared on a `kind = "pattern"` rule.
///
/// Rules without a `pattern` field, or with `kind != Pattern`, or with no
/// `test` entries produce an empty result list — this function is inert for
/// anything outside its scope, matching `run_pattern_rules`' own
/// kind-filtering convention.
pub fn run_pattern_rule_tests(rule: &Rule) -> Vec<TestResult> {
    if rule.kind != RuleKind::Pattern {
        return Vec::new();
    }
    let Some(cases) = rule.test.as_ref() else {
        return Vec::new();
    };
    cases.iter().map(|case| run_one(rule, case)).collect()
}

fn run_one(rule: &Rule, case: &crate::rules::TestCase) -> TestResult {
    if let Some(valid) = case.valid.as_ref() {
        for snippet in valid {
            match match_snippet(rule, snippet) {
                Ok(matches) if !matches.is_empty() => {
                    return fail(
                        rule,
                        &case.name,
                        format!(
                            "valid snippet unexpectedly matched:\nsnippet: {snippet}\nexpected match count: 0\nactual match count: {}",
                            matches.len()
                        ),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    return fail(
                        rule,
                        &case.name,
                        format!("valid snippet {snippet} failed to evaluate: {}", e.message),
                    );
                }
            }
        }
    }

    if let Some(invalid) = case.invalid.as_ref() {
        for snippet in invalid {
            match match_snippet(rule, snippet) {
                Ok(matches) if matches.is_empty() => {
                    return fail(
                        rule,
                        &case.name,
                        format!(
                            "invalid snippet unexpectedly had zero matches:\nsnippet: {snippet}\nexpected match count: >=1\nactual match count: 0"
                        ),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    return fail(
                        rule,
                        &case.name,
                        format!(
                            "invalid snippet {snippet} failed to evaluate: {}",
                            e.message
                        ),
                    );
                }
            }
        }
    }

    if let Some(expect_rewrite) = case.expect_rewrite.as_ref() {
        let Some(template) = rule.rewrite.as_deref() else {
            return fail(
                rule,
                &case.name,
                "expect_rewrite entries declared but rule has no `rewrite` field".to_string(),
            );
        };
        for (snippet, expected) in expect_rewrite {
            let matches = match match_snippet(rule, snippet) {
                Ok(matches) => matches,
                Err(e) => {
                    return fail(
                        rule,
                        &case.name,
                        format!(
                            "expect_rewrite snippet {snippet} failed to evaluate: {}",
                            e.message
                        ),
                    );
                }
            };
            let Some(m) = matches.first() else {
                return fail(
                    rule,
                    &case.name,
                    format!(
                        "expect_rewrite snippet did not match:\nsnippet: {snippet}\nexpected output: {expected}\nactual: no match"
                    ),
                );
            };
            let captures = m
                .get("captures")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let actual = rewrite::substitute(template, &captures);
            if &actual != expected {
                return fail(
                    rule,
                    &case.name,
                    format!(
                        "expect_rewrite output mismatch:\nsnippet: {snippet}\nexpected output: {expected}\nactual output: {actual}"
                    ),
                );
            }
        }
    }

    pass(rule, &case.name)
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Match a test snippet against `rule.pattern`, applying the rule's
/// `constraints` (if any) exactly as `run_pattern_rule` does before
/// counting. Tries every language the rule targets (`rule.languages`, or
/// every supported language when the rule is language-agnostic), skipping
/// languages the snippet doesn't parse in — the same skip-and-continue
/// behavior `run_pattern_rule` applies to per-language pattern-parse
/// failures.
fn match_snippet(rule: &Rule, snippet: &str) -> Result<Vec<serde_json::Value>, ApiError> {
    let Some(pattern_text) = rule.pattern.as_deref() else {
        return Err(ApiError::new(
            "invalid_input",
            "rule kind=pattern is missing its `pattern` field",
        ));
    };

    let langs: Vec<ast_grep_language::SupportLang> = match &rule.languages {
        Some(names) if !names.is_empty() => names
            .iter()
            .filter_map(|n| crate::parse::language_from_name(n))
            .collect(),
        _ => crate::parse::SUPPORTED_LANGUAGES.to_vec(),
    };

    let mut all_matches = Vec::new();
    for lang in langs {
        let path = write_snippet(snippet)?;
        let input = serde_json::json!({
            "pattern": pattern_text,
            "filePath": path.display().to_string(),
            "language": crate::parse::language_name(&lang),
        });
        let result = crate::query::find_pattern::find_pattern(&input);
        let _ = std::fs::remove_file(&path);
        let matches = match result {
            Ok(v) => v["matches"].as_array().cloned().unwrap_or_default(),
            // A snippet that doesn't parse (or a pattern that doesn't parse)
            // in this candidate language is expected for language-agnostic
            // rules tried against every supported language — skip, don't
            // fail the whole check.
            Err(_) => continue,
        };
        let kept = match rule.constraints.as_ref() {
            Some(constraints) if !constraints.is_empty() => {
                pattern::apply_constraints(&matches, constraints)?
            }
            _ => matches,
        };
        all_matches.extend(kept);
    }
    Ok(all_matches)
}

fn write_snippet(snippet: &str) -> Result<std::path::PathBuf, ApiError> {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("varde-rule-test-{}-{n}.tmp", std::process::id()));
    std::fs::write(&path, snippet)
        .map_err(|e| ApiError::new("file_error", format!("failed to write test snippet: {e}")))?;
    Ok(path)
}

/// Run every `[[test]]` entry declared on a `kind = "sql"` rule.
///
/// Each entry's `fixture` is an inline map of relative path → file content;
/// the entries are written into a fresh, uniquely-named temp directory,
/// indexed through the real pipeline (`build::run_with_force`, the same
/// entry point `scan_repo` uses before opening its read-only connection),
/// then the rule's `query` is run via `sql::run_sql_rules` — reusing its
/// exact `:key` param-binding logic rather than reimplementing it. The
/// resulting rows (built from each finding's `location`/`evidence`, the
/// same representation `run_sql_rules` exposes) are compared against
/// `expect_rows` as an order-insensitive multiset: every expected row must
/// match some actual row and vice versa. The temp source tree and its
/// derived DB directory are removed on completion, including when this
/// function returns early via `?`/`return` — `TempFixture`'s `Drop` owns
/// cleanup so a panic mid-test does not leak either directory.
///
/// Rules without `kind = "sql"`, or with no `test` entries, produce an
/// empty result list — mirrors `run_pattern_rule_tests`' inertness
/// convention for out-of-scope rules.
pub fn run_sql_rule_tests(rule: &Rule) -> Vec<TestResult> {
    if rule.kind != RuleKind::Sql {
        return Vec::new();
    }
    let Some(cases) = rule.test.as_ref() else {
        return Vec::new();
    };
    cases.iter().map(|case| run_sql_one(rule, case)).collect()
}

/// One actual query-result row, flattened to file/line plus every evidence
/// column — the same shape `run_sql_rules` exposes via `Finding.location`
/// and `Finding.evidence`, so expected rows in `[[test]]` TOML use the same
/// column names a real scan would report.
fn finding_to_row(
    f: &crate::rules::finding::Finding,
) -> std::collections::HashMap<String, serde_json::Value> {
    let mut row = std::collections::HashMap::new();
    row.insert(
        "file".to_string(),
        serde_json::Value::String(f.location.file.clone()),
    );
    row.insert(
        "line".to_string(),
        serde_json::json!(f.location.span.start_line),
    );
    if let serde_json::Value::Object(evidence) = &f.evidence {
        for (k, v) in evidence {
            row.insert(k.clone(), v.clone());
        }
    }
    row
}

/// Multiset-compare `expected` rows against `actual` rows: every expected
/// row must match some not-yet-consumed actual row and vice versa. A "match"
/// is exact key/value equality. On mismatch, returns a clear failure detail
/// naming missing/extra rows; malformed expected rows (keys that don't
/// appear on any actual row) are called out explicitly.
fn compare_rows_multiset(
    expected: &[std::collections::HashMap<String, serde_json::Value>],
    actual: &[std::collections::HashMap<String, serde_json::Value>],
) -> Result<(), String> {
    let mut remaining_actual: Vec<&std::collections::HashMap<String, serde_json::Value>> =
        actual.iter().collect();
    let mut missing: Vec<&std::collections::HashMap<String, serde_json::Value>> = Vec::new();

    for exp in expected {
        if let Some(pos) = remaining_actual.iter().position(|act| *act == exp) {
            remaining_actual.remove(pos);
        } else {
            missing.push(exp);
        }
    }

    if missing.is_empty() && remaining_actual.is_empty() {
        return Ok(());
    }

    let actual_columns: std::collections::HashSet<&String> =
        actual.iter().flat_map(|row| row.keys()).collect();
    let mut detail = format!(
        "row mismatch (order-insensitive multiset comparison):\nexpected rows: {expected:?}\nactual rows: {actual:?}"
    );
    if !missing.is_empty() {
        detail.push_str(&format!(
            "\nmissing expected rows (no matching actual row): {missing:?}"
        ));
        for row in &missing {
            let unknown_keys: Vec<&String> =
                row.keys().filter(|k| !actual_columns.contains(k)).collect();
            if !unknown_keys.is_empty() {
                detail.push_str(&format!(
                    "\nexpected row has keys not present in any actual query result column: {unknown_keys:?} (available columns: {actual_columns:?})"
                ));
            }
        }
    }
    if !remaining_actual.is_empty() {
        detail.push_str(&format!(
            "\nunexpected extra actual rows: {remaining_actual:?}"
        ));
    }
    Err(detail)
}

fn run_sql_one(rule: &Rule, case: &crate::rules::TestCase) -> TestResult {
    // `run_with_force` -> `db::path::repo_db_path` reads the process-global
    // `HOME` env var, and other tests in this binary temporarily redirect
    // `HOME` to their own temp dir and then delete it on the way out. Without
    // this lock, a fixture built here could get its db path computed against
    // another test's `HOME` mid-swap, then have that directory vanish out
    // from under `run_with_force`'s `create_dir_all`/rename — surfacing as a
    // flaky "No such file or directory" only under full-workspace concurrent
    // test runs. Hold the same lock every other HOME-mutating test uses for
    // the full build+read so this fixture's `HOME` reads stay stable.
    let _home_guard = crate::HOME_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let Some(fixture) = case.fixture.as_ref() else {
        return fail(
            rule,
            &case.name,
            "sql test case is missing its `fixture` table".to_string(),
        );
    };
    let Some(expected) = case.expect_rows.as_ref() else {
        return fail(
            rule,
            &case.name,
            "sql test case is missing its `expect_rows` rows".to_string(),
        );
    };

    let guard = match TempFixture::build(fixture) {
        Ok(guard) => guard,
        Err(e) => {
            return fail(
                rule,
                &case.name,
                format!("failed to prepare fixture temp directory: {e}"),
            );
        }
    };

    let repo_root = guard.repo_root.to_string_lossy().into_owned();
    if let Err(e) = crate::build::run_with_force(&repo_root, true) {
        return fail(rule, &case.name, format!("indexing fixture failed: {e}"));
    }

    let db_path = crate::db::path::repo_db_path(&guard.repo_root);
    let conn = match crate::db::open_read_only(&db_path) {
        Ok(conn) => conn,
        Err(e) => {
            return fail(
                rule,
                &case.name,
                format!("opening fixture database failed: {e}"),
            );
        }
    };

    let (findings, diagnostics) =
        match crate::rules::sql::run_sql_rules(std::slice::from_ref(rule), &conn) {
            Ok(result) => result,
            Err(e) => {
                return fail(
                    rule,
                    &case.name,
                    format!("running rule query failed: {}", e.message),
                );
            }
        };
    if let Some(diag) = diagnostics
        .into_iter()
        .find(|d| d.rule_id.as_deref() == Some(rule.id.as_str()))
    {
        return fail(
            rule,
            &case.name,
            format!("query execution failed: {}", diag.reason),
        );
    }

    let actual_rows: Vec<std::collections::HashMap<String, serde_json::Value>> =
        findings.iter().map(finding_to_row).collect();

    if let Err(detail) = compare_rows_multiset(expected, &actual_rows) {
        return fail(rule, &case.name, detail);
    }
    pass(rule, &case.name)
}

/// A fresh, uniquely-named temp directory populated from a fixture's
/// relative-path → file-content map, plus RAII cleanup of both that
/// directory and the derived index DB directory (`db::path::repo_db_path`'s
/// parent) once indexed. `Drop` removes both unconditionally so a panic
/// mid-test, an early `return`, or a normal fall-through all leave no state
/// behind — matching the concurrent-invocation requirement that no two test
/// entries ever share a path (each `TempFixture` allocates its own
/// counter-suffixed directory name).
struct TempFixture {
    repo_root: std::path::PathBuf,
}

static SQL_FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

impl TempFixture {
    /// Write `files` (relative path → content) into a fresh temp directory.
    fn build(files: &std::collections::HashMap<String, String>) -> std::io::Result<TempFixture> {
        let n = SQL_FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let repo_root = std::env::temp_dir().join(format!(
            "varde-sql-test-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&repo_root)?;
        for (rel_path, contents) in files {
            let dest = repo_root.join(rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, contents)?;
        }
        Ok(TempFixture { repo_root })
    }
}

impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.repo_root);
        let db_path = crate::db::path::repo_db_path(&self.repo_root);
        if let Some(db_dir) = db_path.parent() {
            let _ = std::fs::remove_dir_all(db_dir);
        }
    }
}

#[cfg(test)]
mod pattern_tests {
    use super::*;
    use crate::rules::TestCase;
    use std::collections::HashMap;

    fn base_rule(pattern: &str) -> Rule {
        Rule {
            id: "no-console-log".to_string(),
            kind: RuleKind::Pattern,
            severity: crate::rules::Severity::Warning,
            message: "no console.log".to_string(),
            name: None,
            description: None,
            remediation: None,
            pattern: Some(pattern.to_string()),
            query: None,
            thresholds: None,
            strings: None,
            constraints: None,
            fix: None,
            rewrite: None,
            languages: Some(vec!["javascript".to_string()]),
            exclude_test_paths: None,
            exclude_tooling_paths: None,
            test: None,
        }
    }

    #[test]
    fn valid_snippet_that_does_not_match_passes() {
        let rule = base_rule("console.log($MSG)");
        let case = TestCase {
            name: "no console.log in clean code".to_string(),
            valid: Some(vec!["console.info(\"hi\");".to_string()]),
            invalid: None,
            expect_rewrite: None,
            fixture: None,
            expect_rows: None,
        };
        let result = run_one(&rule, &case);
        assert!(result.pass, "expected pass, got: {:?}", result.detail);
        assert_eq!(result.rule_id, "no-console-log");
        assert_eq!(result.test_name, "no console.log in clean code");
        assert!(result.detail.is_none());
    }

    #[test]
    fn valid_snippet_that_matches_fails_with_detail() {
        let rule = base_rule("console.log($MSG)");
        let case = TestCase {
            name: "should not have console.log".to_string(),
            valid: Some(vec!["console.log(\"oops\");".to_string()]),
            invalid: None,
            expect_rewrite: None,
            fixture: None,
            expect_rows: None,
        };
        let result = run_one(&rule, &case);
        assert!(!result.pass);
        let detail = result.detail.expect("failure must carry detail");
        assert!(
            detail.contains("console.log(\"oops\");"),
            "detail: {detail}"
        );
        assert!(
            detail.contains('0'),
            "detail should mention expected count: {detail}"
        );
    }

    #[test]
    fn invalid_snippet_that_matches_passes() {
        let rule = base_rule("console.log($MSG)");
        let case = TestCase {
            name: "flags console.log".to_string(),
            valid: None,
            invalid: Some(vec!["console.log(\"bad\");".to_string()]),
            expect_rewrite: None,
            fixture: None,
            expect_rows: None,
        };
        let result = run_one(&rule, &case);
        assert!(result.pass, "expected pass, got: {:?}", result.detail);
    }

    #[test]
    fn invalid_snippet_with_zero_matches_fails_with_detail() {
        let rule = base_rule("console.log($MSG)");
        let case = TestCase {
            name: "should flag console.log".to_string(),
            valid: None,
            invalid: Some(vec!["console.info(\"clean\");".to_string()]),
            expect_rewrite: None,
            fixture: None,
            expect_rows: None,
        };
        let result = run_one(&rule, &case);
        assert!(!result.pass);
        let detail = result.detail.expect("failure must carry detail");
        assert!(
            detail.contains("console.info(\"clean\");"),
            "detail: {detail}"
        );
    }

    #[test]
    fn expect_rewrite_matching_output_passes() {
        let mut rule = base_rule("console.log($MSG)");
        rule.rewrite = Some("console.info($MSG)".to_string());
        let mut expect_rewrite = HashMap::new();
        expect_rewrite.insert(
            "console.log(\"hi\");".to_string(),
            "console.info(\"hi\")".to_string(),
        );
        let case = TestCase {
            name: "rewrites to console.info".to_string(),
            valid: None,
            invalid: None,
            expect_rewrite: Some(expect_rewrite),
            fixture: None,
            expect_rows: None,
        };
        let result = run_one(&rule, &case);
        assert!(result.pass, "expected pass, got: {:?}", result.detail);
    }

    #[test]
    fn expect_rewrite_mismatch_fails_with_expected_and_actual_in_detail() {
        let mut rule = base_rule("console.log($MSG)");
        rule.rewrite = Some("console.info($MSG)".to_string());
        let mut expect_rewrite = HashMap::new();
        expect_rewrite.insert(
            "console.log(\"hi\");".to_string(),
            "console.warn(\"hi\")".to_string(),
        );
        let case = TestCase {
            name: "rewrite mismatch".to_string(),
            valid: None,
            invalid: None,
            expect_rewrite: Some(expect_rewrite),
            fixture: None,
            expect_rows: None,
        };
        let result = run_one(&rule, &case);
        assert!(!result.pass);
        let detail = result.detail.expect("failure must carry detail");
        assert!(
            detail.contains("console.warn(\"hi\")"),
            "expected value missing from detail: {detail}"
        );
        assert!(
            detail.contains("console.info(\"hi\")"),
            "actual value missing from detail: {detail}"
        );
    }

    #[test]
    fn run_pattern_rule_tests_covers_every_declared_case() {
        let mut rule = base_rule("console.log($MSG)");
        rule.test = Some(vec![
            TestCase {
                name: "valid case".to_string(),
                valid: Some(vec!["console.info(\"hi\");".to_string()]),
                invalid: None,
                expect_rewrite: None,
                fixture: None,
                expect_rows: None,
            },
            TestCase {
                name: "invalid case".to_string(),
                valid: None,
                invalid: Some(vec!["console.log(\"hi\");".to_string()]),
                expect_rewrite: None,
                fixture: None,
                expect_rows: None,
            },
        ]);
        let results = run_pattern_rule_tests(&rule);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.pass), "{results:?}");
    }

    #[test]
    fn non_pattern_rule_produces_no_results() {
        let mut rule = base_rule("console.log($MSG)");
        rule.kind = RuleKind::Sql;
        rule.test = Some(vec![TestCase {
            name: "irrelevant".to_string(),
            valid: Some(vec!["x".to_string()]),
            invalid: None,
            expect_rewrite: None,
            fixture: None,
            expect_rows: None,
        }]);
        assert!(run_pattern_rule_tests(&rule).is_empty());
    }
}

#[cfg(test)]
mod sql_tests {
    use super::*;
    use crate::rules::{RuleKind, Severity, TestCase};
    use std::collections::HashMap;

    /// Build an inline fixture map containing `count` trivial Rust
    /// functions in a single `lib.rs` — enough for `SELECT file, line FROM
    /// entities WHERE kind = 0` (kind=0 is `Function`, the same convention
    /// `sql.rs`'s builtin-rule tests use) to return `count` rows once
    /// indexed.
    fn fixture_map(function_count: usize) -> HashMap<String, String> {
        let mut src = String::new();
        for i in 0..function_count {
            src.push_str(&format!("fn f{i}() {{}}\n"));
        }
        let mut files = HashMap::new();
        files.insert("lib.rs".to_string(), src);
        files
    }

    fn row(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn sql_rule_with_test(id: &str, query: &str, test: TestCase) -> Rule {
        Rule {
            id: id.to_string(),
            kind: RuleKind::Sql,
            severity: Severity::Warning,
            message: format!("{id} fired"),
            name: None,
            description: None,
            remediation: None,
            pattern: None,
            query: Some(query.to_string()),
            thresholds: None,
            strings: None,
            constraints: None,
            fix: None,
            rewrite: None,
            languages: None,
            exclude_test_paths: None,
            exclude_tooling_paths: None,
            test: Some(vec![test]),
        }
    }

    #[test]
    fn passing_fixture_matches_expect_rows() {
        let rule = sql_rule_with_test(
            "two-functions",
            "SELECT 'lib.rs' AS file, entities.start_line AS line FROM entities WHERE entities.kind = 0",
            TestCase {
                name: "finds both functions".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(2)),
                expect_rows: Some(vec![
                    row(&[("file", "lib.rs".into()), ("line", 1.into())]),
                    row(&[("file", "lib.rs".into()), ("line", 2.into())]),
                ]),
            },
        );
        let results = run_sql_rule_tests(&rule);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].pass,
            "expected pass, got: {:?}",
            results[0].detail
        );
        assert_eq!(results[0].rule_id, "two-functions");
        assert_eq!(results[0].test_name, "finds both functions");
    }

    #[test]
    fn order_insensitive_match_passes_when_rows_are_reversed() {
        // Same expected rows as the passing case, listed in reverse actual
        // order — a multiset comparison must still pass since SQL rules
        // don't guarantee row order without ORDER BY.
        let rule = sql_rule_with_test(
            "two-functions-reversed",
            "SELECT 'lib.rs' AS file, entities.start_line AS line FROM entities WHERE entities.kind = 0",
            TestCase {
                name: "order insensitive".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(2)),
                expect_rows: Some(vec![
                    row(&[("file", "lib.rs".into()), ("line", 2.into())]),
                    row(&[("file", "lib.rs".into()), ("line", 1.into())]),
                ]),
            },
        );
        let results = run_sql_rule_tests(&rule);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].pass,
            "expected pass, got: {:?}",
            results[0].detail
        );
    }

    #[test]
    fn extra_actual_row_fails_with_clear_detail() {
        let rule = sql_rule_with_test(
            "extra-row",
            "SELECT 'lib.rs' AS file, entities.start_line AS line FROM entities WHERE entities.kind = 0",
            TestCase {
                name: "expects one but fixture has two".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(2)),
                expect_rows: Some(vec![row(&[("file", "lib.rs".into()), ("line", 1.into())])]),
            },
        );
        let results = run_sql_rule_tests(&rule);
        assert_eq!(results.len(), 1);
        assert!(!results[0].pass);
        let detail = results[0].detail.clone().expect("failure carries detail");
        assert!(detail.contains("extra"), "detail: {detail}");
    }

    #[test]
    fn missing_expected_row_fails_with_clear_detail() {
        let rule = sql_rule_with_test(
            "missing-row",
            "SELECT 'lib.rs' AS file, entities.start_line AS line FROM entities WHERE entities.kind = 0",
            TestCase {
                name: "expects three but fixture has two".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(2)),
                expect_rows: Some(vec![
                    row(&[("file", "lib.rs".into()), ("line", 1.into())]),
                    row(&[("file", "lib.rs".into()), ("line", 2.into())]),
                    row(&[("file", "lib.rs".into()), ("line", 3.into())]),
                ]),
            },
        );
        let results = run_sql_rule_tests(&rule);
        assert_eq!(results.len(), 1);
        assert!(!results[0].pass);
        let detail = results[0].detail.clone().expect("failure carries detail");
        assert!(detail.contains("missing"), "detail: {detail}");
    }

    #[test]
    fn malformed_expect_rows_keys_report_clear_detail() {
        // `line` and `bogus_column` don't correspond to any actual query
        // result column (the query only ever selects `file`/`line`) —
        // must be a clear failure, not a silent pass or a panic.
        let rule = sql_rule_with_test(
            "malformed-keys",
            "SELECT 'lib.rs' AS file, entities.start_line AS line FROM entities WHERE entities.kind = 0",
            TestCase {
                name: "expected row has an unknown column".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(1)),
                expect_rows: Some(vec![row(&[
                    ("file", "lib.rs".into()),
                    ("line", 1.into()),
                    ("bogus_column", "nope".into()),
                ])]),
            },
        );
        let results = run_sql_rule_tests(&rule);
        assert_eq!(results.len(), 1);
        assert!(!results[0].pass);
        let detail = results[0].detail.clone().expect("failure carries detail");
        assert!(
            detail.contains("bogus_column") && detail.contains("not present"),
            "detail: {detail}"
        );
    }

    #[test]
    fn two_consecutive_runs_do_not_collide_on_temp_paths() {
        let rule = sql_rule_with_test(
            "one-function",
            "SELECT 'lib.rs' AS file, entities.start_line AS line FROM entities WHERE entities.kind = 0",
            TestCase {
                name: "run twice back to back".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(1)),
                expect_rows: Some(vec![row(&[("file", "lib.rs".into()), ("line", 1.into())])]),
            },
        );
        let first = run_sql_rule_tests(&rule);
        let second = run_sql_rule_tests(&rule);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert!(first[0].pass, "first run: {:?}", first[0].detail);
        assert!(second[0].pass, "second run: {:?}", second[0].detail);
    }

    #[test]
    fn missing_fixture_path_fails_with_clear_detail() {
        let rule = sql_rule_with_test(
            "no-fixture",
            "SELECT files.path AS file, entities.start_line AS line FROM entities JOIN files ON files.id = entities.file_id WHERE entities.kind = 0",
            TestCase {
                name: "declares no fixture".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: None,
                expect_rows: Some(vec![row(&[("file", "lib.rs".into()), ("line", 1.into())])]),
            },
        );
        let results = run_sql_rule_tests(&rule);
        assert_eq!(results.len(), 1);
        assert!(!results[0].pass);
        let detail = results[0].detail.clone().expect("failure carries detail");
        assert!(detail.contains("fixture"), "detail: {detail}");
    }

    #[test]
    fn missing_expect_rows_fails_with_clear_detail() {
        let rule = sql_rule_with_test(
            "no-expect-rows",
            "SELECT files.path AS file, entities.start_line AS line FROM entities JOIN files ON files.id = entities.file_id WHERE entities.kind = 0",
            TestCase {
                name: "declares no expect_rows".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(1)),
                expect_rows: None,
            },
        );
        let results = run_sql_rule_tests(&rule);
        assert_eq!(results.len(), 1);
        assert!(!results[0].pass);
        let detail = results[0].detail.clone().expect("failure carries detail");
        assert!(detail.contains("expect_rows"), "detail: {detail}");
    }

    #[test]
    fn non_sql_rule_produces_no_results() {
        let mut rule = sql_rule_with_test(
            "irrelevant",
            "SELECT files.path AS file, entities.start_line AS line FROM entities JOIN files ON files.id = entities.file_id WHERE entities.kind = 0",
            TestCase {
                name: "irrelevant".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(0)),
                expect_rows: Some(vec![]),
            },
        );
        rule.kind = RuleKind::Pattern;
        rule.pattern = Some("foo()".to_string());
        assert!(run_sql_rule_tests(&rule).is_empty());
    }

    #[test]
    fn invalid_query_fails_with_clear_detail_not_panic() {
        let rule = sql_rule_with_test(
            "broken-query",
            "SELECT file, line FROM entities WHERE ((",
            TestCase {
                name: "malformed sql".to_string(),
                valid: None,
                invalid: None,
                expect_rewrite: None,
                fixture: Some(fixture_map(1)),
                expect_rows: Some(vec![]),
            },
        );
        let results = run_sql_rule_tests(&rule);
        assert_eq!(results.len(), 1);
        assert!(!results[0].pass);
        assert!(results[0].detail.is_some());
    }
}
