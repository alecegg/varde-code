//! TS control-flow/error-handling entity extraction tests
//! (task: ts-entity-extraction-control-error).
//! Kinds: catches, throws, control_flow; plus enclosing-function linkage.

mod common;

use ast_grep_language::SupportLang;
use varde_code::extract;
use varde_code::model::{Entity, EntityKind};
use varde_code::parse::parse_source;

fn control_entities() -> Vec<Entity> {
    let src = common::fixture("ts/control_error.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(!parsed.has_error(), "fixture must parse cleanly");
    extract::extract(&parsed, 0)
        .entities
        .into_iter()
        .filter(|e| {
            matches!(
                e.kind,
                EntityKind::Catch | EntityKind::Throw | EntityKind::ControlFlow
            )
        })
        .collect()
}

#[test]
fn extract_control_error_each_construct_has_correct_kind() {
    let entities = control_entities();
    // (kind, name) pairs the fixture must produce.
    let mut expected: Vec<(EntityKind, &str)> = vec![
        // error handling
        (EntityKind::Catch, "err"),
        (EntityKind::Throw, "new Error(\"empty\")"),
        // control flow (name = tree-sitter node kind)
        (EntityKind::ControlFlow, "try_statement"),
        (EntityKind::ControlFlow, "if_statement"),
        (EntityKind::ControlFlow, "for_in_statement"),
        (EntityKind::ControlFlow, "while_statement"),
        (EntityKind::ControlFlow, "return_statement"),
        (EntityKind::ControlFlow, "break_statement"), // inside while
        (EntityKind::ControlFlow, "switch_statement"),
        (EntityKind::ControlFlow, "break_statement"), // inside switch case
        (EntityKind::ControlFlow, "do_statement"),
    ];
    for entity in &entities {
        if let Some(pos) = expected
            .iter()
            .position(|(ek, en)| ek == &entity.kind && en == &entity.name)
        {
            expected.remove(pos);
        } else {
            panic!(
                "unexpected control/error entity: {:?} {:?}",
                entity.kind, entity.name
            );
        }
    }
    assert!(expected.is_empty(), "missing entities: {expected:?}");
}

#[test]
fn extract_control_error_enclosing_function_linkage() {
    let entities = control_entities();
    for e in &entities {
        // Entities inside `process` live on lines 2-16 of the fixture;
        // the switch/do/break constructs at the top level live on lines 19-25.
        let inside_process = e.span.start_line <= 16;
        if inside_process {
            assert_eq!(
                e.enclosing_function.as_deref(),
                Some("process"),
                "entity {:?} {:?} must link to enclosing function 'process' (span {:?})",
                e.kind,
                e.name,
                e.span
            );
        } else {
            assert_eq!(
                e.enclosing_function, None,
                "top-level entity {:?} {:?} must have no enclosing function",
                e.kind, e.name
            );
        }
    }
    // throw's enclosing linkage is asserted above; also verify a span exists.
    let throw = entities
        .iter()
        .find(|e| e.kind == EntityKind::Throw)
        .expect("throw entity present");
    assert!(throw.span.start_byte < throw.span.end_byte);
    assert_eq!(throw.span.start_line, 4);
}
