//! JavaScript entity extraction.
//!
//! JavaScript shares almost every node kind of the 14-kind checklist with
//! TypeScript (`function_declaration`, `class_declaration`, `call_expression`,
//! `member_expression`, ...); see `super::ts` for the reference mapping. The
//! JS-specific differences:
//! - Interface: carved out — `interface_declaration` is a TypeScript-only
//!   node kind; JavaScript has no interface syntax.
//! - Parameter: JS parameters are bare `identifier` / `assignment_pattern`
//!   (defaults) / `rest_pattern` nodes inside `formal_parameters` — the
//!   `required_parameter`/`optional_parameter` kinds are TypeScript-only.
//! - Export/Route/Response: same node kinds and shapes as ts.rs, reused
//!   verbatim (export_statement, Express-style `app.get` / `res.json`).
//! - Literals in type contexts cannot occur (JS has no type syntax), but the
//!   shared `ctx.in_type` guard is kept for safety.

use crate::extract::entity::{EntityMeta, ExtractCtx, entity};
use crate::extract::field_name;
use crate::extract::langs::{chain_status, decorator_name, first_arg_text};
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope.
pub const TYPE_SCOPES: &[&str] = &["class_declaration"];

pub const FUNCTION_SCOPES: &[&str] = &[
    "function_declaration",
    "function_expression",
    "arrow_function",
    "method_definition",
    "generator_function_declaration",
];

/// Entity kinds the fixtures must produce. Interface is carved out (JS has no
/// interface syntax); the other 13 kinds map to native constructs.
pub const REQUIRED_KINDS: [EntityKind; 13] = [
    EntityKind::Function,
    EntityKind::Class,
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

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- structural kinds ----
        "function_declaration" => push_named(node, EntityKind::Function, ctx),
        // Class methods — previously invisible to extraction; `owner_type`
        // (set from `ctx.type_scope`) links each back to its class. See ts.rs.
        "method_definition" => push_named(node, EntityKind::Function, ctx),
        "class_declaration" => {
            push_named(node, EntityKind::Class, ctx);
            // `class X extends Y {}` -> a single Extends entity naming the
            // superclass, linked back to X via enclosing_function. The
            // superclass lives in an unnamed `class_heritage` child wrapping
            // a single expression (identifier or member_expression).
            let class_name = field_name(node).unwrap_or_default();
            if let Some(heritage) = node.children().find(|c| c.kind() == "class_heritage")
                && let Some(base) = heritage.children().find(|c| c.is_named())
            {
                ctx.out.push(Entity {
                    kind: EntityKind::Extends,
                    name: base.text().into_owned(),
                    file_id: ctx.file_id,
                    span: crate::extract::span_of(&base),
                    enclosing_function: Some(class_name.clone()),
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
            // `@Component class Foo {}` — decorator(s) attach as direct
            // children of class_declaration, preceding the `class` keyword.
            // `name` is the decorator expression text (callee for a call-style
            // decorator factory, otherwise the bare expression).
            for decorator in node.children().filter(|c| c.kind() == "decorator") {
                if let Some(dec_name) = decorator_name(&decorator) {
                    ctx.out.push(Entity {
                        kind: EntityKind::Decorator,
                        name: dec_name,
                        file_id: ctx.file_id,
                        span: crate::extract::span_of(&decorator),
                        enclosing_function: Some(class_name.clone()),
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
            }
        }

        // ---- imports ----
        // JS uses the same tree-sitter node shapes as TS.
        "import_statement" => {
            let spec = node
                .field("source")
                .map(|n| super::unquote(n.text().as_ref(), true))
                .unwrap_or_default();
            let imp = entity(
                EntityKind::Import,
                spec,
                ctx.file_id,
                node,
                EntityMeta::default(),
            );
            ctx.out.push(imp);
        }

        // ---- statement-level kinds ----
        // const/let -> lexical_declaration, var -> variable_declaration; one
        // Entity per declarator (handles `const a = 1, b = 2;`).
        "lexical_declaration" | "variable_declaration" => {
            for declarator in node.children() {
                if declarator.kind() == "variable_declarator" {
                    let name = field_name(&declarator).unwrap_or_default();
                    ctx.push(EntityKind::Variable, name, &declarator);
                }
            }
        }
        // JS parameters are bare identifiers/assignment_patterns/rest_patterns
        // inside formal_parameters (no required_/optional_parameter kinds).
        "formal_parameters" => {
            for child in node.children() {
                let name = match child.kind().as_ref() {
                    "identifier" => Some(child.text().into_owned()),
                    "assignment_pattern" => child.field("left").map(|n| n.text().into_owned()),
                    "rest_pattern" => child
                        .children()
                        .find(|c| c.kind() == "identifier")
                        .map(|c| c.text().into_owned()),
                    _ => None,
                };
                if let Some(name) = name {
                    ctx.push(EntityKind::Parameter, name, &child);
                }
            }
        }
        // export statements: `export { a, b }` yields one Export per
        // specifier; `export function/const/class ...` yields one Export
        // named after the declared symbol.
        "export_statement" => {
            let mut names: Vec<String> = Vec::new();
            for child in node.children() {
                if child.kind() == "export_clause" {
                    for spec in child.children() {
                        if spec.kind() == "export_specifier"
                            && let Some(n) = field_name(&spec)
                        {
                            names.push(n);
                        }
                    }
                }
            }
            if names.is_empty()
                && let Some(n) = declaration_name(node)
            {
                names.push(n);
            }
            for name in names {
                ctx.push(EntityKind::Export, name, node);
            }
        }

        // ---- expression-level kinds ----
        "call_expression" => {
            let name = node
                .field("function")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Call, name.clone(), node);
            if let Some(route) = route_of(node) {
                ctx.out.push(Entity {
                    kind: EntityKind::Route,
                    name: name.clone(),
                    file_id: ctx.file_id,
                    span: crate::extract::span_of(node),
                    enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
                    method: Some(route.0),
                    path: Some(route.1),
                    status: None,
                    body_shape: None,
                    body_minhash: None,
                    is_async: None,
                    is_test: false,
                    // Handler function name (final identifier argument) for
                    // route→handler resolution in `detect_routes`.
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
        // Member accesses: property text as name (`a.b` -> "b").
        "member_expression" => {
            let name = node
                .field("property")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::MemberAccess, name, node);
        }
        // Literals: number/string/boolean/null/undefined/template/regex.
        "number" | "string" | "true" | "false" | "null" | "undefined" | "template_string"
        | "regex" => {
            if !ctx.in_type {
                ctx.push(EntityKind::Literal, node.text().into_owned(), node);
            }
        }

        // ---- control-flow/error kinds ----
        // catch (err) -> Catch, named after the exception variable.
        "catch_clause" => {
            let name = node
                .field("parameter")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Catch, name, node);
        }
        // throw <expr> -> Throw, named after the thrown expression. The JS
        // grammar gives throw_statement no field for the expression, so take
        // the text after the 'throw' keyword.
        "throw_statement" => {
            let name = node
                .children()
                .nth(1)
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Throw, name, node);
        }
        // Control flow: name = tree-sitter node kind (superset-safe minimal
        // shape: kind + span + enclosing linkage).
        "if_statement" | "for_statement" | "for_in_statement" | "for_of_statement"
        | "while_statement" | "do_statement" | "switch_statement" | "try_statement"
        | "ternary_expression" | "return_statement" | "break_statement" | "continue_statement" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

fn push_named(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: EntityKind,
    ctx: &mut ExtractCtx,
) {
    ctx.out.push(entity(
        kind,
        field_name(node).unwrap_or_default(),
        ctx.file_id,
        node,
        EntityMeta {
            owner_type: (kind == EntityKind::Function)
                .then_some(ctx.type_scope)
                .flatten()
                .map(|s| s.to_owned()),
            ..Default::default()
        },
    ));
}

/// Name of the declaration wrapped by an export statement, if it has one
/// (function/class declarations and var/const/let declarators).
fn declaration_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    for child in node.children() {
        match child.kind().as_ref() {
            "function_declaration" | "class_declaration" => return field_name(&child),
            "lexical_declaration" | "variable_declaration" => {
                for declarator in child.children() {
                    if declarator.kind() == "variable_declarator" {
                        return field_name(&declarator);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ---- domain-specific detection (Express-style call shapes; same heuristics
// as ts.rs — JS shares the member_expression/call_expression node kinds) ----

const ROUTE_METHODS: &[&str] = &[
    "get", "post", "put", "patch", "delete", "options", "head", "all", "use",
];
const ROUTE_OBJECTS: &[&str] = &["app", "router"];
const RESPONSE_OBJECTS: &[&str] = &["res"];
const RESPONSE_VERBS: &[&str] = &["send", "json", "end", "render", "redirect", "sendFile"];

/// Express-style route registration: `app.get("/path", handler)` ->
/// (method, path). Narrow, fixture-driven heuristic: callee is a
/// member_expression on app/router with an HTTP-method property and a string
/// literal first argument.
fn route_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<(String, String)> {
    let fn_node = node.field("function")?;
    if fn_node.kind() != "member_expression" {
        return None;
    }
    let method = fn_node.field("property")?.text().into_owned();
    if !ROUTE_METHODS.contains(&method.as_str()) {
        return None;
    }
    let base = base_identifier(&fn_node)?;
    if !ROUTE_OBJECTS.contains(&base.as_str()) {
        return None;
    }
    let path = first_arg_text(node)?;
    if !path.starts_with('"') && !path.starts_with('`') {
        return None;
    }
    Some((method, super::unquote(&path, true)))
}

/// Express-style response call: `res.send(...)` / `res.json(...)` /
/// `res.status(200).json(...)` / `res.sendStatus(404)` ->
/// (status, body_shape).
fn response_of(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<(Option<String>, Option<String>)> {
    let fn_node = node.field("function")?;
    if fn_node.kind() != "member_expression" {
        return None;
    }
    let verb = fn_node.field("property")?.text().into_owned();
    let base = base_identifier(&fn_node)?;
    if !RESPONSE_OBJECTS.contains(&base.as_str()) {
        return None;
    }
    match verb.as_str() {
        "sendStatus" => Some((first_arg_text(node), None)),
        v if RESPONSE_VERBS.contains(&v) => Some((chain_status(&fn_node), Some(v.to_string()))),
        _ => None,
    }
}

/// Walk a member-expression chain down to its base identifier
/// (`res.status(200).json` -> "res", `app.get` -> "app").
fn base_identifier(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let mut cur = node.clone();
    loop {
        match cur.kind().as_ref() {
            "identifier" => return Some(cur.text().into_owned()),
            "member_expression" => cur = cur.field("object")?,
            "call_expression" => cur = cur.field("function")?,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::parse::parse_source;

    #[test]
    fn decorator_entity_carries_expression_text_and_owner() {
        let src = "@Component\nclass Foo {}";
        let parsed = parse_source(&SupportLang::JavaScript, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let decorators: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Decorator)
            .collect();
        assert_eq!(decorators.len(), 1, "entities: {entities:?}");
        assert_eq!(decorators[0].name, "Component");
        assert_eq!(decorators[0].enclosing_function.as_deref(), Some("Foo"));
    }

    #[test]
    fn express_route_captures_named_handler_on_owner_type() {
        // `app.get("/users", listUsers)` -> the Route entity carries the
        // handler name so `detect_routes` can resolve it. An inline arrow
        // handler leaves it absent.
        let src = "app.get(\"/users\", listUsers);\napp.post(\"/x\", (req, res) => res.end());\n";
        let parsed = parse_source(&SupportLang::JavaScript, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;
        let routes: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Route)
            .collect();

        let named = routes
            .iter()
            .find(|r| r.path.as_deref() == Some("/users"))
            .expect("GET /users route");
        assert_eq!(named.owner_type.as_deref(), Some("listUsers"));
        let inline = routes
            .iter()
            .find(|r| r.path.as_deref() == Some("/x"))
            .expect("POST /x route");
        assert_eq!(
            inline.owner_type, None,
            "inline arrow handler names no resolvable function"
        );
    }
}
