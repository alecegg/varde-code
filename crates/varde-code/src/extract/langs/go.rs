//! Go entity extraction.
//!
//! Go-specific mapping notes (fixture-driven, superset-safe):
//! - `func` -> Function (function_declaration and method_declaration).
//! - `type X struct {}` -> Class; `type X interface {}` -> Interface.
//! - var/const/short-var declarators -> Variable (one per name).
//! - Exports: Go has no export statement; exported symbols are capitalized
//!   package-level names, so a capitalized top-level declaration name yields
//!   an Export entity alongside its primary entity.
//! - Catch/Throw: Go's error handling is explicit — `panic()` maps to Throw
//!   and `recover()` maps to Catch (closest idiomatic equivalents).
//! - Route/Response: net/http shapes — `mux.HandleFunc("/path", h)` -> Route;
//!   `w.WriteHeader(n)` / `w.Write(...)` -> Response.

use crate::extract::entity::{EntityMeta, ExtractCtx, entity};
use crate::extract::field_name;
use crate::extract::langs::first_arg_text;
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

pub const FUNCTION_SCOPES: &[&str] = &[
    "function_declaration",
    "method_declaration",
    "function_literal",
];

pub const REQUIRED_KINDS: [EntityKind; 14] = [
    EntityKind::Function,
    EntityKind::Class,
    EntityKind::Interface,
    EntityKind::Variable,
    EntityKind::Parameter,
    EntityKind::Export,
    EntityKind::Call,
    EntityKind::Literal,
    EntityKind::MemberAccess,
    EntityKind::Catch,
    EntityKind::Throw,
    EntityKind::ControlFlow,
    EntityKind::Route,
    EntityKind::Response,
];

pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- structural ----
        "function_declaration" | "method_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name.clone(), node);
            maybe_export(node, &name, ctx);
        }
        // ---- imports ----
        // `import "fmt"` or a grouped `import (...)` block both parse down to
        // one `import_spec` per imported package; the `path` field carries
        // the quoted import path (e.g. "fmt", "github.com/foo/bar").
        "import_spec" => {
            if let Some(path) = node.field("path") {
                let spec = super::unquote(path.text().as_ref(), true);
                ctx.push(EntityKind::Import, spec, node);
            }
        }

        "type_spec" => {
            let name = field_name(node).unwrap_or_default();
            let struct_type = node.children().find(|c| c.kind() == "struct_type");
            let has_interface = node.children().any(|c| c.kind() == "interface_type");
            let kind = if struct_type.is_some() {
                Some(EntityKind::Class)
            } else if has_interface {
                Some(EntityKind::Interface)
            } else {
                None
            };
            if let Some(kind) = kind {
                ctx.push(kind, name.clone(), node);
                maybe_export(node, &name, ctx);
            }
            // Struct embedding (a `field_declaration` with no `name` field)
            // -> Extends, linking the embedding struct to the embedded
            // type. Implicit interface satisfaction (structural typing) is
            // intentionally excluded: it would require real type inference,
            // not a syntactic pattern, to detect reliably.
            if let Some(struct_type) = &struct_type {
                for embedded in embedded_field_type_names(struct_type) {
                    ctx.out.push(entity(
                        EntityKind::Extends,
                        embedded,
                        ctx.file_id,
                        node,
                        EntityMeta {
                            enclosing: Some(name.clone()),
                            ..Default::default()
                        },
                    ));
                }
            }
        }

        // ---- variables ----
        "var_spec" | "const_spec" => {
            for n in declarator_names(node) {
                ctx.push(EntityKind::Variable, n.clone(), node);
                maybe_export(node, &n, ctx);
            }
        }
        "short_var_declaration" => {
            // `a, b := ...` — names live in the `left` expression list.
            if let Some(left) = node.field("left") {
                for child in left.children() {
                    if child.kind() == "identifier" {
                        let name = child.text().into_owned();
                        if name != "_" {
                            ctx.push(EntityKind::Variable, name, node);
                        }
                    }
                }
            }
        }

        // ---- parameters ----
        "parameter_declaration" | "variadic_parameter_declaration" => {
            for n in declarator_names(node) {
                ctx.push(EntityKind::Parameter, n, node);
            }
        }

        // ---- expression-level ----
        "call_expression" => {
            let name = node
                .field("function")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Call, name.clone(), node);
            // panic() -> Throw, recover() -> Catch (Go's idiomatic equivalents).
            match name.as_str() {
                "panic" => ctx.push(
                    EntityKind::Throw,
                    first_arg_text(node).unwrap_or_default(),
                    node,
                ),
                "recover" => ctx.push(EntityKind::Catch, "recover".to_string(), node),
                _ => {}
            }
            // net/http routes and responses.
            if let Some(route) = route_of(node) {
                ctx.out.push(Entity {
                    kind: EntityKind::Route,
                    name: name.clone(),
                    file_id: ctx.file_id,
                    span: crate::extract::span_of(node),
                    enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
                    method: None,
                    path: Some(route),
                    status: None,
                    body_shape: None,
                    body_minhash: None,
                    is_async: None,
                    is_test: false,
                    owner_type: None,
                });
            }
            if let Some(resp) = response_of(node) {
                ctx.out.push(Entity {
                    kind: EntityKind::Response,
                    name: name.clone(),
                    file_id: ctx.file_id,
                    span: crate::extract::span_of(node),
                    enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
                    method: None,
                    path: None,
                    status: resp.0,
                    body_shape: resp.1,
                    body_minhash: None,
                    is_async: None,
                    is_test: false,
                    owner_type: None,
                });
            }
        }
        "selector_expression" => {
            let name = node
                .field("field")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::MemberAccess, name, node);
        }
        "int_literal"
        | "float_literal"
        | "imaginary_literal"
        | "rune_literal"
        | "interpreted_string_literal"
        | "raw_string_literal"
        | "true"
        | "false"
        | "nil" => {
            ctx.push(EntityKind::Literal, node.text().into_owned(), node);
        }

        // ---- control flow ----
        "if_statement"
        | "for_statement"
        | "expression_switch_statement"
        | "type_switch_statement"
        | "select_statement"
        | "return_statement"
        | "break_statement"
        | "continue_statement"
        | "go_statement"
        | "defer_statement" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// Names of a `var_spec`/`const_spec`/`parameter_declaration`: the `name`
/// field may hold one or several identifiers.
fn declarator_names(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for n in node.field_children("name") {
        if n.kind() == "identifier" {
            let t = n.text().into_owned();
            if t != "_" && !names.contains(&t) {
                names.push(t);
            }
        }
    }
    if names.is_empty() {
        for c in node.children() {
            if c.kind() == "identifier" {
                let t = c.text().into_owned();
                if t != "_" {
                    names.push(t);
                }
            }
        }
    }
    names
}

/// Names of embedded types in a `struct_type`'s field list: a
/// `field_declaration` with no `name` field is an embedded (anonymous)
/// field — its `type` field is the embedded type, optionally wrapped in a
/// `pointer_type` (`*Base`) or qualified by package (`pkg.Base`).
fn embedded_field_type_names(
    struct_type: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Vec<String> {
    let Some(list) = struct_type
        .children()
        .find(|c| c.kind() == "field_declaration_list")
    else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == "field_declaration" && c.field("name").is_none())
        .filter_map(|c| c.field("type"))
        .map(|t| embedded_type_name(&t))
        .collect()
}

/// Unwraps a `pointer_type` to its base type text; otherwise the node's own
/// text (covers `type_identifier` and `qualified_type`, e.g. `pkg.Base`).
fn embedded_type_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    if node.kind() == "pointer_type" {
        node.children()
            .find(|c| c.kind() != "*")
            .map(|c| c.text().into_owned())
            .unwrap_or_else(|| node.text().into_owned())
    } else {
        node.text().into_owned()
    }
}

/// Go exports are capitalized package-level names: emit an Export entity
/// alongside the primary entity for top-level declarations.
fn maybe_export(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    name: &str,
    ctx: &mut ExtractCtx,
) {
    let top_level = node
        .parent()
        .map(|p| p.kind() == "source_file" || p.kind() == "type_declaration")
        .unwrap_or(false);
    if top_level && name.starts_with(|c: char| c.is_uppercase()) {
        ctx.out.push(entity(
            EntityKind::Export,
            name.to_string(),
            ctx.file_id,
            node,
            EntityMeta::default(),
        ));
    }
}

/// net/http route: `mux.HandleFunc("/path", handler)` — selector call whose
/// field is HandleFunc/Handle with a string-literal first argument.
fn route_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let fn_node = node.field("function")?;
    if fn_node.kind() != "selector_expression" {
        return None;
    }
    let field = fn_node.field("field")?.text();
    if field != "HandleFunc" && field != "Handle" {
        return None;
    }
    let path = first_arg_text(node)?;
    if !path.starts_with('"') && !path.starts_with('`') {
        return None;
    }
    Some(super::unquote(&path, true))
}

/// net/http response: `w.WriteHeader(n)` -> (status, None);
/// `w.Write(...)` / `w.WriteString(...)` / `w.WriteHeader(n)` -> responses.
fn response_of(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<(Option<String>, Option<String>)> {
    let fn_node = node.field("function")?;
    if fn_node.kind() != "selector_expression" {
        return None;
    }
    let field = fn_node.field("field")?.text().into_owned();
    let base = fn_node.field("operand")?.text().into_owned();
    if base != "w" && base != "res" {
        return None;
    }
    match field.as_str() {
        "WriteHeader" => Some((first_arg_text(node), None)),
        "Write" | "WriteString" => Some((None, Some(field))),
        _ => None,
    }
}
