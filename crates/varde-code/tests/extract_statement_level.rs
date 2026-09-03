//! TS statement-level entity extraction tests (task: ts-entity-extraction-statement-level).
//! Statement-level kinds: variables, parameters, exports.

mod common;

use ast_grep_language::SupportLang;
use varde_code::extract;
use varde_code::model::EntityKind;
use varde_code::parse::parse_source;

#[test]
fn extract_statement_level_produces_expected_kinds_and_names() {
    let src = common::fixture("ts/statement_level.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(!parsed.has_error(), "fixture must parse cleanly");
    let entities: Vec<_> = extract::extract(&parsed, 0)
        .entities
        .into_iter()
        .filter(|e| {
            matches!(
                e.kind,
                EntityKind::Variable
                    | EntityKind::Parameter
                    | EntityKind::Export
                    | EntityKind::Function
            )
        })
        .collect();

    // (kind, name) pairs the fixture must produce, each exactly once.
    let mut expected: Vec<(EntityKind, &str)> = vec![
        (EntityKind::Variable, "greeting"),
        (EntityKind::Variable, "counter"),
        (EntityKind::Variable, "legacy"),
        (EntityKind::Variable, "a"),
        (EntityKind::Variable, "b"),
        (EntityKind::Variable, "multiplier"),
        (EntityKind::Parameter, "x"),
        (EntityKind::Parameter, "y"),
        (EntityKind::Function, "add"),
        // export statements: `export function add`, `export const multiplier`,
        // and `export { counter, legacy }` (one Export per specifier).
        (EntityKind::Export, "add"),
        (EntityKind::Export, "multiplier"),
        (EntityKind::Export, "counter"),
        (EntityKind::Export, "legacy"),
    ];

    for entity in &entities {
        if let Some(pos) = expected
            .iter()
            .position(|(ek, en)| ek == &entity.kind && en == &entity.name)
        {
            expected.remove(pos);
        } else {
            panic!("unexpected entity: {:?} {:?}", entity.kind, entity.name);
        }
    }
    assert!(expected.is_empty(), "missing entities: {:?}", expected);
}
