//! Git churn computation.
//!
//! Per-file commit count via `git log`, capped to a rolling 90-day window
//! (documented in `memory-bank/knowledge/reference/sqlite-persistence.md`):
//! the number of commits whose diff touches the file within the last 90
//! days. The window keeps `churn` a *recent activity* signal for hotspots and
//! bounds the walk on deep-history repos.
//!
//! Robustness contract: any failure mode — not inside a git repository, no
//! git binary, file untracked/new with zero commits — yields `0`, never an
//! error or panic.

/// `git log --since` window: only commits from the last 90 days count toward
/// churn. Commit dates are not strictly monotonic, so this is a practical
/// bound, not a hard cutoff.
const CHURN_WINDOW: &str = "--since=90 days ago";

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Batched replacement for calling [`commit_count`] once per file.
///
/// The naive per-file approach spawns one `git log -- <file>` subprocess per
/// file, which is catastrophically slow on repos with nontrivial history
/// (2m19s to index ~40 files was observed against a real-world repo). This
/// instead runs a single `git log --name-only` walk over the repo root and
/// counts file occurrences in memory.
///
/// `known_root` is the repo root the build was told to index (`--repo-root`).
/// Resolving its git top-level *once* replaces the previous per-directory
/// `git rev-parse --show-toplevel` discovery, which spawned one subprocess per
/// unique parent directory (~384 spawns ≈ 3 s on the benchmark repo). Files
/// that happen not to sit under `known_root`'s top-level (nested repos /
/// submodules, symlinks escaping the root) fall back to per-directory
/// discovery so their churn stays correct.
///
/// Files that aren't inside a git repo (or whose repo root can't be
/// resolved) get `0`, matching [`commit_count`]'s failure contract.
pub fn commit_counts_batch(files: &[String], known_root: &Path) -> HashMap<String, u32> {
    let mut result = HashMap::with_capacity(files.len());
    if files.is_empty() {
        return result;
    }

    // Resolve the git top-level once from the known repo root, rather than
    // once per unique directory.
    let known_root = known_root
        .canonicalize()
        .unwrap_or_else(|_| known_root.to_path_buf());
    let toplevel = repo_root(&known_root);

    // Partition files: those under the known top-level use the single walk
    // below; the rest (rare) use per-directory discovery.
    let mut under: Vec<(&String, PathBuf)> = Vec::new();
    let mut other: Vec<&String> = Vec::new();

    for file in files {
        let path = Path::new(file);
        let rel = toplevel.as_deref().and_then(|top| {
            // Fast path: the file is already an absolute path under `top`
            // (the common case — no syscall). Fall back to canonicalize for
            // relative / non-canonical roots.
            if let Ok(rel) = path.strip_prefix(top) {
                return Some(rel.to_path_buf());
            }
            let abs = path.canonicalize().ok()?;
            abs.strip_prefix(top).ok().map(|rel| rel.to_path_buf())
        });
        match rel {
            Some(rel) => under.push((file, rel)),
            None => other.push(file),
        }
    }

    if let Some(top) = toplevel.as_deref().filter(|_| !under.is_empty()) {
        let counts = commit_counts_for_root(top);
        for (file, rel) in under {
            let count = counts
                .get(&rel.to_string_lossy().into_owned())
                .copied()
                .unwrap_or(0);
            result.insert(file.clone(), count);
        }
    }

    // Per-directory discovery for the remainder (nested repos, symlinks
    // escaping the root, or a non-repo build root).
    if !other.is_empty() {
        let mut by_root: HashMap<PathBuf, Vec<&String>> = HashMap::new();
        let mut root_cache: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
        for file in other {
            let dir = Path::new(file)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| Path::new(".").to_path_buf());
            let root = root_cache
                .entry(dir.clone())
                .or_insert_with(|| repo_root(&dir))
                .clone();
            match root {
                Some(root) => by_root.entry(root).or_default().push(file),
                None => {
                    result.insert(file.clone(), 0);
                }
            }
        }

        for (root, root_files) in by_root {
            let counts = commit_counts_for_root(&root);
            for file in root_files {
                let count = Path::new(file)
                    .canonicalize()
                    .ok()
                    .and_then(|abs| abs.strip_prefix(&root).ok().map(|p| p.to_path_buf()))
                    .and_then(|rel| counts.get(&rel.to_string_lossy().into_owned()).copied())
                    .unwrap_or(0);
                result.insert(file.clone(), count);
            }
        }
    }

    result
}

/// Resolve `dir`'s git top-level root, or `None` if it's not inside a repo.
fn repo_root(dir: &Path) -> Option<PathBuf> {
    let text = crate::git::run_git(&["rev-parse", "--show-toplevel"], dir)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Path::new(trimmed).canonicalize().ok()
}

/// One `git log --name-only` walk over `root`'s recent (90-day) history,
/// counting how many commits touched each root-relative path.
fn commit_counts_for_root(root: &Path) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    // `-c core.quotepath=false`: without it git octal-escapes and quotes any
    // path with non-ASCII/special bytes (e.g. `"src/caf\303\251.rs"`), which
    // never matches the un-quoted indexed path and silently yields churn=0 for
    // those files. Disabling quotepath emits the raw UTF-8 path verbatim.
    let Some(text) = crate::git::run_git(
        &[
            "-c",
            "core.quotepath=false",
            "log",
            "--name-only",
            "--pretty=format:",
            CHURN_WINDOW,
        ],
        root,
    ) else {
        return counts;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        *counts.entry(line.to_string()).or_insert(0) += 1;
    }
    counts
}

/// Count commits touching `file` within the last 90 days.
///
/// Runs `git log --pretty=oneline --since=90 days ago -- <file>` from the
/// file's parent directory so a bare relative path resolves inside its own
/// repo. Every output line is one commit touching the file.
pub fn commit_count(file: &str) -> u32 {
    let dir = Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf());

    let Some(text) = crate::git::run_git(
        &[
            "-c",
            "core.quotepath=false",
            "log",
            "--pretty=oneline",
            CHURN_WINDOW,
            "--",
            file,
        ],
        &dir,
    ) else {
        // Not a repo, no git, or the command errored: treat as zero churn.
        return 0;
    };

    text.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

#[cfg(test)]
mod git_log_known_history_matches_expected {
    use super::*;

    /// Scaffold a throwaway git repo at `dir` and commit `file` content
    /// `commits` times, returning the file's path inside the repo.
    fn repo_with_commits(tag: &str, commits: u32) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("varde-churn-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("repo dir creates");
        run(&dir, &["init", "-q"]);
        run(&dir, &["config", "user.email", "test@example.com"]);
        run(&dir, &["config", "user.name", "Churn Test"]);
        let file = dir.join("file.txt");
        for i in 0..commits {
            std::fs::write(&file, format!("revision {i}\n")).expect("writes");
            run(&dir, &["add", "file.txt"]);
            let msg = format!("commit {i}");
            run(&dir, &["commit", "-q", "-m", &msg]);
        }
        file
    }

    fn run(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn known_history_counts_commits() {
        let file = repo_with_commits("known", 4);
        assert_eq!(commit_count(&file.to_string_lossy()), 4);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn untracked_file_scores_zero() {
        let dir =
            std::env::temp_dir().join(format!("varde-churn-untracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir creates");
        run(&dir, &["init", "-q"]);
        // File written but never committed.
        let file = dir.join("new.txt");
        std::fs::write(&file, "uncommitted").expect("writes");
        assert_eq!(commit_count(&file.to_string_lossy()), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_repo_directory_scores_zero_without_panicking() {
        let dir = std::env::temp_dir().join(format!("varde-churn-norepo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir creates");
        let file = dir.join("lonely.txt");
        std::fs::write(&file, "no repo here").expect("writes");
        assert_eq!(commit_count(&file.to_string_lossy()), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: without `core.quotepath=false`, git emits a non-ASCII path
    /// octal-escaped and quoted (`"caf\303\251.rs"`), so the batch counts are
    /// keyed by that quoted string and never match the real UTF-8 path — the
    /// file silently gets churn 0. The batch walk must key counts by the raw
    /// `café.rs` path.
    #[test]
    fn non_ascii_path_is_counted_unquoted() {
        let dir =
            std::env::temp_dir().join(format!("varde-churn-unicode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir creates");
        run(&dir, &["init", "-q"]);
        run(&dir, &["config", "user.email", "test@example.com"]);
        run(&dir, &["config", "user.name", "Churn Test"]);
        let name = "café.rs";
        let file = dir.join(name);
        for i in 0..2 {
            std::fs::write(&file, format!("rev {i}\n")).expect("writes");
            run(&dir, &["add", "--", name]);
            run(&dir, &["commit", "-q", "-m", &format!("c{i}")]);
        }
        let counts = commit_counts_for_root(&dir);
        assert_eq!(
            counts.get(name),
            Some(&2),
            "unquoted UTF-8 path is the count key: {counts:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
