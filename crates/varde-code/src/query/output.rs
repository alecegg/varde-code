//! Output post-processing applied at the single query/scan serialization
//! boundary, after a mode has produced its payload but before it becomes the
//! `{ok, data}` envelope.
//!
//! Two token-efficiency transforms (audit F5 + F6), both keyed off the mode's
//! own input object so no signature threading is needed:
//!
//! - **F5 repo-relative paths.** The MCP schema requires an absolute
//!   `repoRoot`, and the index stores each file as the walker yielded it —
//!   literally `{repoRoot}{sep}{relative}` (walkdir preserves the exact root
//!   prefix). So every emitted path re-states the absolute prefix (~200×/nav_map).
//!   We strip that prefix from every string value in the payload, turning
//!   `/abs/repo/src/main.rs` into `src/main.rs`. Prefix-matching (not a key
//!   allowlist) so bare path strings inside arrays are caught too, and values
//!   outside the repo (e.g. the `~/.config/.../index.db` `dbPath`) are left
//!   untouched because they don't carry the prefix. Round-trips: query inputs
//!   resolve a relative `filePath` against the stored path via a suffix match
//!   (`simple::file_id`), so a relative path fed back in still matches.
//!   Opt out with `absolutePaths: true`.
//!
//! - **F6 line-only spans.** A `span` object carries six fields
//!   (start/end × byte/line/col) where the line pair usually suffices for
//!   navigation. We drop the byte and column fields, keeping `start_line` /
//!   `end_line`. The byte offsets an `--apply` rewrite needs are read from the
//!   in-memory `Finding` before this runs, so trimming the JSON never affects
//!   splicing. Opt in to the full span with `includeSpanDetail: true`.
//!
//! Both transforms are idempotent, so the double pass a `batch` payload sees
//! (once per sub-call, once for the aggregate) is harmless.

use serde_json::Value;

/// Apply the F5 (relativize) and F6 (span trim) transforms to a mode payload
/// in place, reading the opt-out/opt-in flags and `repoRoot` from `input`.
pub(crate) fn postprocess(data: &mut Value, input: &Value) {
    let keep_absolute = input
        .get("absolutePaths")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !keep_absolute && let Some(root) = input.get("repoRoot").and_then(Value::as_str) {
        let root = root.trim_end_matches(std::path::MAIN_SEPARATOR);
        if !root.is_empty() {
            let prefix = format!("{root}{}", std::path::MAIN_SEPARATOR);
            relativize(data, &prefix);
        }
    }

    let keep_span_detail = input
        .get("includeSpanDetail")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !keep_span_detail {
        trim_spans(data);
    }
}

/// Strip `prefix` from the front of every string value in the tree.
fn relativize(v: &mut Value, prefix: &str) {
    match v {
        Value::String(s) => {
            if let Some(rest) = s.strip_prefix(prefix) {
                *s = rest.to_string();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|x| relativize(x, prefix)),
        Value::Object(map) => map.values_mut().for_each(|x| relativize(x, prefix)),
        _ => {}
    }
}

/// Remove the byte/column fields from every `span` object, keeping the line
/// pair. Recurses through the whole tree so nested spans (e.g. a finding's
/// `location.span`, a symbol row's `span`) are all trimmed.
fn trim_spans(v: &mut Value) {
    match v {
        Value::Array(items) => items.iter_mut().for_each(trim_spans),
        Value::Object(map) => {
            if let Some(Value::Object(span)) = map.get_mut("span") {
                span.remove("start_byte");
                span.remove("end_byte");
                span.remove("start_col");
                span.remove("end_col");
            }
            map.values_mut().for_each(trim_spans);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sep() -> char {
        std::path::MAIN_SEPARATOR
    }

    #[test]
    fn relativizes_paths_and_trims_spans_by_default() {
        let root = format!("{s}abs{s}repo", s = sep());
        let input = json!({ "repoRoot": root });
        let mut data = json!({
            "file": format!("{root}{s}src{s}main.rs", s = sep()),
            "reachable": [
                format!("{root}{s}src{s}a.rs", s = sep()),
                format!("{root}{s}src{s}b.rs", s = sep()),
            ],
            "location": {
                "span": {
                    "start_byte": 10, "end_byte": 26,
                    "start_line": 2, "start_col": 4,
                    "end_line": 3, "end_col": 20
                }
            },
            "dbPath": format!("{s}home{s}u{s}.config{s}index.db", s = sep()),
        });

        postprocess(&mut data, &input);

        // F5: repo paths are relative; the out-of-repo dbPath is untouched.
        assert_eq!(data["file"], "src/main.rs".replace('/', &sep().to_string()));
        assert_eq!(
            data["reachable"][0],
            "src/a.rs".replace('/', &sep().to_string())
        );
        assert_eq!(
            data["dbPath"],
            format!("{s}home{s}u{s}.config{s}index.db", s = sep())
        );

        // F6: byte/col dropped, line pair kept.
        let span = &data["location"]["span"];
        assert_eq!(span["start_line"], 2);
        assert_eq!(span["end_line"], 3);
        assert!(span.get("start_byte").is_none());
        assert!(span.get("end_byte").is_none());
        assert!(span.get("start_col").is_none());
        assert!(span.get("end_col").is_none());
    }

    #[test]
    fn absolute_paths_opt_out_keeps_prefix() {
        let root = format!("{s}abs{s}repo", s = sep());
        let input = json!({ "repoRoot": root, "absolutePaths": true });
        let abs = format!("{root}{s}src{s}main.rs", s = sep());
        let mut data = json!({ "file": abs });
        postprocess(&mut data, &input);
        assert_eq!(data["file"], format!("{root}{s}src{s}main.rs", s = sep()));
    }

    #[test]
    fn include_span_detail_opt_in_keeps_byte_and_col() {
        let input = json!({ "includeSpanDetail": true });
        let mut data = json!({
            "span": { "start_byte": 1, "end_byte": 2, "start_line": 1, "start_col": 0, "end_line": 1, "end_col": 5 }
        });
        postprocess(&mut data, &input);
        assert_eq!(data["span"]["start_byte"], 1);
        assert_eq!(data["span"]["end_col"], 5);
    }

    #[test]
    fn relativize_is_idempotent() {
        let root = format!("{s}abs{s}repo", s = sep());
        let input = json!({ "repoRoot": root });
        let mut data = json!({ "file": format!("{root}{s}src{s}main.rs", s = sep()) });
        postprocess(&mut data, &input);
        let once = data.clone();
        postprocess(&mut data, &input);
        assert_eq!(data, once);
    }

    #[test]
    fn dbpath_only_call_without_reporoot_leaves_paths_alone() {
        // No repoRoot → we can't know the prefix, so paths pass through as-is
        // (spans still trim).
        let input = json!({ "dbPath": "/tmp/x.db" });
        let abs = format!("{s}abs{s}repo{s}src{s}main.rs", s = sep());
        let mut data = json!({ "file": abs.clone() });
        postprocess(&mut data, &input);
        assert_eq!(data["file"], abs);
    }
}
