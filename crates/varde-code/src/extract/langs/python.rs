//! Python entity extraction.
//!
//! Python-specific mapping notes (fixture-driven, superset-safe):
//! - `def` -> Function (function_definition, name via field "name").
//! - `class` -> Class (class_definition).
//! - Interface: carved out — Python has no native interface construct
//!   (ABCs/protocols are runtime classes, not syntax).
//! - Variable: narrow — `assignment` statements whose `left` is an identifier
//!   or a pattern_list of identifiers (`x = 1`, `a, b = 1, 2`).
//! - Parameter: identifiers inside the `parameters` node (plain identifiers,
//!   default/typed variants carry `name`, splat patterns wrap an identifier).
//! - Export: carved out — Python has no export statement; `__all__` is a
//!   convention, not syntax.
//! - Call: `call` node, callee text as name. `attribute` -> MemberAccess
//!   (field "attribute").
//! - Literal: integer/float/string/true/false/none, excluding type contexts.
//! - Catch/Throw: `except_clause` (named after the exception variable) and
//!   `raise_statement` (named after the raised expression).
//! - ControlFlow: if/for/while/with/try/return/break/continue statements +
//!   conditional_expression.
//! - Route: narrow Flask shape — `@app.route("/path")` (or a blueprint
//!   object) decorator; method from `methods=[...]` kwarg, default "GET".
//! - Response: narrow Flask shape — calls to Flask's response-producing
//!   functions (`jsonify`, `render_template`, `make_response`, `redirect`,
//!   `send_file`, `send_from_directory`); body_shape is the function name.

use crate::extract::entity::ExtractCtx;
use crate::extract::field_name;
use crate::extract::langs::first_arg_text;
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope.
pub const FUNCTION_SCOPES: &[&str] = &["function_definition"];

/// Node kinds that introduce a named type scope, so a `def` nested inside a
/// `class` records its owning class on `Entity::owner_type` (see
/// `crate::extract::langs::type_scopes`). This lets navigation surfaces
/// disambiguate common method names — `on` in `event_bus.py` becomes
/// `EventBus.on` — and gives class-membership consumers the owner linkage.
pub const TYPE_SCOPES: &[&str] = &["class_definition"];

/// Entity kinds the fixtures must produce. Interface and Export are carved
/// out (see module docs); Route/Response use narrow Flask-style shapes.
pub const REQUIRED_KINDS: [EntityKind; 12] = [
    EntityKind::Function,
    EntityKind::Class,
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

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- imports ----
        // `import a.b, c as d` -> one Import entity per name in the `name`
        // field (dotted_name directly, or the `name` field of an
        // aliased_import for the `as` form). Dotted paths are normalized to
        // slash-separated form so resolve.rs's shared `/`-segment matching
        // applies unchanged.
        "import_statement" => {
            for name_node in node.field_children("name") {
                if let Some(spec) = python_dotted_path(&name_node) {
                    ctx.push(EntityKind::Import, spec, node);
                    // Record the local module-binding name (`import x as y` -> y,
                    // `import a.b.c` -> c) on `owner_type` so resolve.rs can
                    // resolve a receiver-qualified call (`y.fn()` / `c.fn()`) to
                    // the bound module's file.
                    if let Some(binding) = python_import_binding(&name_node)
                        && let Some(last) = ctx.out.last_mut()
                    {
                        last.owner_type = Some(binding);
                    }
                    mark_if_type_checking_only(node, ctx);
                }
            }
        }
        // `from a.b import c` / `from . import c` / `from ..pkg import c`.
        // `module_name` is either a plain dotted_name (absolute) or a
        // relative_import (leading dots + an optional dotted_name). Leading
        // dots are converted to `./`/`../` segments the same way resolve.rs
        // already resolves Rust's `super::`/`self::` prefixes, so a same-repo
        // relative import resolves via its existing relative-path matching.
        "import_from_statement" => {
            if let Some(module) = node.field("module_name") {
                let mut spec = match module.kind().as_ref() {
                    "relative_import" => python_relative_import_path(&module),
                    _ => python_dotted_path(&module).unwrap_or_default(),
                };
                // `from . import util` / `from ..pkg import util` carry no
                // module part beyond the dots — the imported name is the
                // best guess at the target file (`util` is far more often a
                // sibling module than a symbol re-export at package root).
                if (spec.ends_with('/') || spec.is_empty())
                    && let Some(first_name) = node
                        .field_children("name")
                        .next()
                        .and_then(|n| python_dotted_path(&n))
                {
                    spec.push_str(&first_name);
                }
                if !spec.is_empty() {
                    // Local binding names introduced by this `from PKG import a, b`
                    // — the receiver used at call sites (`a.fn()`). Comma-joined
                    // on `owner_type` (one edge per statement is preserved) so
                    // resolve.rs can resolve `a.fn()` to the sibling module `a`
                    // that defines `fn` (e.g. FastAPI `from app import crud`).
                    let bindings: Vec<String> = node
                        .field_children("name")
                        .filter_map(|n| python_import_binding(&n))
                        .collect();
                    ctx.push(EntityKind::Import, spec, node);
                    if !bindings.is_empty()
                        && let Some(last) = ctx.out.last_mut()
                    {
                        last.owner_type = Some(bindings.join(","));
                    }
                    mark_if_type_checking_only(node, ctx);
                }
            }
        }
        // ---- structural ----
        "function_definition" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name, node);
        }
        "lambda" if node.is_named() => ctx.push_callable_boundary(node),
        "class_definition" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            // `class Sub(Base1, Base2):` -> one Extends entity per base class
            // named in the `superclasses` argument_list. Keyword arguments
            // (e.g. `metaclass=Meta`) are not base classes and are skipped.
            if let Some(superclasses) = node.field("superclasses") {
                for base in superclasses.children() {
                    let base_name = match base.kind().as_ref() {
                        "identifier" | "attribute" => Some(base.text().into_owned()),
                        _ => None,
                    };
                    if let Some(base_name) = base_name {
                        ctx.out.push(Entity {
                            kind: EntityKind::Extends,
                            name: base_name,
                            file_id: ctx.file_id,
                            span: crate::extract::span_of(&base),
                            enclosing_function: Some(name.clone()),
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
        }
        // Flask routes live in decorators, which wrap the definition.
        "decorated_definition" => {
            if let Some(route) = route_of(node) {
                ctx.out.push(Entity {
                    kind: EntityKind::Route,
                    name: "app.route".to_string(),
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
                    owner_type: None,
                });
            }
            // Generic decorator entities: one per `@decorator` line wrapping
            // the `definition` (function_definition or class_definition).
            // `name` is the decorator expression text (e.g. "app.route" for
            // `@app.route(...)`, "staticmethod" for `@staticmethod`);
            // `enclosing_function` is the annotated def/class's own name.
            if let Some(definition) = node.field("definition") {
                let owner = field_name(&definition).unwrap_or_default();
                for decorator in node.children().filter(|c| c.kind() == "decorator") {
                    if let Some(dec_name) = python_decorator_name(&decorator) {
                        // Stamp method+path onto route decorators (FastAPI
                        // `@app.get("/x")`, Flask `@bp.route("/x", methods=...)`)
                        // so `entrypoints::detect` can surface the handler as
                        // `"<VERB> <path>"`; `enclosing_function` already names
                        // the decorated function (`owner`).
                        let (method, path) = python_route_meta(&decorator, &dec_name);
                        ctx.out.push(Entity {
                            kind: EntityKind::Decorator,
                            name: dec_name,
                            file_id: ctx.file_id,
                            span: crate::extract::span_of(&decorator),
                            enclosing_function: Some(owner.clone()),
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
            }
        }

        // ---- variables ----
        // Narrow: assignment where `left` is an identifier or identifier list.
        "assignment" => {
            if let Some(left) = node.field("left") {
                for name in assignment_target_names(&left) {
                    ctx.push(EntityKind::Variable, name, node);
                }
            }
        }

        // ---- parameters ----
        "parameters" => {
            for name in parameter_names(node) {
                ctx.push(EntityKind::Parameter, name, node);
            }
        }

        // ---- expression-level ----
        "call" => {
            let name = node
                .field("function")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Call, name.clone(), node);
            // Flask responses: jsonify(...), render_template(...), etc.
            if let Some(body_shape) = response_of(node) {
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
        "attribute" => {
            let name = node
                .field("attribute")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::MemberAccess, name, node);
        }
        // Literals: exclude values inside type annotations (e.g.
        // `Literal["a"]` in `x: Literal["a"]`).
        "integer" | "float" | "string" | "true" | "false" | "none" => {
            if !in_python_type_context(node) {
                ctx.push(EntityKind::Literal, node.text().into_owned(), node);
            }
        }

        // ---- control-flow / error ----
        // `except ValueError as err` -> Catch, named after the exception var.
        "except_clause" => {
            let name = exception_var(node);
            ctx.push(EntityKind::Catch, name, node);
        }
        // `raise ValueError("empty")` -> Throw, named after the raised expr.
        "raise_statement" => {
            let name = node
                .children()
                .nth(1)
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Throw, name, node);
        }
        "if_statement"
        | "for_statement"
        | "while_statement"
        | "with_statement"
        | "try_statement"
        | "return_statement"
        | "break_statement"
        | "continue_statement"
        | "conditional_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// Names assigned by an assignment statement's `left` (an identifier or a
/// pattern_list of identifiers). Narrow by design: only plain identifier
/// targets produce Variable entities.
fn assignment_target_names(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let mut names = Vec::new();
    match node.kind().as_ref() {
        "identifier" => {
            let t = node.text().into_owned();
            if t != "_" {
                names.push(t);
            }
        }
        "pattern_list" => {
            for child in node.children() {
                if child.kind() == "identifier" {
                    let t = child.text().into_owned();
                    if t != "_" {
                        names.push(t);
                    }
                }
            }
        }
        _ => {}
    }
    names
}

/// Parameter names inside a `parameters` node: plain identifiers,
/// default/typed defaults (field "name"), typed/splat patterns wrapping an
/// identifier.
fn parameter_names(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let mut names = Vec::new();
    for child in node.children() {
        let name = match child.kind().as_ref() {
            "identifier" => Some(child.text().into_owned()),
            "default_parameter" | "typed_default_parameter" => field_name(&child),
            "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => child
                .children()
                .find(|c| c.kind() == "identifier")
                .map(|c| c.text().into_owned()),
            _ => None,
        };
        if let Some(name) = name
            && name != "_"
        {
            names.push(name);
        }
    }
    names
}

/// The exception variable of an `except` clause (`except ValueError as err`
/// -> "err"; bare `except:` -> "").
fn exception_var(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    for descendant in node.dfs() {
        if descendant.kind() == "as_pattern_target"
            && let Some(id) = descendant.children().find(|c| c.kind() == "identifier")
        {
            return id.text().into_owned();
        }
    }
    String::new()
}

/// If `import_node` is lexically nested inside an `if TYPE_CHECKING:` block
/// (the standard `typing.TYPE_CHECKING` guard used to import a name only for
/// type hints, never at runtime), tag the entity just pushed for it as
/// `body_shape = "type_only"`. Reused (not a new column) because Import
/// entities don't otherwise use `body_shape` — see `circular_import.toml`,
/// which excludes these from the runtime import graph: Python never executes
/// a `TYPE_CHECKING`-guarded import, so it can't participate in a real
/// load-order cycle.
fn mark_if_type_checking_only(
    import_node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    ctx: &mut ExtractCtx,
) {
    let guarded = import_node.ancestors().skip(1).any(|a| {
        a.kind().as_ref() == "if_statement"
            && a.field("condition")
                .is_some_and(|c| c.text().as_ref().contains("TYPE_CHECKING"))
    });
    if guarded && let Some(last) = ctx.out.last_mut() {
        last.body_shape = Some("type_only".to_string());
    }
}

/// True when the node sits inside a Python type annotation. Python's
/// annotation node kind is `type`; `Literal[...]`/`Union[...]` spellings use
/// generic_type/union_type.
fn in_python_type_context(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    const TYPE_KINDS: &[&str] = &[
        "type",
        "generic_type",
        "union_type",
        "splat_type",
        "type_parameter",
        "type_argument_list",
    ];
    node.ancestors()
        .skip(1)
        .any(|a| TYPE_KINDS.contains(&a.kind().as_ref()))
}

// ---- domain-specific detection (Flask-style shapes) ----

const ROUTE_OBJECTS: &[&str] = &["app", "blueprint"];
const RESPONSE_FUNCTIONS: &[&str] = &[
    "jsonify",
    "render_template",
    "make_response",
    "redirect",
    "send_file",
    "send_from_directory",
];

/// Flask-style route: a `@app.route("/path", methods=["POST"])` decorator on
/// a decorated_definition. Returns (method, path); method defaults to "GET"
/// unless a single-string `methods=[...]` kwarg is present. The decorator
/// attribute is literally `route` — the HTTP verb lives in the `methods`
/// keyword argument.
fn route_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<(String, String)> {
    for decorator in node.children() {
        if decorator.kind() != "decorator" {
            continue;
        }
        let Some(call) = decorator.children().find(|c| c.kind() == "call") else {
            continue;
        };
        let fn_node = call.field("function")?;
        if fn_node.kind() != "attribute" {
            continue;
        }
        let attr = fn_node.field("attribute")?.text();
        if attr != "route" {
            continue;
        }
        let base = fn_node.field("object")?.text();
        if !ROUTE_OBJECTS.contains(&base.as_ref()) {
            continue;
        }
        let path = first_arg_text(&call)?;
        if !path.starts_with('"') && !path.starts_with('\'') {
            continue;
        }
        let method = methods_kwarg(&call).unwrap_or_else(|| "GET".to_string());
        return Some((method, super::unquote(&path, false)));
    }
    None
}

/// `methods=["POST"]` keyword argument of a route call -> "POST" (only when
/// the list holds a single string literal).
fn methods_kwarg(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let args = node.field("arguments")?;
    for child in args.children() {
        if child.kind() == "keyword_argument"
            && child.field("name").map(|n| n.text()) == Some("methods".into())
        {
            let value = child.field("value")?;
            if value.kind() == "list"
                && let Some(s) = value.children().find(|c| c.kind() == "string")
            {
                return Some(super::unquote(&s.text(), false));
            }
        }
    }
    None
}

/// Route `(method, path)` carried by a single `@decorator` node, for stamping
/// onto its `Decorator` entity. Recognizes FastAPI/router verb decorators
/// (`@app.get("/x")`, `@router.post("/x")`) via the shared verb map, and Flask
/// `@<obj>.route("/x", methods=[...])` (verb from the kwarg, `GET` default).
/// Returns `(None, None)` for a non-route decorator (`@staticmethod`, ...).
fn python_route_meta(
    decorator: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    dec_name: &str,
) -> (Option<String>, Option<String>) {
    let call = decorator.children().find(|c| c.kind() == "call");
    let path = call.as_ref().and_then(|c| {
        let p = first_arg_text(c)?;
        (p.starts_with('"') || p.starts_with('\'')).then(|| super::unquote(&p, false))
    });
    let method = match super::http_verb_for_annotation(dec_name) {
        Some(v) => Some(v.to_string()),
        // Flask `@<obj>.route` — the verb lives in the `methods=[...]` kwarg.
        None if dec_name.ends_with(".route") => Some(
            call.as_ref()
                .and_then(methods_kwarg)
                .unwrap_or_else(|| "GET".to_string()),
        ),
        None => None,
    };
    if method.is_none() {
        return (None, None);
    }
    (method, path)
}

/// Flask-style response: a call to one of Flask's response-producing
/// functions -> body_shape is the function name.
fn response_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let fn_node = node.field("function")?;
    if fn_node.kind() != "identifier" {
        return None;
    }
    let name = fn_node.text();
    if RESPONSE_FUNCTIONS.contains(&name.as_ref()) {
        Some(name.into_owned())
    } else {
        None
    }
}

/// The local name a single import clause binds — the receiver used at call
/// sites: the alias for `x as y` (-> `y`), else the last dotted segment
/// (`a.b.c` -> `c`, `crud` -> `crud`). Used to populate `Import.owner_type`
/// for receiver-aware call resolution in resolve.rs.
fn python_import_binding(
    name_node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> Option<String> {
    if name_node.kind() == "aliased_import"
        && let Some(alias) = name_node.field("alias")
    {
        return Some(alias.text().into_owned());
    }
    let text = name_node.text();
    let last = text.rsplit('.').next().unwrap_or(&text).trim();
    (!last.is_empty()).then(|| last.to_owned())
}

/// Slash-normalized dotted path from a `dotted_name` (or the `dotted_name`
/// nested in an `aliased_import`'s `name` field for `import x as y`).
fn python_dotted_path(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let dotted = if node.kind() == "aliased_import" {
        node.field("name")?
    } else {
        node.clone()
    };
    Some(dotted.text().replace('.', "/"))
}

/// `from . import x` / `from ..pkg import y` -> a `./`/`../`-prefixed path
/// resolve.rs's relative-import matching already understands (the same
/// mechanism it uses for Rust's `super::`/`self::`). One leading dot is the
/// current package (`./`); each additional dot climbs one more directory
/// (`../`).
fn python_relative_import_path(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let dots = node
        .children()
        .find(|c| c.kind() == "import_prefix")
        .map(|c| c.text().chars().filter(|&ch| ch == '.').count())
        .unwrap_or(0);
    let module = node
        .children()
        .find(|c| c.kind() == "dotted_name")
        .map(|c| c.text().replace('.', "/"))
        .unwrap_or_default();
    if dots == 0 {
        return module;
    }
    let mut prefix = "./".to_string();
    prefix.push_str(&"../".repeat(dots - 1));
    prefix.push_str(&module);
    prefix
}

/// The decorator expression text of a `decorator` node: the callee name for
/// a call-style decorator (`@app.route(...)` -> "app.route"), otherwise the
/// bare expression text (`@staticmethod` -> "staticmethod").
fn python_decorator_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let inner = node.children().find(|c| c.kind() != "@")?;
    if inner.kind() == "call" {
        inner.field("function").map(|f| f.text().into_owned())
    } else {
        Some(inner.text().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::parse::parse_source;

    #[test]
    fn decorator_entity_carries_raw_text_and_owner() {
        let src = "@app.route(\"/x\")\ndef handler():\n    pass\n";
        let parsed = parse_source(&SupportLang::Python, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let decorators: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Decorator)
            .collect();
        assert_eq!(decorators.len(), 1, "entities: {entities:?}");
        assert_eq!(decorators[0].name, "app.route");
        assert_eq!(decorators[0].enclosing_function.as_deref(), Some("handler"));
    }

    #[test]
    fn method_in_class_records_owning_type_and_module_function_does_not() {
        let src = "class EventBus:\n    def on(self):\n        pass\n\ndef helper():\n    pass\n";
        let parsed = parse_source(&SupportLang::Python, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let on = entities
            .iter()
            .find(|e| e.kind == EntityKind::Function && e.name == "on")
            .expect("method `on` extracted");
        assert_eq!(
            on.owner_type.as_deref(),
            Some("EventBus"),
            "class method must record its owning type: {entities:?}"
        );

        let helper = entities
            .iter()
            .find(|e| e.kind == EntityKind::Function && e.name == "helper")
            .expect("module function `helper` extracted");
        assert_eq!(
            helper.owner_type, None,
            "module-level function has no owning type: {entities:?}"
        );
    }

    #[test]
    fn lambdas_are_callable_boundaries() {
        let parsed = parse_source(
            &SupportLang::Python,
            "def outer():\n    callback = lambda: remote()\n",
        );
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;
        assert_eq!(
            entities
                .iter()
                .filter(|e| e.kind == EntityKind::CallableBoundary)
                .count(),
            1,
            "entities: {entities:?}"
        );
    }
}
