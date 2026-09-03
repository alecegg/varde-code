//! TS domain-specific entity extraction tests (task: ts-entity-extraction-domain-specific).
//! Kinds: routes, responses (Express-style call shapes).

mod common;

use ast_grep_language::SupportLang;
use varde_code::extract;
use varde_code::model::{Entity, EntityKind};
use varde_code::parse::parse_source;

fn domain_entities() -> Vec<Entity> {
    let src = common::fixture("ts/domain_specific.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(!parsed.has_error(), "fixture must parse cleanly");
    extract::extract(&parsed, 0)
        .entities
        .into_iter()
        .filter(|e| matches!(e.kind, EntityKind::Route | EntityKind::Response))
        .collect()
}

#[test]
fn extract_domain_specific_routes_have_method_and_path() {
    let entities = domain_entities();
    let routes: Vec<_> = entities
        .iter()
        .filter(|e| e.kind == EntityKind::Route)
        .map(|e| (e.method.as_deref(), e.path.as_deref()))
        .collect();

    let mut expected: Vec<(Option<&str>, Option<&str>)> = vec![
        (Some("get"), Some("/users")),
        (Some("post"), Some("/users")),
        (Some("get"), Some("/health")),
        (Some("use"), Some("/auth")),
        (Some("delete"), Some("/users/:id")),
    ];
    for got in routes {
        let pos = expected.iter().position(|e| *e == got);
        assert!(pos.is_some(), "unexpected route: {got:?}");
        expected.remove(pos.unwrap());
    }
    assert!(expected.is_empty(), "missing routes: {expected:?}");
}

#[test]
fn extract_domain_specific_responses_have_status_and_body_shape() {
    let entities = domain_entities();
    let responses: Vec<_> = entities
        .iter()
        .filter(|e| e.kind == EntityKind::Response)
        .map(|e| (e.status.clone(), e.body_shape.clone()))
        .collect();

    let mut expected: Vec<(Option<String>, Option<String>)> = vec![
        (Some("200".into()), Some("json".into())),
        (Some("201".into()), Some("json".into())),
        (None, Some("send".into())),
        (Some("404".into()), None), // sendStatus
    ];
    for got in responses {
        let pos = expected.iter().position(|e| *e == got);
        assert!(pos.is_some(), "unexpected response: {got:?}");
        expected.remove(pos.unwrap());
    }
    assert!(expected.is_empty(), "missing responses: {expected:?}");
}
