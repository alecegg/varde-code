//! Shared filter for paths that should be excluded from navigation-facing
//! summaries (e.g. nav_map sections): generated/vendored trees and non-source
//! files (docs, assets, configs the extractor never parses).
//!
//! Matching is done on path components/prefixes, not naive substring
//! matching, so a file literally named `distiller.rs` is not mistaken for
//! something under a `dist/` directory.

use std::path::Path;

/// Returns `true` if `path` is not a file the indexer extracts code from —
/// i.e. [`crate::parse::language_for_path`] does not recognize its extension
/// (docs like `.md`/`.mdx`, config like `.yaml`/`.json`/`.toml`, and binary
/// assets like `.png`/`.pdf`).
///
/// Such files still get a `files` row (the build walks every non-gitignored
/// file so it can persist a skip diagnostic), but they carry no entities and
/// no resolved edges. In graph-blind sections that read the whole `files`
/// table — `hotspots` (churn-only rows) and `subsystems` (singleton
/// communities) — they are pure noise and dominate the output, so those
/// sections filter them out. Entity/edge/fan-in-gated sections never surface
/// them and need no extra guard.
pub fn is_non_source_path(path: &str) -> bool {
    crate::parse::language_for_path(Path::new(path)).is_none()
}

/// Directory names (as path components) that mark a path as scaffold/codegen
/// boilerplate: source stamped out by a project generator (`crewai create`,
/// cookiecutter, `create-*` CLIs), not code that runs in *this* repo.
const SCAFFOLD_DIR_COMPONENTS: &[&str] = &["templates", "template"];

/// Returns `true` if `path` lives under a scaffold/template directory.
///
/// A role-tagged handler defined in a scaffold template (e.g. a Flask
/// `@app.route` inside `crewai_cli/templates/…`) is inert boilerplate that a
/// generator later copies into a *new* project — it is not an entrypoint of
/// the repo being navigated, the same way a handler in a test file is a
/// fixture rather than a real entrypoint. This is applied only in entrypoint
/// detection, deliberately narrow: web frameworks keep view templates (HTML,
/// already non-source) under `templates/`, and some repos ship real code under
/// `src/templates/` — but such code carries no handler role tag, so scoping
/// the filter to entrypoints avoids dropping it from the other sections.
pub fn is_scaffold_template_path(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|s| SCAFFOLD_DIR_COMPONENTS.contains(&s))
            .unwrap_or(false)
    })
}

/// Directory names (as path components) that mark a path as
/// generated/vendored and therefore noise for navigation purposes.
///
/// `vendor`/`third_party`/`bower_components` are third-party dependency trees;
/// `.next`/`.nuxt` are framework build output; `grammars` is the tree-sitter
/// convention for a directory of generated `parser.c`/grammar files (the audit
/// found 25 false-positive complexity findings on `grammars/*/parser.c`).
const EXCLUDED_DIR_COMPONENTS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "vendor",
    "third_party",
    "third-party",
    "bower_components",
    ".next",
    ".nuxt",
    "grammars",
];

/// Consecutive path-component sequences that mark a framework's *compiled*
/// asset tree — generated/bundled output living under an otherwise-ordinary
/// name. Matched as an adjacent window so a lone `static/` (a real source dir
/// in many apps) is not excluded. Phoenix serves compiled JS/CSS from
/// `priv/static/` (the audit found 87% of one repo's findings cited
/// `priv/static/phoenix.*.js` bundles).
const EXCLUDED_DIR_SEQUENCES: &[&[&str]] = &[&["priv", "static"]];

/// Filename suffixes that mark a file as a minified/bundled asset rather than
/// source a human edits (`app.min.js`, `vendor.bundle.js`, ...).
const GENERATED_FILE_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.mjs",
    ".min.cjs",
    ".min.css",
    ".bundle.js",
    ".bundle.mjs",
];

/// Returns `true` if `path` is generated/vendored: under a dependency/build
/// directory (`target/`, `node_modules/`, `vendor/`, `grammars/`, ...), under a
/// framework compiled-asset tree (`priv/static/`), or a minified/bundled
/// filename (`*.min.js`, `*.bundle.js`).
///
/// Matching is component-based: a path only matches on a whole directory
/// component (e.g. `target`), never a substring, so `distiller.rs` or
/// `crates/distributor/` do not false-positive.
pub fn is_generated_or_vendored_path(path: &str) -> bool {
    let p = Path::new(path);
    let components: Vec<&str> = p
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    if components
        .iter()
        .any(|c| EXCLUDED_DIR_COMPONENTS.contains(c))
    {
        return true;
    }
    if EXCLUDED_DIR_SEQUENCES
        .iter()
        .any(|seq| components.windows(seq.len()).any(|w| w == *seq))
    {
        return true;
    }
    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
        let lower = name.to_ascii_lowercase();
        if GENERATED_FILE_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
            return true;
        }
    }
    false
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
    fn true_for_broadened_generated_cases() {
        // Audit S8: these were the specific false-positive sources.
        assert!(is_generated_or_vendored_path("priv/static/phoenix.js"));
        assert!(is_generated_or_vendored_path("priv/static/phoenix.cjs.js"));
        assert!(is_generated_or_vendored_path("priv/static/phoenix.mjs"));
        assert!(is_generated_or_vendored_path("assets/app.min.js"));
        assert!(is_generated_or_vendored_path("public/vendor.bundle.js"));
        assert!(is_generated_or_vendored_path("grammars/kotlin/parser.c"));
        assert!(is_generated_or_vendored_path("vendor/lib/dep.go"));
        assert!(is_generated_or_vendored_path("third_party/x/y.cc"));
    }

    #[test]
    fn false_for_lookalikes_of_broadened_cases() {
        // A lone `static/` source dir must not be excluded (only `priv/static`).
        assert!(!is_generated_or_vendored_path("static/site.js"));
        assert!(!is_generated_or_vendored_path("app/static/handler.rs"));
        // A hand-written file that merely ends in these words is source.
        assert!(!is_generated_or_vendored_path("src/admin.js"));
        assert!(!is_generated_or_vendored_path("src/parser.rs"));
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
    fn true_for_non_source_files() {
        // Docs, config, and binary assets the extractor never parses.
        assert!(is_non_source_path("docs/v1/en/tools/spidertool.mdx"));
        assert!(is_non_source_path("README.md"));
        assert!(is_non_source_path("config/values.yaml"));
        assert!(is_non_source_path("package.json"));
        assert!(is_non_source_path("Cargo.toml"));
        assert!(is_non_source_path("assets/logo.png"));
    }

    #[test]
    fn false_for_source_files() {
        // Every extension the extractor recognizes is a source file.
        assert!(!is_non_source_path("src/lib.rs"));
        assert!(!is_non_source_path("app/main.py"));
        assert!(!is_non_source_path("web/index.ts"));
        assert!(!is_non_source_path("cmd/server/main.go"));
        assert!(!is_non_source_path("include/foo.h"));
    }

    #[test]
    fn true_for_scaffold_template_paths() {
        assert!(is_scaffold_template_path(
            "lib/cli/src/crewai_cli/templates/crew/crew.py"
        ));
        assert!(is_scaffold_template_path(
            "templates/plugin-template/index.ts"
        ));
        assert!(is_scaffold_template_path("a/b/template/main.go"));
    }

    #[test]
    fn false_for_non_scaffold_paths() {
        // Real source, even under a `src/templates/` module, is a component
        // match — but such files carry no handler role tag, so entrypoints is
        // the only caller and this predicate is deliberately scoped there.
        assert!(!is_scaffold_template_path("src/auth/login.rs"));
        assert!(!is_scaffold_template_path("app/main.py"));
        // Substring, not a component: `templated.rs` is not under `templates/`.
        assert!(!is_scaffold_template_path("src/templated.rs"));
        assert!(!is_scaffold_template_path("src/templating/engine.rs"));
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
