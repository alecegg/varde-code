//! Solidity entity extraction.
//!
//! Mapping notes (fixture-driven, superset-safe; node kinds from
//! tree-sitter-solidity via ast-grep-language 0.45.1, verified against a
//! grammar dump):
//! - `function_definition` (`function f(...) { ... }`, incl. abstract/interface
//!   `function ping() external;` bodies) AND `modifier_definition`
//!   (`modifier onlyOwner() { ... }`) -> Function; name via field `name`. A
//!   `constructor_definition` and a `fallback_receive_definition`
//!   (`constructor(...)`, `receive()`, `fallback()`) carry NO `name` field —
//!   they are Solidity's special unnamed members — so they map to Function with
//!   a synthetic name ("constructor" / the leading keyword text) rather than
//!   being dropped.
//! - `contract_declaration` and `library_declaration` -> Class; a library is a
//!   stateless deployed code unit, structurally the closest Class analog (there
//!   is no dedicated module kind in the shared model).
//! - `interface_declaration` -> Interface.
//! - Inheritance `contract C is Base, IFace`: each supertype is a sibling
//!   `inheritance_specifier` child (wrapping a `user_defined_type`). Solidity's
//!   `is` list does NOT syntactically distinguish a base contract from an
//!   implemented interface (both use `is`), so — mirroring scala.rs's
//!   extends_clause first-vs-rest rule — the FIRST specifier maps to Extends and
//!   each remaining one to Implements, owned by the contract. This is a
//!   defensible approximation, not a semantic guarantee.
//! - `state_variable_declaration` (`uint public count;`) and local
//!   `variable_declaration` (the declarator inside a
//!   `variable_declaration_statement`, `uint local = x + 1;`) -> Variable, via
//!   field `name`.
//! - `parameter` -> Parameter, via field `name`. A return-type parameter
//!   (`returns (uint)`) has only a `type` field and no `name`, so it is
//!   intentionally not emitted as a Parameter.
//! - `call_expression` -> Call, named after the callee. The `function` field is
//!   an `expression` wrapper around a bare `identifier` (`foo(...)`) or a
//!   `member_expression` receiver call (`x.foo(...)`, whose `property` is the
//!   method name). EXCEPTION: `require(...)` / `assert(...)` -> Throw (see
//!   below), returning early so they don't ALSO emit a plain Call.
//! - `member_expression` (`x.foo`, `msg.sender`) -> MemberAccess for the
//!   accessed member (`property` field). A receiver call `x.foo(...)` therefore
//!   emits both a MemberAccess ("foo") and a Call ("foo").
//! - `import_directive` (`import "./Foo.sol";`, `import {A} from "./B.sol";`)
//!   -> Import; spec = the unquoted path string (the last `string` child, i.e.
//!   the `from` target), so resolve.rs's shared file-stem matching resolves a
//!   relative sibling import (`./b.sol` -> sibling `b.sol`).
//! - Literals: `number_literal` (incl. hex `0x1F`), `string_literal`,
//!   `boolean_literal`, excluding type contexts (`in_type`).
//! - Error-check idiom -> Throw (NOT Import — `require` is not a module load):
//!   a `call_expression` whose callee identifier is `require`/`assert`, and the
//!   `revert_statement` (`revert("msg")` and `revert CustomError(...)`), all map
//!   to Throw. The require/assert branch returns early so no duplicate Call is
//!   emitted. `revert CustomError(...)` is named after the custom error;
//!   `revert("...")` / bare `revert()` is named "revert".
//! - `emit_statement` (`emit Transfer(...)`) -> Call, named after the emitted
//!   event (`name` field). Events are observable effects; the nearest shared
//!   shape is a Call to the event. `event_definition` / `error_declaration`
//!   (the DECLARATIONS) are contract members with no clean shared kind (not a
//!   function, not a field), so they are intentionally not emitted — documented
//!   carve-out; only their USE sites (emit / revert) surface.
//! - `catch_clause` (`try ... catch { ... }`, `catch Error(string m) { ... }`)
//!   -> Catch, named after the bound exception parameter if present, else "".
//! - ControlFlow: `if_statement`, `for_statement`, `while_statement`,
//!   `try_statement`, `return_statement`. (Solidity has no `switch`; `break`/
//!   `continue` exist but aren't in the fixture's REQUIRED set — the mapped set
//!   is honest about what fixtures produce.)
//! - Export: carved out — Solidity has no `export` keyword; every file-scope
//!   declaration is importable by path, so there is no export statement to map.
//! - Route/Response: carved out — Solidity is a smart-contract language with no
//!   web-routing DSL (same stance as C/Scala/Dart).
//! - Decorator: carved out — modifier *usage* (`function f() onlyOwner`) has no
//!   distinct node kind (it appears inline in the function header), and NatSpec
//!   `@dev` tags live in comments, so there is no clean node to map.

use crate::extract::entity::ExtractCtx;
use crate::extract::field_name;
use crate::extract::langs::{push_type_ref, unquote};
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope. `function_definition` and
/// `modifier_definition` carry the `name` field; `constructor_definition` and
/// `fallback_receive_definition` are unnamed special members but still open a
/// scope for the control-flow/error entities in their bodies.
pub const FUNCTION_SCOPES: &[&str] = &[
    "function_definition",
    "modifier_definition",
    "constructor_definition",
    "fallback_receive_definition",
];

/// Node kinds that introduce a named contract/library/interface type scope (for
/// `Entity::owner_type` linkage on methods and Extends/Implements ownership).
pub const TYPE_SCOPES: &[&str] = &[
    "contract_declaration",
    "library_declaration",
    "interface_declaration",
];

/// Entity kinds the fixtures must produce. Export is carved out (Solidity has
/// no export keyword); Route/Response are carved out (no web DSL). See module
/// docs for the full rationale.
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
    EntityKind::Extends,
];

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- imports ----
        "import_directive" => {
            if let Some(path) = import_path(node) {
                ctx.push(EntityKind::Import, path, node);
            }
        }

        // ---- structural ----
        "function_definition" | "modifier_definition" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name, node);
        }
        "constructor_definition" => {
            ctx.push(EntityKind::Function, "constructor".to_string(), node);
        }
        "fallback_receive_definition" => {
            // `receive() ...` / `fallback() ...`: no name field; the leading
            // keyword is the first identifier-like token of the node text.
            ctx.push(EntityKind::Function, special_member_name(node), node);
        }
        "contract_declaration" | "library_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            visit_supertypes(node, &name, ctx);
        }
        "interface_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Interface, name.clone(), node);
            visit_supertypes(node, &name, ctx);
        }
        // `event Transfer(address indexed from, ...)` / `error Unauthorized()`
        // — named, parameterized contract members. Previously dropped entirely
        // (audit S3: ERC20 Transfer/Approval never indexed, so `get_symbol
        // Transfer` and interface/event coverage failed). Mapped to Function
        // (the closest callable-signature kind; `emit`/`revert` invoke them),
        // carrying the owning contract via `ctx.push`.
        "event_definition" | "error_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name, node);
        }

        // ---- variables (state + locals) ----
        "state_variable_declaration" | "variable_declaration" => {
            if let Some(name) = field_name(node) {
                ctx.push(EntityKind::Variable, name, node);
            }
        }

        // ---- parameters ----
        "parameter" => {
            if let Some(name) = field_name(node) {
                ctx.push(EntityKind::Parameter, name, node);
            }
        }

        // ---- expression-level ----
        "call_expression" => {
            let name = call_name(node);
            // `require(...)` / `assert(...)` are error-check idioms, not calls.
            if name == "require" || name == "assert" {
                ctx.push(EntityKind::Throw, name, node);
                return;
            }
            ctx.push(EntityKind::Call, name, node);
        }
        "emit_statement" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Call, name, node);
        }
        "member_expression" => {
            if let Some(prop) = node.field("property") {
                ctx.push(EntityKind::MemberAccess, prop.text().into_owned(), node);
            }
        }
        "number_literal" | "string_literal" | "boolean_literal" => {
            if !ctx.in_type {
                ctx.push(EntityKind::Literal, node.text().into_owned(), node);
            }
        }

        // ---- error handling ----
        "revert_statement" => {
            ctx.push(EntityKind::Throw, revert_name(node), node);
        }
        "catch_clause" => {
            ctx.push(EntityKind::Catch, catch_var(node), node);
        }

        // ---- control flow ----
        "if_statement" | "for_statement" | "while_statement" | "try_statement"
        | "return_statement" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// Emit Extends (the first `inheritance_specifier`) + Implements (each remaining
/// one) for a contract/interface's `is` list, owned by `owner`. Solidity does
/// not distinguish a base contract from an interface in the `is` list, so the
/// first-vs-rest split mirrors scala.rs (a defensible approximation).
fn visit_supertypes(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    owner: &str,
    ctx: &mut ExtractCtx,
) {
    let mut specs = node
        .children()
        .filter(|c| c.kind() == "inheritance_specifier");
    if let Some(base) = specs.next() {
        push_type_ref(
            ctx,
            EntityKind::Extends,
            supertype_name(&base),
            owner,
            &base,
        );
    }
    for iface in specs {
        push_type_ref(
            ctx,
            EntityKind::Implements,
            supertype_name(&iface),
            owner,
            &iface,
        );
    }
}

/// Bare name of an `inheritance_specifier` (`Base` / `IFace`): the inner
/// `user_defined_type`'s text, falling back to the specifier's own text.
fn supertype_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.children()
        .find(|c| c.kind() == "user_defined_type")
        .map(|c| c.text().into_owned())
        .unwrap_or_else(|| node.text().into_owned())
}

/// Callee name of a `call_expression`. The `function` field is an `expression`
/// wrapper around a bare `identifier` (`foo(...)`) or a `member_expression`
/// receiver call (`x.foo(...)`, whose `property` is the method name).
fn call_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let Some(func) = node.field("function") else {
        return String::new();
    };
    let inner = unwrap_expression(func);
    if inner.kind() == "member_expression"
        && let Some(prop) = inner.field("property")
    {
        return prop.text().into_owned();
    }
    inner.text().into_owned()
}

/// Name of a `revert_statement`: `revert CustomError(...)` -> the custom error
/// identifier (the statement's first named non-argument child); `revert("...")`
/// / `revert()` -> "revert".
fn revert_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let Some(first) = node.children().find(|c| c.is_named()) else {
        return "revert".to_string();
    };
    let inner = unwrap_expression(first);
    // A custom-error revert names an identifier; a `revert("msg")` unwraps to a
    // parenthesized/string expression, not an identifier.
    if inner.kind() == "identifier" {
        return inner.text().into_owned();
    }
    "revert".to_string()
}

/// Exception variable bound by a `catch_clause` (`catch Error(string m) {...}`
/// -> "m", plain `catch {...}` -> ""). The binding is the `name` of the first
/// `parameter` child, if any.
fn catch_var(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.children()
        .find(|c| c.kind() == "parameter")
        .and_then(|p| field_name(&p))
        .unwrap_or_default()
}

/// Unquoted import path of an `import_directive`: the `from` target is the last
/// `string` child (`import {A} from "./B.sol"` -> `./B.sol`; a plain
/// `import "./Foo.sol"` has a single `string` child). Returns `None` if no
/// string child exists (e.g. a bare `import * as X` with no path — not idiomatic
/// for the file-based imports this model resolves).
fn import_path(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    let s = node.children().filter(|c| c.kind() == "string").last()?;
    Some(unquote(s.text().trim(), false))
}

/// Leading keyword name of an unnamed special member
/// (`fallback_receive_definition`): "receive" or "fallback". Falls back to the
/// first word of the node's text.
fn special_member_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.text()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .find(|w| !w.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Peel `expression` wrapper nodes (the Solidity grammar wraps most operands in
/// an `expression` node) to reach the operative child.
fn unwrap_expression<'t>(
    mut node: ast_grep_core::Node<'t, StrDoc<SupportLang>>,
) -> ast_grep_core::Node<'t, StrDoc<SupportLang>> {
    while node.kind() == "expression" {
        let Some(inner) = node.children().find(|c| c.is_named()) else {
            break;
        };
        node = inner;
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::model::Entity;
    use crate::parse::parse_source;

    fn entities(src: &str) -> Vec<Entity> {
        let parsed = parse_source(&SupportLang::Solidity, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        extract::extract(&parsed, 0).entities
    }

    fn find<'a>(es: &'a [Entity], kind: EntityKind, name: &str) -> Option<&'a Entity> {
        es.iter().find(|e| e.kind == kind && e.name == name)
    }

    const PREAMBLE: &str = "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\n";

    fn with_preamble(body: &str) -> String {
        format!("{PREAMBLE}{body}")
    }

    #[test]
    fn events_and_errors_are_captured_as_functions() {
        // Audit S3: `event` and `error` declarations were never indexed.
        let es = entities(&with_preamble(
            "contract T {\n  event Transfer(address indexed from, address indexed to);\n  error Unauthorized();\n  function f() public {}\n}\n",
        ));
        assert!(
            find(&es, EntityKind::Function, "Transfer").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::Function, "Unauthorized").is_some(),
            "{es:?}"
        );
        // The owning contract is recorded so event/interface coverage works.
        assert_eq!(
            find(&es, EntityKind::Function, "Transfer")
                .unwrap()
                .owner_type
                .as_deref(),
            Some("T")
        );
    }

    #[test]
    fn contract_interface_library_map_to_expected_kinds() {
        let es = entities(&with_preamble(
            "interface IFace { function ping() external; }\nlibrary L { function add(uint a) internal pure returns (uint) { return a; } }\ncontract C { function f() public {} }\n",
        ));
        assert!(
            find(&es, EntityKind::Interface, "IFace").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Function, "ping").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Class, "L").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Function, "add").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Class, "C").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Function, "f").is_some(), "{es:?}");
    }

    #[test]
    fn is_list_splits_into_extends_and_implements() {
        let es = entities(&with_preamble(
            "contract Service is Base, IFace, IOther {}\n",
        ));
        let ext = find(&es, EntityKind::Extends, "Base").expect("Extends Base");
        assert_eq!(ext.enclosing_function.as_deref(), Some("Service"));
        let iface = find(&es, EntityKind::Implements, "IFace").expect("Implements IFace");
        assert_eq!(iface.enclosing_function.as_deref(), Some("Service"));
        assert!(
            find(&es, EntityKind::Implements, "IOther").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn state_locals_and_params_map_to_variable_and_parameter() {
        let es = entities(&with_preamble(
            "contract C {\n  uint public count;\n  address owner;\n  function f(uint x, address a) public {\n    uint local = x;\n  }\n}\n",
        ));
        assert!(find(&es, EntityKind::Variable, "count").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Variable, "owner").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Variable, "local").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Parameter, "x").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Parameter, "a").is_some(), "{es:?}");
    }

    #[test]
    fn receiver_call_emits_member_access_and_call() {
        let es = entities(&with_preamble(
            "contract C {\n  function f() public {\n    repo.save(1);\n  }\n}\n",
        ));
        assert!(
            find(&es, EntityKind::MemberAccess, "save").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Call, "save").is_some(), "{es:?}");
    }

    #[test]
    fn require_assert_and_revert_map_to_throw_not_call() {
        let es = entities(&with_preamble(
            "contract C {\n  error Bad(uint c);\n  function f(uint x) public {\n    require(x > 0, \"bad\");\n    assert(x != 0);\n    if (x > 9) { revert(\"big\"); }\n    revert Bad(5);\n  }\n}\n",
        ));
        assert!(find(&es, EntityKind::Throw, "require").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Throw, "assert").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Throw, "revert").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Throw, "Bad").is_some(), "{es:?}");
        // require/assert must NOT also emit a plain Call.
        assert!(find(&es, EntityKind::Call, "require").is_none(), "{es:?}");
        assert!(find(&es, EntityKind::Call, "assert").is_none(), "{es:?}");
    }

    #[test]
    fn try_catch_and_control_flow_extracted() {
        let es = entities(&with_preamble(
            "contract C {\n  function g() external returns (uint) { return 1; }\n  function f(uint x) public {\n    for (uint i = 0; i < x; i++) {}\n    while (x > 0) { x--; }\n    if (x == 0) { return; }\n    try this.g() returns (uint r) { return; } catch { return; }\n  }\n}\n",
        ));
        assert!(
            find(&es, EntityKind::ControlFlow, "for_statement").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::ControlFlow, "while_statement").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::ControlFlow, "if_statement").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::ControlFlow, "try_statement").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Catch, "").is_some(), "{es:?}");
    }

    #[test]
    fn imports_yield_unquoted_paths() {
        let es = entities(&with_preamble(
            "import \"./Foo.sol\";\nimport {A, B} from \"./B.sol\";\n",
        ));
        assert!(
            find(&es, EntityKind::Import, "./Foo.sol").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Import, "./B.sol").is_some(), "{es:?}");
    }

    #[test]
    fn literals_and_emit_extracted() {
        let es = entities(&with_preamble(
            "contract C {\n  event E(uint v);\n  function f() public {\n    uint a = 42;\n    string memory s = \"hi\";\n    bool b = true;\n    emit E(a);\n  }\n}\n",
        ));
        assert!(find(&es, EntityKind::Literal, "42").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Literal, "true").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Call, "E").is_some(), "{es:?}");
    }
}
