//! `scan` orchestration: tie the rule-pack loader and both rule engines
//! together into one read-and-report flow.
//!
//! Flow: open the persisted DB read-only (error if absent/unopenable) →
//! staleness check against the working tree (error if stale) →
//! `load_rules(repo_root)` → dispatch `kind=pattern` rules over the source
//! tree and `kind=sql` rules against the connection → merge findings and
//! diagnostics from all three sources into `{ findings, diagnostics }`.
//!
//! Named `scan_cli` (distinct from `scan`, the low-level file-listing
//! module this flow reuses for the pattern-rule file set).

use crate::query::ApiError;
use crate::rules::Severity;
use crate::rules::rewrite::{RewriteStatus, RewriteTarget};
use std::collections::HashMap;
use std::path::Path;

/// Required `repoRoot` from the input, validated to be an existing directory.
/// Shared by the rule-pack management endpoints (`rules_list`/`rules_seed`/
/// `rules_remove`), which all take the same `{ repoRoot }` shape before doing
/// their own thing with it.
fn require_repo_dir(input: &serde_json::Value) -> Result<&Path, ApiError> {
    let repo_root = crate::query::req_str(input, "repoRoot")?;
    let repo_path = Path::new(repo_root);
    if !repo_path.is_dir() {
        return Err(ApiError::new(
            "invalid_input",
            format!("repoRoot is not a directory: {repo_root}"),
        ));
    }
    Ok(repo_path)
}

/// Resolve `severityThreshold` from the scan input (default `"error"`).
pub fn severity_threshold(input: &serde_json::Value) -> Result<Severity, ApiError> {
    match input.get("severityThreshold").and_then(|v| v.as_str()) {
        None => Ok(Severity::Error),
        Some(name) => Severity::from_name(name).ok_or_else(|| {
            ApiError::new(
                "invalid_input",
                format!("unknown severityThreshold {name:?} (expected error|warning|info)"),
            )
        }),
    }
}

/// Whether any finding's severity meets or exceeds `threshold`.
///
/// Findings carry a lowercase severity name; a missing/unparseable severity
/// is treated as `Info` (the lowest rank), so it can never trip a threshold.
pub fn findings_at_or_above(payload: &serde_json::Value, threshold: Severity) -> bool {
    payload
        .get("findings")
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
        .any(|f| {
            // A missing/unparseable severity is not a finding the threshold
            // can trip on.
            f.get("severity")
                .and_then(|s| s.as_str())
                .and_then(Severity::from_name)
                .is_some_and(|severity| severity >= threshold)
        })
}

/// Whether any *unresolved* finding's severity meets or exceeds `threshold`.
///
/// `--apply` exit-code gate: a finding whose `rewrite_status` is `"applied"`
/// was fixed in place and no longer blocks the CI gate; every other finding
/// (skipped-dirty/overlap/conflict, or any finding with no rewrite status at
/// all — SQL rules, rewrite-less pattern rules, non-apply runs) gates
/// exactly as before. Non-apply runs emit no `rewrite_status`, so this is
/// identical to `findings_at_or_above` for them.
pub fn unresolved_findings_at_or_above(payload: &serde_json::Value, threshold: Severity) -> bool {
    payload
        .get("findings")
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
        .filter(|f| f.get("rewrite_status").and_then(|s| s.as_str()) != Some("applied"))
        .any(|f| {
            f.get("severity")
                .and_then(|s| s.as_str())
                .and_then(Severity::from_name)
                .is_some_and(|severity| severity >= threshold)
        })
}

/// Run the scan flow for `{ repoRoot, ... }` and return the merged
/// `{ findings, diagnostics }` payload.
///
/// Tool-level failures — a missing/unopenable database, a stale database,
/// or an unreadable repo root — are whole-flow `ApiError`s (the CLI renders
/// the `{ok:false,error}` envelope and exits non-zero). An empty rule pack
/// is a valid no-op scan: `Ok` with empty arrays.
///
/// `apply: true` (from the `--apply` CLI flag) additionally runs the
/// rewrite machinery over pattern-rule findings that carry a `rewrite`
/// template: per-file git-clean gating (unless `force`), span planning,
/// atomic writes. Findings from rules without a `rewrite` template are
/// never written and never git-gated.
pub fn scan_repo(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let repo_root = crate::query::req_str(input, "repoRoot")?;
    let repo_path = Path::new(repo_root);
    if !repo_path.is_dir() {
        return Err(ApiError::new(
            "invalid_input",
            format!("repoRoot is not a directory: {repo_root}"),
        ));
    }
    let apply = input
        .get("apply")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let force = input
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 1. Build-on-read (§ Phase 3): scan's SQL rules read the global slice
    //    (communities, clone bands, `community_id`) plus churn, so freshen
    //    those before opening the index. This replaces the old stale/missing
    //    DB hard errors with freshen-then-scan — every tool is build-on-read.
    crate::query::freshen_for_mode("scan", input)?;

    // 2. Read-only connection to the now-fresh persisted DB.
    let db_path = crate::db::path::repo_db_path(repo_path);
    let conn = crate::db::open_read_only(&db_path).map_err(|e| {
        ApiError::new(
            "db_error",
            format!(
                "database unavailable at {}: {e}; run `varde-code build --repo-root {repo_root}` first",
                db_path.display()
            ),
        )
    })?;

    // 3. Load + merge rule packs (never fails the run — malformed rules are
    //    skip-and-reported as diagnostics by the loader itself).
    let (rules, mut diagnostics) = crate::rules::load_rules(repo_path);

    // 4. Dispatch: pattern rules over the source tree, SQL rules against
    //    the open connection. Hard `Err`s from either engine (e.g. the DB
    //    became unusable) propagate as whole-flow errors; per-rule failures
    //    come back as diagnostics.
    let (mut findings, pattern_diags) =
        crate::rules::pattern::run_pattern_rules(&rules, repo_path, &conn)?;
    let (sql_findings, sql_diags) = crate::rules::sql::run_sql_rules(&rules, &conn)?;
    findings.extend(sql_findings);
    diagnostics.extend(pattern_diags);
    diagnostics.extend(sql_diags);

    // 4b. Drop findings covered by an inline per-file or next-line
    //     suppression comment (see rules::suppress for the marker
    //     constants), and collect suppressions that matched nothing
    //     (stale — likely safe to delete).
    let (findings, stale_suppressions) =
        crate::rules::suppress::filter_findings(findings, repo_path);

    // 5. Optional rewrite application: only with `apply: true`, and only
    //    for pattern-rule findings whose rule carries a `rewrite` template.
    //    File contents outside matched spans stay byte-exact; writes are
    //    atomic (temp file + rename). Each rewrite-bearing finding carries
    //    its outcome in `rewrite_status`; a top-level `rewrite_summary`
    //    counts findings per status.
    let statuses = if apply {
        apply_rewrites(&rules, &findings, repo_path, force)?
    } else {
        HashMap::new()
    };

    // (ARCHITECTURE-002) The id-keyed status wiring below is safe because
    // of three invariants, which a future change must re-check:
    //   1. finding ids are content-addressed (FNV-1a over
    //      `rule_id \0 file \0 start \0 end` — see rules/finding.rs), so
    //      they are unique per rule+file+span within one run;
    //   2. `statuses` keys are exactly the ids of rewrite-bearing findings
    //      (apply_rewrites inserts a status for every finding whose rule
    //      carries a `rewrite` template, and nothing else);
    //   3. duplicate rule ids are deduped at load with diagnostics, so a
    //      rule id maps to one template.
    // The debug_assert below makes #2 observable in debug builds.
    let rewrite_rule_ids: std::collections::HashSet<&str> = rules
        .iter()
        .filter(|r| r.kind == crate::rules::RuleKind::Pattern)
        .filter_map(|r| r.rewrite.as_deref().map(|_| r.id.as_str()))
        .collect();
    let rewrite_bearing_count = findings
        .iter()
        .filter(|f| rewrite_rule_ids.contains(f.rule_id.as_str()))
        .count();

    let mut payload = serde_json::json!({ "findings": findings, "diagnostics": diagnostics });
    if !stale_suppressions.is_empty() {
        payload["stale_suppressions"] = serde_json::json!(stale_suppressions);
    }
    if apply {
        debug_assert_eq!(
            statuses.len(),
            rewrite_bearing_count,
            "every rewrite-bearing finding must receive a rewrite_status"
        );
        let mut summary = serde_json::Map::new();
        for (status, count) in count_statuses(&statuses) {
            summary.insert(status.to_string(), serde_json::json!(count));
        }
        for finding in payload["findings"].as_array_mut().expect("findings array") {
            if let Some(status) = finding["id"].as_str().and_then(|id| statuses.get(id)) {
                finding["rewrite_status"] = serde_json::json!(status);
            }
        }
        if !summary.is_empty() {
            payload["rewrite_summary"] = serde_json::Value::Object(summary);
        }
    }

    Ok(payload)
}

/// Count findings per `RewriteStatus` value, emitting kebab-case status
/// names (e.g. `"skipped-dirty"`) — the same names the per-finding field
/// serializes to. Iteration order is deterministic by construction of the
/// status enum's `Serialize` mapping (declaration order).
fn count_statuses(statuses: &HashMap<String, RewriteStatus>) -> Vec<(&'static str, usize)> {
    use crate::rules::rewrite::RewriteStatus::*;
    let mut counts: Vec<(&str, usize)> = vec![
        ("applied", 0),
        ("skipped-dirty", 0),
        ("skipped-overlap", 0),
        ("skipped-conflict", 0),
    ];
    for status in statuses.values() {
        let key = match status {
            Applied => 0usize,
            SkippedDirty => 1,
            SkippedOverlap => 2,
            SkippedConflict => 3,
        };
        counts[key].1 += 1;
    }
    counts.into_iter().filter(|(_, n)| *n > 0).collect()
}

/// Apply every `rewrite`-bearing finding's substitution to disk, gated per
/// file by git status, and return each finding's outcome.
///
/// Returns a map from finding id to its `RewriteStatus` — only findings
/// whose rule carries a `rewrite` template are present. Callers (the JSON
/// envelope / summary in `apply-output-envelope-and-exit-code`) attach the
/// status to the finding and count statuses from this map.
///
/// Per file, in order: git-clean check (dirty → every finding in the file
/// is `skipped-dirty`, unless `force`; one batched git probe serves the
/// whole run — CODE-003), then span planning (`plan_file_rewrites`: sort,
/// skip overlaps), then a single atomic write splicing every applied
/// replacement into the file content.
///
/// Findings are grouped by the **physical** file that will be written — the
/// canonical target, so a symlinked entry and its real target (both walked,
/// both matching) plan as one file and produce a single write
/// (CORRECTNESS-004): the link survives and the real file is rewritten.
pub(crate) fn apply_rewrites(
    rules: &[crate::rules::Rule],
    findings: &[crate::rules::finding::Finding],
    repo_root: &Path,
    force: bool,
) -> Result<HashMap<String, RewriteStatus>, ApiError> {
    let rewrite_by_rule: HashMap<&str, &str> = rules
        .iter()
        .filter(|r| r.kind == crate::rules::RuleKind::Pattern)
        .filter_map(|r| {
            r.rewrite
                .as_deref()
                .map(|template| (r.id.as_str(), template))
        })
        .collect();
    if rewrite_by_rule.is_empty() {
        return Ok(HashMap::new());
    }

    // Group each rewrite-bearing finding's span+replacement by file.
    struct FileTarget<'a> {
        finding: &'a crate::rules::finding::Finding,
        target: RewriteTarget,
    }
    let mut by_file: HashMap<std::path::PathBuf, Vec<FileTarget<'_>>> = HashMap::new();
    for finding in findings {
        let Some(template) = rewrite_by_rule.get(finding.rule_id.as_str()) else {
            continue;
        };
        let span = &finding.location.span;
        let replacement = crate::rules::rewrite::substitute(template, &finding.evidence);
        // Resolve the walk path (possibly relative — the scan walked a
        // relative repoRoot) to the absolute physical file, following
        // symlinks: that is what gets read and written (CORRECTNESS-002,
        // CORRECTNESS-004).
        let real = crate::rules::finding::resolve_real_path(&finding.location.file);
        by_file.entry(real).or_default().push(FileTarget {
            finding,
            target: RewriteTarget {
                start_byte: span.start_byte,
                end_byte: span.end_byte,
                replacement,
            },
        });
    }

    // One git probe for the whole run (CODE-003). A canonicalized
    // repo_root keeps `--is-inside-work-tree` correct for relative inputs
    // (CORRECTNESS-002). `force` skips the gate entirely, as before.
    let repo_root_abs = repo_root.canonicalize().unwrap_or_else(|_| {
        std::path::absolute(repo_root).unwrap_or_else(|_| repo_root.to_path_buf())
    });
    let real_paths: Vec<std::path::PathBuf> = by_file.keys().cloned().collect();
    let gate = if force {
        GitGate::NotAGitRepo
    } else {
        git_gate(&repo_root_abs, &real_paths)
    };
    if matches!(gate, GitGate::Unverifiable) {
        tracing::warn!(
            "apply: git status failed; every file treated as dirty and skipped (use --force to override)"
        );
    }

    let mut statuses = HashMap::new();
    for (real_path, targets) in &by_file {
        if gate.dirty(real_path) {
            for t in targets {
                statuses.insert(t.finding.id.clone(), RewriteStatus::SkippedDirty);
            }
            continue;
        }

        // Span planning: sort by span, first-sorted wins, overlaps skipped.
        let rewrites: Vec<RewriteTarget> = targets.iter().map(|t| t.target.clone()).collect();
        let plans = crate::rules::rewrite::plan_file_rewrites(&rewrites);
        let mut applied: Vec<&RewriteTarget> = Vec::new();
        for (t, plan) in targets.iter().zip(&plans) {
            match plan {
                crate::rules::rewrite::RewritePlan::Apply => {
                    applied.push(&t.target);
                    statuses.insert(t.finding.id.clone(), RewriteStatus::Applied);
                }
                crate::rules::rewrite::RewritePlan::SkippedOverlap => {
                    statuses.insert(t.finding.id.clone(), RewriteStatus::SkippedOverlap);
                }
            }
        }
        if applied.is_empty() {
            continue;
        }

        // Staleness guard (closes the parse-to-write TOCTOU): the applied
        // spans were computed against whatever the file looked like when it
        // was matched, which can be long before this write — the scan's SQL
        // rules and suppression filtering run in between. A read failure or
        // out-of-range/off-char-boundary span is already caught by `splice`
        // below, but a file that changed and happens to still be in-range
        // would otherwise be silently misapplied. Compare against the state
        // recorded at match time; `None` (stat unavailable then) leaves this
        // unverified rather than blocking the write. Not redundant with the
        // `recheck_dirty` git-status check below: this catches a filesystem
        // mtime/content change since match time, that catches a git-stage
        // change since the batched dirty probe — distinct windows, both real.
        if let Some(expected) = targets.iter().find_map(|t| t.finding.matched_file_state)
            && crate::rules::finding::FileState::of(real_path) != Some(expected)
        {
            for t in targets {
                statuses.insert(t.finding.id.clone(), RewriteStatus::SkippedConflict);
            }
            tracing::warn!(
                file = %real_path.display(),
                "apply: file changed since it was matched; skipped to avoid misapplying stale offsets"
            );
            continue;
        }

        // Read, splice, atomic-write against the canonical physical file
        // (the same path the git gate keyed on). A read failure or a span
        // that is not on a UTF-8 char boundary is a per-file conflict —
        // reported, not a crash (byte-exact preservation elsewhere is the
        // priority).
        let content = match std::fs::read_to_string(real_path) {
            Ok(content) => content,
            Err(e) => {
                for t in targets {
                    statuses.insert(t.finding.id.clone(), RewriteStatus::SkippedConflict);
                }
                tracing::warn!(file = %real_path.display(), "apply: unreadable file skipped: {e}");
                continue;
            }
        };
        applied.sort_by_key(|t| (t.start_byte, t.end_byte));
        let rewritten = match splice(&content, &applied) {
            Some(rewritten) => rewritten,
            None => {
                for t in targets {
                    statuses.insert(t.finding.id.clone(), RewriteStatus::SkippedConflict);
                }
                tracing::warn!(file = %real_path.display(), "apply: span not on a UTF-8 boundary; file skipped");
                continue;
            }
        };
        // Re-verify git-clean immediately before writing (closes the
        // gate-to-write TOCTOU): the batched probe above ran once for the
        // whole run, so a file that became dirty afterward — but before its
        // own turn in this loop — would otherwise still be overwritten.
        if gate.recheck_dirty(real_path) {
            for t in targets {
                statuses.insert(t.finding.id.clone(), RewriteStatus::SkippedDirty);
            }
            tracing::warn!(file = %real_path.display(), "apply: file became dirty since the initial git check; skipped");
            continue;
        }

        if let Err(e) = write_atomic(real_path, &rewritten) {
            for t in targets {
                statuses.insert(t.finding.id.clone(), RewriteStatus::SkippedConflict);
            }
            tracing::warn!(file = %real_path.display(), "apply: write failed, file skipped: {e}");
        }
    }
    Ok(statuses)
}

/// Outcome of the batched git gate for one `--apply` run.
///
/// One git probe + one status call serve the whole run (CODE-003) instead
/// of a subprocess per rewritten file.
enum GitGate {
    /// git absent, or `repo_root` not inside a work tree → every file clean
    /// (no git state to be undone against — documented scope, CORRECTNESS-005).
    NotAGitRepo,
    /// A work tree exists but the status query failed (corrupt index,
    /// unreadable `.git`, unexpected output shape) → every file dirty.
    /// Failure-closed: an ungated write is worse than a skipped one
    /// (CORRECTNESS-005).
    Unverifiable,
    /// A work tree exists and status ran: its root (for a later per-file
    /// recheck) plus the set of dirty absolute paths.
    WorkTree(
        std::path::PathBuf,
        std::collections::HashSet<std::path::PathBuf>,
    ),
}

impl GitGate {
    /// Whether `real_path` (a canonical, absolute physical file path) is
    /// dirty under this gate.
    fn dirty(&self, real_path: &std::path::Path) -> bool {
        match self {
            GitGate::NotAGitRepo => false,
            GitGate::Unverifiable => true,
            GitGate::WorkTree(_, set) => set.contains(real_path),
        }
    }

    /// Re-probe git for exactly one file, right before it is written.
    ///
    /// The batched `dirty()` check runs once at the top of `apply_rewrites`
    /// (CODE-003); a file that becomes dirty *after* that probe but before
    /// its own write is otherwise overwritten regardless (an editor save, a
    /// concurrent process, `git checkout` racing the apply run). This is a
    /// second, single-file `git status` call scoped to files that are
    /// actually about to be written — one subprocess per output, not per
    /// input, so it doesn't reintroduce the per-candidate-file cost CODE-003
    /// removed.
    ///
    /// A status failure here fails closed (treated as dirty): skipping a
    /// write is always safer than risking one against a file we can no
    /// longer verify. `NotAGitRepo` has nothing to re-check against, so it
    /// stays clean, matching `dirty()`'s rationale.
    fn recheck_dirty(&self, real_path: &std::path::Path) -> bool {
        match self {
            GitGate::NotAGitRepo => false,
            GitGate::Unverifiable => true,
            GitGate::WorkTree(worktree, _) => {
                if !real_path.starts_with(worktree) {
                    return false;
                }
                match status_dirty_paths(worktree, &[real_path]) {
                    Some(dirty) => dirty.contains(real_path),
                    None => true,
                }
            }
        }
    }
}

/// Probe git once per apply run and classify the rewrite targets.
///
/// `repo_root` and `targets` are canonicalized here (resolving `..` and
/// symlink aliases such as macOS's `/var` → `/private/var`), so callers may
/// pass the paths they were given (CORRECTNESS-002); `targets` are the
/// physical files to be written, symlinks already resolved
/// (CORRECTNESS-004), and dirty matches are keyed by the same canonical
/// form (porcelain paths are canonicalized identically).
///
/// Sequence: `rev-parse --is-inside-work-tree` distinguishes "not a repo / no
/// git" (clean, documented) from "a work tree exists"; with a work tree
/// confirmed, a single batched `git status --porcelain=v1 -z -uall
/// --ignored` over the in-work-tree targets decides dirtiness. `--ignored`
/// closes CORRECTNESS-003 (an ignored untracked file is still untracked →
/// dirty). Any status failure after the work-tree probe is a genuine git
/// error → `Unverifiable` (failure-closed, CORRECTNESS-005). Targets whose
/// canonical path lies outside the work tree (symlink targets elsewhere)
/// cannot be gated — clean, same rationale as not-a-repo.
fn git_gate(repo_root: &std::path::Path, targets: &[std::path::PathBuf]) -> GitGate {
    let repo_root = repo_root.canonicalize().unwrap_or_else(|_| {
        std::path::absolute(repo_root).unwrap_or_else(|_| repo_root.to_path_buf())
    });
    let targets: Vec<std::path::PathBuf> = targets
        .iter()
        .map(|t| t.canonicalize().unwrap_or_else(|_| t.clone()))
        .collect();
    let Some(inside) = crate::git::run_git(&["rev-parse", "--is-inside-work-tree"], &repo_root)
    else {
        return GitGate::NotAGitRepo;
    };
    if inside.trim() != "true" {
        return GitGate::NotAGitRepo;
    }
    // Work tree confirmed. Resolve its root: porcelain paths are printed
    // relative to the work tree, so matching against absolute targets needs
    // it (repo_root may be a subdir of the work tree).
    let Some(top) = crate::git::run_git(&["rev-parse", "--show-toplevel"], &repo_root) else {
        return GitGate::Unverifiable;
    };
    let worktree = std::path::PathBuf::from(top.trim());

    let inside_targets: Vec<&std::path::Path> = targets
        .iter()
        .filter(|p| p.starts_with(&worktree))
        .map(|p| p.as_path())
        .collect();
    if inside_targets.is_empty() {
        return GitGate::WorkTree(worktree, std::collections::HashSet::new());
    }
    match status_dirty_paths(&worktree, &inside_targets) {
        Some(dirty) => GitGate::WorkTree(worktree, dirty),
        None => GitGate::Unverifiable,
    }
}

/// Run the batched status query (chunked so the command line stays well
/// under ARG_MAX for repo-wide codemods) and collect dirty absolute paths.
///
/// `None` when any status call fails or emits an unexpected record shape —
/// the caller then fails closed (every file dirty).
fn status_dirty_paths(
    worktree: &std::path::Path,
    targets: &[&std::path::Path],
) -> Option<std::collections::HashSet<std::path::PathBuf>> {
    // Conservative chunk size: ~1000 absolute paths stays far below the
    // macOS/Linux exec arg ceiling while keeping this O(chunks), not
    // O(files), subprocesses.
    const CHUNK: usize = 1000;
    let mut dirty = std::collections::HashSet::new();
    for chunk in targets.chunks(CHUNK) {
        let paths: Vec<String> = chunk
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut args: Vec<&str> = vec![
            "-c",
            "status.renames=false",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored",
            "-z",
            "--",
        ];
        args.extend(paths.iter().map(|s| s.as_str()));
        let status = crate::git::run_git(&args, worktree)?;
        let paths = parse_porcelain_dirty_paths(&status)?;
        for p in paths {
            let joined = worktree.join(p);
            // Canonicalize so a dirty symlink entry matches its canonical
            // (write) target; deleted entries (canonicalize fails) never
            // match an existing target anyway.
            let canonical = std::fs::canonicalize(&joined).unwrap_or(joined);
            dirty.insert(canonical);
        }
    }
    Some(dirty)
}

/// Parse `git status --porcelain=v1 -z` output (run with
/// `status.renames=false`, so every record is the single-field `XY path`
/// shape — never the two-record rename form) into dirty paths, relative to
/// the work-tree root the call ran in.
///
/// A `!` status letter (an ignored entry, listed because of `--ignored`) is
/// dirty like any other change: an untracked file is dirty regardless of
/// ignore status (CORRECTNESS-003). A record that fails the `XY path` shape
/// is a wire-format surprise → `None` (caller fails closed).
fn parse_porcelain_dirty_paths(status: &str) -> Option<Vec<&str>> {
    let is_status_letter = |b: u8| {
        matches!(
            b,
            b' ' | b'M' | b'T' | b'A' | b'D' | b'R' | b'C' | b'U' | b'?' | b'!'
        )
    };
    let mut paths = Vec::new();
    for entry in status.split('\0') {
        if entry.is_empty() {
            continue;
        }
        let bytes = entry.as_bytes();
        if bytes.len() < 4
            || !is_status_letter(bytes[0])
            || !is_status_letter(bytes[1])
            || bytes[2] != b' '
        {
            return None;
        }
        paths.push(&entry[3..]);
    }
    Some(paths)
}

/// Splice `replacement` over every applied span of `content`, preserving all
/// other bytes exactly. `applied` must be sorted by span and non-overlapping
/// (as produced by `plan_file_rewrites` + sort). Returns `None` when a span
/// is not on a UTF-8 char boundary (tree-sitter spans are node boundaries,
/// so this is defensive).
fn splice(content: &str, applied: &[&RewriteTarget]) -> Option<String> {
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0usize;
    for t in applied {
        let start = t.start_byte as usize;
        let end = t.end_byte as usize;
        if start < cursor
            || end > content.len()
            || !content.is_char_boundary(start)
            || !content.is_char_boundary(end)
        {
            return None;
        }
        out.push_str(&content[cursor..start]);
        out.push_str(&t.replacement);
        cursor = end;
    }
    out.push_str(&content[cursor..]);
    Some(out)
}

/// Write `content` to `path` atomically: write a temp file in the same
/// directory, then rename over the original — no partial writes on crash
/// mid-`--apply`. The temp file is removed if the rename fails.
///
/// The original file's permission bits are copied onto the temp file before
/// the rename (CORRECTNESS-001): an executable script stays executable and
/// a read-only file stays read-only after the rewrite — the swap never
/// silently resets the mode to the umask default. When the original does
/// not exist (a brand-new write) the temp keeps its default mode.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let tmp = dir.join(format!(".{name}.varde-apply-tmp-{}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, content) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Preserve the original's mode on the temp before the swap; a missing
    // original (new file) falls back gracefully to the default mode.
    if let Ok(metadata) = std::fs::metadata(path)
        && let Err(e) = std::fs::set_permissions(&tmp, metadata.permissions())
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// List the rules that would run for `{ repoRoot }` — the same merged,
/// built-in-aware set `scan_repo` loads — without executing a scan or
/// requiring a persisted DB.
///
/// Each entry is tagged `source`:
/// - `"builtin"`: a shipped default, unmodified by user/repo scope;
/// - `"override"`: a user/repo rule reusing a built-in id (the built-in is
///   shadowed, per `load_rules`'s override semantics);
/// - `"custom"`: a user/repo-only rule with no built-in counterpart.
pub fn rules_list(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    let repo_path = require_repo_dir(input)?;

    let (rules, diagnostics) = crate::rules::load_rules(repo_path);
    let builtin_by_id: std::collections::HashMap<String, crate::rules::Rule> =
        crate::rules::builtin_rules()
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();

    let rules_json: Vec<serde_json::Value> = rules
        .iter()
        .map(|rule| {
            let source = match builtin_by_id.get(&rule.id) {
                Some(builtin) if builtin == rule => "builtin",
                Some(_) => "override",
                None => "custom",
            };
            serde_json::json!({
                "id": rule.id,
                "kind": rule.kind,
                "severity": rule.severity,
                "name": rule.name,
                "description": rule.description,
                "message": rule.message,
                "source": source,
            })
        })
        .collect();

    Ok(serde_json::json!({ "rules": rules_json, "diagnostics": diagnostics }))
}

/// Materialize the built-in rule packs as editable TOML files in the repo
/// or user rules dir (`{ repoRoot }`, plus `user`/`force` from the caller).
pub fn rules_seed(
    input: &serde_json::Value,
    user: bool,
    force: bool,
) -> Result<serde_json::Value, ApiError> {
    let repo_path = require_repo_dir(input)?;

    let target_dir = if user {
        crate::rules::user_rules_dir()
    } else {
        repo_path.join(".varde-code").join("rules")
    };

    let results = crate::rules::seed_builtin_rules(&target_dir, force).map_err(|e| {
        ApiError::new(
            "io_error",
            format!("failed to seed rules into {}: {e}", target_dir.display()),
        )
    })?;

    let seeded_json: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "fileName": r.file_name,
                "path": r.path.display().to_string(),
                "written": r.written,
                "skippedExisting": r.skipped_existing,
            })
        })
        .collect();

    Ok(serde_json::json!({ "targetDir": target_dir.display().to_string(), "seeded": seeded_json }))
}

/// Delete previously seeded built-in rule pack files from the repo or user
/// rules dir (`{ repoRoot }`, plus `user`/`force` from the caller).
///
/// Only removes files matching a shipped built-in pack's name; custom rule
/// files in the same directory are left alone. A seeded file that was
/// customized since seeding is skipped unless `force` is true.
pub fn rules_remove(
    input: &serde_json::Value,
    user: bool,
    force: bool,
) -> Result<serde_json::Value, ApiError> {
    let repo_path = require_repo_dir(input)?;

    let target_dir = if user {
        crate::rules::user_rules_dir()
    } else {
        repo_path.join(".varde-code").join("rules")
    };

    let results = crate::rules::unseed_builtin_rules(&target_dir, force).map_err(|e| {
        ApiError::new(
            "io_error",
            format!(
                "failed to remove seeded rules from {}: {e}",
                target_dir.display()
            ),
        )
    })?;

    let removed_json: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "fileName": r.file_name,
                "path": r.path.display().to_string(),
                "removed": r.removed,
                "skippedModified": r.skipped_modified,
            })
        })
        .collect();

    Ok(
        serde_json::json!({ "targetDir": target_dir.display().to_string(), "removed": removed_json }),
    )
}

#[cfg(test)]
mod threshold {
    use super::*;
    use crate::rules::Severity;

    fn payload_with_severities(severities: &[&str]) -> serde_json::Value {
        let findings: Vec<serde_json::Value> = severities
            .iter()
            .map(|s| serde_json::json!({ "severity": s, "id": "x", "rule_id": "r", "message": "m", "location": {} }))
            .collect();
        serde_json::json!({ "findings": findings, "diagnostics": [] })
    }

    #[test]
    fn default_threshold_is_error() {
        let input = serde_json::json!({ "repoRoot": "/tmp/x" });
        assert_eq!(
            severity_threshold(&input).expect("defaults"),
            Severity::Error
        );
        let input = serde_json::json!({ "repoRoot": "/tmp/x", "severityThreshold": "warning" });
        assert_eq!(
            severity_threshold(&input).expect("parses"),
            Severity::Warning
        );
        let input = serde_json::json!({ "repoRoot": "/tmp/x", "severityThreshold": "bogus" });
        assert!(
            severity_threshold(&input).is_err(),
            "unknown level rejected"
        );
    }

    #[test]
    fn findings_at_or_above_compares_against_threshold() {
        // Default threshold error: error trips it, warning/info do not.
        assert!(findings_at_or_above(
            &payload_with_severities(&["error"]),
            Severity::Error
        ));
        assert!(!findings_at_or_above(
            &payload_with_severities(&["warning"]),
            Severity::Error
        ));
        assert!(!findings_at_or_above(
            &payload_with_severities(&["info"]),
            Severity::Error
        ));
        assert!(!findings_at_or_above(
            &payload_with_severities(&[]),
            Severity::Error
        ));
        // Lower threshold: warning trips it.
        assert!(findings_at_or_above(
            &payload_with_severities(&["warning"]),
            Severity::Warning
        ));
        assert!(findings_at_or_above(
            &payload_with_severities(&["info"]),
            Severity::Info
        ));
        assert!(!findings_at_or_above(
            &payload_with_severities(&["info"]),
            Severity::Warning
        ));
        // Missing severity never trips a threshold.
        let no_severity = serde_json::json!({ "findings": [{ "id": "x" }], "diagnostics": [] });
        assert!(!findings_at_or_above(&no_severity, Severity::Info));
    }

    fn payload_with_rewrite_statuses(items: &[(&str, Option<&str>)]) -> serde_json::Value {
        let findings: Vec<serde_json::Value> = items
            .iter()
            .map(|(severity, status)| {
                let mut f = serde_json::json!({ "severity": severity, "id": "x", "rule_id": "r", "message": "m", "location": {} });
                if let Some(status) = status {
                    f["rewrite_status"] = serde_json::json!(status);
                }
                f
            })
            .collect();
        serde_json::json!({ "findings": findings, "diagnostics": [] })
    }

    #[test]
    fn unresolved_gate_ignores_applied_findings_but_not_skipped() {
        let err = Severity::Error;
        // Applied error finding → resolved, gate passes.
        assert!(!unresolved_findings_at_or_above(
            &payload_with_rewrite_statuses(&[("error", Some("applied"))]),
            err
        ));
        // Skipped error finding → unresolved, gate fails.
        for status in [
            Some("skipped-dirty"),
            Some("skipped-overlap"),
            Some("skipped-conflict"),
        ] {
            assert!(
                unresolved_findings_at_or_above(
                    &payload_with_rewrite_statuses(&[("error", status)]),
                    err
                ),
                "{status:?} must remain unresolved"
            );
        }
        // No rewrite_status at all (SQL finding, non-apply run) → gates as before.
        assert!(unresolved_findings_at_or_above(
            &payload_with_rewrite_statuses(&[("error", None)]),
            err
        ));
        assert!(
            !unresolved_findings_at_or_above(
                &payload_with_rewrite_statuses(&[("warning", Some("applied"))]),
                err
            ),
            "applied warning below threshold never trips"
        );
        // Mixed: one applied error + one skipped error → still unresolved.
        assert!(unresolved_findings_at_or_above(
            &payload_with_rewrite_statuses(&[
                ("error", Some("applied")),
                ("error", Some("skipped-dirty"))
            ]),
            err
        ));
    }

    #[test]
    fn unresolved_gate_matches_findings_at_or_above_when_no_statuses() {
        // Non-apply runs have no rewrite_status → both gates agree.
        for sevs in [&["error"][..], &["warning"][..], &[][..]] {
            let payload = payload_with_severities(sevs);
            assert_eq!(
                findings_at_or_above(&payload, Severity::Error),
                unresolved_findings_at_or_above(&payload, Severity::Error)
            );
        }
    }
}

#[cfg(test)]
mod orchestration {
    use super::*;

    const TS_FIXTURE: &str = r#"
async function fetchData(): Promise<void> {
  console.trace("async log");
}
"#;

    const RULE_PACK: &str = r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.trace($MSG)"

[[rule]]
id = "function-count"
kind = "sql"
severity = "error"
message = "function present"
query = "SELECT f.path AS file, e.start_line AS line FROM entities e JOIN files f ON f.id = e.file_id WHERE e.kind = 0"
"#;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("varde-scan-flow-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        dir
    }

    fn write(path: &std::path::Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir creates");
        }
        std::fs::write(path, contents).expect("fixture writes");
    }

    /// Build a fresh index using `home` as the conventional DB root.
    fn with_fresh_db(home: &std::path::Path, repo: &std::path::Path) {
        let _home_override = crate::test_support::HomeOverride::while_locked(home);
        crate::build::run_with_force(repo.to_str().expect("repo is utf8"), true)
            .expect("fresh build succeeds");
    }

    // `with_fresh_db` owns its RAII restoration. These preserve the existing
    // fixture call shape while no longer mutating HOME directly.
    fn capture_home() {}

    fn restore_home(_: ()) {}

    /// Run `git <args>` inside `repo`, asserting success.
    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    /// Init a git repo at `repo` and commit every current file.
    fn git_init_and_commit(repo: &std::path::Path) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "test@example.com"]);
        git(repo, &["config", "user.name", "test"]);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "initial"]);
    }

    /// Rule pack with a pattern rule carrying a `rewrite` template
    /// (`console.trace($MSG)` → `console.info($MSG)`).
    const REWRITE_RULE_PACK: &str = r#"
[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "console call detected"
pattern = "console.trace($MSG)"
rewrite = "console.info($MSG)"
"#;

    const TS_WITH_TRACE: &str = r#"
async function fetchData(): Promise<void> {
  console.trace("async log");
}
"#;

    /// Error-severity rewrite rule — for the exit-code gate test: an applied
    /// error finding is resolved, a skipped one is not.
    const REWRITE_RULE_PACK_ERROR: &str = r#"
[[rule]]
id = "no-console-error"
kind = "pattern"
severity = "error"
message = "console call detected"
pattern = "console.trace($MSG)"
rewrite = "console.info($MSG)"
"#;

    fn setup_apply_repo(tag: &str, home: &std::path::Path, fixture: &str) -> std::path::PathBuf {
        let repo = tempdir(tag);
        write(&repo.join("main.ts"), fixture);
        write(&repo.join(".varde-code/rules/pack.toml"), REWRITE_RULE_PACK);
        let original_home = capture_home();
        with_fresh_db(home, &repo);
        restore_home(original_home);
        repo
    }

    #[test]
    fn scan_without_apply_writes_nothing() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-noapply");
        let repo = setup_apply_repo("repo-noapply", &home, TS_WITH_TRACE);
        git_init_and_commit(&repo);
        let before = std::fs::read_to_string(repo.join("main.ts")).expect("fixture reads");

        let input = serde_json::json!({ "repoRoot": repo.display().to_string() });
        let payload = scan_repo(&input).expect("read-only scan succeeds");
        assert!(!payload["findings"].as_array().expect("array").is_empty());
        // No apply flag → no write; the file is byte-identical.
        assert_eq!(
            std::fs::read_to_string(repo.join("main.ts")).expect("reads"),
            before
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn scan_with_apply_rewrites_clean_git_file_atomically() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-apply-clean");
        let repo = setup_apply_repo("repo-apply-clean", &home, TS_WITH_TRACE);
        git_init_and_commit(&repo);

        let input = serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        });
        let payload = scan_repo(&input).expect("apply scan succeeds");
        let findings = payload["findings"].as_array().expect("findings array");
        assert_eq!(findings.len(), 1, "one trace finding: {findings:?}");
        // Envelope: the rewrite-bearing finding carries rewrite_status=applied.
        assert_eq!(
            findings[0]["rewrite_status"], "applied",
            "per-finding rewrite_status: {findings:?}"
        );
        // Top-level summary counts it.
        assert_eq!(
            payload["rewrite_summary"],
            serde_json::json!({ "applied": 1 })
        );

        let rewritten = std::fs::read_to_string(repo.join("main.ts")).expect("reads");
        assert!(
            rewritten.contains("console.info(\"async log\")"),
            "matched span replaced: {rewritten}"
        );
        assert!(
            !rewritten.contains("console.trace"),
            "no trace remains: {rewritten}"
        );
        // Byte-exact elsewhere: the enclosing function text is untouched.
        assert!(rewritten.contains("async function fetchData(): Promise<void> {"));
        // No temp files left behind (atomic write cleaned up).
        let leftovers: Vec<_> = std::fs::read_dir(&repo)
            .expect("read dir")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("varde-apply-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no tmp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn scan_with_apply_skips_dirty_file_and_applies_clean_sibling() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-apply-dirty");
        let repo = tempdir("repo-apply-dirty");
        write(&repo.join("clean.ts"), TS_WITH_TRACE);
        write(&repo.join("dirty.ts"), TS_WITH_TRACE);
        write(&repo.join(".varde-code/rules/pack.toml"), REWRITE_RULE_PACK);
        let original_home = capture_home();
        with_fresh_db(&home, &repo);
        restore_home(original_home);
        git_init_and_commit(&repo);

        // Make dirty.ts dirty after the commit (uncommitted change).
        write(
            &repo.join("dirty.ts"),
            &format!("{TS_WITH_TRACE}\n// uncommitted\n"),
        );
        let dirty_before = std::fs::read_to_string(repo.join("dirty.ts")).expect("reads");

        let input = serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        });
        let payload = scan_repo(&input).expect("apply scan succeeds");
        let findings = payload["findings"].as_array().expect("findings array");
        // Both files match; the scan runs regardless of git state.
        assert_eq!(findings.len(), 2, "both files found: {findings:?}");
        // Envelope: clean finding applied, dirty finding skipped-dirty.
        let applied = findings
            .iter()
            .find(|f| f["rewrite_status"] == "applied")
            .expect("clean file applied");
        assert!(
            applied["location"]["file"]
                .as_str()
                .unwrap()
                .ends_with("clean.ts")
        );
        let skipped = findings
            .iter()
            .find(|f| f["rewrite_status"] == "skipped-dirty")
            .expect("dirty file skipped");
        assert!(
            skipped["location"]["file"]
                .as_str()
                .unwrap()
                .ends_with("dirty.ts")
        );
        assert_eq!(
            payload["rewrite_summary"],
            serde_json::json!({ "applied": 1, "skipped-dirty": 1 })
        );

        // Clean file was rewritten; dirty file skipped untouched.
        let clean = std::fs::read_to_string(repo.join("clean.ts")).expect("reads");
        assert!(
            clean.contains("console.info(\"async log\")"),
            "clean file applied: {clean}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("dirty.ts")).expect("reads"),
            dirty_before,
            "dirty file not written without --force"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// H1 regression: a finding whose `matched_file_state` disagrees with
    /// the file's current on-disk state (it changed after matching, before
    /// `--apply` gets to it) is skipped as a conflict rather than spliced —
    /// even though its byte offsets still land in range on the new content.
    #[test]
    fn apply_skips_finding_whose_file_changed_since_it_was_matched() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-apply-stale");
        let repo = setup_apply_repo("repo-apply-stale", &home, TS_WITH_TRACE);
        git_init_and_commit(&repo);

        let (rules, _diags) = crate::rules::load_rules(&repo);
        let file_path = repo.join("main.ts");
        let content_before = std::fs::read_to_string(&file_path).expect("reads");
        let trace_at = content_before
            .find("console.trace")
            .expect("fixture has a trace call");
        let span = crate::model::Span {
            start_byte: trace_at as u32,
            end_byte: (trace_at + "console.trace".len()) as u32,
            start_line: 3,
            start_col: 2,
            end_line: 3,
            end_col: 2 + "console.trace".len() as u32,
        };
        // A `matched_file_state` that does not describe the file as it
        // exists on disk right now — standing in for "matched, then the
        // file changed before apply got here" without needing a genuine
        // race. The len deliberately disagrees with the real file.
        let stale_state = crate::rules::finding::FileState {
            mtime: std::fs::metadata(&file_path)
                .expect("stat")
                .modified()
                .expect("mtime"),
            len: content_before.len() as u64 + 1,
        };
        let finding = crate::rules::finding::Finding {
            id: "stale-test".to_string(),
            rule_id: "no-console".to_string(),
            severity: Severity::Warning,
            message: "console call detected".to_string(),
            location: crate::rules::finding::Location {
                file: file_path.display().to_string(),
                span,
            },
            evidence: serde_json::json!({}),
            remediation: None,
            certainty: None,
            agent_instructions: None,
            rewrite_status: None,
            matched_file_state: Some(stale_state),
        };

        let statuses =
            apply_rewrites(&rules, &[finding], &repo, false).expect("apply_rewrites runs");
        assert_eq!(
            statuses.get("stale-test"),
            Some(&RewriteStatus::SkippedConflict)
        );
        // File untouched — the guard fired before any read-splice-write.
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("reads"),
            content_before,
            "file must be untouched when the match-time state is stale"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// H2 regression: `GitGate::recheck_dirty` catches a file that became
    /// dirty after the batched probe `git_gate` ran — the exact window
    /// between the one-shot status call and this file's own write.
    #[test]
    fn recheck_dirty_catches_a_file_dirtied_after_the_batched_probe() {
        let home = tempdir("home-recheck-dirty");
        let repo = tempdir("repo-recheck-dirty");
        write(&repo.join("main.ts"), TS_WITH_TRACE);
        git_init_and_commit(&repo);
        let _ = std::fs::remove_dir_all(&home);

        let real = std::fs::canonicalize(repo.join("main.ts")).expect("canonicalize");
        let gate = git_gate(&repo, std::slice::from_ref(&real));
        assert!(!gate.dirty(&real), "clean at probe time");

        // The file becomes dirty *after* the batched probe — simulating an
        // edit landing in the window between the gate and this file's write.
        write(
            &repo.join("main.ts"),
            &format!("{TS_WITH_TRACE}\n// edited after probe\n"),
        );

        assert!(
            !gate.dirty(&real),
            "the stale batched result alone would miss this"
        );
        assert!(
            gate.recheck_dirty(&real),
            "a fresh per-file check must catch the post-probe edit"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn apply_exit_gate_applied_error_is_resolved_skipped_is_not() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");

        // Applied: error-severity finding fixed in place → gate passes.
        let home = tempdir("home-gate-applied");
        let repo = tempdir("repo-gate-applied");
        write(&repo.join("main.ts"), TS_WITH_TRACE);
        write(
            &repo.join(".varde-code/rules/pack.toml"),
            REWRITE_RULE_PACK_ERROR,
        );
        let original_home = capture_home();
        with_fresh_db(&home, &repo);
        restore_home(original_home);
        git_init_and_commit(&repo);
        let payload = scan_repo(&serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        }))
        .expect("apply scan succeeds");
        assert_eq!(payload["findings"][0]["rewrite_status"], "applied");
        // run_scan's gate over the emitted envelope: no unresolved error.
        assert!(!unresolved_findings_at_or_above(&payload, Severity::Error));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);

        // Skipped: error finding left untouched (dirty file) → gate fails.
        let home = tempdir("home-gate-skipped");
        let repo = tempdir("repo-gate-skipped");
        write(&repo.join("main.ts"), TS_WITH_TRACE);
        write(
            &repo.join(".varde-code/rules/pack.toml"),
            REWRITE_RULE_PACK_ERROR,
        );
        let original_home = capture_home();
        with_fresh_db(&home, &repo);
        restore_home(original_home);
        git_init_and_commit(&repo);
        write(
            &repo.join("main.ts"),
            &format!("{TS_WITH_TRACE}\n// dirty\n"),
        );
        let payload = scan_repo(&serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        }))
        .expect("apply scan succeeds");
        assert_eq!(payload["findings"][0]["rewrite_status"], "skipped-dirty");
        assert!(unresolved_findings_at_or_above(&payload, Severity::Error));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn scan_output_omits_rewrite_status_for_findings_without_rewrite() {
        // SQL-rule findings and rewrite-less pattern findings keep their old
        // shape: no `rewrite_status` key (not `null`) even in an apply run.
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-no-status");
        let repo = tempdir("repo-no-status");
        write(&repo.join("main.ts"), TS_WITH_TRACE);
        write(
            &repo.join(".varde-code/rules/pack.toml"),
            r#"
[[rule]]
id = "plain-pattern"
kind = "pattern"
severity = "warning"
message = "plain pattern, no rewrite"
pattern = "console.trace($MSG)"

[[rule]]
id = "function-count"
kind = "sql"
severity = "error"
message = "function present"
query = "SELECT f.path AS file, e.start_line AS line FROM entities e JOIN files f ON f.id = e.file_id WHERE e.kind = 0"
"#,
        );
        let original_home = capture_home();
        with_fresh_db(&home, &repo);
        restore_home(original_home);

        let payload = scan_repo(&serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        }))
        .expect("apply scan succeeds");
        let findings = payload["findings"].as_array().expect("findings array");
        assert!(findings.len() >= 2, "both engines fire: {findings:?}");
        for f in findings {
            assert!(
                f.get("rewrite_status").is_none(),
                "no rewrite_status on rewrite-less finding: {f:?}"
            );
        }
        assert!(
            payload.get("rewrite_summary").is_none(),
            "no summary when nothing was applied"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn scan_with_apply_and_force_writes_dirty_file() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-apply-force");
        let repo = setup_apply_repo("repo-apply-force", &home, TS_WITH_TRACE);
        git_init_and_commit(&repo);
        // Dirty the file after the commit.
        write(
            &repo.join("main.ts"),
            &format!("{TS_WITH_TRACE}\n// uncommitted\n"),
        );

        let input = serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
            "force": true,
        });
        let payload = scan_repo(&input).expect("force apply scan succeeds");
        assert_eq!(payload["findings"].as_array().expect("array").len(), 1);

        let rewritten = std::fs::read_to_string(repo.join("main.ts")).expect("reads");
        assert!(
            rewritten.contains("console.info(\"async log\")"),
            "dirty file written with --force: {rewritten}"
        );
        assert!(
            rewritten.contains("// uncommitted"),
            "uncommitted tail preserved (byte-exact outside span): {rewritten}"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A path to `path` relative to the process cwd — lets a test pass a
    /// *relative* `repoRoot` (which `scan_repo` must handle, CORRECTNESS-002)
    /// without mutating the process cwd (tests run in parallel threads).
    fn rel_from_cwd(path: &std::path::Path) -> std::path::PathBuf {
        let cwd = std::env::current_dir().expect("process cwd");
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::path::absolute(path).expect("path absolutizes")
        };
        let cwd_comp: Vec<_> = cwd.components().collect();
        let abs_comp: Vec<_> = abs.components().collect();
        let mut common = 0;
        while common < cwd_comp.len()
            && common < abs_comp.len()
            && cwd_comp[common] == abs_comp[common]
        {
            common += 1;
        }
        let mut out = std::path::PathBuf::new();
        for _ in common..cwd_comp.len() {
            out.push("..");
        }
        for comp in &abs_comp[common..] {
            out.push(comp.as_os_str());
        }
        out
    }

    #[test]
    fn apply_with_relative_repo_root_still_gates_dirty_files() {
        // CORRECTNESS-002: with a relative repoRoot, the old per-file git
        // call resolved the file path against repo_root a second time
        // (pathspec "sub/a.ts" from cwd "sub" → missing), so every dirty
        // file came back clean and was written without --force. The gate
        // must detect dirty files whether repoRoot is absolute or relative.
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-relroot");
        let repo = tempdir("repo-relroot");
        write(&repo.join("clean.ts"), TS_WITH_TRACE);
        write(&repo.join("dirty.ts"), TS_WITH_TRACE);
        write(&repo.join(".varde-code/rules/pack.toml"), REWRITE_RULE_PACK);
        let original_home = capture_home();
        with_fresh_db(&home, &repo);
        restore_home(original_home);
        git_init_and_commit(&repo);
        // Dirty one file after the commit.
        write(
            &repo.join("dirty.ts"),
            &format!("{TS_WITH_TRACE}\n// uncommitted\n"),
        );
        let dirty_before = std::fs::read_to_string(repo.join("dirty.ts")).expect("reads");

        let rel = rel_from_cwd(&repo);
        let payload = scan_repo(&serde_json::json!({
            "repoRoot": rel.display().to_string(),
            "apply": true,
        }))
        .expect("apply scan with relative repoRoot succeeds");
        let findings = payload["findings"].as_array().expect("findings array");
        assert_eq!(findings.len(), 2, "both files found: {findings:?}");
        let skipped = findings
            .iter()
            .find(|f| f["rewrite_status"] == "skipped-dirty")
            .expect("dirty file skipped even with a relative repoRoot");
        assert!(
            skipped["location"]["file"]
                .as_str()
                .unwrap()
                .ends_with("dirty.ts")
        );
        let applied = findings
            .iter()
            .find(|f| f["rewrite_status"] == "applied")
            .expect("clean sibling still applied");
        assert!(
            applied["location"]["file"]
                .as_str()
                .unwrap()
                .ends_with("clean.ts")
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("dirty.ts")).expect("reads"),
            dirty_before,
            "dirty file not written without --force"
        );
        assert!(
            std::fs::read_to_string(repo.join("clean.ts"))
                .expect("reads")
                .contains("console.info"),
            "clean sibling rewritten"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn apply_without_git_repo_treats_files_clean_and_writes() {
        // Documented scope (CORRECTNESS-005): outside any git repo there is
        // no git state to be undone against, so files are clean and written.
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-nogit");
        let repo = setup_apply_repo("repo-nogit", &home, TS_WITH_TRACE);
        // No git_init_and_commit: the repo dir has no .git.

        let payload = scan_repo(&serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        }))
        .expect("apply scan without git succeeds");
        assert_eq!(
            payload["findings"][0]["rewrite_status"], "applied",
            "not-a-repo → clean → applied: {payload:?}"
        );
        assert!(
            std::fs::read_to_string(repo.join("main.ts"))
                .expect("reads")
                .contains("console.info"),
            "file written"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn apply_with_corrupt_git_index_fails_closed_and_skips_files() {
        // CORRECTNESS-005: a genuine git failure (corrupt index) must fail
        // closed — every file skipped-dirty, nothing written — rather than
        // silently treating the repo as clean.
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-corrupt");
        let repo = setup_apply_repo("repo-corrupt", &home, TS_WITH_TRACE);
        git_init_and_commit(&repo);
        // Poison the index: `git rev-parse` still works, `git status` errors.
        std::fs::write(repo.join(".git/index"), b"not an index").expect("index overwritten");
        let before = std::fs::read_to_string(repo.join("main.ts")).expect("reads");

        let payload = scan_repo(&serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        }))
        .expect("apply scan with corrupt git index succeeds");
        let findings = payload["findings"].as_array().expect("findings array");
        assert!(
            findings
                .iter()
                .all(|f| f["rewrite_status"] == "skipped-dirty"),
            "every finding skipped on git failure: {findings:?}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("main.ts")).expect("reads"),
            before,
            "nothing written when the git gate cannot run"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn apply_preserves_file_permission_bits() {
        // CORRECTNESS-001: the atomic write must preserve the original
        // file's mode — an executable stays executable, a read-only file
        // stays read-only (both rewritten in place).
        use std::os::unix::fs::PermissionsExt;
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-perms");
        let repo = tempdir("repo-perms");
        write(&repo.join("exec.ts"), TS_WITH_TRACE);
        write(&repo.join("ro.ts"), TS_WITH_TRACE);
        write(&repo.join(".varde-code/rules/pack.toml"), REWRITE_RULE_PACK);
        let original_home = capture_home();
        with_fresh_db(&home, &repo);
        restore_home(original_home);
        // Set modes BEFORE the commit so the worktree stays clean (a mode
        // change after the commit would itself be a git change → skipped).
        std::fs::set_permissions(repo.join("exec.ts"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");
        std::fs::set_permissions(repo.join("ro.ts"), std::fs::Permissions::from_mode(0o444))
            .expect("chmod 444");
        git_init_and_commit(&repo);

        let payload = scan_repo(&serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        }))
        .expect("apply scan succeeds");
        let findings = payload["findings"].as_array().expect("findings array");
        assert_eq!(findings.len(), 2, "both files found: {findings:?}");
        assert!(
            findings.iter().all(|f| f["rewrite_status"] == "applied"),
            "both clean files applied: {findings:?}"
        );

        let mode = |p: &std::path::Path| {
            std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777
        };
        assert_eq!(
            mode(&repo.join("exec.ts")),
            0o755,
            "executable bit preserved"
        );
        assert_eq!(mode(&repo.join("ro.ts")), 0o444, "read-only mode preserved");
        assert!(
            std::fs::read_to_string(repo.join("exec.ts"))
                .expect("reads")
                .contains("console.info"),
            "exec fixture rewritten"
        );
        assert!(
            std::fs::read_to_string(repo.join("ro.ts"))
                .expect("reads")
                .contains("console.info"),
            "read-only fixture rewritten"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn apply_rewrites_symlink_target_preserving_the_link() {
        // CORRECTNESS-004: a symlinked file's rewrite must land on the real
        // target (canonicalize-then-write) and leave the link itself alone —
        // never silently de-symlink the entry.
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-symlink");
        let repo = tempdir("repo-symlink");
        write(&repo.join("real.ts"), TS_WITH_TRACE);
        std::os::unix::fs::symlink(repo.join("real.ts"), repo.join("link.ts")).expect("symlink");
        write(&repo.join(".varde-code/rules/pack.toml"), REWRITE_RULE_PACK);
        let original_home = capture_home();
        with_fresh_db(&home, &repo);
        restore_home(original_home);
        git_init_and_commit(&repo);

        let payload = scan_repo(&serde_json::json!({
            "repoRoot": repo.display().to_string(),
            "apply": true,
        }))
        .expect("apply scan succeeds");
        let findings = payload["findings"].as_array().expect("findings array");
        assert_eq!(findings.len(), 2, "link + real both found: {findings:?}");
        // The link survives as a symlink pointing at the same target…
        let link_meta = std::fs::symlink_metadata(repo.join("link.ts")).expect("link metadata");
        assert!(
            link_meta.file_type().is_symlink(),
            "link is not de-symlinked"
        );
        assert_eq!(
            std::fs::read_link(repo.join("link.ts")).expect("read_link"),
            repo.join("real.ts"),
            "link still points at real.ts"
        );
        // …and the real target was rewritten exactly once, no corruption.
        let rewritten = std::fs::read_to_string(repo.join("real.ts")).expect("reads");
        assert!(
            rewritten.contains("console.info(\"async log\")"),
            "real target rewritten: {rewritten}"
        );
        assert!(
            !rewritten.contains("console.trace"),
            "no trace remains: {rewritten}"
        );
        assert_eq!(
            rewritten.matches("console.info").count(),
            1,
            "single rewrite, no double-apply corruption: {rewritten}"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn fresh_db_runs_both_rule_kinds_and_merges_output() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home");
        let repo = tempdir("repo");
        write(&repo.join("main.ts"), TS_FIXTURE);
        write(&repo.join(".varde-code/rules/pack.toml"), RULE_PACK);
        let original_home = capture_home();
        with_fresh_db(&home, &repo);

        let input = serde_json::json!({ "repoRoot": repo.display().to_string() });
        let payload = scan_repo(&input).expect("scan succeeds");
        let findings = payload["findings"].as_array().expect("findings array");
        let diagnostics = payload["diagnostics"]
            .as_array()
            .expect("diagnostics array");

        let pattern_ids: Vec<&str> = findings
            .iter()
            .filter(|f| f["rule_id"] == "no-console")
            .map(|f| f["rule_id"].as_str().unwrap())
            .collect();
        let sql_ids: Vec<&str> = findings
            .iter()
            .filter(|f| f["rule_id"] == "function-count")
            .map(|f| f["rule_id"].as_str().unwrap())
            .collect();
        assert_eq!(pattern_ids.len(), 1, "pattern rule matches: {findings:?}");
        assert_eq!(sql_ids.len(), 1, "sql rule matches the persisted entity");
        assert_eq!(findings.len(), 2, "both engines' findings merged");

        let sql_finding = findings
            .iter()
            .find(|f| f["rule_id"] == "function-count")
            .unwrap();
        assert!(
            sql_finding["location"]["file"]
                .as_str()
                .unwrap()
                .ends_with("main.ts")
        );
        assert_eq!(sql_finding["severity"], "error");
        assert_eq!(sql_finding["certainty"], serde_json::Value::Null);

        assert!(
            diagnostics.is_empty(),
            "no diagnostics on a clean run: {diagnostics:?}"
        );

        restore_home(original_home);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn missing_db_builds_on_read_then_scans() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-empty");
        let repo = tempdir("repo-empty");
        write(&repo.join(".varde-code/rules/pack.toml"), RULE_PACK);
        // No build → no DB under the conventional path; scan must build-on-read.

        let input = serde_json::json!({ "repoRoot": repo.display().to_string() });
        let payload = scan_repo(&input).expect("missing db self-builds and scans");
        assert!(payload["findings"].as_array().expect("array").is_empty());
        assert!(payload["diagnostics"].as_array().expect("array").is_empty());

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn stale_db_self_freshens_and_matches_build_scan() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-stale");
        let repo = tempdir("repo-stale");
        write(&repo.join("main.ts"), TS_FIXTURE);
        write(&repo.join(".varde-code/rules/pack.toml"), RULE_PACK);
        let original_home = capture_home();
        with_fresh_db(&home, &repo);

        // Modify the source after the build → the DB is now stale.
        write(
            &repo.join("main.ts"),
            &format!("{TS_FIXTURE}\n// touched\n"),
        );

        let input = serde_json::json!({ "repoRoot": repo.display().to_string() });
        let freshened = scan_repo(&input).expect("stale db self-freshens and scans");

        // Parity: a fresh build + scan must produce the identical payload.
        with_fresh_db(&home, &repo);
        let rebuilt = scan_repo(&input).expect("build+scan succeeds");
        assert_eq!(freshened, rebuilt, "freshen-then-scan matches build+scan");

        restore_home(original_home);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn rules_list_tags_builtin_override_and_custom() {
        let repo = tempdir("rules-list-repo");
        write(
            &repo.join(".varde-code/rules/pack.toml"),
            r#"
[[rule]]
id = "churn-complexity-hotspot"
kind = "sql"
severity = "error"
message = "repo override of the built-in"
query = "SELECT path AS file, 1 AS line FROM files WHERE complexity > :max_complexity"
thresholds = { max_complexity = 1.0 }

[[rule]]
id = "no-console"
kind = "pattern"
severity = "warning"
message = "custom repo-only rule"
pattern = "console.log($MSG)"
"#,
        );

        let input = serde_json::json!({ "repoRoot": repo.display().to_string() });
        let payload = rules_list(&input).expect("rules_list succeeds without a DB");
        let rules = payload["rules"].as_array().expect("rules array");
        assert!(payload["diagnostics"].as_array().expect("array").is_empty());

        let by_id = |id: &str| rules.iter().find(|r| r["id"] == id).expect("rule present");
        assert_eq!(by_id("churn-complexity-hotspot")["source"], "override");
        assert_eq!(by_id("no-console")["source"], "custom");
        assert_eq!(by_id("file-complexity-hotspot")["source"], "builtin");

        assert_eq!(
            rules.len(),
            crate::rules::builtin_rules().len() + 1,
            "every built-in id present once (one overridden, not duplicated) plus the custom rule"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn rules_list_requires_no_db_and_rejects_bad_repo_root() {
        let input = serde_json::json!({ "repoRoot": "/definitely/not/a/real/path" });
        let err = rules_list(&input).expect_err("missing repo root is an error");
        assert_eq!(err.code, "invalid_input");
    }

    #[test]
    fn empty_rule_pack_is_a_valid_noop_scan() {
        let _guard = crate::HOME_TEST_LOCK.lock().expect("home lock");
        let home = tempdir("home-norules");
        let repo = tempdir("repo-norules");
        write(&repo.join("main.ts"), TS_FIXTURE);
        let original_home = capture_home();
        with_fresh_db(&home, &repo);

        let input = serde_json::json!({ "repoRoot": repo.display().to_string() });
        let payload = scan_repo(&input).expect("empty pack scans cleanly");
        assert!(payload["findings"].as_array().expect("array").is_empty());
        assert!(payload["diagnostics"].as_array().expect("array").is_empty());

        restore_home(original_home);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&repo);
    }
}

#[cfg(test)]
mod git_gate {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("varde-gitgate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        dir
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_and_commit(repo: &Path) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "t@example.com"]);
        git(repo, &["config", "user.name", "test"]);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "init"]);
    }

    fn file(repo: &Path, name: &str) -> PathBuf {
        let p = repo.join(name);
        std::fs::write(&p, "content\n").expect("file writes");
        p
    }

    /// The dirty set for `targets` in `repo` (assumes a healthy work tree).
    fn dirty_set(repo: &Path, targets: &[PathBuf]) -> HashSet<PathBuf> {
        match git_gate(repo, targets) {
            GitGate::WorkTree(_, set) => set,
            GitGate::NotAGitRepo => panic!("expected a work tree"),
            GitGate::Unverifiable => panic!("expected a clean status run"),
        }
    }

    #[test]
    fn not_a_git_repo_is_clean() {
        let repo = tempdir("norepo");
        let f = file(&repo, "a.ts");
        assert!(matches!(git_gate(&repo, &[f]), GitGate::NotAGitRepo));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn clean_tracked_file_is_not_dirty() {
        let repo = tempdir("clean");
        let f = file(&repo, "a.ts");
        init_and_commit(&repo);
        assert!(dirty_set(&repo, &[f]).is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn modified_tracked_file_is_dirty() {
        let repo = tempdir("modified");
        let f = file(&repo, "a.ts");
        init_and_commit(&repo);
        std::fs::write(&f, "changed\n").expect("modifies");
        let real = std::fs::canonicalize(&f).expect("canonical");
        assert_eq!(
            dirty_set(&repo, &[f]),
            HashSet::from([real]),
            "modified tracked file reported dirty"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn staged_file_is_dirty() {
        let repo = tempdir("staged");
        let f = file(&repo, "a.ts");
        init_and_commit(&repo);
        std::fs::write(&f, "staged change\n").expect("modifies");
        git(&repo, &["add", "a.ts"]);
        assert!(!dirty_set(&repo, &[f]).is_empty(), "staged file is dirty");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn untracked_file_is_dirty() {
        let repo = tempdir("untracked");
        file(&repo, "keep.ts");
        init_and_commit(&repo);
        let f = file(&repo, "new.ts");
        assert!(
            !dirty_set(&repo, &[f]).is_empty(),
            "untracked file is dirty"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn gitignored_file_is_dirty() {
        // CORRECTNESS-003: a git-ignored (untracked + .gitignore-matched)
        // file must not bypass the dirty gate — `git status` only lists it
        // with --ignored, which the batched status call passes.
        let repo = tempdir("ignored");
        file(&repo, "keep.ts");
        init_and_commit(&repo);
        std::fs::write(repo.join(".gitignore"), "gen.ts\n").expect("gitignore writes");
        git(&repo, &["add", ".gitignore"]);
        git(&repo, &["commit", "-q", "-m", "ignore"]);
        let f = file(&repo, "gen.ts");
        assert!(
            !dirty_set(&repo, &[f]).is_empty(),
            "ignored untracked file is dirty"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn corrupt_index_gates_every_file_dirty() {
        // CORRECTNESS-005: a genuine git failure (corrupt index) must fail
        // closed — every file dirty — not silently clean.
        let repo = tempdir("corrupt");
        let f = file(&repo, "a.ts");
        init_and_commit(&repo);
        std::fs::write(repo.join(".git/index"), b"garbage index").expect("index overwritten");
        assert!(matches!(git_gate(&repo, &[f]), GitGate::Unverifiable));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn target_outside_work_tree_is_not_gated() {
        // A symlink target outside the work tree has no git state to gate
        // against — clean, same rationale as not-a-repo.
        let repo = tempdir("outside");
        file(&repo, "keep.ts");
        init_and_commit(&repo);
        let outside = tempdir("outside-target");
        std::fs::write(outside.join("real.ts"), "x\n").expect("writes");
        std::os::unix::fs::symlink(outside.join("real.ts"), repo.join("link.ts")).expect("symlink");
        let real = std::fs::canonicalize(repo.join("link.ts")).expect("canonical");
        assert!(
            matches!(git_gate(&repo, &[real]), GitGate::WorkTree(..)),
            "outside-worktree target is not gated"
        );
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
