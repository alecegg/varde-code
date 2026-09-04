//! SQL-rule execution: run `kind = sql` rules against an already-open
//! read-only connection and produce `Finding`s.
//!
//! The caller owns the connection (opened read-only, staleness-checked —
//! see `scan_cli`); this module only prepares, binds, and maps. Rule `query`
//! text may reference `:key` named parameters bound from the rule's
//! `thresholds` (f64) and `strings` (String) maps — each map key becomes a
//! `:key` binding, matching varde's TOML/SQL rule convention.

use crate::query::ApiError;
use crate::rules::finding::{Finding, Location, finding_id};
use crate::rules::{Diagnostic, Rule, RuleKind};
use rusqlite::{Connection, ToSql};

/// Run every `kind = sql` rule in `rules` against `conn`.
///
/// Filters to `kind=sql` internally (pattern rules are silently skipped).
/// Per-rule failures — prepare errors (bad SQL syntax), execution errors
/// (missing table/column), or result sets missing the required `file`/`line`
/// columns — are skip-and-reported as `Diagnostic`s; the pipeline continues
/// with the remaining rules, so an all-failing rules slice yields
/// `Ok((vec![], diagnostics))`, never `Err`.
///
/// Required result columns: `file` and `line` populate `Finding.location`;
/// every other selected column becomes a `Finding.evidence` entry keyed by
/// column name. `certainty` is always `None` (SQL rules have no ast-grep
/// confidence signal); `agent_instructions` sources from the rule's `fix`.
pub fn run_sql_rules(
    rules: &[Rule],
    conn: &Connection,
) -> Result<(Vec<Finding>, Vec<Diagnostic>), ApiError> {
    let mut findings = Vec::new();
    let mut diagnostics = Vec::new();
    for rule in rules.iter().filter(|r| r.kind == RuleKind::Sql) {
        match run_sql_rule(rule, conn) {
            Ok(mut rule_findings) => findings.append(&mut rule_findings),
            Err(diag) => diagnostics.push(diag),
        }
    }
    Ok((findings, diagnostics))
}

/// A bound named-parameter value, owned so the binding slice can reference
/// it across the statement's lifetime.
enum Param {
    F64(f64),
    Str(String),
}

impl ToSql for Param {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            Param::F64(v) => v.to_sql(),
            Param::Str(v) => v.to_sql(),
        }
    }
}

/// Run a `kind=sql` rule's query against the index.
///
/// TRUST BOUNDARY: `rule.query` is arbitrary SQL authored in the rule's TOML.
/// Parameter *values* are bound safely (see `Param`/`ToSql` below), but the
/// query body itself is executed verbatim via `conn.prepare`. This is safe
/// only because rules are loaded from trusted on-disk TOML the operator
/// controls. If rules ever become loadable from an untrusted source (a remote
/// registry, a URL, a PR-supplied pack), this becomes an arbitrary-SQL
/// surface (including `ATTACH` and writes): open a read-only connection for
/// rule SQL and/or reject anything that isn't a single `SELECT` before then.
fn run_sql_rule(rule: &Rule, conn: &Connection) -> Result<Vec<Finding>, Diagnostic> {
    let diag = |reason: String| Diagnostic {
        rule_id: Some(rule.id.clone()),
        file: "<sql rule>".to_string(),
        reason,
    };

    let Some(query) = rule.query.as_deref() else {
        return Err(diag(
            "rule kind=sql is missing its `query` field".to_string(),
        ));
    };

    let mut stmt = match conn.prepare(query) {
        Ok(stmt) => stmt,
        Err(e) => return Err(diag(format!("SQL prepare failed: {e}"))),
    };

    // Build the `:key` binding list from thresholds + strings (rule-defined,
    // so the set of params is dynamic, not fixed at compile time). Only keys
    // the query text actually declares are bound — rusqlite errors on any
    // `:key` binding absent from the statement, so a rule that keeps a
    // shared thresholds/strings map and references a subset in `query`
    // would otherwise fail outright on the unreferenced keys.
    let declared: std::collections::HashSet<String> = (1..=stmt.parameter_count())
        .filter_map(|i| stmt.parameter_name(i).map(str::to_string))
        .collect();
    let mut keys: Vec<String> = Vec::new();
    let mut params: Vec<Param> = Vec::new();
    if let Some(thresholds) = &rule.thresholds {
        for (key, value) in thresholds {
            let name = format!(":{key}");
            if declared.contains(&name) {
                keys.push(name);
                params.push(Param::F64(*value));
            }
        }
    }
    if let Some(strings) = &rule.strings {
        for (key, value) in strings {
            let name = format!(":{key}");
            if declared.contains(&name) {
                keys.push(name);
                params.push(Param::Str(value.clone()));
            }
        }
    }
    let bound: Vec<(&str, &dyn ToSql)> = keys
        .iter()
        .zip(params.iter())
        .map(|(key, param)| (key.as_str(), param as &dyn ToSql))
        .collect();

    let column_names: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let file_col = column_names.iter().position(|c| c == "file");
    let line_col = column_names.iter().position(|c| c == "line");
    let (Some(file_col), Some(line_col)) = (file_col, line_col) else {
        return Err(diag(
            "query must select a `file` column and a `line` column".to_string(),
        ));
    };

    // Optional precise-span columns. A rule that selects these (typically its
    // anchor entity's `start_byte`/`end_col`/…) gets a finding with a real
    // byte/column span instead of the line-only fallback. They are treated as
    // location columns — recognized here so they don't leak into `evidence`
    // (and thus into `{placeholder}` message rendering). Absent, the span
    // degrades to `start_line == end_line == line` with zero byte/col, exactly
    // as before this was added.
    let col_of = |name: &str| column_names.iter().position(|c| c == name);
    let start_byte_col = col_of("start_byte");
    let end_byte_col = col_of("end_byte");
    let start_col_col = col_of("start_col");
    let end_line_col = col_of("end_line");
    let end_col_col = col_of("end_col");
    let is_location = |index: usize| {
        index == file_col
            || index == line_col
            || Some(index) == start_byte_col
            || Some(index) == end_byte_col
            || Some(index) == start_col_col
            || Some(index) == end_line_col
            || Some(index) == end_col_col
    };

    let rows = match stmt.query_map(bound.as_slice(), |row| {
        let file: String = row.get(file_col)?;
        let line: i64 = row.get(line_col)?;
        let opt = |c: Option<usize>| -> rusqlite::Result<Option<i64>> {
            match c {
                Some(i) => row.get::<_, Option<i64>>(i),
                None => Ok(None),
            }
        };
        let span_cols = (
            opt(start_byte_col)?,
            opt(end_byte_col)?,
            opt(start_col_col)?,
            opt(end_line_col)?,
            opt(end_col_col)?,
        );
        let mut evidence = serde_json::Map::new();
        for (index, name) in column_names.iter().enumerate() {
            if is_location(index) {
                continue;
            }
            evidence.insert(name.clone(), value_ref_to_json(row.get_ref(index)?));
        }
        Ok((file, line, span_cols, evidence))
    }) {
        Ok(rows) => rows,
        Err(e) => return Err(diag(format!("SQL execution failed: {e}"))),
    };

    let mut findings = Vec::new();
    for row in rows {
        let (file, line, span_cols, evidence) = match row {
            Ok(row) => row,
            Err(e) => return Err(diag(format!("SQL row mapping failed: {e}"))),
        };
        let (start_byte, end_byte, start_col, end_line, end_col) = span_cols;
        let u32_or =
            |v: Option<i64>, default: u32| v.map(crate::model::saturating_u32).unwrap_or(default);
        let line_u32 = crate::model::saturating_u32(line);
        let span = crate::model::Span {
            start_byte: u32_or(start_byte, 0),
            end_byte: u32_or(end_byte, 0),
            start_line: line_u32,
            start_col: u32_or(start_col, 0),
            end_line: u32_or(end_line, line_u32),
            end_col: u32_or(end_col, 0),
        };
        let id = finding_id(&rule.id, &file, &span);
        let message = render_message(&rule.message, &evidence);
        findings.push(Finding {
            id,
            rule_id: rule.id.clone(),
            severity: rule.severity,
            message,
            location: Location { file, span },
            evidence: serde_json::Value::Object(evidence),
            remediation: rule.remediation.clone(),
            certainty: None,
            agent_instructions: rule.fix.clone(),
            rewrite_status: None,
            matched_file_state: None,
        });
    }
    Ok(findings)
}

/// Substitute `{column_name}` placeholders in a rule's `message` template
/// with the matching evidence column's value, so e.g. `fat-interface`'s
/// `"'{class_name}' declares {member_count} methods, exceeding threshold of
/// {threshold}"` renders as `"'Foo' declares 12 methods, exceeding
/// threshold of 10"` instead of leaking the raw `{class_name}` tokens into
/// the finding (as it did before this substitution existed — evidence was
/// collected per non-file/line query column but never applied back to the
/// message). A placeholder naming an evidence column absent from this row
/// (or non-scalar) is left untouched rather than panicking — better a
/// visible unrendered token than a crash on a rule-authoring typo.
fn render_message(template: &str, evidence: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close_rel) = rest[open..].find('}') else {
            out.push_str(rest);
            return out;
        };
        let close = open + close_rel;
        let key = &rest[open + 1..close];
        match evidence.get(key) {
            Some(serde_json::Value::String(s)) => {
                out.push_str(&rest[..open]);
                out.push_str(s);
            }
            Some(v @ (serde_json::Value::Number(_) | serde_json::Value::Bool(_))) => {
                out.push_str(&rest[..open]);
                out.push_str(&v.to_string());
            }
            _ => out.push_str(&rest[..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Convert a rusqlite `ValueRef` into a `serde_json::Value` for evidence.
/// Blobs are stringified lossily (byte → UTF-8) — the persisted schema's
/// only blob column (`body_minhash`) is not a plausible evidence column.
fn value_ref_to_json(value: rusqlite::types::ValueRef<'_>) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(i) => serde_json::json!(i),
        ValueRef::Real(f) => serde_json::json!(f),
        ValueRef::Text(bytes) => {
            serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned())
        }
        ValueRef::Blob(bytes) => {
            serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Severity;
    use std::collections::HashMap;

    fn temp_db(tag: &str) -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("varde-sql-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        let db_path = dir.join("index.db");
        let conn = crate::db::open_or_rebuild(&db_path).expect("db opens");
        conn.execute_batch(
            "CREATE TABLE t (file TEXT, line INTEGER, count INTEGER, note TEXT);
             INSERT INTO t VALUES ('a.rs', 1, 5, 'alpha'), ('b.rs', 2, 1, 'beta'), ('c.rs', 3, 2, 'gamma');",
        )
        .expect("fixture schema");
        (db_path, conn)
    }

    fn sql_rule(
        id: &str,
        query: &str,
        thresholds: Option<HashMap<String, f64>>,
        strings: Option<HashMap<String, String>>,
        fix: Option<&str>,
    ) -> Rule {
        Rule {
            id: id.to_string(),
            kind: RuleKind::Sql,
            severity: Severity::Error,
            message: format!("{id} fired"),
            name: None,
            description: None,
            remediation: None,
            pattern: None,
            query: Some(query.to_string()),
            thresholds,
            strings,
            constraints: None,
            fix: fix.map(|f| f.to_string()),
            rewrite: None,
            languages: None,
            exclude_test_paths: None,
            exclude_tooling_paths: None,
            test: None,
        }
    }

    #[test]
    fn location_and_evidence_from_result_columns_with_threshold_binding() {
        let (_dir, conn) = temp_db("basic");
        let mut thresholds = HashMap::new();
        thresholds.insert("limit".to_string(), 3.0);
        let rules = vec![sql_rule(
            "high-count",
            "SELECT file, line, count FROM t WHERE count > :limit",
            Some(thresholds),
            None,
            None,
        )];
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(findings.len(), 1, "only the row above :limit matches");
        let f = &findings[0];
        assert_eq!(f.location.file, "a.rs");
        assert_eq!(f.location.span.start_line, 1);
        assert_eq!(f.location.span.end_line, 1);
        assert_eq!(
            f.evidence,
            serde_json::json!({ "count": 5 }),
            "every non-file/line column becomes an evidence entry"
        );
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn string_parameter_binding_selects_matching_row() {
        let (_dir, conn) = temp_db("strings");
        let mut strings = HashMap::new();
        strings.insert("needle".to_string(), "beta".to_string());
        let rules = vec![sql_rule(
            "needle-rule",
            "SELECT file, line, note FROM t WHERE note = :needle",
            None,
            Some(strings),
            None,
        )];
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(findings.len(), 1, "correct binding selects b.rs");
        assert_eq!(findings[0].location.file, "b.rs");
        assert_eq!(findings[0].evidence, serde_json::json!({ "note": "beta" }));
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn fix_becomes_agent_instructions_and_certainty_is_none() {
        let (_dir, conn) = temp_db("fix");
        let rules = vec![sql_rule(
            "with-fix",
            "SELECT file, line FROM t",
            None,
            None,
            Some("rewrite the query"),
        )];
        let (findings, _) = run_sql_rules(&rules, &conn).expect("runs");
        assert_eq!(findings.len(), 3);
        for f in &findings {
            assert_eq!(f.agent_instructions.as_deref(), Some("rewrite the query"));
            assert_eq!(f.certainty, None, "SQL findings have no confidence signal");
        }
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn pattern_rules_are_silently_skipped() {
        let (_dir, conn) = temp_db("mixed");
        let mut pattern = sql_rule("pattern-rule", "SELECT file, line FROM t", None, None, None);
        pattern.kind = RuleKind::Pattern;
        pattern.pattern = Some("foo()".to_string());
        pattern.query = None;
        let rules = vec![
            pattern,
            sql_rule("sql-rule", "SELECT file, line FROM t", None, None, None),
        ];
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty());
        assert!(
            findings.iter().all(|f| f.rule_id == "sql-rule"),
            "only kind=sql rules evaluated"
        );
        assert_eq!(findings.len(), 3);
        let _ = std::fs::remove_dir_all(&_dir);
    }
    #[test]
    fn invalid_sql_syntax_is_skip_and_reported_other_rules_run() {
        let (_dir, conn) = temp_db("bad-syntax");
        let rules = vec![
            sql_rule(
                "broken",
                "SELECT file, line FROM t WHERE ((",
                None,
                None,
                None,
            ),
            sql_rule("valid", "SELECT file, line FROM t", None, None, None),
        ];
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("Ok, not Err");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("broken"));
        assert!(!diagnostics[0].reason.is_empty());
        assert!(
            findings.iter().all(|f| f.rule_id == "valid"),
            "valid rule's findings still returned"
        );
        assert_eq!(findings.len(), 3);
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn runtime_execution_failure_is_skip_and_reported() {
        let (_dir, conn) = temp_db("bad-exec");
        let rules = vec![
            sql_rule(
                "missing-table",
                "SELECT file, line FROM nonexistent_table",
                None,
                None,
                None,
            ),
            sql_rule("valid", "SELECT file, line FROM t", None, None, None),
        ];
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("Ok, not Err");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("missing-table"));
        assert!(diagnostics[0].reason.contains("nonexistent_table"));
        assert_eq!(findings.len(), 3, "remaining rules still run");
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn missing_file_or_line_column_is_skip_and_reported() {
        let (_dir, conn) = temp_db("no-file-col");
        let rules = vec![
            sql_rule("no-file", "SELECT note, line FROM t", None, None, None),
            sql_rule("valid", "SELECT file, line FROM t", None, None, None),
        ];
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("Ok, not Err");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("no-file"));
        assert!(
            diagnostics[0].reason.contains("file"),
            "reason names the missing column: {}",
            diagnostics[0].reason
        );
        assert_eq!(findings.len(), 3);
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn builtin_churn_complexity_hotspot_fires_only_above_both_thresholds() {
        let (_dir, conn) = temp_db("builtin-hotspot");
        conn.execute_batch(
            "INSERT INTO files (path, complexity, churn) VALUES
                ('hot.rs', 80, 40),
                ('complex-only.rs', 80, 5),
                ('churny-only.rs', 10, 40),
                ('neither.rs', 10, 5);",
        )
        .expect("files fixture rows insert");

        let rules = crate::rules::builtin_rules();
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let hotspot_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule_id == "churn-complexity-hotspot")
            .collect();
        assert_eq!(
            hotspot_findings.len(),
            1,
            "only the file above both thresholds fires: {hotspot_findings:?}"
        );
        assert_eq!(hotspot_findings[0].location.file, "hot.rs");
        assert_eq!(hotspot_findings[0].certainty, None);
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn builtin_file_complexity_hotspot_fires_only_above_complexity_threshold() {
        let (_dir, conn) = temp_db("builtin-file-hotspot");
        conn.execute_batch(
            "INSERT INTO files (path, complexity, churn) VALUES
                ('complex.rs', 80, 5),
                ('low.rs', 10, 40),
                ('boundary.rs', 50, 3);",
        )
        .expect("files fixture rows insert");

        let rules = crate::rules::builtin_rules();
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let hotspot_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule_id == "file-complexity-hotspot")
            .collect();
        assert_eq!(
            hotspot_findings.len(),
            1,
            "only the file strictly above the complexity threshold fires: {hotspot_findings:?}"
        );
        assert_eq!(hotspot_findings[0].location.file, "complex.rs");
        assert_eq!(hotspot_findings[0].certainty, None);
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn builtin_function_complexity_hotspot_fires_only_above_threshold_per_function() {
        let (_dir, conn) = temp_db("builtin-function-hotspot");
        conn.execute_batch(
            "INSERT INTO files (path) VALUES ('a.rs'), ('b.rs');
             INSERT INTO entities (kind, name, file_id, start_byte, end_byte,
                                   start_line, start_col, end_line, end_col)
             VALUES
                -- a.rs: complex_fn (16 control-flow entities -> complexity 17)
                (0, 'complex_fn', 1, 0, 500, 5, 0, 40, 1),
                -- a.rs: simple_fn (2 control-flow entities -> complexity 3)
                (0, 'simple_fn', 1, 600, 700, 50, 0, 55, 1),
                -- b.rs: other_simple_fn (0 control-flow entities -> complexity 1)
                (0, 'other_simple_fn', 2, 0, 50, 3, 0, 5, 1);",
        )
        .expect("entities fixture rows insert");
        for line in 6..22 {
            conn.execute(
                "INSERT INTO entities (kind, name, file_id, start_byte, end_byte,
                                       start_line, start_col, end_line, end_col,
                                       enclosing_function)
                 VALUES (12, 'if', 1, 0, 1, ?1, 0, ?1, 1, 'complex_fn')",
                [line],
            )
            .expect("control-flow entity insert");
        }
        for line in 51..53 {
            conn.execute(
                "INSERT INTO entities (kind, name, file_id, start_byte, end_byte,
                                       start_line, start_col, end_line, end_col,
                                       enclosing_function)
                 VALUES (12, 'if', 1, 0, 1, ?1, 0, ?1, 1, 'simple_fn')",
                [line],
            )
            .expect("control-flow entity insert");
        }

        let rules = crate::rules::builtin_rules();
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let hotspot_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule_id == "function-complexity-hotspot")
            .collect();
        assert_eq!(
            hotspot_findings.len(),
            1,
            "only the function strictly above the threshold fires: {hotspot_findings:?}"
        );
        assert_eq!(hotspot_findings[0].location.file, "a.rs");
        assert_eq!(
            hotspot_findings[0]
                .evidence
                .get("name")
                .and_then(|v| v.as_str()),
            Some("complex_fn")
        );
        assert_eq!(hotspot_findings[0].certainty, None);
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn builtin_low_fan_in_high_fan_out_fires_on_glue_files_only() {
        let (_dir, conn) = temp_db("builtin-fan-shape");
        conn.execute_batch(
            "INSERT INTO files (path, fan_in, fan_out) VALUES
                ('glue.rs', 1, 20),
                ('entry.rs', 50, 45),
                ('isolated.rs', 0, 5),
                ('boundary.rs', 2, 15);",
        )
        .expect("files fixture rows insert");

        let rules = crate::rules::builtin_rules();
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let fan_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule_id == "low-fan-in-high-fan-out-file")
            .collect();
        // glue.rs is the classic orchestrator (1 in, 20 out); boundary.rs
        // sits exactly on both thresholds (fan_in <= 2, fan_out >= 15 are
        // inclusive). entry.rs has huge fan-out but is a hub (50 in), and
        // isolated.rs simply depends on little — neither fires.
        let mut fired: Vec<&str> = fan_findings
            .iter()
            .map(|f| f.location.file.as_str())
            .collect();
        fired.sort_unstable();
        assert_eq!(fired, vec!["boundary.rs", "glue.rs"], "{fan_findings:?}");
        for f in &fan_findings {
            assert_eq!(f.location.span.start_line, 1, "{f:?}");
            assert_eq!(f.certainty, None, "{f:?}");
        }
    }

    #[test]
    fn builtin_circular_import_fires_once_per_direct_cycle_only() {
        let (_dir, conn) = temp_db("builtin-circular-import");
        conn.execute_batch(
            "INSERT INTO files (path) VALUES
                ('a.rs'), ('b.rs'), ('c.rs'), ('d.rs'), ('unresolved.rs');
             -- a.rs <-> b.rs: a direct cycle, should fire exactly once.
             INSERT INTO resolved_edges (from_file_id, to_file_id, kind, resolved) VALUES
                (1, 2, 1, 1),
                (2, 1, 1, 1),
                -- c.rs -> d.rs one-way: no cycle, must not fire.
                (3, 4, 1, 1),
                -- a.rs -> c.rs: a call edge (kind=0), not an import; must
                -- not be treated as part of any import cycle.
                (1, 3, 0, 1),
                (3, 1, 0, 1),
                -- unresolved.rs -> a.rs and a.rs -> unresolved.rs, but
                -- neither edge is resolved; an unresolved 'cycle' must not
                -- fire.
                (5, 1, 1, 0),
                (1, 5, 1, 0);",
        )
        .expect("resolved_edges fixture rows insert");

        let rules = crate::rules::builtin_rules();
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let cycle_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule_id == "circular-import")
            .collect();
        assert_eq!(
            cycle_findings.len(),
            1,
            "exactly one finding for the a.rs/b.rs cycle, no double-report, no false positives: {cycle_findings:?}"
        );
        assert_eq!(cycle_findings[0].location.file, "a.rs");
        assert_eq!(
            cycle_findings[0]
                .evidence
                .get("other")
                .and_then(|v| v.as_str()),
            Some("b.rs")
        );
        assert_eq!(cycle_findings[0].certainty, None);
    }

    #[test]
    fn builtin_vertical_slice_sprawl_fires_only_at_or_above_foreign_slice_threshold() {
        let (_dir, conn) = temp_db("builtin-vertical-slice-sprawl");
        conn.execute_batch(
            "INSERT INTO files (path, community_id) VALUES
                ('home.rs', 0),      -- id 1: the sprawler/focused functions live here
                ('slice_a.rs', 1),   -- id 2
                ('slice_b.rs', 2),   -- id 3
                ('slice_c.rs', 3),   -- id 4
                ('same_slice.rs', 0); -- id 5: same community as home.rs

             -- `sprawler`: 4 call-site entities, each a resolved Call edge
             -- into a different file. Three land in three distinct foreign
             -- communities (1, 2, 3); the fourth lands in home.rs's own
             -- community (0) and must not count toward the foreign-slice
             -- total.
             INSERT INTO entities (kind, name, file_id, start_byte, end_byte,
                                   start_line, start_col, end_line, end_col)
             VALUES
                (0, 'sprawler', 1, 0, 500, 5, 0, 40, 1),
                (6, 'a', 1, 10, 15, 6, 0, 6, 5),
                (6, 'b', 1, 20, 25, 7, 0, 7, 5),
                (6, 'c', 1, 30, 35, 8, 0, 8, 5),
                (6, 'd', 1, 40, 45, 9, 0, 9, 5);
             UPDATE entities SET enclosing_function = 'sprawler'
                WHERE kind = 6 AND file_id = 1;

             -- `focused`: 2 call-site entities, both landing in the same
             -- foreign community (1) — one distinct foreign slice, below
             -- the >= 3 threshold, must not fire.
             INSERT INTO entities (kind, name, file_id, start_byte, end_byte,
                                   start_line, start_col, end_line, end_col)
             VALUES
                (0, 'focused', 1, 600, 700, 50, 0, 55, 1),
                (6, 'e', 1, 610, 615, 51, 0, 51, 5),
                (6, 'f', 1, 620, 625, 52, 0, 52, 5);
             UPDATE entities SET enclosing_function = 'focused'
                WHERE kind = 6 AND file_id = 1 AND name IN ('e', 'f');",
        )
        .expect("entities fixture rows insert");

        conn.execute_batch(
            "INSERT INTO resolved_edges (from_file_id, to_file_id, kind, resolved, from_entity_id)
             VALUES
                -- sprawler's calls: entities 2..5 are 'a'..'d' (ids assigned
                -- in insertion order, offset by the 1 pre-existing 'sprawler'
                -- Function row).
                (1, 2, 0, 1, 2),  -- a -> slice_a.rs (community 1)
                (1, 3, 0, 1, 3),  -- b -> slice_b.rs (community 2)
                (1, 4, 0, 1, 4),  -- c -> slice_c.rs (community 3)
                (1, 5, 0, 1, 5),  -- d -> same_slice.rs (community 0, same as home.rs)
                -- focused's calls: entities 7..8 are 'e'..'f'.
                (1, 2, 0, 1, 7),  -- e -> slice_a.rs (community 1)
                (1, 2, 0, 1, 8);  -- f -> slice_a.rs (community 1, same target again)",
        )
        .expect("resolved_edges fixture rows insert");

        let rules = crate::rules::builtin_rules();
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let sprawl_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule_id == "vertical-slice-sprawl")
            .collect();
        assert_eq!(
            sprawl_findings.len(),
            1,
            "only sprawler (3 distinct foreign slices) fires, not focused (1 distinct foreign slice): {sprawl_findings:?}"
        );
        assert_eq!(sprawl_findings[0].location.file, "home.rs");
        assert_eq!(
            sprawl_findings[0]
                .evidence
                .get("name")
                .and_then(|v| v.as_str()),
            Some("sprawler")
        );
        assert_eq!(
            sprawl_findings[0]
                .evidence
                .get("slices")
                .and_then(|v| v.as_i64()),
            Some(3),
            "same-community edge (d -> same_slice.rs) must not inflate the count: {sprawl_findings:?}"
        );
        assert_eq!(sprawl_findings[0].certainty, None);
    }

    #[test]
    fn builtin_duplicate_code_clone_fires_only_for_bands_at_or_above_min_size() {
        let (_dir, conn) = temp_db("builtin-clone");
        conn.execute_batch(
            "INSERT INTO files (path) VALUES
                ('tri_a.rs'), ('tri_b.rs'), ('tri_c.rs'),
                ('pair_a.rs'), ('pair_b.rs'), ('solo.rs');
             INSERT INTO entities (kind, name, file_id, start_byte, end_byte,
                                   start_line, start_col, end_line, end_col)
             VALUES
                (0, 'tri_a_fn', 1, 0, 10, 10, 0, 12, 4),
                (0, 'tri_b_fn', 2, 0, 10, 20, 0, 22, 4),
                (0, 'tri_c_fn', 3, 0, 10, 30, 0, 32, 4),
                (0, 'pair_a_fn', 4, 0, 10, 40, 0, 42, 4),
                (0, 'pair_b_fn', 5, 0, 10, 50, 0, 52, 4),
                (0, 'solo_fn', 6, 0, 10, 60, 0, 62, 4);
             INSERT INTO clone_bands (label) VALUES ('clone-band-0'), ('clone-band-1');
             INSERT INTO clone_band_members (band_id, entity_id) VALUES
                (1, 1), (1, 2), (1, 3),
                (2, 4), (2, 5);",
        )
        .expect("clone fixture rows insert");

        let rules = crate::rules::builtin_rules();
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("runs");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let clone_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule_id == "duplicate-code-clone")
            .collect();
        assert_eq!(
            clone_findings.len(),
            3,
            "one finding per member of the 3-member band, no others: {clone_findings:?}"
        );
        let mut located: Vec<(String, u32)> = clone_findings
            .iter()
            .map(|f| (f.location.file.clone(), f.location.span.start_line))
            .collect();
        located.sort();
        assert_eq!(
            located,
            vec![
                ("tri_a.rs".to_string(), 10),
                ("tri_b.rs".to_string(), 20),
                ("tri_c.rs".to_string(), 30),
            ],
            "every member of the qualifying band is reported at its own line"
        );
        assert!(
            clone_findings
                .iter()
                .all(|f| f.evidence.get("label").and_then(|l| l.as_str()) == Some("clone-band-0")),
            "band label rides along as evidence"
        );
        assert!(
            findings
                .iter()
                .all(|f| f.rule_id != "churn-complexity-hotspot"
                    && f.rule_id != "file-complexity-hotspot"),
            "NULL complexity/churn must not fire the hotspot rules: {:?}",
            findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&_dir);
    }

    #[test]
    fn all_rules_failing_yields_empty_findings_with_diagnostics() {
        let (_dir, conn) = temp_db("all-bad");
        let rules = vec![
            sql_rule("bad-one", "SELECT file, line FROM nope", None, None, None),
            sql_rule("bad-two", "SELECT note FROM t", None, None, None),
        ];
        let (findings, diagnostics) = run_sql_rules(&rules, &conn).expect("Ok, not Err");
        assert!(findings.is_empty());
        assert_eq!(diagnostics.len(), 2);
        assert!(
            diagnostics
                .iter()
                .all(|d| d.rule_id.is_some() && !d.reason.is_empty()),
            "every diagnostic populated: {diagnostics:?}"
        );
        let _ = std::fs::remove_dir_all(&_dir);
    }
}
