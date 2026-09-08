//! Shared scaffold for session-start hook install targets.
//!
//! Mirrors `skills.rs`'s `SKILL_PACKS`/`InstallResult`/`RemoveResult` shape,
//! generalized for two install kinds instead of one: whole-file writes
//! (`HookKind::WriteFile`, same as `skills_install`) and merges into a
//! shared config file the user may already have unrelated content in
//! (`HookKind::MergeInto`, new — JSON via `serde_json` or TOML via
//! `toml-edit`, which preserves the user's comments/formatting unlike plain
//! `toml`).
//!
//! This module intentionally defines no concrete [`HookTarget`]s yet — the
//! sibling tasks (Claude Code, Codex, opencode, Pi) each add one to
//! [`HOOK_TARGETS`]. Only the shared merge/diff helpers are exercised by
//! this task's tests.

use std::io;
use std::path::{Path, PathBuf};

/// Stable marker identifying an entry this tool injected, used both to
/// merge idempotently and to scope `remove`'s diff check to just this
/// tool's own addition — never the whole shared config file.
pub const MARKER_KEY: &str = "__source";
pub const MARKER_VALUE: &str = "varde-code";

/// A path relative to an agent's install target directory.
pub type RelPath = &'static str;

/// How to merge an injected entry into an existing structured file without
/// clobbering unrelated content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Merge one key into a top-level JSON object (`serde_json`).
    Json,
    /// Merge one table into a top-level TOML document (`toml_edit`),
    /// preserving the rest of the file's comments/formatting.
    Toml,
}

/// How a hook is installed for a given agent.
#[derive(Debug, Clone, Copy)]
pub enum HookKind {
    /// Merge the injected entry into an existing (or newly created) file at
    /// `rel_path`, using `merge` to combine with any unrelated content.
    MergeInto(RelPath, MergeStrategy),
    /// Write embedded `contents` verbatim to `rel_path`, overwriting only
    /// this tool's own file (same shape as `skills_install`).
    WriteFile(RelPath, &'static str),
}

/// One agent's hook install target.
#[derive(Debug, Clone, Copy)]
pub struct HookTarget {
    /// Bare agent name, e.g. `"claude"`.
    pub agent: &'static str,
    pub kind: HookKind,
}

/// Bare agent name for the Claude Code hook target.
pub const CLAUDE_AGENT: &str = "claude";

/// The command injected into Claude Code's `SessionStart` hook. `$(pwd)` is
/// resolved by the shell at hook-run time (not baked in statically), so the
/// `repoRoot` always reflects the session's actual working directory.
pub const CLAUDE_SESSION_START_COMMAND: &str =
    "varde-code nav_map --json \"{\\\"repoRoot\\\":\\\"$(pwd)\\\"}\" --format text";

/// Bare agent name for the Codex CLI hook target.
pub const CODEX_AGENT: &str = "codex";

/// The command injected into Codex's `SessionStart` hook. Same command as
/// Claude Code's; `$(pwd)` is resolved by the shell at hook-run time.
pub const CODEX_SESSION_START_COMMAND: &str = CLAUDE_SESSION_START_COMMAND;

/// Bare agent name for the opencode hook target.
pub const OPENCODE_AGENT: &str = "opencode";

/// The embedded opencode plugin JS, written verbatim to
/// `~/.config/opencode/plugin/varde-code-nav-map.js` (user-level default).
/// Shells out to `varde-code nav_map` via opencode's `$` executor on
/// `session.start` and injects the result into session context. Best-effort
/// per opencode's documented plugin API — see the plan's Assumptions
/// section; no live opencode install was available to verify against.
pub const OPENCODE_PLUGIN_JS: &str = include_str!("../assets/hooks/opencode-plugin.js");

/// Bare agent name for the Pi coding agent hook target.
pub const PI_AGENT: &str = "pi";

/// The embedded Pi native extension JS, written verbatim to
/// `~/.pi/agent/extensions/pi-extension.js` (user-level default). Shells
/// out to `varde-code nav_map` on Pi's `session_start` event and
/// returns `{ systemPrompt: ... }` to inject the result into session
/// context. Uses Pi's native first-party `ExtensionAPI` factory function
/// directly (`export default function(pi) {...}`) — no third-party
/// `pi-hooks` compat shim. Best-effort per Pi's documented extension API —
/// see the plan's Assumptions section; no live Pi install was available to
/// verify against.
pub const PI_EXTENSION_JS: &str = include_str!("../assets/hooks/pi-extension.js");

/// Concrete per-agent hook targets. Claude Code merges a `SessionStart`
/// entry into `settings.json` (callers resolve the user-level default path
/// `~/.claude/settings.json`; `target_dir` passed to [`install_hooks`] is
/// just the directory containing it). Codex merges a `SessionStart` entry
/// into `config.toml` (user-level default `~/.codex/config.toml` — chosen
/// over project-local `.codex/config.toml` because of a known upstream
/// reliability bug with project-local Codex hooks; see the plan's
/// Assumptions section). opencode writes a whole-file plugin JS (user-level
/// default `~/.config/opencode/plugin/`), reusing the scaffold's generic
/// `WriteFile` install/remove logic (same whole-file-diff protection as
/// `skills_remove`) unchanged. Sibling task adds Pi.
pub const HOOK_TARGETS: &[HookTarget] = &[
    HookTarget {
        agent: CLAUDE_AGENT,
        kind: HookKind::MergeInto("settings.json", MergeStrategy::Json),
    },
    HookTarget {
        agent: CODEX_AGENT,
        kind: HookKind::MergeInto("config.toml", MergeStrategy::Toml),
    },
    HookTarget {
        agent: OPENCODE_AGENT,
        kind: HookKind::WriteFile("plugin/varde-code-nav-map.js", OPENCODE_PLUGIN_JS),
    },
    HookTarget {
        agent: PI_AGENT,
        kind: HookKind::WriteFile("pi-extension.js", PI_EXTENSION_JS),
    },
];

/// Outcome of installing a single hook target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    pub agent: String,
    pub path: PathBuf,
    pub written: bool,
    /// `true` when the target already had this tool's entry/file and
    /// `force` was false, so it was left untouched.
    pub skipped_existing: bool,
}

/// Outcome of removing a single previously installed hook target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveResult {
    pub agent: String,
    pub path: PathBuf,
    pub removed: bool,
    /// `true` when the injected entry (or, for whole-file targets, the
    /// whole file) no longer matches what this tool would have written
    /// (locally edited) and `force` was false, so it was left alone.
    pub skipped_modified: bool,
}

/// Install every target in `targets` under `target_dir`. Existing entries
/// are left untouched unless `force` is true.
pub fn install_hooks(
    target_dir: &Path,
    targets: &[HookTarget],
    force: bool,
) -> io::Result<Vec<InstallResult>> {
    let mut results = Vec::new();
    for target in targets {
        let result = match target.kind {
            HookKind::WriteFile(rel_path, contents) => {
                install_write_file(target_dir, target.agent, rel_path, contents, force)?
            }
            HookKind::MergeInto(rel_path, strategy) => {
                install_merge(target_dir, target.agent, rel_path, strategy, force)?
            }
        };
        results.push(result);
    }
    Ok(results)
}

/// Remove every target in `targets` under `target_dir`. A target edited
/// since install (content-scoped for merge targets) is left in place unless
/// `force` is true.
pub fn remove_hooks(
    target_dir: &Path,
    targets: &[HookTarget],
    force: bool,
) -> io::Result<Vec<RemoveResult>> {
    let mut results = Vec::new();
    for target in targets {
        let result = match target.kind {
            HookKind::WriteFile(rel_path, contents) => {
                remove_write_file(target_dir, target.agent, rel_path, contents, force)?
            }
            HookKind::MergeInto(rel_path, strategy) => {
                remove_merge(target_dir, target.agent, rel_path, strategy, force)?
            }
        };
        if let Some(result) = result {
            results.push(result);
        }
    }
    Ok(results)
}

/// Join a `RelPath` under `target_dir`, asserting the relative path cannot
/// escape it. `RelPath` is always a `&'static str` constant today, so a `..`
/// or absolute component would be a programming error, not untrusted input —
/// the `debug_assert!` catches such a constant in tests/debug before it can
/// write outside the install root.
fn safe_join(target_dir: &Path, rel_path: RelPath) -> PathBuf {
    debug_assert!(
        !rel_path.contains("..") && !Path::new(rel_path).is_absolute(),
        "hook rel_path must stay within target_dir: {rel_path:?}",
    );
    target_dir.join(rel_path)
}

fn install_write_file(
    target_dir: &Path,
    agent: &str,
    rel_path: RelPath,
    contents: &str,
    force: bool,
) -> io::Result<InstallResult> {
    let path = safe_join(target_dir, rel_path);
    let exists = path.exists();
    if exists && !force {
        return Ok(InstallResult {
            agent: agent.to_string(),
            path,
            written: false,
            skipped_existing: true,
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, contents)?;
    Ok(InstallResult {
        agent: agent.to_string(),
        path,
        written: true,
        skipped_existing: false,
    })
}

fn remove_write_file(
    target_dir: &Path,
    agent: &str,
    rel_path: RelPath,
    contents: &str,
    force: bool,
) -> io::Result<Option<RemoveResult>> {
    let path = safe_join(target_dir, rel_path);
    if !path.exists() {
        return Ok(None);
    }
    let on_disk = std::fs::read_to_string(&path)?;
    let modified = on_disk != contents;
    if modified && !force {
        return Ok(Some(RemoveResult {
            agent: agent.to_string(),
            path,
            removed: false,
            skipped_modified: true,
        }));
    }
    std::fs::remove_file(&path)?;
    Ok(Some(RemoveResult {
        agent: agent.to_string(),
        path,
        removed: true,
        skipped_modified: false,
    }))
}

fn install_merge(
    target_dir: &Path,
    agent: &str,
    rel_path: RelPath,
    strategy: MergeStrategy,
    force: bool,
) -> io::Result<InstallResult> {
    let path = safe_join(target_dir, rel_path);
    match strategy {
        MergeStrategy::Json => {
            let mut doc = if path.exists() {
                let text = std::fs::read_to_string(&path)?;
                serde_json::from_str::<serde_json::Value>(&text)
                    .unwrap_or(serde_json::Value::Object(Default::default()))
            } else {
                serde_json::Value::Object(Default::default())
            };
            let already_present = if agent == CLAUDE_AGENT {
                claude_session_start_entry_index(&doc).is_some()
            } else {
                json_marker_present(&doc)
            };
            if already_present && !force {
                return Ok(InstallResult {
                    agent: agent.to_string(),
                    path,
                    written: false,
                    skipped_existing: true,
                });
            }
            if agent == CLAUDE_AGENT {
                claude_install_session_start(&mut doc);
            } else {
                json_merge_marker(&mut doc);
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let text = serde_json::to_string_pretty(&doc).map_err(io::Error::from)?;
            std::fs::write(&path, text)?;
            Ok(InstallResult {
                agent: agent.to_string(),
                path,
                written: true,
                skipped_existing: false,
            })
        }
        MergeStrategy::Toml => {
            let mut doc = if path.exists() {
                let text = std::fs::read_to_string(&path)?;
                text.parse::<toml_edit::DocumentMut>()
                    .unwrap_or_else(|_| toml_edit::DocumentMut::new())
            } else {
                toml_edit::DocumentMut::new()
            };
            let already_present = if agent == CODEX_AGENT {
                codex_session_start_present(&doc)
            } else {
                toml_marker_present(&doc)
            };
            if already_present && !force {
                return Ok(InstallResult {
                    agent: agent.to_string(),
                    path,
                    written: false,
                    skipped_existing: true,
                });
            }
            if agent == CODEX_AGENT {
                codex_install_session_start(&mut doc);
            } else {
                toml_merge_marker(&mut doc);
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, doc.to_string())?;
            Ok(InstallResult {
                agent: agent.to_string(),
                path,
                written: true,
                skipped_existing: false,
            })
        }
    }
}

fn remove_merge(
    target_dir: &Path,
    agent: &str,
    rel_path: RelPath,
    strategy: MergeStrategy,
    force: bool,
) -> io::Result<Option<RemoveResult>> {
    let path = safe_join(target_dir, rel_path);
    if !path.exists() {
        return Ok(None);
    }
    match strategy {
        MergeStrategy::Json => {
            let text = std::fs::read_to_string(&path)?;
            let mut doc: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };
            if agent == CLAUDE_AGENT {
                if claude_session_start_entry_index(&doc).is_none() {
                    return Ok(None);
                }
                let modified = claude_session_start_modified(&doc);
                if modified && !force {
                    return Ok(Some(RemoveResult {
                        agent: agent.to_string(),
                        path,
                        removed: false,
                        skipped_modified: true,
                    }));
                }
                claude_remove_session_start(&mut doc);
            } else {
                if !json_marker_present(&doc) {
                    return Ok(None);
                }
                let modified = json_marker_modified(&doc);
                if modified && !force {
                    return Ok(Some(RemoveResult {
                        agent: agent.to_string(),
                        path,
                        removed: false,
                        skipped_modified: true,
                    }));
                }
                json_remove_marker(&mut doc);
            }
            let text = serde_json::to_string_pretty(&doc).map_err(io::Error::from)?;
            std::fs::write(&path, text)?;
            Ok(Some(RemoveResult {
                agent: agent.to_string(),
                path,
                removed: true,
                skipped_modified: false,
            }))
        }
        MergeStrategy::Toml => {
            let text = std::fs::read_to_string(&path)?;
            let mut doc = match text.parse::<toml_edit::DocumentMut>() {
                Ok(d) => d,
                Err(_) => return Ok(None),
            };
            if agent == CODEX_AGENT {
                if !codex_session_start_present(&doc) {
                    return Ok(None);
                }
                let modified = codex_session_start_modified(&doc);
                if modified && !force {
                    return Ok(Some(RemoveResult {
                        agent: agent.to_string(),
                        path,
                        removed: false,
                        skipped_modified: true,
                    }));
                }
                codex_remove_session_start(&mut doc);
            } else {
                if !toml_marker_present(&doc) {
                    return Ok(None);
                }
                let modified = toml_marker_modified(&doc);
                if modified && !force {
                    return Ok(Some(RemoveResult {
                        agent: agent.to_string(),
                        path,
                        removed: false,
                        skipped_modified: true,
                    }));
                }
                toml_remove_marker(&mut doc);
            }
            std::fs::write(&path, doc.to_string())?;
            Ok(Some(RemoveResult {
                agent: agent.to_string(),
                path,
                removed: true,
                skipped_modified: false,
            }))
        }
    }
}

// --- Claude Code `hooks.SessionStart` merge/diff helpers --------------
//
// Claude Code's `settings.json` shape for a `SessionStart` hook is:
// `{"hooks": {"SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "..."}]}]}}`.
// The marker is embedded on the innermost `command` hook object (not the
// document root) so identity/diff-scoping stays scoped to just this
// tool's own entry, never the whole `settings.json` file or the whole
// `SessionStart` array.

/// The exact `SessionStart` array entry this tool injects.
fn claude_session_start_entry() -> serde_json::Value {
    serde_json::json!({
        "matcher": "",
        "hooks": [
            {
                "type": "command",
                "command": CLAUDE_SESSION_START_COMMAND,
                MARKER_KEY: MARKER_VALUE,
            }
        ]
    })
}

/// Index of this tool's entry within `doc.hooks.SessionStart`, if present.
fn claude_session_start_entry_index(doc: &serde_json::Value) -> Option<usize> {
    doc.get("hooks")?
        .get("SessionStart")?
        .as_array()?
        .iter()
        .position(|entry| {
            entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|hooks| hooks.iter().any(|h| h.get(MARKER_KEY).is_some()))
                .unwrap_or(false)
        })
}

/// `true` when this tool's entry exists but no longer matches what this
/// tool would have written (i.e. the user edited it locally).
fn claude_session_start_modified(doc: &serde_json::Value) -> bool {
    match claude_session_start_entry_index(doc) {
        Some(idx) => doc["hooks"]["SessionStart"][idx] != claude_session_start_entry(),
        None => false,
    }
}

/// Insert (or replace, if already present and `force`d) this tool's
/// `SessionStart` entry into `doc`, leaving all other keys untouched.
fn claude_install_session_start(doc: &mut serde_json::Value) {
    if !doc.is_object() {
        *doc = serde_json::Value::Object(Default::default());
    }
    let hooks = doc
        .as_object_mut()
        .expect("object")
        .entry("hooks")
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    if !hooks.is_object() {
        *hooks = serde_json::Value::Object(Default::default());
    }
    let session_start = hooks
        .as_object_mut()
        .expect("object")
        .entry("SessionStart")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if !session_start.is_array() {
        *session_start = serde_json::Value::Array(Vec::new());
    }
    let array = session_start.as_array_mut().expect("array");
    match array.iter().position(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| hooks.iter().any(|h| h.get(MARKER_KEY).is_some()))
            .unwrap_or(false)
    }) {
        Some(idx) => array[idx] = claude_session_start_entry(),
        None => array.push(claude_session_start_entry()),
    }
}

/// Remove this tool's `SessionStart` entry from `doc`, pruning the
/// now-empty `SessionStart`/`hooks` containers if this was the only entry,
/// but never touching unrelated sibling keys.
fn claude_remove_session_start(doc: &mut serde_json::Value) {
    let Some(idx) = claude_session_start_entry_index(doc) else {
        return;
    };
    if let Some(array) = doc
        .get_mut("hooks")
        .and_then(|h| h.get_mut("SessionStart"))
        .and_then(|s| s.as_array_mut())
    {
        array.remove(idx);
    }
    let session_start_empty = doc
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(|s| s.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(false);
    if session_start_empty
        && let Some(hooks_obj) = doc.get_mut("hooks").and_then(|h| h.as_object_mut())
    {
        hooks_obj.remove("SessionStart");
    }
    let hooks_empty = doc
        .get("hooks")
        .and_then(|h| h.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    if hooks_empty && let Some(obj) = doc.as_object_mut() {
        obj.remove("hooks");
    }
}

// --- Codex `[hooks.session_start]` merge/diff helpers (toml_edit) -----
//
// Codex's `config.toml` shape for a `SessionStart` hook is assumed to be:
// `[hooks.session_start]` with a `command` key (a reasonable table shape
// consistent with Codex's documented `SessionStart` shell-command hook
// type — see the plan's Assumptions section for the medium-confidence
// citation on why user-level scope is used instead of project-local
// `.codex/config.toml`). The marker lives alongside `command` in the same
// table so identity/diff-scoping stays scoped to just this tool's own
// table, never the rest of `config.toml`.

fn codex_session_start_present(doc: &toml_edit::DocumentMut) -> bool {
    doc.get("hooks")
        .and_then(|h| h.get("session_start"))
        .and_then(|s| s.get(MARKER_KEY))
        .is_some()
}

/// `true` when this tool's `[hooks.session_start]` table exists but no
/// longer matches what this tool would have written (i.e. the user edited
/// it locally).
fn codex_session_start_modified(doc: &toml_edit::DocumentMut) -> bool {
    let Some(table) = doc.get("hooks").and_then(|h| h.get("session_start")) else {
        return false;
    };
    let command = table.get("command").and_then(|v| v.as_str());
    let marker = table.get(MARKER_KEY).and_then(|v| v.as_str());
    command != Some(CODEX_SESSION_START_COMMAND) || marker != Some(MARKER_VALUE)
}

/// Insert (or replace, if already present and `force`d) this tool's
/// `[hooks.session_start]` table into `doc`, leaving all other keys/tables
/// (including `hooks.*` siblings) untouched.
fn codex_install_session_start(doc: &mut toml_edit::DocumentMut) {
    if doc.get("hooks").is_none() || !doc["hooks"].is_table_like() {
        doc["hooks"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let hooks = doc["hooks"].as_table_like_mut().expect("table");
    if hooks.get("session_start").is_none() || !hooks.get("session_start").unwrap().is_table_like()
    {
        hooks.insert(
            "session_start",
            toml_edit::Item::Table(toml_edit::Table::new()),
        );
    }
    let session_start = hooks
        .get_mut("session_start")
        .expect("just inserted")
        .as_table_like_mut()
        .expect("table");
    session_start.insert("command", toml_edit::value(CODEX_SESSION_START_COMMAND));
    session_start.insert(MARKER_KEY, toml_edit::value(MARKER_VALUE));
}

/// Remove this tool's `[hooks.session_start]` table from `doc`, pruning the
/// now-empty `hooks` table if this was the only entry, but never touching
/// unrelated sibling keys/tables.
fn codex_remove_session_start(doc: &mut toml_edit::DocumentMut) {
    let Some(hooks) = doc.get_mut("hooks").and_then(|h| h.as_table_like_mut()) else {
        return;
    };
    hooks.remove("session_start");
    let hooks_empty = hooks.is_empty();
    if hooks_empty {
        doc.as_table_mut().remove("hooks");
    }
}

// --- JSON merge/diff helpers -----------------------------------------

/// The canonical injected entry, keyed by [`MARKER_KEY`]/[`MARKER_VALUE`].
/// Sibling tasks that need richer per-agent payloads can extend this by
/// merging additional keys alongside the marker; this scaffold only
/// establishes marker identity.
fn marker_object() -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert(
        MARKER_KEY.to_string(),
        serde_json::Value::String(MARKER_VALUE.to_string()),
    );
    map
}

/// Merge the injected marker entry into `doc`'s top-level object without
/// touching any other keys.
fn json_merge_marker(doc: &mut serde_json::Value) {
    if !doc.is_object() {
        *doc = serde_json::Value::Object(Default::default());
    }
    let obj = doc.as_object_mut().expect("object");
    for (k, v) in marker_object() {
        obj.insert(k, v);
    }
}

fn json_marker_present(doc: &serde_json::Value) -> bool {
    doc.get(MARKER_KEY).is_some()
}

/// `true` when the marker key exists but its value diverges from what this
/// tool would have written (i.e. the user edited it locally).
fn json_marker_modified(doc: &serde_json::Value) -> bool {
    match doc.get(MARKER_KEY) {
        Some(v) => v != &serde_json::Value::String(MARKER_VALUE.to_string()),
        None => false,
    }
}

fn json_remove_marker(doc: &mut serde_json::Value) {
    if let Some(obj) = doc.as_object_mut() {
        obj.remove(MARKER_KEY);
    }
}

// --- TOML merge/diff helpers (toml_edit, format-preserving) -----------

fn toml_merge_marker(doc: &mut toml_edit::DocumentMut) {
    doc[MARKER_KEY] = toml_edit::value(MARKER_VALUE);
}

fn toml_marker_present(doc: &toml_edit::DocumentMut) -> bool {
    doc.get(MARKER_KEY).is_some()
}

fn toml_marker_modified(doc: &toml_edit::DocumentMut) -> bool {
    match doc.get(MARKER_KEY).and_then(|v| v.as_str()) {
        Some(v) => v != MARKER_VALUE,
        None => doc.get(MARKER_KEY).is_some(),
    }
}

fn toml_remove_marker(doc: &mut toml_edit::DocumentMut) {
    doc.remove(MARKER_KEY);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("varde-hooks-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir creates");
        dir
    }

    #[test]
    fn json_merge_creates_file_with_only_injected_entry() {
        let dir = tempdir("json-empty");
        let targets = &[HookTarget {
            agent: "test-json",
            kind: HookKind::MergeInto("settings.json", MergeStrategy::Json),
        }];

        let installed = install_hooks(&dir, targets, false).expect("install succeeds");
        assert_eq!(installed.len(), 1);
        assert!(installed[0].written && !installed[0].skipped_existing);

        let text = fs::read_to_string(dir.join("settings.json")).expect("file exists");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(doc.as_object().expect("object").len(), 1);
        assert_eq!(doc[MARKER_KEY], MARKER_VALUE);
    }

    #[test]
    fn json_merge_preserves_unrelated_keys() {
        let dir = tempdir("json-preserve");
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"unrelated": {"nested": true}, "otherKey": 42}"#)
            .expect("seed writes");

        let targets = &[HookTarget {
            agent: "test-json",
            kind: HookKind::MergeInto("settings.json", MergeStrategy::Json),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");

        let text = fs::read_to_string(&path).expect("file exists");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(doc["unrelated"]["nested"], true);
        assert_eq!(doc["otherKey"], 42);
        assert_eq!(doc[MARKER_KEY], MARKER_VALUE);
    }

    #[test]
    fn toml_merge_creates_file_with_only_injected_entry() {
        let dir = tempdir("toml-empty");
        let targets = &[HookTarget {
            agent: "test-toml",
            kind: HookKind::MergeInto("config.toml", MergeStrategy::Toml),
        }];

        install_hooks(&dir, targets, false).expect("install succeeds");

        let text = fs::read_to_string(dir.join("config.toml")).expect("file exists");
        let doc = text.parse::<toml_edit::DocumentMut>().expect("valid toml");
        assert_eq!(doc[MARKER_KEY].as_str(), Some(MARKER_VALUE));
    }

    #[test]
    fn toml_merge_preserves_unrelated_content_and_comments() {
        let dir = tempdir("toml-preserve");
        let path = dir.join("config.toml");
        let seed = "# a user comment\nunrelated_key = \"keep me\"\n\n[some_table]\nfoo = 1\n";
        fs::write(&path, seed).expect("seed writes");

        let targets = &[HookTarget {
            agent: "test-toml",
            kind: HookKind::MergeInto("config.toml", MergeStrategy::Toml),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");

        let text = fs::read_to_string(&path).expect("file exists");
        assert!(
            text.contains("# a user comment"),
            "comment preserved:\n{text}"
        );
        assert!(
            text.contains("unrelated_key = \"keep me\""),
            "unrelated key preserved:\n{text}"
        );
        assert!(text.contains("[some_table]"), "table preserved:\n{text}");
        assert!(text.contains("foo = 1"), "table content preserved:\n{text}");

        let doc = text.parse::<toml_edit::DocumentMut>().expect("valid toml");
        assert_eq!(doc[MARKER_KEY].as_str(), Some(MARKER_VALUE));
    }

    #[test]
    fn merge_install_is_idempotent_without_force() {
        let dir = tempdir("json-idempotent");
        let targets = &[HookTarget {
            agent: "test-json",
            kind: HookKind::MergeInto("settings.json", MergeStrategy::Json),
        }];
        install_hooks(&dir, targets, false).expect("first install succeeds");
        let second = install_hooks(&dir, targets, false).expect("second install succeeds");
        assert!(!second[0].written && second[0].skipped_existing);
    }

    #[test]
    fn json_remove_scoped_diff_skips_locally_modified_entry_unless_forced() {
        let dir = tempdir("json-remove-modified");
        let path = dir.join("settings.json");
        let targets = &[HookTarget {
            agent: "test-json",
            kind: HookKind::MergeInto("settings.json", MergeStrategy::Json),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");

        // Simulate the user locally editing the injected marker value, while
        // also adding unrelated content elsewhere in the same file.
        fs::write(
            &path,
            format!(r#"{{"{MARKER_KEY}": "user-changed", "unrelatedKey": "user-added"}}"#),
        )
        .expect("edit writes");

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert_eq!(removed.len(), 1);
        assert!(!removed[0].removed && removed[0].skipped_modified);

        // The unrelated edit made alongside the modified marker must never
        // be touched or flagged by the scoped diff.
        let text = fs::read_to_string(&path).expect("file still exists");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(doc["unrelatedKey"], "user-added");
        assert_eq!(doc[MARKER_KEY], "user-changed");

        let forced = remove_hooks(&dir, targets, true).expect("forced remove succeeds");
        assert!(forced[0].removed && !forced[0].skipped_modified);
        let text = fs::read_to_string(&path).expect("file still exists");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert!(doc.get(MARKER_KEY).is_none());
        assert_eq!(
            doc["unrelatedKey"], "user-added",
            "unrelated content untouched by forced remove"
        );
    }

    #[test]
    fn json_remove_untouched_when_marker_unmodified() {
        let dir = tempdir("json-remove-clean");
        let path = dir.join("settings.json");
        let targets = &[HookTarget {
            agent: "test-json",
            kind: HookKind::MergeInto("settings.json", MergeStrategy::Json),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert!(removed[0].removed && !removed[0].skipped_modified);
        let text = fs::read_to_string(&path).expect("file still exists");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert!(doc.get(MARKER_KEY).is_none());
    }

    #[test]
    fn toml_remove_scoped_diff_skips_locally_modified_entry_unless_forced() {
        let dir = tempdir("toml-remove-modified");
        let path = dir.join("config.toml");
        let targets = &[HookTarget {
            agent: "test-toml",
            kind: HookKind::MergeInto("config.toml", MergeStrategy::Toml),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");

        let seeded = fs::read_to_string(&path).expect("file exists");
        let with_local_edit = seeded.replace(
            &format!("{MARKER_KEY} = \"{MARKER_VALUE}\""),
            &format!("{MARKER_KEY} = \"user-changed\""),
        );
        let with_unrelated = format!("{with_local_edit}\nunrelated_key = \"kept\"\n");
        fs::write(&path, &with_unrelated).expect("edit writes");

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert!(!removed[0].removed && removed[0].skipped_modified);

        let text = fs::read_to_string(&path).expect("file still exists");
        assert!(text.contains("unrelated_key = \"kept\""));
        assert!(text.contains("user-changed"));

        let forced = remove_hooks(&dir, targets, true).expect("forced remove succeeds");
        assert!(forced[0].removed && !forced[0].skipped_modified);
        let text = fs::read_to_string(&path).expect("file still exists");
        assert!(!text.contains(MARKER_KEY));
        assert!(
            text.contains("unrelated_key = \"kept\""),
            "unrelated content untouched by forced remove"
        );
    }

    #[test]
    fn write_file_kind_round_trips_like_skills() {
        let dir = tempdir("writefile-roundtrip");
        let targets = &[HookTarget {
            agent: "test-writefile",
            kind: HookKind::WriteFile("plugin.js", "// injected plugin\n"),
        }];

        let installed = install_hooks(&dir, targets, false).expect("install succeeds");
        assert!(installed[0].written && !installed[0].skipped_existing);
        assert_eq!(
            fs::read_to_string(dir.join("plugin.js")).unwrap(),
            "// injected plugin\n"
        );

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert!(removed[0].removed && !removed[0].skipped_modified);
        assert!(!dir.join("plugin.js").exists());
    }

    #[test]
    fn write_file_remove_skips_locally_modified_unless_forced() {
        let dir = tempdir("writefile-modified");
        let path = dir.join("plugin.js");
        let targets = &[HookTarget {
            agent: "test-writefile",
            kind: HookKind::WriteFile("plugin.js", "// injected plugin\n"),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");
        fs::write(&path, "// user customized\n").expect("edit writes");

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert!(!removed[0].removed && removed[0].skipped_modified);
        assert!(path.exists());

        let forced = remove_hooks(&dir, targets, true).expect("forced remove succeeds");
        assert!(forced[0].removed && !forced[0].skipped_modified);
        assert!(!path.exists());
    }

    #[test]
    fn claude_install_creates_settings_with_session_start_hook() {
        let dir = tempdir("claude-install-empty");
        let installed = install_hooks(&dir, HOOK_TARGETS, false).expect("install succeeds");
        assert_eq!(installed.len(), HOOK_TARGETS.len());
        assert!(installed[0].written && !installed[0].skipped_existing);

        let text = fs::read_to_string(dir.join("settings.json")).expect("file exists");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        let command = doc["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .expect("command string");
        assert!(
            command.contains("varde-code nav_map"),
            "command invokes nav_map: {command}"
        );
        assert_eq!(
            doc["hooks"]["SessionStart"][0]["hooks"][0][MARKER_KEY],
            MARKER_VALUE
        );
    }

    #[test]
    fn claude_install_preserves_unrelated_keys() {
        let dir = tempdir("claude-install-preserve");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{"permissions": {"allow": ["Bash"]}, "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": []}]}}"#,
        )
        .expect("seed writes");

        install_hooks(&dir, HOOK_TARGETS, false).expect("install succeeds");

        let text = fs::read_to_string(&path).expect("file exists");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(doc["permissions"]["allow"][0], "Bash");
        assert_eq!(doc["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        let command = doc["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .expect("command string");
        assert!(command.contains("varde-code nav_map"));
    }

    #[test]
    fn claude_remove_leaves_unrelated_content_untouched() {
        let dir = tempdir("claude-remove");
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"permissions": {"allow": ["Bash"]}}"#).expect("seed writes");

        install_hooks(&dir, HOOK_TARGETS, false).expect("install succeeds");
        let removed = remove_hooks(&dir, HOOK_TARGETS, false).expect("remove succeeds");
        assert!(removed[0].removed && !removed[0].skipped_modified);

        let text = fs::read_to_string(&path).expect("file still exists");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(doc["permissions"]["allow"][0], "Bash");
        assert!(
            doc.get("hooks").is_none(),
            "SessionStart-only hooks object pruned: {doc}"
        );
    }

    #[test]
    fn claude_install_skips_locally_edited_entry_unless_forced() {
        let dir = tempdir("claude-install-skip-edited");
        let path = dir.join("settings.json");
        install_hooks(&dir, HOOK_TARGETS, false).expect("install succeeds");

        let text = fs::read_to_string(&path).expect("file exists");
        let mut doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        doc["hooks"]["SessionStart"][0]["hooks"][0]["command"] =
            serde_json::Value::String("echo user-customized".to_string());
        fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).expect("edit writes");

        let installed = install_hooks(&dir, HOOK_TARGETS, false).expect("install succeeds");
        assert!(
            !installed[0].written && installed[0].skipped_existing,
            "local edit survives without --force"
        );

        let text = fs::read_to_string(&path).expect("file still exists");
        assert!(
            text.contains("echo user-customized"),
            "local edit preserved:\n{text}"
        );
    }

    #[test]
    fn codex_install_creates_config_toml_with_session_start_hook() {
        let dir = tempdir("codex-install-empty");
        let targets = &[HookTarget {
            agent: CODEX_AGENT,
            kind: HookKind::MergeInto("config.toml", MergeStrategy::Toml),
        }];
        let installed = install_hooks(&dir, targets, false).expect("install succeeds");
        assert_eq!(installed.len(), 1);
        assert!(installed[0].written && !installed[0].skipped_existing);

        let text = fs::read_to_string(dir.join("config.toml")).expect("file exists");
        let doc = text.parse::<toml_edit::DocumentMut>().expect("valid toml");
        let command = doc["hooks"]["session_start"]["command"]
            .as_str()
            .expect("command string");
        assert!(
            command.contains("varde-code nav_map"),
            "command invokes nav_map: {command}"
        );
        assert_eq!(
            doc["hooks"]["session_start"][MARKER_KEY].as_str(),
            Some(MARKER_VALUE)
        );
    }

    #[test]
    fn codex_install_preserves_unrelated_content_and_comments() {
        let dir = tempdir("codex-install-preserve");
        let path = dir.join("config.toml");
        let seed = "# a user comment\nmodel = \"gpt-5\"\n\n[some_table]\nfoo = 1\n";
        fs::write(&path, seed).expect("seed writes");

        let targets = &[HookTarget {
            agent: CODEX_AGENT,
            kind: HookKind::MergeInto("config.toml", MergeStrategy::Toml),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");

        let text = fs::read_to_string(&path).expect("file exists");
        assert!(
            text.contains("# a user comment"),
            "comment preserved:\n{text}"
        );
        assert!(
            text.contains("model = \"gpt-5\""),
            "unrelated key preserved:\n{text}"
        );
        assert!(text.contains("[some_table]"), "table preserved:\n{text}");
        assert!(text.contains("foo = 1"), "table content preserved:\n{text}");

        let doc = text.parse::<toml_edit::DocumentMut>().expect("valid toml");
        let command = doc["hooks"]["session_start"]["command"]
            .as_str()
            .expect("command string");
        assert!(command.contains("varde-code nav_map"));
    }

    #[test]
    fn codex_remove_leaves_unrelated_content_untouched() {
        let dir = tempdir("codex-remove");
        let path = dir.join("config.toml");
        let seed = "# keep this comment\nmodel = \"gpt-5\"\n";
        fs::write(&path, seed).expect("seed writes");

        let targets = &[HookTarget {
            agent: CODEX_AGENT,
            kind: HookKind::MergeInto("config.toml", MergeStrategy::Toml),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");
        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert!(removed[0].removed && !removed[0].skipped_modified);

        let text = fs::read_to_string(&path).expect("file still exists");
        assert!(
            text.contains("# keep this comment"),
            "comment untouched:\n{text}"
        );
        assert!(
            text.contains("model = \"gpt-5\""),
            "unrelated key untouched:\n{text}"
        );
        let doc = text.parse::<toml_edit::DocumentMut>().expect("valid toml");
        assert!(
            doc.get("hooks").is_none(),
            "session_start-only hooks table pruned: {doc}"
        );
    }

    #[test]
    fn codex_install_skips_locally_edited_entry_unless_forced() {
        let dir = tempdir("codex-install-skip-edited");
        let path = dir.join("config.toml");
        let targets = &[HookTarget {
            agent: CODEX_AGENT,
            kind: HookKind::MergeInto("config.toml", MergeStrategy::Toml),
        }];
        install_hooks(&dir, targets, false).expect("install succeeds");

        let text = fs::read_to_string(&path).expect("file exists");
        let edited = text.replacen(CODEX_SESSION_START_COMMAND, "echo user-customized", 1);
        fs::write(&path, edited).expect("edit writes");

        let installed = install_hooks(&dir, targets, false).expect("install succeeds");
        assert!(
            !installed[0].written && installed[0].skipped_existing,
            "local edit survives without --force"
        );

        let text = fs::read_to_string(&path).expect("file still exists");
        assert!(
            text.contains("echo user-customized"),
            "local edit preserved:\n{text}"
        );

        let forced = install_hooks(&dir, targets, true).expect("forced install succeeds");
        assert!(forced[0].written && !forced[0].skipped_existing);
        let text = fs::read_to_string(&path).expect("file still exists");
        assert!(
            text.contains(CODEX_SESSION_START_COMMAND),
            "forced install restores canonical command:\n{text}"
        );
    }

    fn opencode_target() -> &'static [HookTarget] {
        const TARGETS: &[HookTarget] = &[HookTarget {
            agent: OPENCODE_AGENT,
            kind: HookKind::WriteFile("plugin/varde-code-nav-map.js", OPENCODE_PLUGIN_JS),
        }];
        TARGETS
    }

    #[test]
    fn opencode_install_writes_plugin_js_when_absent() {
        let dir = tempdir("opencode-install");
        let targets = opencode_target();

        let installed = install_hooks(&dir, targets, false).expect("install succeeds");
        assert_eq!(installed.len(), 1);
        assert!(installed[0].written && !installed[0].skipped_existing);

        let path = dir.join("plugin/varde-code-nav-map.js");
        assert!(path.exists());
        let text = fs::read_to_string(&path).expect("file exists");
        assert_eq!(text, OPENCODE_PLUGIN_JS);
        assert!(
            text.contains("varde-code nav_map"),
            "plugin shells out to nav_map: {text}"
        );
    }

    #[test]
    fn opencode_remove_after_unmodified_install_deletes_file() {
        let dir = tempdir("opencode-remove-clean");
        let targets = opencode_target();
        install_hooks(&dir, targets, false).expect("install succeeds");

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert_eq!(removed.len(), 1);
        assert!(removed[0].removed && !removed[0].skipped_modified);
        assert!(!dir.join("plugin/varde-code-nav-map.js").exists());
    }

    #[test]
    fn opencode_remove_skips_locally_modified_file_unless_forced() {
        let dir = tempdir("opencode-remove-modified");
        let targets = opencode_target();
        install_hooks(&dir, targets, false).expect("install succeeds");

        let path = dir.join("plugin/varde-code-nav-map.js");
        fs::write(&path, "// user customized plugin\n").expect("edit writes");

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert!(
            !removed[0].removed && removed[0].skipped_modified,
            "modified file skipped without --force"
        );
        assert!(path.exists());

        let forced = remove_hooks(&dir, targets, true).expect("forced remove succeeds");
        assert!(forced[0].removed && !forced[0].skipped_modified);
        assert!(!path.exists());
    }

    #[test]
    fn opencode_plugin_js_is_syntactically_valid() {
        let dir = tempdir("opencode-js-syntax");
        let path = dir.join("varde-code-nav-map.js");
        fs::write(&path, OPENCODE_PLUGIN_JS).expect("write plugin js");

        match std::process::Command::new("node")
            .arg("--check")
            .arg(&path)
            .output()
        {
            Ok(output) => {
                assert!(
                    output.status.success(),
                    "node --check failed:\nstdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                eprintln!("node --check passed for opencode plugin JS");
            }
            Err(err) => {
                eprintln!("node not found on PATH, skipping JS syntax check: {err}");
            }
        }
    }

    fn pi_target() -> &'static [HookTarget] {
        const TARGETS: &[HookTarget] = &[HookTarget {
            agent: PI_AGENT,
            kind: HookKind::WriteFile("pi-extension.js", PI_EXTENSION_JS),
        }];
        TARGETS
    }

    #[test]
    fn pi_install_writes_extension_js_when_absent() {
        let dir = tempdir("pi-install");
        let targets = pi_target();

        let installed = install_hooks(&dir, targets, false).expect("install succeeds");
        assert_eq!(installed.len(), 1);
        assert!(installed[0].written && !installed[0].skipped_existing);

        let path = dir.join("pi-extension.js");
        assert!(path.exists());
        let text = fs::read_to_string(&path).expect("file exists");
        assert_eq!(text, PI_EXTENSION_JS);
        assert!(
            text.contains("session_start"),
            "extension registers session_start handler: {text}"
        );
    }

    #[test]
    fn pi_remove_after_unmodified_install_deletes_file() {
        let dir = tempdir("pi-remove-clean");
        let targets = pi_target();
        install_hooks(&dir, targets, false).expect("install succeeds");

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert_eq!(removed.len(), 1);
        assert!(removed[0].removed && !removed[0].skipped_modified);
        assert!(!dir.join("pi-extension.js").exists());
    }

    #[test]
    fn pi_remove_skips_locally_modified_file_unless_forced() {
        let dir = tempdir("pi-remove-modified");
        let targets = pi_target();
        install_hooks(&dir, targets, false).expect("install succeeds");

        let path = dir.join("pi-extension.js");
        fs::write(&path, "// user customized extension\n").expect("edit writes");

        let removed = remove_hooks(&dir, targets, false).expect("remove succeeds");
        assert!(
            !removed[0].removed && removed[0].skipped_modified,
            "modified file skipped without --force"
        );
        assert!(path.exists());

        let forced = remove_hooks(&dir, targets, true).expect("forced remove succeeds");
        assert!(forced[0].removed && !forced[0].skipped_modified);
        assert!(!path.exists());
    }

    #[test]
    fn pi_extension_js_is_syntactically_valid() {
        let dir = tempdir("pi-js-syntax");
        let path = dir.join("pi-extension.js");
        fs::write(&path, PI_EXTENSION_JS).expect("write extension js");

        let node_bin = if std::path::Path::new("/opt/homebrew/bin/node").exists() {
            "/opt/homebrew/bin/node"
        } else {
            "node"
        };

        match std::process::Command::new(node_bin)
            .arg("--check")
            .arg(&path)
            .output()
        {
            Ok(output) => {
                assert!(
                    output.status.success(),
                    "node --check failed:\nstdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                eprintln!("node --check passed for pi extension JS");
            }
            Err(err) => {
                eprintln!("node not found, skipping JS syntax check: {err}");
            }
        }
    }
}
