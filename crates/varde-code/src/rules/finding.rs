//! `Finding`: the scan output unit shared by both rule engines.
//!
//! Adopts varde's `ScanFinding` shape: id, rule_id, severity, message,
//! location, evidence, remediation, certainty, agent_instructions. Findings
//! are transient — emitted to stdout or a caller-specified file by `scan`,
//! never persisted.

use crate::model::Span;
use crate::rules::Severity;
use crate::rules::rewrite::RewriteStatus;
use serde::{Deserialize, Serialize};

/// Match confidence, mirroring varde's scan findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Certainty {
    High,
    Medium,
    Low,
}

/// A match's file + byte/line span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub file: String,
    pub span: Span,
}

/// A file's on-disk state (mtime + length) as observed when a finding's span
/// was computed.
///
/// `--apply` (`scan_cli::apply_rewrites`) re-reads each file immediately
/// before splicing, but the byte offsets themselves come from a parse that
/// happened earlier in the scan (pattern matching, possibly followed by SQL
/// rules and suppression filtering). If the file changed in between and the
/// stale offsets still happen to land in-range on a char boundary, splicing
/// them into the new content silently corrupts the file rather than being
/// caught as a conflict. Comparing the state recorded here against a fresh
/// stat right before writing closes that window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileState {
    pub mtime: std::time::SystemTime,
    pub len: u64,
}

impl FileState {
    /// Best-effort: `None` if the file can't be stat'd (deleted, permission
    /// error, ...) — callers treat that as "staleness unverifiable", not a
    /// hard failure, since the same read is retried and separately guarded
    /// wherever this matters.
    pub fn of(path: &std::path::Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(FileState {
            mtime: meta.modified().ok()?,
            len: meta.len(),
        })
    }
}

/// Resolve a walked path (possibly relative — the scan may have walked a
/// relative `repoRoot`) to the absolute physical file, following symlinks.
/// Shared by match-time stat capture (`pattern::run_pattern_rule`) and
/// apply-time write targeting (`scan_cli::apply_rewrites`) so both name the
/// same file the same way (CORRECTNESS-002/004).
pub fn resolve_real_path(walk_path: &str) -> std::path::PathBuf {
    let walk_path = std::path::Path::new(walk_path);
    let abs = std::path::absolute(walk_path).unwrap_or_else(|_| walk_path.to_path_buf());
    std::fs::canonicalize(&abs).unwrap_or(abs)
}

/// One rule match, ready for JSON emission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Deterministic id derived from `rule_id + file + span` — repeated
    /// scans of an unchanged repo produce identical ids.
    pub id: String,
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    pub location: Location,
    /// Populated from the match's `captures` map without lossy conversion.
    pub evidence: serde_json::Value,
    /// Hoisted to the scan payload's per-rule `rules` legend at emission
    /// (static per rule, so re-inlining it on every finding was pure
    /// repetition — audit F2); omitted from the per-finding JSON there.
    pub remediation: Option<String>,
    /// Pattern matches carry ast-grep's confidence; SQL findings always
    /// `None` (no match-confidence signal to derive it from). Skipped when
    /// `None` so SQL findings don't ship a `certainty: null` (audit F2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certainty: Option<Certainty>,
    /// Sourced from the rule's `fix` field when present (informational only).
    /// Skipped when `None` — null on ~100% of findings otherwise (audit F2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_instructions: Option<String>,
    /// Outcome of an `--apply` run, set only for pattern-rule findings whose
    /// rule carries a `rewrite` template — omitted from JSON otherwise
    /// (never `null`), so SQL-rule findings and rewrite-less pattern
    /// findings keep their exact old shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_status: Option<RewriteStatus>,
    /// The file's on-disk state observed when this finding's span was
    /// computed, for `--apply`'s staleness guard. Internal to a single
    /// scan-then-apply process run — never serialized. `None` for SQL-rule
    /// findings (never rewrite-bearing) and whenever the stat failed.
    #[serde(skip)]
    pub matched_file_state: Option<FileState>,
}

/// Deterministic finding id: FNV-1a 64 over
/// `rule_id \0 file \0 start_byte \0 end_byte`, hex-encoded.
///
/// The same rule + file + span always yields the same id across runs and
/// platforms — ids are stable under repeated scans of an unchanged repo.
///
/// The `--apply` rewrite-status wiring (`scan_cli::scan_repo`) depends on
/// per-run id uniqueness: statuses are keyed by finding id and attached by
/// lookup, so any future change to the hash inputs (or to rule-id dedup at
/// load) must re-check that a status can never land on the wrong finding.
pub(crate) fn finding_id(rule_id: &str, file: &str, span: &Span) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for part in [
        rule_id,
        file,
        &span.start_byte.to_string(),
        &span.end_byte.to_string(),
    ] {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        // Extra round per field mixes in the implicit `\0` separator (see
        // doc comment) so e.g. "ab"+"c" and "a"+"bc" hash differently.
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Convert a constraint-passed, correlation-enriched pattern match into a
/// `Finding`.
///
/// `location` carries the match's file + span (from find_pattern's match
/// object); `evidence` is the match's `captures` map verbatim; certainty is
/// `High` for a pattern match.
pub fn match_to_finding(
    rule: &crate::rules::Rule,
    m: &serde_json::Value,
    location: Location,
) -> Finding {
    let span = location.span;
    Finding {
        id: finding_id(&rule.id, &location.file, &span),
        rule_id: rule.id.clone(),
        severity: rule.severity,
        message: rule.message.clone(),
        location: Location {
            file: location.file,
            span,
        },
        evidence: m
            .get("captures")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        remediation: rule.remediation.clone(),
        certainty: Some(Certainty::High),
        agent_instructions: rule.fix.clone(),
        rewrite_status: None,
        // Set by the caller (`pattern::run_pattern_rule`), which has the
        // file path and can stat it — kept out of this constructor's
        // signature since only the pattern-rule path populates it.
        matched_file_state: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Rule;
    use crate::rules::RuleKind;

    fn rule_with_fix(fix: Option<&str>) -> Rule {
        Rule {
            id: "no-console".to_string(),
            kind: RuleKind::Pattern,
            severity: Severity::Warning,
            message: "console call detected".to_string(),
            name: None,
            description: None,
            remediation: Some("remove it".to_string()),
            pattern: Some("console.log($MSG)".to_string()),
            query: None,
            thresholds: None,
            strings: None,
            constraints: None,
            fix: fix.map(|f| f.to_string()),
            rewrite: None,
            languages: None,
            exclude_test_paths: None,
            exclude_tooling_paths: None,
            test: None,
        }
    }

    fn match_value() -> serde_json::Value {
        serde_json::json!({
            "kind": "call_expression",
            "text": "console.log('hi')",
            "span": { "start_byte": 10, "end_byte": 26, "start_line": 2, "start_col": 4, "end_line": 2, "end_col": 20 },
            "captures": { "MSG": { "kind": "string", "text": "'hi'" } },
        })
    }

    fn location() -> Location {
        Location {
            file: "/tmp/repo/src/main.ts".to_string(),
            span: Span {
                start_byte: 10,
                end_byte: 26,
                start_line: 2,
                start_col: 4,
                end_line: 2,
                end_col: 20,
            },
        }
    }

    #[test]
    fn fix_field_becomes_agent_instructions() {
        let finding = match_to_finding(
            &rule_with_fix(Some("delete the line")),
            &match_value(),
            location(),
        );
        assert_eq!(
            finding.agent_instructions.as_deref(),
            Some("delete the line")
        );
    }

    #[test]
    fn no_fix_yields_none_agent_instructions() {
        let finding = match_to_finding(&rule_with_fix(None), &match_value(), location());
        assert_eq!(finding.agent_instructions, None);
    }

    #[test]
    fn finding_id_is_deterministic() {
        let rule = rule_with_fix(None);
        let first = match_to_finding(&rule, &match_value(), location());
        let second = match_to_finding(&rule, &match_value(), location());
        assert_eq!(first.id, second.id, "same rule+match → identical id");
        assert_eq!(first.id.len(), 16, "hex-encoded 64-bit hash");
    }

    #[test]
    fn rewrite_status_defaults_to_none() {
        let finding = match_to_finding(&rule_with_fix(None), &match_value(), location());
        assert_eq!(finding.rewrite_status, None);
    }

    #[test]
    fn rewrite_status_serializes_only_when_present() {
        let mut finding = match_to_finding(&rule_with_fix(None), &match_value(), location());
        // Absent → omitted entirely, not `null`.
        let json = serde_json::to_value(&finding).expect("serializes");
        assert!(json.get("rewrite_status").is_none());

        finding.rewrite_status = Some(RewriteStatus::SkippedDirty);
        let json = serde_json::to_value(&finding).expect("serializes");
        assert_eq!(json["rewrite_status"], "skipped-dirty");
    }

    #[test]
    fn evidence_round_trips_captures_verbatim() {
        let finding = match_to_finding(&rule_with_fix(None), &match_value(), location());
        assert_eq!(
            finding.evidence,
            serde_json::json!({ "MSG": { "kind": "string", "text": "'hi'" } })
        );
    }
}
