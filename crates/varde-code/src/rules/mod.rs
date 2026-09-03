//! Rule-pack schema: the TOML `[[rule]]` types shared by the loader and
//! both execution engines (pattern + SQL).
//!
//! A rule pack is a TOML file with one `[[rule]]` array-of-tables entry per
//! rule. Each rule carries a `kind = "pattern" | "sql"` discriminator; the
//! kind-specific payload lives in `pattern` (kind=pattern) or `query`
//! (kind=sql).

use serde::{Deserialize, Serialize};

pub mod correlate;
pub mod finding;
pub mod pattern;
pub mod rewrite;
pub mod sql;
pub mod suppress;
pub mod test_runner;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The two rule kinds, discriminated by the TOML `kind` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    /// Matches source at scan time via find_pattern's ast-grep matcher.
    Pattern,
    /// Queries the persisted intelligence DB via a read-only connection.
    Sql,
}

/// Rule severity, mirroring varde's TS severity convention. `Ord` ranks
/// `Error > Warning > Info` (derived `Ord` would rank by declaration order,
/// so it is implemented explicitly via a rank map).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl PartialOrd for Severity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Severity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl Severity {
    /// Resolve a severity level name case-insensitively.
    pub fn from_name(name: &str) -> Option<Severity> {
        match name.to_ascii_lowercase().as_str() {
            "error" => Some(Severity::Error),
            "warning" => Some(Severity::Warning),
            "info" => Some(Severity::Info),
            _ => None,
        }
    }

    /// Severity rank: `Error = 2 > Warning = 1 > Info = 0`.
    fn rank(self) -> u8 {
        match self {
            Severity::Error => 2,
            Severity::Warning => 1,
            Severity::Info => 0,
        }
    }
}

/// Case-insensitive `Severity` deserialization: `severity = "warning"`,
/// `"Warning"`, and `"WARNING"` all map to `Severity::Warning`.
fn deserialize_severity<'de, D>(deserializer: D) -> Result<Severity, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Severity::from_name(&raw)
        .ok_or_else(|| serde::de::Error::unknown_variant(&raw, &["error", "warning", "info"]))
}

/// One `[[rule]]` table from a rule pack.
///
/// Required fields (`id`, `kind`, `severity`, `message`) fail the whole
/// rule's deserialization when missing — the loader reports them as
/// skip-and-report `Diagnostic`s. Optional fields default to `None`.
/// Unrecognized extra fields are ignored (permissive), not rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub kind: RuleKind,
    #[serde(deserialize_with = "deserialize_severity")]
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// Kind-specific payload: the ast-grep-style pattern (kind=pattern).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Kind-specific payload: the SQL query (kind=sql).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Numeric thresholds, pinned by sql-rule-engine: each key binds as the
    /// named parameter `:key` in the rule's SQL query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thresholds: Option<HashMap<String, f64>>,
    /// String parameters, pinned by sql-rule-engine: each key binds as the
    /// named parameter `:key` in the rule's SQL query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strings: Option<HashMap<String, String>>,
    /// Per-capture regex constraints (kind=pattern): capture name → regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraints: Option<HashMap<String, String>>,
    /// Suggested-fix rewrite template — informational only, never applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Meta-variable rewrite template (kind=pattern): when `--apply` is
    /// passed, matched spans are replaced with this template after
    /// `$VAR`/`$$$VAR` substitution. Free-text `fix` guidance is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<String>,
    /// Language scoping for pattern rules: which `SupportLang`-resolvable
    /// names the rule targets. `None`/empty means "run against every file
    /// whose parse succeeds" (language-agnostic default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<String>>,
    /// Pattern rules only: when `true`, matches in files where
    /// `files.is_test_path` is set are dropped before becoming findings.
    /// SQL rules express the same exclusion directly in their query (see
    /// `low_fan_in_high_fan_out_file.toml`/`circular_import.toml`); pattern
    /// rules have no query to add a `WHERE` clause to, so this flag drives
    /// the same DB-backed check from `run_pattern_rule`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_test_paths: Option<bool>,
    /// Pattern rules only: when `true`, matches in files where
    /// `files.is_tooling_path` is set (benchmarks, dev/build scripts, docs
    /// generators — see db.rs) are dropped before becoming findings. Same
    /// mechanism as `exclude_test_paths`, separate flag: a rule may want one
    /// without the other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_tooling_paths: Option<bool>,
    /// `[[test]]` entries declared on this rule — self-tests validated by
    /// the (separate) test-runner tasks. Inert to `scan`'s
    /// `run_pattern_rules`/`run_sql_rules`. Parsed and validated manually
    /// (not via `#[serde]` on this field) so a single malformed entry can be
    /// skipped and reported without failing the whole rule, matching the
    /// loader's existing skip-and-report convention.
    #[serde(skip)]
    pub test: Option<Vec<TestCase>>,
}

/// One `[[test]]` entry declared on a rule — a self-test fixture for the
/// (separate) pattern/SQL test runners. `name` is required for every entry;
/// the remaining fields are kind-specific:
/// - pattern rules: `valid` (snippets that must NOT match), `invalid`
///   (snippets that must match), `expect_rewrite` (input → expected output,
///   requires the rule to also define `rewrite`);
/// - sql rules: `fixture` (inline `[test.fixture]` table of relative path →
///   file content, written to a temp dir and indexed), `expect_rows`
///   (expected query result rows, an array of tables compared against the
///   actual rows as an order-insensitive multiset).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestCase {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_rewrite: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_rows: Option<Vec<HashMap<String, serde_json::Value>>>,
}

/// A skipped/malformed rule, reported by the loader instead of failing the
/// whole load. Consumed by `scan` for its diagnostics output, not persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The rule's id when it could be determined (missing-field rules that
    /// fail deserialization may still carry an `id` in the TOML table).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    /// The rule-pack file the diagnostic came from.
    pub file: String,
    /// Human-readable reason for the skip.
    pub reason: String,
}

/// Where a rule-pack file lives — determines merge precedence (repo wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleScope {
    User,
    Repo,
}

/// A discovered rule-pack file, tagged with the scope it was found in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub scope: RuleScope,
}

/// User-scope rule directory, resolved per call so tests can override it:
/// `VARDE_USER_RULES_DIR` when set, else `$HOME/.config/varde-code/rules/`
/// (the same HOME-based convention `db::path::repo_db_path` uses).
pub fn user_rules_dir() -> PathBuf {
    match std::env::var_os("VARDE_USER_RULES_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = std::env::var_os("HOME").unwrap_or_else(|| "~".into());
            PathBuf::from(home)
                .join(".config")
                .join("varde-code")
                .join("rules")
        }
    }
}

/// Discover rule-pack TOML files in both scopes.
///
/// User scope: `~/.config/varde-code/rules/`. Repo scope:
/// `<repo_root>/.varde-code/rules/`. A missing directory in either scope is
/// not an error — it contributes zero files. Ordering is deterministic:
/// user scope first, then repo scope, each sorted by path, so the merge
/// step's "first-loaded wins" resolution is stable across runs and
/// platforms.
pub fn discover(repo_root: &Path, user_dir: Option<&Path>) -> Vec<DiscoveredFile> {
    let mut files = Vec::new();
    if let Some(dir) = user_dir {
        collect_toml_files(dir, RuleScope::User, &mut files);
    }
    collect_toml_files(
        &repo_root.join(".varde-code").join("rules"),
        RuleScope::Repo,
        &mut files,
    );
    files
}

fn collect_toml_files(dir: &Path, scope: RuleScope, out: &mut Vec<DiscoveredFile>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // Missing/unreadable scope dir → zero files, not an error.
        Err(_) => return,
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(err) => {
                // Mid-iteration I/O error on one entry (e.g. a dir removed
                // concurrently, a permissions error on one child) — unlike a
                // wholly unreadable scope dir above, this drops just that
                // entry, so it's worth a diagnostic even though `discover`
                // has no per-file `Diagnostic` channel to surface it through.
                tracing::warn!(dir = %dir.display(), %err, "skipping unreadable rule-pack directory entry");
                None
            }
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    out.extend(paths.into_iter().map(|path| DiscoveredFile { path, scope }));
}

/// A rule plus the pack file it came from, threaded through load so the
/// merge step can name the source file of duplicate-id skips.
#[derive(Debug, Clone)]
pub struct LoadedRule {
    pub rule: Rule,
    pub file: String,
}

/// Validate the kind-specific required fields of a deserialized rule.
///
/// `pattern` is required for `kind=pattern`, `query` for `kind=sql` — a rule
/// missing its kind's payload field is a skip-and-report error, not a whole
/// load failure.
fn validate_kind_fields(rule: &Rule) -> Result<(), String> {
    match rule.kind {
        RuleKind::Pattern if rule.pattern.is_none() => {
            Err("rule kind=pattern requires a `pattern` field".to_string())
        }
        RuleKind::Pattern => {
            // Load-time rewrite validation: every `$VAR`/`$$$VAR` in the
            // rewrite template must name a capture declared in pattern.
            if let (Some(pattern), Some(rewrite)) = (&rule.pattern, &rule.rewrite) {
                crate::rules::pattern::validate_rewrite_template(rewrite, pattern)?;
            }
            Ok(())
        }
        RuleKind::Sql if rule.query.is_none() => {
            Err("rule kind=sql requires a `query` field".to_string())
        }
        // ARCHITECTURE-001: a SQL rule carrying a `rewrite` field would
        // silently drop it (the apply flow filters kind=pattern), so it is
        // rejected at load with the usual skip-and-report diagnostic — a
        // broken rule is a `Diagnostic`, never a silent no-op. SQL rules
        // keep `fix` as guidance text; rewrite is pattern rules only.
        RuleKind::Sql if rule.rewrite.is_some() => {
            Err("rewrite is only supported on kind=pattern rules (SQL findings are not anchored to a single source span)".to_string())
        }
        _ => Ok(()),
    }
}

/// Validate one already-deserialized `TestCase` against its rule's kind and
/// fields, mirroring `validate_kind_fields`'s skip-and-report convention but
/// per test entry rather than per rule.
fn validate_test_case(tc: &TestCase, rule: &Rule) -> Result<(), String> {
    match rule.kind {
        RuleKind::Pattern => {
            if tc.fixture.is_some() {
                return Err(format!(
                    "test `{}`: `fixture` is only valid on kind=sql rules",
                    tc.name
                ));
            }
            if tc.expect_rows.is_some() {
                return Err(format!(
                    "test `{}`: `expect_rows` is only valid on kind=sql rules",
                    tc.name
                ));
            }
            if tc.expect_rewrite.is_some() && rule.rewrite.is_none() {
                return Err(format!(
                    "test `{}`: `expect_rewrite` requires the rule to define `rewrite`",
                    tc.name
                ));
            }
        }
        RuleKind::Sql => {
            if tc.valid.is_some() {
                return Err(format!(
                    "test `{}`: `valid` is only valid on kind=pattern rules",
                    tc.name
                ));
            }
            if tc.invalid.is_some() {
                return Err(format!(
                    "test `{}`: `invalid` is only valid on kind=pattern rules",
                    tc.name
                ));
            }
            if tc.expect_rewrite.is_some() {
                return Err(format!(
                    "test `{}`: `expect_rewrite` is only valid on kind=pattern rules",
                    tc.name
                ));
            }
        }
    }
    Ok(())
}

/// Parse a rule table's `[[test]]` array-of-tables entries against the
/// already-validated `rule`. Each entry is parsed and validated
/// independently: a malformed entry (missing `name`, wrong TOML type, or a
/// kind-mismatched field) is dropped and reported as a `Diagnostic`; the
/// rule's other test entries still load, consistent with the loader's
/// skip-and-report convention for malformed rules.
fn parse_test_entries(
    entry: &toml::Value,
    rule: &Rule,
    file: &str,
) -> (Vec<TestCase>, Vec<Diagnostic>) {
    let mut cases = Vec::new();
    let mut diagnostics = Vec::new();
    let Some(test_entries) = entry.get("test").and_then(|v| v.as_array()) else {
        return (cases, diagnostics);
    };
    for test_entry in test_entries {
        match test_entry.clone().try_into::<TestCase>() {
            Ok(tc) => match validate_test_case(&tc, rule) {
                Ok(()) => cases.push(tc),
                Err(reason) => diagnostics.push(Diagnostic {
                    rule_id: Some(rule.id.clone()),
                    file: file.to_string(),
                    reason,
                }),
            },
            Err(e) => diagnostics.push(Diagnostic {
                rule_id: Some(rule.id.clone()),
                file: file.to_string(),
                reason: format!("malformed [[test]] entry: {e}"),
            }),
        }
    }
    (cases, diagnostics)
}

/// Parse one rule-pack file into rules and skip-and-report diagnostics.
///
/// Failure modes, in order:
/// - unreadable file → one file-level `Diagnostic`, zero rules;
/// - file not valid TOML → one file-level `Diagnostic`, zero rules;
/// - a `[[rule]]` entry missing required fields or its kind's payload → one
///   per-rule `Diagnostic` (naming the rule's id when determinable), the
///   file's other rules still load;
/// - unknown/extra fields → ignored (permissive), the rule loads.
pub fn parse_pack_file(path: &Path) -> (Vec<Rule>, Vec<Diagnostic>) {
    let (loaded, diagnostics) = parse_loaded(path);
    (loaded.into_iter().map(|lr| lr.rule).collect(), diagnostics)
}

fn parse_loaded(path: &Path) -> (Vec<LoadedRule>, Vec<Diagnostic>) {
    let file = path.display().to_string();
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) => {
            return (
                Vec::new(),
                vec![Diagnostic {
                    rule_id: None,
                    file,
                    reason: format!("unreadable rule pack: {e}"),
                }],
            );
        }
    };
    parse_pack_str(&contents, &file)
}

/// Parse one rule-pack's contents into rules and skip-and-report
/// diagnostics — shared by file-based packs (`parse_loaded`) and the
/// embedded built-in packs (`builtin_loaded`).
fn parse_pack_str(contents: &str, file: &str) -> (Vec<LoadedRule>, Vec<Diagnostic>) {
    let doc: toml::Value = match toml::from_str(contents) {
        Ok(doc) => doc,
        Err(e) => {
            return (
                Vec::new(),
                vec![Diagnostic {
                    rule_id: None,
                    file: file.to_string(),
                    reason: format!("invalid TOML: {e}"),
                }],
            );
        }
    };

    let mut rules = Vec::new();
    let mut diagnostics = Vec::new();
    let Some(entries) = doc.get("rule").and_then(|v| v.as_array()) else {
        // A pack with no `[[rule]]` tables is valid but empty.
        return (rules, diagnostics);
    };

    for entry in entries {
        match entry.clone().try_into() {
            Ok(rule) => match validate_kind_fields(&rule) {
                Ok(()) => {
                    let mut rule: Rule = rule;
                    let (test_cases, test_diagnostics) = parse_test_entries(entry, &rule, file);
                    diagnostics.extend(test_diagnostics);
                    rule.test = if test_cases.is_empty() {
                        None
                    } else {
                        Some(test_cases)
                    };
                    rules.push(LoadedRule {
                        rule,
                        file: file.to_string(),
                    });
                }
                Err(reason) => diagnostics.push(Diagnostic {
                    rule_id: Some(rule.id),
                    file: file.to_string(),
                    reason,
                }),
            },
            Err(e) => diagnostics.push(Diagnostic {
                rule_id: toml::Value::clone(entry)
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| id.to_string()),
                file: file.to_string(),
                reason: e.to_string(),
            }),
        }
    }
    (rules, diagnostics)
}

/// Merge loaded rules by id across scopes.
///
/// Resolution rules (deterministic — discovery order is sorted):
/// - cross-scope conflict (same id in user and repo): the repo-scoped rule
///   wins, silently — expected behavior, not a diagnostic;
/// - same-scope conflict (duplicate id within one scope): the first-loaded
///   rule is kept, each later duplicate is skipped and reported as a
///   `Diagnostic` naming the duplicate's source file.
///
/// User-scope rules keep their position when repo rules replace them;
/// repo-only rules append in discovery order.
pub fn merge(user: Vec<LoadedRule>, repo: Vec<LoadedRule>) -> (Vec<Rule>, Vec<Diagnostic>) {
    let mut rules: Vec<Rule> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    // Ids whose current position was established by the user scope —
    // a repo rule hitting one of these is a silent cross-scope replace.
    let mut user_owned: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut diagnostics = Vec::new();

    for LoadedRule { rule, file } in user {
        if positions.contains_key(&rule.id) {
            diagnostics.push(Diagnostic {
                rule_id: Some(rule.id.clone()),
                file,
                reason: format!(
                    "duplicate rule id `{}` in user scope; first-loaded rule kept",
                    rule.id
                ),
            });
        } else {
            user_owned.insert(rule.id.clone());
            positions.insert(rule.id.clone(), rules.len());
            rules.push(rule);
        }
    }

    for LoadedRule { rule, file } in repo {
        if let Some(&pos) = positions.get(&rule.id) {
            if user_owned.remove(&rule.id) {
                // Cross-scope conflict: repo wins, silently.
                rules[pos] = rule;
            } else {
                diagnostics.push(Diagnostic {
                    rule_id: Some(rule.id.clone()),
                    file,
                    reason: format!(
                        "duplicate rule id `{}` in repo scope; first-loaded rule kept",
                        rule.id
                    ),
                });
            }
        } else {
            positions.insert(rule.id.clone(), rules.len());
            rules.push(rule);
        }
    }

    (rules, diagnostics)
}

/// Rule packs compiled into the binary — shipped defaults, not user-authored
/// TOML on disk. Each entry is `include_str!`'d at compile time and parsed
/// with the same validation as file-based packs.
const BUILTIN_RULE_PACKS: &[(&str, &str)] = &[
    (
        "<builtin>/complexity.toml",
        include_str!("builtin/complexity.toml"),
    ),
    (
        "<builtin>/empty_catch_block.toml",
        include_str!("builtin/empty_catch_block.toml"),
    ),
    (
        "<builtin>/eval_usage.toml",
        include_str!("builtin/eval_usage.toml"),
    ),
    (
        "<builtin>/duplicate_code_clone.toml",
        include_str!("builtin/duplicate_code_clone.toml"),
    ),
    (
        "<builtin>/debug_statement_strict.toml",
        include_str!("builtin/debug_statement_strict.toml"),
    ),
    (
        "<builtin>/hardcoded_credential_literal.toml",
        include_str!("builtin/hardcoded_credential_literal.toml"),
    ),
    (
        "<builtin>/low_fan_in_high_fan_out_file.toml",
        include_str!("builtin/low_fan_in_high_fan_out_file.toml"),
    ),
    (
        "<builtin>/circular_import.toml",
        include_str!("builtin/circular_import.toml"),
    ),
    (
        "<builtin>/vertical_slice_sprawl.toml",
        include_str!("builtin/vertical_slice_sprawl.toml"),
    ),
    ("<builtin>/solid.toml", include_str!("builtin/solid.toml")),
];

/// Parse every built-in rule pack. Diagnostics here would indicate a bug in
/// a shipped pack (never a user-input error), but are still surfaced rather
/// than silently dropped.
fn builtin_loaded() -> (Vec<LoadedRule>, Vec<Diagnostic>) {
    let mut rules = Vec::new();
    let mut diagnostics = Vec::new();
    for (label, contents) in BUILTIN_RULE_PACKS {
        let (pack_loaded, pack_diags) = parse_pack_str(contents, label);
        rules.extend(pack_loaded);
        diagnostics.extend(pack_diags);
    }
    (rules, diagnostics)
}

/// All shipped built-in rules (post-parse, post-validation).
pub fn builtin_rules() -> Vec<Rule> {
    builtin_loaded().0.into_iter().map(|lr| lr.rule).collect()
}

/// Outcome of seeding a single built-in pack file into `target_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedResult {
    pub file_name: String,
    pub path: PathBuf,
    pub written: bool,
    /// `true` when a file already existed and `force` was false, so it was
    /// left untouched (the common case on a repeat `rules_seed` run).
    pub skipped_existing: bool,
}

/// Write every built-in rule pack out to `target_dir` as editable TOML
/// files, so users can seed-and-customize instead of only overriding by id.
///
/// Existing files are left untouched unless `force` is true — safe to
/// re-run after adding a new built-in pack without clobbering local edits.
pub fn seed_builtin_rules(target_dir: &Path, force: bool) -> std::io::Result<Vec<SeedResult>> {
    std::fs::create_dir_all(target_dir)?;
    let mut results = Vec::with_capacity(BUILTIN_RULE_PACKS.len());
    for (label, contents) in BUILTIN_RULE_PACKS {
        let file_name = label
            .rsplit('/')
            .next()
            .expect("builtin label always has a filename component")
            .to_string();
        let path = target_dir.join(&file_name);
        let exists = path.exists();
        if exists && !force {
            results.push(SeedResult {
                file_name,
                path,
                written: false,
                skipped_existing: true,
            });
            continue;
        }
        std::fs::write(&path, contents)?;
        results.push(SeedResult {
            file_name,
            path,
            written: true,
            skipped_existing: false,
        });
    }
    Ok(results)
}

/// Outcome of removing a single seeded built-in pack file from `target_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnseedResult {
    pub file_name: String,
    pub path: PathBuf,
    pub removed: bool,
    /// `true` when the file's contents no longer match the shipped built-in
    /// (i.e. it was customized) and `force` was false, so it was left alone.
    pub skipped_modified: bool,
}

/// Delete previously seeded built-in rule pack files from `target_dir`.
///
/// Only files whose name matches a shipped built-in pack are considered;
/// anything else in `target_dir` (custom rule files) is left untouched. A
/// seeded file whose contents were edited since seeding is left in place
/// unless `force` is true, so an unseed doesn't silently discard local
/// customizations.
pub fn unseed_builtin_rules(target_dir: &Path, force: bool) -> std::io::Result<Vec<UnseedResult>> {
    let mut results = Vec::with_capacity(BUILTIN_RULE_PACKS.len());
    for (label, contents) in BUILTIN_RULE_PACKS {
        let file_name = label
            .rsplit('/')
            .next()
            .expect("builtin label always has a filename component")
            .to_string();
        let path = target_dir.join(&file_name);
        if !path.exists() {
            continue;
        }
        let on_disk = std::fs::read_to_string(&path)?;
        let modified = on_disk != *contents;
        if modified && !force {
            results.push(UnseedResult {
                file_name,
                path,
                removed: false,
                skipped_modified: true,
            });
            continue;
        }
        std::fs::remove_file(&path)?;
        results.push(UnseedResult {
            file_name,
            path,
            removed: true,
            skipped_modified: false,
        });
    }
    Ok(results)
}

/// Discover, parse, validate, and merge every rule pack for `repo_root`.
///
/// Runs the full pipeline: discovery (user scope from
/// `VARDE_USER_RULES_DIR`/`~/.config/varde-code/rules/`, repo scope from
/// `<repo_root>/.varde-code/rules/`) → per-file parse/validate → merge by id
/// (repo wins cross-scope, first-loaded wins same-scope). Every diagnostic
/// from each stage is accumulated into the returned list. Never fails: a
/// repo with zero rule packs in either scope yields `(vec![], vec![])` plus
/// whatever built-in rules ship with the binary.
///
/// Built-in rules are lowest priority: a user- or repo-scoped rule with the
/// same id silently overrides its built-in counterpart (no diagnostic — this
/// is the expected way to disable/replace a default).
pub fn load_rules(repo_root: &Path) -> (Vec<Rule>, Vec<Diagnostic>) {
    let user_dir = user_rules_dir();
    let files = discover(repo_root, Some(&user_dir));

    let mut user_loaded = Vec::new();
    let mut repo_loaded = Vec::new();
    let mut diagnostics = Vec::new();
    for file in files {
        let (loaded, file_diags) = parse_loaded(&file.path);
        diagnostics.extend(file_diags);
        match file.scope {
            RuleScope::User => user_loaded.extend(loaded),
            RuleScope::Repo => repo_loaded.extend(loaded),
        }
    }

    let (mut rules, merge_diags) = merge(user_loaded, repo_loaded);
    diagnostics.extend(merge_diags);

    let (builtin, builtin_diags) = builtin_loaded();
    diagnostics.extend(builtin_diags);
    let mut seen: std::collections::HashSet<String> = rules.iter().map(|r| r.id.clone()).collect();
    for LoadedRule { rule, .. } in builtin {
        if seen.insert(rule.id.clone()) {
            rules.push(rule);
        }
    }
    (rules, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A throwaway tempdir unique per test process (project convention —
    /// no tempfile crate; see `db.rs`'s schema tests).
    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("varde-rules-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir creates");
        dir
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir creates");
        }
        fs::write(path, contents).expect("fixture writes");
    }

    fn minimal_toml() -> &'static str {
        r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"
"#
    }

    #[test]
    fn seed_then_unseed_round_trips() {
        let dir = tempdir("seed-round-trip");

        let seeded = seed_builtin_rules(&dir, false).expect("seed succeeds");
        assert!(!seeded.is_empty(), "ships at least one built-in pack");
        assert!(seeded.iter().all(|r| r.written && !r.skipped_existing));
        for r in &seeded {
            assert!(r.path.exists(), "{} written to disk", r.file_name);
        }

        let removed = unseed_builtin_rules(&dir, false).expect("unseed succeeds");
        assert_eq!(removed.len(), seeded.len());
        assert!(removed.iter().all(|r| r.removed && !r.skipped_modified));
        for r in &removed {
            assert!(!r.path.exists(), "{} deleted from disk", r.file_name);
        }
    }

    #[test]
    fn unseed_skips_locally_modified_files_unless_forced() {
        let dir = tempdir("unseed-modified");
        let seeded = seed_builtin_rules(&dir, false).expect("seed succeeds");
        let first = &seeded[0];
        write(&first.path, "# locally customized\n");

        let removed = unseed_builtin_rules(&dir, false).expect("unseed succeeds");
        let entry = removed
            .iter()
            .find(|r| r.file_name == first.file_name)
            .expect("entry present");
        assert!(
            !entry.removed && entry.skipped_modified,
            "modified file left alone"
        );
        assert!(
            first.path.exists(),
            "modified file survives non-forced unseed"
        );

        let forced = unseed_builtin_rules(&dir, true).expect("forced unseed succeeds");
        let entry = forced
            .iter()
            .find(|r| r.file_name == first.file_name)
            .expect("entry present");
        assert!(
            entry.removed && !entry.skipped_modified,
            "force discards local edits"
        );
        assert!(!first.path.exists(), "modified file removed under force");
    }

    #[test]
    fn unseed_ignores_files_not_present() {
        let dir = tempdir("unseed-absent");
        let removed = unseed_builtin_rules(&dir, false).expect("unseed on empty dir succeeds");
        assert!(
            removed.is_empty(),
            "nothing to remove when nothing was seeded"
        );
    }

    #[test]
    fn minimal_rule_deserializes_with_optionals_none() {
        let doc: toml::Value = toml::from_str(minimal_toml()).expect("document parses");
        let rules = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .expect("rule array");
        let rule: Rule = rules[0].clone().try_into().expect("rule deserializes");

        assert_eq!(rule.id, "no-console");
        assert_eq!(rule.kind, RuleKind::Pattern);
        assert_eq!(rule.severity, Severity::Warning);
        assert_eq!(rule.message, "console call detected");
        assert_eq!(rule.pattern.as_deref(), Some("console.log($MSG)"));
        assert_eq!(rule.name, None);
        assert_eq!(rule.description, None);
        assert_eq!(rule.remediation, None);
        assert_eq!(rule.query, None);
        assert_eq!(rule.thresholds, None);
        assert_eq!(rule.strings, None);
        assert_eq!(rule.constraints, None);
        assert_eq!(rule.fix, None);
        assert_eq!(rule.rewrite, None);
    }

    #[test]
    fn parse_accepts_rewrite_referencing_only_pattern_captures() {
        let dir = tempdir("rewrite-valid");
        let pack = dir.join("valid.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "log-to-info"
kind = "pattern"
severity = "warning"
message = "use console.info"
pattern = "console.log($MSG)"
rewrite = "console.info($MSG)"

[[rule]]
id = "variadic-log"
kind = "pattern"
severity = "warning"
message = "use console.info"
pattern = "console.log($$$ARGS)"
rewrite = "console.info($$$ARGS)"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 2, "both rules load: {diagnostics:?}");
        assert!(diagnostics.is_empty(), "no diagnostics: {diagnostics:?}");
        assert_eq!(rules[0].rewrite.as_deref(), Some("console.info($MSG)"));
        assert_eq!(rules[1].rewrite.as_deref(), Some("console.info($$$ARGS)"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_rejects_rewrite_referencing_unknown_capture_and_keeps_other_rules() {
        let dir = tempdir("rewrite-invalid");
        let pack = dir.join("invalid.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "bad-rewrite"
kind = "pattern"
severity = "warning"
message = "typo in capture name"
pattern = "console.log($MSG)"
rewrite = "console.info($MESG)"

[[rule]]
id = "good-rule"
kind = "pattern"
severity = "warning"
message = "fine"
pattern = "debugger;"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(
            rules.len(),
            1,
            "bad-rewrite skipped, good-rule kept: {diagnostics:?}"
        );
        assert_eq!(rules[0].id, "good-rule");
        assert_eq!(
            diagnostics.len(),
            1,
            "exactly one diagnostic: {diagnostics:?}"
        );
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("bad-rewrite"));
        assert!(
            diagnostics[0].reason.contains("MESG"),
            "diagnostic identifies the missing capture: {}",
            diagnostics[0].reason
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_rejects_sql_rule_with_rewrite_field() {
        // ARCHITECTURE-001: a SQL rule carrying `rewrite` would silently
        // drop it (the apply flow filters kind=pattern). The loader rejects
        // it with a skip-and-report diagnostic naming the rule; sibling
        // rules still load.
        let dir = tempdir("rewrite-sql");
        let pack = dir.join("sql-rewrite.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "sql-with-rewrite"
kind = "sql"
severity = "error"
message = "file exceeds complexity budget"
query = "SELECT file, line FROM entities WHERE complexity > :max_lines"
rewrite = "SELECT 1"

[[rule]]
id = "good-sql"
kind = "sql"
severity = "warning"
message = "fine"
query = "SELECT 1"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(
            rules.len(),
            1,
            "sql-with-rewrite skipped, good-sql kept: {diagnostics:?}"
        );
        assert_eq!(rules[0].id, "good-sql");
        assert_eq!(
            diagnostics.len(),
            1,
            "exactly one diagnostic: {diagnostics:?}"
        );
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("sql-with-rewrite"));
        assert!(
            diagnostics[0].reason.contains("kind=pattern"),
            "diagnostic explains the constraint: {}",
            diagnostics[0].reason
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_rule_without_rewrite_skips_rewrite_validation() {
        // No `rewrite` field → load behavior unchanged (no validation runs).
        let dir = tempdir("rewrite-absent");
        let pack = dir.join("plain.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "plain"
kind = "pattern"
severity = "warning"
message = "no rewrite"
pattern = "console.log($MSG)"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "plain");
        assert!(diagnostics.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_populates_test_field_from_test_entries() {
        let dir = tempdir("test-entries-valid");
        let pack = dir.join("with-tests.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"

[[rule.test]]
name = "flags console.log"
invalid = ["console.log(1)"]
valid = ["console.info(1)"]
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(rules.len(), 1);
        let tests = rules[0].test.as_ref().expect("test cases populated");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "flags console.log");
        assert_eq!(
            tests[0].invalid.as_deref(),
            Some(&["console.log(1)".to_string()][..])
        );
        assert_eq!(
            tests[0].valid.as_deref(),
            Some(&["console.info(1)".to_string()][..])
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_drops_test_entry_missing_name_and_keeps_rule() {
        let dir = tempdir("test-entries-missing-name");
        let pack = dir.join("missing-name.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"

[[rule.test]]
invalid = ["console.log(1)"]
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1, "malformed test entry doesn't drop the rule");
        assert!(rules[0].test.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("no-console"));
        assert!(
            diagnostics[0].reason.contains("name"),
            "{:?}",
            diagnostics[0].reason
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_drops_test_entry_with_sql_only_field_on_pattern_rule() {
        let dir = tempdir("test-entries-kind-mismatch-pattern");
        let pack = dir.join("kind-mismatch.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"

[[rule.test]]
name = "wrong kind field"

[rule.test.fixture]
"a.rs" = "fn f() {}"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1);
        assert!(rules[0].test.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("no-console"));
        assert!(
            diagnostics[0].reason.contains("fixture"),
            "{:?}",
            diagnostics[0].reason
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_drops_test_entry_with_pattern_only_field_on_sql_rule() {
        let dir = tempdir("test-entries-kind-mismatch-sql");
        let pack = dir.join("kind-mismatch-sql.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "too-many-loc"
kind = "sql"
severity = "warning"
message = "function too long"
query = "SELECT 1"

[[rule.test]]
name = "wrong kind field"
valid = ["console.log(1)"]
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1);
        assert!(rules[0].test.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("too-many-loc"));
        assert!(
            diagnostics[0].reason.contains("valid"),
            "{:?}",
            diagnostics[0].reason
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_drops_test_entry_with_wrong_toml_type() {
        let dir = tempdir("test-entries-wrong-type");
        let pack = dir.join("wrong-type.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"

[[rule.test]]
name = "wrong type"
invalid = "console.log(1)"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1);
        assert!(rules[0].test.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("no-console"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_drops_expect_rewrite_test_entry_when_rule_has_no_rewrite() {
        let dir = tempdir("test-entries-expect-rewrite-no-rewrite");
        let pack = dir.join("expect-rewrite-no-rewrite.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"

[[rule.test]]
name = "expects a rewrite that doesn't exist"

[rule.test.expect_rewrite]
"console.log(1)" = "console.info(1)"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1, "malformed test entry doesn't drop the rule");
        assert!(rules[0].test.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("no-console"));
        assert!(
            diagnostics[0].reason.contains("expect_rewrite")
                && diagnostics[0].reason.contains("rewrite"),
            "{:?}",
            diagnostics[0].reason
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_keeps_other_valid_test_entries_when_one_is_malformed() {
        let dir = tempdir("test-entries-partial");
        let pack = dir.join("partial.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"

[[rule.test]]
name = "good entry"
invalid = ["console.log(1)"]

[[rule.test]]
fixture = "wrong-kind-and-missing-name"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1);
        let tests = rules[0].test.as_ref().expect("one valid test case kept");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "good entry");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("no-console"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewrite_field_deserializes_when_present() {
        let toml = r#"
[[rule]]
id = "no-console-rewrite"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.log($MSG)"
rewrite = "console.info($MSG)"
"#;
        let doc: toml::Value = toml::from_str(toml).expect("document parses");
        let rule: Rule = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .expect("one rule")
            .clone()
            .try_into()
            .expect("rule deserializes");
        assert_eq!(rule.rewrite.as_deref(), Some("console.info($MSG)"));
    }

    #[test]
    fn sql_rule_ignores_rewrite_field() {
        // A SQL rule carries no `rewrite` semantics: the field is simply not
        // set (no schema coupling — SQL rules keep `fix` as free-text).
        let toml = r#"
[[rule]]
id = "complexity-guard"
kind = "sql"
severity = "error"
message = "file exceeds complexity budget"
query = "SELECT file, line FROM entities WHERE complexity > :max_lines"
"#;
        let doc: toml::Value = toml::from_str(toml).expect("document parses");
        let rule: Rule = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .expect("one rule")
            .clone()
            .try_into()
            .expect("rule deserializes");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(rule.rewrite, None);
    }

    #[test]
    fn full_rule_round_trips_every_field() {
        let toml = r#"
[[rule]]
id = "complexity-guard"
kind = "sql"
severity = "error"
name = "Complexity guard"
description = "Flags over-complex files"
message = "file exceeds complexity budget"
remediation = "split the file"
query = "SELECT file, line FROM entities WHERE complexity > :max_lines"
thresholds = { max_lines = 50.0 }
strings = { needle = "TODO" }
constraints = { VAR = "^[A-Z]" }
fix = "refactor into smaller functions"
"#;
        let doc: toml::Value = toml::from_str(toml).expect("document parses");
        let rules = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .expect("rule array");
        let rule: Rule = rules[0].clone().try_into().expect("rule deserializes");

        assert_eq!(rule.id, "complexity-guard");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(rule.severity, Severity::Error);
        assert_eq!(rule.name.as_deref(), Some("Complexity guard"));
        assert_eq!(
            rule.description.as_deref(),
            Some("Flags over-complex files")
        );
        assert_eq!(rule.message, "file exceeds complexity budget");
        assert_eq!(rule.remediation.as_deref(), Some("split the file"));
        assert_eq!(rule.pattern, None);
        assert_eq!(
            rule.query.as_deref(),
            Some("SELECT file, line FROM entities WHERE complexity > :max_lines")
        );

        let mut expected_thresholds = HashMap::new();
        expected_thresholds.insert("max_lines".to_string(), 50.0);
        assert_eq!(rule.thresholds, Some(expected_thresholds));

        let mut expected_strings = HashMap::new();
        expected_strings.insert("needle".to_string(), "TODO".to_string());
        assert_eq!(rule.strings, Some(expected_strings));

        let mut expected_constraints = HashMap::new();
        expected_constraints.insert("VAR".to_string(), "^[A-Z]".to_string());
        assert_eq!(rule.constraints, Some(expected_constraints));

        assert_eq!(rule.fix.as_deref(), Some("refactor into smaller functions"));
    }

    #[test]
    fn sql_kind_deserializes_with_lowercase_discriminator() {
        let toml = r#"
[[rule]]
id = "dead-import"
kind = "sql"
severity = "info"
message = "import never used"
query = "SELECT file, line FROM entities WHERE kind = 8"
"#;
        let doc: toml::Value = toml::from_str(toml).expect("document parses");
        let rule: Rule = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .expect("one rule")
            .clone()
            .try_into()
            .expect("rule deserializes");
        assert_eq!(rule.kind, RuleKind::Sql);
    }

    #[test]
    fn diagnostic_round_trips_serialization() {
        let diag = Diagnostic {
            rule_id: Some("no-console".to_string()),
            file: "/tmp/rules/one.toml".to_string(),
            reason: "missing required field `message`".to_string(),
        };
        let json = serde_json::to_value(&diag).expect("diagnostic serializes");
        assert_eq!(json["rule_id"], "no-console");
        assert_eq!(json["file"], "/tmp/rules/one.toml");
        assert_eq!(json["reason"], "missing required field `message`");
    }

    #[test]
    fn both_scopes_discovered_with_scope_tags() {
        let repo = tempdir("discover-both");
        let user = tempdir("discover-user");
        write(&repo.join(".varde-code/rules/a.toml"), "[rules]");
        write(&repo.join(".varde-code/rules/z.toml"), "[rules]");
        write(&user.join("u.toml"), "[rules]");
        // Non-TOML files in the scope dirs are ignored.
        write(&user.join("readme.md"), "# not a pack");

        let files = discover(&repo, Some(&user));
        assert_eq!(files.len(), 3);
        // User scope first, each scope sorted by path.
        assert_eq!(files[0].path, user.join("u.toml"));
        assert_eq!(files[0].scope, RuleScope::User);
        assert_eq!(files[1].path, repo.join(".varde-code/rules/a.toml"));
        assert_eq!(files[1].scope, RuleScope::Repo);
        assert_eq!(files[2].path, repo.join(".varde-code/rules/z.toml"));
        assert_eq!(files[2].scope, RuleScope::Repo);

        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&user);
    }

    #[test]
    fn missing_dirs_yield_empty_list_without_error() {
        let repo = tempdir("discover-empty");
        let user = tempdir("discover-empty-user");
        // Neither scope dir exists under the temp roots.
        let files = discover(&repo.join("nonexistent"), Some(&user.join("nonexistent")));
        assert!(files.is_empty());

        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&user);
    }
    #[test]
    fn parse_skips_rule_missing_required_field_and_keeps_valid_rule() {
        let dir = tempdir("parse-missing");
        let pack = dir.join("one.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "valid-one"
kind = "pattern"
severity = "warning"
message = "valid rule"
pattern = "console.log($MSG)"

[[rule]]
id = "broken-two"
kind = "pattern"
severity = "error"
pattern = "debugger"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "valid-one");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("broken-two"));
        assert!(!diagnostics[0].reason.is_empty());
        assert_eq!(diagnostics[0].file, pack.display().to_string());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_skips_rule_missing_kind_specific_field() {
        let dir = tempdir("parse-kind");
        let pack = dir.join("kind.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "query-only"
kind = "pattern"
severity = "info"
message = "has a query but kind=pattern needs pattern"
query = "SELECT 1"
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert!(rules.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("query-only"));
        assert!(
            diagnostics[0].reason.contains("pattern"),
            "reason names the missing kind field: {}",
            diagnostics[0].reason
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_ignores_unknown_extra_fields() {
        let dir = tempdir("parse-unknown");
        let pack = dir.join("extra.toml");
        write(
            &pack,
            r#"
[[rule]]
id = "future-proof"
kind = "pattern"
severity = "warning"
message = "loads despite unknown fields"
pattern = "foo()"
some_future_field = 42
"#,
        );
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "future-proof");
        assert!(diagnostics.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_reports_whole_file_on_invalid_toml_without_panicking() {
        let dir = tempdir("parse-bad-toml");
        let pack = dir.join("broken.toml");
        write(&pack, "this is not toml [[[");
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert!(rules.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id, None);
        assert!(diagnostics[0].reason.contains("TOML"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_empty_pack_file_yields_no_rules_no_diagnostics() {
        let dir = tempdir("parse-empty-pack");
        let pack = dir.join("empty.toml");
        write(&pack, "# no rules here");
        let (rules, diagnostics) = parse_pack_file(&pack);
        assert!(rules.is_empty());
        assert!(diagnostics.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_missing_file_reports_file_level_diagnostic() {
        let dir = tempdir("parse-missing-file");
        let (rules, diagnostics) = parse_pack_file(&dir.join("absent.toml"));
        assert!(rules.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id, None);
        assert!(diagnostics[0].reason.contains("unreadable"));

        let _ = fs::remove_dir_all(&dir);
    }
    fn loaded(id: &str, kind: RuleKind, file: &str) -> LoadedRule {
        LoadedRule {
            rule: Rule {
                id: id.to_string(),
                kind,
                severity: Severity::Warning,
                message: "m".to_string(),
                name: None,
                description: None,
                remediation: None,
                pattern: None,
                query: None,
                thresholds: None,
                strings: None,
                constraints: None,
                fix: None,
                rewrite: None,
                languages: None,
                exclude_test_paths: None,
                exclude_tooling_paths: None,
                test: None,
            },
            file: file.to_string(),
        }
    }

    #[test]
    fn merge_repo_wins_cross_scope_conflict_silently() {
        let user = vec![loaded("dup-id", RuleKind::Pattern, "user/a.toml")];
        let repo = vec![loaded("dup-id", RuleKind::Sql, "repo/b.toml")];
        let (rules, diagnostics) = merge(user, repo);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind, RuleKind::Sql, "repo-scoped rule wins");
        assert!(
            diagnostics.is_empty(),
            "cross-scope conflict is not a diagnostic"
        );
    }

    #[test]
    fn merge_reports_same_scope_duplicate_and_keeps_first() {
        let repo = vec![
            loaded("dup-id", RuleKind::Pattern, "repo/a.toml"),
            loaded("dup-id", RuleKind::Sql, "repo/z.toml"),
        ];
        let (rules, diagnostics) = merge(Vec::new(), repo);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind, RuleKind::Pattern, "first-loaded kept");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("dup-id"));
        assert_eq!(diagnostics[0].file, "repo/z.toml");
        assert!(!diagnostics[0].reason.is_empty());
    }

    #[test]
    fn merge_keeps_user_order_with_repo_replace_in_place() {
        let user = vec![
            loaded("a", RuleKind::Pattern, "u/1.toml"),
            loaded("b", RuleKind::Pattern, "u/2.toml"),
        ];
        let repo = vec![
            loaded("b", RuleKind::Sql, "r/1.toml"),
            loaded("c", RuleKind::Sql, "r/2.toml"),
        ];
        let (rules, diagnostics) = merge(user, repo);
        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"], "user order, repo-only appended");
        assert_eq!(rules[1].kind, RuleKind::Sql, "b replaced in place by repo");
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn merge_repo_internal_duplicate_after_cross_scope_replace_is_diagnosed() {
        let user = vec![loaded("dup", RuleKind::Pattern, "u/1.toml")];
        let repo = vec![
            loaded("dup", RuleKind::Sql, "r/1.toml"),
            loaded("dup", RuleKind::Pattern, "r/2.toml"),
        ];
        let (rules, diagnostics) = merge(user, repo);
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].kind,
            RuleKind::Sql,
            "first repo rule replaced user's"
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule_id.as_deref(), Some("dup"));
        assert_eq!(diagnostics[0].file, "r/2.toml");
    }
    #[test]
    fn languages_field_deserializes_when_present() {
        let toml = r#"
[[rule]]
id = "lang-scoped"
kind = "pattern"
severity = "warning"
message = "scoped to ts/js"
pattern = "console.log($MSG)"
languages = ["typescript", "javascript"]
"#;
        let doc: toml::Value = toml::from_str(toml).expect("document parses");
        let rule: Rule = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .expect("one rule")
            .clone()
            .try_into()
            .expect("rule deserializes");
        assert_eq!(
            rule.languages,
            Some(vec!["typescript".to_string(), "javascript".to_string()])
        );
    }

    #[test]
    fn languages_field_is_none_when_absent() {
        let doc: toml::Value = toml::from_str(minimal_toml()).expect("document parses");
        let rule: Rule = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .expect("one rule")
            .clone()
            .try_into()
            .expect("rule deserializes");
        assert_eq!(rule.languages, None);
    }
    #[test]
    fn thresholds_and_strings_deserialize_as_typed_maps() {
        let toml = r#"
[[rule]]
id = "typed-params"
kind = "sql"
severity = "error"
message = "typed"
query = "SELECT file, line FROM entities WHERE complexity > :max_lines"
thresholds = { max_lines = 50.0 }
strings = { needle = "TODO" }
"#;
        let doc: toml::Value = toml::from_str(toml).expect("document parses");
        let rule: Rule = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .expect("one rule")
            .clone()
            .try_into()
            .expect("rule deserializes");
        let mut expected_thresholds = HashMap::new();
        expected_thresholds.insert("max_lines".to_string(), 50.0);
        assert_eq!(rule.thresholds, Some(expected_thresholds));
        let mut expected_strings = HashMap::new();
        expected_strings.insert("needle".to_string(), "TODO".to_string());
        assert_eq!(rule.strings, Some(expected_strings));
    }

    #[test]
    fn thresholds_and_strings_are_none_when_omitted() {
        let doc: toml::Value = toml::from_str(minimal_toml()).expect("document parses");
        let rule: Rule = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .expect("one rule")
            .clone()
            .try_into()
            .expect("rule deserializes");
        assert_eq!(rule.thresholds, None);
        assert_eq!(rule.strings, None);
    }
    #[test]
    fn severity_ordering_ranks_error_above_warning_above_info() {
        use Severity::*;
        for (higher, lower) in [(Error, Warning), (Error, Info), (Warning, Info)] {
            assert!(higher > lower, "{higher:?} should outrank {lower:?}");
            assert!(lower < higher);
            assert_ne!(higher, lower);
        }
        assert_eq!(Error, Error);
        assert!(Error >= Warning && Warning >= Info);
    }

    #[test]
    fn severity_deserializes_case_insensitively() {
        for (raw, expected) in [
            ("error", Severity::Error),
            ("Error", Severity::Error),
            ("WARNING", Severity::Warning),
            ("warning", Severity::Warning),
            ("Info", Severity::Info),
        ] {
            let toml = format!(
                "[[rule]]\nid = \"r\"\nkind = \"pattern\"\nseverity = \"{raw}\"\nmessage = \"m\"\npattern = \"x\"\n"
            );
            let doc: toml::Value = toml::from_str(&toml).expect("document parses");
            let rule: Rule = doc
                .get("rule")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .expect("one rule")
                .clone()
                .try_into()
                .expect("rule deserializes");
            assert_eq!(rule.severity, expected, "severity {raw:?}");
        }
    }

    #[test]
    fn unknown_severity_is_a_deserialization_error() {
        let toml = r#"
[[rule]]
id = "r"
kind = "pattern"
severity = "critical"
message = "m"
pattern = "x"
"#;
        let doc: toml::Value = toml::from_str(toml).expect("document parses");
        let err = doc
            .get("rule")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .expect("one rule")
            .clone()
            .try_into::<Rule>();
        assert!(err.is_err(), "unknown severity level is rejected");
    }

    #[test]
    fn builtin_rules_parse_with_no_diagnostics() {
        let (loaded, diagnostics) = builtin_loaded();
        assert!(
            diagnostics.is_empty(),
            "shipped built-in packs must always parse cleanly: {diagnostics:?}"
        );
        assert!(
            loaded
                .iter()
                .any(|lr| lr.rule.id == "churn-complexity-hotspot"),
            "churn-complexity-hotspot must be among the built-in rules"
        );
        assert!(
            loaded
                .iter()
                .any(|lr| lr.rule.id == "file-complexity-hotspot"),
            "file-complexity-hotspot must be among the built-in rules"
        );
        for id in [
            "empty-catch-block",
            "empty-except-block",
            "empty-catch-block-swift",
            "eval-usage",
            "duplicate-code-clone",
            "debug-macro-strict",
            "debug-statement-strict",
            "console-log-strict",
            "hardcoded-credential-literal",
            "hardcoded-credential-declaration",
            "low-fan-in-high-fan-out-file",
        ] {
            assert!(
                loaded.iter().any(|lr| lr.rule.id == id),
                "{id} must be among the built-in rules"
            );
        }
    }

    #[test]
    fn churn_complexity_hotspot_shape_is_correct() {
        let rules = builtin_rules();
        let rule = rules
            .iter()
            .find(|r| r.id == "churn-complexity-hotspot")
            .expect("churn-complexity-hotspot shipped");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(rule.severity, Severity::Warning);
        assert!(rule.query.as_deref().unwrap_or("").contains("complexity"));
        assert!(rule.query.as_deref().unwrap_or("").contains("churn"));
        let thresholds = rule.thresholds.as_ref().expect("thresholds present");
        assert_eq!(thresholds.get("max_complexity"), Some(&50.0));
        assert_eq!(thresholds.get("min_churn"), Some(&20.0));
    }

    #[test]
    fn file_complexity_hotspot_shape_is_correct() {
        let rules = builtin_rules();
        let rule = rules
            .iter()
            .find(|r| r.id == "file-complexity-hotspot")
            .expect("file-complexity-hotspot shipped");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(rule.severity, Severity::Warning);
        assert!(rule.query.as_deref().unwrap_or("").contains("complexity"));
        assert!(
            !rule.query.as_deref().unwrap_or("").contains("churn"),
            "the complexity-only rule must not carry the churn clause"
        );
        let thresholds = rule.thresholds.as_ref().expect("thresholds present");
        assert_eq!(thresholds.get("max_complexity"), Some(&50.0));
    }

    #[test]
    fn function_complexity_hotspot_shape_is_correct() {
        let rules = builtin_rules();
        let rule = rules
            .iter()
            .find(|r| r.id == "function-complexity-hotspot")
            .expect("function-complexity-hotspot shipped");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(rule.severity, Severity::Warning);
        let query = rule.query.as_deref().unwrap_or("");
        assert!(query.contains("enclosing_function"));
        assert!(
            query.contains("GROUP BY"),
            "aggregates per function: {query}"
        );
        let thresholds = rule.thresholds.as_ref().expect("thresholds present");
        assert_eq!(thresholds.get("max_function_complexity"), Some(&15.0));
    }

    #[test]
    fn duplicate_code_clone_shape_is_correct() {
        let rules = builtin_rules();
        let rule = rules
            .iter()
            .find(|r| r.id == "duplicate-code-clone")
            .expect("duplicate-code-clone shipped");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(rule.severity, Severity::Warning);
        let query = rule.query.as_deref().unwrap_or("");
        assert!(
            query.contains("clone_band_members"),
            "query joins the member table"
        );
        assert!(
            query.contains("HAVING COUNT(*) >= :min_band_size"),
            "size filter"
        );
        assert!(
            !query.contains("cbm2"),
            "no N^2 self-join: the sketch's cbm2 join counted rows per pair and would flag pairs"
        );
        assert!(
            query.contains("is_test = 0"),
            "excludes test functions from both the member count and the emitted rows"
        );
        let thresholds = rule.thresholds.as_ref().expect("thresholds present");
        assert_eq!(thresholds.len(), 2, "min_band_size + min_span_lines");
        assert_eq!(thresholds.get("min_band_size"), Some(&3.0));
        assert_eq!(thresholds.get("min_span_lines"), Some(&2.0));
    }

    #[test]
    fn debug_statement_strict_shapes_are_correct() {
        let rules = builtin_rules();
        let by_id = |id: &str| {
            rules
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id} shipped"))
        };

        let dbg = by_id("debug-macro-strict");
        assert_eq!(dbg.kind, RuleKind::Pattern);
        assert_eq!(dbg.severity, Severity::Warning);
        assert_eq!(dbg.pattern.as_deref(), Some("dbg!($$$ARGS)"));
        assert_eq!(dbg.languages.as_deref(), Some(&["rust".to_string()][..]));

        let dstmt = by_id("debug-statement-strict");
        assert_eq!(dstmt.pattern.as_deref(), Some("debugger;"));
        assert_eq!(
            dstmt.languages.as_deref(),
            Some(
                &[
                    "javascript".to_string(),
                    "typescript".to_string(),
                    "tsx".to_string()
                ][..]
            )
        );

        let clog = by_id("console-log-strict");
        assert_eq!(clog.pattern.as_deref(), Some("console.log($$$ARGS)"));
        assert_eq!(
            clog.languages.as_deref(),
            Some(
                &[
                    "javascript".to_string(),
                    "typescript".to_string(),
                    "tsx".to_string()
                ][..]
            )
        );

        // The narrowed-set decision: no print-style rule ships at all.
        assert!(
            rules
                .iter()
                .all(|r| !r.id.contains("println") && !r.id.contains("print-strict")),
            "print/fmt.Println variants are deliberately out of the pack"
        );
    }

    #[test]
    fn low_fan_in_high_fan_out_shape_is_correct() {
        let rules = builtin_rules();
        let rule = rules
            .iter()
            .find(|r| r.id == "low-fan-in-high-fan-out-file")
            .expect("low-fan-in-high-fan-out-file shipped");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(
            rule.severity,
            Severity::Info,
            "info, not warning — entry points structurally match this shape"
        );
        let query = rule.query.as_deref().expect("sql rule has query");
        assert!(query.contains("fan_in <= :max_fan_in"), "{query}");
        assert!(query.contains("fan_out >= :min_fan_out"), "{query}");
        let thresholds = rule.thresholds.as_ref().expect("thresholds present");
        assert_eq!(thresholds.get("max_fan_in"), Some(&2.0));
        assert_eq!(thresholds.get("min_fan_out"), Some(&15.0));
        assert_eq!(thresholds.len(), 2, "only the two fan thresholds");
    }

    #[test]
    fn circular_import_shape_is_correct() {
        let rules = builtin_rules();
        let rule = rules
            .iter()
            .find(|r| r.id == "circular-import")
            .expect("circular-import shipped");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(rule.severity, Severity::Warning);
        let query = rule.query.as_deref().expect("sql rule has query");
        assert!(query.contains("resolved_edges"), "{query}");
        assert!(
            query.contains("e1.from_file_id < e1.to_file_id"),
            "dedupes the symmetric pair to one finding per cycle: {query}"
        );
        assert!(
            rule.thresholds.is_none(),
            "no tunable threshold — a direct cycle exists or it doesn't"
        );
    }

    #[test]
    fn vertical_slice_sprawl_shape_is_correct() {
        let rules = builtin_rules();
        let rule = rules
            .iter()
            .find(|r| r.id == "vertical-slice-sprawl")
            .expect("vertical-slice-sprawl shipped");
        assert_eq!(rule.kind, RuleKind::Sql);
        assert_eq!(rule.severity, Severity::Warning);
        let query = rule.query.as_deref().expect("sql rule has query");
        assert!(query.contains("from_entity_id"), "{query}");
        assert!(
            query.contains("call_ent.kind = 6"),
            "call-site entities only: {query}"
        );
        assert!(
            query.contains("COUNT(DISTINCT tf.community_id)"),
            "counts distinct foreign communities, not raw cross-slice call volume: {query}"
        );
        assert!(
            query.contains("tf.community_id != f.community_id"),
            "same-slice calls don't count as sprawl: {query}"
        );
        assert_eq!(
            rule.thresholds,
            Some(
                [("min_foreign_slices".to_string(), 3.0)]
                    .into_iter()
                    .collect()
            )
        );
    }

    #[test]
    fn hardcoded_credential_shapes_are_correct() {
        let rules = builtin_rules();
        let by_id = |id: &str| {
            rules
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id} shipped"))
        };

        for id in [
            "hardcoded-credential-literal",
            "hardcoded-credential-declaration",
        ] {
            let rule = by_id(id);
            assert_eq!(rule.kind, RuleKind::Pattern);
            assert_eq!(rule.severity, Severity::Warning);
            let constraints = rule.constraints.as_ref().expect("constraints present");
            let key_re = &constraints["KEY"];
            let val_re = &constraints["VAL"];
            assert!(key_re.starts_with('^'), "KEY regex anchored: {key_re}");
            assert!(key_re.ends_with('$'), "KEY regex anchored: {key_re}");
            assert!(
                key_re.contains("(?i)"),
                "KEY regex case-insensitive: {key_re}"
            );
            assert!(
                val_re.contains("15") && val_re.contains("[0-9]"),
                "VAL requires >=16 chars with a digit: {val_re}"
            );
            assert!(
                val_re.contains("[A-Za-z0-9_\\-+/=]"),
                "base64-ish alphabet: {val_re}"
            );
        }

        assert_eq!(
            by_id("hardcoded-credential-literal").pattern.as_deref(),
            Some("$KEY = $VAL")
        );
        assert_eq!(
            by_id("hardcoded-credential-declaration").pattern.as_deref(),
            Some("const $KEY = $VAL")
        );
        assert_eq!(
            by_id("hardcoded-credential-literal").languages.as_deref(),
            Some(
                &[
                    "python".to_string(),
                    "javascript".to_string(),
                    "typescript".to_string(),
                    "tsx".to_string()
                ][..]
            )
        );
        assert_eq!(
            by_id("hardcoded-credential-declaration")
                .languages
                .as_deref(),
            Some(
                &[
                    "javascript".to_string(),
                    "typescript".to_string(),
                    "tsx".to_string()
                ][..]
            )
        );
    }

    #[test]
    fn empty_catch_block_and_eval_usage_shapes_are_correct() {
        let rules = builtin_rules();
        let by_id = |id: &str| {
            rules
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id} shipped"))
        };

        let catch = by_id("empty-catch-block");
        assert_eq!(catch.kind, RuleKind::Pattern);
        assert_eq!(catch.severity, Severity::Warning);
        assert_eq!(
            catch.pattern.as_deref(),
            Some("try { $$$A } catch ($ERR) { }")
        );
        assert_eq!(
            catch.languages.as_deref(),
            Some(
                &[
                    "typescript".to_string(),
                    "tsx".to_string(),
                    "javascript".to_string()
                ][..]
            )
        );

        let except_ = by_id("empty-except-block");
        assert_eq!(except_.kind, RuleKind::Pattern);
        assert_eq!(except_.severity, Severity::Warning);
        assert_eq!(
            except_.pattern.as_deref(),
            Some("try:\n    $$$A\nexcept $ERR: pass")
        );
        assert_eq!(
            except_.languages.as_deref(),
            Some(&["python".to_string()][..])
        );

        let swift = by_id("empty-catch-block-swift");
        assert_eq!(swift.kind, RuleKind::Pattern);
        assert_eq!(swift.severity, Severity::Warning);
        assert_eq!(swift.pattern.as_deref(), Some("do { $$$A } catch { }"));
        assert_eq!(swift.languages.as_deref(), Some(&["swift".to_string()][..]));

        let eval = by_id("eval-usage");
        assert_eq!(eval.kind, RuleKind::Pattern);
        assert_eq!(eval.severity, Severity::Warning);
        assert_eq!(eval.pattern.as_deref(), Some("eval($$$ARGS)"));
        assert_eq!(
            eval.languages.as_deref(),
            Some(
                &[
                    "javascript".to_string(),
                    "typescript".to_string(),
                    "tsx".to_string(),
                    "python".to_string(),
                ][..]
            )
        );
    }

    /// `VARDE_USER_RULES_DIR` is process-global; serializing the tests that
    /// touch it prevents env races between parallel test threads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn load_rules_includes_builtin_when_no_user_or_repo_rules_exist() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let repo = tempdir("load-builtin-only");
        let user = tempdir("load-builtin-only-user");
        unsafe { std::env::set_var("VARDE_USER_RULES_DIR", &user) };

        let (rules, _diagnostics) = load_rules(&repo);
        assert!(
            rules.iter().any(|r| r.id == "churn-complexity-hotspot"),
            "built-in rule loads even with empty user/repo scopes"
        );
        assert!(
            rules.iter().any(|r| r.id == "file-complexity-hotspot"),
            "file-complexity-hotspot loads even with empty user/repo scopes"
        );
        assert!(
            rules.iter().any(|r| r.id == "empty-catch-block"),
            "empty-catch-block loads even with empty user/repo scopes"
        );
        assert!(
            rules.iter().any(|r| r.id == "eval-usage"),
            "eval-usage loads even with empty user/repo scopes"
        );
        assert!(
            rules.iter().any(|r| r.id == "duplicate-code-clone"),
            "duplicate-code-clone loads even with empty user/repo scopes"
        );
        assert!(
            rules.iter().any(|r| r.id == "console-log-strict"),
            "console-log-strict loads even with empty user/repo scopes"
        );
        assert!(
            rules.iter().any(|r| r.id == "hardcoded-credential-literal"),
            "hardcoded-credential-literal loads even with empty user/repo scopes"
        );
        assert!(
            rules.iter().any(|r| r.id == "low-fan-in-high-fan-out-file"),
            "low-fan-in-high-fan-out-file loads even with empty user/repo scopes"
        );

        unsafe { std::env::remove_var("VARDE_USER_RULES_DIR") };
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&user);
    }

    #[test]
    fn repo_rule_silently_overrides_builtin_by_id() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let repo = tempdir("load-builtin-override");
        let user = tempdir("load-builtin-override-user");
        unsafe { std::env::set_var("VARDE_USER_RULES_DIR", &user) };
        write(
            &repo.join(".varde-code/rules/override.toml"),
            r#"
[[rule]]
id = "churn-complexity-hotspot"
kind = "pattern"
severity = "info"
message = "repo override"
pattern = "foo()"
"#,
        );

        let (rules, diagnostics) = load_rules(&repo);
        let matches: Vec<&Rule> = rules
            .iter()
            .filter(|r| r.id == "churn-complexity-hotspot")
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "no duplicate — repo rule replaces the built-in"
        );
        assert_eq!(
            matches[0].kind,
            RuleKind::Pattern,
            "repo-scoped rule wins over built-in"
        );
        assert!(
            diagnostics
                .iter()
                .all(|d| d.rule_id.as_deref() != Some("churn-complexity-hotspot")),
            "overriding a built-in is expected, not diagnosed: {diagnostics:?}"
        );

        unsafe { std::env::remove_var("VARDE_USER_RULES_DIR") };
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&user);
    }
}
