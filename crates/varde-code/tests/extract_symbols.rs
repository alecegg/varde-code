//! TS symbol extraction tests (task: ts-symbol-extraction).
//! Symbols are named bindings/references distinct from the Entity list.
//!
//! The later blocks cover the generalized, language-aware symbol path added for
//! the multi-language extractors (PHP/Ruby/C/C++) — see
//! `extract::symbol::classify`.

mod common;

use ast_grep_language::SupportLang;
use varde_code::extract;
use varde_code::model::{Symbol, SymbolKind};
use varde_code::parse::parse_source;

#[test]
fn extract_symbols_captures_bindings_and_references() {
    let src = common::fixture("ts/symbols.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    assert!(!parsed.has_error(), "fixture must parse cleanly");
    let result = extract::extract(&parsed, 0);

    // Bindings introduced by imports.
    let mut expected: Vec<(SymbolKind, &str)> = vec![
        (SymbolKind::Binding, "helper"),
        (SymbolKind::Binding, "Util"),
        (SymbolKind::Binding, "express"),
        // References: identifiers used in expressions (not entity names).
        (SymbolKind::Reference, "input"),
        (SymbolKind::Reference, "doubled"),
        (SymbolKind::Reference, "base"),
        (SymbolKind::Reference, "value"),
    ];

    for s in &result.symbols {
        if let Some(pos) = expected
            .iter()
            .position(|(ek, en)| ek == &s.kind && en == &s.name)
        {
            expected.remove(pos);
        } else {
            panic!(
                "unexpected symbol: {:?} {:?} (full: {:?})",
                s.kind, s.name, result.symbols
            );
        }
    }
    assert!(expected.is_empty(), "missing symbols: {expected:?}");
}

#[test]
fn extract_symbols_are_distinct_from_entities() {
    let src = common::fixture("ts/symbols.ts");
    let parsed = parse_source(&SupportLang::TypeScript, &src);
    let result = extract::extract(&parsed, 0);

    let mut symbols: Vec<&Symbol> = result.symbols.iter().collect();
    for entity in &result.entities {
        // No symbol may share both the same name and the same byte span as an
        // entity — the lists are non-redundant by construction.
        symbols.retain(|s| !(s.name == entity.name && s.span == entity.span));
    }
    assert_eq!(
        symbols.len(),
        result.symbols.len(),
        "symbol/entity overlap found"
    );
}

/// Regression: PHP names variables `variable_name` and identifiers `name`, never
/// `identifier`, so the old `identifier`-only gate produced ZERO symbols for
/// `.php` (empty `symbols_in_file`). The language-aware gate must now yield
/// references, and the `$x`/inner-`name` pair must not double-count.
#[test]
fn php_produces_reference_symbols_without_duplicates() {
    let src = "<?php\nfunction handle($input) {\n  $doubled = $input * 2;\n  return $doubled;\n}\n";
    let parsed = parse_source(&SupportLang::Php, src);
    assert!(!parsed.has_error(), "fixture must parse cleanly");
    let result = extract::extract(&parsed, 0);

    let refs: Vec<&str> = result
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Reference)
        .map(|s| s.name.as_str())
        .collect();
    assert!(!refs.is_empty(), "PHP should now produce symbols: {refs:?}");
    // Variable *uses* are references, spelled with the `$` (the `variable_name`).
    assert!(refs.contains(&"$input"), "expected $input use: {refs:?}");
    assert!(
        refs.contains(&"$doubled"),
        "expected $doubled use: {refs:?}"
    );
    // The inner bare `name` of a `$var` must not be emitted alongside it.
    assert!(
        !refs.contains(&"input") && !refs.contains(&"doubled"),
        "inner name of a variable_name double-counted: {refs:?}"
    );
    // The function *name* is an entity, never a reference symbol.
    assert!(
        !refs.contains(&"handle"),
        "function name leaked as a reference: {refs:?}"
    );
}

/// The generalized classifier must not double-count entity names as references:
/// a method name / callee is an Entity, not a Reference. (Ruby uses `identifier`
/// nodes but routes through the generic path.)
#[test]
fn ruby_entity_names_are_not_reference_symbols() {
    let src = "def greet(name)\n  puts name\nend\n";
    let parsed = parse_source(&SupportLang::Ruby, src);
    assert!(!parsed.has_error(), "fixture must parse cleanly");
    let result = extract::extract(&parsed, 0);

    let refs: Vec<&str> = result
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Reference)
        .map(|s| s.name.as_str())
        .collect();
    // `greet` is a Function entity; `puts` is a Call entity — neither is a ref.
    assert!(!refs.contains(&"greet"), "method name leaked: {refs:?}");
    assert!(!refs.contains(&"puts"), "callee leaked: {refs:?}");
    // `name` used in the body IS a reference.
    assert!(refs.contains(&"name"), "expected `name` use: {refs:?}");

    // Non-redundancy invariant: no symbol shares both name and span with an
    // entity (mirrors the TS assertion above, for the generic path).
    for s in &result.symbols {
        for e in &result.entities {
            assert!(
                !(s.name == e.name && s.span == e.span),
                "symbol/entity overlap: {:?} {:?}",
                s.kind,
                s.name
            );
        }
    }
}
