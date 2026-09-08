//! Entity/symbol extraction: an unconditional full-tree walk that maps
//! tree-sitter node kinds to the 14-kind entity checklist.
//!
//! This deliberately does not use `ast-grep-core`'s pattern engine — the hot
//! path is a whole-tree walk, not pattern matching.

pub mod entity;
pub mod langs;
pub mod minhash;
pub mod symbol;

use crate::model::{Entity, EntityKind, Symbol};
use crate::parse::ParsedFile;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// The extraction result for one parsed file: flat entity + symbol lists plus
/// a syntax-error flag folded into the single walk.
#[derive(Debug, Default)]
pub struct ExtractResult {
    pub entities: Vec<Entity>,
    pub symbols: Vec<Symbol>,
    /// True when the tree contains ERROR/MISSING nodes (syntax error).
    pub has_error: bool,
}

/// Extract entities and symbols from a parsed file in a single traversal.
///
/// The previous `entity::extract_entities` + `symbol::extract_symbols` ran two
/// independent full-tree walks; `has_error` (a `root.has_error()` pass at parse
/// time) was a third. This merges all three: one recursive descent that, per
/// node, runs the language entity visit, classifies identifier symbols, and
/// flags ERROR/MISSING nodes.
pub fn extract(parsed: &ParsedFile, file_id: u32) -> ExtractResult {
    let mut result = ExtractResult::default();
    let root = parsed.root.root();
    let mut enclosing: Vec<String> = Vec::new();
    let mut type_scope: Vec<String> = Vec::new();
    let mut ctx = WalkCtx {
        lang: parsed.lang,
        file_id,
        result: &mut result,
        enclosing: &mut enclosing,
        type_scope: &mut type_scope,
    };
    walk(&root, &mut ctx, false, false, 0);
    drop_blank_named(&mut result);
    result
}

/// Drop rows whose name failed to resolve to a real identifier (empty or
/// whitespace-only).
///
/// Blank names come from unnamed constructs and error recovery — Ruby
/// `class << self` singleton classes, Haskell type operators (`:>`), JS
/// `export default function(){}`, Lua table-literal method closures — where the
/// extractor's `field_name(node).unwrap_or_default()` yields `""`. As name-keyed
/// rows these are pure noise: they can never match a `get_symbol`/`dependencies`
/// lookup, they flood `symbols_in_file`, and they inflate scan counts (the audit
/// found 22 blank symbols in one Ruby file, 25 in one Haskell file).
///
/// Control-flow and error markers are exempt: a bare `rescue` (Catch) or a
/// re-`raise` (Throw) is a real, queryable point whose name is *legitimately*
/// absent, and error-handling scan rules rely on those markers existing.
fn drop_blank_named(result: &mut ExtractResult) {
    result
        .entities
        .retain(|e| kind_allows_blank_name(e.kind) || !e.name.trim().is_empty());
    result.symbols.retain(|s| !s.name.trim().is_empty());
}

/// True for the entity kinds whose name may legitimately be empty (control-flow
/// and error markers named after a caught/thrown value that need not exist).
fn kind_allows_blank_name(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::ControlFlow | EntityKind::Catch | EntityKind::Throw
    )
}

/// The stable-across-the-walk inputs threaded through every [`walk`] call:
/// language/file identity plus the mutable output and scope stacks. Bundled
/// so `walk`'s own per-recursion-step arguments (`node`, `in_type`,
/// `parent_is_type`, `depth`) aren't lost among them positionally.
struct WalkCtx<'a> {
    lang: SupportLang,
    file_id: u32,
    result: &'a mut ExtractResult,
    enclosing: &'a mut Vec<String>,
    type_scope: &'a mut Vec<String>,
}

/// Maximum tree depth the walk will descend before bailing. Guards against
/// stack overflow (an uncatchable process abort) on pathological or
/// machine-generated input whose AST nests thousands of levels deep — far
/// beyond any hand-written source. Exceeding it flags `has_error` so callers
/// see the file as incompletely processed rather than silently truncated.
pub(crate) const MAX_WALK_DEPTH: u32 = 2000;

/// The merged entity + symbol + has_error + type-context traversal (see
/// [`extract`]). `in_type` is the propagated "inside a type context" flag — an
/// O(1) per-node check replacing `in_type_context`'s per-call ancestor walk.
///
/// `in_type` replicates `in_type_context` exactly, which skips the immediate
/// parent (`.skip(1)`): it is "the grandparent-or-higher is a type kind".
/// `parent_is_type` carries the parent's own type-kind-ness one level down.
fn walk(
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    ctx: &mut WalkCtx<'_>,
    in_type: bool,
    parent_is_type: bool,
    depth: u32,
) {
    // Syntax-error flag: a single ERROR/MISSING node anywhere in the file.
    if node.is_error() || node.is_missing() {
        ctx.result.has_error = true;
    }

    // `node.kind()` is an FFI call returning a `Cow<str>`; the walk consulted it
    // ~4× per node (identifier check, function-scope test, type-scope test,
    // type-kind test). Materialize it once and reuse the borrow — the walk is
    // the dominant cold-build cost (55–63% of parse+extract; see
    // examples/parse_vs_walk), so shaving repeated per-node work compounds
    // across millions of nodes.
    let kind_cow = node.kind();
    let kind: &str = kind_cow.as_ref();

    // Entities: per-language visit threaded with the enclosing-scope stack.
    {
        let mut ectx = entity::ExtractCtx {
            lang: ctx.lang,
            file_id: ctx.file_id,
            out: &mut ctx.result.entities,
            enclosing: ctx.enclosing.last().map(|s| s.as_str()),
            type_scope: ctx.type_scope.last().map(|s| s.as_str()),
            in_type,
        };
        langs::visit(node, kind, &mut ectx);
    }

    // Symbols: identifier classification (Binding / Reference / neither). The
    // identifier-leaf gate is language-aware (e.g. PHP names variables
    // `variable_name`, not `identifier`) — see `symbol::is_symbol_ident`.
    if symbol::is_symbol_ident(ctx.lang, kind)
        && let Some((kind, name)) = symbol::classify(node, ctx.lang, in_type)
    {
        ctx.result.symbols.push(Symbol {
            kind,
            name: name.to_owned(),
            file_id: ctx.file_id,
            span: span_of(node),
        });
    }

    // Named functions/methods push onto the enclosing stack for children.
    let is_scope = langs::is_function_scope(ctx.lang, node, kind);
    if is_scope {
        ctx.enclosing
            .push(langs::function_scope_name(ctx.lang, node).unwrap_or_default());
    }
    // Named classes/interfaces/impl-blocks push onto the type-scope stack so
    // methods nested inside record their owning type (`Entity::owner_type`).
    let is_type_scope = langs::type_scopes(ctx.lang).contains(&kind);
    if is_type_scope {
        ctx.type_scope
            .push(langs::type_scope_name(ctx.lang, node).unwrap_or_default());
    }
    // Children are "in a type context" when the current node *or higher* is a
    // type kind (i.e. `in_type_context` of the child); the child's own parent
    // is the current node, so propagate `parent_is_type` as `is_type_kind(node)`.
    let node_is_type = is_type_kind(ctx.lang, kind);
    if depth >= MAX_WALK_DEPTH {
        // Bail before recursing deeper: mark the file as incompletely
        // processed rather than overflowing the stack. Scope pushes above are
        // still balanced by the pops below.
        ctx.result.has_error = true;
    } else {
        for child in node.children() {
            walk(
                &child,
                ctx,
                parent_is_type || in_type,
                node_is_type,
                depth + 1,
            );
        }
    }
    if is_type_scope {
        ctx.type_scope.pop();
    }
    if is_scope {
        ctx.enclosing.pop();
    }
}

/// Shared span builder used by all extractors.
pub fn span_of(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> crate::model::Span {
    let inner = node.get_inner_node();
    let range = node.range();
    let start = inner.start_position();
    let end = inner.end_position();
    crate::model::Span {
        start_byte: crate::model::saturating_u32(range.start),
        end_byte: crate::model::saturating_u32(range.end),
        start_line: crate::model::saturating_u32(start.row).saturating_add(1),
        start_col: crate::model::saturating_u32(start.column),
        end_line: crate::model::saturating_u32(end.row).saturating_add(1),
        end_col: crate::model::saturating_u32(end.column),
    }
}

/// The `name` field text of a node, if present.
pub fn field_name(node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) -> Option<String> {
    node.field("name").map(|n| n.text().into_owned())
}

/// Node kinds that mark the start of a type context (type annotation, generic
/// type arguments, union/intersection/array/function types, ...).
const TYPE_KINDS: &[&str] = &[
    "type_annotation",
    "return_type",
    "type_arguments",
    "type_parameters",
    "predefined_type",
    "literal_type",
    "union_type",
    "intersection_type",
    "array_type",
    "generic_type",
    "function_type",
    "tuple_type",
    "mapped_type",
    "nullable_type",
    "optional_type",
    "index_type_query",
    "type_parameter",
];

/// True when `kind` is a type-context node kind for `lang`.
///
/// Rust's grammar names type-context node kinds differently from the shared
/// `TYPE_KINDS` list above (which is TS/JS-centric but also matches several
/// other typed languages' kind names closely enough to work as-is) — dispatch
/// to `langs::rust::is_type_kind` for Rust rather than missing them (review
/// fix M4: the divergence let Rust identifiers in type position leak out as
/// spurious `Reference` symbols).
#[inline]
pub(crate) fn is_type_kind(lang: SupportLang, kind: &str) -> bool {
    match lang {
        SupportLang::Rust => langs::rust::is_type_kind(kind),
        _ => TYPE_KINDS.contains(&kind),
    }
}

#[cfg(test)]
mod type_context_tests {
    use super::*;

    /// Regression (review fix M4): the shared `TYPE_KINDS` list (TS/JS-centric)
    /// and Rust's own type-context node kinds diverged, so a path identifier
    /// inside a Rust type with no `generic_type`/`array_type`/... wrapper in
    /// its ancestor chain (e.g. `&std::string::String`'s `reference_type` ->
    /// `scoped_type_identifier` chain, neither of which the shared list knew
    /// about) never had `in_type` set and leaked out as a spurious `Reference`
    /// symbol. `is_type_kind` now dispatches to `langs::rust::is_type_kind`
    /// for Rust so this is caught.
    #[test]
    fn rust_path_identifiers_in_a_bare_reference_type_are_not_references() {
        let src = "fn f(y: &std::string::String) -> std::string::String {\n    y.clone()\n}\n";
        let parsed = crate::parse::parse_source(&SupportLang::Rust, src);
        let result = crate::extract::extract(&parsed, 0);
        let reference_names: Vec<&str> = result
            .symbols
            .iter()
            .filter(|s| s.kind == crate::model::SymbolKind::Reference)
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            !reference_names.contains(&"std") && !reference_names.contains(&"string"),
            "path segments of a Rust type must not leak as Reference symbols: {reference_names:?}"
        );
    }
}

#[cfg(test)]
mod blank_name_tests {
    use super::*;
    use crate::model::EntityKind;

    /// Regression (audit S8): unnamed declarations and error recovery produced
    /// blank-name (`""`) entities/symbols that flooded name-keyed query output
    /// (22 in one Ruby file, 25 in one Haskell file). `drop_blank_named` strips
    /// them; every remaining declaration/reference row must carry a real name.
    #[test]
    fn declaration_and_reference_rows_never_have_blank_names() {
        // A JS default-anonymous export historically emitted a blank Function.
        let cases = [
            (
                SupportLang::JavaScript,
                "export default function () { return 1; }\n",
            ),
            (
                SupportLang::Ruby,
                "class Foo\n  class << self\n    def bar; end\n  end\nend\n",
            ),
            (
                SupportLang::Haskell,
                "data (path :: k) :> (a :: Type)\nmain :: IO ()\nmain = pure ()\n",
            ),
        ];
        for (lang, src) in cases {
            let parsed = crate::parse::parse_source(&lang, src);
            let result = crate::extract::extract(&parsed, 0);
            for e in &result.entities {
                assert!(
                    kind_allows_blank_name(e.kind) || !e.name.trim().is_empty(),
                    "{lang:?}: blank-name {:?} entity survived the filter",
                    e.kind
                );
            }
            for s in &result.symbols {
                assert!(
                    !s.name.trim().is_empty(),
                    "{lang:?}: blank-name symbol survived the filter"
                );
            }
        }
    }

    /// The filter must NOT strip control-flow/error markers whose name is
    /// legitimately absent — a bare `rescue` is a real, queryable Catch point
    /// that error-handling scan rules depend on.
    #[test]
    fn bare_rescue_keeps_its_unnamed_catch_marker() {
        let src = "def risky\n  do_work\nrescue\n  nil\nend\n";
        let parsed = crate::parse::parse_source(&SupportLang::Ruby, src);
        let result = crate::extract::extract(&parsed, 0);
        assert!(
            result.entities.iter().any(|e| e.kind == EntityKind::Catch),
            "bare rescue must still emit a Catch marker even with a blank name"
        );
    }
}
