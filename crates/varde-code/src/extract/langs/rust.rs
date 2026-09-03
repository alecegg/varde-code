//! Rust entity extraction.
//!
//! Rust-specific mapping notes (fixture-driven, superset-safe):
//! - Function: `fn` items -> Function (function_item and
//!   function_signature_item — the latter covers trait method declarations).
//! - Class: `struct` and `enum` items -> Class (enums are type declarations;
//!   documented mapping).
//! - Interface: `trait` items -> Interface (Rust's abstraction mechanism).
//! - Variable: `let` declarations -> one Variable per pattern identifier
//!   (plain identifiers and destructuring: tuple/struct/tuple-struct
//!   patterns).
//! - Parameter: identifiers inside the `parameters` node (`parameter` carries
//!   a `pattern` field; `self_parameter` -> "self").
//! - Export: `pub` items at the top level -> Export entity alongside their
//!   primary entity (narrow: visibility modifier on a top-level
//!   fn/struct/enum/trait/const item).
//! - Call: `call_expression`, callee text as name. `field_expression` ->
//!   MemberAccess (field "field").
//! - Literal: integer/float/string/char/boolean/raw-string literals,
//!   excluding type contexts.
//! - Catch: carved out — Rust has no try/catch construct (Result-based
//!   error handling is not a syntactic counterpart).
//! - Throw: `panic!` macro invocation -> Throw, named after the first
//!   argument (the panic message).
//! - ControlFlow: if/for/while/loop/match/return/break/continue expressions.
//!   (`if let` / `while let` parse as if_expression / while_expression with a
//!   let_condition — tree-sitter-rust has no separate node kinds.)
//! - Route/Response: carved out — Rust has no built-in web framework shapes
//!   that map to a narrow, non-fuzzy heuristic.

use crate::extract::entity::{EntityMeta, ExtractCtx, entity};
use crate::extract::field_name;
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope.
pub const TYPE_SCOPES: &[&str] = &["impl_item", "trait_item"];

pub const FUNCTION_SCOPES: &[&str] = &[
    "function_item",
    "function_signature_item",
    "closure_expression",
];

/// Entity kinds the fixtures must produce. Catch, Route, and Response are
/// carved out (see module docs).
pub const REQUIRED_KINDS: [EntityKind; 11] = [
    EntityKind::Function,
    EntityKind::Class,
    EntityKind::Interface,
    EntityKind::Variable,
    EntityKind::Parameter,
    EntityKind::Export,
    EntityKind::Call,
    EntityKind::Literal,
    EntityKind::MemberAccess,
    EntityKind::Throw,
    EntityKind::ControlFlow,
];

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- structural ----
        "function_item" | "function_signature_item" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name.clone(), node);
            maybe_export(node, &name, ctx);
        }
        // structs and enums are both type declarations -> Class.
        "struct_item" | "enum_item" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            maybe_export(node, &name, ctx);
        }
        // traits are Rust's abstraction mechanism -> Interface.
        "trait_item" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Interface, name.clone(), node);
            maybe_export(node, &name, ctx);
        }
        // const items have no dedicated entity kind; only the export linkage
        // matters at the top level.
        "const_item" => {
            if let Some(name) = field_name(node) {
                maybe_export(node, &name, ctx);
            }
        }

        // `impl Trait for Type { ... }` -> Implements, linking Type to
        // Trait. Inherent impls (`impl Type { ... }`, no `trait` field) are
        // not a hierarchy relationship and are skipped.
        "impl_item" => {
            if let (Some(trait_node), Some(type_node)) = (node.field("trait"), node.field("type")) {
                let trait_name = impl_type_name(&trait_node);
                let type_name = impl_type_name(&type_node);
                if !trait_name.is_empty() && !type_name.is_empty() {
                    ctx.out.push(entity(
                        EntityKind::Implements,
                        trait_name,
                        ctx.file_id,
                        node,
                        EntityMeta {
                            enclosing: Some(type_name),
                            ..Default::default()
                        },
                    ));
                }
            }
        }

        // ---- imports ----
        // `use` items: one Import entity per statement, named after the
        // module path portion (e.g. "crate::helper::util" -> "crate::helper").
        "use_declaration" => {
            let path = rust_use_info(node);
            let imp = entity(
                EntityKind::Import,
                path,
                ctx.file_id,
                node,
                EntityMeta::default(),
            );
            ctx.out.push(imp);
        }

        // ---- variables ----
        // `let` with a pattern: one Variable per bound identifier.
        "let_declaration" => {
            if let Some(pattern) = node.field("pattern") {
                for name in pattern_identifiers(&pattern) {
                    ctx.push(EntityKind::Variable, name, node);
                }
            }
        }

        // ---- parameters ----
        "parameters" => {
            for child in node.children() {
                match child.kind().as_ref() {
                    "parameter" => {
                        if let Some(pattern) = child.field("pattern") {
                            for name in pattern_identifiers(&pattern) {
                                ctx.push(EntityKind::Parameter, name, node);
                            }
                        }
                    }
                    "self_parameter" => {
                        ctx.push(EntityKind::Parameter, "self".to_string(), node);
                    }
                    _ => {}
                }
            }
        }

        // ---- expression-level ----
        "call_expression" => {
            let name = node
                .field("function")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Call, name, node);
        }
        "field_expression" => {
            let name = node
                .field("field")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::MemberAccess, name, node);
        }
        "integer_literal" | "float_literal" | "string_literal" | "char_literal"
        | "boolean_literal" | "raw_string_literal" => {
            if !in_rust_type_context(node) {
                ctx.push(EntityKind::Literal, node.text().into_owned(), node);
            }
        }
        // `panic!(...)` -> Throw, named after the panic message argument.
        "macro_invocation" => {
            let is_panic = node
                .field("macro")
                .map(|m| m.text() == "panic")
                .unwrap_or(false);
            if is_panic {
                ctx.push(
                    EntityKind::Throw,
                    macro_first_arg(node).unwrap_or_default(),
                    node,
                );
            }
        }

        // ---- control flow ----
        "if_expression"
        | "for_expression"
        | "while_expression"
        | "loop_expression"
        | "match_expression"
        | "return_expression"
        | "break_expression"
        | "continue_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// All bound identifiers in a let/parameter pattern, recursing through
/// tuple/struct/tuple-struct/ref/mut/or patterns and skipping the type name
/// of struct-like patterns.
fn pattern_identifiers(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    collect_pattern_ids(node, &mut names, 0);
    names
}

fn collect_pattern_ids(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    names: &mut Vec<String>,
    depth: u32,
) {
    // Guard against stack overflow on pathologically nested patterns; a real
    // pattern is never anywhere near this deep, so bailing loses nothing.
    if depth >= crate::extract::MAX_WALK_DEPTH {
        return;
    }
    match node.kind().as_ref() {
        "identifier" | "shorthand_field_identifier" => {
            let t = node.text().into_owned();
            if t != "_" {
                names.push(t);
            }
        }
        // Skip the type name (`Some(v)` -> only `v`; `Point { x }` -> only
        // `x`); the struct's name lives on the Class entity instead.
        "tuple_struct_pattern" | "struct_pattern" => {
            for child in node.children() {
                let is_type = node
                    .field("type")
                    .map(|t| t.node_id() == child.node_id())
                    .unwrap_or(false);
                if !is_type {
                    collect_pattern_ids(&child, names, depth + 1);
                }
            }
        }
        _ => {
            for child in node.children() {
                collect_pattern_ids(&child, names, depth + 1);
            }
        }
    }
}

/// Rust `pub` items at the top level emit an Export entity alongside their
/// primary entity (narrow: parent must be the source file).
fn maybe_export(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    name: &str,
    ctx: &mut ExtractCtx,
) {
    let top_level = node
        .parent()
        .map(|p| p.kind() == "source_file")
        .unwrap_or(false);
    let is_pub = node.children().any(|c| c.kind() == "visibility_modifier");
    if top_level && is_pub {
        ctx.out.push(entity(
            EntityKind::Export,
            name.to_string(),
            ctx.file_id,
            node,
            EntityMeta::default(),
        ));
    }
}

/// Base type name of an `impl_item`'s `trait`/`type` field: unwraps a
/// `generic_type` (`Wrapper<T>` -> "Wrapper") to its base; otherwise the
/// node's own text (covers `type_identifier` and `scoped_type_identifier`,
/// e.g. `std::fmt::Display`).
fn impl_type_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    if node.kind() == "generic_type" {
        node.field("type")
            .map(|t| t.text().into_owned())
            .unwrap_or_else(|| node.text().into_owned())
    } else {
        node.text().into_owned()
    }
}

/// Owning-type name for a `TYPE_SCOPES` node: an `impl_item`'s `type` field
/// (the concrete type being implemented, not the `trait` field — methods
/// inside `impl Trait for Type` belong to `Type`), or `trait_item`'s `name`
/// field for trait default-method bodies.
pub(crate) fn type_scope_name(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    match node.kind().as_ref() {
        "impl_item" => node.field("type").map(|t| impl_type_name(&t)),
        _ => field_name(node),
    }
}

/// First argument text of a `panic!(...)`-style macro invocation: the first
/// non-punctuation token inside the token_tree.
fn macro_first_arg(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let tree = node.children().find(|c| c.kind() == "token_tree")?;
    tree.children()
        .find(|c| !matches!(c.kind().as_ref(), "(" | ")" | "[" | "]" | "{" | "}" | ","))
        .map(|c| c.text().into_owned())
}

/// Rust node kinds that mark the start of a type context (primitive/generic/
/// array/reference/function types, a bare or scoped type name, a lifetime,
/// ...). Rust's grammar names these differently from the shared TS/JS-centric
/// `crate::extract::TYPE_KINDS` list (review fix M4) — kept here as the single
/// source of truth for Rust and dispatched to by `crate::extract::is_type_kind`
/// so the language-agnostic `in_type` propagation (which feeds symbol
/// classification — see `crate::extract::symbol::classify`) recognizes Rust
/// type contexts too, not just this module's own literal-suppression check.
pub(crate) const TYPE_KINDS: &[&str] = &[
    "primitive_type",
    "generic_type",
    "generic_type_with_turbofish",
    "array_type",
    "tuple_type",
    "reference_type",
    "pointer_type",
    "function_type",
    "unit_type",
    "scoped_type_identifier",
    "type_identifier",
    "type_arguments",
    "type_parameters",
    "lifetime",
];

/// True when `kind` is a Rust type-context node kind. See [`TYPE_KINDS`].
pub(crate) fn is_type_kind(kind: &str) -> bool {
    TYPE_KINDS.contains(&kind)
}

/// True when the node sits inside a Rust type context (parameter type,
/// return type, array/generic/reference/function types, ...). Prevents type
/// spellings like `[u8; 4]` from surfacing as expression literals.
fn in_rust_type_context(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    node.ancestors()
        .skip(1)
        .any(|a| is_type_kind(a.kind().as_ref()))
}

/// Extract the module path from a Rust `use` statement.
///
/// AST shapes (tree-sitter-rust):
/// - `use crate::helper::util;`      -> scoped_identifier, name "crate::helper::util"
/// - `use crate::helper::{a, b};`    -> scoped_use_list, name "crate::helper"
/// - `use foo;` / `use foo as bar;`  -> identifier / use_as_clause
/// - `use std::collections::*;`      -> scoped_identifier + use_wildcard
fn rust_use_info(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let arg = node.children().find(|c| {
        matches!(
            c.kind().as_ref(),
            "scoped_identifier"
                | "scoped_use_list"
                | "use_list"
                | "identifier"
                | "use_as_clause"
                | "use_wildcard"
        )
    });
    let Some(arg) = arg else {
        return node.text().into_owned();
    };
    match arg.kind().as_ref() {
        "scoped_use_list" => arg
            .children()
            .find(|c| c.kind() == "scoped_identifier")
            .map(|c| c.text().into_owned())
            .unwrap_or_default(),
        "use_list" => String::new(),
        // `use foo as bar;` — the module path is the first named child ("foo").
        "use_as_clause" => arg
            .children()
            .find(|c| c.is_named())
            .map(|c| c.text().into_owned())
            .unwrap_or_default(),
        _ => arg.text().into_owned(),
    }
}
