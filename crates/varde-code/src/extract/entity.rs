//! Tree walker driving per-language entity extraction.
//!
//! Walking strategy: unconditional recursive descent over the whole tree,
//! emitting one `Entity` per matched construct, threaded with the enclosing
//! named function/method stack so control-flow/error entities can record their
//! `enclosing_function` linkage. Per-language node-kind mapping lives in
//! `super::langs`.

use crate::extract::span_of;
use crate::model::{Entity, EntityKind};
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_language::SupportLang;

/// Context handed to per-language visitors.
pub struct ExtractCtx<'a> {
    pub lang: SupportLang,
    pub file_id: u32,
    pub out: &'a mut Vec<Entity>,
    /// Name of the nearest enclosing named function/method, if any (borrowed).
    pub enclosing: Option<&'a str>,
    /// Name of the nearest enclosing class/interface/impl-target type, if
    /// any (borrowed). Set on every entity as `owner_type` so
    /// class-membership queries don't need containment/span math.
    pub type_scope: Option<&'a str>,
    /// True when this node sits inside a type context (see `crate::extract`).
    pub in_type: bool,
}

impl<'a> ExtractCtx<'a> {
    /// Emit an anonymous callable span for containment-based rules.
    ///
    /// Boundaries intentionally have no name. They partition their enclosing
    /// named declaration but must never produce an independent finding.
    pub fn push_callable_boundary(&mut self, node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>) {
        self.push(EntityKind::CallableBoundary, String::new(), node);
    }

    /// Build and push an entity for the current file/enclosing context.
    ///
    /// Collapses the repeated `ctx.out.push(entity(kind, name, ctx.file,
    /// node, ctx.enclosing.clone()))` shape used at every language
    /// extractor's match arm.
    pub fn push(
        &mut self,
        kind: EntityKind,
        name: String,
        node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    ) {
        // Catch-all for C/C++: a reserved keyword surfacing as an entity name is
        // always an error-recovery/preprocessor artifact (e.g. a scoped
        // `enum class` mis-parsed so the `class` token becomes the type name).
        // The per-arm guards in c.rs/cpp.rs cover the common paths; this backstops
        // the rest, since a keyword can never be a real identifier. Control-flow
        // entities are named after their node kind (`if_statement`, not `if`), so
        // they are unaffected.
        if matches!(self.lang, SupportLang::C | SupportLang::Cpp)
            && super::langs::c::is_reserved_keyword(&name)
        {
            return;
        }
        // Set on every entity kind (not just `Function`), so control-flow
        // entities inherit the same owner-type tag as their enclosing
        // method. `function-complexity-hotspot` uses this to disambiguate
        // same-named methods across different classes in one file (see
        // complexity.toml) — without it, two classes each defining a
        // `send` method would share one bucket keyed on the bare name.
        // Function entities additionally capture their body's per-band
        // MinHash signature for clone-band detection.
        let owner_type = self.type_scope.map(|s| s.to_owned());
        let is_function = kind == EntityKind::Function;
        let body_minhash = is_function
            .then(|| crate::extract::minhash::body_minhash(node.text().as_ref()))
            .flatten();
        self.out.push(Entity {
            kind,
            name,
            file_id: self.file_id,
            span: span_of(node),
            enclosing_function: self.enclosing.map(|s| s.to_owned()),
            method: None,
            path: None,
            status: None,
            body_shape: None,
            body_minhash,
            is_async: is_function.then(|| super::langs::node_is_async(node)),
            is_test: is_function && super::langs::node_is_test(node),
            owner_type,
        });
    }
}

/// Kind-specific fields for [`entity`], bundled into one argument so call
/// sites across the language extractors (which vary in which of these they
/// set) don't carry four trailing positional `Option`/`bool` params that are
/// easy to misorder. Defaults match the common case (a plain entity with no
/// enclosing/owner context).
#[derive(Default)]
pub struct EntityMeta {
    pub enclosing: Option<String>,
    pub is_async: Option<bool>,
    pub is_test: bool,
    pub owner_type: Option<String>,
}

/// Shared entity constructor for language extractors that build an `Entity`
/// outside the `ExtractCtx::push` path (e.g. import/export entities computed
/// before an enclosing context exists).
///
/// Function entities additionally capture their body's per-band MinHash
/// signature for clone-band detection.
pub fn entity(
    kind: EntityKind,
    name: String,
    file_id: u32,
    node: &ast_grep_core::Node<'_, StrDoc<SupportLang>>,
    meta: EntityMeta,
) -> Entity {
    let body_minhash = (kind == EntityKind::Function)
        .then(|| crate::extract::minhash::body_minhash(node.text().as_ref()))
        .flatten();
    Entity {
        kind,
        name,
        file_id,
        span: span_of(node),
        enclosing_function: meta.enclosing,
        method: None,
        path: None,
        status: None,
        body_shape: None,
        body_minhash,
        is_async: meta.is_async,
        is_test: meta.is_test,
        owner_type: meta.owner_type,
    }
}
