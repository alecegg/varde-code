//! Inline suppression comments: `varde-ignore-file` / `varde-ignore-next-line`.
//!
//! Matched only within comments identified by the configured tree-sitter
//! grammar. This keeps marker text inside string literals from suppressing
//! unrelated findings.
//!
//! Syntax: `<marker> [rule-id ...] [-- reason]`. No rule ids = suppress every
//! rule on that line/file. `varde-ignore-file` suppresses file-wide (line 0);
//! `varde-ignore-next-line` suppresses the line immediately following the
//! comment.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::parse::{language_for_path, parse_source};
use crate::rules::finding::Finding;

const FILE_MARKER: &str = "varde-ignore-file";
const NEXT_LINE_MARKER: &str = "varde-ignore-next-line";

/// One suppression directive parsed from a source comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    /// 1-based line this suppression applies to; 0 = whole file.
    pub line: u32,
    /// 1-based line the suppression comment itself appears on.
    pub comment_line: u32,
    /// Empty = suppress every rule on this line/file.
    pub rule_ids: Vec<String>,
    /// Human-authored reason after `--`, when present.
    pub reason: Option<String>,
}

impl Suppression {
    fn matches(&self, line: u32, rule_id: &str) -> bool {
        (self.line == 0 || self.line == line)
            && (self.rule_ids.is_empty() || self.rule_ids.iter().any(|r| r == rule_id))
    }
}

/// A suppression comment that did not match any finding — likely stale
/// (the rule was removed/renamed, or the flagged code was fixed) and worth
/// surfacing so it can be deleted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StaleSuppression {
    pub file: String,
    pub comment_line: u32,
    pub rule_ids: Vec<String>,
    pub reason: Option<String>,
}

fn split_reason(rest: &str) -> (&str, Option<String>) {
    match rest.find("--") {
        Some(idx) => {
            let targets = rest[..idx].trim();
            let reason = rest[idx + 2..].trim();
            (
                targets,
                if reason.is_empty() {
                    None
                } else {
                    Some(reason.to_string())
                },
            )
        }
        None => (rest.trim(), None),
    }
}

fn parse_marker_tail(rest: &str) -> (Vec<String>, Option<String>) {
    let (targets, reason) = split_reason(rest);
    let rule_ids = targets
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (rule_ids, reason)
}

/// Parse every `varde-ignore-file` / `varde-ignore-next-line` marker from
/// comments in a supported source file.
#[must_use]
pub fn parse_suppressions(source: &str, path: &Path) -> Vec<Suppression> {
    let Some(lang) = language_for_path(path) else {
        return Vec::new();
    };
    let parsed = parse_source(&lang, source);
    let comment_ranges: Vec<_> = parsed
        .root
        .root()
        .dfs()
        .filter(|node| node.kind().contains("comment"))
        .map(|node| node.range())
        .collect();

    let mut out = Vec::new();
    let mut line_start = 0;
    for (idx, raw_line) in source.split_inclusive('\n').enumerate() {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let comment_line = (idx + 1) as u32;
        if let Some(pos) = line.find(FILE_MARKER) {
            let marker_start = line_start + pos;
            if comment_ranges
                .iter()
                .any(|range| range.start <= marker_start && marker_start < range.end)
            {
                let (rule_ids, reason) = parse_marker_tail(&line[pos + FILE_MARKER.len()..]);
                out.push(Suppression {
                    line: 0,
                    comment_line,
                    rule_ids,
                    reason,
                });
            }
        } else if let Some(pos) = line.find(NEXT_LINE_MARKER) {
            let marker_start = line_start + pos;
            if comment_ranges
                .iter()
                .any(|range| range.start <= marker_start && marker_start < range.end)
            {
                let (rule_ids, reason) = parse_marker_tail(&line[pos + NEXT_LINE_MARKER.len()..]);
                out.push(Suppression {
                    line: comment_line + 1,
                    comment_line,
                    rule_ids,
                    reason,
                });
            }
        }
        line_start += raw_line.len();
    }
    out
}

/// Drop findings covered by an inline suppression comment in their own
/// file, and report the suppressions that matched nothing (stale).
///
/// Reads each finding's file once regardless of how many findings it
/// contains. A file with no suppression markers is skipped without
/// per-finding work.
///
/// Stale detection is scoped to files that produced at least one finding
/// this run (from any rule) — it does not walk the whole repo, so a
/// suppression comment in a file with zero findings (e.g. the flagged code
/// was deleted entirely) is not caught. Catching that case would need a
/// full source-tree walk independent of the rule engines.
#[must_use]
pub fn filter_findings(
    findings: Vec<Finding>,
    repo_root: &Path,
) -> (Vec<Finding>, Vec<StaleSuppression>) {
    let mut by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        by_file.entry(f.location.file.as_str()).or_default().push(i);
    }

    let mut keep = vec![true; findings.len()];
    let mut stale = Vec::new();

    for (file, idxs) in by_file {
        let path = Path::new(file);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            repo_root.join(path)
        };
        let Ok(source) = std::fs::read_to_string(&abs) else {
            continue;
        };
        let suppressions = parse_suppressions(&source, &abs);
        if suppressions.is_empty() {
            continue;
        }
        let used: Vec<AtomicBool> = suppressions
            .iter()
            .map(|_| AtomicBool::new(false))
            .collect();

        for &i in &idxs {
            let finding = &findings[i];
            let line = finding.location.span.start_line;
            if let Some((si, _)) = suppressions
                .iter()
                .enumerate()
                .find(|(_, s)| s.matches(line, &finding.rule_id))
            {
                used[si].store(true, Ordering::Relaxed);
                keep[i] = false;
            }
        }

        for (si, s) in suppressions.iter().enumerate() {
            if !used[si].load(Ordering::Relaxed) {
                stale.push(StaleSuppression {
                    file: file.to_string(),
                    comment_line: s.comment_line,
                    rule_ids: s.rule_ids.clone(),
                    reason: s.reason.clone(),
                });
            }
        }
    }

    let out = findings
        .into_iter()
        .enumerate()
        .filter_map(|(i, f)| keep[i].then_some(f))
        .collect();
    (out, stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Span;
    use crate::rules::Severity;
    use crate::rules::finding::Location;

    fn finding(rule_id: &str, file: &str, line: u32) -> Finding {
        Finding {
            id: format!("{rule_id}-{file}-{line}"),
            rule_id: rule_id.to_string(),
            severity: Severity::Warning,
            message: "msg".to_string(),
            location: Location {
                file: file.to_string(),
                span: Span {
                    start_byte: 0,
                    end_byte: 0,
                    start_line: line,
                    start_col: 0,
                    end_line: line,
                    end_col: 0,
                },
            },
            evidence: serde_json::Value::Null,
            remediation: None,
            certainty: None,
            agent_instructions: None,
            rewrite_status: None,
            matched_file_state: None,
        }
    }

    #[test]
    fn parses_bare_file_marker() {
        let supps = parse_suppressions("// varde-ignore-file\nconst x = 1;\n", Path::new("a.ts"));
        assert_eq!(supps.len(), 1);
        assert_eq!(supps[0].line, 0);
        assert!(supps[0].rule_ids.is_empty());
        assert_eq!(supps[0].reason, None);
    }

    #[test]
    fn parses_scoped_next_line_marker_with_reason() {
        let supps = parse_suppressions(
            "// varde-ignore-next-line no-console -- flagged, safe here\nconsole.log(1);\n",
            Path::new("a.ts"),
        );
        assert_eq!(supps.len(), 1);
        assert_eq!(supps[0].line, 2);
        assert_eq!(supps[0].comment_line, 1);
        assert_eq!(supps[0].rule_ids, vec!["no-console".to_string()]);
        assert_eq!(supps[0].reason.as_deref(), Some("flagged, safe here"));
    }

    #[test]
    fn parses_multiple_scoped_rule_ids() {
        let supps = parse_suppressions(
            "// varde-ignore-next-line no-console, no-debugger\nx();\n",
            Path::new("a.ts"),
        );
        assert_eq!(
            supps[0].rule_ids,
            vec!["no-console".to_string(), "no-debugger".to_string()]
        );
    }

    #[test]
    fn filter_drops_matching_finding_and_keeps_others() {
        let dir = std::env::temp_dir().join(format!("varde-suppress-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.ts"),
            "// varde-ignore-next-line no-console\nconsole.log(1);\nconsole.log(2);\n",
        )
        .unwrap();

        let findings = vec![
            finding("no-console", "a.ts", 2),
            finding("no-console", "a.ts", 3),
        ];
        let (kept, stale) = filter_findings(findings, &dir);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].location.span.start_line, 3);
        assert!(stale.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_wide_marker_suppresses_every_line() {
        let dir = std::env::temp_dir().join(format!("varde-suppress-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.ts"), "// varde-ignore-file\nconsole.log(1);\n").unwrap();

        let findings = vec![finding("no-console", "b.ts", 2)];
        let (kept, stale) = filter_findings(findings, &dir);
        assert!(kept.is_empty());
        assert!(stale.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn marker_inside_string_literal_does_not_suppress_findings() {
        let dir =
            std::env::temp_dir().join(format!("varde-suppress-string-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("string.ts"),
            "const directive = \"varde-ignore-file\";\nconsole.log(1);\n",
        )
        .unwrap();

        let findings = vec![finding("no-console", "string.ts", 2)];
        let (kept, stale) = filter_findings(findings, &dir);
        assert_eq!(kept.len(), 1);
        assert!(stale.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unused_suppression_is_reported_stale() {
        let dir = std::env::temp_dir().join(format!("varde-suppress-test3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("c.ts"),
            "// varde-ignore-next-line no-console\nconsole.log(1);\nconst x = 1;\n",
        )
        .unwrap();

        // A finding on an unrelated line keeps the file in scope for stale
        // detection (see filter_findings' doc comment on this limitation).
        let findings = vec![finding("no-debugger", "c.ts", 3)];
        let (kept, stale) = filter_findings(findings, &dir);
        assert_eq!(kept.len(), 1);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].rule_ids, vec!["no-console".to_string()]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scoped_marker_does_not_suppress_unmatched_rule() {
        let dir = std::env::temp_dir().join(format!("varde-suppress-test4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("d.ts"),
            "// varde-ignore-next-line no-console\nconsole.log(1);\n",
        )
        .unwrap();

        let findings = vec![finding("no-debugger", "d.ts", 2)];
        let (kept, stale) = filter_findings(findings, &dir);
        assert_eq!(
            kept.len(),
            1,
            "unrelated rule id must survive the scoped suppression"
        );
        assert_eq!(
            stale.len(),
            1,
            "the scoped suppression matched nothing, so it is stale"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
