//! C entity extraction.
//!
//! C-specific mapping notes (fixture-driven, superset-safe), verified against
//! the tree-sitter-c grammar with `examples/dump_c.rs`:
//! - `function_definition` -> Function. The name is nested: the `declarator`
//!   field is a `function_declarator` whose own `declarator` field bottoms out
//!   at an `identifier`, optionally wrapped in `pointer_declarator` /
//!   `reference_declarator` layers for pointer-returning functions. See
//!   [`declarator_identifier`], which peels those wrappers.
//! - `struct_specifier` / `union_specifier` / `enum_specifier` with a `name`
//!   (a `type_identifier`) -> Class. A named record/enum is the closest kind to
//!   a "type with members".
//! - `type_definition` (`typedef ...`) -> Class, named after the introduced
//!   type alias (the trailing `type_identifier` in the `declarator` field).
//!   This is the closest kind — a typedef names a type — even though the alias
//!   carries no members of its own.
//! - `declaration` -> Variable (one per declared name). The `declarator` field
//!   is either a bare `identifier`, an `init_declarator` (`x = 1`), or a
//!   pointer/array wrapper; [`declarator_identifier`] unwraps each to its name.
//!   Function *prototypes* (declarator is a `function_declarator`) are skipped —
//!   they declare no storage.
//! - `parameter_declaration` -> Parameter (name via the unwrapped declarator).
//! - `preproc_include` -> Import; the `path` child is a `string_literal`
//!   (`"helper.h"`) or `system_lib_string` (`<stdio.h>`). Quotes/angle brackets
//!   are stripped. System includes still emit an Import — they simply won't
//!   resolve to a repo file, which resolve.rs handles as an unresolved edge.
//! - `call_expression` (callee = `function` field) -> Call.
//! - `field_expression` (`a.b` / `a->b`) -> MemberAccess, named after the
//!   `field` member.
//! - `number_literal` / `string_literal` / `char_literal` / `true` / `false`
//!   -> Literal. C has no type-annotation context to exclude, so all literals
//!   are emitted.
//! - ControlFlow: if/for/while/do/switch/case/return/break/continue/goto plus
//!   `conditional_expression` (the `?:` ternary).
//! - Carve-outs (see [`REQUIRED_KINDS`]): Interface (C has no interface
//!   construct), Export (no export statement — visibility is `static` linkage,
//!   not a declaration kind we model), Catch/Throw (C has no exceptions),
//!   Route/Response (no dominant in-grammar web framework). Import is exercised
//!   by fixtures + the resolve check but is a bonus kind, not on the required
//!   checklist.

use crate::extract::entity::ExtractCtx;
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

type Node<'a> = ast_grep_core::Node<'a, StrDoc<SupportLang>>;

/// Node kinds that introduce a named function scope.
pub const FUNCTION_SCOPES: &[&str] = &["function_definition"];

/// Node kinds that introduce a named record/enum type scope (for
/// `Entity::owner_type` linkage). C has no methods, but a struct's fields are
/// still owned by the record.
pub const TYPE_SCOPES: &[&str] = &["struct_specifier", "union_specifier", "enum_specifier"];

/// Entity kinds the fixtures must produce. Interface/Export/Catch/Throw/
/// Route/Response are carved out (see module docs). Import is a bonus kind
/// (exercised by fixtures + the resolve check) but not on the required list.
pub const REQUIRED_KINDS: [EntityKind; 8] = [
    EntityKind::Function,
    EntityKind::Class,
    EntityKind::Variable,
    EntityKind::Parameter,
    EntityKind::Call,
    EntityKind::Literal,
    EntityKind::MemberAccess,
    EntityKind::ControlFlow,
];

pub fn visit(node: &Node<'_>, kind: &str, ctx: &mut ExtractCtx) {
    match kind {
        // ---- structural ----
        "function_definition" => {
            let name = node
                .field("declarator")
                .and_then(|d| declarator_identifier(&d))
                .unwrap_or_default();
            ctx.push(EntityKind::Function, name, node);
        }
        "struct_specifier" | "union_specifier" | "enum_specifier" => {
            // Only *named* records/enums are types; an anonymous record used
            // inline (`struct { int x; } v;`) has no `name` field and is
            // covered by the surrounding declaration's Variable.
            if let Some(name) = node.field("name") {
                ctx.push(EntityKind::Class, name.text().into_owned(), node);
            }
        }
        "type_definition" => {
            // `typedef <type> Alias;` — the introduced alias is the
            // `type_identifier` in the `declarator` field.
            if let Some(name) = node
                .field("declarator")
                .and_then(|d| declarator_identifier(&d))
            {
                ctx.push(EntityKind::Class, name, node);
            }
        }

        // ---- imports ----
        "preproc_include" => {
            if let Some(path) = node.field("path") {
                let spec = include_path(&path);
                if !spec.is_empty() {
                    ctx.push(EntityKind::Import, spec, node);
                }
            }
        }

        // ---- variables ----
        "declaration" => {
            // A `declaration` can declare several names (`int a, b;`). Each
            // top-level declarator child is one declared entity. Function
            // prototypes (declarator is a function_declarator) declare no
            // storage, so they are skipped.
            for decl in node.field_children("declarator") {
                if is_function_declarator(&decl) {
                    continue;
                }
                if let Some(name) = declarator_identifier(&decl) {
                    ctx.push(EntityKind::Variable, name, node);
                }
            }
        }

        // ---- parameters ----
        "parameter_declaration" => {
            if let Some(name) = node
                .field("declarator")
                .and_then(|d| declarator_identifier(&d))
            {
                ctx.push(EntityKind::Parameter, name, node);
            }
        }

        // ---- expression-level ----
        "call_expression" => {
            let name = node
                .field("function")
                .map(|n| n.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Call, name, node);
        }
        "field_expression" => {
            if let Some(field) = node.field("field") {
                ctx.push(EntityKind::MemberAccess, field.text().into_owned(), node);
            }
        }

        // ---- literals ----
        "number_literal" | "string_literal" | "char_literal" | "true" | "false" => {
            ctx.push(EntityKind::Literal, node.text().into_owned(), node);
        }

        // ---- control flow ----
        "if_statement"
        | "for_statement"
        | "while_statement"
        | "do_statement"
        | "switch_statement"
        | "case_statement"
        | "return_statement"
        | "break_statement"
        | "continue_statement"
        | "goto_statement"
        | "conditional_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// Peel a declarator down to its bottom `identifier` / `field_identifier`
/// name. C nests declarators: a `function_declarator` / `init_declarator` /
/// `pointer_declarator` / `reference_declarator` / `array_declarator` /
/// `parenthesized_declarator` each carry an inner `declarator` field (or, for
/// `type_definition`, a trailing `type_identifier`). Returns the innermost
/// name, or `None` for abstract declarators with no name.
pub(crate) fn declarator_identifier(node: &Node<'_>) -> Option<String> {
    match node.kind().as_ref() {
        "identifier" | "field_identifier" | "type_identifier" => Some(node.text().into_owned()),
        // Wrappers that carry the real declarator in their `declarator` field
        // (`pointer_declarator`/`function_declarator` in a `declaration`), or,
        // when that field is absent (e.g. a `reference_declarator` inside a
        // catch/param list), directly as an identifier child. Try the field
        // first, then fall through to the child scan below.
        "function_declarator"
        | "init_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator" => node
            .field("declarator")
            .and_then(|d| declarator_identifier(&d))
            .or_else(|| child_identifier(node)),
        // `type_definition`'s declarator field is a bare `type_identifier`
        // (handled above) but the trailing alias may also appear as a child
        // when wrapped; fall back to the first named identifier-ish child.
        _ => child_identifier(node),
    }
}

/// Scan a node's direct children for the first identifier-ish name, recursing
/// into further declarator wrappers. Used as the fallback when a wrapper has no
/// `declarator` field (its inner declarator is a plain child).
fn child_identifier(node: &Node<'_>) -> Option<String> {
    node.children().find_map(|c| match c.kind().as_ref() {
        "identifier" | "field_identifier" | "type_identifier" => Some(c.text().into_owned()),
        "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "function_declarator"
        | "init_declarator" => declarator_identifier(&c),
        _ => None,
    })
}

/// True when this declarator (after peeling pointer/array wrappers) is a
/// `function_declarator` — i.e. a function prototype, not a storage variable.
fn is_function_declarator(node: &Node<'_>) -> bool {
    match node.kind().as_ref() {
        "function_declarator" => true,
        "pointer_declarator" | "array_declarator" | "parenthesized_declarator" => node
            .field("declarator")
            .map(|d| is_function_declarator(&d))
            .unwrap_or(false),
        _ => false,
    }
}

/// Strip the surrounding quotes/angle brackets from an include `path` node:
/// `"helper.h"` -> `helper.h`, `<stdio.h>` -> `stdio.h`.
fn include_path(node: &Node<'_>) -> String {
    let raw = node.text();
    let t = raw.trim();
    let bytes = t.as_bytes();
    if t.len() >= 2 {
        let stripped = (bytes[0] == b'"' && bytes[t.len() - 1] == b'"')
            || (bytes[0] == b'<' && bytes[t.len() - 1] == b'>');
        if stripped {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Entity, EntityKind};
    use crate::parse::parse_source;

    fn extract(src: &str) -> Vec<Entity> {
        let parsed = parse_source(&SupportLang::C, src);
        crate::extract::extract(&parsed, 0).entities
    }

    fn names(entities: &[Entity], kind: EntityKind) -> Vec<String> {
        entities
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.name.clone())
            .collect()
    }

    #[test]
    fn function_name_unwraps_declarator_nesting() {
        let src = "int add(int a, int b) { return a + b; }\n\
                   char *dup(const char *s) { return 0; }\n";
        let e = extract(src);
        let fns = names(&e, EntityKind::Function);
        assert!(fns.contains(&"add".to_string()), "got {fns:?}");
        // Pointer-returning function: name is under a pointer_declarator layer.
        assert!(fns.contains(&"dup".to_string()), "got {fns:?}");
    }

    #[test]
    fn records_and_typedef_map_to_class() {
        let src = "struct Point { int x; int y; };\n\
                   union U { int i; float f; };\n\
                   enum Color { RED, GREEN };\n\
                   typedef int MyInt;\n";
        let e = extract(src);
        let classes = names(&e, EntityKind::Class);
        for want in ["Point", "U", "Color", "MyInt"] {
            assert!(
                classes.contains(&want.to_string()),
                "missing {want}: {classes:?}"
            );
        }
    }

    #[test]
    fn variables_params_and_calls() {
        let src = "int counter = 0;\n\
                   int f(int n) { int x = n; g(x); return x; }\n";
        let e = extract(src);
        assert!(names(&e, EntityKind::Variable).contains(&"counter".to_string()));
        assert!(names(&e, EntityKind::Variable).contains(&"x".to_string()));
        assert!(names(&e, EntityKind::Parameter).contains(&"n".to_string()));
        assert!(names(&e, EntityKind::Call).contains(&"g".to_string()));
    }

    #[test]
    fn member_access_from_dot_and_arrow() {
        let src = "int f(struct S *p, struct S q) { return p->a + q.b; }\n";
        let e = extract(src);
        let ma = names(&e, EntityKind::MemberAccess);
        assert!(ma.contains(&"a".to_string()), "got {ma:?}");
        assert!(ma.contains(&"b".to_string()), "got {ma:?}");
    }

    #[test]
    fn includes_emit_imports_stripped() {
        let src = "#include <stdio.h>\n#include \"helper.h\"\n";
        let e = extract(src);
        let imports = names(&e, EntityKind::Import);
        assert!(imports.contains(&"stdio.h".to_string()), "got {imports:?}");
        assert!(imports.contains(&"helper.h".to_string()), "got {imports:?}");
    }

    #[test]
    fn control_flow_and_literals() {
        let src = "int f(int a) {\n\
                     if (a) { return 1; }\n\
                     for (int i = 0; i < 3; i++) {}\n\
                     while (a) { break; }\n\
                     switch (a) { case 1: continue; }\n\
                     int t = a ? 2 : 3;\n\
                     char c = 'x';\n\
                     const char *s = \"hi\";\n\
                     return 0;\n\
                   }\n";
        let e = extract(src);
        let cf = names(&e, EntityKind::ControlFlow);
        for want in [
            "if_statement",
            "for_statement",
            "while_statement",
            "return_statement",
        ] {
            assert!(cf.contains(&want.to_string()), "missing {want}: {cf:?}");
        }
        let lits = names(&e, EntityKind::Literal);
        assert!(lits.iter().any(|l| l == "'x'"));
        assert!(lits.iter().any(|l| l == "\"hi\""));
    }
}
