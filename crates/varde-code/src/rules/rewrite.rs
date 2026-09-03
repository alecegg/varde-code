//! Rewrite-template substitution engine for pattern rules.
//!
//! A pattern rule may carry a `rewrite` field: a `$VAR` / `$$$VAR` template
//! (same token syntax as the rule's `pattern`) that replaces a matched
//! span when `scan --apply` runs. This module owns the pure substitution
//! (`substitute`) and the span-planning (`plan_file_rewrites` in
//! `rewrite-multi-match-ordering-and-overlap-detection`); the CLI applies
//! the result to disk in `scan_cli.rs`.
//!
//! Substitution source is the match's `captures` map as produced by
//! `find_pattern.rs` (`collect_captures`/`bind_sequence`) and carried on
//! `Finding.evidence`: `$VAR` binds a single node object `{kind, text,
//! span}`, `$$$VAR` binds an array of node objects. A `$$$VAR` capture's
//! bound nodes join into the replacement text with no separator — the same
//! binding semantics the constraint filter applies to a variadic capture's
//! joined text (`filter_matches`'s `.concat()`).

/// Substitute `$VAR` / `$$$VAR` tokens in a `rewrite` template with the
/// matched capture texts from `captures`.
///
/// - `$VAR` → the single capture node's `text` (byte-exact).
/// - `$$$VAR` → the bound nodes' texts concatenated in source order.
/// - Anything else — literal text, `$` with no capture name, a `$$$`/`$`
///   token whose name is absent from `captures` — is copied through
///   verbatim, byte-exact.
///
/// Tokenization is shared with the load-time validator
/// (`pattern::capture_names`) via [`template_tokens`] — see its doc for the
/// token syntax contract. Capture names are validated at rule-load time
/// against the pattern's captures (`pattern::validate_rewrite_template`), so
/// a name absent from the map is unreachable in practice; it is passed
/// through untouched defensively.
pub fn substitute(template: &str, captures: &serde_json::Value) -> String {
    let captures = captures.as_object();
    let mut out = String::with_capacity(template.len());
    let mut cursor = 0usize;
    for token in template_tokens(template) {
        // Literal run before this token. Token boundaries are ASCII (`$`
        // sigil + `[A-Za-z0-9_]` name run), so every boundary is a UTF-8
        // char boundary and slicing can never split a multi-byte sequence.
        out.push_str(&template[cursor..token.start]);
        match captures.and_then(|m| m.get(token.name)) {
            Some(value) => out.push_str(&capture_text(value)),
            None => out.push_str(&template[token.start..token.end]),
        }
        cursor = token.end;
    }
    out.push_str(&template[cursor..]);
    out
}

/// One `$VAR` / `$$$VAR` template token: its byte span in the template plus
/// the capture name.
///
/// Token syntax contract — shared by all three consumers (`capture_names`
/// in `pattern.rs`, `substitute` here, and the matcher's `replace_meta` in
/// `find_pattern.rs`, which rewrites tokens into marker identifiers and so
/// keeps its own byte loop):
/// - `$$$` is checked before `$` — a `$$$VAR` is one variadic token, never
///   `$` + `$$VAR`;
/// - a capture name is the run of `[A-Za-z0-9_]` immediately after the
///   sigil; a `$`/`$$$` with no name (bare `$`, `$;`) is not a capture and
///   passes through literally;
/// - `$AB` is one name `AB` (the name run swallows `B`), never `$A` + `B`;
/// - sigil and name characters are ASCII, so token boundaries can never
///   split a multi-byte UTF-8 sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TemplateToken<'a> {
    /// Byte offset of the token's first byte (the `$` sigil).
    pub(crate) start: usize,
    /// Byte offset one past the name run — the whole token's end.
    pub(crate) end: usize,
    /// Whether the sigil was `$$$` (variadic capture).
    pub(crate) variadic: bool,
    /// The capture name (non-empty by construction).
    pub(crate) name: &'a str,
}

/// Scan a template for `$VAR` / `$$$VAR` tokens, in byte order.
///
/// Non-sigil bytes (including multi-byte UTF-8) and `$`/`$$$` sigils with
/// no following name run are skipped, never yielded — they are literal
/// template text. Yields one [`TemplateToken`] per named capture reference.
pub(crate) fn template_tokens(template: &str) -> impl Iterator<Item = TemplateToken<'_>> {
    let bytes = template.as_bytes();
    let mut i = 0usize;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            let rest = &template[i..];
            let (sigil_len, variadic) = if rest.starts_with("$$$") {
                (3, true)
            } else if rest.starts_with('$') {
                (1, false)
            } else {
                // Advance over a whole UTF-8 char — never step mid-sequence,
                // so `&template[i..]` stays on a char boundary. (`$` is
                // ASCII and can never appear inside a multi-byte sequence,
                // so the starts_with checks above only fire at true sigils.)
                let ch = rest.chars().next().expect("non-empty rest");
                i += ch.len_utf8();
                continue;
            };
            let after = &rest[sigil_len..];
            let name_end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            let name = &after[..name_end];
            if name.is_empty() {
                i += sigil_len;
                continue;
            }
            let token = TemplateToken {
                start: i,
                end: i + sigil_len + name_end,
                variadic,
                name,
            };
            i = token.end;
            return Some(token);
        }
        None
    })
}

/// Extract the replacement text for one capture binding.
///
/// `$VAR` expects a single node object; `$$$VAR` expects an array of node
/// objects. A shape mismatch (e.g. a `$$$VAR` in the template against a
/// single-node capture) degrades gracefully: single object → its text,
/// array → joined texts, anything else → empty.
fn capture_text(value: &serde_json::Value) -> String {
    match value {
        // Array capture, whether bound as `$$$VAR` or referenced as `$VAR`
        // (defensive — load-time validation permits same-name cross-sigil
        // references by token-name comparison): join all bound nodes' text.
        serde_json::Value::Array(nodes) => nodes
            .iter()
            .filter_map(node_text)
            .collect::<Vec<_>>()
            .concat(),
        node => match node_text(node) {
            Some(text) => text.to_string(),
            None => String::new(),
        },
    }
}

/// The `text` field of a capture node object, if present.
fn node_text(node: &serde_json::Value) -> Option<&str> {
    node.get("text").and_then(|t| t.as_str())
}

/// One rewrite candidate within a single file: the byte span to replace and
/// the substitution text that replaces it.
///
/// Rule-agnostic by design — targets from several rules land in the same
/// list and are planned uniformly (task AC: same span-sort-and-overlap
/// logic regardless of which rule produced a match).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteTarget {
    pub start_byte: u32,
    pub end_byte: u32,
    pub replacement: String,
}

/// Per-finding outcome of an `--apply` run, surfaced in scan JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RewriteStatus {
    /// The matched span was replaced and the file written to disk.
    Applied,
    /// The file has uncommitted git changes and `--force` was not passed.
    SkippedDirty,
    /// The match's span overlapped an earlier-applied span in the same file.
    SkippedOverlap,
    /// The rewrite could not be applied for another reason (unreadable
    /// file, non-char-boundary span, write failure).
    SkippedConflict,
}

/// A target's fate after per-file planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewritePlan {
    /// The target is applied in the single file pass.
    Apply,
    /// The target's span overlaps an earlier-sorted applied target's span
    /// and is skipped (reported, not applied, no crash).
    SkippedOverlap,
}

/// Plan a single file's rewrite targets: sort by span (start, then end) and
/// mark each target Apply or SkippedOverlap in one pass.
///
/// A target is applied iff its start is at/after the end of the last
/// applied target — adjacent spans (`[0,5)` then `[5,10)`) do not overlap
/// and both apply. First-sorted wins: an overlapping later target is
/// skipped and reported rather than corrupting the file via double writes.
///
/// Returns one `RewritePlan` per input target, in the same order the
/// targets were passed in (callers map decisions back to findings by
/// position), after planning against the span-sorted view internally.
pub fn plan_file_rewrites(targets: &[RewriteTarget]) -> Vec<RewritePlan> {
    let mut order: Vec<usize> = (0..targets.len()).collect();
    order.sort_by_key(|&i| (targets[i].start_byte, targets[i].end_byte));

    let mut plans: Vec<RewritePlan> = vec![RewritePlan::SkippedOverlap; targets.len()];
    let mut last_end: u32 = 0;
    for &i in &order {
        let t = &targets[i];
        if t.start_byte >= last_end {
            plans[i] = RewritePlan::Apply;
            last_end = last_end.max(t.end_byte);
        }
        // else: overlaps the previous applied span → stays SkippedOverlap.
    }
    plans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single(text: &str) -> serde_json::Value {
        serde_json::json!({ "kind": "identifier", "text": text })
    }

    fn captures(pairs: Vec<(&str, serde_json::Value)>) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (name, value) in pairs {
            map.insert(name.to_string(), value);
        }
        serde_json::Value::Object(map)
    }

    #[test]
    fn single_capture_substitutes_byte_exact() {
        let caps = captures(vec![("MSG", single("\"hi\""))]);
        let out = substitute("console.info($MSG);", &caps);
        assert_eq!(out, "console.info(\"hi\");");
    }

    #[test]
    fn multiple_captures_in_one_template() {
        let caps = captures(vec![
            ("KEY", single("api_key")),
            ("VAL", single("\"sk-abc\"")),
        ]);
        let out = substitute("$KEY = $VAL", &caps);
        assert_eq!(out, "api_key = \"sk-abc\"");
    }

    #[test]
    fn variadic_capture_joins_bound_nodes_without_separator() {
        let caps = captures(vec![(
            "ARGS",
            serde_json::json!([single("a"), single("b"), single("c")]),
        )]);
        let out = substitute("f($$$ARGS)", &caps);
        assert_eq!(out, "f(abc)");
    }

    #[test]
    fn variadic_capture_with_zero_nodes_substitutes_empty() {
        let caps = captures(vec![("ARGS", serde_json::json!([]))]);
        let out = substitute("f($$$ARGS)", &caps);
        assert_eq!(out, "f()");
    }

    #[test]
    fn literal_text_is_preserved_byte_exact() {
        let caps = captures(vec![("VAR", single("x"))]);
        let out = substitute("before $VAR after — unchanged", &caps);
        assert_eq!(out, "before x after — unchanged");
    }

    #[test]
    fn unknown_capture_name_passes_through_verbatim() {
        let caps = captures(vec![("OTHER", single("x"))]);
        let out = substitute("$MISSING stays", &caps);
        assert_eq!(out, "$MISSING stays");
    }

    #[test]
    fn bare_dollar_without_name_is_literal() {
        let caps = captures(vec![]);
        assert_eq!(substitute("cost is $5", &caps), "cost is $5");
        assert_eq!(substitute("a $ b", &caps), "a $ b");
        assert_eq!(substitute("$$$ tail", &caps), "$$$ tail");
    }

    #[test]
    fn multi_byte_utf8_content_survives_untouched() {
        let caps = captures(vec![
            ("MSG", single("héllo — wörld")),
            ("ARGS", serde_json::json!([single("日本"), single("語")])),
        ]);
        // Template and capture content both contain multi-byte UTF-8; the
        // tokenizer must not split a char boundary.
        let out = substitute("s($MSG) + s($$$ARGS) — ünïcode", &caps);
        assert_eq!(out, "s(héllo — wörld) + s(日本語) — ünïcode");
    }

    #[test]
    fn cross_sigil_same_name_reference_degrades_gracefully() {
        // Load-time validation is token-name based, so a `$$$ARGS` in the
        // template may legally pair with a single-node capture (or vice
        // versa). Substitution must not panic — it joins/uses what's there.
        let single_cap = captures(vec![("ARGS", single("only"))]);
        assert_eq!(substitute("$ARGS", &single_cap), "only");
        assert_eq!(substitute("$$$ARGS", &single_cap), "only");

        let variadic_cap = captures(vec![(
            "ARGS",
            serde_json::json!([single("a"), single("b")]),
        )]);
        assert_eq!(substitute("$ARGS", &variadic_cap), "ab");
        assert_eq!(substitute("$$$ARGS", &variadic_cap), "ab");
    }

    #[test]
    fn name_run_terminates_at_non_identifier() {
        let caps = captures(vec![("A", single("1"))]);
        // `$A` followed by `;` — the name must not swallow the punctuation.
        assert_eq!(substitute("x$A;y", &caps), "x1;y");
        // `$A` immediately followed by more identifier chars is a longer
        // name (`AB`), not `A` + literal `B`.
        assert_eq!(substitute("$AB", &caps), "$AB");
    }

    fn target(start: u32, end: u32) -> RewriteTarget {
        RewriteTarget {
            start_byte: start,
            end_byte: end,
            replacement: format!("[{start},{end})"),
        }
    }

    #[test]
    fn non_overlapping_targets_all_apply_in_single_pass() {
        let targets = vec![target(10, 20), target(0, 5), target(25, 30)];
        let plans = plan_file_rewrites(&targets);
        assert_eq!(
            plans,
            vec![RewritePlan::Apply, RewritePlan::Apply, RewritePlan::Apply]
        );
    }

    #[test]
    fn adjacent_spans_do_not_overlap() {
        let targets = vec![target(0, 5), target(5, 10)];
        let plans = plan_file_rewrites(&targets);
        assert_eq!(plans, vec![RewritePlan::Apply, RewritePlan::Apply]);
    }

    #[test]
    fn later_sorted_overlapping_target_is_skipped_earlier_applied() {
        // [0,10) wins; [5,15) and [2,4) both overlap it and are skipped.
        let targets = vec![target(5, 15), target(0, 10), target(2, 4)];
        let plans = plan_file_rewrites(&targets);
        assert_eq!(
            plans,
            vec![
                RewritePlan::SkippedOverlap,
                RewritePlan::Apply,
                RewritePlan::SkippedOverlap
            ]
        );
    }

    #[test]
    fn identical_spans_first_wins() {
        let targets = vec![target(0, 5), target(0, 5)];
        let plans = plan_file_rewrites(&targets);
        assert_eq!(plans, vec![RewritePlan::Apply, RewritePlan::SkippedOverlap]);
    }

    #[test]
    fn nested_span_after_an_apply_is_skipped_then_later_span_applies() {
        // [0,10) applies; [2,3) nested inside it skipped; [10,12) adjacent
        // to the applied span applies (start == last_end).
        let targets = vec![target(2, 3), target(0, 10), target(10, 12)];
        let plans = plan_file_rewrites(&targets);
        assert_eq!(
            plans,
            vec![
                RewritePlan::SkippedOverlap,
                RewritePlan::Apply,
                RewritePlan::Apply
            ]
        );
    }

    #[test]
    fn rule_agnostic_input_treats_mixed_targets_uniformly() {
        // No rule identity is part of the input — targets from two rules
        // land in one list and are planned by span alone.
        let targets = vec![target(0, 5), target(3, 8), target(9, 12)];
        let plans = plan_file_rewrites(&targets);
        assert_eq!(
            plans,
            vec![
                RewritePlan::Apply,
                RewritePlan::SkippedOverlap,
                RewritePlan::Apply
            ]
        );
    }

    #[test]
    fn empty_target_list_plans_empty() {
        assert!(plan_file_rewrites(&[]).is_empty());
    }
}
