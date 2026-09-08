//! Swift entity extraction.
//!
//! Mapping notes (fixture-driven, superset-safe; node kinds from
//! tree-sitter-swift 0.7.3, the alex-pinkus grammar):
//! - `function_declaration` -> Function (functions AND methods). `init` /
//!   `deinit` constructors and protocol-member function requirements are
//!   folded in as function declarations (`init_declaration`,
//!   `protocol_function_declaration`).
//! - `class_declaration` -> Class (this grammar folds struct/enum/actor/
//!   extension declarations into `class_declaration`, distinguished by the
//!   `declaration_kind` field).
//! - `protocol_declaration` -> Interface (Swift's abstraction mechanism).
//! - `property_declaration` -> Variable (let/var at any scope — locals,
//!   stored properties, top-level constants).
//! - `parameter` -> Parameter (name field of the function signature entry).
//! - Export: Swift has no export statement; a `public` *top-level* declaration
//!   (parent is `source_file`) yields an Export entity alongside its primary
//!   entity. Narrow: top-level Function/Class/Protocol only.
//! - `call_expression` -> Call (callee text). Also the anchor for
//!   domain-specific Vapor detection (see below).
//! - `navigation_expression` -> MemberAccess (the identifier inside the
//!   trailing `navigation_suffix`).
//! - Literals: integer/real/string (single/multi-line/raw)/boolean/nil,
//!   excluding type contexts.
//! - Catch/Throw: `catch_block` -> Catch (named after the bound error, if
//!   any); `control_transfer_statement` containing a `throw_keyword` -> Throw
//!   (named after the thrown expression).
//! - ControlFlow: if/for/while/repeat-while/switch/guard/do and the
//!   return/break/continue/fallthrough `control_transfer_statement`s.
//! - Route: Vapor DSL shape `app.get("/path") { ... }` — a call whose callee
//!   is a member access on app/router with an HTTP verb and a string-literal
//!   first argument. Narrow, fixture-driven.
//! - Response: Vapor `res.send(...)` / `res.json(...)` / `res.text(...)` ->
//!   body_shape = verb.

use crate::extract::entity::{EntityMeta, ExtractCtx, entity};
use crate::extract::field_name;
use crate::extract::langs::{push_type_ref, strip_generic_args};
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope.
pub const TYPE_SCOPES: &[&str] = &["class_declaration", "protocol_declaration"];

pub const FUNCTION_SCOPES: &[&str] = &[
    "function_declaration",
    "init_declaration",
    "protocol_function_declaration",
    "computed_getter",
    "computed_setter",
];

/// Entity kinds the fixtures must produce (all 14 expressible in Swift).
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
        // ---- imports ----
        // `import Foundation` / `import class Foundation.NSString` / `@testable
        // import Foo`. Swift's import statement is module-level, not
        // file-level, so intra-repo resolution will rarely hit for typical
        // Swift usage (a module usually spans many files with no 1:1
        // module-to-file naming convention) — extracted anyway for
        // consistency with every other language, and to catch the cases
        // where it does line up (a single-file module, or a submodule import
        // like `Foundation.NSString` matching a same-named file). No
        // dedicated path field on this grammar; the last whitespace-separated
        // token of the node text is the module/submodule path, which also
        // strips any leading `kind` modifier (`class`/`struct`/`func`/...).
        "import_declaration" => {
            let spec = node
                .text()
                .split_whitespace()
                .next_back()
                .unwrap_or_default()
                .replace('.', "/");
            ctx.push(EntityKind::Import, spec, node);
        }

        // ---- structural ----
        "function_declaration"
        | "init_declaration"
        | "protocol_function_declaration"
        | "computed_getter"
        | "computed_setter" => {
            let name = function_scope_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name.clone(), node);
            maybe_export(node, &name, ctx);
        }
        "lambda_literal" => ctx.push_callable_boundary(node),
        "class_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            maybe_export(node, &name, ctx);
            // `class Foo: Base, IFoo` — `inheritance_specifier` doesn't
            // distinguish a superclass from protocols, and unlike C#, Swift
            // protocols have no naming convention (like `I`-prefix) to
            // disambiguate. A real superclass in first position is
            // syntactically identical to a class that implements only
            // protocols (e.g. `class Foo: Codable, Equatable { }`), so
            // guessing "first entry is Extends" is wrong for that common
            // shape (CORRECTNESS-001). Safer default: emit every entry as
            // Implements rather than falsely claiming an Extends
            // relationship that may not exist. Known limitation: a genuine
            // `class Foo: Base { }` superclass relationship is therefore
            // reported as Implements too.
            for spec in node
                .children()
                .filter(|c| c.kind() == "inheritance_specifier")
            {
                if let Some(ty) = spec.children().next() {
                    push_type_ref(
                        ctx,
                        EntityKind::Implements,
                        strip_generic_args(&ty.text()),
                        &name,
                        &spec,
                    );
                }
            }
        }
        "protocol_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Interface, name.clone(), node);
            maybe_export(node, &name, ctx);
            // `protocol Foo: Bar, Baz` — protocol inheritance is always
            // Extends (protocols cannot conform to a "class").
            for spec in node
                .children()
                .filter(|c| c.kind() == "inheritance_specifier")
            {
                if let Some(ty) = spec.children().next() {
                    push_type_ref(
                        ctx,
                        EntityKind::Extends,
                        strip_generic_args(&ty.text()),
                        &name,
                        &spec,
                    );
                }
            }
        }

        // ---- variables ----
        // let/var declarations: name lives in the `name` field as a pattern
        // (`let x = 5` -> pattern "x"). One Entity per declaration.
        "property_declaration" => {
            let name = node
                .field("name")
                .map(|n| {
                    n.field("bound_identifier")
                        .map(|b| b.text().into_owned())
                        .unwrap_or_else(|| n.text().into_owned())
                })
                .unwrap_or_default();
            ctx.push(EntityKind::Variable, name, node);
        }

        // ---- parameters ----
        "parameter" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Parameter, name, node);
        }

        // ---- expression-level ----
        "call_expression" => {
            let name = node
                .children()
                .next()
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Call, name.clone(), node);
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
                    owner_type: None,
                });
            }
            if let Some(verb) = response_of(node) {
                ctx.out.push(Entity {
                    kind: EntityKind::Response,
                    name: name.clone(),
                    file_id: ctx.file_id,
                    span: crate::extract::span_of(node),
                    enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
                    method: None,
                    path: None,
                    status: None,
                    body_shape: Some(verb),
                    body_minhash: None,
                    is_async: None,
                    is_test: false,
                    owner_type: None,
                });
            }
        }
        // Member accesses: the identifier inside the trailing navigation_suffix
        // (`app.get` -> "get").
        "navigation_expression" => {
            if let Some(name) = member_name(node) {
                ctx.push(EntityKind::MemberAccess, name, node);
            }
        }
        "integer_literal"
        | "real_literal"
        | "line_string_literal"
        | "multi_line_string_literal"
        | "raw_string_literal"
        | "boolean_literal"
        | "nil" => {
            if !ctx.in_type {
                ctx.push(EntityKind::Literal, node.text().into_owned(), node);
            }
        }

        // ---- error handling ----
        // do { } catch [let err] { } — named after the bound error pattern.
        "catch_block" => {
            let name = node
                .field("error")
                .and_then(|p| {
                    p.field("bound_identifier")
                        .map(|b| b.text().into_owned())
                        .or_else(|| {
                            p.children()
                                .find(|c| c.kind() == "simple_identifier")
                                .map(|c| c.text().into_owned())
                        })
                })
                .unwrap_or_default();
            ctx.push(EntityKind::Catch, name, node);
        }
        // return/break/continue/fallthrough/throw all parse as
        // control_transfer_statement; a throw_keyword child distinguishes
        // Throw from control flow.
        "control_transfer_statement" => {
            let has_throw = node.children().any(|c| c.kind() == "throw_keyword");
            if has_throw {
                // `throw <expr>` has no `result` field in this grammar; take the
                // first named child other than the throw_keyword itself.
                let name = node
                    .field("result")
                    .map(|n| n.text().into_owned())
                    .or_else(|| {
                        node.children()
                            .find(|c| c.is_named() && c.kind() != "throw_keyword")
                            .map(|n| n.text().into_owned())
                    })
                    .unwrap_or_default();
                ctx.push(EntityKind::Throw, name, node);
            } else {
                let text = node.text().into_owned();
                let name = if text.starts_with("return") {
                    "return_statement"
                } else if text.starts_with("break") {
                    "break_statement"
                } else if text.starts_with("continue") {
                    "continue_statement"
                } else if text.starts_with("fallthrough") {
                    "fallthrough_statement"
                } else {
                    "control_transfer_statement"
                };
                ctx.push(EntityKind::ControlFlow, name.to_string(), node);
            }
        }

        // ---- control flow ----
        "if_statement"
        | "for_statement"
        | "while_statement"
        | "repeat_while_statement"
        | "switch_statement"
        | "guard_statement"
        | "do_statement"
        | "ternary_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// Member name of a navigation_expression: the identifier inside its trailing
/// navigation_suffix.
fn member_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.field("suffix")
        .and_then(|s| s.children().find(|c| c.kind() == "simple_identifier"))
        .map(|n| n.text().into_owned())
}

/// Stable display and scope name for a Swift callable node.
///
/// Accessors only name their operation. Prefix the enclosing property or
/// subscript so unrelated accessors do not share a diagnostic label.
pub fn function_scope_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    if !matches!(node.kind().as_ref(), "computed_getter" | "computed_setter") {
        return field_name(node);
    }
    let declaration = node.parent()?.parent()?;
    let owner = match declaration.kind().as_ref() {
        "property_declaration" => field_name(&declaration)?,
        "subscript_declaration" => "subscript".to_string(),
        _ => return None,
    };
    let operation = if node.kind() == "computed_getter" {
        "get"
    } else {
        "set"
    };
    Some(format!("{owner}.{operation}"))
}

/// Walk a navigation chain down to its base identifier (`app.get` -> "app",
/// `res.send` -> "res").
fn base_identifier(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    match node.kind().as_ref() {
        "simple_identifier" => Some(node.text().into_owned()),
        "navigation_expression" => base_identifier(&node.field("target")?),
        _ => None,
    }
}

/// Swift exports are `public` top-level declarations: emit an Export entity
/// alongside the primary entity.
fn maybe_export(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    name: &str,
    ctx: &mut ExtractCtx,
) {
    let top_level = node
        .parent()
        .map(|p| p.kind() == "source_file")
        .unwrap_or(false);
    let is_public = node.children().any(|c| {
        c.kind() == "modifiers"
            && c.children()
                .any(|k| k.kind() == "visibility_modifier" && k.text() == "public")
    });
    if top_level && is_public {
        ctx.out.push(entity(
            EntityKind::Export,
            name.to_string(),
            ctx.file_id,
            node,
            EntityMeta::default(),
        ));
    }
}

// ---- domain-specific detection (Vapor-style call shapes) ----

const ROUTE_METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "options", "head"];
const ROUTE_OBJECTS: &[&str] = &["app", "router", "routes"];
const RESPONSE_OBJECTS: &[&str] = &["res", "response"];
const RESPONSE_VERBS: &[&str] = &["send", "json", "text"];

/// Vapor route registration: `app.get("/path") { ... }` -> (method, path).
/// Narrow heuristic: a member call on app/router with an HTTP verb and a
/// string-literal first argument.
fn route_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<(String, String)> {
    let callee = node.children().next()?;
    if callee.kind() != "navigation_expression" {
        return None;
    }
    let method = member_name(&callee)?;
    if !ROUTE_METHODS.contains(&method.as_str()) {
        return None;
    }
    let base = base_identifier(&callee)?;
    if !ROUTE_OBJECTS.contains(&base.as_str()) {
        return None;
    }
    let path = first_arg_text(node)?;
    if !path.starts_with('"') {
        return None;
    }
    Some((method, super::unquote(&path, false)))
}

/// Vapor response call: `res.send(...)` / `res.json(...)` / `res.text(...)`
/// -> body_shape = verb.
fn response_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let callee = node.children().next()?;
    if callee.kind() != "navigation_expression" {
        return None;
    }
    let base = base_identifier(&callee)?;
    if !RESPONSE_OBJECTS.contains(&base.as_str()) {
        return None;
    }
    let verb = member_name(&callee)?;
    if !RESPONSE_VERBS.contains(&verb.as_str()) {
        return None;
    }
    Some(verb)
}

/// Text of the first argument of a call: call_suffix -> value_arguments ->
/// first value_argument -> first named child.
fn first_arg_text(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let suffix = node.children().find(|c| c.kind() == "call_suffix")?;
    let args = suffix.children().find(|c| c.kind() == "value_arguments")?;
    let argument = args.children().find(|c| c.kind() == "value_argument")?;
    argument
        .children()
        .find(|c| c.is_named())
        .map(|c| c.text().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::parse::parse_source;

    #[test]
    fn class_inheritance_specifiers_carry_raw_name_and_owner_as_implements() {
        // CORRECTNESS-001: Swift has no syntactic or naming-convention signal
        // to tell a superclass from a protocol in `class Foo: A, B { }` (a
        // real superclass in first position is syntactically identical to a
        // class implementing only protocols, e.g. `class Foo: Codable,
        // Equatable { }`). Since misreporting Implements-as-Extends is worse
        // than the reverse, every class inheritance_specifier is emitted as
        // Implements — the safer default — rather than guessing the first
        // entry is always a superclass.
        let src = "class Foo: Base, IFoo { }";
        let parsed = parse_source(&SupportLang::Swift, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let extends: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Extends)
            .collect();
        assert!(
            extends.is_empty(),
            "class inheritance is ambiguous; no entry is confidently Extends: {entities:?}"
        );

        let implements: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .collect();
        assert_eq!(implements.len(), 2, "entities: {entities:?}");
        assert_eq!(implements[0].name, "Base");
        assert_eq!(implements[0].enclosing_function.as_deref(), Some("Foo"));
        assert_eq!(implements[1].name, "IFoo");
        assert_eq!(implements[1].enclosing_function.as_deref(), Some("Foo"));
    }

    #[test]
    fn protocols_only_no_superclass_all_implements() {
        // Same ambiguity, the shape the review flagged directly: two
        // protocols and no superclass.
        let src = "class Foo: Codable, Equatable { }";
        let parsed = parse_source(&SupportLang::Swift, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let extends: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Extends)
            .collect();
        assert!(extends.is_empty(), "no superclass present: {entities:?}");

        let implements: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .collect();
        assert_eq!(implements.len(), 2, "entities: {entities:?}");
        assert_eq!(implements[0].name, "Codable");
        assert_eq!(implements[1].name, "Equatable");
    }

    #[test]
    fn generic_supertype_names_are_stripped_to_bare_identifier() {
        // CORRECTNESS-002: `Collection<Element>` must resolve to bare
        // "Collection".
        let src = "class Foo: Collection<Element> { }";
        let parsed = parse_source(&SupportLang::Swift, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let implements: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .collect();
        assert_eq!(implements.len(), 1, "entities: {entities:?}");
        assert_eq!(implements[0].name, "Collection");
    }

    #[test]
    fn computed_property_and_subscript_accessors_are_named_functions() {
        let src = "struct Settings {\n  var value: Int {\n    get { loadValue() }\n    set { saveValue(newValue) }\n  }\n  subscript(index: Int) -> Int {\n    get { loadEntry(index) }\n    set { saveEntry(index, newValue) }\n  }\n}\n";
        let parsed = parse_source(&SupportLang::Swift, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        for (name, call_name) in [
            ("value.get", "loadValue"),
            ("value.set", "saveValue"),
            ("subscript.get", "loadEntry"),
            ("subscript.set", "saveEntry"),
        ] {
            let function = entities
                .iter()
                .find(|e| e.kind == EntityKind::Function && e.name == name)
                .unwrap_or_else(|| panic!("missing {name}: {entities:?}"));
            let call = entities
                .iter()
                .find(|e| e.kind == EntityKind::Call && e.name == call_name)
                .unwrap_or_else(|| panic!("missing {call_name}: {entities:?}"));
            assert!(
                function.span.start_byte <= call.span.start_byte
                    && function.span.end_byte >= call.span.end_byte,
                "{name} must contain {call_name}: {entities:?}"
            );
        }
    }

    #[test]
    fn closure_expressions_are_callable_boundaries() {
        let src = "func outer() { queue { deferred() } }";
        let parsed = parse_source(&SupportLang::Swift, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;
        assert!(
            entities
                .iter()
                .any(|entity| entity.kind == EntityKind::CallableBoundary),
            "closure boundary: {entities:?}"
        );
    }
}
