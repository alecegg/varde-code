//! Scala entity extraction.
//!
//! Mapping notes (fixture-driven, superset-safe; node kinds from
//! tree-sitter-scala via ast-grep-language 0.45.1):
//! - `function_definition` (`def f = ...`) AND `function_declaration` (an
//!   abstract `def f: T` inside a trait) -> Function; name via field `name`.
//! - `class_definition` (covers plain `class` and `case class`) -> Class.
//! - `trait_definition` -> Interface. Traits are Scala's interface analog — the
//!   mixin/abstract-contract mechanism composed into classes with `with` — so
//!   they map to the closest kind (Interface) rather than being conflated with
//!   Class.
//! - `object_definition` -> Class. An `object` is a singleton type declaration;
//!   the nearest structural kind is Class (there is no dedicated singleton kind
//!   in the shared model).
//! - `extends_clause` -> the first supertype (the `type` field, i.e. everything
//!   before `with`) is Extends; each additional type after `with` is a mixin ->
//!   Implements, owned by the enclosing class/object. Scala doesn't distinguish
//!   "the superclass" from "the first mixed-in trait" syntactically, so this
//!   first-vs-rest split is the defensible approximation (mirrors how Kotlin's
//!   `Base(), IFoo` is split). Generic supertypes use `[...]` brackets in
//!   Scala (not `<...>`), so type arguments are stripped by `strip_type_args`
//!   below rather than the shared `<`-based `strip_generic_args`.
//! - `val_definition` / `var_definition` -> Variable, named after the bound
//!   `pattern` identifier (`val x = ...` -> "x"). Covers locals, object/class
//!   fields, and top-level vals uniformly.
//! - `parameter` (method parameter lists) and `class_parameter` (the
//!   constructor parameters of a `class`/`case class`) -> Parameter, via field
//!   `name`.
//! - `call_expression` -> Call, named after the callee (`function` field: a
//!   bare `identifier`, or the `field` of a `field_expression` receiver call).
//! - `field_expression` (`x.foo`) -> MemberAccess for the accessed member
//!   (`field` field). A receiver call `x.foo(...)` therefore emits both a
//!   MemberAccess ("foo") and a Call ("foo").
//! - `import_declaration` -> Import. The dotted path is recovered from node
//!   text and normalized to a slash-separated spec (a `{A, B}` selector group
//!   or a trailing `._` wildcard is stripped — neither names a single file) so
//!   resolve.rs's shared `/`-segment stem matching applies. NOTE: Scala imports
//!   are package-based and in general do NOT correspond to file stems (a
//!   package can span many files, and a file's path need not mirror its
//!   package), so most imports stay unresolved — the accepted approximation per
//!   the roadmap Risks section.
//! - Literals: integer/floating-point/string/boolean/null/character, excluding
//!   type contexts (`in_type`).
//! - `throw_expression` -> Throw, named after the thrown expression (the type
//!   of a `throw new E(...)`, else the thrown expression text).
//! - `catch_clause` -> Catch, named after the bound exception variable of its
//!   first `case` pattern (`catch { case e: E => ... }` -> "e").
//! - ControlFlow: `if_expression`, `while_expression`, `for_expression`,
//!   `match_expression`, `try_expression`, `return_expression`. (Scala has no
//!   `break`/`continue` keywords — loop control is library-based.)
//! - Export: carved out — Scala has no `export`-style visibility declaration in
//!   the sense the model means (Scala 2 has no export keyword at all; access is
//!   controlled by `private`/`protected` modifiers, not a public-export
//!   statement). No Export entity is emitted.
//! - Route/Response: carved out — Scala's web frameworks (Play, Akka HTTP,
//!   http4s, ...) each use a different, non-uniform routing DSL with no single
//!   idiomatic call shape worth hard-coding, so no narrow Route/Response
//!   heuristic is honest here (same stance as C).

use crate::extract::entity::ExtractCtx;
use crate::extract::field_name;
use crate::extract::langs::push_type_ref;
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope. `function_declaration` is
/// an abstract `def` (no body) inside a trait; `function_definition` has a body.
pub const FUNCTION_SCOPES: &[&str] = &["function_definition", "function_declaration"];

/// Node kinds that introduce a named class/trait/object type scope (for
/// `Entity::owner_type` linkage on methods and Extends/Implements ownership).
pub const TYPE_SCOPES: &[&str] = &["class_definition", "trait_definition", "object_definition"];

/// Entity kinds the fixtures must produce. Export is carved out (Scala has no
/// export declaration); Route/Response are carved out (no single idiomatic web
/// DSL). See module docs for the full rationale.
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
        "import_declaration" => {
            ctx.push(EntityKind::Import, import_path(node), node);
        }

        // ---- structural ----
        "function_definition" | "function_declaration" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Function, name, node);
        }
        "class_definition" | "object_definition" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Class, name.clone(), node);
            visit_supertypes(node, &name, ctx);
        }
        "trait_definition" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Interface, name.clone(), node);
            visit_supertypes(node, &name, ctx);
        }
        // `type Member = List[Int]` — a type member/alias; mapped to Interface
        // (type-level). Previously dropped (audit S3).
        "type_definition" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Interface, name, node);
        }
        // `given regOrd: Ordering[Int] = ...` — a Scala 3 given (implicit
        // instance); mapped to Variable (a named value). Anonymous givens have
        // no name field and are dropped by the blank-name filter. Previously
        // dropped entirely (audit S3).
        "given_definition" => {
            let name = field_name(node).unwrap_or_default();
            ctx.push(EntityKind::Variable, name, node);
        }

        // ---- variables ----
        "val_definition" | "var_definition" => {
            if let Some(pat) = node.field("pattern")
                && pat.kind() == "identifier"
            {
                let name = pat.text().into_owned();
                if name != "_" {
                    ctx.push(EntityKind::Variable, name, node);
                }
            }
        }

        // ---- parameters ----
        "parameter" | "class_parameter" => {
            if let Some(name) = field_name(node).filter(|n| n != "_") {
                ctx.push(EntityKind::Parameter, name, node);
            }
        }

        // ---- expression-level ----
        "call_expression" => {
            let name = call_name(node);
            ctx.push(EntityKind::Call, name, node);
        }
        "field_expression" => {
            if let Some(field) = node.field("field") {
                ctx.push(EntityKind::MemberAccess, field.text().into_owned(), node);
            }
        }
        "integer_literal"
        | "floating_point_literal"
        | "string"
        | "boolean_literal"
        | "null_literal"
        | "character_literal" => {
            if !ctx.in_type {
                ctx.push(EntityKind::Literal, node.text().into_owned(), node);
            }
        }

        // ---- error handling ----
        "throw_expression" => {
            ctx.push(EntityKind::Throw, thrown_name(node), node);
        }
        "catch_clause" => {
            ctx.push(EntityKind::Catch, catch_var(node), node);
        }

        // ---- control flow ----
        "if_expression" | "while_expression" | "for_expression" | "match_expression"
        | "try_expression" | "return_expression" => {
            ctx.push(EntityKind::ControlFlow, node.kind().into_owned(), node);
        }

        // ---- annotations ----
        // `@Controller` / `@cask.get("/x")` -> a Decorator entity owned by the
        // annotated definition. The grammar exposes the annotation type as a
        // `name` field (`type_identifier` for a bare name, or a dotted
        // `stable_type_identifier` like `cask.get`), so `field_name` recovers
        // it directly.
        "annotation" => {
            if let (Some(name), Some(owner)) = (field_name(node), annotation_owner_name(node)) {
                push_type_ref(ctx, EntityKind::Decorator, name, &owner, node);
            }
        }

        _ => {}
    }
}

/// Nearest enclosing definition's own name for an `annotation` node — the
/// class/object/trait/function this annotation is attached to.
fn annotation_owner_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    const OWNER_KINDS: &[&str] = &[
        "class_definition",
        "object_definition",
        "trait_definition",
        "function_definition",
        "function_declaration",
    ];
    node.ancestors()
        .find(|a| OWNER_KINDS.contains(&a.kind().as_ref()))
        .and_then(|a| field_name(&a))
}

/// Emit Extends (first supertype) + Implements (each mixin after `with`) for a
/// class/trait/object's `extends_clause`, owned by `owner`. The `extends_clause`
/// lists its supertypes as direct `type_identifier`/`generic_type` children in
/// source order: the first is the extended base, the rest are mixed-in traits.
fn visit_supertypes(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    owner: &str,
    ctx: &mut ExtractCtx,
) {
    let Some(clause) = node.children().find(|c| c.kind() == "extends_clause") else {
        return;
    };
    let mut supertypes = clause
        .children()
        .filter(|c| matches!(c.kind().as_ref(), "type_identifier" | "generic_type"));
    if let Some(base) = supertypes.next() {
        push_type_ref(ctx, EntityKind::Extends, type_name(&base), owner, &base);
    }
    for mixin in supertypes {
        push_type_ref(
            ctx,
            EntityKind::Implements,
            type_name(&mixin),
            owner,
            &mixin,
        );
    }
}

/// Bare name of a supertype node: a `generic_type` (`Base[Int]`) yields its
/// inner `type_identifier` (`Base`); a plain `type_identifier` yields itself.
/// Falls back to stripping `[...]` from the raw text for any other shape.
fn type_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    if node.kind() == "generic_type"
        && let Some(inner) = node.field("type")
    {
        return inner.text().into_owned();
    }
    strip_type_args(&node.text())
}

/// Strip Scala type arguments (`Base[Int]` -> `"Base"`). Scala uses square
/// brackets for generics, so the shared `<`-based `strip_generic_args` does not
/// apply.
fn strip_type_args(name: &str) -> String {
    match name.find('[') {
        Some(i) => name[..i].trim().to_string(),
        None => name.trim().to_string(),
    }
}

/// Callee name of a `call_expression`: the `function` field is a bare
/// `identifier` (`foo(...)`) or a `field_expression` receiver call
/// (`x.foo(...)`, whose `field` is the method name).
fn call_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let Some(func) = node.field("function") else {
        return String::new();
    };
    if func.kind() == "field_expression"
        && let Some(field) = func.field("field")
    {
        return field.text().into_owned();
    }
    func.text().into_owned()
}

/// Name of a thrown expression: `throw new E(...)` -> "E" (the constructed
/// type), otherwise the thrown expression's text.
fn thrown_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let Some(inner) = node.children().find(|c| c.is_named()) else {
        return String::new();
    };
    if inner.kind() == "instance_expression"
        && let Some(ty) = inner
            .children()
            .find(|c| matches!(c.kind().as_ref(), "type_identifier" | "generic_type"))
    {
        return type_name(&ty);
    }
    inner.text().into_owned()
}

/// Exception variable bound by a `catch_clause`'s first `case` pattern
/// (`catch { case e: E => ... }` -> "e"), falling back to the empty string.
fn catch_var(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let Some(block) = node.children().find(|c| c.kind() == "case_block") else {
        return String::new();
    };
    let Some(case) = block.children().find(|c| c.kind() == "case_clause") else {
        return String::new();
    };
    let Some(pat) = case.field("pattern") else {
        return String::new();
    };
    // `case e: E` parses as a `typed_pattern` whose `pattern` field is the
    // bound identifier; `case e` is a bare identifier.
    if pat.kind() == "typed_pattern" {
        return pat
            .field("pattern")
            .map(|p| p.text().into_owned())
            .unwrap_or_default();
    }
    if pat.kind() == "identifier" {
        return pat.text().into_owned();
    }
    String::new()
}

/// Slash-separated import path from an `import_declaration` node's raw text. A
/// `{A, B}` selector group and a trailing `._` wildcard are stripped (neither
/// names a single file). Normalized to `/`-separated so resolve.rs's shared
/// segment stem matching applies unchanged.
fn import_path(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> String {
    let text = node.text();
    let t = text.trim().trim_start_matches("import").trim();
    // Drop a `{...}` selector group (`import a.b.{X, Y}` -> `a.b`).
    let t = match t.find('{') {
        Some(i) => t[..i].trim_end_matches('.').trim(),
        None => t,
    };
    // Drop a wildcard tail (`import a.b._` / `import a.b.*`).
    let t = t.trim_end_matches("._").trim_end_matches(".*");
    t.replace('.', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::model::Entity;
    use crate::parse::parse_source;

    fn entities(src: &str) -> Vec<Entity> {
        let parsed = parse_source(&SupportLang::Scala, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        extract::extract(&parsed, 0).entities
    }

    fn find<'a>(es: &'a [Entity], kind: EntityKind, name: &str) -> Option<&'a Entity> {
        es.iter().find(|e| e.kind == kind && e.name == name)
    }

    #[test]
    fn given_and_type_definitions_are_captured() {
        // Audit S3: Scala 3 `given` and `type` members were dropped.
        let es = entities("given regOrd: Ordering[Int] = ???\ntype Member = List[Int]\n");
        assert!(
            find(&es, EntityKind::Variable, "regOrd").is_some(),
            "given: {es:?}"
        );
        assert!(
            find(&es, EntityKind::Interface, "Member").is_some(),
            "type: {es:?}"
        );
    }

    #[test]
    fn def_class_trait_object_map_to_expected_kinds() {
        let es = entities(
            "trait T { def g(): Int }\nobject O { def f(): Int = 1 }\nclass C\ncase class P(x: Int)\n",
        );
        assert!(find(&es, EntityKind::Interface, "T").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Function, "g").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Class, "O").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Function, "f").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Class, "C").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Class, "P").is_some(), "{es:?}");
    }

    #[test]
    fn extends_and_with_split_into_extends_and_implements() {
        let es = entities("class Service extends Base with Logging with Tracing\n");
        let ext = find(&es, EntityKind::Extends, "Base").expect("Extends Base");
        assert_eq!(ext.enclosing_function.as_deref(), Some("Service"));
        let log = find(&es, EntityKind::Implements, "Logging").expect("Implements Logging");
        assert_eq!(log.enclosing_function.as_deref(), Some("Service"));
        assert!(
            find(&es, EntityKind::Implements, "Tracing").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn generic_supertypes_strip_bracket_type_args() {
        let es = entities("class Gen extends Base[Int] with Trait[String]\n");
        assert!(find(&es, EntityKind::Extends, "Base").is_some(), "{es:?}");
        assert!(
            find(&es, EntityKind::Implements, "Trait").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn annotations_emit_decorator_entities_named_and_owned() {
        // A bare class annotation and a dotted call annotation on a method
        // (cask-style `@cask.get`) each yield a Decorator owned by the
        // annotated definition; the dotted form keeps its full `a.b` name so a
        // `*.get` suffix role-tag rule can match it.
        let es =
            entities("@Controller\nclass Foo {\n  @cask.get(\"/y\")\n  def bar(): Int = 1\n}\n");
        let ctrl = find(&es, EntityKind::Decorator, "Controller").expect("@Controller decorator");
        assert_eq!(ctrl.enclosing_function.as_deref(), Some("Foo"));
        let get = find(&es, EntityKind::Decorator, "cask.get").expect("@cask.get decorator");
        assert_eq!(get.enclosing_function.as_deref(), Some("bar"));
    }

    #[test]
    fn val_var_and_params_map_to_variable_and_parameter() {
        let es = entities(
            "object O {\n  val x = 1\n  var y = 2\n  def f(a: Int, b: String): Int = a\n}\n",
        );
        assert!(find(&es, EntityKind::Variable, "x").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Variable, "y").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Parameter, "a").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Parameter, "b").is_some(), "{es:?}");
    }

    #[test]
    fn receiver_call_emits_member_access_and_call() {
        let es = entities("object O {\n  def f(): Unit = repo.save()\n}\n");
        assert!(
            find(&es, EntityKind::MemberAccess, "save").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Call, "save").is_some(), "{es:?}");
    }

    #[test]
    fn throw_and_catch_are_extracted() {
        let es = entities(
            "object O {\n  def f(): Unit = {\n    try { throw new RuntimeException(\"boom\") }\n    catch { case e: Exception => println(e) }\n  }\n}\n",
        );
        assert!(
            find(&es, EntityKind::Throw, "RuntimeException").is_some(),
            "{es:?}"
        );
        assert!(find(&es, EntityKind::Catch, "e").is_some(), "{es:?}");
        assert!(
            find(&es, EntityKind::ControlFlow, "try_expression").is_some(),
            "{es:?}"
        );
    }

    #[test]
    fn import_becomes_slash_path_not_a_call() {
        let es = entities("import com.example.Helper\n");
        assert!(
            find(&es, EntityKind::Import, "com/example/Helper").is_some(),
            "{es:?}"
        );
        let es2 = entities("import com.example.{A, B}\n");
        assert!(
            find(&es2, EntityKind::Import, "com/example").is_some(),
            "{es2:?}"
        );
    }

    #[test]
    fn literals_are_extracted() {
        let es = entities("object O {\n  val a = 42\n  val b = \"hi\"\n  val c = true\n}\n");
        assert!(find(&es, EntityKind::Literal, "42").is_some(), "{es:?}");
        assert!(find(&es, EntityKind::Literal, "true").is_some(), "{es:?}");
    }
}
