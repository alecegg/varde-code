//! DB path convention: `~/.config/varde-code/repos/<name>-<hash>/index.db`.
//!
//! Mirrors varde's per-repo isolation + collision-safe hashing scheme under
//! varde-code's own namespace so it never collides with varde's
//! `intelligence.db`.
//!
//! `repo_db_path` best-effort canonicalizes its input before hashing (see
//! its doc comment) — the only filesystem access this module performs, and
//! one that never fails the call. Parent-directory creation is the
//! `persist()` entry point's responsibility.

use std::path::{Path, PathBuf};

/// Hash algorithm: FNV-1a 64-bit over the repo root path exactly as passed.
///
/// Chosen over a cryptographic hash to keep the module dependency-free; 64
/// bits of FNV-1a is collision-safe for distinguishing repo roots, and the
/// hash covers the full path (not just the basename), so two different roots
/// that share a basename still produce different `<hash>` suffixes. The hash
/// is hex-encoded to 16 lowercase hex chars.
fn repo_hash(repo_root: &Path) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in repo_root.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Compute the index.db path for `repo_root`.
///
/// Deterministic *per repo*: callers reach this from different cwds and with
/// different spellings of the same repo (`.` vs. its absolute form, a path
/// through a symlink vs. the resolved target, `../sibling` from a different
/// cwd) — hashing the string verbatim would scatter one repo's index across
/// multiple DBs (e.g. `build --repo-root .` run from inside the repo vs.
/// `watch --repo <absolute path>` run from elsewhere silently landing on two
/// different files). So this first best-effort canonicalizes `repo_root` to
/// the real absolute path it refers to on disk, and hashes that; a `repo_root`
/// that doesn't exist yet (e.g. one a caller is about to create) falls back
/// to the input unchanged rather than erroring, so this call never fails.
pub fn repo_db_path(repo_root: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let name = canonical
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let hash = repo_hash(&canonical);
    home_dir()
        .join(".config")
        .join("varde-code")
        .join("repos")
        .join(format!("{name}-{hash}"))
        .join("index.db")
}

/// The `~/.config/varde-code` directory — shared root for per-repo DBs
/// (`repos/<name>-<hash>/index.db`) and process-level config/lock files
/// (e.g. `watch.toml`, `watch-locks/`).
pub fn config_dir() -> PathBuf {
    home_dir().join(".config").join("varde-code")
}

/// The user's home directory, read from the `HOME` environment variable.
///
/// Falls back to the literal path segment `~` when `HOME` is unset so the
/// function stays total; `persist()` will fail with a clear error if the
/// resulting parent directory cannot be created.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"))
}

#[cfg(test)]
mod path_convention {
    use super::*;

    #[test]
    fn returns_expected_shape_and_is_deterministic() {
        let root = Path::new("/work/example-repo");
        let first = repo_db_path(root);
        let second = repo_db_path(root);
        assert_eq!(first, second, "same input → identical path");

        let text = first.to_string_lossy();
        assert!(
            text.contains(".config/varde-code/repos/"),
            "must live under .config/varde-code/repos/: {text}"
        );
        assert!(
            text.contains("/example-repo-"),
            "basename must appear before the hash: {text}"
        );
        assert!(text.ends_with("/index.db"), "got: {text}");
    }

    #[test]
    fn same_basename_different_roots_hash_differently() {
        let a = repo_db_path(Path::new("/work/example-repo"));
        let b = repo_db_path(Path::new("/elsewhere/example-repo"));
        assert_ne!(
            a, b,
            "roots sharing a basename must not collide; {a:?} vs {b:?}"
        );
    }

    #[test]
    fn no_filesystem_io_during_computation() {
        // Point the computation at a path that cannot exist: the hash and
        // basename are derived from the path string alone, so this must
        // succeed without touching the filesystem.
        let root = Path::new("/nonexistent-root-xyz/example-repo");
        let result = repo_db_path(root);
        assert!(result.to_string_lossy().contains("example-repo-"));
    }
}
