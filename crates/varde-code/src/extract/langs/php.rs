//! PHP entity extraction.
//!
//! PHP-specific mapping notes (fixture-driven, superset-safe), verified against
//! the tree-sitter-php grammar with `examples/dump_php.rs`:
//! - `function_definition` / `method_declaration` -> Function (name via field
//!   "name").
//! - `class_declaration` -> Class; `base_clause` (`extends Base`) -> Extends;
//!   `class_interface_clause` (`implements I`) -> Implements (one per listed
//!   interface). `interface_declaration` -> Interface.
//! - `trait_declaration` -> Class. A PHP trait carries method *implementation*
//!   (a horizontal-reuse / mixin unit), so it maps to Class rather than
//!   Interface — unlike Ruby's `module` (which is the interface analogue),
//!   PHP already has a real `interface` keyword, so the trait is the
//!   implementation-bearing kind.
//! - Import: `namespace_use_declaration` (`use App\Models\User;`) — spec taken
//!   from the `namespace_use_clause`, and `require`/`require_once`/`include`/
//!   `include_once` expressions — spec taken from the string argument. `\`
//!   namespace separators are normalized to `/` so resolve.rs's shared
//!   `/`-segment stem matching applies (mirrors Python's dotted-path handling).
//! - Variable: `assignment_expression` / `augmented_assignment_expression`
//!   whose `left` is a `variable_name`, plus typed/untyped property
//!   declarations (`private int $count = 0;` -> the `$count` property element).
//! - Parameter: `simple_parameter` / `variadic_parameter` /
//!   `property_promotion_parameter` inside `formal_parameters` (name via the
//!   `variable_name` child).
//! - Export: carved out — PHP has no export statement; visibility modifiers
//!   (`public`/`private`/`protected`) are not declarations.
//! - Call: `function_call_expression` (callee = `function` field) and
//!   `member_call_expression` / `nullsafe_member_call_expression` (callee =
//!   `name` field; these ALSO emit a MemberAccess named after the method).
//!   `member_access_expression` / `nullsafe_member_access_expression`
//!   (`$this->count`) -> MemberAccess (field "name"). `object_creation_expression`
//!   (`new Foo()`) -> Call named after the constructed class.
//! - Literal: integer/float/string/encapsed_string/boolean/null.
//! - Catch/Throw: `catch_clause` (named after the caught variable) and
//!   `throw_expression` / `throw_statement` (named after the thrown expression,
//!   e.g. the `new X(...)`).
//! - ControlFlow: if/else/switch/case/default/for/foreach/while/do/try/return/
//!   break/continue/match + the ternary (`conditional_expression`).
//! - Route/Response: narrow Laravel shape — a static call `Route::<verb>` with
//!   a string first arg (`Route::get('/x', ...)`) -> Route with method=VERB,
//!   path=string; and Laravel response helpers (`response()->json(...)`,
//!   `view(...)`, `json(...)`, `redirect(...)`, `abort(...)`) -> Response with
//!   body_shape set to the helper name.

use crate::extract::entity::ExtractCtx;
use crate::extract::field_name;
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope.
pub const FUNCTION_SCOPES: &[&str] = &["function_definition", "method_declaration"];

/// Node kinds that introduce a named class/interface/trait type scope (for
/// `Entity::owner_type` linkage on methods).
pub const TYPE_SCOPES: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "trait_declaration",
    "enum_declaration",
];

/// Entity kinds the fixtures must produce. Export is carved out (see module
/// docs); Interface is expressed via `interface`, Route/Response via Laravel.
pub const REQUIRED_KINDS: [EntityKind; 13] = [
    EntityKind::Function,
    EntityKind::Class,
    EntityKind::Interface,
    EntityKind::Variable,
    EntityKind::Parameter,
    EntityKind::Call,
    EntityKind::Literal,
    EntityKind::MemberAccess,
    EntityKind::Catch,
    EntityKind::Throw,
    EntityKind::ControlFlow,
    EntityKind::Route,
    EntityKind::Response,
];

/// HTTP-verb static methods that introduce a Laravel route (`Route::get`).
const HTTP_VERBS: &[&str] = &[
    "get", "post", "put", "patch", "delete", "options", "head", "any", "match",
];

/// Laravel response-producing helpers -> body_shape is the helper name.
const RESPONSE_FUNCTIONS: &[&str] = &[
    "json", "view", "redirect", "abort", "response", "back", "download", "stream",
];

/// Require-family constructs that name a dependency instead of a normal call.
const REQUIRE_KINDS: &[&str] = &[
    "require_expression",
    "require_once_expression",
    "include_expression",
    "include_once_expression",
];

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- structural ----
        "function_definition" | "method_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name, node);
        }
        "class_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            // `class Sub extends Base` -> one Extends entity for the superclass.
            if let Some(base) = node.children().find(|c| c.kind() == "base_clause")
                && let Some(parent) = type_ref_child(&base)
            {
                push_owned(
                    ctx,
                    EntityKind::Extends,
                    parent.text().into_owned(),
                    &name,
                    &parent,
                );
            }
            // `implements A, B` -> one Implements entity per listed interface.
            if let Some(iface) = node
                .children()
                .find(|c| c.kind() == "class_interface_clause")
            {
                for c in iface.children() {
                    if matches!(c.kind().as_ref(), "name" | "qualified_name") {
                        push_owned(
                            ctx,
                            EntityKind::Implements,
                            c.text().into_owned(),
                            &name,
                            &c,
                        );
                    }
                }
            }
        }
        "interface_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Interface, name, node);
        }
        // A trait carries implementation (horizontal reuse / mixin) -> Class.
        "trait_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name, node);
        }

        // ---- imports ----
        "namespace_use_declaration" => {
            for spec in namespace_use_specs(node) {
                ctx.push(EntityKind::Import, spec, node);
            }
        }
        k if REQUIRE_KINDS.contains(&k) => {
            if let Some(spec) = first_string_content(node) {
                ctx.push(EntityKind::Import, normalize_ns(&spec), node);
            }
        }

        // ---- variables ----
        "assignment_expression" | "augmented_assignment_expression" => {
            if let Some(left) = node.field("left")
                && left.kind() == "variable_name"
            {
                ctx.push(EntityKind::Variable, var_name(&left), node);
            }
        }
        // Typed/untyped property declarations: `private int $count = 0;`.
        "property_element" => {
            if let Some(v) = node.children().find(|c| c.kind() == "variable_name") {
                ctx.push(EntityKind::Variable, var_name(&v), node);
            }
        }

        // ---- parameters ----
        "simple_parameter" | "variadic_parameter" | "property_promotion_parameter" => {
            if let Some(v) = node.children().find(|c| c.kind() == "variable_name") {
                ctx.push(EntityKind::Parameter, var_name(&v), node);
            }
        }

        // ---- calls ----
        "function_call_expression" => visit_function_call(node, ctx),
        "member_call_expression" | "nullsafe_member_call_expression" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::MemberAccess, name.clone(), node);
            emit_response_if_helper(node, &name, ctx);
            ctx.push(EntityKind::Call, name, node);
        }
        "scoped_call_expression" => visit_scoped_call(node, ctx),
        "member_access_expression" | "nullsafe_member_access_expression" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::MemberAccess, name, node);
        }
        "object_creation_expression" => {
            // `new Foo(...)` -> Call named after the constructed class.
            if let Some(cls) = node
                .children()
                .find(|c| matches!(c.kind().as_ref(), "name" | "qualified_name"))
            {
                ctx.push(EntityKind::Call, cls.text().into_owned(), node);
            }
        }

        // ---- literals ----
        "integer" | "float" | "string" | "encapsed_string" | "boolean" | "null" => {
            ctx.push(EntityKind::Literal, node.text().into_owned(), node);
        }

        // ---- control-flow / error ----
        "catch_clause" => {
            ctx.push(EntityKind::Catch, catch_var(node), node);
        }
        "throw_expression" | "throw_statement" => {
            let name = node
                .children()
                .find(|c| c.get_inner_node().is_named())
                .map(|c| c.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Throw, name, node);
        }
        "if_statement"
        | "else_clause"
        | "else_if_clause"
        | "switch_statement"
        | "case_statement"
        | "default_statement"
        | "for_statement"
        | "foreach_statement"
        | "while_statement"
        | "do_statement"
        | "try_statement"
        | "return_statement"
        | "break_statement"
        | "continue_statement"
        | "match_expression"
        | "conditional_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        // ---- attributes (PHP 8) ----
        // `#[Route("/x")]` / `#[Get]` on a class or method -> a Decorator
        // entity named for the attribute, owned by the annotated declaration.
        // This is what lets Symfony controllers/actions be role-tagged.
        "attribute" => {
            if let Some(name_node) = node
                .children()
                .find(|c| matches!(c.kind().as_ref(), "name" | "qualified_name"))
                && let Some(owner) = attribute_owner_name(node)
            {
                push_owned(
                    ctx,
                    EntityKind::Decorator,
                    name_node.text().into_owned(),
                    &owner,
                    &name_node,
                );
            }
        }

        _ => {}
    }
}

/// Nearest enclosing declaration's own name for an `attribute` node — the
/// class/interface/trait or function/method the `#[...]` is attached to.
fn attribute_owner_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    const OWNER_KINDS: &[&str] = &[
        "class_declaration",
        "interface_declaration",
        "trait_declaration",
        "function_definition",
        "method_declaration",
    ];
    node.ancestors()
        .find(|a| OWNER_KINDS.contains(&a.kind().as_ref()))
        .and_then(|a| field_name(&a))
}

/// Handle a `function_call_expression`: require/include family is matched at
/// the statement level, so here it is imports? no — plain calls, plus Laravel
/// response helpers (`view(...)`, `json(...)`, `response()`).
fn visit_function_call(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>, ctx: &mut ExtractCtx) {
    let callee = node
        .field("function")
        .map(|f| f.text().into_owned())
        .unwrap_or_default();
    emit_response_if_helper(node, &callee, ctx);
    ctx.push(EntityKind::Call, callee, node);
}

/// Handle a `scoped_call_expression` (`Class::method(...)`): a Laravel
/// `Route::<verb>('/path', ...)` becomes a Route; otherwise a plain Call named
/// after the method.
fn visit_scoped_call(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>, ctx: &mut ExtractCtx) {
    let method = node
        .field("name")
        .map(|n| n.text().into_owned())
        .unwrap_or_default();
    let scope = node
        .field("scope")
        .map(|s| s.text().into_owned())
        .unwrap_or_default();

    if scope == "Route"
        && HTTP_VERBS.contains(&method.as_str())
        && let Some(path) = first_arg_string(node)
    {
        ctx.out.push(Entity {
            kind: EntityKind::Route,
            name: format!("{scope}::{method}"),
            file_id: ctx.file_id,
            span: crate::extract::span_of(node),
            enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
            method: Some(method.to_uppercase()),
            path: Some(path),
            status: None,
            body_shape: None,
            body_minhash: None,
            is_async: None,
            is_test: false,
            owner_type: ctx.type_scope.map(|s| s.to_owned()),
        });
        return;
    }

    ctx.push(EntityKind::Call, method, node);
}

/// Emit a Response entity when `callee` is a Laravel response helper.
fn emit_response_if_helper(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    callee: &str,
    ctx: &mut ExtractCtx,
) {
    if RESPONSE_FUNCTIONS.contains(&callee) {
        ctx.out.push(Entity {
            kind: EntityKind::Response,
            name: callee.to_owned(),
            file_id: ctx.file_id,
            span: crate::extract::span_of(node),
            enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
            method: None,
            path: None,
            status: None,
            body_shape: Some(callee.to_owned()),
            body_minhash: None,
            is_async: None,
            is_test: false,
            owner_type: ctx.type_scope.map(|s| s.to_owned()),
        });
    }
}

/// Push a type-reference entity (`Extends`/`Implements`) owned by `owner`.
fn push_owned(
    ctx: &mut ExtractCtx,
    kind: EntityKind,
    name: String,
    owner: &str,
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) {
    ctx.out.push(Entity {
        kind,
        name: normalize_ns(&name),
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

/// First `name` / `qualified_name` child of a `base_clause` (the superclass).
fn type_ref_child<'t>(
    node: &ast_grep_core::Node<'t, StrDoc<SupportLang>>,
) -> Option<ast_grep_core::Node<'t, StrDoc<SupportLang>>> {
    node.children()
        .find(|c| matches!(c.kind().as_ref(), "name" | "qualified_name"))
}

/// `$name` (variable_name node) -> "name" (drop the leading `$`).
fn var_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.text().trim_start_matches('$').to_owned()
}

/// Import specs for a `namespace_use_declaration`, one per `namespace_use_clause`
/// (`use A\B, C\D;` -> ["A/B", "C/D"]). `\` is normalized to `/`.
fn namespace_use_specs(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let mut specs = Vec::new();
    for child in node.children() {
        if child.kind() == "namespace_use_clause" {
            // The clause text is the dotted path (`App\Models\User` or an
            // aliased `X\Y as Z`); take the leading qualified-name part.
            let spec = child
                .children()
                .find(|c| matches!(c.kind().as_ref(), "qualified_name" | "name"))
                .map(|c| c.text().into_owned())
                .unwrap_or_else(|| child.text().into_owned());
            specs.push(normalize_ns(&spec));
        }
    }
    specs
}

/// First string argument content of a call's `arguments`, if present.
fn first_arg_string(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let args = node.children().find(|c| c.kind() == "arguments")?;
    let arg = args.children().find(|c| c.kind() == "argument")?;
    string_content(&arg.children().find(|c| c.get_inner_node().is_named())?)
}

/// Content of the first string literal anywhere below `node` (used for
/// `require "helper.php"` where the string is a direct child).
fn first_string_content(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.children().find_map(|c| string_content(&c))
}

/// Inner text of a `string` / `encapsed_string` node, joining `string_content`
/// runs (`"helper.php"` -> "helper.php"), ignoring interpolations/quotes.
fn string_content(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    if !matches!(node.kind().as_ref(), "string" | "encapsed_string") {
        return None;
    }
    let mut out = String::new();
    for child in node.children() {
        if child.kind() == "string_content" {
            out.push_str(child.text().as_ref());
        }
    }
    Some(out)
}

/// The caught variable of a `catch_clause` (`catch (\Exception $e)` -> "e"),
/// falling back to the caught type text, then "".
fn catch_var(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    if let Some(v) = node.children().find(|c| c.kind() == "variable_name") {
        return var_name(&v);
    }
    node.children()
        .find(|c| c.kind() == "type_list")
        .map(|c| normalize_ns(c.text().as_ref()))
        .unwrap_or_default()
}

/// Normalize `\`-separated PHP namespaces to `/` so resolve.rs's shared
/// `/`-segment stem matching applies (mirrors Python's dotted-path handling).
/// A leading separator is trimmed (`\Exception` -> "Exception").
fn normalize_ns(spec: &str) -> String {
    spec.trim_start_matches('\\').replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::parse::parse_source;

    fn entities(src: &str) -> Vec<Entity> {
        let parsed = parse_source(&SupportLang::Php, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        extract::extract(&parsed, 0).entities
    }

    fn find<'a>(es: &'a [Entity], kind: EntityKind, name: &str) -> Option<&'a Entity> {
        es.iter().find(|e| e.kind == kind && e.name == name)
    }

    #[test]
    fn class_extends_and_implements() {
        let es = entities(
            "<?php\ninterface I {}\nclass C extends Base implements I {\n  public function m() {}\n}\n",
        );
        assert!(find(&es, EntityKind::Interface, "I").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Class, "C").is_some(), "{es:?}");
        let ext = find(&es, EntityKind::Extends, "Base").expect("Extends Base");
        assert_eq!(ext.enclosing_function.as_deref(), Some("C"));
        let imp = find(&es, EntityKind::Implements, "I").expect("Implements I");
        assert_eq!(imp.enclosing_function.as_deref(), Some("C"));
    }

    #[test]
    fn php8_attributes_emit_decorator_entities_named_and_owned() {
        // Symfony-style `#[Route]` on a class and `#[Get("/x")]` on a method
        // must each yield a Decorator owned by the annotated declaration.
        let es = entities(
            "<?php\n#[Route(\"/api\")]\nclass ApiController extends AbstractController {\n  #[Get(\"/items\")]\n  public function list() {}\n}\n",
        );
        let route = find(&es, EntityKind::Decorator, "Route").expect("#[Route] decorator");
        assert_eq!(route.enclosing_function.as_deref(), Some("ApiController"));
        let get = find(&es, EntityKind::Decorator, "Get").expect("#[Get] decorator");
        assert_eq!(get.enclosing_function.as_deref(), Some("list"));
    }

    #[test]
    fn trait_maps_to_class() {
        let es = entities("<?php\ntrait T {\n  public function shared() { return 1; }\n}\n");
        assert!(find(&es, EntityKind::Class, "T").is_some(), "{es:?}");
        assert!(
            find(&es, EntityKind::Function, "shared").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn use_and_require_become_imports_normalized() {
        let es = entities("<?php\nuse App\\Models\\User;\nrequire_once \"helper.php\";\n");
        assert!(
            find(&es, EntityKind::Import, "App/Models/User").is_some(),
            "namespaced use should normalize \\ to /: {es:?}"
        );
        assert!(
            find(&es, EntityKind::Import, "helper.php").is_some(),
            "require string should be an Import: {es:?}"
        );
    }

    #[test]
    fn member_call_emits_member_access_and_call() {
        let es = entities("<?php\nfunction f($m) {\n  return $m->upper();\n}\n");
        assert!(
            find(&es, EntityKind::MemberAccess, "upper").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Call, "upper").is_some(), "{es:?}");
    }

    #[test]
    fn catch_and_throw() {
        let es = entities(
            "<?php\nfunction f() {\n  try { risky(); } catch (\\Exception $e) { throw new RuntimeException(\"boom\"); }\n}\n",
        );
        assert!(find(&es, EntityKind::Catch, "e").is_some(), "{es:?}");
        // throw is named after the `new RuntimeException("boom")` expression.
        assert!(
            es.iter()
                .any(|e| e.kind == EntityKind::Throw && e.name.contains("RuntimeException")),
            "{es:?}"
        );
    }

    #[test]
    fn laravel_route_and_response() {
        let es = entities(
            "<?php\nRoute::get('/users', 'C@index');\nfunction show() { return view('users.index'); }\n",
        );
        let route = es
            .iter()
            .find(|e| e.kind == EntityKind::Route)
            .expect("route entity");
        assert_eq!(route.method.as_deref(), Some("GET"));
        assert_eq!(route.path.as_deref(), Some("/users"));
        let resp = es
            .iter()
            .find(|e| e.kind == EntityKind::Response)
            .expect("response entity");
        assert_eq!(resp.body_shape.as_deref(), Some("view"));
    }
}
