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
/// - `alembic/versions`, `migrations/versions`: auto-generated DB migration
///   revisions (SQLAlchemy/Alembic) — boilerplate `op.create_table(...)` the
///   cross-language audit flagged as `duplicate-code-clone` false positives.
const EXCLUDED_DIR_SEQUENCES: &[&[&str]] = &[
    &["priv", "static"],
    &["alembic", "versions"],
    &["migrations", "versions"],
];

/// Filename suffixes that mark a file as a minified/bundled/generated asset
/// rather than source a human edits (`app.min.js`, `vendor.bundle.js`,
/// `helloworld.pb.go`, `client.gen.ts`, ...). `.pb.go` is protoc-generated Go;
/// `.gen.*` is the widespread codegen convention (OpenAPI clients, etc.) — both
/// surfaced as scan false positives in the cross-language audit.
const GENERATED_FILE_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.mjs",
    ".min.cjs",
    ".min.css",
    ".bundle.js",
    ".bundle.mjs",
    ".pb.go",
    ".gen.go",
    ".gen.ts",
    ".gen.tsx",
    ".gen.js",
    ".gen.mjs",
    ".gen.cjs",
];

/// Web front-end asset source extensions — the JS/TS/CSS family. Under an
/// `assets/` pipeline directory these are front-end glue (or a vendored
/// framework client), not the code an agent orients on in a backend repo
/// (audit F9: an Elixir/Phoenix repo surfaced `assets/js/phoenix/*.js` as its
/// "foundational files" and top symbols, hiding every `.ex` controller).
const FRONTEND_ASSET_EXTENSIONS: &[&str] = &[
    "js", "mjs", "cjs", "jsx", "ts", "tsx", "css", "scss", "sass", "less", "vue", "svelte",
];

/// Directory component that marks a front-end asset pipeline.
const FRONTEND_ASSET_DIR: &str = "assets";

/// Returns `true` if `path` is a JS/TS/CSS-family file living under an
/// `assets/` directory — front-end pipeline code that should not dominate a
/// backend repo's nav_map *orientation* sections (`foundational_files`,
/// `symbols`, `entrypoints`). Deliberately scoped to orientation: the file is
/// still fully indexed, queryable, and scanned; it is only kept out of the
/// "what is this repo about" summary. `assets/` is matched as a whole path
/// component, so `src/assets_loader.rs` (no `assets` component, non-asset
/// extension) is unaffected, and a `.ex`/`.rs`/`.py` file that happens to sit
/// under `assets/` is kept (only asset-language extensions match).
pub fn is_frontend_asset_path(path: &str) -> bool {
    let p = Path::new(path);
    let under_assets = p
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .any(|c| c == FRONTEND_ASSET_DIR);
    if !under_assets {
        return false;
    }
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| FRONTEND_ASSET_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

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

/// A line at/above this length is a strong minification signal — hand-written
/// source virtually never sustains lines this long.
const MINIFIED_MAX_LINE: usize = 2000;
/// A line this long counts as "long" for the fraction test below.
const MINIFIED_LONG_LINE: usize = 500;
/// Fraction of lines that must be "long" for the file to read as minified.
/// This distinguishes a genuinely minified/bundled file (predominantly long
/// lines) from ordinary source that merely holds ONE long data/base64 line
/// (a tiny fraction of its lines).
const MINIFIED_LONG_FRACTION: f64 = 0.5;

/// Returns `true` if `source` looks machine-generated/minified from its
/// CONTENT (used at index time to skip a file entirely, unlike the path-based
/// [`is_generated_or_vendored_path`]). Two signals:
///
/// 1. A codegen marker in the first few lines — the `Code generated ... DO NOT
///    EDIT` stamp emitted by protoc, stringer, and many other tools.
/// 2. Minified/bundled layout: a very long max line *and* a high average line
///    length. The audit found a single 490 KB webpack bundle (`chat.js`, not
///    named `*.min.js`) supplying 74% of a Kotlin repo's entities and 93% of
///    its call edges, corrupting `foundational_files`/`symbols`/`hotspots` and
///    the module graph. Requiring both a long max *and* high average avoids
///    false-positiving ordinary source that has one long embedded string.
pub fn is_minified_source(source: &str) -> bool {
    for line in source.lines().take(5) {
        if line.contains("Code generated") && line.contains("DO NOT EDIT") {
            return true;
        }
    }
    let mut max_len = 0usize;
    let mut long_lines = 0usize;
    let mut count = 0usize;
    for line in source.lines() {
        let n = line.len();
        if n > max_len {
            max_len = n;
        }
        if n >= MINIFIED_LONG_LINE {
            long_lines += 1;
        }
        count += 1;
    }
    if count == 0 {
        return false;
    }
    max_len >= MINIFIED_MAX_LINE && (long_lines as f64 / count as f64) >= MINIFIED_LONG_FRACTION
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
    fn true_for_cross_language_audit_generated_files() {
        // Go protoc, codegen `.gen.*`, and Alembic/DB migration revisions —
        // the specific scan false positives found auditing Go/TS/Python repos.
        assert!(is_generated_or_vendored_path(
            "grpc/example1/gen/helloworld/v1/helloworld.pb.go"
        ));
        assert!(is_generated_or_vendored_path(
            "frontend/src/client/sdk.gen.ts"
        ));
        assert!(is_generated_or_vendored_path("src/client/core.gen.js"));
        assert!(is_generated_or_vendored_path(
            "backend/app/alembic/versions/1a31ce608336_init.py"
        ));
        assert!(is_generated_or_vendored_path(
            "db/migrations/versions/abc.py"
        ));
        // Lookalikes that are real source must stay included.
        assert!(!is_generated_or_vendored_path("src/protobuf_helpers.go"));
        assert!(!is_generated_or_vendored_path("src/generator.ts"));
        assert!(!is_generated_or_vendored_path("app/versions/policy.py"));
    }

    #[test]
    fn is_minified_source_detects_generated_and_minified_content() {
        // Codegen marker (protoc/stringer/etc.) in the header.
        assert!(is_minified_source(
            "// Code generated by protoc-gen-go. DO NOT EDIT.\npackage v1\n"
        ));
        // A minified/bundled file: consistently very long lines.
        let bundle = format!("!function(t,n){{{}}}();", "a=1;".repeat(1000));
        assert!(is_minified_source(&bundle));
        // Ordinary source with ONE long embedded string is NOT minified
        // (low average line length).
        let normal = format!(
            "fn main() {{\n    let s = \"{}\";\n    println!(\"{{s}}\");\n}}\n",
            "x".repeat(3000)
        );
        assert!(!is_minified_source(&normal));
        assert!(!is_minified_source("fn a() {}\nfn b() {}\n"));
        assert!(!is_minified_source(""));
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
    fn frontend_asset_paths_are_orientation_noise() {
        // Phoenix vendored JS client + ordinary front-end pipeline code.
        assert!(is_frontend_asset_path("assets/js/phoenix/index.js"));
        assert!(is_frontend_asset_path("assets/js/app.js"));
        assert!(is_frontend_asset_path("assets/css/app.scss"));
        assert!(is_frontend_asset_path("web/assets/components/Nav.tsx"));
        assert!(is_frontend_asset_path("assets/vendor/topbar.js"));
    }

    #[test]
    fn frontend_asset_filter_is_scoped_to_assets_dir_and_web_extensions() {
        // Not under an `assets/` component.
        assert!(!is_frontend_asset_path("src/app.js"));
        assert!(!is_frontend_asset_path("lib/assets_loader.rs"));
        // Under assets/, but a backend-language file — kept for orientation.
        assert!(!is_frontend_asset_path("assets/pipeline.ex"));
        assert!(!is_frontend_asset_path("assets/gen.py"));
        // `assets` as a substring of a component, not the component itself.
        assert!(!is_frontend_asset_path("src/assetsx/app.js"));
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
