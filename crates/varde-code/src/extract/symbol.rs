//! Symbol extraction: named bindings and references.
//!
//! Entity/Symbol boundary rule (documented in `crate::model`): an `Entity`
//! carries the primary name of a structural construct (a function, class,
//! variable, call, ...). A `Symbol` is a named binding or reference that is
//! *not* an Entity's primary name:
//!
//! - `Binding`: a name introduced by an import (`import { helper } from ...`).
//! - `Reference`: an identifier used in an expression that is not consumed as
//!   an entity name (e.g. `input` in `input * 2`, `config` in `config.foo`).
//!
//! The two lists are non-redundant: identifiers in "name positions" (declared
//! names, callee identifiers, member properties, parameter patterns) belong to
//! the Entity and are never duplicated as Symbols.

use crate::extract::{is_type_kind, span_of};
use crate::model::{Symbol, SymbolKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Recursively walk the tree and collect symbols, returning `true` when the
/// tree contains an ERROR/MISSING node (matching the merged walk's `has_error`).
///
/// This is the symbol-only counterpart to [`crate::extract::extract`] — used by
/// query paths (e.g. `detect_changes`) that need symbol classification without
/// paying for the entity walk. `lang` selects the right type-context node-kind
/// list (see `crate::extract::is_type_kind`) — without it this walk would
/// always use the TS/JS-centric list regardless of the file's actual language.
pub fn extract_symbols(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    lang: SupportLang,
    file_id: u32,
    out: &mut Vec<Symbol>,
) -> bool {
    let mut has_error = false;
    let mut ctx = SymbolWalkCtx {
        lang,
        file_id,
        out,
        has_error: &mut has_error,
    };
    extract_symbols_inner(node, &mut ctx, false, false, 0);
    has_error
}

/// The stable-across-the-walk inputs threaded through every
/// [`extract_symbols_inner`] call — mirrors `crate::extract::WalkCtx`.
struct SymbolWalkCtx<'a> {
    lang: SupportLang,
    file_id: u32,
    out: &'a mut Vec<Symbol>,
    has_error: &'a mut bool,
}

fn extract_symbols_inner(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    ctx: &mut SymbolWalkCtx<'_>,
    in_type: bool,
    parent_is_type: bool,
    depth: u32,
) {
    // `in_type` replicates `in_type_context` (grandparent-or-higher); see
    // `crate::extract::walk` for the two-flag propagation rationale.
    if node.is_error() || node.is_missing() {
        *ctx.has_error = true;
    }
    if is_symbol_ident(ctx.lang, node.kind().as_ref())
        && let Some((kind, name)) = classify(node, ctx.lang, in_type)
    {
        ctx.out.push(Symbol {
            kind,
            name: name.to_owned(),
            file_id: ctx.file_id,
            span: span_of(node),
        });
    }
    let node_is_type = is_type_kind(ctx.lang, node.kind().as_ref());
    // Depth cap mirrors `crate::extract::walk`: bail before overflowing the
    // stack on pathologically deep trees, flagging the file as incomplete.
    if depth >= crate::extract::MAX_WALK_DEPTH {
        *ctx.has_error = true;
        return;
    }
    for child in node.children() {
        extract_symbols_inner(
            &child,
            ctx,
            parent_is_type || in_type,
            node_is_type,
            depth + 1,
        );
    }
}

/// Node kinds that are "identifier leaves" — candidates for symbol
/// classification — for a given language.
///
/// The original ten extraction languages classify only `identifier` nodes (the
/// JS/TS-tuned path in [`classify_jsts`]); this preserves their long-standing
/// behavior exactly. The languages added later carry their own gate: PHP names
/// identifiers `name` and variables `variable_name` and never emits an
/// `identifier` node, so gating on `identifier` alone produced **zero** symbols
/// for `.php` files (empty `symbols_in_file`/`get_symbol`/`filter_symbols`).
/// Ruby/C/C++ do use `identifier`, but route through the generalized classifier
/// so their entity names aren't double-counted as references.
pub(crate) fn is_symbol_ident(lang: SupportLang, kind: &str) -> bool {
    match lang {
        SupportLang::Php => matches!(kind, "name" | "variable_name"),
        // Haskell's expression identifier leaf is `variable` (there is no
        // `identifier` node); type/constructor leaves (`name`/`constructor`)
        // are declared-name or type-context positions handled by the generic
        // classifier's name-field/`in_type` gate, so `variable` alone yields
        // the reference set without leaking declared/callee names.
        SupportLang::Haskell => kind == "variable",
        // Bash's identifier leaf is `variable_name`, used both for assignment
        // targets (`x=1`) and variable expansions (`$x`, `${x}`). There is no
        // `identifier` node. Command names/args are `word` leaves under
        // `command_name`, deliberately excluded so calls don't leak as
        // references. The assignment-target `variable_name` is the `name` field
        // of `variable_assignment`, so the generic classifier's name-field gate
        // drops it, leaving only expansions as References.
        SupportLang::Bash => kind == "variable_name",
        SupportLang::Ruby
        | SupportLang::C
        | SupportLang::Cpp
        | SupportLang::Scala
        | SupportLang::Dart
        | SupportLang::Lua
        | SupportLang::Elixir
        | SupportLang::Solidity => kind == "identifier",
        _ => kind == "identifier",
    }
}

/// Classify an identifier node as a Binding, a Reference, or neither.
///
/// Returns the identifier text borrowed from the tree (no allocation); the
/// caller clones it only on the accepted path. `in_type` is the propagated
/// "inside a type context" flag (see `crate::extract::is_type_kind`).
///
/// Dispatches on language: the original ten keep the JS/TS-tuned classifier
/// ([`classify_jsts`]) verbatim, while the later additions use a grammar-
/// agnostic, field-driven classifier ([`classify_generic`]) — the symbol layer
/// was previously JS/TS-shaped and did not generalize (see the roadmap's
/// cross-cutting note).
pub(crate) fn classify<'r>(
    node: &ast_grep_core::Node<'r, StrDoc<SupportLang>>,
    lang: SupportLang,
    in_type: bool,
) -> Option<(SymbolKind, &'r str)> {
    match lang {
        SupportLang::Php
        | SupportLang::Ruby
        | SupportLang::C
        | SupportLang::Cpp
        | SupportLang::Scala
        | SupportLang::Dart
        | SupportLang::Lua
        | SupportLang::Elixir
        | SupportLang::Solidity
        | SupportLang::Bash
        | SupportLang::Haskell => classify_generic(node, in_type),
        _ => classify_jsts(node, in_type),
    }
}

/// Grammar-agnostic classifier for languages without a bespoke path.
///
/// Emits only `Reference`s: an identifier leaf that is a *use*, not an entity's
/// primary name. An identifier is excluded (not a symbol) when it occupies its
/// parent's declared-name / callee / accessed-member / declarator field, when
/// it is a parameter name, or when it sits in a type context. Import bindings
/// are already captured as `Import` entities, so no `Binding` is emitted here.
///
/// Assignment *targets* (`$x = 1`, `x = 1`) are intentionally left as
/// references — in the dynamic languages this path serves they are also the
/// `Variable` entity, but the entity spans the whole statement while the
/// reference spans just the identifier, so the two never share a (name, span)
/// and the non-redundancy invariant holds. This matches Python's existing
/// behavior on the JS/TS path.
fn classify_generic<'r>(
    node: &ast_grep_core::Node<'r, StrDoc<SupportLang>>,
    in_type: bool,
) -> Option<(SymbolKind, &'r str)> {
    let parent = node.parent()?;
    let std::borrow::Cow::Borrowed(name) = node.text() else {
        return None;
    };
    // PHP `$x` is a `variable_name` wrapping a `name` leaf; both pass the gate.
    // Emit the `variable_name` (the idiomatic `$x`) and drop its inner `name`
    // so the reference isn't counted twice.
    if node.kind() == "name" && parent.kind() == "variable_name" {
        return None;
    }
    if generic_name_position(node, &parent) || in_type {
        return None;
    }
    Some((SymbolKind::Reference, name))
}

/// True when `node` is the primary-name position of its parent under the
/// generic (field-driven) model: the declared name, callee, accessed member,
/// or declarator, or a parameter name.
fn generic_name_position(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    parent: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> bool {
    // A declared/callee/member name occupies one of these fields across the
    // grammars this path serves: `name` (functions/classes/methods/PHP member
    // calls), `function`/`method` (call callees), `property`/`field` (member
    // access), `constructor`, and `declarator` (the C/C++ declarator chain,
    // whose leaf identifier is the declared function/variable name).
    const NAME_FIELDS: &[&str] = &[
        "name",
        "function",
        "method",
        "property",
        "field",
        "constructor",
        "declarator",
    ];
    for field in NAME_FIELDS {
        if parent
            .field(field)
            .is_some_and(|n| n.node_id() == node.node_id())
        {
            return true;
        }
    }
    // Parameter names: an identifier directly under a parameter node (Ruby
    // `method_parameters`/`block_parameters`, PHP `simple_parameter`, …). C/C++
    // parameter names are reached via the `declarator` field above instead.
    parent.kind().as_ref().contains("parameter")
}

/// Classify an identifier under the JS/TS-tuned rules used by the original ten
/// extraction languages. Behavior is unchanged from before the multi-language
/// symbol work — kept verbatim to avoid regressing those languages.
fn classify_jsts<'r>(
    node: &ast_grep_core::Node<'r, StrDoc<SupportLang>>,
    in_type: bool,
) -> Option<(SymbolKind, &'r str)> {
    let parent = node.parent()?;
    // ast-grep's `Node::text()` is documented to always borrow from the
    // source for a real (non-synthetic) node; an owned `Cow` would mean that
    // invariant broke upstream. Rather than bake that third-party guarantee
    // into a panic, treat it as "not classifiable" — matches every other
    // early-return in this function.
    let std::borrow::Cow::Borrowed(name) = node.text() else {
        return None;
    };

    // Bindings introduced by imports.
    if parent.kind() == "import_specifier"
        && parent
            .field("name")
            .is_some_and(|n| n.node_id() == node.node_id())
    {
        return Some((SymbolKind::Binding, name));
    }
    // Default import: `import express from ...` — the identifier is an
    // unnamed child of import_clause.
    if parent.kind() == "import_clause" {
        return Some((SymbolKind::Binding, name));
    }
    // Namespace import: `import * as ns from ...` — alias field.
    if parent.kind() == "namespace_import"
        && parent
            .field("alias")
            .is_some_and(|n| n.node_id() == node.node_id())
    {
        return Some((SymbolKind::Binding, name));
    }

    // Identifiers consumed as an entity's primary name are not symbols.
    if is_entity_name_position(node, &parent) {
        return None;
    }
    // Identifiers in type contexts are types, not references.
    if in_type {
        return None;
    }

    Some((SymbolKind::Reference, name))
}

/// True when this identifier is the primary-name position of an Entity
/// (declared name, callee, member property, parameter pattern, ...).
fn is_entity_name_position(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    parent: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
) -> bool {
    let is_self = |n: Option<ast_grep_core::Node<'_, StrDoc<SupportLang>>>| {
        n.is_some_and(|n| n.node_id() == node.node_id())
    };
    match parent.kind().as_ref() {
        "variable_declarator"
        | "function_declaration"
        | "class_declaration"
        | "interface_declaration"
        | "method_definition"
        | "function_expression"
        | "generator_function_declaration"
        | "export_specifier" => is_self(parent.field("name")),
        "required_parameter" | "optional_parameter" => is_self(parent.field("pattern")),
        "call_expression" => is_self(parent.field("function")),
        "member_expression" => is_self(parent.field("property")),
        _ => false,
    }
}
