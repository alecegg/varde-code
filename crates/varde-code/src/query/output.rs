//! Output post-processing applied at the single query/scan serialization
//! boundary, after a mode has produced its payload but before it becomes the
//! `{ok, data, meta}` envelope.
//!
//! Two token-efficiency transforms (audit F5 + F6), both keyed off the mode's
//! own input object so no signature threading is needed:
//!
//! - **F5 repo-relative paths.** The MCP schema requires an absolute
//!   `repoRoot`, and the index stores each file as the walker yielded it —
//!   literally `{repoRoot}{sep}{relative}` (walkdir preserves the exact root
//!   prefix). So every emitted path re-states the absolute prefix (~200×/nav_map).
//!   We strip that prefix from every string value in the payload, turning
//!   `/abs/repo/src/main.rs` into `src/main.rs`. Only known repository-path
//!   fields are transformed, so source text and external paths stay verbatim.
//!   Round-trips: query inputs
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

use serde::Serialize;
use serde_json::Value;

/// Machine-output metadata supplied with every envelope.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct OutputMeta {
    /// Whether the compact path/span output policy was applied.
    pub compact: bool,
    /// Whether a mode reported omitted results.
    pub truncated: bool,
}

/// Render a result with the output policy derived from its input.
///
/// Errors have no payload to compact, so they retain default metadata. This
/// avoids reporting a policy that could not have run.
pub fn render_with_input(result: Result<Value, super::ApiError>, input: &Value) -> String {
    match result {
        Ok(mut data) => {
            let meta = postprocess(&mut data, input);
            super::render_with_meta(Ok(data), meta)
        }
        Err(error) => super::render_with_meta(Err(error), OutputMeta::default()),
    }
}

/// Apply the F5 (relativize) and F6 (span trim) transforms to a mode payload
/// in place, reading the opt-out/opt-in flags and path root from `input`.
pub(crate) fn postprocess(data: &mut Value, input: &Value) -> OutputMeta {
    let keep_absolute = input
        .get("absolutePaths")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let root = input
        .get("repoRoot")
        .or_else(|| input.get("rulesDir"))
        .and_then(Value::as_str)
        .map(|root| root.trim_end_matches(std::path::MAIN_SEPARATOR))
        .filter(|root| !root.is_empty());
    if !keep_absolute && let Some(root) = root {
        let prefix = format!("{root}{}", std::path::MAIN_SEPARATOR);
        relativize(data, &prefix);
    }

    let keep_span_detail = input
        .get("includeSpanDetail")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !keep_span_detail {
        trim_spans(data);
    }

    OutputMeta {
        compact: root.is_some() && (!keep_absolute || !keep_span_detail),
        truncated: is_truncated(data),
    }
}

fn is_truncated(data: &Value) -> bool {
    data.pointer("/guide/truncated")
        .and_then(Value::as_object)
        .is_some_and(|sections| !sections.is_empty())
        || data.as_array().is_some_and(|results| {
            results.iter().any(|result| {
                result
                    .pointer("/meta/truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
        })
}

/// Strip `prefix` from known repository-path fields only.
fn relativize(v: &mut Value, prefix: &str) {
    match v {
        Value::Array(items) => items.iter_mut().for_each(|item| relativize(item, prefix)),
        Value::Object(map) => {
            for (key, value) in map {
                if is_repo_path_field(key) {
                    relativize_path(value, prefix);
                }
                relativize(value, prefix);
            }
        }
        _ => {}
    }
}

fn is_repo_path_field(field: &str) -> bool {
    matches!(
        field,
        "file"
            | "filePath"
            | "sourceFile"
            | "targetFile"
            | "coversFile"
            | "path"
            | "files"
            | "seedFiles"
            | "targetDir"
            | "changedFiles"
            | "changedFilesSample"
            | "reachable"
            | "readingOrder"
            | "members"
            | "from"
            | "to"
    )
}

fn relativize_path(v: &mut Value, prefix: &str) {
    match v {
        Value::String(path) => {
            if let Some(relative) = path.strip_prefix(prefix) {
                *path = relative.to_string();
            }
        }
        Value::Array(paths) => paths
            .iter_mut()
            .for_each(|path| relativize_path(path, prefix)),
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
            "source": format!("{root}{s}this is source text", s = sep()),
        });

        let meta = postprocess(&mut data, &input);

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
        assert_eq!(
            data["source"],
            format!("{root}{s}this is source text", s = sep())
        );
        assert!(meta.compact);
        assert!(!meta.truncated);

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
    fn reports_nav_map_truncation_in_metadata() {
        let input = json!({});
        let mut data = json!({
            "guide": { "truncated": { "symbols": { "shown": 1, "total": 2 } } }
        });

        let meta = postprocess(&mut data, &input);

        assert!(meta.truncated);
    }

    #[test]
    fn reports_batch_child_truncation_in_metadata() {
        let input = json!({});
        let mut data = json!([
            { "mode": "find_pattern", "meta": { "truncated": true } }
        ]);

        let meta = postprocess(&mut data, &input);

        assert!(meta.truncated);
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
