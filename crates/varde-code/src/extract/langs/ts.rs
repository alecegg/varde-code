//! TypeScript entity extraction (the reference implementation for the
//! coverage-parity checklist; JS/TSX share most of these node kinds).

use crate::extract::entity::{EntityMeta, ExtractCtx, entity};
use crate::extract::field_name;
use crate::extract::langs::{
    chain_status, decorator_name, first_arg_text, push_type_ref, strip_generic_args,
};
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope.
pub const TYPE_SCOPES: &[&str] = &["class_declaration", "interface_declaration"];

pub const FUNCTION_SCOPES: &[&str] = &[
    "function_declaration",
    "function_expression",
    "arrow_function",
    "method_definition",
    "generator_function_declaration",
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

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- structural kinds ----
        "function_declaration" => push_named(node, EntityKind::Function, ctx),
        "class_declaration" => {
            let name = field_name(node).unwrap_or_default();
            push_named(node, EntityKind::Class, ctx);
            // `class Foo extends Base implements IFoo, IBar` — the grammar
            // gives `extends`/`implements` their own clauses under a
            // `class_heritage` wrapper, so no first-entry heuristic is
            // needed here (unlike C#/Swift).
            if let Some(heritage) = node.children().find(|c| c.kind() == "class_heritage") {
                if let Some(extends) = heritage.children().find(|c| c.kind() == "extends_clause")
                    && let Some(value) = extends.field("value")
                {
                    push_type_ref(
                        ctx,
                        EntityKind::Extends,
                        value.text().into_owned(),
                        &name,
                        &extends,
                    );
                }
                if let Some(implements) = heritage
                    .children()
                    .find(|c| c.kind() == "implements_clause")
                {
                    for ty in implements.children().filter(|c| c.is_named()) {
                        push_type_ref(
                            ctx,
                            EntityKind::Implements,
                            strip_generic_args(&ty.text()),
                            &name,
                            &implements,
                        );
                    }
                }
            }
            // `@Component class Foo {}` — decorator(s) attach as direct
            // children of class_declaration, preceding the `class` keyword.
            // `name` is the decorator expression text (callee for a call-style
            // decorator factory, otherwise the bare expression).
            for decorator in node.children().filter(|c| c.kind() == "decorator") {
                if let Some(dec_name) = decorator_name(&decorator) {
                    push_type_ref(ctx, EntityKind::Decorator, dec_name, &name, &decorator);
                }
            }
        }
        "interface_declaration" => push_named(node, EntityKind::Interface, ctx),
        // `type X = ...` — a first-class TS type declaration (alias, union,
        // mapped type). Mapped to Interface (the closest structural type kind)
        // so it is queryable via `symbols_in_file`/`get_symbol`; previously
        // dropped entirely (audit S3: all 17 aliases in one file were missing).
        "type_alias_declaration" => push_named(node, EntityKind::Interface, ctx),
        // `enum X { ... }` / `const enum X { ... }` — a runtime object of named
        // members; mapped to Class so it surfaces as a declaration rather than
        // being dropped.
        "enum_declaration" => push_named(node, EntityKind::Class, ctx),
        // Class/interface methods — previously invisible to extraction (only
        // top-level `function_declaration` emitted a Function entity), so
        // class-membership queries (SOLID interface-coverage/fat-interface
        // rules) had no method to count. `owner_type` (set from `ctx.type_scope`
        // inside `entity()`/`push_named`) links each method back to its class.
        "method_definition" => push_named(node, EntityKind::Function, ctx),
        // Interface method member (`method_signature`, e.g. `foo(): void;`
        // inside `interface Foo { ... }`) — same class-membership need as
        // `method_definition`, just for the interface side of a
        // coverage/ISP comparison.
        "method_signature" => push_named(node, EntityKind::Function, ctx),

        // ---- imports ----
        // `import ... from "spec"` -> one Import entity named after the
        // (unquoted) module specifier.
        "import_statement" => {
            let spec = node
                .field("source")
                .map(|n| super::unquote(n.text().as_ref(), true))
                .unwrap_or_default();
            let mut imp = entity(
                EntityKind::Import,
                spec,
                ctx.file_id,
                node,
                EntityMeta::default(),
            );
            // `import type { Foo } from "./bar"` (and `import type Foo from
            // ...`) is erased entirely at compile time — no runtime module
            // reference exists, so it can't produce a real load-order cycle.
            // `body_shape` is otherwise unused on Import entities (reused
            // here, not a new column); `circular_import.toml` excludes edges
            // tagged this way from the runtime import graph.
            if is_ts_type_only_import(node) {
                imp.body_shape = Some("type_only".to_string());
            }
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
        // Function/method parameters carry their identifier in the `pattern`
        // field (required_parameter / optional_parameter).
        "required_parameter" | "optional_parameter" => {
            let name = node
                .field("pattern")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Parameter, name, node);
        }
        // export statements: `export { a, b }` yields one Export per
        // specifier; `export function/const/class ...` yields one Export
        // named after the declared symbol.
        "export_statement" => {
            let mut names: Vec<String> = Vec::new();
            // `export { a, b }`: specifiers live under an export_clause.
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
        // Calls: callee text as name. Also the anchor for domain-specific
        // route/response detection.
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
        // Fixture-driven scoping decision: literals inside type annotations
        // (`predefined_type`) are not matched; literal *types* (`literal_type`)
        // do collide with expression literals and are currently included.
        "number" | "string" | "true" | "false" | "null" | "undefined" | "template_string"
        | "regex" => {
            // Literals inside type annotations (predefined_type, literal_type,
            // generic type args, ...) are types, not expression literals.
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
        // throw <expr> -> Throw, named after the thrown expression. The JS/TS
        // grammar gives throw_statement no field for the expression (it is a
        // bare seq('throw', $._expressions, ';')), so take the text after the
        // 'throw' keyword.
        "throw_statement" => {
            let name = node
                .children()
                .nth(1)
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Throw, name, node);
        }
        // Control flow: name = tree-sitter node kind (superset-safe minimal
        // shape per the plan: kind + span + enclosing linkage).
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
            is_async: (kind == EntityKind::Function)
                .then(|| super::super::langs::node_is_async(node)),
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

// ---- domain-specific detection (Express-style call shapes) ----

const ROUTE_METHODS: &[&str] = &[
    "get", "post", "put", "patch", "delete", "options", "head", "all", "use",
];
const ROUTE_OBJECTS: &[&str] = &["app", "router"];
const RESPONSE_OBJECTS: &[&str] = &["res"];
const RESPONSE_VERBS: &[&str] = &["send", "json", "end", "render", "redirect", "sendFile"];

/// Express-style route registration: `app.get("/path", handler)` →
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
/// `res.status(200).json(...)` / `res.sendStatus(404)` →
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

/// Strip surrounding quotes/backticks from a string literal's text.
/// True for a top-level type-only import: `import type Foo from "..."` or
/// `import type { Foo } from "..."`. Named-specifier-level `import { type
/// Foo, bar } from "..."` (a mixed import) is intentionally NOT matched
/// here — that statement still has a real runtime module reference via
/// `bar`, so it can legitimately participate in a load-bearing cycle.
fn is_ts_type_only_import(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    node.children()
        .take_while(|c| c.kind().as_ref() != "import_clause")
        .any(|c| c.text().as_ref() == "type")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::parse::parse_source;

    #[test]
    fn extends_implements_entities_carry_raw_name_and_owner() {
        let src = "class Foo extends Base implements IFoo, IBar { }";
        let parsed = parse_source(&SupportLang::TypeScript, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let extends: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Extends)
            .collect();
        assert_eq!(extends.len(), 1, "entities: {entities:?}");
        assert_eq!(extends[0].name, "Base");
        assert_eq!(extends[0].enclosing_function.as_deref(), Some("Foo"));

        let mut implements: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .map(|e| e.name.as_str())
            .collect();
        implements.sort_unstable();
        assert_eq!(implements, vec!["IBar", "IFoo"]);
        assert!(
            entities
                .iter()
                .filter(|e| e.kind == EntityKind::Implements)
                .all(|e| e.enclosing_function.as_deref() == Some("Foo"))
        );
    }

    #[test]
    fn generic_implements_supertype_names_are_stripped_to_bare_identifier() {
        // CORRECTNESS-002: `implements Comparable<Foo>` must yield
        // "Comparable", matching the `extends` branch's existing behavior
        // (which already excludes type_arguments via the grammar's `value`
        // field).
        let src = "class Foo implements Comparable<Foo> { }";
        let parsed = parse_source(&SupportLang::TypeScript, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let implements: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .collect();
        assert_eq!(implements.len(), 1, "entities: {entities:?}");
        assert_eq!(implements[0].name, "Comparable");
    }

    #[test]
    fn type_aliases_and_enums_are_captured_as_declarations() {
        // Audit S3: `type X = ...` and `enum X {}` were dropped entirely, so
        // they never appeared in symbols_in_file/get_symbol. Aliases map to
        // Interface (type-level), enums to Class (runtime object of members).
        let src = "export type JsonValue = string | number;\ntype Mode = \"a\" | \"b\";\nexport enum Color { Red, Green }\n";
        let parsed = parse_source(&SupportLang::TypeScript, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let interfaces: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Interface)
            .map(|e| e.name.as_str())
            .collect();
        assert!(
            interfaces.contains(&"JsonValue") && interfaces.contains(&"Mode"),
            "type aliases must be captured as Interface: {entities:?}"
        );
        assert!(
            entities
                .iter()
                .any(|e| e.kind == EntityKind::Class && e.name == "Color"),
            "enum must be captured as Class: {entities:?}"
        );
    }

    #[test]
    fn decorator_entity_carries_expression_text_and_owner() {
        let src = "@Component\nclass Foo {}";
        let parsed = parse_source(&SupportLang::TypeScript, src);
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
}
