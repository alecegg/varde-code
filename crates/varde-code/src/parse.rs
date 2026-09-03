//! Tree-sitter parsing via `ast-grep-language`.
//!
//! Grammar loading/registration is delegated to `ast-grep-language` (per the
//! plan: direct dependency, no reimplementation). Parsing is an unconditional
//! full-tree walk — `ast-grep-core`'s pattern engine is deliberately not used
//! for this hot path.

use anyhow::{Context, Result};
use ast_grep_core::AstGrep;
use ast_grep_core::language::Language;
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;
use std::path::Path;

/// Map a file extension to its ast-grep language. `None` for unsupported
/// extensions (binary, unknown, etc.).
pub fn language_for_path(path: &Path) -> Option<SupportLang> {
    // Match the raw extension bytes case-insensitively (no lowercase String
    // allocation per file).
    let ext = path.extension()?.to_str()?;
    let lang = if ext.eq_ignore_ascii_case("ts")
        || ext.eq_ignore_ascii_case("mts")
        || ext.eq_ignore_ascii_case("cts")
    {
        SupportLang::TypeScript
    } else if ext.eq_ignore_ascii_case("tsx") {
        SupportLang::Tsx
    } else if ext.eq_ignore_ascii_case("js")
        || ext.eq_ignore_ascii_case("jsx")
        || ext.eq_ignore_ascii_case("mjs")
        || ext.eq_ignore_ascii_case("cjs")
    {
        SupportLang::JavaScript
    } else if ext.eq_ignore_ascii_case("c") || ext.eq_ignore_ascii_case("h") {
        // `.h` is treated as C by convention. C++ projects that use `.h` for
        // headers still parse — the C++ grammar is a near-superset, so a C
        // parse of a C++ header degrades gracefully rather than failing; the
        // dedicated C++ extensions below cover the unambiguous cases.
        SupportLang::C
    } else if ext.eq_ignore_ascii_case("cpp")
        || ext.eq_ignore_ascii_case("cc")
        || ext.eq_ignore_ascii_case("cxx")
        || ext.eq_ignore_ascii_case("c++")
        || ext.eq_ignore_ascii_case("hpp")
        || ext.eq_ignore_ascii_case("hh")
        || ext.eq_ignore_ascii_case("hxx")
        || ext.eq_ignore_ascii_case("h++")
    {
        SupportLang::Cpp
    } else if ext.eq_ignore_ascii_case("go") {
        SupportLang::Go
    } else if ext.eq_ignore_ascii_case("java") {
        SupportLang::Java
    } else if ext.eq_ignore_ascii_case("cs") {
        SupportLang::CSharp
    } else if ext.eq_ignore_ascii_case("kt") || ext.eq_ignore_ascii_case("kts") {
        SupportLang::Kotlin
    } else if ext.eq_ignore_ascii_case("swift") {
        SupportLang::Swift
    } else if ext.eq_ignore_ascii_case("py") {
        SupportLang::Python
    } else if ext.eq_ignore_ascii_case("rb") {
        SupportLang::Ruby
    } else if ext.eq_ignore_ascii_case("php") {
        SupportLang::Php
    } else if ext.eq_ignore_ascii_case("lua") {
        SupportLang::Lua
    } else if ext.eq_ignore_ascii_case("scala")
        || ext.eq_ignore_ascii_case("sc")
        || ext.eq_ignore_ascii_case("sbt")
    {
        SupportLang::Scala
    } else if ext.eq_ignore_ascii_case("dart") {
        SupportLang::Dart
    } else if ext.eq_ignore_ascii_case("ex") || ext.eq_ignore_ascii_case("exs") {
        SupportLang::Elixir
    } else if ext.eq_ignore_ascii_case("sol") {
        SupportLang::Solidity
    } else if ext.eq_ignore_ascii_case("hs") {
        SupportLang::Haskell
    } else if ext.eq_ignore_ascii_case("sh")
        || ext.eq_ignore_ascii_case("bash")
        || ext.eq_ignore_ascii_case("zsh")
        || ext.eq_ignore_ascii_case("ksh")
        || ext.eq_ignore_ascii_case("bats")
    {
        // Shell scripts (bash/sh/zsh/ksh/bats) all parse against the
        // tree-sitter-bash grammar; ast-grep's own extension list groups them
        // the same way.
        SupportLang::Bash
    } else if ext.eq_ignore_ascii_case("rs") {
        SupportLang::Rust
    } else {
        return None;
    };
    Some(lang)
}

/// Map a path to a language across the *full* `ast-grep` grammar set, not just
/// the extraction-supported subset in [`language_for_path`]. Structural
/// `find_pattern` search only needs a grammar to parse against — no hand-written
/// entity extractor — so it recognizes every language `ast-grep-language` links
/// (e.g. C/C++, PHP, HCL/Terraform), while the indexing pipeline stays
/// restricted to [`SUPPORTED_LANGUAGES`]. Kept as a distinct function so the two
/// mappings can diverge on purpose.
pub(crate) fn any_language_for_path(path: &Path) -> Option<SupportLang> {
    SupportLang::from_path(path)
}

/// Every language the extractor/parser supports, in a stable order. Used by
/// the pattern-rule pipeline's language-agnostic default ("run against every
/// file whose parse succeeds"). A slice (not a fixed-size array) so adding a
/// language is a one-line append with no size bump.
pub const SUPPORTED_LANGUAGES: &[SupportLang] = &[
    SupportLang::TypeScript,
    SupportLang::Tsx,
    SupportLang::JavaScript,
    SupportLang::C,
    SupportLang::Cpp,
    SupportLang::Go,
    SupportLang::Java,
    SupportLang::CSharp,
    SupportLang::Kotlin,
    SupportLang::Swift,
    SupportLang::Python,
    SupportLang::Ruby,
    SupportLang::Php,
    SupportLang::Scala,
    SupportLang::Dart,
    SupportLang::Lua,
    SupportLang::Elixir,
    SupportLang::Solidity,
    SupportLang::Haskell,
    SupportLang::Bash,
    SupportLang::Rust,
];

/// Resolve a rule/CLI language name to its `SupportLang`. Accepts the
/// canonical names plus common aliases (`ts`, `js`, `py`, `cs`).
pub fn language_from_name(name: &str) -> Option<SupportLang> {
    use SupportLang::*;
    Some(match name {
        "rust" => Rust,
        "typescript" | "ts" => TypeScript,
        "tsx" => Tsx,
        "javascript" | "js" => JavaScript,
        "c" => C,
        "cpp" | "c++" | "cxx" => Cpp,
        "go" | "golang" => Go,
        "java" => Java,
        "csharp" | "cs" => CSharp,
        "kotlin" | "kt" => Kotlin,
        "swift" => Swift,
        "python" | "py" => Python,
        "ruby" | "rb" => Ruby,
        "php" => Php,
        "lua" => Lua,
        "scala" => Scala,
        "dart" => Dart,
        "elixir" | "ex" => Elixir,
        "solidity" | "sol" => Solidity,
        "haskell" | "hs" => Haskell,
        "bash" | "sh" | "shell" => Bash,
        _ => return None,
    })
}

/// A parsed source file: the ast-grep root plus a syntax-error flag.
pub struct ParsedFile {
    pub lang: SupportLang,
    pub root: AstGrep<StrDoc<SupportLang>>,
}

impl ParsedFile {
    /// True when the tree contains ERROR/MISSING nodes (syntax error).
    ///
    /// Lazily walks the tree, so the build path never pays for it — the merged
    /// extract walk folds `has_error` detection into its single traversal. Only
    /// the query path (`find_pattern`, which parses without extracting) calls
    /// this.
    pub fn has_error(&self) -> bool {
        self.root.root().get_inner_node().has_error()
    }
}

/// Parse a source string with the given language. `ast-grep-language` grammar
/// loading cannot fail at runtime; syntax errors surface as ERROR nodes in the
/// tree, which we surface via `has_error` instead of panicking.
pub fn parse_source(lang: &SupportLang, source: &str) -> ParsedFile {
    // tree-sitter's lexer reserves byte 0x00 as an internal end-of-input
    // sentinel, so a literal NUL inside otherwise-valid source (e.g. in a
    // string/template literal) produces a spurious ERROR node. Replace with
    // a same-length placeholder to preserve byte offsets.
    let source = if source.as_bytes().contains(&0) {
        std::borrow::Cow::Owned(source.replace('\0', " "))
    } else {
        std::borrow::Cow::Borrowed(source)
    };
    let root = lang.ast_grep(source.as_ref());
    ParsedFile { lang: *lang, root }
}

/// Parse a file on disk. Returns `Ok(None)` for unsupported extensions.
pub fn parse_file(path: &Path) -> Result<Option<ParsedFile>> {
    let Some(lang) = language_for_path(path) else {
        return Ok(None);
    };
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(parse_source(&lang, &source)))
}
