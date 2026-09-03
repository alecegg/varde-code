//! C++ entity extraction.
//!
//! C++-specific mapping notes (fixture-driven, superset-safe), verified against
//! the tree-sitter-cpp grammar with `examples/dump_cpp.rs`. C++ reuses C's
//! node-kind mapping (shared declarator-unwrap via [`super::c`]) and adds the
//! class/namespace/exception constructs on top:
//! - `function_definition` -> Function (name via the unwrapped `declarator`;
//!   methods carry a `field_identifier` leaf, free functions an `identifier`).
//! - `class_specifier` / `struct_specifier` / `union_specifier` /
//!   `enum_specifier` (named) -> Class. `base_class_clause` (`: public Base`)
//!   -> one Extends per listed base type.
//! - `namespace_definition` -> a type scope (for `owner_type` linkage) and
//!   emits nothing itself; C++ namespaces are grouping scopes, not entities we
//!   model.
//! - `declaration` / `field_declaration` -> Variable (member and local
//!   variables); function prototypes are skipped. `parameter_declaration` ->
//!   Parameter. Names all go through [`super::c::declarator_identifier`].
//! - Import: `preproc_include` (`#include`, same as C) and
//!   `using_declaration` (`using std::string;` / `using namespace std;`) ->
//!   Import, spec taken from the declaration's identifier text. `::` is
//!   normalized to `/` so resolve.rs's shared `/`-segment stem matching applies
//!   (mirrors PHP's `\` handling); a header `#include` resolves to a sibling
//!   file, a `using` typically stays unresolved (a namespace, not a file).
//! - Call: `call_expression` (callee = `function` field) -> Call.
//!   `new_expression` -> Call, named after the constructed type (the closest
//!   kind — a constructor invocation).
//! - `field_expression` (`a.b` / `a->b` / method calls) -> MemberAccess.
//! - `number_literal` / `string_literal` / `char_literal` / `true` / `false`
//!   -> Literal.
//! - `try_statement` -> ControlFlow; `catch_clause` -> Catch, named after the
//!   caught parameter; `throw_statement` / `throw_expression` -> Throw, named
//!   after the thrown expression.
//! - ControlFlow: if/for/while/do/switch/case/return/break/continue/goto/try
//!   plus `conditional_expression`.
//! - Carve-outs (see [`REQUIRED_KINDS`]): Interface (C++ has no interface
//!   keyword — pure-virtual "interfaces" are too fuzzy to detect reliably),
//!   Export (no export statement we model), Route/Response (no dominant
//!   in-grammar web framework). Unlike C, C++ KEEPS Catch/Throw. Import is a
//!   bonus kind (fixtures + resolve check) but not on the required checklist.

use crate::extract::entity::ExtractCtx;
use crate::extract::langs::c::declarator_identifier;
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

type Node<'a> = ast_grep_core::Node<'a, StrDoc<SupportLang>>;

/// Node kinds that introduce a named function scope.
pub const FUNCTION_SCOPES: &[&str] = &["function_definition"];

/// Node kinds that introduce a named class/record/namespace type scope (for
/// `Entity::owner_type` linkage on methods).
pub const TYPE_SCOPES: &[&str] = &[
    "class_specifier",
    "struct_specifier",
    "union_specifier",
    "enum_specifier",
    "namespace_definition",
];

/// Entity kinds the fixtures must produce. Interface/Export/Route/Response are
/// carved out (see module docs); C++ keeps Catch/Throw. Import is a bonus kind
/// (exercised by fixtures + the resolve check) but not on the required list.
pub const REQUIRED_KINDS: [EntityKind; 10] = [
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
        "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
            if let Some(name) = node.field("name") {
                let cls = name.text().into_owned();
                ctx.push(EntityKind::Class, cls.clone(), node);
                // Inheritance: each base type in the base_class_clause -> Extends.
                if let Some(clause) = node.children().find(|c| c.kind() == "base_class_clause") {
                    for base in clause.children() {
                        if matches!(
                            base.kind().as_ref(),
                            "type_identifier" | "qualified_identifier" | "template_type"
                        ) {
                            super::push_type_ref(
                                ctx,
                                EntityKind::Extends,
                                base.text().into_owned(),
                                &cls,
                                &base,
                            );
                        }
                    }
                }
            }
        }
        "type_definition" => {
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
        "using_declaration" => {
            // `using std::string;` / `using namespace std;` — take the
            // trailing identifier/qualified name, normalizing `::` -> `/`.
            if let Some(spec) = using_spec(node) {
                ctx.push(EntityKind::Import, spec, node);
            }
        }

        // ---- variables ----
        "declaration" | "field_declaration" => {
            for decl in node.field_children("declarator") {
                if is_function_declarator(&decl) {
                    continue;
                }
                if let Some(name) = declarator_identifier(&decl) {
                    ctx.push(EntityKind::Variable, name, node);
                }
            }
            // A `field_declaration` with an initialized member (`int count = 0;`)
            // exposes its name as a bare `field_identifier` child rather than a
            // `declarator` field.
            if kind == "field_declaration"
                && node.field("declarator").is_none()
                && let Some(fid) = node.children().find(|c| c.kind() == "field_identifier")
            {
                ctx.push(EntityKind::Variable, fid.text().into_owned(), node);
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
        "new_expression" => {
            // `new Foo(...)` — a constructor invocation; map to Call named
            // after the constructed type.
            let name = node
                .field("type")
                .map(|t| t.text().into_owned())
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

        // ---- exceptions ----
        "catch_clause" => {
            // Name after the caught parameter (`catch (const E& e)` -> "e").
            let name = node
                .field("parameters")
                .and_then(|p| p.children().find(|c| c.kind() == "parameter_declaration"))
                .and_then(|pd| {
                    pd.field("declarator")
                        .and_then(|d| declarator_identifier(&d))
                })
                .unwrap_or_default();
            ctx.push(EntityKind::Catch, name, node);
        }
        "throw_statement" | "throw_expression" => {
            // Name after the thrown expression (the first named child).
            let name = node
                .children()
                .find(|c| c.get_inner_node().is_named())
                .map(|c| c.text().into_owned())
                .unwrap_or_default();
            ctx.push(EntityKind::Throw, name, node);
        }

        // ---- control flow ----
        "if_statement"
        | "for_statement"
        | "for_range_loop"
        | "while_statement"
        | "do_statement"
        | "switch_statement"
        | "case_statement"
        | "return_statement"
        | "break_statement"
        | "continue_statement"
        | "goto_statement"
        | "try_statement"
        | "conditional_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// True when this declarator (after peeling pointer/array wrappers) is a
/// `function_declarator` — a method/function prototype, not a storage variable.
fn is_function_declarator(node: &Node<'_>) -> bool {
    match node.kind().as_ref() {
        "function_declarator" => true,
        "pointer_declarator"
        | "array_declarator"
        | "reference_declarator"
        | "parenthesized_declarator" => node
            .field("declarator")
            .map(|d| is_function_declarator(&d))
            .unwrap_or(false),
        _ => false,
    }
}

/// Spec of a `using_declaration`: the trailing qualified/plain identifier,
/// with `::` normalized to `/` for the shared stem matcher. Returns `None`
/// when the declaration carries no name (defensive).
fn using_spec(node: &Node<'_>) -> Option<String> {
    let name = node
        .children()
        .filter(|c| {
            matches!(
                c.kind().as_ref(),
                "qualified_identifier" | "identifier" | "namespace_identifier" | "type_identifier"
            )
        })
        .map(|c| c.text().into_owned())
        .next()?;
    Some(name.replace("::", "/"))
}

/// Strip the surrounding quotes/angle brackets from an include `path` node:
/// `"helper.h"` -> `helper.h`, `<string>` -> `string`.
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
        let parsed = parse_source(&SupportLang::Cpp, src);
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
    fn class_with_base_emits_class_and_extends() {
        let src = "class Base {};\nclass Widget : public Base { public: void run(); };\n";
        let e = extract(src);
        let classes = names(&e, EntityKind::Class);
        assert!(classes.contains(&"Base".to_string()), "got {classes:?}");
        assert!(classes.contains(&"Widget".to_string()), "got {classes:?}");
        assert!(
            names(&e, EntityKind::Extends).contains(&"Base".to_string()),
            "extends: {:?}",
            names(&e, EntityKind::Extends)
        );
    }

    #[test]
    fn method_name_and_params() {
        let src = "class W { public: void run(int n) { int x = n; go(x); } };\n";
        let e = extract(src);
        assert!(names(&e, EntityKind::Function).contains(&"run".to_string()));
        assert!(names(&e, EntityKind::Parameter).contains(&"n".to_string()));
        assert!(names(&e, EntityKind::Variable).contains(&"x".to_string()));
        assert!(names(&e, EntityKind::Call).contains(&"go".to_string()));
    }

    #[test]
    fn member_variable_initializer_is_variable() {
        let src = "class W { int count = 0; };\n";
        let e = extract(src);
        assert!(
            names(&e, EntityKind::Variable).contains(&"count".to_string()),
            "got {:?}",
            names(&e, EntityKind::Variable)
        );
    }

    #[test]
    fn try_catch_throw_and_new() {
        let src = "void f() {\n\
                     Base* b = new Base();\n\
                     try { b->go(); throw std::runtime_error(\"x\"); }\n\
                     catch (const std::exception& e) { handle(e); }\n\
                   }\n";
        let e = extract(src);
        assert!(names(&e, EntityKind::ControlFlow).contains(&"try_statement".to_string()));
        assert!(
            names(&e, EntityKind::Catch).contains(&"e".to_string()),
            "catch: {:?}",
            names(&e, EntityKind::Catch)
        );
        assert!(
            !names(&e, EntityKind::Throw).is_empty(),
            "throw missing: {:?}",
            names(&e, EntityKind::Throw)
        );
        // `new Base()` -> Call named after the type.
        assert!(
            names(&e, EntityKind::Call).contains(&"Base".to_string()),
            "call: {:?}",
            names(&e, EntityKind::Call)
        );
    }

    #[test]
    fn includes_and_using_emit_imports() {
        let src = "#include \"helper.h\"\nusing std::string;\n";
        let e = extract(src);
        let imports = names(&e, EntityKind::Import);
        assert!(imports.contains(&"helper.h".to_string()), "got {imports:?}");
        assert!(
            imports.iter().any(|i| i == "std/string"),
            "using not normalized: {imports:?}"
        );
    }

    #[test]
    fn member_access_and_literals() {
        let src = "void f(W* p) { int y = p->count; bool ok = true; double d = 3.14; }\n";
        let e = extract(src);
        assert!(names(&e, EntityKind::MemberAccess).contains(&"count".to_string()));
        let lits = names(&e, EntityKind::Literal);
        assert!(lits.iter().any(|l| l == "true"));
        assert!(lits.iter().any(|l| l == "3.14"));
    }
}
