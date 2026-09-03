//! TS expression-level entity extraction tests (task: ts-entity-extraction-expression-level).
//! Expression-level kinds: calls, literals, member_accesses.

mod common;

use ast_grep_language::SupportLang;
use varde_code::extract;
use varde_code::model::EntityKind;
use varde_code::parse::parse_source;

/// Entities of the three expression-level kinds produced by the fixture.
fn expression_entities() -> Vec<(EntityKind, String)> {
    let src = common::fixture("ts/expression_level.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(!parsed.has_error(), "fixture must parse cleanly");
    extract::extract(&parsed, 0)
        .entities
        .into_iter()
        .filter(|e| {
            matches!(
                e.kind,
                EntityKind::Call | EntityKind::Literal | EntityKind::MemberAccess
            )
        })
        .map(|e| (e.kind, e.name))
        .collect()
}

#[test]
fn extract_expression_level_one_record_per_occurrence() {
    let mut got = expression_entities();
    let expected: Vec<(EntityKind, &str)> = vec![
        (EntityKind::Call, "add"),
        (EntityKind::Call, "config.resolve"),
        (EntityKind::MemberAccess, "resolve"),
        (EntityKind::MemberAccess, "length"),
        (EntityKind::Literal, "8080"),
        (EntityKind::Literal, "\"localhost\""),
        (EntityKind::Literal, "10"),
        (EntityKind::Literal, "true"),
        (EntityKind::Literal, "null"),
        (EntityKind::Literal, "`Port ${port}`"),
    ];
    for (ek, en) in expected {
        let pos = got
            .iter()
            .position(|(gk, gn)| gk == &ek && gn == en)
            .unwrap_or_else(|| panic!("missing {ek:?} {en:?} in {got:?}"));
        got.remove(pos);
    }
    assert!(
        got.is_empty(),
        "unexpected extra expression entities: {got:?}"
    );
}
