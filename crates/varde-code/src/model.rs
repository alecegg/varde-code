//! In-memory representation extracted from source files: entities, symbols,
//! and per-file diagnostics. This is the phase's in-process handoff structure
//! (flat Vec per file) consumed by later phases (resolution, persistence, query).

use serde::{Deserialize, Serialize};

/// One JSON document emitted by `varde-code extract <path>`.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractOutput {
    pub entities: Vec<Entity>,
    pub symbols: Vec<Symbol>,
    pub diagnostics: Vec<Diagnostic>,
    /// Interned file paths: every `file_id` above is an index into this
    /// table, assigned once at scan time instead of re-allocating the same
    /// path string once per entity/symbol.
    pub files: Vec<String>,
    /// Scan-time metadata parallel to [`ExtractOutput::files`] (index-aligned):
    /// `mtime`/`size`/`content_hash` computed at scan time so persistence can
    /// write `files` rows without re-statting or re-reading the file bytes.
    #[serde(skip)]
    pub file_meta: Vec<FileMeta>,
}

/// Scan-time file metadata threaded through to persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub mtime: i64,
    pub size: i64,
    /// FNV-1a 64-bit content hash of the file bytes, hex-encoded (16 chars).
    pub content_hash: String,
}

/// The 14 base entity kinds of the coverage-parity checklist plus
/// superset-safe additions (Import). Sourced from varde's `intelligence-schema.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Function,
    Class,
    Interface,
    Variable,
    Parameter,
    Export,
    Call,
    Literal,
    MemberAccess,
    /// An import/use/require statement naming the module it points at
    /// (`name` = module specifier). Superset addition consumed by resolution.
    Import,
    Catch,
    Throw,
    ControlFlow,
    Route,
    Response,
    /// A class/interface `extends` relationship (superset addition consumed
    /// by type-hierarchy resolution; not yet extracted).
    Extends,
    /// A class `implements` relationship (superset addition consumed by
    /// type-hierarchy resolution; not yet extracted).
    Implements,
    /// A decorator/annotation attached to a declaration (superset addition;
    /// not yet extracted).
    Decorator,
    /// The declared type of a field/parameter/local variable: `name` is the
    /// type name, `enclosing_function` is the variable's own name. Lets call
    /// resolution recover a receiver's static type (`_svc.Do()` where
    /// `_svc : IFoo`) to disambiguate a method call the plain name-uniqueness
    /// fallback can't. Emitted only for the namespace-import languages that
    /// use it (see `resolve::resolves_imports_by_namespace`).
    TypeRef,
    /// An anonymous callable span. It partitions a surrounding named
    /// function without creating a separately reportable declaration.
    CallableBoundary,
}

impl EntityKind {
    /// Stable discriminant for `entities.kind INTEGER` storage — order must
    /// never change (existing DBs would silently misread), only append.
    pub fn as_i64(self) -> i64 {
        match self {
            EntityKind::Function => 0,
            EntityKind::Class => 1,
            EntityKind::Interface => 2,
            EntityKind::Variable => 3,
            EntityKind::Parameter => 4,
            EntityKind::Export => 5,
            EntityKind::Call => 6,
            EntityKind::Literal => 7,
            EntityKind::MemberAccess => 8,
            EntityKind::Import => 9,
            EntityKind::Catch => 10,
            EntityKind::Throw => 11,
            EntityKind::ControlFlow => 12,
            EntityKind::Route => 13,
            EntityKind::Response => 14,
            EntityKind::Extends => 15,
            EntityKind::Implements => 16,
            EntityKind::Decorator => 17,
            EntityKind::TypeRef => 18,
            EntityKind::CallableBoundary => 19,
        }
    }

    pub fn from_i64(v: i64) -> Option<Self> {
        Some(match v {
            0 => EntityKind::Function,
            1 => EntityKind::Class,
            2 => EntityKind::Interface,
            3 => EntityKind::Variable,
            4 => EntityKind::Parameter,
            5 => EntityKind::Export,
            6 => EntityKind::Call,
            7 => EntityKind::Literal,
            8 => EntityKind::MemberAccess,
            9 => EntityKind::Import,
            10 => EntityKind::Catch,
            11 => EntityKind::Throw,
            12 => EntityKind::ControlFlow,
            13 => EntityKind::Route,
            14 => EntityKind::Response,
            15 => EntityKind::Extends,
            16 => EntityKind::Implements,
            17 => EntityKind::Decorator,
            18 => EntityKind::TypeRef,
            19 => EntityKind::CallableBoundary,
            _ => return None,
        })
    }

    /// snake_case name, matching the existing serde rendering — used where
    /// SQL-stored entities need to be rendered back to the CLI/JSON contract.
    pub fn as_str(self) -> &'static str {
        match self {
            EntityKind::Function => "function",
            EntityKind::Class => "class",
            EntityKind::Interface => "interface",
            EntityKind::Variable => "variable",
            EntityKind::Parameter => "parameter",
            EntityKind::Export => "export",
            EntityKind::Call => "call",
            EntityKind::Literal => "literal",
            EntityKind::MemberAccess => "member_access",
            EntityKind::Import => "import",
            EntityKind::Catch => "catch",
            EntityKind::Throw => "throw",
            EntityKind::ControlFlow => "control_flow",
            EntityKind::Route => "route",
            EntityKind::Response => "response",
            EntityKind::Extends => "extends",
            EntityKind::Implements => "implements",
            EntityKind::Decorator => "decorator",
            EntityKind::TypeRef => "type_ref",
            EntityKind::CallableBoundary => "callable_boundary",
        }
    }
}

#[cfg(test)]
mod entity_kind_tests {
    use super::EntityKind;

    #[test]
    fn as_i64_from_i64_round_trip() {
        let all = [
            EntityKind::Function,
            EntityKind::Class,
            EntityKind::Interface,
            EntityKind::Variable,
            EntityKind::Parameter,
            EntityKind::Export,
            EntityKind::Call,
            EntityKind::Literal,
            EntityKind::MemberAccess,
            EntityKind::Import,
            EntityKind::Catch,
            EntityKind::Throw,
            EntityKind::ControlFlow,
            EntityKind::Route,
            EntityKind::Response,
            EntityKind::Extends,
            EntityKind::Implements,
            EntityKind::Decorator,
            EntityKind::TypeRef,
            EntityKind::CallableBoundary,
        ];
        for kind in all {
            assert_eq!(EntityKind::from_i64(kind.as_i64()), Some(kind));
        }

        assert_eq!(EntityKind::Extends.as_i64(), 15);
        assert_eq!(EntityKind::from_i64(15), Some(EntityKind::Extends));
        assert_eq!(EntityKind::Implements.as_i64(), 16);
        assert_eq!(EntityKind::from_i64(16), Some(EntityKind::Implements));
        assert_eq!(EntityKind::Decorator.as_i64(), 17);
        assert_eq!(EntityKind::from_i64(17), Some(EntityKind::Decorator));
        assert_eq!(EntityKind::CallableBoundary.as_i64(), 19);
        assert_eq!(EntityKind::from_i64(19), Some(EntityKind::CallableBoundary));
        assert_eq!(EntityKind::CallableBoundary.as_str(), "callable_boundary");

        // Existing discriminants unchanged.
        assert_eq!(EntityKind::Function.as_i64(), 0);
        assert_eq!(EntityKind::Response.as_i64(), 14);
    }
}

/// A top-level structural unit extracted from source (function, class, call, ...).
///
/// Entity/Symbol boundary rule: an `Entity` is a declared or occurring
/// structural construct; a `Symbol` is a named binding/reference that is *not*
/// the primary name of an Entity (see `crate::extract::symbol`). The two lists
/// are kept non-redundant: the declaration's own name lives on the Entity, and
/// references *to* it live as Symbols.
#[derive(Debug, Clone, Serialize)]
pub struct Entity {
    pub kind: EntityKind,
    pub name: String,
    /// Index into the file table (`ExtractOutput::files` / the interned path
    /// list threaded through scan/resolve/persist) rather than an owned
    /// path, since a repo-scale run allocates one `Entity` per construct but
    /// only has a few thousand distinct file paths.
    pub file_id: u32,
    pub span: Span,
    /// For control-flow/error-handling entities: the name of the nearest
    /// enclosing named function/method (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_function: Option<String>,
    /// Route entities: HTTP method (get/post/...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Route entities: path string from the first call argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Response entities: HTTP status (e.g. "200") when derivable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Response entities: body shape (json/send/...) when derivable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_shape: Option<String>,
    /// Function entities: per-band MinHash signatures (one `u64` per band),
    /// computed at extract time and consumed by clone-band detection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_minhash: Option<Vec<u64>>,
    /// Function/method entities: whether the declaration carries the
    /// language's async modifier (`async fn` in Rust, `async function`/`async
    /// () =>` in JS/TS, `async def` in Python, `async` methods in C#, `suspend`
    /// in Kotlin). `None` for non-function entities and languages without an
    /// async construct.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_async: Option<bool>,
    /// Function entities: whether the declaration is a test function (Rust
    /// `#[test]`/`#[tokio::test]`/etc.). Always `false` for non-function
    /// entities and languages without test-attribute detection. Excluded
    /// from clone-band scan findings so shared test scaffolding doesn't
    /// register as production code duplication.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_test: bool,
    /// Function entities: name of the immediately enclosing class/interface/
    /// impl-target type (if the function is a method, not a free function).
    /// Drives class-membership queries (SOLID rules: interface-coverage,
    /// fat-interface member counts) that would otherwise have no way to
    /// answer "which methods belong to this type" from the flat entity list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_type: Option<String>,
}

/// A named binding or reference within or across entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// The point where a name is introduced (e.g. an import, a type parameter).
    Binding,
    /// A use of a name introduced elsewhere.
    Reference,
}

impl SymbolKind {
    pub fn as_i64(self) -> i64 {
        match self {
            SymbolKind::Binding => 0,
            SymbolKind::Reference => 1,
        }
    }

    pub fn from_i64(v: i64) -> Option<Self> {
        Some(match v {
            0 => SymbolKind::Binding,
            1 => SymbolKind::Reference,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Binding => "binding",
            SymbolKind::Reference => "reference",
        }
    }
}

/// A named binding or reference within or across entities.
#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    pub file_id: u32,
    pub span: Span,
}

/// Per-file diagnostic emitted when a file is skipped or partially processed.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub file_id: u32,
    pub message: String,
    pub severity: String,
}

/// A byte/line range within a source file (1-based lines, 0-based columns,
/// 0-based byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// Cast a byte offset/line/column into a [`Span`] field, saturating to
/// `u32::MAX` and logging a warning instead of silently wrapping when the
/// source value doesn't fit — e.g. a >4 GB file's byte offsets, or an
/// out-of-range value from an untrusted `kind=sql` rule's own query result.
/// Saturating (vs. dropping the file/finding) keeps the pipeline running;
/// the warning is what makes the otherwise-silent truncation visible.
pub fn saturating_u32<T>(value: T) -> u32
where
    T: TryInto<u32> + Copy + std::fmt::Display,
{
    match value.try_into() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(value = %value, "value does not fit in u32; saturating to u32::MAX");
            u32::MAX
        }
    }
}
