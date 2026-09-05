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
        "function_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name.clone(), node);
            maybe_export(node, &name, ctx);
        }
        // A method's receiver type is its owning type. Go methods are declared
        // at file scope (not nested inside the type), so the generic
        // type-scope stack can't supply `owner_type` the way it does for
        // brace-nested languages — read it off the `receiver` field directly
        // and stamp it via `EntityMeta`, matching what `ctx.push` sets for
        // is_async/is_test on a Function.
        "method_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.out.push(entity(
                EntityKind::Function,
                name.clone(),
                ctx.file_id,
                node,
                EntityMeta {
                    is_async: Some(crate::extract::langs::node_is_async(node)),
                    is_test: crate::extract::langs::node_is_test(node),
                    owner_type: receiver_type_name(node),
                    ..Default::default()
                },
            ));
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
            // net/http + gin/echo/chi/fiber routes and responses.
            if let Some((method, path)) = route_of(node) {
                ctx.out.push(Entity {
                    kind: EntityKind::Route,
                    name: name.clone(),
                    file_id: ctx.file_id,
                    span: crate::extract::span_of(node),
                    enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
                    method: Some(method),
                    path: Some(path),
                    status: None,
                    body_shape: None,
                    body_minhash: None,
                    is_async: None,
                    is_test: false,
                    // The handler's function name (final identifier argument),
                    // stashed on `owner_type` so `detect_routes` can resolve
                    // the route to its handler for flow-tree rooting.
                    owner_type: crate::extract::langs::last_arg_identifier(node),
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

/// The bare type name of a `method_declaration`'s receiver, for `owner_type`
/// linkage. The `receiver` field is a `parameter_list` holding one
/// `parameter_declaration` whose `type` field is the receiver type —
/// `Foo`, `*Foo` (pointer receiver), or a generic `Foo[T]`. Pointer stripping
/// reuses [`embedded_type_name`]; a generic instantiation's `type_arguments`
/// suffix is dropped so `Foo[T]` and `*Foo` both yield `Foo`.
fn receiver_type_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let receiver = node.field("receiver")?;
    let param = receiver
        .children()
        .find(|c| c.kind() == "parameter_declaration")?;
    let ty = param.field("type")?;
    let base = if ty.kind() == "generic_type" {
        ty.children()
            .find(|c| c.kind() != "type_arguments")
            .map(|c| c.text().into_owned())
            .unwrap_or_else(|| ty.text().into_owned())
    } else {
        embedded_type_name(&ty)
    };
    let base = base.trim();
    (!base.is_empty()).then(|| base.to_string())
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

/// HTTP verb methods shared by the common Go router frameworks (gin, echo,
/// chi, fiber). Matched case-insensitively against the selector field so
/// gin's `.GET` and chi's `.Get` both resolve.
const GO_ROUTE_VERBS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];

/// A route-registration call -> `(method, path)`.
///
/// Recognizes net/http's `mux.HandleFunc("/p", h)` / `mux.Handle("/p", h)`
/// (verb not in the call -> `"*"`) and the verb-method form shared by
/// gin/echo/chi/fiber (`r.GET("/p", h)` / `r.Get("/p", h)`). Guarded by
/// requiring the first argument to be a string literal beginning with `/`, so
/// ordinary member calls like `cache.Get("key")` are not mistaken for routes.
fn route_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<(String, String)> {
    let fn_node = node.field("function")?;
    if fn_node.kind() != "selector_expression" {
        return None;
    }
    let field = fn_node.field("field")?.text().into_owned();
    let raw = first_arg_text(node)?;
    if !raw.starts_with('"') && !raw.starts_with('`') {
        return None;
    }
    let path = super::unquote(&raw, true);
    if !path.starts_with('/') {
        return None;
    }
    if field == "HandleFunc" || field == "Handle" {
        return Some(("*".to_string(), path));
    }
    let method = field.to_ascii_uppercase();
    if GO_ROUTE_VERBS.contains(&method.as_str()) {
        return Some((method, path));
    }
    None
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

#[cfg(test)]
mod tests {
    use crate::extract;
    use crate::model::{Entity, EntityKind};
    use crate::parse::parse_source;
    use ast_grep_language::SupportLang;

    fn routes(src: &str) -> Vec<(Option<String>, Option<String>)> {
        let parsed = parse_source(&SupportLang::Go, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        extract::extract(&parsed, 0)
            .entities
            .into_iter()
            .filter(|e: &Entity| e.kind == EntityKind::Route)
            .map(|e| (e.method, e.path))
            .collect()
    }

    #[test]
    fn gin_echo_chi_verb_routes_capture_method_and_path() {
        // gin/echo `.GET`, chi `.Get`, and net/http `.HandleFunc` (verb `*`).
        let src = "package main\nfunc setup(r Router) {\n\tr.GET(\"/users\", list)\n\tr.Get(\"/health\", ok)\n\tr.HandleFunc(\"/legacy\", h)\n}\n";
        let got = routes(src);
        let has = |m: &str, p: &str| {
            got.iter()
                .any(|(gm, gp)| gm.as_deref() == Some(m) && gp.as_deref() == Some(p))
        };
        assert!(has("GET", "/users"), "routes: {got:?}");
        assert!(has("GET", "/health"), "routes: {got:?}");
        assert!(has("*", "/legacy"), "routes: {got:?}");
    }

    #[test]
    fn method_receiver_type_recorded_as_owner_and_plain_func_has_none() {
        // A method's receiver type is its owning type: `func (f Foo) Bar()`
        // -> owner_type "Foo"; a pointer receiver `func (s *Store) Save()`
        // strips the `*` -> "Store"; a plain `func free()` has no owner.
        let src = "package m\ntype Foo struct{}\ntype Store struct{}\nfunc (f Foo) Bar() {}\nfunc (s *Store) Save() {}\nfunc free() {}\n";
        let parsed = parse_source(&SupportLang::Go, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;
        let owner = |name: &str| {
            entities
                .iter()
                .find(|e: &&Entity| e.kind == EntityKind::Function && e.name == name)
                .unwrap_or_else(|| panic!("function {name} present: {entities:?}"))
                .owner_type
                .clone()
        };
        assert_eq!(owner("Bar").as_deref(), Some("Foo"));
        assert_eq!(owner("Save").as_deref(), Some("Store"));
        assert_eq!(owner("free"), None);
    }

    #[test]
    fn non_route_member_calls_are_not_routes() {
        // `cache.Get("key")` shares the verb-method name but its argument is
        // not a `/`-path, so it must not be mistaken for a route.
        let src =
            "package main\nfunc f(cache C) {\n\tcache.Get(\"key\")\n\tdb.Delete(\"row-1\")\n}\n";
        assert!(
            routes(src).is_empty(),
            "unexpected routes: {:?}",
            routes(src)
        );
    }
}
