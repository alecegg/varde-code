//! Kotlin entity extraction.
//!
//! Mapping notes (fixture-driven, superset-safe; node kinds from
//! tree-sitter-kotlin-sg 0.4.1):
//! - `function_declaration` -> Function (top-level functions AND methods).
//! - `class_declaration` -> Class; declarations whose keyword is
//!   `interface`/`fun interface` -> Interface (this grammar folds interfaces
//!   into `class_declaration`, distinguished by the leading keyword).
//! - `variable_declaration` -> Variable, one per declared name. This also
//!   fires for the `variable_declaration` nested inside `property_declaration`,
//!   so locals, properties, and `const val` all yield exactly one entity.
//! - `parameter` -> Parameter (function parameter list entries).
//! - Export: Kotlin has no export statement; a `public` *top-level* declaration
//!   (parent is `source_file`) yields an Export entity alongside its primary
//!   entity. Narrow: top-level Function/Class/Interface/property only.
//! - `call_expression` -> Call (callee text). Also the anchor for
//!   domain-specific Ktor detection (see below).
//! - `navigation_expression` -> MemberAccess (the `simple_identifier` inside
//!   the trailing `navigation_suffix`).
//! - Literals: integer/real/string/boolean/null/character, excluding type
//!   contexts.
//! - Catch/Throw: `catch_block` -> Catch (named after the exception variable);
//!   `jump_expression` starting with `throw` -> Throw (named after the thrown
//!   expression text).
//! - ControlFlow: if/when/for/while/do-while/try, and the return/break/continue
//!   `jump_expression`s. (Kotlin has no ternary operator.)
//! - Route: Ktor DSL shape `routing { get("/path") { ... } }` — a
//!   `call_expression` on an HTTP verb whose first argument is a string literal
//!   and whose enclosing context is a `routing`/`route` block. Narrow,
//!   fixture-driven.
//! - Response: Ktor `call.respondText(...)` / `call.respond(...)` /
//!   `call.respondBytes(...)` — `call` member call -> body_shape = verb.

use crate::extract::entity::{EntityMeta, ExtractCtx, entity};
use crate::extract::langs::{push_type_ref, strip_generic_args};
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope.
pub const TYPE_SCOPES: &[&str] = &[
    "class_declaration",
    "object_declaration",
    "companion_object",
];
pub const FUNCTION_SCOPES: &[&str] = &["function_declaration"];

/// Entity kinds the fixtures must produce (all 14 expressible in Kotlin).
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
        // `import com.foo.Bar` / `import com.foo.*` / `import com.foo.Bar as
        // Baz`. No dedicated path field on this grammar, so the dotted path
        // is recovered from node text; the `as Alias` suffix and a trailing
        // `.*` wildcard segment are both stripped (neither names a single
        // file). Normalized to a slash-separated path so resolve.rs's shared
        // `/`-segment matching applies unchanged.
        "import_header" => {
            let spec = kotlin_import_path(node);
            ctx.push(EntityKind::Import, spec, node);
        }

        // ---- structural ----
        "function_declaration" => {
            let name = first_identifier(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name.clone(), node);
            maybe_export(node, &name, ctx);
        }
        "class_declaration" => {
            let name = first_identifier(node).unwrap_or_default();
            let kind = if is_interface(node) {
                EntityKind::Interface
            } else {
                EntityKind::Class
            };
            ctx.push(kind, name.clone(), node);
            maybe_export(node, &name, ctx);
            // `class Foo : Base(), IFoo` — a `delegation_specifier` wrapping a
            // `constructor_invocation` (`Base()`, call syntax) names the
            // superclass; one wrapping a bare `user_type` (`IFoo`, no call
            // syntax) names an implemented interface.
            for spec in node
                .children()
                .filter(|c| c.kind() == "delegation_specifier")
            {
                if let Some((edge_kind, ty_name)) = delegation_target(&spec) {
                    push_type_ref(ctx, edge_kind, ty_name, &name, &spec);
                }
            }
        }
        // `object Registry : Base` / `companion object Key` — Kotlin singleton
        // and companion declarations. Previously dropped entirely (audit S3:
        // top-level `object` and `companion object` were absent from
        // symbols_in_file). Mapped to Class (a named type). An unnamed companion
        // object defaults to "Companion", Kotlin's implicit name for it.
        "object_declaration" | "companion_object" => {
            let name = first_identifier(node).unwrap_or_else(|| {
                if kind == "companion_object" {
                    "Companion".to_string()
                } else {
                    String::new()
                }
            });
            ctx.push(EntityKind::Class, name.clone(), node);
            maybe_export(node, &name, ctx);
            for spec in node
                .children()
                .filter(|c| c.kind() == "delegation_specifier")
            {
                if let Some((edge_kind, ty_name)) = delegation_target(&spec) {
                    push_type_ref(ctx, edge_kind, ty_name, &name, &spec);
                }
            }
        }
        // Top-level `public` property: Export alongside the Variable entity
        // emitted for its nested variable_declaration.
        "property_declaration" => {
            if let Some(name) = first_identifier(node) {
                maybe_export(node, &name, ctx);
            }
        }

        // ---- variables ----
        // One Entity per declared name; also fires for the variable_declaration
        // nested under property_declaration (locals, properties, const val).
        "variable_declaration" => {
            // One Entity per declared name; also fires for the variable_declaration
            // nested under property_declaration (locals, properties, const val).
            if let Some(name) = first_identifier(node).filter(|n| n != "_") {
                ctx.push(EntityKind::Variable, name, node);
            }
        }

        // ---- parameters ----
        "parameter" => {
            if let Some(name) = first_identifier(node) {
                ctx.push(EntityKind::Parameter, name, node);
            }
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
        // (`call.respondText` -> "respondText").
        "navigation_expression" => {
            if let Some(name) = member_name(node) {
                ctx.push(EntityKind::MemberAccess, name, node);
            }
        }
        "integer_literal" | "real_literal" | "string_literal" | "boolean_literal"
        | "null_literal" | "character_literal" => {
            if !ctx.in_type {
                ctx.push(EntityKind::Literal, node.text().into_owned(), node);
            }
        }

        // ---- error handling ----
        "catch_block" => {
            let name = node
                .children()
                .find(|c| c.kind() == "simple_identifier")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Catch, name, node);
        }
        // return/break/continue/throw all parse as jump_expression; the
        // leading keyword distinguishes Throw from control flow.
        "jump_expression" => {
            let text = node.text().into_owned();
            if text.starts_with("throw") {
                // The thrown expression is the first named child (the `throw`
                // keyword is an anonymous token).
                let name = node
                    .children()
                    .find(|c| c.is_named())
                    .map(|n| n.text().into_owned())
                    .unwrap_or_default();
                ctx.push(EntityKind::Throw, name, node);
            } else {
                let name = if text.starts_with("return") {
                    "return_statement"
                } else if text.starts_with("break") {
                    "break_statement"
                } else if text.starts_with("continue") {
                    "continue_statement"
                } else {
                    "jump_expression"
                };
                ctx.push(EntityKind::ControlFlow, name.to_string(), node);
            }
        }

        // ---- control flow ----
        "if_expression" | "when_expression" | "for_statement" | "while_statement"
        | "do_while_statement" | "try_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        // ---- annotations ----
        // Any annotation (`@RestController`, `@GetMapping("/x")`, ...) ->
        // a Decorator entity whose `name` is the annotation's type identifier
        // and whose `enclosing_function` is the annotated declaration's own
        // name. This is what lets Kotlin Spring/Micronaut controllers be
        // role-tagged (the grammar emits no `name` field on `annotation`, so
        // the type name is recovered by descending to the first
        // `type_identifier`, which nests under `user_type` for `@Foo` and
        // under `constructor_invocation` for `@Foo("x")`).
        "annotation" => {
            if let (Some(name), Some(owner)) =
                (annotation_type_name(node), annotation_owner_name(node))
            {
                let (method, path) = kotlin_route_meta(node, &name);
                ctx.out.push(Entity {
                    kind: EntityKind::Decorator,
                    name,
                    file_id: ctx.file_id,
                    span: crate::extract::span_of(node),
                    enclosing_function: Some(owner),
                    method,
                    path,
                    status: None,
                    body_shape: None,
                    body_minhash: None,
                    is_async: None,
                    is_test: false,
                    owner_type: None,
                });
            }
        }

        _ => {}
    }
}

/// Route `(method, path)` carried by a Kotlin Spring/Micronaut annotation, for
/// stamping onto its `Decorator` entity so `entrypoints::detect` can render the
/// handler as `"<VERB> <path>"`. Verb from the annotation name
/// (`@GetMapping` -> GET); a prefix annotation (`@RequestMapping("/api")`)
/// contributes only its base path. `(None, None)` for non-route annotations.
fn kotlin_route_meta(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    name: &str,
) -> (Option<String>, Option<String>) {
    let verb = super::http_verb_for_annotation(name);
    if verb.is_none() && !super::is_route_prefix_annotation(name) {
        return (None, None);
    }
    (
        verb.map(|v| v.to_string()),
        kotlin_annotation_first_string(node),
    )
}

/// First string-literal argument of a Kotlin `annotation` node
/// (`@GetMapping("/{id}")` -> `/{id}`), searched depth-first so it is found
/// under the `constructor_invocation`/`value_arguments` wrappers. The Kotlin
/// grammar wraps the text in `string_literal` with `"` delimiters, stripped
/// by `unquote`.
fn kotlin_annotation_first_string(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    for child in node.children() {
        if child.kind().as_ref().contains("string_literal") {
            return Some(super::unquote(&child.text(), false));
        }
        if let Some(found) = kotlin_annotation_first_string(&child) {
            return Some(found);
        }
    }
    None
}

/// The annotation's type name: the first `type_identifier` in pre-order under
/// an `annotation` node. Handles both `@Foo` (`annotation > user_type >
/// type_identifier`) and `@Foo("x")` (`annotation > constructor_invocation >
/// user_type > type_identifier`).
fn annotation_type_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    for child in node.children() {
        if child.kind() == "type_identifier" {
            return Some(child.text().into_owned());
        }
        if let Some(found) = annotation_type_name(&child) {
            return Some(found);
        }
    }
    None
}

/// Nearest enclosing declaration's own name for an `annotation` node: the
/// class/interface or function this annotation is attached to (climbing
/// ancestors past the intervening `modifiers` wrapper).
fn annotation_owner_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    const OWNER_KINDS: &[&str] = &["class_declaration", "function_declaration"];
    node.ancestors()
        .find(|a| OWNER_KINDS.contains(&a.kind().as_ref()))
        .and_then(|a| first_identifier(&a))
}

/// Name of the type a `class_declaration` introduces, for `owner_type`
/// linkage on nested methods. The Kotlin grammar exposes no `name` field on
/// `class_declaration` (the type name is a bare `type_identifier` child), so
/// the generic `field_name` path used by [`super::mod::type_scope_name`]
/// returns None and would stamp an empty owner. Resolve it the same way the
/// class entity's own name is resolved (`first_identifier`).
pub(super) fn type_scope_name(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    first_identifier(node)
}

/// The declaration/parameter name: the first `simple_identifier` (functions,
/// variables, parameters) or `type_identifier` (classes/interfaces) child.
fn first_identifier(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.children()
        .find(|c| c.kind() == "simple_identifier" || c.kind() == "type_identifier")
        .map(|n| n.text().into_owned())
}

/// This grammar folds `interface` (and `fun interface`) declarations into
/// `class_declaration`; distinguish by the keyword between any leading
/// modifiers and the type name.
fn is_interface(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    let mut text = node.text().into_owned();
    if let Some(mods) = node.children().find(|c| c.kind() == "modifiers") {
        let mods_text = mods.text().into_owned();
        if text.starts_with(mods_text.as_str()) {
            text.drain(..mods_text.len());
        }
    }
    let t = text.trim_start();
    t.starts_with("interface") || t.starts_with("fun interface")
}

/// Member name of a navigation_expression: the identifier inside its trailing
/// navigation_suffix.
fn member_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.children()
        .find(|c| c.kind() == "navigation_suffix")
        .and_then(|s| s.children().find(|c| c.kind() == "simple_identifier"))
        .map(|n| n.text().into_owned())
}

/// Kotlin exports are `public` top-level declarations: emit an Export entity
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

// ---- domain-specific detection (Ktor-style call shapes) ----

const ROUTE_METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "options", "head"];
const RESPONSE_VERBS: &[&str] = &["respond", "respondText", "respondBytes", "respondTextBytes"];

/// Ktor route registration: `routing { get("/path") { ... } }` →
/// (method, path). Narrow heuristic: a call on an HTTP verb with a string
/// literal first argument, nested inside a `routing`/`route` call block.
fn route_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<(String, String)> {
    let callee = node.children().next()?;
    if callee.kind() != "simple_identifier" {
        return None;
    }
    let method = callee.text().into_owned();
    if !ROUTE_METHODS.contains(&method.as_str()) {
        return None;
    }
    if !in_routing_block(node) {
        return None;
    }
    let path = first_arg_text(node)?;
    if !path.starts_with('"') {
        return None;
    }
    Some((method, super::unquote(&path, false)))
}

/// True when an ancestor call is `routing { ... }` / `route(...) { ... }`.
fn in_routing_block(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    node.ancestors().skip(1).any(|a| {
        if a.kind() != "call_expression" {
            return false;
        }
        a.children()
            .next()
            .map(|c| {
                let t = c.text().into_owned();
                t == "routing" || t == "route"
            })
            .unwrap_or(false)
    })
}

/// Ktor response call: `call.respondText(...)` / `call.respond(...)` etc. →
/// body_shape = verb.
fn response_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let callee = node.children().next()?;
    if callee.kind() != "navigation_expression" {
        return None;
    }
    let base = callee.children().next()?.text().into_owned();
    if base != "call" {
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

/// Recover the slash-separated import path from an `import_header` node's
/// raw text (no dedicated path field exists on this grammar).
fn kotlin_import_path(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let text = node.text();
    let t = text.trim().trim_start_matches("import").trim();
    let t = t.split(" as ").next().unwrap_or(t).trim();
    let t = t.strip_suffix(".*").unwrap_or(t);
    t.replace('.', "/")
}

/// Classify a `delegation_specifier` child of a `class_declaration`: a
/// `constructor_invocation` (`Base()`, call syntax) names the superclass
/// (Extends); a bare `user_type` (`IFoo`, no call syntax) names an
/// implemented interface (Implements). Returns the raw type name alongside
/// the classified edge kind.
fn delegation_target(
    spec: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<(EntityKind, String)> {
    let inner = spec.children().next()?;
    match inner.kind().as_ref() {
        "constructor_invocation" => {
            let ty = inner.children().find(|c| c.kind() == "user_type")?;
            Some((EntityKind::Extends, strip_generic_args(&ty.text())))
        }
        "user_type" => Some((EntityKind::Implements, strip_generic_args(&inner.text()))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::parse::parse_source;

    #[test]
    fn object_and_companion_object_declarations_are_captured() {
        // Audit S3: top-level `object` and `companion object` were dropped.
        let src = "object Registry {\n  fun reg() {}\n}\nclass Foo {\n  companion object Key\n}\n";
        let parsed = parse_source(&SupportLang::Kotlin, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;
        let classes: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Class)
            .map(|e| e.name.as_str())
            .collect();
        assert!(classes.contains(&"Registry"), "object: {classes:?}");
        assert!(classes.contains(&"Key"), "companion object: {classes:?}");
        // The object's member method records it as owner.
        assert!(
            entities.iter().any(|e| e.kind == EntityKind::Function
                && e.name == "reg"
                && e.owner_type.as_deref() == Some("Registry")),
            "object member owner: {entities:?}"
        );
    }

    #[test]
    fn extends_implements_entities_carry_raw_name_and_owner() {
        let src = "class Foo : Base(), IFoo { }";
        let parsed = parse_source(&SupportLang::Kotlin, src);
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
    fn method_in_class_records_owning_type_and_top_level_function_does_not() {
        // A method nested in a class must carry `owner_type` = the class name;
        // the Kotlin grammar has no `name` field on `class_declaration` (the
        // name is a bare `type_identifier` child), so the type-scope name must
        // be resolved the same way the entity name is (`first_identifier`),
        // not via the generic `field_name` path (which returned None → "").
        let src = "class Foo {\n  fun bar() { }\n}\nfun free() { }\n";
        let parsed = parse_source(&SupportLang::Kotlin, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let bar = entities
            .iter()
            .find(|e| e.kind == EntityKind::Function && e.name == "bar")
            .expect("method bar present");
        assert_eq!(
            bar.owner_type.as_deref(),
            Some("Foo"),
            "class method must record owning type: {entities:?}"
        );

        let free = entities
            .iter()
            .find(|e| e.kind == EntityKind::Function && e.name == "free")
            .expect("top-level free present");
        assert_eq!(
            free.owner_type, None,
            "top-level function must not carry an owner_type: {entities:?}"
        );
    }

    #[test]
    fn generic_supertype_names_are_stripped_to_bare_identifier() {
        // CORRECTNESS-002: `Repository<User>()` / bare `Comparable<Foo>`
        // must resolve to bare "Repository" / "Comparable".
        let src = "class Foo : Repository<User>(), Comparable<Foo> { }";
        let parsed = parse_source(&SupportLang::Kotlin, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let extends: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Extends)
            .collect();
        assert_eq!(extends.len(), 1, "entities: {entities:?}");
        assert_eq!(extends[0].name, "Repository");

        let implements: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .collect();
        assert_eq!(implements.len(), 1, "entities: {entities:?}");
        assert_eq!(implements[0].name, "Comparable");
    }

    #[test]
    fn annotations_emit_decorator_entities_named_and_owned() {
        // Spring-style class + method annotations, both bare (`@RestController`)
        // and call (`@RequestMapping("/x")` / `@GetMapping("/y")`), must each
        // yield a Decorator entity named for the annotation type and owned by
        // the annotated declaration.
        let src = "@RestController\n@RequestMapping(\"/x\")\nclass Foo {\n  @GetMapping(\"/y\")\n  fun bar() {}\n}\n";
        let parsed = parse_source(&SupportLang::Kotlin, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let decorators: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Decorator)
            .collect();

        let find = |name: &str, owner: &str| {
            decorators
                .iter()
                .any(|d| d.name == name && d.enclosing_function.as_deref() == Some(owner))
        };
        assert!(find("RestController", "Foo"), "decorators: {decorators:?}");
        assert!(find("RequestMapping", "Foo"), "decorators: {decorators:?}");
        assert!(find("GetMapping", "bar"), "decorators: {decorators:?}");
    }
}
