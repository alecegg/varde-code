//! TS structural entity extraction tests (task: ts-entity-extraction-structural).
//! Structural kinds: functions, classes, interfaces.

mod common;

use ast_grep_language::SupportLang;
use varde_code::extract;
use varde_code::model::EntityKind;
use varde_code::parse::parse_source;

fn extracted() -> Vec<varde_code::model::Entity> {
    let src = common::fixture("ts/structural.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(!parsed.has_error(), "fixture must parse cleanly");
    extract::extract(&parsed, 0)
        .entities
        .into_iter()
        .filter(|e| {
            matches!(
                e.kind,
                EntityKind::Function | EntityKind::Class | EntityKind::Interface
            )
        })
        .collect()
}

#[test]
fn extract_structural_one_entity_per_declaration() {
    let entities = extracted();
    assert_eq!(
        entities.len(),
        4,
        "one entity per declaration (function, class, class method, interface); \
         got: {entities:?}"
    );
}

#[test]
fn extract_structural_entity_fields_match_fixture() {
    let src = common::fixture("ts/structural.ts");
    let entities = extracted();

    // Function greet — declared on line 1, starts at column 0.
    let f = &entities[0];
    assert_eq!(f.kind, EntityKind::Function);
    assert_eq!(f.name, "greet");
    assert_eq!(f.file_id, 0);
    assert_eq!(f.span.start_line, 1);
    assert_eq!(f.span.start_col, 0);
    assert_eq!(f.span.end_line, 3);
    assert_eq!(f.owner_type, None);
    assert_span_covers(&src, f.span.start_byte, f.span.end_byte, "function");

    // Class Greeter — declared on line 5.
    let c = &entities[1];
    assert_eq!(c.kind, EntityKind::Class);
    assert_eq!(c.name, "Greeter");
    assert_eq!(c.span.start_line, 5);
    assert_eq!(c.span.start_col, 0);
    assert_eq!(c.span.end_line, 11);
    assert_span_covers(&src, c.span.start_byte, c.span.end_byte, "class");

    // Greeter's constructor — a method_definition inside the class body,
    // linked back to its owning class via owner_type.
    let m = &entities[2];
    assert_eq!(m.kind, EntityKind::Function);
    assert_eq!(m.name, "constructor");
    assert_eq!(m.owner_type.as_deref(), Some("Greeter"));

    // Interface Point — declared on line 13.
    let i = &entities[3];
    assert_eq!(i.kind, EntityKind::Interface);
    assert_eq!(i.name, "Point");
    assert_eq!(i.span.start_line, 13);
    assert_eq!(i.span.start_col, 0);
    assert_eq!(i.span.end_line, 16);
    assert_span_covers(&src, i.span.start_byte, i.span.end_byte, "interface");
}

/// The byte span must point exactly at the declaration node: starts with the
/// keyword and ends with the closing brace.
fn assert_span_covers(src: &str, start: u32, end: u32, keyword: &str) {
    let text = &src[start as usize..end as usize];
    assert!(
        text.starts_with(keyword),
        "span must start with '{keyword}', got {text:?}"
    );
    assert!(
        text.trim_end().ends_with('}'),
        "span must end with '}}', got {text:?}"
    );
}
