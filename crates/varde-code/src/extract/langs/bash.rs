//! Bash (shell) entity extraction.
//!
//! Bash is a procedural/dynamic shell language with a deliberately smaller
//! entity surface than the general-purpose languages (roadmap Tier B, "medium
//! value"). The mapping is modeled on Lua (procedural/dynamic, call-based
//! imports) but carves out even more, because Bash has no types, no named
//! parameters, and no exceptions. Fixture-driven and superset-safe: every kind
//! in [`REQUIRED_KINDS`] is produced by the fixtures.
//!
//! Node-kind -> EntityKind mapping (verified against the tree-sitter-bash dump):
//! - Function: `function_definition` — covers both the POSIX `foo() { … }` form
//!   and the `function bar { … }` form. The declared name is the `name` field,
//!   a `word` node in both forms.
//! - Variable: the assigned name of a `variable_assignment` (`x=1`, `s="hi"`)
//!   taken from its `name` field (a `variable_name`). A `declaration_command`
//!   (`local`/`declare`/`readonly`/`typeset` + an assignment) is handled by
//!   recursing onto the inner `variable_assignment` — the walk visits it as a
//!   child — so `local y=2` emits the Variable `y` without a dedicated arm.
//! - Export: a `declaration_command` whose leading keyword is `export`. Bash has
//!   a real `export` builtin, so `export FOO=…` / `export FOO` is a genuine
//!   export construct and maps to Export (named after the exported variable).
//!   We deliberately do NOT also emit a Variable for the exported name: the
//!   export *is* the declaration, and emitting both would double-count one name
//!   at one span. (A plain `x=1` with no `export` stays a Variable via the
//!   `variable_assignment` arm.)
//! - Import: a `source X` or `. X` command — spec is the file argument,
//!   unquoted. The `./b.sh` form stem-matches sibling files via resolve.rs's
//!   shared matching. Branches early so it is not also emitted as a Call.
//! - Call: a `command` — callee is the `command_name` (first word). Control-flow
//!   builtins (`return`/`break`/`continue`) and `source`/`.`/`trap` branch off
//!   first (see below) so they are not double-counted as plain calls.
//! - Literal: `string` (double-quoted), `raw_string` (single-quoted), and
//!   `number`. Bash has no type context, so no `in_type` exclusion is needed.
//! - ControlFlow: `if_statement`, `elif_clause`, `else_clause`, `for_statement`,
//!   `while_statement` (the grammar reuses this node for `until` too), and
//!   `case_statement`/`case_item`. Bash's `return`/`break`/`continue` are not
//!   distinct grammar nodes — they parse as ordinary `command`s whose
//!   `command_name` is the keyword — so the `command` arm maps those three
//!   command names to ControlFlow instead of Call.
//!
//! Carve-outs (documented, not silently dropped):
//! - Class / Interface / Extends / Implements: Bash has no types, classes, or
//!   interfaces of any kind, so all four are carved out (and there are no type
//!   scopes — see [`TYPE_SCOPES`]).
//! - Parameter: Bash functions have no named parameters. Arguments are
//!   positional (`$1`, `$2`, `$@`) and are references to implicit positional
//!   variables, not named declarations, so Parameter is carved out.
//! - MemberAccess: Bash has no object/field access. The closest shape is array
//!   subscripting (`${arr[0]}`), but that is an index expression, not a named
//!   member, so MemberAccess is carved out rather than forced.
//! - Throw: Bash has no exceptions and no `throw`/`raise` keyword — error
//!   handling is exit codes and `set -e`. Carved out.
//! - Catch: Bash has no `try`/`catch`. The `trap … ERR` builtin is the closest
//!   handler idiom, but it registers a signal/error handler by string rather
//!   than delimiting a lexical protected region, so it is not a clean Catch
//!   analog; `trap` is left as an ordinary Call and Catch is carved out.
//! - Export note: Export is INCLUDED (Bash has a real `export`), unlike Lua
//!   which carves it out.
//! - Route / Response: Bash has no web DSL, so both are carved out.

use crate::extract::entity::ExtractCtx;
use crate::extract::langs::unquote;
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope (for `enclosing_function`
/// linkage on control-flow entities). Both `foo()` and `function foo` forms
/// parse to `function_definition`.
pub const FUNCTION_SCOPES: &[&str] = &["function_definition"];

/// Bash has no class/interface/type syntax, so there are no type scopes (see
/// module docs). The default `type_scopes` arm (`_ => &[]`) covers the empty
/// case; kept for symmetry with other language modules.
pub const TYPE_SCOPES: &[&str] = &[];

/// Entity kinds the fixtures must produce. Class/Interface/Parameter/
/// MemberAccess/Catch/Throw/Route/Response are carved out (see module docs).
/// Import is emitted from the command arm (`source`/`.`).
pub const REQUIRED_KINDS: [EntityKind; 6] = [
    EntityKind::Function,
    EntityKind::Variable,
    EntityKind::Export,
    EntityKind::Call,
    EntityKind::Literal,
    EntityKind::ControlFlow,
];

/// Command names that source another file (Bash's import idiom).
const SOURCE_COMMANDS: &[&str] = &["source", "."];

/// Command names that are control-flow keywords rather than external calls.
/// (`return`/`break`/`continue` parse as ordinary commands in tree-sitter-bash.)
const CONTROL_FLOW_COMMANDS: &[&str] = &["return", "break", "continue"];

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- functions ----
        // `foo() { … }` and `function bar { … }` both -> `function_definition`.
        "function_definition" => {
            ctx.push(EntityKind::Function, function_name(node), node);
        }

        // ---- variables / exports ----
        // `export FOO=…` / `export FOO` -> Export; `local`/`declare`/`readonly`
        // just wrap an inner `variable_assignment` the walk visits as a child.
        "declaration_command" => {
            if is_export(node)
                && let Some(name) = declaration_name(node)
            {
                ctx.push(EntityKind::Export, name, node);
            }
        }
        // `x=1`, `s="hi"` -> Variable. An assignment nested inside an `export`
        // declaration is skipped here (the Export arm already named it), so the
        // exported name isn't double-counted.
        "variable_assignment" => {
            if node
                .parent()
                .is_some_and(|p| p.kind() == "declaration_command" && is_export(&p))
            {
                return;
            }
            ctx.push(EntityKind::Variable, assignment_name(node), node);
        }

        // ---- commands (imports / control-flow builtins / calls) ----
        "command" => visit_command(node, ctx),

        // ---- literals ----
        "string" | "raw_string" | "number" => {
            ctx.push(EntityKind::Literal, node.text().into_owned(), node);
        }

        // ---- control-flow ----
        // `until` reuses `while_statement`. `return`/`break`/`continue` are
        // commands (handled in `visit_command`), not distinct nodes here.
        "if_statement" | "elif_clause" | "else_clause" | "for_statement" | "while_statement"
        | "case_statement" | "case_item" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// Handle a `command`: imports (`source`/`.`), control-flow builtins
/// (`return`/`break`/`continue`), and the plain call itself.
fn visit_command(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>, ctx: &mut ExtractCtx) {
    let callee = command_name(node);

    // `source ./lib.sh` / `. ./lib.sh` -> Import (not a Call).
    if SOURCE_COMMANDS.contains(&callee.as_str()) {
        if let Some(spec) = first_arg_text(node) {
            ctx.push(EntityKind::Import, unquote(&spec, false), node);
        }
        return;
    }

    // `return` / `break` / `continue` -> ControlFlow (not a Call).
    if CONTROL_FLOW_COMMANDS.contains(&callee.as_str()) {
        ctx.push(EntityKind::ControlFlow, callee, node);
        return;
    }

    ctx.push(EntityKind::Call, callee, node);
}

/// Name of a `function_definition`: the `name` field (a `word` node), covering
/// both `foo()` and `function foo`. Falls back to "".
fn function_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.field("name")
        .map(|n| n.text().into_owned())
        .unwrap_or_default()
}

/// Assigned name of a `variable_assignment`: the `name` field (a
/// `variable_name`). Falls back to "".
fn assignment_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.field("name")
        .map(|n| n.text().into_owned())
        .unwrap_or_default()
}

/// Callee name of a `command`: the `command_name` field's text (`grep`, `echo`,
/// `source`, `myfunc`). Falls back to "".
fn command_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.field("name")
        .map(|n| n.text().into_owned())
        .unwrap_or_default()
}

/// True when a `declaration_command`'s leading keyword is `export`.
fn is_export(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    node.children().next().is_some_and(|c| c.kind() == "export")
}

/// Exported variable name in a `declaration_command`: either the `name` field
/// of a nested `variable_assignment` (`export FOO=1`) or a bare `variable_name`
/// child (`export FOO`). Falls back to `None`.
fn declaration_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    for child in node.children() {
        match child.kind().as_ref() {
            "variable_assignment" => {
                return child.field("name").map(|n| n.text().into_owned());
            }
            "variable_name" => return Some(child.text().into_owned()),
            _ => {}
        }
    }
    None
}

/// Text of the first argument of a `command` (the first child after the
/// `command_name` that is not a redirect/operator token). Used for the
/// `source X` file spec.
fn first_arg_text(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.children()
        .find(|c| {
            matches!(
                c.kind().as_ref(),
                "word" | "string" | "raw_string" | "concatenation"
            )
        })
        .map(|c| c.text().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::model::Entity;
    use crate::parse::parse_source;

    fn entities(src: &str) -> Vec<Entity> {
        let parsed = parse_source(&SupportLang::Bash, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        extract::extract(&parsed, 0).entities
    }

    fn find<'a>(es: &'a [Entity], kind: EntityKind, name: &str) -> Option<&'a Entity> {
        es.iter().find(|e| e.kind == kind && e.name == name)
    }

    #[test]
    fn posix_and_keyword_functions() {
        let es = entities("#!/usr/bin/env bash\nfoo() { echo hi; }\nfunction bar { echo yo; }\n");
        assert!(find(&es, EntityKind::Function, "foo").is_some());
        assert!(find(&es, EntityKind::Function, "bar").is_some());
    }

    #[test]
    fn variables_plain_and_local() {
        let es = entities("#!/usr/bin/env bash\nx=1\nlocal y=2\ns=\"hi\"\n");
        assert!(find(&es, EntityKind::Variable, "x").is_some());
        assert!(find(&es, EntityKind::Variable, "y").is_some());
        assert!(find(&es, EntityKind::Variable, "s").is_some());
    }

    #[test]
    fn export_becomes_export_not_variable() {
        let es = entities("#!/usr/bin/env bash\nexport Z=3\nexport W\n");
        assert!(find(&es, EntityKind::Export, "Z").is_some());
        assert!(find(&es, EntityKind::Export, "W").is_some());
        // The exported name is not also double-counted as a Variable.
        assert!(find(&es, EntityKind::Variable, "Z").is_none());
    }

    #[test]
    fn source_becomes_import_both_forms() {
        let es = entities("#!/usr/bin/env bash\nsource ./lib.sh\n. ./other.sh\n");
        assert!(find(&es, EntityKind::Import, "./lib.sh").is_some());
        assert!(find(&es, EntityKind::Import, "./other.sh").is_some());
        // A source is an Import, never a plain Call.
        assert!(find(&es, EntityKind::Call, "source").is_none());
        assert!(find(&es, EntityKind::Call, ".").is_none());
    }

    #[test]
    fn commands_become_calls() {
        let es = entities("#!/usr/bin/env bash\ngrep -r foo .\nmyfunc arg1 arg2\n");
        assert!(find(&es, EntityKind::Call, "grep").is_some());
        assert!(find(&es, EntityKind::Call, "myfunc").is_some());
    }

    #[test]
    fn literals() {
        let es = entities("#!/usr/bin/env bash\ns=\"hello\"\nr='raw'\nn=42\n");
        assert!(find(&es, EntityKind::Literal, "\"hello\"").is_some());
        assert!(find(&es, EntityKind::Literal, "'raw'").is_some());
        assert!(find(&es, EntityKind::Literal, "42").is_some());
    }

    #[test]
    fn control_flow() {
        let es = entities(
            "#!/usr/bin/env bash\nif [ \"$x\" = \"1\" ]; then echo a; elif [ \"$x\" = \"2\" ]; then echo b; else echo c; fi\nfor i in 1 2 3; do echo $i; done\nwhile true; do break; done\nuntil false; do continue; done\ncase $x in 1) echo one;; *) echo other;; esac\nf() { return 0; }\n",
        );
        assert!(find(&es, EntityKind::ControlFlow, "if_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "elif_clause").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "else_clause").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "for_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "while_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "case_statement").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "case_item").is_some());
        // `return`/`break`/`continue` commands map to ControlFlow, not Call.
        assert!(find(&es, EntityKind::ControlFlow, "return").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "break").is_some());
        assert!(find(&es, EntityKind::ControlFlow, "continue").is_some());
        assert!(find(&es, EntityKind::Call, "return").is_none());
    }
}
