//! C# entity extraction.
//!
//! C#-specific mapping notes (fixture-driven, superset-safe):
//! - method_declaration / constructor_declaration / local_function_statement
//!   -> Function.
//! - class_declaration -> Class; interface_declaration -> Interface.
//! - field_declaration / local_declaration_statement -> Variable, one Entity
//!   per variable_declarator name (const fields parse as field_declaration
//!   with a `const` modifier; there is no constant_declaration node kind).
//! - parameter -> Parameter.
//! - Exports: C# has no export statement; the closest analog is `public`
//!   visibility on a top-level type, so a top-level (parent is the
//!   compilation_unit or a namespace declaration) `public` class/interface
//!   yields an Export entity alongside its Class/Interface entity.
//! - Catch/Throw: native try/catch — catch_clause -> Catch (named after the
//!   exception variable from catch_declaration), throw_statement -> Throw
//!   (named after the thrown expression).
//! - Route: ASP.NET minimal API `app.MapGet("/path", handler)` selector calls
//!   (MapGet/MapPost/MapPut/MapDelete/MapPatch) with a string-literal first
//!   argument.
//! - Response: minimal API `Results.Ok(...)` / `Results.Json(...)` calls ->
//!   body_shape.

use crate::extract::entity::{EntityMeta, ExtractCtx, entity};
use crate::extract::field_name;
use crate::extract::langs::{first_arg_text, push_type_ref, strip_generic_args};
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named type scope (members inside record
/// `owner_type`). Records and structs carry methods/properties too, so they
/// join class/interface here — otherwise a record's members would have no
/// owning type for class-membership queries (SOLID rules).
pub const TYPE_SCOPES: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "record_declaration",
    "struct_declaration",
];

pub const FUNCTION_SCOPES: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "local_function_statement",
];

/// Entity kinds the fixtures must produce.
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
        // `using System;` / `using static System.Console;` /
        // `using Alias = System.Foo;`. The namespace being imported is the
        // grammar's abstract `type` supertype (concretely `identifier`,
        // `qualified_name`, etc. — not a literal node kind, so it can't be
        // matched by kind directly) and always carries the actual namespace
        // regardless of form; the optional `name` field is only the alias
        // identifier in the `Alias = ...` form, so skip whichever named
        // child that field points at and take what remains. Normalized to a
        // slash-separated path so resolve.rs's shared `/`-segment matching
        // applies unchanged.
        "using_directive" => {
            let alias_range = node.field("name").map(|n| n.range());
            if let Some(ty) = node
                .children()
                .find(|c| c.is_named() && Some(c.range()) != alias_range)
            {
                let spec = ty.text().replace('.', "/");
                ctx.push(EntityKind::Import, spec, node);
            }
        }

        // ---- structural ----
        "method_declaration" | "constructor_declaration" | "local_function_statement" => {
            ctx.push(
                EntityKind::Function,
                field_name(node).unwrap_or_default(),
                node,
            );
        }
        // `record`/`record class`/`record struct` share class semantics: a
        // record can inherit one base record plus interfaces, so it reuses the
        // class heritage heuristic. Previously dropped entirely (audit S3: a
        // `public record` produced no type symbol, only its constructor).
        "class_declaration" | "record_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            maybe_export(node, &name, ctx);
            // `class Foo : Base, IFoo, IBar` — the grammar's `base_list` does
            // not distinguish a base class from interfaces (C# allows at most
            // one base class and requires it listed first when both are
            // present), so *if* the first entry looks like a base class it is
            // Extends and the rest Implements. There is no syntactic signal
            // in the grammar to tell a base class from an interface, so this
            // falls back to the C# ecosystem's `I`-prefix-plus-uppercase
            // naming convention (e.g. `IFoo`) as a heuristic: known
            // limitation — a base class named e.g. `IState` following that
            // convention by coincidence would be wrongly treated as an
            // interface and reclassified as Implements. See CORRECTNESS-001.
            if let Some(base_list) = node.children().find(|c| c.kind() == "base_list") {
                for (i, ty) in base_list_types(&base_list).into_iter().enumerate() {
                    let kind = if i == 0 && !looks_like_interface_name(&ty) {
                        EntityKind::Extends
                    } else {
                        EntityKind::Implements
                    };
                    push_type_ref(ctx, kind, ty, &name, &base_list);
                }
            }
        }
        "interface_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Interface, name.clone(), node);
            maybe_export(node, &name, ctx);
            // `interface IFoo : IBar, IBaz` — interfaces can only extend other
            // interfaces, so every base_list entry here is Extends.
            if let Some(base_list) = node.children().find(|c| c.kind() == "base_list") {
                for ty in base_list_types(&base_list) {
                    push_type_ref(ctx, EntityKind::Extends, ty, &name, &base_list);
                }
            }
        }

        // `struct Foo : IBar` — structs cannot inherit a base class, so every
        // base_list entry is an interface (Implements). Previously dropped
        // entirely (audit S3: a `readonly struct` produced no type symbol).
        "struct_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            maybe_export(node, &name, ctx);
            if let Some(base_list) = node.children().find(|c| c.kind() == "base_list") {
                for ty in base_list_types(&base_list) {
                    push_type_ref(ctx, EntityKind::Implements, ty, &name, &base_list);
                }
            }
        }
        // `enum Color { ... }` (incl. nested enums) — a runtime type of named
        // members; mapped to Class so it surfaces as a declaration. Its `: byte`
        // base is a storage type, not inheritance, so no heritage is emitted.
        "enum_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            maybe_export(node, &name, ctx);
        }

        // ---- properties ----
        // `public int Foo { get; set; }` — a property is declared state, the
        // C# analog of a field; mapped to Variable with its owning type (via
        // `ctx.push`) so class-membership queries see it. Previously never
        // captured (audit S3: 0 properties anywhere). A TypeRef links it to its
        // declared type for receiver resolution, mirroring field handling.
        "property_declaration" => {
            let name = field_name(node).unwrap_or_default();
            if let Some(ty) = node.field("type") {
                push_type_ref_for_var(ctx, &strip_generic_args(&ty.text()), &name, node);
            }
            ctx.push(EntityKind::Variable, name, node);
        }

        // ---- variables / parameters ----
        "field_declaration" | "local_declaration_statement" => {
            let ty = declared_type(node);
            for n in declarator_names(node) {
                if let Some(ty) = &ty {
                    push_type_ref_for_var(ctx, ty, &n, node);
                }
                ctx.push(EntityKind::Variable, n, node);
            }
        }
        "parameter" => {
            let name = field_name(node).unwrap_or_default();
            if let Some(ty) = node.field("type") {
                push_type_ref_for_var(ctx, &strip_generic_args(&ty.text()), &name, node);
            }
            ctx.push(EntityKind::Parameter, name, node);
        }

        // ---- expression-level ----
        "invocation_expression" => {
            let name = node
                .field("function")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Call, name.clone(), node);
            // ASP.NET minimal API routes and results.
            if let Some((method, path)) = minimal_api_route_of(node) {
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
            if let Some(body_shape) = results_response_of(node) {
                ctx.out.push(Entity {
                    kind: EntityKind::Response,
                    name: name.clone(),
                    file_id: ctx.file_id,
                    span: crate::extract::span_of(node),
                    enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
                    method: None,
                    path: None,
                    status: None,
                    body_shape: Some(body_shape),
                    body_minhash: None,
                    is_async: None,
                    is_test: false,
                    owner_type: None,
                });
            }
        }
        "object_creation_expression" => {
            let name = node
                .field("type")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Call, name, node);
        }
        "member_access_expression" => {
            let name = node
                .field("name")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::MemberAccess, name, node);
        }
        "integer_literal"
        | "real_literal"
        | "string_literal"
        | "character_literal"
        | "boolean_literal"
        | "null_literal"
        | "verbatim_string_literal" => {
            if !ctx.in_type {
                ctx.push(EntityKind::Literal, node.text().into_owned(), node);
            }
        }

        // ---- error handling / control flow ----
        "catch_clause" => {
            let name = node
                .children()
                .find(|c| c.kind() == "catch_declaration")
                .and_then(|c| field_name(&c))
                .unwrap_or_default();
            ctx.push(EntityKind::Catch, name, node);
        }
        "throw_statement" => {
            let name = node
                .children()
                .skip(1)
                .find(|c| c.kind() != ";")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Throw, name, node);
        }
        "if_statement"
        | "for_statement"
        | "foreach_statement"
        | "while_statement"
        | "do_statement"
        | "switch_statement"
        | "return_statement"
        | "break_statement"
        | "continue_statement"
        | "try_statement"
        | "conditional_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        // ---- decorators (attributes) ----
        // `[ApiController] class Foo { ... }` / `[Override] void Bar() {}` —
        // `attribute_list` is a direct child of the declaration it decorates,
        // so its owner is simply that parent's own name. One Decorator entity
        // per `attribute` inside the list (`[Foo, Bar]` -> two entities).
        "attribute_list" => {
            if let Some(owner_node) = node.parent() {
                let owner = field_name(&owner_node).unwrap_or_default();
                for attr in node.children().filter(|c| c.kind() == "attribute") {
                    let name = field_name(&attr).unwrap_or_default();
                    ctx.out.push(Entity {
                        kind: EntityKind::Decorator,
                        name,
                        file_id: ctx.file_id,
                        span: crate::extract::span_of(&attr),
                        enclosing_function: Some(owner.clone()),
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

        _ => {}
    }
}

/// Emit a `TypeRef` entity linking a variable/parameter to its declared type:
/// `name` = the type, `enclosing_function` = the variable's own name (so call
/// resolution can look the receiver's static type up by variable name). Uses
/// [`entity`] rather than `ctx.push` because `push` would set
/// `enclosing_function` to the enclosing *method*, not the variable.
fn push_type_ref_for_var(
    ctx: &mut ExtractCtx<'_>,
    ty: &str,
    var_name: &str,
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) {
    if ty.is_empty() || var_name.is_empty() {
        return;
    }
    ctx.out.push(entity(
        EntityKind::TypeRef,
        ty.to_owned(),
        ctx.file_id,
        node,
        EntityMeta {
            enclosing: Some(var_name.to_owned()),
            owner_type: ctx.type_scope.map(|s| s.to_owned()),
            ..Default::default()
        },
    ));
}

/// The declared type of a `field_declaration`/`local_declaration_statement`
/// (the `type` child of its inner `variable_declaration`), generic arguments
/// stripped to the bare type name (`List<User>` -> `List`).
fn declared_type(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.children()
        .find(|c| c.kind() == "variable_declaration")
        .and_then(|decl| decl.field("type"))
        .map(|ty| strip_generic_args(&ty.text()))
}

/// Names of a `field_declaration`/`local_declaration_statement`: both wrap a
/// `variable_declaration` holding one or more `variable_declarator`s.
fn declarator_names(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for child in node.children() {
        if child.kind() == "variable_declaration" {
            for decl in child.children() {
                if decl.kind() == "variable_declarator"
                    && let Some(n) = field_name(&decl)
                {
                    names.push(n);
                }
            }
        }
    }
    names
}

/// Names of the `type` children of a `base_list` node (`class Foo : Base,
/// IFoo` -> `["Base", "IFoo"]`), in source order.
/// C# ecosystem naming convention for interfaces: `I` followed by an
/// uppercase letter (e.g. `IFoo`, not `Int32`). Not a language rule — a
/// heuristic used only to disambiguate the first `base_list` entry when no
/// syntactic signal distinguishes a base class from an interface. See
/// CORRECTNESS-001.
fn looks_like_interface_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('I')) && matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
}

fn base_list_types(base_list: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    base_list
        .children()
        .filter(|c| c.is_named())
        .map(|c| strip_generic_args(&c.text()))
        .collect()
}

/// C# exports are `public` top-level types: emit an Export entity alongside
/// the Class/Interface entity for a `public` declaration whose parent is the
/// compilation unit or a namespace declaration.
fn maybe_export(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    name: &str,
    ctx: &mut ExtractCtx,
) {
    let top_level = node
        .parent()
        .map(|p| {
            matches!(
                p.kind().as_ref(),
                "compilation_unit" | "file_scoped_namespace_declaration" | "namespace_declaration"
            )
        })
        .unwrap_or(false);
    let is_public = node
        .children()
        .any(|c| c.kind() == "modifier" && c.text().into_owned().trim() == "public");
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

/// ASP.NET minimal API route: `app.MapGet("/path", handler)` ->
/// (HTTP method, path). Narrow heuristic: member-access callee on `app` whose
/// member is a Map* verb with a string-literal first argument.
fn minimal_api_route_of(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<(String, String)> {
    const ROUTES: &[(&str, &str)] = &[
        ("MapGet", "GET"),
        ("MapPost", "POST"),
        ("MapPut", "PUT"),
        ("MapDelete", "DELETE"),
        ("MapPatch", "PATCH"),
    ];
    let fn_node = node.field("function")?;
    if fn_node.kind() != "member_access_expression" {
        return None;
    }
    let verb = fn_node.field("name")?.text().into_owned();
    let method = ROUTES.iter().find(|(n, _)| *n == verb).map(|(_, m)| m)?;
    let base = fn_node.field("expression")?.text().into_owned();
    if base != "app" {
        return None;
    }
    let path = first_arg_text(node)?;
    if !path.starts_with('"') {
        return None;
    }
    Some((method.to_string(), super::unquote(&path, false)))
}

/// ASP.NET minimal API result: `Results.Ok(...)` / `Results.Json(...)` ->
/// body_shape (lowercased verb).
fn results_response_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let fn_node = node.field("function")?;
    if fn_node.kind() != "member_access_expression" {
        return None;
    }
    let base = fn_node.field("expression")?.text().into_owned();
    if base != "Results" {
        return None;
    }
    let verb = fn_node.field("name")?.text().into_owned();
    match verb.as_str() {
        "Ok" | "Json" => Some(verb.to_ascii_lowercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::parse::parse_source;

    #[test]
    fn extends_implements_entities_carry_raw_name_and_owner() {
        let src = "class Foo : Base, IFoo { }";
        let parsed = parse_source(&SupportLang::CSharp, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let extends: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Extends)
            .collect();
        assert_eq!(extends.len(), 1, "entities: {entities:?}");
        assert_eq!(extends[0].name, "Base");
        assert_eq!(extends[0].enclosing_function.as_deref(), Some("Foo"));

        let implements: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .collect();
        assert_eq!(implements.len(), 1, "entities: {entities:?}");
        assert_eq!(implements[0].name, "IFoo");
        assert_eq!(implements[0].enclosing_function.as_deref(), Some("Foo"));
    }

    #[test]
    fn interfaces_only_no_base_class_all_implements() {
        // CORRECTNESS-001: no base class present, only interfaces (both
        // following the `I`-prefix convention) — none should be misclassified
        // as Extends just because it's listed first.
        let src = "class Foo : IFoo, IBar { }";
        let parsed = parse_source(&SupportLang::CSharp, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let extends: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Extends)
            .collect();
        assert!(extends.is_empty(), "no base class present: {entities:?}");

        let implements: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .collect();
        assert_eq!(implements.len(), 2, "entities: {entities:?}");
        assert_eq!(implements[0].name, "IFoo");
        assert_eq!(implements[1].name, "IBar");
    }

    #[test]
    fn generic_supertype_names_are_stripped_to_bare_identifier() {
        // CORRECTNESS-002: `class Foo : Base<T>` must yield "Base", not
        // "Base<T>", so it matches the in-repo `Base` entity name.
        let src = "class Foo : Base<T> { }";
        let parsed = parse_source(&SupportLang::CSharp, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let extends: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Extends)
            .collect();
        assert_eq!(extends.len(), 1, "entities: {entities:?}");
        assert_eq!(extends[0].name, "Base");
    }

    #[test]
    fn records_structs_enums_and_properties_are_captured() {
        // Audit S3: record/struct/enum type declarations and properties were
        // never emitted (only their constructors appeared, mis-labeled).
        let src = "public record Step : Base { public int X { get; } }\n\
                   public struct Purpose : IEquatable<Purpose> { }\n\
                   public class Kernel { public IServiceProvider Services { get; } public enum Behavior { A } }\n";
        let parsed = parse_source(&SupportLang::CSharp, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let classes: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Class)
            .map(|e| e.name.as_str())
            .collect();
        for expect in ["Step", "Purpose", "Kernel", "Behavior"] {
            assert!(
                classes.contains(&expect),
                "missing class {expect}: {classes:?}"
            );
        }
        // Properties are captured as Variable carrying their owning type.
        let x = entities
            .iter()
            .find(|e| e.kind == EntityKind::Variable && e.name == "X")
            .expect("property X captured");
        assert_eq!(x.owner_type.as_deref(), Some("Step"));
        assert!(
            entities.iter().any(|e| e.kind == EntityKind::Variable
                && e.name == "Services"
                && e.owner_type.as_deref() == Some("Kernel")),
            "property Services with owner: {entities:?}"
        );
        // A record inherits a base (Extends); a struct only implements.
        assert!(
            entities
                .iter()
                .any(|e| e.kind == EntityKind::Extends && e.name == "Base"),
            "record base as Extends: {entities:?}"
        );
        assert!(
            entities
                .iter()
                .any(|e| e.kind == EntityKind::Implements && e.name == "IEquatable"),
            "struct interface as Implements: {entities:?}"
        );
    }

    #[test]
    fn decorator_entity_carries_attribute_name_and_owner() {
        let src = "[ApiController]\nclass Foo { }";
        let parsed = parse_source(&SupportLang::CSharp, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let decorators: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Decorator)
            .collect();
        assert_eq!(decorators.len(), 1, "entities: {entities:?}");
        assert_eq!(decorators[0].name, "ApiController");
        assert_eq!(decorators[0].enclosing_function.as_deref(), Some("Foo"));
    }
}
