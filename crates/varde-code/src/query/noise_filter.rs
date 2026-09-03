//! Shared filter for generated/vendored paths that should be excluded from
//! navigation-facing summaries (e.g. nav_map sections).
//!
//! Matching is done on path components/prefixes, not naive substring
//! matching, so a file literally named `distiller.rs` is not mistaken for
//! something under a `dist/` directory.

use std::path::Path;

/// Directory names (as path components) that mark a path as
/// generated/vendored and therefore noise for navigation purposes.
const EXCLUDED_DIR_COMPONENTS: &[&str] = &["target", "node_modules", "dist", "build"];

/// Returns `true` if `path` is under a generated/vendored directory such as
/// `target/`, `node_modules/`, `dist/`, or `build/`.
///
/// Matching is component-based: a path only matches if one of its
/// directory components exactly equals an excluded name (e.g. `target`),
/// so `distiller.rs` or `crates/distributor/` do not false-positive.
pub fn is_generated_or_vendored_path(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|s| EXCLUDED_DIR_COMPONENTS.contains(&s))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn true_for_generated_or_vendored_paths() {
        assert!(is_generated_or_vendored_path("target/foo.rs"));
        assert!(is_generated_or_vendored_path("node_modules/bar.js"));
        assert!(is_generated_or_vendored_path("dist/x.js"));
        assert!(is_generated_or_vendored_path("build/y.rs"));
    }

    #[test]
    fn true_for_nested_generated_or_vendored_paths() {
        assert!(is_generated_or_vendored_path("crates/foo/target/debug/x"));
        assert!(is_generated_or_vendored_path("a/b/node_modules/c.js"));
    }

    #[test]
    fn false_for_normal_source_paths() {
        assert!(!is_generated_or_vendored_path("src/lib.rs"));
        assert!(!is_generated_or_vendored_path(
            "crates/varde-code/src/lib.rs"
        ));
        assert!(!is_generated_or_vendored_path(
            "memory-bank/working/plans/x.md"
        ));
    }

    #[test]
    fn false_for_broader_non_excluded_list() {
        for p in [
            "crates/",
            "src/",
            "memory-bank/",
            "crates/varde-code/Cargo.toml",
        ] {
            assert!(
                !is_generated_or_vendored_path(p),
                "unexpected match for {p}"
            );
        }
    }

    #[test]
    fn no_false_positive_on_substring_matching_names() {
        // "distiller.rs" contains "dist" as a substring but is not a `dist/`
        // directory component, so naive substring matching would wrongly
        // flag it. Ensure our component-based check does not.
        assert!(!is_generated_or_vendored_path("src/distiller.rs"));
        assert!(!is_generated_or_vendored_path(
            "crates/distributor/src/lib.rs"
        ));
        assert!(!is_generated_or_vendored_path("builder/src/lib.rs"));
    }
}
