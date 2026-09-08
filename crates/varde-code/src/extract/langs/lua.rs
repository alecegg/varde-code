//! Lua entity extraction.
//!
//! Lua is procedural/dynamic with no class syntax, so the mapping is modeled on
//! Go (procedural) and Ruby (dynamic, call-based imports). Fixture-driven and
//! superset-safe: every kind in [`REQUIRED_KINDS`] is produced by the fixtures.
//!
//! Node-kind -> EntityKind mapping (verified against the tree-sitter-lua dump):
//! - Function: `function_declaration` (covers both `function f()` and
//!   `local function g()`) and `function_definition` (anonymous `function()`).
//!   The declared name is the `name` field, which is an `identifier`
//!   (`function f`), a `dot_index_expression` (`function M.method` -> name
//!   `M.method`), or a `method_index_expression` (`function M:m` -> name
//!   `M:m`). Anonymous definitions get an empty name.
//! - Variable: each `identifier` target in an `assignment_statement`'s
//!   `variable_list` — covers `local x = …`, global `x = …`, and each target
//!   of a multiple assignment (`local ok, err = …`).
//! - Parameter: each `identifier` inside a `parameters` node.
//! - Call: `function_call`; callee is the `name` field (`identifier`,
//!   `dot_index_expression`, or `method_index_expression`).
//! - Import: a `require` call — spec taken from the string-literal argument.
//!   Both `require("mod")` and the bare `require "mod"` parse to a
//!   `function_call` whose `arguments` node carries the string, so one path
//!   covers both (require tails resolve against sibling files by stem via
//!   resolve.rs's shared matching). Branches early like Ruby's `require`.
//! - MemberAccess: `dot_index_expression` (`t.k` -> "k") and
//!   `method_index_expression` (`t:m` -> "m"). Emitted for the accessed member
//!   whether it appears as a call callee or a bare field access.
//! - Literal: `string` / `number` / `true` / `false` / `nil` (Lua has no type
//!   context, so no `in_type` exclusion is needed).
//! - Throw: `error(…)` / `assert(…)` calls — Lua's error-raising idiom (there
//!   is no `throw` keyword). Named after the first argument's text. Branches
//!   early like Ruby's `raise`/`fail`.
//! - Catch: `pcall(…)` / `xpcall(…)` calls — Lua's protected-call idiom, the
//!   closest structural analog to a `try`/`catch` boundary (there is no
//!   `catch` construct). Named after the protected callee's text.
//! - ControlFlow: `if_statement`, `elseif_statement`, `else_statement`,
//!   `for_statement` (numeric + generic), `while_statement`,
//!   `repeat_statement`, `do_statement`, `return_statement`,
//!   `break_statement`, `goto_statement`, `label_statement`.
//!
//! Carve-outs (documented, not silently dropped):
//! - Class / Interface: Lua has no class or interface syntax. Its OOP is
//!   convention-based (tables + metatables, `setmetatable`), which is not
//!   expressible structurally, so both are carved out. Extends/Implements are
//!   consequently unreachable and also carved out (they are not in the 14-kind
//!   checklist regardless).
//! - Export: Lua has no export keyword. The module pattern is a bare
//!   `return M` at end of file — a `return_statement` (already ControlFlow),
//!   not a declaration — so Export is carved out.
//! - Route / Response: Lua has no single idiomatic web DSL (OpenResty, Lapis,
//!   and Kong all differ), so both are carved out rather than encode one
//!   framework's shape.

use crate::extract::entity::ExtractCtx;
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope (for `enclosing_function`
/// linkage on control-flow/error entities). Anonymous `function_definition`s
/// participate too so a nested error/return links to its closest function.
pub const FUNCTION_SCOPES: &[&str] = &["function_declaration", "function_definition"];

/// Lua has no class/interface syntax, so there are no type scopes (see module
/// docs). Kept for symmetry with other language modules; the default
/// `type_scopes` arm (`_ => &[]`) covers the empty case.
pub const TYPE_SCOPES: &[&str] = &[];

/// Entity kinds the fixtures must produce. Class/Interface/Export/Route/
/// Response are carved out (see module docs). Import and Throw/Catch are
/// emitted from the call arm.
pub const REQUIRED_KINDS: [EntityKind; 9] = [
    EntityKind::Function,
    EntityKind::Variable,
    EntityKind::Parameter,
    EntityKind::Call,
    EntityKind::Literal,
    EntityKind::MemberAccess,
    EntityKind::Catch,
    EntityKind::Throw,
    EntityKind::ControlFlow,
];

/// Calls that name a dependency instead of a normal call.
const REQUIRE_FUNCTIONS: &[&str] = &["require"];

/// Calls that raise an error (Lua's `throw` analog).
const THROW_FUNCTIONS: &[&str] = &["error", "assert"];

/// Protected-call functions (Lua's `try`/`catch` analog).
const CATCH_FUNCTIONS: &[&str] = &["pcall", "xpcall"];

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- structural ----
        // `function f()`, `local function g()`, `function M.m()`, `function M:m()`.
        "function_declaration" => {
            ctx.push(EntityKind::Function, function_name(node), node);
        }
        // `function() … end` as a value. When it is assigned to a table field
        // (`{ foo = function() end }`) or an assignment target
        // (`status.bar = function() end`), name it after that field/target so
        // metatable-/table-based OOP methods are queryable instead of anonymous
        // (audit S3: table-literal method values were all emitted blank and
        // dropped). A truly anonymous closure keeps a blank name and is filtered.
        "function_definition" => {
            let name = function_definition_name(node);
            if name.is_empty() {
                ctx.push_callable_boundary(node);
            } else {
                ctx.push(EntityKind::Function, name, node);
            }
        }

        // ---- variables ----
        // `local x = …`, global `x = …`, and each target of a multiple
        // assignment. The declared targets live in the `variable_list` child.
        "assignment_statement" => {
            for name in assignment_target_names(node) {
                ctx.push(EntityKind::Variable, name, node);
            }
        }

        // ---- parameters ----
        "parameters" => {
            for name in parameter_names(node) {
                ctx.push(EntityKind::Parameter, name, node);
            }
        }

        // ---- calls (imports / throws / catches / member access) ----
        "function_call" => visit_call(node, ctx),

        // ---- member access outside a call (`local k = t.k`) ----
        "dot_index_expression" | "method_index_expression" => {
            // A member access that is the callee of a `function_call` is
            // handled by `visit_call` (so the callee-name isn't double-counted
            // when it also drives the Call/MemberAccess pair). Skip it here.
            if node.parent().is_some_and(|p| p.kind() == "function_call") {
                return;
            }
            if let Some(name) = member_name(node) {
                ctx.push(EntityKind::MemberAccess, name, node);
            }
        }

        // ---- literals ----
        "string" | "number" | "true" | "false" | "nil" => {
            ctx.push(EntityKind::Literal, node.text().into_owned(), node);
        }

        // ---- control-flow ----
        "if_statement" | "elseif_statement" | "else_statement" | "for_statement"
        | "while_statement" | "repeat_statement" | "do_statement" | "return_statement"
        | "break_statement" | "goto_statement" | "label_statement" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// Handle a `function_call`: imports (`require`), throws (`error`/`assert`),
/// catches (`pcall`/`xpcall`), member access, and the plain call itself.
fn visit_call(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>, ctx: &mut ExtractCtx) {
    let callee = call_name(node);

    // `require "mod"` / `require("mod")` -> Import (not a Call). Lua module
    // paths use `.` as the package separator (`require "a.b.c"` -> `a/b/c`);
    // normalize to slash-separated form so resolve.rs's shared `/`-segment
    // matching applies unchanged (and its extension-stripping does not mistake
    // the trailing `.c` for a file extension).
    if REQUIRE_FUNCTIONS.contains(&callee.as_str()) {
        if let Some(spec) = first_string_arg(node) {
            ctx.push(EntityKind::Import, spec.replace('.', "/"), node);
        }
        return;
    }

    // `error("boom")` / `assert(x, "msg")` -> Throw (not a Call).
    if THROW_FUNCTIONS.contains(&callee.as_str()) {
        let name = first_arg(node)
            .map(|a| a.text().into_owned())
            .unwrap_or_default();
        ctx.push(EntityKind::Throw, name, node);
        return;
    }

    // `pcall(fn, …)` / `xpcall(fn, handler)` -> Catch (not a plain Call).
    if CATCH_FUNCTIONS.contains(&callee.as_str()) {
        let name = first_arg(node)
            .map(|a| a.text().into_owned())
            .unwrap_or_else(|| callee.clone());
        ctx.push(EntityKind::Catch, name, node);
        return;
    }

    // `t.k(…)` / `t:m(…)` -> MemberAccess for the accessed member.
    if let Some(name_node) = node.field("name")
        && matches!(
            name_node.kind().as_ref(),
            "dot_index_expression" | "method_index_expression"
        )
        && let Some(member) = member_name(&name_node)
    {
        ctx.push(EntityKind::MemberAccess, member, node);
    }

    ctx.push(EntityKind::Call, callee, node);
}

/// Name of a `function_declaration`: an `identifier` (`function f`), a
/// `dot_index_expression` (`function M.method` -> "M.method"), or a
/// `method_index_expression` (`function M:m` -> "M:m"). Falls back to "".
fn function_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.field("name")
        .map(|n| n.text().into_owned())
        .unwrap_or_default()
}

/// Name for a `function_definition` used as a value, derived from context: the
/// table-field key it is bound to (`{ foo = function() end }` -> "foo"), or the
/// matching-position target of the enclosing assignment (`a.b = function() end`
/// -> "a.b"). Empty for a truly anonymous closure (callback argument, `return
/// function() end`), which the blank-name filter then drops.
fn function_definition_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let Some(parent) = node.parent() else {
        return String::new();
    };
    match parent.kind().as_ref() {
        "field" => parent
            .field("name")
            .map(|n| n.text().into_owned())
            .unwrap_or_default(),
        "expression_list" => {
            let Some(assign) = parent
                .parent()
                .filter(|p| p.kind() == "assignment_statement")
            else {
                return String::new();
            };
            let Some(idx) = parent
                .children()
                .filter(|c| c.is_named())
                .position(|c| c.node_id() == node.node_id())
            else {
                return String::new();
            };
            assign
                .children()
                .find(|c| c.kind() == "variable_list")
                .and_then(|vlist| vlist.children().filter(|c| c.is_named()).nth(idx))
                .map(|c| c.text().into_owned())
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Callee name of a `function_call`: the `name` field's text (covers a plain
/// `identifier`, a `dot_index_expression`, and a `method_index_expression`).
fn call_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.field("name")
        .map(|n| n.text().into_owned())
        .unwrap_or_default()
}

/// The accessed member of a `dot_index_expression` (`t.k` -> "k", `field`
/// field) or `method_index_expression` (`t:m` -> "m", `method` field).
fn member_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.field("field")
        .or_else(|| node.field("method"))
        .map(|n| n.text().into_owned())
}

/// Names bound by an `assignment_statement`: the `identifier` targets in its
/// `variable_list` child (each element of a single or multiple assignment).
/// Underscore is skipped.
fn assignment_target_names(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let Some(list) = node.children().find(|c| c.kind() == "variable_list") else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == "identifier")
        .map(|c| c.text().into_owned())
        .filter(|n| n != "_")
        .collect()
}

/// Parameter names inside a `parameters` node (`identifier` children).
fn parameter_names(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    node.children()
        .filter(|c| c.kind() == "identifier")
        .map(|c| c.text().into_owned())
        .filter(|n| n != "_")
        .collect()
}

/// First positional argument node of a call (skipping `(` `,` `)` tokens). The
/// `arguments` node holds the args for both `f(x)` and the bare `f "x"` form.
fn first_arg<'t>(
    node: &ast_grep_core::Node<'t, StrDoc<SupportLang>>,
) -> Option<ast_grep_core::Node<'t, StrDoc<SupportLang>>> {
    let args = node.field("arguments")?;
    args.children()
        .find(|c| !matches!(c.kind().as_ref(), "(" | "," | ")"))
}

/// Content of the first string-literal argument of a call (`require "mod"`
/// -> "mod"). `None` when the first argument is not a string.
fn first_string_arg(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    string_content(&first_arg(node)?)
}

/// Inner text of a `string` node (`"mod"` -> "mod"), joining the literal
/// `string_content` runs.
fn string_content(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    if node.kind() != "string" {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::model::Entity;
    use crate::parse::parse_source;

    fn entities(src: &str) -> Vec<Entity> {
        let parsed = parse_source(&SupportLang::Lua, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        extract::extract(&parsed, 0).entities
    }

    fn find<'a>(es: &'a [Entity], kind: EntityKind, name: &str) -> Option<&'a Entity> {
        es.iter().find(|e| e.kind == kind && e.name == name)
    }

    #[test]
    fn table_field_function_values_are_named_from_their_key_or_target() {
        // Audit S3: table-based OOP methods assigned as function values were
        // emitted anonymously (blank) and dropped. They are now named from the
        // assignment target or table-literal field key.
        let es = entities(
            "local status = {}\nstatus.foo = function() return 1 end\nlocal M = { baz = function() return 2 end }\n",
        );
        assert!(
            find(&es, EntityKind::Function, "status.foo").is_some(),
            "assignment: {es:?}"
        );
        assert!(
            find(&es, EntityKind::Function, "baz").is_some(),
            "table field: {es:?}"
        );
    }

    #[test]
    fn named_and_local_and_table_functions() {
        let es = entities(
            "function f() end\nlocal function g() end\nlocal M = {}\nfunction M.m() end\nfunction M:c() end\n",
        );
        assert!(find(&es, EntityKind::Function, "f").is_some());
        assert!(find(&es, EntityKind::Function, "g").is_some());
        assert!(find(&es, EntityKind::Function, "M.m").is_some());
        assert!(find(&es, EntityKind::Function, "M:c").is_some());
    }

    #[test]
    fn anonymous_function_is_not_emitted_as_a_blank_name_entity() {
        // Audit S8: a blank-name Function is pure noise for name-keyed queries
        // and floods `symbols_in_file`. The `extract` post-filter drops it (the
        // `z` parameter and `z` reference still carry the real signal).
        let es = entities("local h = function(z) return z end\n");
        assert!(find(&es, EntityKind::Function, "").is_none());
        assert!(find(&es, EntityKind::Parameter, "z").is_some());
    }

    #[test]
    fn unnamed_function_values_are_callable_boundaries() {
        let es = entities("pcall(function() remote() end)\n");
        assert!(
            es.iter()
                .any(|e| e.kind == EntityKind::CallableBoundary && e.name.is_empty()),
            "{es:?}"
        );
    }

    #[test]
    fn variables_local_global_and_multiple() {
        let es = entities("local x = 1\ny = 2\nlocal a, b = 3, 4\n");
        assert!(find(&es, EntityKind::Variable, "x").is_some());
        assert!(find(&es, EntityKind::Variable, "y").is_some());
        assert!(find(&es, EntityKind::Variable, "a").is_some());
        assert!(find(&es, EntityKind::Variable, "b").is_some());
    }

    #[test]
    fn parameters() {
        let es = entities("function f(a, b) return a + b end\n");
        assert!(find(&es, EntityKind::Parameter, "a").is_some());
        assert!(find(&es, EntityKind::Parameter, "b").is_some());
    }

    #[test]
    fn require_becomes_import_both_forms() {
        let es = entities("local m = require(\"foo\")\nlocal n = require \"bar\"\n");
        assert!(find(&es, EntityKind::Import, "foo").is_some());
        assert!(find(&es, EntityKind::Import, "bar").is_some());
        // A require is an Import, never a plain Call.
        assert!(find(&es, EntityKind::Call, "require").is_none());
    }

    #[test]
    fn error_and_assert_become_throw() {
        let es = entities("error(\"boom\")\nassert(x, \"must\")\n");
        assert!(es.iter().any(|e| e.kind == EntityKind::Throw));
        assert!(find(&es, EntityKind::Call, "error").is_none());
        assert!(find(&es, EntityKind::Call, "assert").is_none());
    }

    #[test]
    fn pcall_becomes_catch() {
        let es = entities("local ok = pcall(function() error(\"x\") end)\n");
        assert!(es.iter().any(|e| e.kind == EntityKind::Catch));
        assert!(find(&es, EntityKind::Call, "pcall").is_none());
    }

    #[test]
    fn calls_and_member_access() {
        let es = entities("print(1)\nlocal k = t.key\nt:m()\nM.m(1)\n");
        assert!(find(&es, EntityKind::Call, "print").is_some());
        assert!(find(&es, EntityKind::MemberAccess, "key").is_some());
        assert!(find(&es, EntityKind::MemberAccess, "m").is_some());
    }

    #[test]
    fn literals() {
        let es = entities("local s = \"hi\"\nlocal n = 42\nlocal t = true\nlocal z = nil\n");
        assert!(find(&es, EntityKind::Literal, "\"hi\"").is_some());
        assert!(find(&es, EntityKind::Literal, "42").is_some());
        assert!(find(&es, EntityKind::Literal, "true").is_some());
        assert!(find(&es, EntityKind::Literal, "nil").is_some());
    }

    #[test]
    fn control_flow() {
        let es = entities(
            "if x then return 1 elseif y then return 2 else return 3 end\nfor i = 1, 3 do break end\nwhile x do x = x - 1 end\nrepeat x = x + 1 until x > 3\n",
        );
        assert!(find(&es, EntityKind::ControlFlow, "if_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "for_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "while_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "repeat_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "return_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "break_statement").is_some());
    }
}
