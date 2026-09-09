//! Pattern-rule execution: filter, correlate, and convert find_pattern
//! matches into `Finding`s.
//!
//! The matcher itself is find_pattern's (reused unchanged); this module owns
//! everything the rule layer adds on top: per-capture regex constraints, the
//! enclosing-entity correlation, match → Finding conversion, and the batch
//! `run_pattern_rules` pipeline.

use crate::query::ApiError;
use crate::rules::Diagnostic;
use crate::rules::correlate::EnclosingLookup;
use crate::rules::finding::{Finding, Location, match_to_finding};
use std::collections::HashMap;

/// Filter a find_pattern match array by a rule's per-capture regex
/// constraints (ast-grep's per-metavariable `constraints` convention).
///
/// A match survives only if every named constraint's regex matches its
/// corresponding capture's text (AND semantics). A capture named in the
/// constraints map but absent from the match drops the match. `$$$VAR`
/// variadic captures bind an array of nodes — the regex applies to the full
/// joined text of all bound nodes. An invalid regex is an `ApiError`, not a
/// panic.
///
/// A constraint string prefixed with `!` negates the check — the match
/// survives only if the regex (everything after the `!`) does NOT match.
/// This is a rule-layer convention, not a regex feature: the underlying
/// `regex` crate has no lookaround, so "must not be exactly X" can't be
/// expressed as a positive pattern for an arbitrary alternative set (e.g.
/// `empty-except-block`'s exclusion of `except ImportError:`/`except
/// ModuleNotFoundError:` while still flagging every other exception type).
pub fn apply_constraints(
    matches: &[serde_json::Value],
    constraints: &HashMap<String, String>,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let compiled = compile_constraints(constraints)?;
    Ok(filter_matches(matches, &compiled))
}

/// Collect the distinct `$VAR` / `$$$VAR` capture names referenced by a
/// pattern or rewrite template, in first-occurrence order.
///
/// Tokenization is shared with the substitution engine
/// (`rewrite::template_tokens` — see its doc for the token syntax contract,
/// also mirrored by `find_pattern::replace_meta`'s marker rewriting). Used
/// by load-time validation only.
pub fn capture_names(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for token in crate::rules::rewrite::template_tokens(template) {
        if seen.insert(token.name) {
            names.push(token.name.to_string());
        }
    }
    names
}

/// Validate a pattern rule's `rewrite` template against its `pattern` at
/// rule-load time.
///
/// Every `$VAR` / `$$$VAR` token in `rewrite` must name a capture declared
/// in `pattern` (token-name comparison — the same name in either sigil
/// form satisfies it, per the load-time-validation decision). A rewrite
/// referencing an undeclared capture is a rule-authoring error and rejects
/// the rule at load (skip-and-report `Diagnostic`), never a silent partial
/// rewrite mid-scan.
pub fn validate_rewrite_template(rewrite: &str, pattern: &str) -> Result<(), String> {
    let pattern_names = capture_names(pattern);
    let pattern_captures: std::collections::HashSet<&str> =
        pattern_names.iter().map(|s| s.as_str()).collect();
    for name in capture_names(rewrite) {
        if !pattern_captures.contains(name.as_str()) {
            return Err(format!(
                "rewrite references capture `{name}` which is not declared in pattern"
            ));
        }
    }
    Ok(())
}

/// Compile a rule's per-capture regex constraints once.
///
/// Hoisted out of the per-language match loop so a rule with an invalid
/// constraint regex fails with a single diagnostic (not one per target
/// language) and — more importantly — never pays for a `find_pattern` scan
/// across the repo tree before the regex is known to be broken.
fn compile_constraints(
    constraints: &HashMap<String, String>,
) -> Result<Vec<(String, bool, regex::Regex)>, ApiError> {
    let mut compiled = Vec::with_capacity(constraints.len());
    for (name, pattern) in constraints {
        let (negate, regex_src) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        let regex = regex::Regex::new(regex_src).map_err(|e| {
            ApiError::new(
                "invalid_constraint",
                format!("constraint for capture `{name}` is not a valid regex: {e}"),
            )
        })?;
        compiled.push((name.clone(), negate, regex));
    }
    Ok(compiled)
}

/// Filter matches against pre-compiled per-capture regex constraints.
fn filter_matches(
    matches: &[serde_json::Value],
    compiled: &[(String, bool, regex::Regex)],
) -> Vec<serde_json::Value> {
    if compiled.is_empty() {
        return matches.to_vec();
    }

    let mut kept = Vec::new();
    'matches: for m in matches {
        let Some(captures) = m.get("captures").and_then(|c| c.as_object()) else {
            continue;
        };
        for (name, negate, regex) in compiled {
            let Some(capture) = captures.get(name.as_str()) else {
                continue 'matches;
            };
            let text = match capture {
                // `$$$VAR` variadic capture: array of bound nodes.
                serde_json::Value::Array(nodes) => nodes
                    .iter()
                    .filter_map(|n| n.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .concat(),
                // `$VAR` capture: a single node object.
                node => match node.get("text").and_then(|t| t.as_str()) {
                    Some(text) => text.to_string(),
                    None => continue 'matches,
                },
            };
            if regex.is_match(&text) == *negate {
                continue 'matches;
            }
        }
        kept.push(m.clone());
    }
    kept
}

/// Execute a batch of `kind=pattern` rules against `repo_root`.
///
/// Per-rule pipeline: resolve target files by `Rule.languages` (None/empty =
/// every file whose parse succeeds, per supported language), run find_pattern
/// over the repo tree (pattern parsed once per rule+language, per-file
/// matching parallelized), apply the rule's per-capture regex constraints,
/// correlate each surviving match against its enclosing persisted
/// function/method entity (the structural facts ride along on the finding's
/// `evidence.enclosing_function`), and convert to `Finding`.
///
/// Partial-failure semantics: a rule that fails (bad pattern syntax,
/// unsupported language name, invalid constraint regex) is skip-and-reported
/// as a `Diagnostic` and the pipeline continues with the remaining rules. A
/// DB-level failure (absent/unopenable/missing schema — the persisted DB the
/// correlation queries) is a whole-pipeline `ApiError`, not a per-rule
/// diagnostic: an unusable DB is a precondition failure, and callers open
/// the read-only connection before invoking this.
pub fn run_pattern_rules(
    rules: &[crate::rules::Rule],
    repo_root: &std::path::Path,
    conn: &rusqlite::Connection,
) -> Result<(Vec<Finding>, Vec<Diagnostic>), ApiError> {
    let mut findings = Vec::new();
    let mut diagnostics = Vec::new();
    for rule in rules
        .iter()
        .filter(|r| r.kind == crate::rules::RuleKind::Pattern)
    {
        match run_pattern_rule(rule, repo_root, conn) {
            Ok((mut rule_findings, rule_diags)) => {
                findings.append(&mut rule_findings);
                diagnostics.extend(rule_diags);
            }
            Err(e) => return Err(e),
        }
    }
    Ok((findings, diagnostics))
}

fn run_pattern_rule(
    rule: &crate::rules::Rule,
    repo_root: &std::path::Path,
    conn: &rusqlite::Connection,
) -> Result<(Vec<Finding>, Vec<Diagnostic>), ApiError> {
    let Some(pattern) = rule.pattern.as_deref() else {
        return Ok((
            Vec::new(),
            vec![Diagnostic {
                rule_id: Some(rule.id.clone()),
                file: repo_root.display().to_string(),
                reason: "rule kind=pattern is missing its `pattern` field".to_string(),
            }],
        ));
    };

    let mut diagnostics = Vec::new();

    // Compile the rule's per-capture regex constraints once, before scanning
    // any file — an invalid regex fails the whole rule with a single
    // diagnostic instead of one per target language, and never pays for a
    // find_pattern scan across the repo tree first.
    let compiled_constraints =
        match compile_constraints(rule.constraints.as_ref().unwrap_or(&HashMap::new())) {
            Ok(compiled) => compiled,
            Err(e) => {
                return Ok((
                    Vec::new(),
                    vec![Diagnostic {
                        rule_id: Some(rule.id.clone()),
                        file: repo_root.display().to_string(),
                        reason: e.message,
                    }],
                ));
            }
        };

    let langs: Vec<ast_grep_language::SupportLang> = match &rule.languages {
        Some(names) if !names.is_empty() => {
            let mut resolved = Vec::new();
            for name in names {
                match crate::parse::language_from_name(name) {
                    Some(lang) => resolved.push(lang),
                    None => diagnostics.push(Diagnostic {
                        rule_id: Some(rule.id.clone()),
                        file: repo_root.display().to_string(),
                        reason: format!("unsupported language `{name}` in rule.languages"),
                    }),
                }
            }
            resolved
        }
        None => crate::parse::SUPPORTED_LANGUAGES.to_vec(),
        Some(_) => crate::parse::SUPPORTED_LANGUAGES.to_vec(),
    };

    let mut findings = Vec::new();
    // Prepare the correlation statement once per rule — a scan with hundreds
    // of matches pays one SQL compile, not one per match (perf budget).
    let mut enclosing = EnclosingLookup::new(conn)?;
    let mut test_paths = crate::rules::correlate::TestPathLookup::new(conn);
    let exclude_test_paths = rule.exclude_test_paths.unwrap_or(false);
    let exclude_tooling_paths = rule.exclude_tooling_paths.unwrap_or(false);
    // A pattern that fails to parse in a language it was never written for is
    // normal for language-agnostic rules (e.g. `console.log($MSG)` is not
    // valid Go) — only a pattern that fails in EVERY tried language is a
    // broken rule worth reporting.
    let mut parse_failures = 0usize;
    let mut lang_count = 0usize;
    // Cache the match-time file stat per distinct resolved real path (many
    // findings can share one file within a rule's evaluation, and different
    // walk paths — symlinks, relative vs. absolute — can name the same
    // physical file) — see `Finding::matched_file_state`. Keying by the
    // resolved path, not the walk path, keeps this cache consistent with the
    // staleness guard in `scan_cli::apply_rewrites`, which also resolves
    // through `resolve_real_path` before comparing state.
    let mut file_state_cache: HashMap<String, Option<crate::rules::finding::FileState>> =
        HashMap::new();
    for lang in langs {
        lang_count += 1;
        let input = serde_json::json!({
            "pattern": pattern,
            "path": repo_root.display().to_string(),
            "language": crate::parse::language_name(&lang),
        });
        let matches = match crate::query::find_pattern::find_pattern_unbounded(&input) {
            Ok(matches) => matches,
            Err(e) if e.code == "invalid_pattern" => {
                parse_failures += 1;
                continue;
            }
            Err(e) => return Err(e),
        };
        let matches = matches["matches"].as_array().cloned().unwrap_or_default();
        let kept = filter_matches(&matches, &compiled_constraints);
        for m in kept {
            let Some(file) = m.get("file").and_then(|f| f.as_str()) else {
                continue;
            };
            if exclude_test_paths && test_paths.is_test_path(file)? {
                continue;
            }
            if exclude_tooling_paths && test_paths.is_tooling_path(file)? {
                continue;
            }
            let Some(span) = m.get("span") else { continue };
            let start_byte = span.get("start_byte").and_then(|b| b.as_u64()).unwrap_or(0) as u32;
            let end_byte = span.get("end_byte").and_then(|b| b.as_u64()).unwrap_or(0) as u32;
            let location = Location {
                file: file.to_string(),
                span: crate::model::Span {
                    start_byte,
                    end_byte,
                    start_line: span.get("start_line").and_then(|b| b.as_u64()).unwrap_or(0) as u32,
                    start_col: span.get("start_col").and_then(|b| b.as_u64()).unwrap_or(0) as u32,
                    end_line: span.get("end_line").and_then(|b| b.as_u64()).unwrap_or(0) as u32,
                    end_col: span.get("end_col").and_then(|b| b.as_u64()).unwrap_or(0) as u32,
                },
            };
            let mut finding = match_to_finding(rule, &m, location);
            let real = crate::rules::finding::resolve_real_path(file);
            finding.matched_file_state = *file_state_cache
                .entry(real.to_string_lossy().into_owned())
                .or_insert_with(|| crate::rules::finding::FileState::of(&real));
            // Relational context: attach the enclosing entity's structural
            // facts to the finding's evidence.
            if let Some(fact) = enclosing.lookup(file, start_byte, end_byte)?
                && let Some(evidence) = finding.evidence.as_object_mut()
            {
                evidence.insert(
                    "enclosing_function".to_string(),
                    serde_json::json!({ "name": fact.name, "is_async": fact.is_async }),
                );
            }
            findings.push(finding);
        }
    }
    if lang_count > 0 && parse_failures == lang_count {
        diagnostics.push(Diagnostic {
            rule_id: Some(rule.id.clone()),
            file: repo_root.display().to_string(),
            reason: "pattern does not parse in any target language".to_string(),
        });
    }
    Ok((findings, diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_rules_use_the_php_grammar() {
        assert_eq!(
            crate::parse::language_name(&ast_grep_language::SupportLang::Php),
            "php"
        );
    }

    #[test]
    fn every_supported_rule_language_has_its_own_grammar_name() {
        use ast_grep_language::SupportLang::*;

        let cases = [
            (TypeScript, "typescript"),
            (Tsx, "tsx"),
            (JavaScript, "javascript"),
            (C, "c"),
            (Cpp, "cpp"),
            (Go, "go"),
            (Java, "java"),
            (CSharp, "csharp"),
            (Kotlin, "kotlin"),
            (Swift, "swift"),
            (Python, "python"),
            (Ruby, "ruby"),
            (Php, "php"),
            (Lua, "lua"),
            (Scala, "scala"),
            (Dart, "dart"),
            (Elixir, "elixir"),
            (Solidity, "solidity"),
            (Haskell, "haskell"),
            (Bash, "bash"),
            (Rust, "rust"),
        ];

        assert_eq!(crate::parse::SUPPORTED_LANGUAGES.len(), cases.len());
        for (lang, name) in cases {
            assert_eq!(crate::parse::language_name(&lang), name, "{lang:?}");
        }
    }

    fn match_with_captures(
        captures: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Value {
        serde_json::json!({
            "kind": "call_expression",
            "text": "foo()",
            "span": { "start_byte": 0, "end_byte": 5, "start_line": 1, "start_col": 0, "end_line": 1, "end_col": 5 },
            "captures": serde_json::Value::Object(captures),
        })
    }

    fn capture_node(text: &str) -> serde_json::Value {
        serde_json::json!({ "kind": "identifier", "text": text })
    }

    fn constraints(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn matching_constraint_retains_match() {
        let mut caps = serde_json::Map::new();
        caps.insert("VAR".to_string(), capture_node("FooBar"));
        let matches = vec![match_with_captures(caps)];
        let kept =
            apply_constraints(&matches, &constraints(&[("VAR", "^[A-Z]")])).expect("no error");
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn nonmatching_constraint_drops_match() {
        let mut caps = serde_json::Map::new();
        caps.insert("VAR".to_string(), capture_node("FooBar"));
        let matches = vec![match_with_captures(caps)];
        let kept =
            apply_constraints(&matches, &constraints(&[("VAR", "^[a-z]")])).expect("no error");
        assert!(kept.is_empty());
    }

    #[test]
    fn missing_capture_drops_match() {
        let mut caps = serde_json::Map::new();
        caps.insert("OTHER".to_string(), capture_node("x"));
        let matches = vec![match_with_captures(caps)];
        let kept =
            apply_constraints(&matches, &constraints(&[("VAR", "^[A-Z]")])).expect("no error");
        assert!(kept.is_empty());
    }

    #[test]
    fn all_constraints_must_match_and_semantics() {
        let mut caps = serde_json::Map::new();
        caps.insert("VAR".to_string(), capture_node("FooBar"));
        caps.insert("TARGET".to_string(), capture_node("baz"));
        let matches = vec![match_with_captures(caps)];
        // VAR matches, TARGET does not → whole match dropped (AND semantics).
        let kept = apply_constraints(
            &matches,
            &constraints(&[("VAR", "^[A-Z]"), ("TARGET", "^[A-Z]")]),
        )
        .expect("no error");
        assert!(kept.is_empty());
    }

    #[test]
    fn variadic_capture_constrains_joined_text() {
        let mut caps = serde_json::Map::new();
        caps.insert(
            "ARGS".to_string(),
            serde_json::json!([capture_node("foo"), capture_node("Bar")]),
        );
        let matches = vec![match_with_captures(caps)];
        // Joined text "fooBar" — only the mixed-case regex matches.
        let kept =
            apply_constraints(&matches, &constraints(&[("ARGS", "^fooBar$")])).expect("no error");
        assert_eq!(kept.len(), 1);
        let dropped =
            apply_constraints(&matches, &constraints(&[("ARGS", "^foo$")])).expect("no error");
        assert!(dropped.is_empty());
    }

    #[test]
    fn invalid_regex_returns_api_error() {
        let mut caps = serde_json::Map::new();
        caps.insert("VAR".to_string(), capture_node("x"));
        let matches = vec![match_with_captures(caps)];
        let err = apply_constraints(&matches, &constraints(&[("VAR", "([unclosed")]))
            .expect_err("invalid regex errors");
        assert_eq!(err.code, "invalid_constraint");
        assert!(err.message.contains("VAR"));
    }

    #[test]
    fn empty_constraints_passthrough() {
        let matches = vec![match_with_captures(serde_json::Map::new())];
        let kept = apply_constraints(&matches, &HashMap::new()).expect("no error");
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn capture_names_sigil_order_and_variadic() {
        // `$$$` is checked before `$`: `$$$ARGS` is one variadic capture,
        // not `$` + `$$ARGS`.
        assert_eq!(capture_names("f($$$ARGS)"), vec!["ARGS"]);
        assert_eq!(capture_names("$$$A$B"), vec!["A", "B"]);
    }

    #[test]
    fn capture_names_bare_dollar_and_empty_name_are_not_captures() {
        assert!(capture_names("a $ b").is_empty(), "$ followed by space");
        assert!(capture_names("$$$ tail").is_empty(), "bare $$$");
        assert!(
            capture_names("cost $; end").is_empty(),
            "$ followed by punctuation"
        );
        assert!(capture_names("no tokens").is_empty());
        // `$5` is a name run (digits are name chars): the token is `5`.
        assert_eq!(capture_names("cost is $5 total"), vec!["5"]);
    }

    #[test]
    fn capture_names_name_run_termination() {
        // `$A_B` is one name `A_B` (underscores are name chars); `$A;` ends
        // at the punctuation; `$AB` is one name `AB`, never `A` + literal `B`.
        assert_eq!(capture_names("$A_B"), vec!["A_B"]);
        assert_eq!(capture_names("x$A;y"), vec!["A"]);
        assert_eq!(capture_names("$AB"), vec!["AB"]);
        assert_eq!(capture_names("$A2"), vec!["A2"], "digits are name chars");
    }

    #[test]
    fn capture_names_utf8_adjacency_and_dedup() {
        // Multi-byte UTF-8 around sigils never splits a char; first
        // occurrence order is preserved for repeated names.
        assert_eq!(
            capture_names("s($MSG) — ünïcode $OTHER"),
            vec!["MSG", "OTHER"]
        );
        assert_eq!(
            capture_names("$MSG then $MSG again"),
            vec!["MSG"],
            "duplicate name collected once, first-occurrence position"
        );
    }

    #[test]
    fn validate_rewrite_template_name_run_edge_is_rejected() {
        // Pattern declares `$A`; rewrite references `$A_B` — the name run
        // makes that a *different* capture, so the rewrite is rejected
        // (and vice versa: `$A` is not declared by a pattern with `$A_B`).
        let err = validate_rewrite_template("log($A_B)", "log($A)").expect_err("A_B != A");
        assert!(
            err.contains("`A_B`"),
            "diagnostic names the offending capture: {err}"
        );
        let err = validate_rewrite_template("log($A)", "log($A_B)").expect_err("A != A_B");
        assert!(
            err.contains("`A`"),
            "diagnostic names the offending capture: {err}"
        );
    }

    #[test]
    fn validate_rewrite_template_cross_sigil_reference_is_accepted() {
        // Token-name comparison: either sigil form satisfies the check, so
        // a rewrite may legally reference a pattern capture with the other
        // sigil (documented load-time-validation decision).
        assert!(validate_rewrite_template("f($$$ARGS)", "f($ARGS)").is_ok());
        assert!(validate_rewrite_template("f($ARGS)", "f($$$ARGS)").is_ok());
        assert!(validate_rewrite_template("f($A)", "f($A)").is_ok());
    }
    mod pipeline {
        use super::*;
        use crate::extract;
        use crate::model::{EntityKind, FileMeta};
        use crate::parse::parse_source;
        use crate::rules::{Rule, RuleKind};
        use std::fs;
        use std::path::{Path, PathBuf};

        const FIXTURE: &str = r#"
async function fetchData(): Promise<void> {
  console.log("async log");
}

function render(): void {
  console.log("sync log");
}
"#;

        fn tempdir(tag: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("varde-pipeline-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp dir creates");
            dir
        }

        fn rule(id: &str, pattern: &str, constraints: Option<HashMap<String, String>>) -> Rule {
            Rule {
                id: id.to_string(),
                kind: RuleKind::Pattern,
                severity: crate::rules::Severity::Warning,
                message: format!("{id} fired"),
                name: None,
                description: None,
                remediation: None,
                pattern: Some(pattern.to_string()),
                query: None,
                thresholds: None,
                strings: None,
                constraints,
                fix: None,
                rewrite: None,
                languages: None,
                exclude_test_paths: None,
                exclude_tooling_paths: None,
                test: None,
            }
        }

        /// Persist a DB whose entities mirror the fixture's functions with
        /// realistic byte spans and the given async flags.
        fn persist_fixture_db(repo: &Path, file_path: &Path) -> PathBuf {
            let parsed = parse_source(&ast_grep_language::SupportLang::TypeScript, FIXTURE);
            assert!(!parsed.has_error(), "fixture parses");
            let extracted = extract::extract(&parsed, 0).entities;
            let mut entities = Vec::new();
            // type-hierarchy: unchanged (test fixture builds Function-only entities to test async-flag pattern rules)
            for e in extracted
                .into_iter()
                .filter(|e| e.kind == EntityKind::Function)
            {
                let mut e = e;
                e.is_async = Some(e.name == "fetchData");
                entities.push(e);
            }
            let db_path = repo.join("index.db");
            let output = vec![crate::model::ExtractOutput {
                entities,
                symbols: vec![],
                diagnostics: vec![],
                files: vec![file_path.display().to_string()],
                file_meta: vec![FileMeta {
                    mtime: 0,
                    size: 0,
                    content_hash: "0".repeat(16),
                }],
            }];
            let graph = crate::resolve::ResolvedGraph {
                nodes: vec![],
                edges: vec![],
                communities: vec![],
                clone_bands: vec![],
            };
            crate::persist::persist(&db_path, &output, &graph, repo).expect("persist succeeds");
            db_path
        }

        #[test]
        fn constraints_and_is_async_correlation_end_to_end() {
            let repo = tempdir("e2e");
            let file_path = repo.join("main.ts");
            fs::write(&file_path, FIXTURE).expect("fixture writes");
            let db_path = persist_fixture_db(&repo, &file_path);
            let conn = crate::db::open(&db_path).expect("db opens");

            let mut constraints = HashMap::new();
            constraints.insert("MSG".to_string(), "^\"async".to_string());
            let rules = vec![
                rule("async-only", "console.log($MSG)", Some(constraints)),
                rule("all-logs", "console.log($MSG)", None),
            ];

            let (findings, diagnostics) =
                run_pattern_rules(&rules, &repo, &conn).expect("pipeline succeeds");
            assert!(diagnostics.is_empty(), "no diagnostics: {diagnostics:?}");

            // Rule 1: the constraint keeps only the async console.log.
            let async_only: Vec<_> = findings
                .iter()
                .filter(|f| f.rule_id == "async-only")
                .collect();
            assert_eq!(
                async_only.len(),
                1,
                "constraint keeps exactly the async log: {findings:?}"
            );
            let evidence = async_only[0].evidence.as_object().expect("evidence object");
            let enclosing = evidence["enclosing_function"]
                .as_object()
                .expect("enclosing attached");
            assert_eq!(enclosing["name"], "fetchData");
            assert_eq!(enclosing["is_async"], true);

            // Rule 2: both logs fire, each carrying its own enclosing fact.
            let all: Vec<_> = findings
                .iter()
                .filter(|f| f.rule_id == "all-logs")
                .collect();
            assert_eq!(all.len(), 2, "both logs found: {findings:?}");
            for f in &all {
                let evidence = f.evidence.as_object().expect("evidence object");
                let enclosing = evidence["enclosing_function"]
                    .as_object()
                    .expect("enclosing attached");
                assert!(
                    matches!(enclosing["is_async"].as_bool(), Some(true) | Some(false)),
                    "correlation attached a boolean fact"
                );
            }

            let _ = fs::remove_dir_all(&repo);
            let _ = fs::remove_file(&db_path);
        }

        #[test]
        fn unusable_db_returns_api_error_not_empty_success() {
            let repo = tempdir("nodb");
            let file_path = repo.join("main.ts");
            fs::write(&file_path, FIXTURE).expect("fixture writes");
            // An empty file is openable but has no schema — correlation fails.
            let broken_db = repo.join("broken.db");
            fs::write(&broken_db, "").expect("empty db file writes");
            let conn = rusqlite::Connection::open(&broken_db).expect("conn opens");

            let rules = vec![rule("all-logs", "console.log($MSG)", None)];
            let err = run_pattern_rules(&rules, &repo, &conn).expect_err("no-DB is a hard error");
            assert_eq!(err.code, "db_error");

            let _ = fs::remove_dir_all(&repo);
        }

        #[test]
        fn invalid_pattern_rule_is_skip_and_reported() {
            let repo = tempdir("badpat");
            let file_path = repo.join("main.ts");
            fs::write(&file_path, FIXTURE).expect("fixture writes");
            let db_path = persist_fixture_db(&repo, &file_path);
            let conn = crate::db::open(&db_path).expect("db opens");

            let rules = vec![
                rule("broken", "(((", None),
                rule("all-logs", "console.log($MSG)", None),
            ];
            let (findings, diagnostics) =
                run_pattern_rules(&rules, &repo, &conn).expect("pipeline does not abort");
            assert_eq!(
                diagnostics.len(),
                1,
                "exactly the broken rule is reported: {diagnostics:?}"
            );
            assert_eq!(diagnostics[0].rule_id.as_deref(), Some("broken"));
            assert!(!diagnostics[0].reason.is_empty());
            assert!(
                findings.iter().any(|f| f.rule_id == "all-logs"),
                "valid rule's findings still returned"
            );

            let _ = fs::remove_dir_all(&repo);
            let _ = fs::remove_file(&db_path);
        }

        #[test]
        fn invalid_constraint_regex_yields_exactly_one_diagnostic_across_all_languages() {
            let repo = tempdir("badregex");
            let file_path = repo.join("main.ts");
            fs::write(&file_path, FIXTURE).expect("fixture writes");
            let db_path = persist_fixture_db(&repo, &file_path);
            let conn = crate::db::open(&db_path).expect("db opens");

            let mut constraints = HashMap::new();
            constraints.insert("MSG".to_string(), "([unclosed".to_string());
            // No `languages` scoping — the rule targets all 10 supported
            // languages. Before the fix, an invalid regex was rediscovered
            // once per language (up to 10 diagnostics + 10 wasted repo
            // scans); it must now fail fast with exactly one diagnostic.
            let rules = vec![rule(
                "broken-constraint",
                "console.log($MSG)",
                Some(constraints),
            )];
            let (findings, diagnostics) =
                run_pattern_rules(&rules, &repo, &conn).expect("pipeline does not abort");
            assert!(findings.is_empty());
            assert_eq!(
                diagnostics.len(),
                1,
                "invalid regex fails the whole rule once, not per language: {diagnostics:?}"
            );
            assert_eq!(diagnostics[0].rule_id.as_deref(), Some("broken-constraint"));
            assert!(diagnostics[0].reason.contains("MSG"));

            let _ = fs::remove_dir_all(&repo);
            let _ = fs::remove_file(&db_path);
        }

        #[test]
        fn unsupported_language_name_skips_rule_with_diagnostic() {
            let repo = tempdir("badlang");
            let file_path = repo.join("main.ts");
            fs::write(&file_path, FIXTURE).expect("fixture writes");
            let db_path = persist_fixture_db(&repo, &file_path);
            let conn = crate::db::open(&db_path).expect("db opens");

            let mut scoped = rule("scoped", "console.log($MSG)", None);
            scoped.languages = Some(vec!["cobol".to_string()]);
            let (findings, diagnostics) =
                run_pattern_rules(&[scoped], &repo, &conn).expect("pipeline succeeds");
            assert!(findings.is_empty());
            assert_eq!(diagnostics.len(), 1);
            assert!(diagnostics[0].reason.contains("cobol"));

            let _ = fs::remove_dir_all(&repo);
            let _ = fs::remove_file(&db_path);
        }

        #[test]
        fn empty_language_list_matches_omitted_language_list() {
            let repo = tempdir("empty-languages");
            let file_path = repo.join("main.ts");
            fs::write(&file_path, FIXTURE).expect("fixture writes");
            let db_path = persist_fixture_db(&repo, &file_path);
            let conn = crate::db::open(&db_path).expect("db opens");

            let omitted = rule("omitted-languages", "console.log($MSG)", None);
            let mut empty = rule("empty-languages", "console.log($MSG)", None);
            empty.languages = Some(Vec::new());

            let (omitted_findings, omitted_diagnostics) =
                run_pattern_rules(&[omitted], &repo, &conn).expect("pipeline succeeds");
            let (empty_findings, empty_diagnostics) =
                run_pattern_rules(&[empty], &repo, &conn).expect("pipeline succeeds");

            assert_eq!(empty_diagnostics, omitted_diagnostics);
            assert_eq!(empty_findings.len(), omitted_findings.len());
            assert_eq!(empty_findings.len(), 2, "both console calls match");

            let _ = fs::remove_dir_all(&repo);
            let _ = fs::remove_file(&db_path);
        }

        /// Persist a DB that knows the fixture files but has no entities
        /// (enclosing-entity correlation then simply attaches no evidence).
        fn persist_files_only(repo: &Path, files: &[PathBuf]) -> PathBuf {
            let db_path = repo.join("index.db");
            let output: Vec<crate::model::ExtractOutput> = files
                .iter()
                .map(|p| crate::model::ExtractOutput {
                    entities: vec![],
                    symbols: vec![],
                    diagnostics: vec![],
                    files: vec![p.display().to_string()],
                    file_meta: vec![crate::model::FileMeta {
                        mtime: 0,
                        size: 0,
                        content_hash: "0".repeat(16),
                    }],
                })
                .collect();
            let graph = crate::resolve::ResolvedGraph {
                nodes: vec![],
                edges: vec![],
                communities: vec![],
                clone_bands: vec![],
            };
            crate::persist::persist(&db_path, &output, &graph, repo).expect("persist succeeds");
            db_path
        }

        #[test]
        fn builtin_empty_catch_and_eval_rules_fire_end_to_end() {
            let repo = tempdir("builtin-pattern-rules");
            let ts = repo.join("sample.ts");
            fs::write(
                &ts,
                r#"function sample() {
  try { risky(); } catch (err) { }
  try { risky(); } catch (err) { /* intentional */ }
  try { risky(); } catch (err) { handle(err); }
  eval(userInput);
  obj.eval("not-a-match");
}
"#,
            )
            .expect("ts fixture writes");
            let py = repo.join("sample.py");
            fs::write(
                &py,
                r#"def sample():
    try:
        risky()
    except ValueError as e:
        pass
    try:
        risky()
    except KeyError as e:
        handle(e)
    try:
        risky()
    except OSError as e:
        # intentional: retry loop owns it
        pass
    eval(user_input)
"#,
            )
            .expect("py fixture writes");
            let swift = repo.join("sample.swift");
            fs::write(
                &swift,
                r#"func sample() {
    do { try risky() } catch { }
    do { try risky() } catch { note() }
    do { try risky() } catch let err { }
}
"#,
            )
            .expect("swift fixture writes");
            let db_path = persist_files_only(&repo, &[ts, py, swift]);
            let conn = crate::db::open(&db_path).expect("db opens");

            let (findings, diagnostics) =
                run_pattern_rules(&crate::rules::builtin_rules(), &repo, &conn)
                    .expect("pipeline succeeds");
            assert!(diagnostics.is_empty(), "no diagnostics: {diagnostics:?}");

            let count = |id: &str| findings.iter().filter(|f| f.rule_id == id).count();
            assert_eq!(count("empty-catch-block"), 1, "only the truly empty catch");
            assert_eq!(count("empty-except-block"), 1, "only the uncommented pass");
            assert_eq!(
                count("empty-catch-block-swift"),
                1,
                "only the empty do/catch"
            );
            assert_eq!(count("eval-usage"), 2, "one eval per ts/py fixture");
            assert_eq!(
                findings.len(),
                5,
                "no unexpected findings: {:?}",
                findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>()
            );

            // The SQL built-ins are not pattern rules; the pipeline must not
            // have touched them.
            assert!(
                findings
                    .iter()
                    .all(|f| f.rule_id != "churn-complexity-hotspot")
            );

            let _ = fs::remove_dir_all(&repo);
            let _ = fs::remove_file(&db_path);
        }

        #[test]
        fn builtin_debug_rules_fire_end_to_end() {
            let repo = tempdir("builtin-debug-rules");
            let rs = repo.join("sample.rs");
            fs::write(
                &rs,
                r#"fn sample() {
    let x = 42;
    dbg!(x);
    dbg!(x, x);
    println!("x = {}", x);
    println!("dbg: {}", x);
    let mut v = vec![1, 2];
    v.push(dbg!(v.len()));
}
"#,
            )
            .expect("rs fixture writes");
            let ts_content = r#"function a() {
  const x = 1;
  console.log(x);
  console.log("hi", x);
  console.warn(x);
  console.error(x);
  debugger;
}
class C {
  m() {
    debugger;
  }
}
"#;
            let ts = repo.join("sample.ts");
            fs::write(&ts, ts_content).expect("ts fixture writes");
            let js = repo.join("sample.js");
            fs::write(&js, ts_content).expect("js fixture writes");
            let tsx = repo.join("sample.tsx");
            fs::write(&tsx, ts_content).expect("tsx fixture writes");
            let db_path = persist_files_only(&repo, &[rs, ts, js, tsx]);
            let conn = crate::db::open(&db_path).expect("db opens");

            let (findings, diagnostics) =
                run_pattern_rules(&crate::rules::builtin_rules(), &repo, &conn)
                    .expect("pipeline succeeds");
            assert!(diagnostics.is_empty(), "no diagnostics: {diagnostics:?}");

            let count = |id: &str| findings.iter().filter(|f| f.rule_id == id).count();
            // dbg!(x), dbg!(x, x), dbg!(v.len()) — the println! variants do not match.
            assert_eq!(
                count("debug-macro-strict"),
                3,
                "every dbg! invocation fires"
            );
            // Two debugger statements per JS-family fixture, one pass per language.
            assert_eq!(
                count("debug-statement-strict"),
                6,
                "debugger fires per language pass"
            );
            // console.log fires; console.warn/console.error do not.
            assert_eq!(
                count("console-log-strict"),
                6,
                "console.log fires per language pass"
            );
            assert_eq!(
                findings.len(),
                15,
                "no unexpected findings: {:?}",
                findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>()
            );
            assert!(
                findings
                    .iter()
                    .all(|f| f.rule_id.starts_with("debug-") || f.rule_id == "console-log-strict"),
                "no other rule fires: {:?}",
                findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>()
            );

            let _ = fs::remove_dir_all(&repo);
            let _ = fs::remove_file(&db_path);
        }

        #[test]
        fn builtin_credential_rules_fire_end_to_end() {
            let repo = tempdir("builtin-credential-rules");
            let ts_content = r#"const API_KEY = "sk-9f8d7c6b5a4f3e2d1c0b9a8f7d6c5b4a";
let apiKey = "abcd1234abcd1234abcd1234";
const password = "hunter2";
const username = "alice";
const secret = process.env.SECRET;
const api_key = "your-api-key-here";
const client_secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
config.api_key = "sk-9f8d7c6b5a4f3e2d1c0b9a8f7d6c5b4a";
session_token = "1234567890abcdef1234567890abcdef";
"#;
            let ts = repo.join("sample.ts");
            fs::write(&ts, ts_content).expect("ts fixture writes");
            let js = repo.join("sample.js");
            fs::write(&js, ts_content).expect("js fixture writes");
            let tsx = repo.join("sample.tsx");
            fs::write(&tsx, ts_content).expect("tsx fixture writes");
            let py = repo.join("sample.py");
            fs::write(
                &py,
                r#"API_KEY = "9f8d7c6b5a4f3e2d1c0b9a8f"
password = "correcthorsebatterystaple"
token = compute_token()
api_key = "changeme"
client_id = "1234567890abcdef1234567890abcdef"
"#,
            )
            .expect("py fixture writes");
            let db_path = persist_files_only(&repo, &[ts, js, tsx, py]);
            let conn = crate::db::open(&db_path).expect("db opens");

            let (findings, diagnostics) =
                run_pattern_rules(&crate::rules::builtin_rules(), &repo, &conn)
                    .expect("pipeline succeeds");
            assert!(diagnostics.is_empty(), "no diagnostics: {diagnostics:?}");

            let count = |id: &str| findings.iter().filter(|f| f.rule_id == id).count();
            // Per JS-family file: API_KEY + apiKey + client_secret declarations,
            // plus the session_token reassignment. Placeholder/short/env/member
            // shapes must not fire.
            assert_eq!(
                count("hardcoded-credential-declaration"),
                9,
                "3 per JS-family file"
            );
            assert_eq!(
                count("hardcoded-credential-literal"),
                4,
                "1 per JS-family file + the python API_KEY"
            );
            assert_eq!(
                findings.len(),
                13,
                "no unexpected findings: {:?}",
                findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>()
            );
            assert!(
                findings
                    .iter()
                    .all(|f| f.rule_id.starts_with("hardcoded-credential-")),
                "only the credential rules fire: {:?}",
                findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>()
            );

            let _ = fs::remove_dir_all(&repo);
            let _ = fs::remove_file(&db_path);
        }
    }
}
