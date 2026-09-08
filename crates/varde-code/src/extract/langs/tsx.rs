//! TSX entity extraction.
//!
//! TSX uses the tree-sitter-typescript **tsx** grammar, whose node kinds for
//! the 14-kind entity checklist are identical to TypeScript's
//! (`function_declaration`, `class_declaration`, `interface_declaration`,
//! `call_expression`, `member_expression`, ...). The full TS implementation in
//! `super::ts` therefore covers TSX unchanged; this module delegates to it.
//!
//! JSX-only node kinds (`jsx_element`, `jsx_self_closing_element`,
//! `jsx_attribute`, `jsx_expression`, ...) match nothing in the TS visitor, so
//! JSX is naturally ignored — JSX attribute value strings and expression
//! literals inside `{...}` still surface as ordinary expression-level entities
//! (e.g. Literal, Call, MemberAccess) exactly as they would outside JSX.

use crate::extract::entity::ExtractCtx;
use crate::extract::langs::ts;
use crate::model::EntityKind;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Node kinds that introduce a named function scope (same set as TS).
pub const FUNCTION_SCOPES: &[&str] = ts::FUNCTION_SCOPES;

/// Node kinds that introduce a named type scope (same set as TS).
pub const TYPE_SCOPES: &[&str] = ts::TYPE_SCOPES;

/// Entity kinds the fixtures must produce (all 14, same as TS).
pub const REQUIRED_KINDS: [EntityKind; 14] = ts::REQUIRED_KINDS;

/// Emit entities for one node (called for every node in the tree).
///
/// Straight delegation: the TSX grammar shares every node kind the TS visitor
/// matches, and JSX-only kinds fall through the TS match arm producing nothing.
pub fn visit(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    kind: &str,
    ctx: &mut ExtractCtx,
) {
    ts::visit(node, kind, ctx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::model::Entity;
    use crate::parse::parse_source;

    /// Extends/Implements entity emission for `class_declaration` is wired
    /// entirely through `ts::visit` (see module docs above), so a `.tsx`
    /// file with the same heritage syntax as TS must resolve identically.
    #[test]
    fn extends_implements_entities_carry_raw_name_and_owner() {
        let src = "class Foo extends Base implements IFoo, IBar { }";
        let parsed = parse_source(&SupportLang::Tsx, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let extends: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Extends)
            .collect();
        assert_eq!(extends.len(), 1, "entities: {entities:?}");
        assert_eq!(extends[0].name, "Base");
        assert_eq!(extends[0].enclosing_function.as_deref(), Some("Foo"));

        let mut implements: Vec<&str> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Implements)
            .map(|e| e.name.as_str())
            .collect();
        implements.sort_unstable();
        assert_eq!(implements, vec!["IBar", "IFoo"]);
        assert!(
            entities
                .iter()
                .filter(|e| e.kind == EntityKind::Implements)
                .all(|e| e.enclosing_function.as_deref() == Some("Foo"))
        );
    }

    /// Decorator entity emission is also wired entirely through `ts::visit`.
    #[test]
    fn decorator_entity_carries_expression_text_and_owner() {
        let src = "@Component\nclass Foo {}";
        let parsed = parse_source(&SupportLang::Tsx, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        let decorators: Vec<&Entity> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Decorator)
            .collect();
        assert_eq!(decorators.len(), 1, "entities: {entities:?}");
        assert_eq!(decorators[0].name, "Component");
        assert_eq!(decorators[0].enclosing_function.as_deref(), Some("Foo"));
    }

    #[test]
    fn delegated_visitor_emits_generator_and_closure_entities() {
        let src = r#"
            function* entries() { yield load(); }
            function view() {
                return <button onClick={() => remote()}>Run</button>;
            }
        "#;
        let parsed = parse_source(&SupportLang::Tsx, src);
        assert!(!parsed.has_error(), "fixture must parse cleanly");
        let entities = extract::extract(&parsed, 0).entities;

        assert!(
            entities
                .iter()
                .any(|e| e.kind == EntityKind::Function && e.name == "entries"),
            "{entities:?}"
        );
        assert!(
            entities
                .iter()
                .any(|e| e.kind == EntityKind::CallableBoundary && e.name.is_empty()),
            "{entities:?}"
        );
    }
}
