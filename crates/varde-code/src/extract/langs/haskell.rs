//! Haskell entity extraction.
//!
//! Haskell is a pure functional language with no OOP classes, no statement-level
//! control flow (everything is an expression), and no built-in exception syntax
//! (exceptions are library functions). The mapping is fixture-driven and
//! superset-safe: every kind in [`REQUIRED_KINDS`] is produced by the fixtures.
//!
//! Node-kind -> EntityKind mapping (verified against the tree-sitter-haskell
//! dump via `ast-grep-language` 0.45.1):
//! - Function: `function` — a binding with a `patterns` field (`f x = …`). The
//!   name is the `name` field (a `variable` leaf). Haskell functions are
//!   **equations with pattern-matched clauses**: the same name may have MANY
//!   `function` nodes (one per clause, e.g. `g 0 = 1` and `g n = …`). We emit a
//!   Function per clause — the resolver/dedup tolerates repeats and per-clause
//!   spans are individually useful (matches the "emit per binding" guidance).
//!   Type signatures (`f :: Int -> Int`) are separate `signature` nodes that
//!   carry no implementation, so they are NOT emitted as Function (avoids a
//!   phantom duplicate for every signed binding).
//! - Variable: `bind` — a binding WITHOUT patterns (`x = …`). Covers top-level
//!   value bindings (`useM = M.lookup …`), and `let`/`where` local binds (both
//!   are `bind` nodes). Name is the `name` field. A `bind` whose RHS is a
//!   lambda is still a Variable (we do not special-case lambda-valued binds;
//!   the simple pattern-vs-no-pattern split is the documented rule).
//! - Class: `data_type` (`data T = …`), `newtype` (`newtype W = …`), and
//!   `type_synomym` (`type A = …`). Haskell has no OOP class; these are its
//!   defined-type declarations and Class is the closest structural analog.
//!   Name is the `name` field.
//! - Interface: `class` — a **typeclass** declaration (`class Show a where …`).
//!   A typeclass is a named behavioural contract, so Interface is the right kind
//!   (not Class). Name is the `name` field.
//! - Implements: `instance` — an `instance Show T where …` declaration maps to
//!   Implements referencing the typeclass being instantiated (the `name` field,
//!   `Show`). Haskell has no owner-type scope for instances (see the scope
//!   carve-out below), so the entity carries `enclosing_function = None`.
//! - Parameter: each `variable` leaf inside a `patterns` node (a function
//!   clause's argument patterns). Wildcards (`_`) are skipped.
//! - Export: `export` — an entry in the module header's export list
//!   (`module Foo (a, b) where`). Haskell HAS an explicit export construct and
//!   the grammar exposes it cleanly (`header` -> `exports` -> `export`), so each
//!   exported name is an Export. Name is the export's text (`a`, `MyType(..)`).
//! - Import: `import` — `import Data.List (…)` / `import qualified Data.Map as M`.
//!   Spec is the imported module path from the `module` field, normalized to a
//!   `/`-separated path (`Data.List` -> `Data/List`) so resolve.rs's shared
//!   segment stem matching applies. NOTE: Haskell imports are module/package-
//!   based and in general do NOT correspond to file stems (a module name need
//!   not mirror a file path), so most imports stay unresolved — the accepted
//!   approximation per the roadmap Risks section.
//! - Call: `apply` — function application (`g x y`, `error "msg"`). The callee
//!   is the head of the (left-nested) application spine: a bare `variable`
//!   (`g`), or a `qualified` name (`M.lookup` -> "lookup"). `error`/`throw`/
//!   `throwIO`/`ioError`/`errorWithoutStackTrace` heads become Throw and
//!   `catch`/`handle`/`try`/`bracket`/`finally` heads become Catch (Haskell's
//!   exception idiom is library functions, not syntax — see below).
//! - MemberAccess: `qualified` (`M.lookup` -> "lookup") — the qualified-name
//!   form is Haskell's closest analog to member access (a name reached through a
//!   module namespace). Record field access is also function application in
//!   Haskell (`field record`), already covered by Call, so no separate handling.
//! - Literal: the leaves under a `literal` node — `integer` / `float` /
//!   `string` / `char`. `literal` itself is a wrapper; matching the leaves keeps
//!   the emitted text tight (`1`, `"hi"`, `'c'`).
//! - Throw: an `apply` whose callee is `error`/`throw`/`throwIO`/`ioError`/
//!   `errorWithoutStackTrace`. Haskell has no `throw` keyword — these library
//!   functions are the error-raising idiom. Named after the callee.
//! - Catch: an `apply` whose callee is `catch`/`handle`/`try`/`bracket`/
//!   `finally`. Haskell has no `try`/`catch` syntax — these `Control.Exception`
//!   combinators are the protected-execution idiom, the closest structural
//!   analog to a catch boundary. Named after the callee.
//! - ControlFlow: `conditional` (`if … then … else …`), `case`
//!   (`case … of …`), `guards` (equation guards `| cond = …`), and `let_in`
//!   (`let … in …`). Haskell has no statement-level control flow — these are all
//!   expressions — but they are the branch/binding constructs and map cleanly.
//!
//! Carve-outs (documented, not silently dropped):
//! - Catch is INCLUDED (library-combinator best-effort, above); Throw is
//!   INCLUDED. Neither is carved out.
//! - Extends: carved out. Haskell has no subtype/inheritance relation; typeclass
//!   superclass constraints (`class Eq a => Ord a`) are a constraint context,
//!   not a structural "extends" of the same shape the model means, so no Extends
//!   is emitted. (Implements is still produced for `instance`.)
//! - Decorator: carved out — Haskell has no annotation/decorator syntax.
//! - Route / Response: carved out — Haskell has no single idiomatic web DSL
//!   (Servant, Yesod, Scotty, and WAI all differ), so both are carved out rather
//!   than encode one framework's shape (same stance as Lua/Scala).
//!
//! SCOPE-STACKING CARVE-OUT: the walker (`crate::extract::walk`) stacks the
//! enclosing-function / owner-type scopes by matching `node.kind()` against
//! [`FUNCTION_SCOPES`] / [`TYPE_SCOPES`] and naming the scope via
//! `field_name(node)`. Haskell `function`/`bind` nodes DO expose a `name` field,
//! so [`FUNCTION_SCOPES`] is populated and nested control-flow/error entities
//! link to their enclosing binding. [`TYPE_SCOPES`] covers `data_type`/`newtype`
//! /`type_synomym`/`class` (all expose `name`); `instance` has a `name` field
//! that is the *typeclass*, not an owner type, so it is intentionally excluded
//! from TYPE_SCOPES (its members would otherwise be tagged with the wrong owner).

use crate::extract::entity::ExtractCtx;
use crate::extract::field_name;
use crate::extract::langs::push_type_ref;
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function/binding scope (for
/// `enclosing_function` linkage). `function` is a pattern-clause binding;
/// `bind` is a no-pattern value binding. Both expose a `name` field.
pub const FUNCTION_SCOPES: &[&str] = &["function", "bind"];

/// Node kinds that introduce a named type scope (for `owner_type` linkage). The
/// defined-type declarations and the typeclass all expose `name`. `instance` is
/// excluded: its `name` field is the typeclass being implemented, not an owner
/// type (see the module-doc scope carve-out).
pub const TYPE_SCOPES: &[&str] = &["data_type", "newtype", "type_synomym", "class"];

/// Entity kinds the fixtures must produce. Route/Response/Extends/Decorator are
/// carved out (see module docs). Export IS included (explicit export list);
/// Catch/Throw ARE included (library combinators). Implements comes from
/// `instance` but is not in the 14-kind checklist, so it is not listed here.
pub const REQUIRED_KINDS: [EntityKind; 13] = [
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
    EntityKind::Import,
];

/// Application heads that raise an error (Haskell's `throw` analog).
const THROW_FUNCTIONS: &[&str] = &[
    "error",
    "throw",
    "throwIO",
    "ioError",
    "errorWithoutStackTrace",
];

/// Application heads that run a protected/exception-handling action (Haskell's
/// `try`/`catch` analog — `Control.Exception` combinators).
const CATCH_FUNCTIONS: &[&str] = &["catch", "handle", "try", "bracket", "finally"];

/// Emit entities for one node (called for every node in the tree).
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    match kind {
        // ---- imports / exports ----
        "import" => {
            ctx.push(EntityKind::Import, import_spec(node), node);
        }
        "export" => {
            ctx.push(EntityKind::Export, node.text().into_owned(), node);
        }

        // ---- structural: functions & value binds ----
        // `f x = …` (pattern clause) -> Function. Multi-clause functions emit
        // one Function per clause (see module docs).
        "function" => {
            ctx.push(
                EntityKind::Function,
                field_name(node).unwrap_or_default(),
                node,
            );
        }
        // `x = …` (no patterns) -> Variable. Covers top-level value binds and
        // `let`/`where` local binds uniformly.
        "bind" => {
            ctx.push(
                EntityKind::Variable,
                field_name(node).unwrap_or_default(),
                node,
            );
        }

        // ---- structural: types & typeclasses ----
        "data_type" | "newtype" | "type_synomym" => {
            ctx.push(
                EntityKind::Class,
                field_name(node).unwrap_or_default(),
                node,
            );
        }
        "class" => {
            ctx.push(
                EntityKind::Interface,
                field_name(node).unwrap_or_default(),
                node,
            );
        }
        // `instance Show T where …` -> Implements referencing the typeclass.
        "instance" => {
            push_type_ref(
                ctx,
                EntityKind::Implements,
                field_name(node).unwrap_or_default(),
                "",
                node,
            );
        }

        // ---- parameters ----
        "patterns" => {
            for name in pattern_param_names(node) {
                ctx.push(EntityKind::Parameter, name, node);
            }
        }

        // ---- application (calls / throws / catches) ----
        "apply" => visit_apply(node, ctx),

        // ---- qualified name access (`M.lookup`) -> MemberAccess ----
        "qualified" => {
            // A `qualified` that is the callee of an `apply` is handled in
            // `visit_apply` (which emits the MemberAccess there so the callee
            // isn't processed twice). Skip when the parent is that `apply`'s
            // head spine.
            if is_apply_callee(node) {
                return;
            }
            if let Some(member) = qualified_member(node) {
                ctx.push(EntityKind::MemberAccess, member, node);
            }
        }

        // ---- literals ----
        "integer" | "float" | "string" | "char" => {
            ctx.push(EntityKind::Literal, node.text().into_owned(), node);
        }

        // ---- control flow (all expressions in Haskell) ----
        "conditional" | "case" | "guards" | "let_in" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        _ => {}
    }
}

/// Handle an `apply` (function application): dispatch on the callee head to
/// Throw / Catch, emit a MemberAccess for a qualified head, and always emit the
/// plain Call.
fn visit_apply(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>, ctx: &mut ExtractCtx) {
    let Some(head) = apply_head(node) else {
        return;
    };
    let callee = callee_name(&head);

    // `error "msg"` / `throwIO e` -> Throw (not a plain Call).
    if THROW_FUNCTIONS.contains(&callee.as_str()) {
        ctx.push(EntityKind::Throw, callee, node);
        return;
    }
    // `catch action handler` / `try act` -> Catch (not a plain Call).
    if CATCH_FUNCTIONS.contains(&callee.as_str()) {
        ctx.push(EntityKind::Catch, callee, node);
        return;
    }
    // `M.lookup x` -> MemberAccess for the accessed member, plus the Call.
    if head.kind() == "qualified"
        && let Some(member) = qualified_member(&head)
    {
        ctx.push(EntityKind::MemberAccess, member, node);
    }
    ctx.push(EntityKind::Call, callee, node);
}

/// The head of an application spine. `apply` is left-nested (`((g x) y)`), so
/// the callee is reached by descending the `function` field until it is no
/// longer an `apply`.
fn apply_head<'t>(
    node: &ast_grep_core::Node<'t, StrDoc<SupportLang>>,
) -> Option<ast_grep_core::Node<'t, StrDoc<SupportLang>>> {
    let mut cur = node.field("function")?;
    while cur.kind() == "apply" {
        cur = cur.field("function")?;
    }
    Some(cur)
}

/// True when `node` is the head-spine callee of an enclosing `apply` (so its
/// MemberAccess is emitted by `visit_apply`, not the standalone `qualified`
/// arm). Walks up through the left-nested `function` fields.
fn is_apply_callee(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> bool {
    let mut child = node.clone();
    while let Some(parent) = child.parent() {
        if parent.kind() != "apply" {
            return false;
        }
        if parent
            .field("function")
            .is_some_and(|f| f.node_id() == child.node_id())
        {
            return true;
        }
        child = parent;
    }
    false
}

/// Callee name of an application head: a bare `variable`/`constructor` yields
/// its text; a `qualified` (`M.lookup`) yields the member (`lookup`).
fn callee_name(head: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    if head.kind() == "qualified"
        && let Some(member) = qualified_member(head)
    {
        return member;
    }
    head.text().into_owned()
}

/// The accessed member of a `qualified` node (`M.lookup` -> "lookup"): the
/// trailing `variable`/`constructor`/`operator` leaf after the module prefix.
fn qualified_member(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.children()
        .filter(|c| matches!(c.kind().as_ref(), "variable" | "constructor" | "operator"))
        .last()
        .map(|c| c.text().into_owned())
}

/// Slash-separated module path of an `import` node's `module` field
/// (`Data.List` -> "Data/List"). Falls back to the empty string.
fn import_spec(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    node.field("module")
        .map(|m| m.text().replace('.', "/"))
        .unwrap_or_default()
}

/// Parameter identifier names inside a `patterns` node: the `variable` leaves
/// (a clause's argument patterns), skipping the wildcard `_`.
fn pattern_param_names(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Vec<String> {
    let mut names = Vec::new();
    collect_pattern_vars(node, &mut names);
    names
}

/// Recursively collect `variable` leaves of a pattern subtree (covers nested
/// patterns like `(x, y)` and `Just z`), skipping wildcards.
fn collect_pattern_vars(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    out: &mut Vec<String>,
) {
    if node.kind() == "variable" {
        let t = node.text().into_owned();
        if t != "_" {
            out.push(t);
        }
        return;
    }
    for child in node.children() {
        collect_pattern_vars(&child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::model::Entity;
    use crate::parse::parse_source;

    fn entities(src: &str) -> Vec<Entity> {
        let parsed = parse_source(&SupportLang::Haskell, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        extract::extract(&parsed, 0).entities
    }

    fn find<'a>(es: &'a [Entity], kind: EntityKind, name: &str) -> Option<&'a Entity> {
        es.iter().find(|e| e.kind == kind && e.name == name)
    }

    #[test]
    fn function_binding_and_params() {
        let es = entities("f :: Int -> Int -> Int\nf x y = x + y\n");
        assert!(find(&es, EntityKind::Function, "f").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Parameter, "x").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Parameter, "y").is_some(), "{es:?}");
        // A bare signature must not become a phantom Function duplicate beyond
        // the clause itself.
        let fns: Vec<_> = es
            .iter()
            .filter(|e| e.kind == EntityKind::Function && e.name == "f")
            .collect();
        assert_eq!(fns.len(), 1, "one clause -> one Function: {es:?}");
    }

    #[test]
    fn multi_clause_function_emits_one_per_clause() {
        let es = entities("g :: Int -> Int\ng 0 = 1\ng n = n * 2\n");
        let count = es
            .iter()
            .filter(|e| e.kind == EntityKind::Function && e.name == "g")
            .count();
        assert_eq!(count, 2, "two clauses -> two Functions: {es:?}");
    }

    #[test]
    fn data_newtype_type_are_class_and_class_is_interface() {
        let es = entities(
            "data T = A | B\nnewtype W = W Int\ntype Alias = Int\nclass Show a where\n  showit :: a -> String\n",
        );
        assert!(find(&es, EntityKind::Class, "T").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Class, "W").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Class, "Alias").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Interface, "Show").is_some(), "{es:?}");
    }

    #[test]
    fn instance_becomes_implements() {
        let es = entities(
            "class Show a where\n  showit :: a -> String\ndata T = A\ninstance Show T where\n  showit x = \"T\"\n",
        );
        assert!(
            find(&es, EntityKind::Implements, "Show").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn top_level_bind_is_variable() {
        let es = entities("answer :: Int\nanswer = 42\n");
        assert!(
            find(&es, EntityKind::Variable, "answer").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Literal, "42").is_some(), "{es:?}");
    }

    #[test]
    fn module_header_produces_exports() {
        let es = entities("module Foo (a, b) where\na :: Int\na = 1\nb :: Int\nb = 2\n");
        assert!(
            es.iter().any(|e| e.kind == EntityKind::Export),
            "expected exports: {es:?}"
        );
    }

    #[test]
    fn import_becomes_slash_path() {
        let es =
            entities("import Data.List (sort)\nimport qualified Data.Map as M\nx :: Int\nx = 1\n");
        assert!(
            find(&es, EntityKind::Import, "Data/List").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::Import, "Data/Map").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn apply_is_call_and_qualified_is_member_access() {
        let es = entities("h :: Int\nh = g x y\nu :: Int\nu = M.lookup k\n");
        assert!(find(&es, EntityKind::Call, "g").is_some(), "{es:?}");
        assert!(
            find(&es, EntityKind::MemberAccess, "lookup").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Call, "lookup").is_some(), "{es:?}");
    }

    #[test]
    fn error_is_throw_and_catch_is_catch() {
        let es = entities(
            "boom :: Int\nboom = error \"msg\"\nsafe :: IO ()\nsafe = catch action handler\n",
        );
        assert!(find(&es, EntityKind::Throw, "error").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Catch, "catch").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Call, "error").is_none(), "{es:?}");
        assert!(find(&es, EntityKind::Call, "catch").is_none(), "{es:?}");
    }

    #[test]
    fn control_flow_conditional_case_guards() {
        let es = entities(
            "classify :: Int -> String\nclassify n\n  | n < 0 = \"neg\"\n  | otherwise = branch\n  where branch = case n of\n                   0 -> \"zero\"\n                   _ -> if n > 10 then \"big\" else \"pos\"\n",
        );
        assert!(
            find(&es, EntityKind::ControlFlow, "guards").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::ControlFlow, "case").is_some(),
            "{es:?}"
        );
        assert!(
            find(&es, EntityKind::ControlFlow, "conditional").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn literals_all_kinds() {
        let es = entities("lits :: (Int, Double, String, Char)\nlits = (1, 2.5, \"hi\", 'c')\n");
        assert!(find(&es, EntityKind::Literal, "1").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Literal, "2.5").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Literal, "\"hi\"").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Literal, "'c'").is_some(), "{es:?}");
    }
}
