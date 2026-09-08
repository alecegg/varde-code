//! Per-language entity extraction.
//!
//! The tree walker in `super::entity` calls into this module for every node;
//! each language module maps its tree-sitter node kinds to the shared 14-kind
//! entity checklist. The walker owns recursion, the enclosing-function stack,
//! and span/name plumbing.

pub mod bash;
pub mod c;
pub mod cpp;
pub mod cs;
pub mod dart;
pub mod elixir;
pub mod go;
pub mod haskell;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod lua;
pub mod php;
pub mod python;
pub mod role_tags;
pub mod ruby;
pub mod rust;
pub mod scala;
pub mod solidity;
pub mod swift;
pub mod ts;
pub mod tsx;

use crate::extract::entity::ExtractCtx;
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Emit entities for `node` (called for every node in the tree). Languages
/// without a visitor yet match nothing.
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match ctx.lang {
        SupportLang::TypeScript => ts::visit(node, kind, ctx),
        SupportLang::Tsx => tsx::visit(node, kind, ctx),
        SupportLang::JavaScript => javascript::visit(node, kind, ctx),
        SupportLang::C => c::visit(node, kind, ctx),
        SupportLang::Cpp => cpp::visit(node, kind, ctx),
        SupportLang::Go => go::visit(node, kind, ctx),
        SupportLang::Java => java::visit(node, kind, ctx),
        SupportLang::CSharp => cs::visit(node, kind, ctx),
        SupportLang::Dart => dart::visit(node, kind, ctx),
        SupportLang::Elixir => elixir::visit(node, kind, ctx),
        SupportLang::Kotlin => kotlin::visit(node, kind, ctx),
        SupportLang::Swift => swift::visit(node, kind, ctx),
        SupportLang::Python => python::visit(node, kind, ctx),
        SupportLang::Php => php::visit(node, kind, ctx),
        SupportLang::Lua => lua::visit(node, kind, ctx),
        SupportLang::Ruby => ruby::visit(node, kind, ctx),
        SupportLang::Rust => rust::visit(node, kind, ctx),
        SupportLang::Scala => scala::visit(node, kind, ctx),
        SupportLang::Solidity => solidity::visit(node, kind, ctx),
        SupportLang::Haskell => haskell::visit(node, kind, ctx),
        SupportLang::Bash => bash::visit(node, kind, ctx),
        _ => {}
    }
}

/// Strip generic type arguments (`Comparable<Foo>` -> `"Comparable"`) so the
/// name matches the bare declared name `resolve_type_hierarchy` looks up
/// (CORRECTNESS-002). Textual, not grammar-field-based: taking everything
/// before the first `<` is enough since identifiers in these languages can't
/// contain `<`.
pub fn strip_generic_args(name: &str) -> String {
    match name.find('<') {
        Some(i) => name[..i].trim().to_string(),
        None => name.trim().to_string(),
    }
}

/// Strip one layer of matching quote characters from a string-literal node's
/// raw source text (`"foo"` / `'foo'` -> `foo`). `allow_backtick` enables the
/// third quote kind used by JS/TS/Go template/raw strings; other languages'
/// grammars have no backtick string literal, so passing `false` there means a
/// literal `` `foo` `` (which can't occur) is never mistaken for a quoted
/// literal. Unmatched or too-short input is returned trimmed but otherwise
/// unchanged.
pub fn unquote(text: &str, allow_backtick: bool) -> String {
    let t = text.trim();
    if t.len() >= 2 {
        let bytes = t.as_bytes();
        let quoted = (bytes[0] == b'"' && bytes[t.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[t.len() - 1] == b'\'')
            || (allow_backtick && bytes[0] == b'`' && bytes[t.len() - 1] == b'`');
        if quoted {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// Whether a function entity's node carries its language's async modifier.
///
/// The modifier keyword sits at different depths per language's tree-sitter
/// grammar: a direct `async` token child (JS/TS/TSX, Python), a
/// `function_modifiers` wrapper containing the `async` token (Rust), a
/// `modifier` child whose text is `async` (C#), or a `modifiers` wrapper
/// holding a `function_modifier` whose text is `suspend` (Kotlin's coroutine
/// analog). Go, Java, and Swift have no such construct and always report
/// `false`. The check is deliberately scoped to the node's immediate
/// modifier region so a nested `async` block inside a function body can
/// never false-positive.
pub fn node_is_async(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    node.children().any(|child| match child.kind().as_ref() {
        "async" => true,
        "modifier" => {
            child.text().as_ref().trim() == "async" || child.text().as_ref().trim() == "suspend"
        }
        "function_modifiers" | "modifiers" => modifier_wrapper_has_async(&child),
        _ => false,
    })
}

/// Whether a modifier wrapper node (Rust `function_modifiers`, Kotlin
/// `modifiers`) contains the async/suspend keyword one level down.
fn modifier_wrapper_has_async(wrapper: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    wrapper.children().any(|child| {
        child.kind().as_ref() == "async"
            || matches!(child.kind().as_ref(), "function_modifier" | "modifier")
                && matches!(child.text().as_ref().trim(), "async" | "suspend")
    })
}

/// Whether a function entity's node is a test function, based on a preceding
/// attribute (Rust `#[test]`, `#[tokio::test]`, `#[rstest]`, etc.).
///
/// Only Rust is handled today (this repo's own codebase is Rust-only); other
/// languages always report `false`. Rust attributes are sibling
/// `attribute_item` nodes preceding the function in the source, not children
/// of the function node itself, so this walks backward over immediately
/// preceding siblings (stopping at the first non-attribute, non-comment
/// node) and checks each attribute's macro path.
pub fn node_is_test(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    node.prev_all()
        .take_while(|sib| {
            matches!(
                sib.kind().as_ref(),
                "attribute_item" | "line_comment" | "block_comment"
            )
        })
        .any(|sib| {
            sib.kind().as_ref() == "attribute_item" && attribute_is_test(sib.text().as_ref())
        })
}

/// Whether a Rust attribute's macro path names a test attribute: bare
/// `#[test]`, a namespaced `#[tokio::test]`/`#[async_std::test]` (last path
/// segment `test`), or `#[rstest]`.
///
/// Matched on the path token itself, not a raw substring of the attribute's
/// source text — `.contains("test")` previously false-positived on
/// `#[cfg(feature = "fastest")]` and `#[attest]`, silently excluding real
/// production functions from scan. The path is everything before the first
/// `(` or `=` (an attribute's optional argument list/value), so
/// `#[test_case(1, 2)]`-style arguments never leak into the match.
fn attribute_is_test(attr_text: &str) -> bool {
    let inner = attr_text
        .trim()
        .trim_start_matches('#')
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let path = inner.split(['(', '=']).next().unwrap_or("").trim();
    path.rsplit("::").next() == Some("test") || path == "rstest"
}

/// Node kinds that introduce a named function scope (for `enclosing_function`
/// linkage on control-flow/error entities).
pub fn function_scopes(lang: SupportLang) -> &'static [&'static str] {
    match lang {
        SupportLang::TypeScript | SupportLang::Tsx => ts::FUNCTION_SCOPES,
        SupportLang::JavaScript => javascript::FUNCTION_SCOPES,
        SupportLang::C => c::FUNCTION_SCOPES,
        SupportLang::Cpp => cpp::FUNCTION_SCOPES,
        SupportLang::Go => go::FUNCTION_SCOPES,
        SupportLang::Java => java::FUNCTION_SCOPES,
        SupportLang::CSharp => cs::FUNCTION_SCOPES,
        SupportLang::Dart => dart::FUNCTION_SCOPES,
        SupportLang::Kotlin => kotlin::FUNCTION_SCOPES,
        SupportLang::Swift => swift::FUNCTION_SCOPES,
        SupportLang::Python => python::FUNCTION_SCOPES,
        SupportLang::Php => php::FUNCTION_SCOPES,
        SupportLang::Lua => lua::FUNCTION_SCOPES,
        SupportLang::Ruby => ruby::FUNCTION_SCOPES,
        SupportLang::Rust => rust::FUNCTION_SCOPES,
        SupportLang::Scala => scala::FUNCTION_SCOPES,
        SupportLang::Solidity => solidity::FUNCTION_SCOPES,
        SupportLang::Haskell => haskell::FUNCTION_SCOPES,
        SupportLang::Bash => bash::FUNCTION_SCOPES,
        _ => &[],
    }
}

/// Whether this syntax node introduces a function scope.
pub fn is_function_scope(
    lang: SupportLang,
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
) -> bool {
    match lang {
        SupportLang::CSharp => cs::is_function_scope(node, kind),
        _ => function_scopes(lang).contains(&kind),
    }
}

/// Stable name for the function scope introduced by `node`.
///
/// Most grammars expose a declaration's own `name` field. Accessors need
/// their containing member added because their direct name is an operation.
pub fn function_scope_name(
    lang: SupportLang,
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    match lang {
        SupportLang::CSharp => cs::function_scope_name(node),
        SupportLang::Java => java::function_scope_name(node),
        SupportLang::Kotlin => kotlin::function_scope_name(node),
        SupportLang::Swift => swift::function_scope_name(node),
        _ => crate::extract::field_name(node),
    }
}

/// Node kinds that introduce a named class/interface/impl-target type scope
/// (for `Entity::owner_type` linkage on methods — see `super::entity`).
///
/// Two languages set `owner_type` outside this brace-nesting mechanism and so
/// map to `&[]` here:
/// - Go: methods are declared at file scope (`func (r Foo) Bar()`), not nested
///   inside the type, so `go.rs` reads the receiver type off the declaration
///   directly. (Interface satisfaction is also structural — no `implements`
///   syntax to key a scope off of.)
/// - Elixir: `def` and `defmodule` are both `call` kind and indistinguishable
///   by node kind, so `elixir.rs` resolves the enclosing module by walking
///   ancestors.
pub fn type_scopes(lang: SupportLang) -> &'static [&'static str] {
    match lang {
        SupportLang::TypeScript => ts::TYPE_SCOPES,
        SupportLang::Tsx => tsx::TYPE_SCOPES,
        SupportLang::JavaScript => javascript::TYPE_SCOPES,
        SupportLang::C => c::TYPE_SCOPES,
        SupportLang::Cpp => cpp::TYPE_SCOPES,
        SupportLang::Java => java::TYPE_SCOPES,
        SupportLang::CSharp => cs::TYPE_SCOPES,
        SupportLang::Dart => dart::TYPE_SCOPES,
        SupportLang::Kotlin => kotlin::TYPE_SCOPES,
        SupportLang::Swift => swift::TYPE_SCOPES,
        SupportLang::Php => php::TYPE_SCOPES,
        SupportLang::Python => python::TYPE_SCOPES,
        SupportLang::Ruby => ruby::TYPE_SCOPES,
        SupportLang::Rust => rust::TYPE_SCOPES,
        SupportLang::Scala => scala::TYPE_SCOPES,
        SupportLang::Solidity => solidity::TYPE_SCOPES,
        SupportLang::Haskell => haskell::TYPE_SCOPES,
        _ => &[],
    }
}

/// Name of the type introduced by a `type_scopes` node. Defaults to the
/// node's `name` field (covers `class_declaration`/`interface_declaration`/
/// `protocol_declaration` uniformly); Rust's `impl_item` has no `name` field
/// so it delegates to `rust::type_scope_name`, and Kotlin's `class_declaration`
/// names its type with a bare `type_identifier` child (no `name` field) so it
/// delegates to `kotlin::type_scope_name`.
pub fn type_scope_name(
    lang: SupportLang,
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    match lang {
        SupportLang::Rust => rust::type_scope_name(node),
        SupportLang::Kotlin => kotlin::type_scope_name(node),
        _ => crate::extract::field_name(node),
    }
}

/// Entity kinds a language's fixtures must produce (checked by the
/// coverage-parity harness). Defaults to all 14; languages that cannot express
/// a kind (e.g. interfaces in Python) carve it out here.
pub fn required_kinds(lang: SupportLang) -> Vec<EntityKind> {
    use EntityKind::*;
    let all = vec![
        Function,
        Class,
        Interface,
        Variable,
        Parameter,
        Export,
        Call,
        Literal,
        MemberAccess,
        Catch,
        Throw,
        ControlFlow,
        Route,
        Response,
    ];
    match lang {
        SupportLang::TypeScript | SupportLang::Tsx => ts::REQUIRED_KINDS.to_vec(),
        SupportLang::JavaScript => javascript::REQUIRED_KINDS.to_vec(),
        SupportLang::C => c::REQUIRED_KINDS.to_vec(),
        SupportLang::Cpp => cpp::REQUIRED_KINDS.to_vec(),
        SupportLang::Go => go::REQUIRED_KINDS.to_vec(),
        SupportLang::Java => java::REQUIRED_KINDS.to_vec(),
        SupportLang::CSharp => cs::REQUIRED_KINDS.to_vec(),
        SupportLang::Dart => dart::REQUIRED_KINDS.to_vec(),
        SupportLang::Elixir => elixir::REQUIRED_KINDS.to_vec(),
        SupportLang::Kotlin => kotlin::REQUIRED_KINDS.to_vec(),
        SupportLang::Swift => swift::REQUIRED_KINDS.to_vec(),
        SupportLang::Python => python::REQUIRED_KINDS.to_vec(),
        SupportLang::Php => php::REQUIRED_KINDS.to_vec(),
        SupportLang::Lua => lua::REQUIRED_KINDS.to_vec(),
        SupportLang::Ruby => ruby::REQUIRED_KINDS.to_vec(),
        SupportLang::Rust => rust::REQUIRED_KINDS.to_vec(),
        SupportLang::Scala => scala::REQUIRED_KINDS.to_vec(),
        SupportLang::Solidity => solidity::REQUIRED_KINDS.to_vec(),
        SupportLang::Haskell => haskell::REQUIRED_KINDS.to_vec(),
        SupportLang::Bash => bash::REQUIRED_KINDS.to_vec(),
        _ => all,
    }
}

// --- Shared node helpers -------------------------------------------------
//
// These are grammar-agnostic node utilities that several language extractors
// use verbatim. Language-specific variants (e.g. Kotlin/Swift resolve call
// arguments through `call_suffix`/`value_arguments`, so they keep their own
// `first_arg_text`) stay in their own module.

/// Push a type-reference [`Entity`] (`kind`, `name`) owned by `owner`, spanning
/// `node`. Used by the class-like extractors (cs/java/kotlin/swift/ts) to emit
/// supertype / implemented-interface references.
pub(super) fn push_type_ref(
    ctx: &mut ExtractCtx,
    kind: EntityKind,
    name: String,
    owner: &str,
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) {
    ctx.out.push(Entity {
        kind,
        name,
        file_id: ctx.file_id,
        span: crate::extract::span_of(node),
        enclosing_function: Some(owner.to_string()),
        method: None,
        path: None,
        status: None,
        body_shape: None,
        body_minhash: None,
        is_async: None,
        is_test: false,
        owner_type: None,
    });
}

/// Text of the first positional argument of a call `node` whose arguments live
/// under a conventional `arguments` field delimited by `(` `,` `)` tokens
/// (cs/go/java/javascript/python/ts). Kotlin/Swift use a different grammar and
/// keep their own variant.
pub(super) fn first_arg_text(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    let args = node.field("arguments")?;
    args.children()
        .find(|c| !matches!(c.kind().as_ref(), "(" | "," | ")"))
        .map(|c| c.text().into_owned())
}

/// Text of the last positional argument of a call `node` when that argument
/// is a bare identifier. Used to recover a route's handler function name from
/// a registration call — `app.get("/x", handler)` / `r.GET("/x", handler)` /
/// `mux.HandleFunc("/x", handler)` all pass the handler as the final argument
/// (any middleware precedes it). Returns `None` for an inline closure/arrow
/// handler or a member expression (`h.List`), which name no single resolvable
/// function. Relies on the shared `arguments` field + `identifier` node kind
/// that both the Go and JS/TS grammars use.
pub(super) fn last_arg_identifier(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    let args = node.field("arguments")?;
    args.children()
        .filter(|c| c.is_named())
        .last()
        .filter(|c| c.kind() == "identifier")
        .map(|c| c.text().into_owned())
}

/// Name of a JS/TS decorator `node` (`@Foo` -> `"Foo"`, `@Foo(...)` -> `"Foo"`).
pub(super) fn decorator_name(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    let inner = node.children().find(|c| c.kind() != "@")?;
    if inner.kind() == "call_expression" {
        inner.field("function").map(|f| f.text().into_owned())
    } else {
        Some(inner.text().into_owned())
    }
}

/// Map a framework route annotation/decorator `name` to its concrete HTTP
/// verb, or `None` for a *prefix-only* annotation whose verb is unknown or
/// carried elsewhere (`RequestMapping`, `Route`, `Controller`, Flask
/// `*.route`). Recognizes, on the last dotted/`::` segment:
///   - Spring/JVM `*Mapping` (`GetMapping` → `GET`; `RequestMapping` → `None`),
///   - ASP.NET `Http*` attributes (`HttpGet` → `GET`),
///   - JAX-RS / NestJS bare verbs (`GET`, `Get`, `Post`, ...),
///   - Express/FastAPI receiver-dotted verbs (`app.get`/`router.post` → the
///     verb; `app.route` → `None`).
///
/// Used by the per-language extractors to stamp `method` onto a route-carrying
/// `Decorator` entity so [`crate::query::entrypoints::detect`] can surface the
/// handler as `"<VERB> <path>"`.
pub(super) fn http_verb_for_annotation(name: &str) -> Option<&'static str> {
    let seg = name.rsplit(['.', ':']).next().unwrap_or(name);
    let core = seg
        .strip_prefix("Http")
        .or_else(|| seg.strip_suffix("Mapping"))
        .unwrap_or(seg);
    match core.to_ascii_uppercase().as_str() {
        "GET" => Some("GET"),
        "POST" => Some("POST"),
        "PUT" => Some("PUT"),
        "DELETE" => Some("DELETE"),
        "PATCH" => Some("PATCH"),
        "HEAD" => Some("HEAD"),
        "OPTIONS" => Some("OPTIONS"),
        _ => None,
    }
}

/// Whether a route annotation/decorator `name` (last segment) is a *prefix*
/// annotation — one that contributes a base path shared by the sibling actions
/// of its class rather than defining a route itself (`@RequestMapping("/api")`,
/// `[Route("api/[controller]")]`, NestJS `@Controller("cats")`). The extractor
/// stamps its path onto the class's `Decorator` so `detect` can prepend it to
/// each action's method-level path.
pub(super) fn is_route_prefix_annotation(name: &str) -> bool {
    let seg = name.rsplit(['.', ':']).next().unwrap_or(name);
    matches!(seg, "RequestMapping" | "Route" | "Controller")
}

/// Walk a JS/TS member-call chain from `fn_node` looking for a `.status(<arg>)`
/// call, returning its first argument's text (the HTTP status literal).
pub(super) fn chain_status(
    fn_node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    let mut cur = fn_node.clone();
    loop {
        let obj = cur.field("object")?;
        if obj.kind() != "call_expression" {
            return None;
        }
        let inner_fn = obj.field("function")?;
        if inner_fn.kind() != "member_expression" {
            return None;
        }
        let prop = inner_fn.field("property")?.text();
        if prop == "status" {
            return first_arg_text(&obj);
        }
        cur = inner_fn;
    }
}

#[cfg(test)]
mod node_is_test_tests {
    use super::*;

    /// `is_test` flags for each top-level function in `source`, keyed by name
    /// — exercises the full parse -> extract path (not just
    /// `attribute_is_test` in isolation) so a regression in either the
    /// attribute-path matcher or its sibling-walk plumbing would be caught.
    fn is_test_flags(source: &str) -> std::collections::HashMap<String, bool> {
        let parsed = crate::parse::parse_source(&SupportLang::Rust, source);
        let result = crate::extract::extract(&parsed, 0);
        result
            .entities
            .into_iter()
            .filter(|e| e.kind == EntityKind::Function)
            .map(|e| (e.name, e.is_test))
            .collect()
    }

    #[test]
    fn recognizes_bare_and_namespaced_test_attributes() {
        let flags = is_test_flags(
            "#[test]\nfn a() {}\n#[tokio::test]\nasync fn b() {}\n#[rstest]\nfn c() {}\n",
        );
        assert!(flags["a"]);
        assert!(flags["b"]);
        assert!(flags["c"]);
    }

    /// Regression (review fix M3): `.contains("test")` on the raw attribute
    /// text previously flagged these as tests. Matching the attribute's
    /// macro path instead of a substring must not.
    #[test]
    fn does_not_false_positive_on_test_substring_in_unrelated_attributes() {
        let flags =
            is_test_flags("#[cfg(feature = \"fastest\")]\nfn a() {}\n#[attest]\nfn b() {}\n");
        assert!(!flags["a"]);
        assert!(!flags["b"]);
    }

    #[test]
    fn plain_function_is_not_a_test() {
        let flags = is_test_flags("fn a() {}\n");
        assert!(!flags["a"]);
    }
}
