//! Elixir entity extraction.
//!
//! GRAMMAR SHAPE (tree-sitter-elixir): almost everything is a `call` node.
//! `defmodule`, `def`, `defp`, `defmacro`, `defprotocol`, `defimpl`,
//! `defstruct`, `import`, `alias`, `require`, `use`, `raise`, `throw`, and the
//! control-flow macros (`if`/`case`/`cond`/`for`/`with`/`receive`/`try`) are
//! all parsed as `call` nodes whose callee lives in the `target` field (an
//! `identifier`, or a `dot` for remote calls like `Enum.map`). The macro's
//! operands live in an `arguments` child. So `visit` matches `kind == "call"`
//! and dispatches on the callee identifier, Ruby-style (early-return per
//! branch), before falling back to a plain Call.
//!
//! SCOPE-STACKING CARVE-OUT: the walker (`crate::extract::walk`) stacks the
//! enclosing-function / owner-type scopes by matching `node.kind()` against
//! `FUNCTION_SCOPES` / `TYPE_SCOPES` and naming the scope via
//! `field_name(node)`. In Elixir `def` and `defmodule` share the single kind
//! `call`, so they CANNOT be distinguished by kind, and a `call` node has no
//! `name` field. Therefore both scope lists are **empty** and the generic
//! stack does not light up for Elixir; `enclosing_function` stays `None`.
//! `owner_type`, however, IS populated for functions: `visit_call` resolves
//! the enclosing `defmodule` by walking ancestors (see `enclosing_module_name`)
//! and stamps it on each `def`/`defp`, so `Foo.Bar.bar` records its owning
//! module even though the kind-keyed stack can't. Every entity kind is still
//! extracted.
//!
//! MAPPING (verified against a grammar dump):
//! - `def` / `defp` / `defmacro` -> Function. The function head is the first
//!   argument: a nested `call` (`bar(x, y)`) whose target identifier is the
//!   name and whose arguments are the Parameters, or a bare `identifier`
//!   (`def f`, no arg list) which is the name with no params.
//! - `defmodule` -> Class. Modules are Elixir's primary structural unit; Class
//!   is the closest kind. Name is the `alias` argument text (`Foo.Bar`).
//! - `defprotocol` -> Interface (a named behavioural contract). `defimpl A,
//!   for: B` -> Implements referencing the protocol `A`.
//! - `defstruct` -> Class (a module-level record definition). Named after the
//!   struct's field list text so it is non-empty and distinct.
//! - module attribute `@x value` (`unary_operator` `@` wrapping a `call`) ->
//!   Variable named `@x`. Covers `@moduledoc`, `@doc`, `@spec`, and plain
//!   module attributes uniformly (no allow-list; a Decorator mapping would
//!   need one and Elixir attributes aren't decorators).
//! - `import` / `alias` / `require` / `use` -> Import; spec is the module-path
//!   argument text (`A.B`). Branch and return early (not also a Call).
//! - local call `bar(x)` -> Call (callee identifier). Remote call `A.b(...)` /
//!   `x.field` (target is a `dot`) -> MemberAccess for the accessed member
//!   (the `dot`'s right identifier) AND a Call named the same.
//! - `=` match (`binary_operator` operator `=`) -> Variable for each
//!   identifier bound on the left pattern (tuple/list/bare).
//! - Parameter: identifiers in a function head's argument list.
//! - Literal: `integer` / `float` / `string` / `atom` / `boolean` / `nil` /
//!   `char` / `charlist`.
//! - `raise` / `throw` calls -> Throw (named after the first argument).
//! - `rescue_block` / `catch_block` (children of a `try` call) -> Catch.
//! - `if`/`unless`/`case`/`cond`/`for`/`with`/`receive`/`try` calls ->
//!   ControlFlow (named after the macro). Detected by callee identifier since
//!   they are `call` nodes, not dedicated kinds.
//! - Route: narrow Phoenix router shape — an HTTP-verb macro call whose first
//!   argument is a string path (`get "/users", Ctrl, :index`) -> Route. Clean
//!   in the grammar (callee identifier + leading `string` arg).
//!
//! CARVE-OUTS:
//! - Export: Elixir has no export statement; visibility is `def` (public) vs
//!   `defp` (private), not a declaration. Carved out.
//! - Response: Phoenix has no single response-DSL macro comparable to
//!   Sinatra's `json`/`erb` helpers (responses are conn-pipeline calls); no
//!   clean narrow shape, so Response is carved out. Route alone is included.

use crate::extract::entity::ExtractCtx;
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Empty: `def`/`defmodule` are both `call` kind and indistinguishable by
/// kind, and a `call` has no `name` field for the walker to stack. See the
/// module-doc scope-stacking carve-out.
pub const FUNCTION_SCOPES: &[&str] = &[];

/// Empty for the same reason as [`FUNCTION_SCOPES`].
pub const TYPE_SCOPES: &[&str] = &[];

/// Entity kinds the fixtures must produce. Export and Response are carved out
/// (see module docs); Interface via `defprotocol`, Route via Phoenix verbs.
pub const REQUIRED_KINDS: [EntityKind; 12] = [
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
];

/// Macros that define a named function.
const FUNCTION_DEFS: &[&str] = &[
    "def",
    "defp",
    "defmacro",
    "defmacrop",
    "defguard",
    "defguardp",
];

/// Directive macros that name a module dependency instead of a normal call.
const IMPORT_DIRECTIVES: &[&str] = &["import", "alias", "require", "use"];

/// Control-flow macros (parsed as `call` nodes, dispatched by callee).
const CONTROL_FLOW: &[&str] = &[
    "if", "unless", "case", "cond", "for", "with", "receive", "try",
];

/// Phoenix router HTTP-verb macros: `get "/path", Ctrl, :action` -> Route.
const HTTP_VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "options", "head"];

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // Every macro/definition/call is a `call` node — dispatch on callee.
        "call" => visit_call(node, ctx),

        // `@x value` module attribute -> Variable named `@x`.
        "unary_operator" => {
            if let Some(name) = module_attribute_name(node) {
                ctx.push(EntityKind::Variable, name, node);
            }
        }

        // `a = 1`, `{b, c} = {2, 3}` -> Variable per bound identifier. Other
        // binary operators (`+`, `<-`, `>`, ...) are ignored here.
        "binary_operator" => {
            if is_match_operator(node)
                && let Some(left) = node.field("left")
            {
                for name in pattern_identifiers(&left) {
                    ctx.push(EntityKind::Variable, name, node);
                }
            }
        }

        // rescue/catch clauses of a `try`.
        "rescue_block" | "catch_block" | "after_block" => {
            ctx.push(EntityKind::Catch, node.kind().into_owned(), node);
        }

        // Literals.
        "integer" | "float" | "string" | "atom" | "boolean" | "nil" | "char" | "charlist" => {
            ctx.push(EntityKind::Literal, node.text().into_owned(), node);
        }

        _ => {}
    }
}

/// Handle a `call` node: definitions, imports, control-flow, raises, routes,
/// member access, and the plain call itself.
fn visit_call(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>, ctx: &mut ExtractCtx) {
    let target = match node.field("target") {
        Some(t) => t,
        None => return,
    };

    // Remote call / field access: `Enum.map(...)`, `x.field` — target is a
    // `dot`. Handle member access + Call, then done.
    if target.kind() == "dot" {
        if let Some(member) = dot_member(&target) {
            ctx.push(EntityKind::MemberAccess, member.clone(), node);
            ctx.push(EntityKind::Call, member, node);
        }
        return;
    }

    // Otherwise the callee is a plain identifier.
    let callee = target.text().into_owned();

    // `def bar(x, y)` / `defp helper(z)` / `defmacro mac(a)` -> Function.
    if FUNCTION_DEFS.contains(&callee.as_str()) {
        if let Some((name, head)) = function_head(node) {
            ctx.push(EntityKind::Function, name, node);
            // The enclosing `defmodule` is this function's owning type. Both
            // `def` and `defmodule` are `call` kind, so the generic type-scope
            // stack (keyed on node kind) can't distinguish them — resolve the
            // module by walking ancestors and stamp it on the just-pushed
            // Function entity, preserving the is_async/is_test/minhash that
            // `ctx.push` computed.
            if let Some(owner) = enclosing_module_name(node)
                && let Some(last) = ctx.out.last_mut()
            {
                last.owner_type = Some(owner);
            }
            // Parameters live in the function head's argument list.
            if let Some(head) = head {
                for pname in head_parameter_names(&head) {
                    ctx.push(EntityKind::Parameter, pname, node);
                }
            }
        }
        return;
    }

    // `defmodule Foo.Bar` -> Class.
    if callee == "defmodule" {
        ctx.push(
            EntityKind::Class,
            first_arg_text(node).unwrap_or_default(),
            node,
        );
        return;
    }

    // `defprotocol Size` -> Interface.
    if callee == "defprotocol" {
        ctx.push(
            EntityKind::Interface,
            first_arg_text(node).unwrap_or_default(),
            node,
        );
        return;
    }

    // `defimpl Size, for: BitString` -> Implements referencing the protocol.
    if callee == "defimpl" {
        push_owned(
            ctx,
            EntityKind::Implements,
            first_arg_text(node).unwrap_or_default(),
            node,
        );
        return;
    }

    // `defstruct name: nil, age: 0` -> Class (module-level record).
    if callee == "defstruct" {
        let name = first_arg_text(node).unwrap_or_else(|| "defstruct".to_string());
        ctx.push(EntityKind::Class, name, node);
        return;
    }

    // `import X` / `alias A.B` / `require Y` / `use Z` -> Import.
    if IMPORT_DIRECTIVES.contains(&callee.as_str()) {
        if let Some(spec) = first_arg_text(node) {
            ctx.push(EntityKind::Import, spec, node);
        }
        return;
    }

    // `raise ...` / `throw ...` -> Throw.
    if callee == "raise" || callee == "throw" {
        ctx.push(
            EntityKind::Throw,
            first_arg_text(node).unwrap_or_default(),
            node,
        );
        return;
    }

    // Phoenix route `get "/users", Ctrl, :index` -> Route.
    if HTTP_VERBS.contains(&callee.as_str())
        && let Some(path) = first_string_arg(node)
    {
        ctx.out.push(Entity {
            kind: EntityKind::Route,
            name: callee.clone(),
            file_id: ctx.file_id,
            span: crate::extract::span_of(node),
            enclosing_function: ctx.enclosing.map(|s| s.to_owned()),
            method: Some(callee.to_uppercase()),
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

    // Control-flow macros -> ControlFlow.
    if CONTROL_FLOW.contains(&callee.as_str()) {
        ctx.push(EntityKind::ControlFlow, callee, node);
        return;
    }

    // Plain local call.
    ctx.push(EntityKind::Call, callee, node);
}

/// Push a type-reference entity (`Implements`) — Elixir has no owner-type
/// scope (see the module-doc carve-out), so `enclosing_function` is left None.
fn push_owned(
    ctx: &mut ExtractCtx,
    kind: EntityKind,
    name: String,
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) {
    ctx.out.push(Entity {
        kind,
        name,
        file_id: ctx.file_id,
        span: crate::extract::span_of(node),
        enclosing_function: None,
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

/// Name of the nearest enclosing `defmodule` (`Foo.Bar`) for a `def`/`defp`
/// node, or None at file top level. Elixir nests function definitions inside a
/// module's `do` block, so the owning module is an ancestor `call` whose
/// callee (`target`) is the `defmodule` macro; its first argument is the
/// module alias.
fn enclosing_module_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.ancestors()
        .filter(|a| a.kind() == "call")
        .find(|a| {
            a.field("target")
                .map(|t| t.text() == "defmodule")
                .unwrap_or(false)
        })
        .and_then(|m| first_arg_text(&m))
}

/// The `arguments` child of a `call` (`(x, y)` or a comma list), if present.
fn arguments<'t>(
    node: &ast_grep_core::Node<'t, StrDoc<SupportLang>>,
) -> Option<ast_grep_core::Node<'t, StrDoc<SupportLang>>> {
    node.children().find(|c| c.kind() == "arguments")
}

/// First positional argument node of a call (skipping `(` `,` `)` tokens).
fn first_arg<'t>(
    node: &ast_grep_core::Node<'t, StrDoc<SupportLang>>,
) -> Option<ast_grep_core::Node<'t, StrDoc<SupportLang>>> {
    arguments(node)?
        .children()
        .find(|c| !matches!(c.kind().as_ref(), "(" | "," | ")"))
}

/// Text of the first positional argument (`import Enum` -> "Enum").
fn first_arg_text(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    first_arg(node).map(|a| a.text().into_owned())
}

/// Inner text of the first string-literal argument (`get "/x"` -> "/x"),
/// joining `quoted_content` runs. `None` when the first arg is not a string.
fn first_string_arg(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let arg = first_arg(node)?;
    if arg.kind() != "string" {
        return None;
    }
    let mut out = String::new();
    for child in arg.children() {
        if child.kind() == "quoted_content" {
            out.push_str(child.text().as_ref());
        }
    }
    Some(out)
}

/// The right-hand member of a `dot` node (`Enum.map` -> "map", `x.field` ->
/// "field"): the `dot`'s `right` field, or the trailing identifier child.
fn dot_member(dot: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    if let Some(right) = dot.field("right") {
        return Some(right.text().into_owned());
    }
    dot.children()
        .filter(|c| matches!(c.kind().as_ref(), "identifier" | "alias"))
        .last()
        .map(|c| c.text().into_owned())
}

/// Function name + head node from a `def`/`defp`/... call. The first argument
/// is either a nested `call` (`bar(x, y)`) whose target identifier is the name
/// and whose arguments hold the parameters, or a bare `identifier` (`def f`)
/// which is the name with no head. Returns `(name, Some(head_call))` or
/// `(name, None)`.
#[allow(clippy::type_complexity)]
fn function_head<'t>(
    node: &ast_grep_core::Node<'t, StrDoc<SupportLang>>,
) -> Option<(String, Option<ast_grep_core::Node<'t, StrDoc<SupportLang>>>)> {
    let head = first_arg(node)?;
    match head.kind().as_ref() {
        "call" => {
            // `bar(x, y)` — name is the head call's target identifier.
            let name = head.field("target")?.text().into_owned();
            Some((name, Some(head)))
        }
        // `def f` — bare identifier name, no parameters. A guard head
        // (`def f(x) when ...`) is a `binary_operator`; fall through to its
        // left side (the actual call) if present.
        "identifier" => Some((head.text().into_owned(), None)),
        "binary_operator" => {
            let left = head.field("left")?;
            if left.kind() == "call" {
                let name = left.field("target")?.text().into_owned();
                Some((name, Some(left)))
            } else if left.kind() == "identifier" {
                Some((left.text().into_owned(), None))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parameter identifier names inside a function head's `arguments` list.
/// Elixir params are patterns; collect the identifier leaves (skipping `_`).
fn head_parameter_names(head: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let Some(args) = arguments(head) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for child in args.children() {
        collect_pattern_idents(&child, &mut names);
    }
    names
}

/// Identifiers bound by a match's left pattern (`a`, or `b`/`c` in `{b, c}`).
fn pattern_identifiers(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let mut names = Vec::new();
    collect_pattern_idents(node, &mut names);
    names
}

/// Recursively collect identifier leaves of a pattern, skipping the wildcard
/// `_` and pinned/typed decorations that aren't bindings. Bounded to a small
/// pattern subtree.
fn collect_pattern_idents(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    out: &mut Vec<String>,
) {
    match node.kind().as_ref() {
        "identifier" => {
            let t = node.text().into_owned();
            if t != "_" && !t.starts_with('_') {
                out.push(t);
            }
        }
        // Delimiters / keyword syntax — recurse into structural children.
        "(" | ")" | "," | "{" | "}" | "[" | "]" | "keyword" => {}
        _ => {
            for child in node.children() {
                collect_pattern_idents(&child, out);
            }
        }
    }
}

/// Name of a module attribute `unary_operator` (`@x value` -> "@x", `@moduledoc`
/// -> "@moduledoc"). Returns `None` for non-`@` unary operators (`-x`, `&f`).
fn module_attribute_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let op = node.field("operator")?;
    if op.text() != "@" {
        return None;
    }
    // The operand is a `call` whose target identifier is the attribute name,
    // or a bare identifier (`@x` with no value).
    let operand = node
        .children()
        .find(|c| matches!(c.kind().as_ref(), "call" | "identifier"))?;
    let name = if operand.kind() == "call" {
        operand.field("target")?.text().into_owned()
    } else {
        operand.text().into_owned()
    };
    Some(format!("@{name}"))
}

/// True when a `binary_operator` is a match (`=`) binding.
fn is_match_operator(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    node.field("operator").is_some_and(|o| o.text() == "=")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::model::Entity;
    use crate::parse::parse_source;

    fn entities(src: &str) -> Vec<Entity> {
        let parsed = parse_source(&SupportLang::Elixir, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        extract::extract(&parsed, 0).entities
    }

    fn find<'a>(es: &'a [Entity], kind: EntityKind, name: &str) -> Option<&'a Entity> {
        es.iter().find(|e| e.kind == kind && e.name == name)
    }

    #[test]
    fn defmodule_is_class_and_def_is_function_with_params() {
        let es = entities("defmodule Foo.Bar do\n  def bar(x, y) do\n    x + y\n  end\nend\n");
        assert!(find(&es, EntityKind::Class, "Foo.Bar").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Function, "bar").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Parameter, "x").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Parameter, "y").is_some(), "{es:?}");
    }

    #[test]
    fn function_records_enclosing_module_as_owner_type() {
        // A `def`/`defp` inside `defmodule Foo.Bar` records that module as its
        // `owner_type`. Elixir's `def`/`defmodule` are both `call` kind, so the
        // generic type-scope stack can't supply this — it's resolved by walking
        // ancestors to the enclosing `defmodule`.
        let es =
            entities("defmodule Foo.Bar do\n  def bar(x), do: x\n  defp helper(z), do: z\nend\n");
        assert_eq!(
            find(&es, EntityKind::Function, "bar")
                .expect("bar present")
                .owner_type
                .as_deref(),
            Some("Foo.Bar"),
            "def must carry its enclosing module: {es:?}"
        );
        assert_eq!(
            find(&es, EntityKind::Function, "helper")
                .expect("helper present")
                .owner_type
                .as_deref(),
            Some("Foo.Bar"),
            "defp must carry its enclosing module: {es:?}"
        );
    }

    #[test]
    fn defp_and_no_arg_def_extract() {
        let es = entities("defmodule M do\n  defp helper(z), do: z\n  def f, do: 1\nend\n");
        assert!(
            find(&es, EntityKind::Function, "helper").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Parameter, "z").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Function, "f").is_some(), "{es:?}");
    }

    #[test]
    fn import_directives_become_imports_not_calls() {
        let es = entities(
            "defmodule M do\n  import Enum\n  alias A.B\n  require Logger\n  use GenServer\nend\n",
        );
        assert!(find(&es, EntityKind::Import, "Enum").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Import, "A.B").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Import, "Logger").is_some(), "{es:?}");
        assert!(
            find(&es, EntityKind::Import, "GenServer").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Call, "import").is_none(), "{es:?}");
    }

    #[test]
    fn remote_call_emits_member_access_and_call() {
        let es = entities("defmodule M do\n  def f(x) do\n    Enum.map(x, x)\n  end\nend\n");
        assert!(
            find(&es, EntityKind::MemberAccess, "map").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Call, "map").is_some(), "{es:?}");
    }

    #[test]
    fn match_binding_and_literals() {
        let es = entities(
            "defmodule M do\n  def f do\n    a = 1\n    {b, c} = {2, 3}\n    s = \"hi\"\n    at = :ok\n  end\nend\n",
        );
        assert!(find(&es, EntityKind::Variable, "a").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Variable, "b").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Variable, "c").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Literal, "1").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Literal, ":ok").is_some(), "{es:?}");
    }

    #[test]
    fn module_attribute_is_variable() {
        let es = entities("defmodule M do\n  @answer 42\n  @moduledoc \"docs\"\nend\n");
        assert!(
            find(&es, EntityKind::Variable, "@answer").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::Variable, "@moduledoc").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn control_flow_and_throw_and_catch() {
        let es = entities(
            "defmodule M do\n  def f(x) do\n    if x, do: 1\n    case x do\n      _ -> :ok\n    end\n    try do\n      raise \"boom\"\n    rescue\n      e -> e\n    end\n  end\nend\n",
        );
        assert!(find(&es, EntityKind::ControlFlow, "if").is_some(), "{es:?}");
        assert!(
            find(&es, EntityKind::ControlFlow, "case").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::ControlFlow, "try").is_some(),
            "{es:?}"
        );
        assert!(
            es.iter().any(|e| e.kind == EntityKind::Throw),
            "expected a Throw: {es:?}"
        );
        assert!(
            es.iter().any(|e| e.kind == EntityKind::Catch),
            "expected a Catch: {es:?}"
        );
    }

    #[test]
    fn protocol_impl_and_phoenix_route() {
        let es = entities(
            "defprotocol Size do\n  def size(data)\nend\ndefimpl Size, for: BitString do\n  def size(s), do: 1\nend\ndefmodule R do\n  get \"/users\", UserController, :index\nend\n",
        );
        assert!(find(&es, EntityKind::Interface, "Size").is_some(), "{es:?}");
        assert!(
            find(&es, EntityKind::Implements, "Size").is_some(),
            "{es:?}"
        );
        let route = es
            .iter()
            .find(|e| e.kind == EntityKind::Route)
            .expect("route entity");
        assert_eq!(route.method.as_deref(), Some("GET"));
        assert_eq!(route.path.as_deref(), Some("/users"));
    }
}
