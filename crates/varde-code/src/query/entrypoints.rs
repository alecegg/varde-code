//! Semantic entrypoint detection.
//!
//! A standalone computable unit for the nav-map "Entrypoints" section (see
//! `memory-bank/working/plans/2026-09-03-nav-map-draft/plan.md`, "Sections
//! (output shape)" → "Entrypoints"): function/class entities role-tagged
//! (route handler, page component, CLI command, background job, event
//! listener, middleware) via [`crate::extract::langs::role_tags`], with
//! bootstrap/process entrypoints (`main.rs`, `__main__.py`, ...) excluded.
//!
//! Role-tag matching is not reimplemented here — every candidate entity is
//! joined against its `Decorator`/`Extends`/`Implements` entities (via
//! `enclosing_function`, the same join `resolve_type_hierarchy` uses to walk
//! from an `Extends`/`Implements` entity back to its owning class) and its
//! file path, then handed to [`crate::extract::langs::role_tags::match_role_tag`].
//! Generated/vendored paths are dropped via
//! [`super::noise_filter::is_generated_or_vendored_path`]. This module is not
//! yet wired into any dispatcher/CLI mode — that's the later
//! `nav-map-dispatch-cli` task; flow-tree walking is a separate later task
//! (`flows-reachable-tree`).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::extract::langs::role_tags::{self, RoleTag, RoleTagRule};
use crate::model::EntityKind;

use super::noise_filter::{
    is_frontend_asset_path, is_generated_or_vendored_path, is_scaffold_template_path,
};
use super::{ApiError, db_err};

/// Route metadata (`method`, `path`) stamped on `Decorator` entities, keyed by
/// the decorated declaration `(file_id, enclosing_function)` — an action
/// method's own name, or a controller class's name for a base-path prefix.
type RouteMetaByOwner = HashMap<(i64, String), (Option<String>, Option<String>)>;

/// One detected semantic entrypoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entrypoint {
    pub entity_id: i64,
    pub file: String,
    pub symbol: String,
    pub role: RoleTag,
    /// Whether `entity_id` is a callable flow-tree root. Role-tagged handlers
    /// and call-based routes resolved to their handler function are `true`; a
    /// path-only route (registration site with no resolvable handler) is
    /// `false` and is excluded from flow-tree building.
    pub flow_root: bool,
    /// HTTP verb for a route entrypoint (`"GET"`), when known. For an
    /// annotation/decorator handler this is recovered from the route decorator
    /// (`@GetMapping` -> `GET`); the `symbol` stays the handler name so it
    /// remains a valid `explore` target and flow-tree root.
    pub method: Option<String>,
    /// HTTP path for a route entrypoint (`"/api/catalog/{id}"`), when known —
    /// the method-level path combined with any class-level base path.
    pub path: Option<String>,
}

impl RoleTag {
    fn as_str(self) -> &'static str {
        match self {
            RoleTag::RouteHandler => "route_handler",
            RoleTag::PageComponent => "page_component",
            RoleTag::CliCommand => "cli_command",
            RoleTag::BackgroundJob => "background_job",
            RoleTag::EventListener => "event_listener",
            RoleTag::Middleware => "middleware",
            RoleTag::ProcessMain => "process_main",
        }
    }
}

impl Entrypoint {
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "entity_id": self.entity_id,
            "file": self.file,
            "symbol": self.symbol,
            "role": self.role.as_str(),
        });
        let map = obj.as_object_mut().expect("json object");
        if let Some(method) = &self.method {
            map.insert("method".into(), serde_json::Value::String(method.clone()));
        }
        if let Some(path) = &self.path {
            map.insert("path".into(), serde_json::Value::String(path.clone()));
        }
        obj
    }
}

/// Bootstrap/process-entrypoint filename conventions (primary exclusion
/// signal). Matched against the file path's final component; case-sensitive
/// (all these conventions are themselves case-sensitive in practice).
const BOOTSTRAP_FILENAMES: &[&str] = &[
    "main.rs",
    "main.py",
    "__main__.py",
    "Program.cs",
    "index.ts",
    "index.js",
    "main.go",
    "main.java",
    "Main.java",
    "main.c",
    "main.cpp",
];

/// Returns `true` when `path`'s final component matches a known
/// bootstrap/process-entrypoint filename convention.
fn is_bootstrap_filename(path: &str) -> bool {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    BOOTSTRAP_FILENAMES.contains(&base)
}

/// Rule table selection per language, keyed the same way the extractors key
/// their own tree-sitter grammar choice: by file extension.
fn rules_for_path(path: &str) -> Option<&'static [RoleTagRule]> {
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "py" => role_tags::PYTHON_RULES,
        "java" => role_tags::JAVA_RULES,
        "cs" => role_tags::CS_RULES,
        "ts" => role_tags::TS_RULES,
        "tsx" => role_tags::TSX_RULES,
        "js" | "jsx" | "mjs" | "cjs" => role_tags::JS_RULES,
        "rb" => role_tags::RUBY_RULES,
        "kt" | "kts" => role_tags::KOTLIN_RULES,
        "scala" | "sc" => role_tags::SCALA_RULES,
        "php" => role_tags::PHP_RULES,
        _ => return None,
    })
}

/// A candidate function/class entity plus the joined signals used for
/// role-tag matching and bootstrap exclusion.
struct Candidate {
    entity_id: i64,
    file_id: i64,
    path: String,
    name: String,
    /// Entity kind (`EntityKind::as_i64`): `Function` or `Class`. Used to gate
    /// a class candidate on actually owning methods (see [`detect`]).
    kind: i64,
    /// The owning type's name for a method entity, else NULL/empty. A
    /// constructor is a method whose name equals its `owner_type` — used to
    /// drop constructors from candidacy so they don't inherit their class's
    /// type-level decorators (see [`detect`]).
    owner_type: Option<String>,
    decorators: Vec<String>,
    base_classes: Vec<String>,
}

/// Detect semantic entrypoints across the whole persisted repo.
///
/// Algorithm:
/// 1. Load every `Function`/`Class` entity plus its file path.
/// 2. Join each candidate against `Decorator` and `Extends`/`Implements`
///    entities sharing the same `file_id` and whose `enclosing_function`
///    equals the candidate's name (the exact join
///    [`crate::resolve::resolve_type_hierarchy`] uses to walk from an
///    `Extends`/`Implements` entity back to its owning class).
/// 3. Run the joined (decorator, base_class, path) signals through
///    [`role_tags::match_role_tag`] using the per-language rule table
///    selected by file extension.
/// 4. Drop generated/vendored paths.
/// 5. Drop bootstrap/process entrypoints: primary signal is a filename
///    convention match ([`is_bootstrap_filename`]); supplementary signal is
///    fan-out/fan-in asymmetry over resolved `Call` edges (calls at least
///    [`FAN_OUT_MIN`] other functions while being called by nobody) — this
///    only ever *adds* an exclusion for entities the filename check missed,
///    never overrides a role-tag match on its own when the filename doesn't
///    already flag it and the asymmetry isn't strong.
pub fn detect(conn: &Connection) -> Result<Vec<Entrypoint>, ApiError> {
    let mut stmt = conn
        .prepare(
            // Test files are excluded: a route handler / job defined in a
            // test file is a fixture, not a real application entrypoint. This
            // also neutralizes decorator collisions with test tooling — e.g.
            // `@patch` / `@mock.patch` (unittest.mock) shares its final
            // segment with the HTTP verb `patch`, but lives in test files.
            "SELECT e.id, e.file_id, f.path, e.name, e.kind, e.owner_type
             FROM entities e
             JOIN files f ON f.id = e.file_id
             WHERE e.kind IN (?1, ?2) AND f.is_test_path = 0",
        )
        .map_err(db_err)?;
    let candidates: Vec<Candidate> = stmt
        .query_map(
            [EntityKind::Function.as_i64(), EntityKind::Class.as_i64()],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?
        .into_iter()
        .map(
            |(entity_id, file_id, path, name, kind, owner_type)| Candidate {
                entity_id,
                file_id,
                path,
                name,
                kind,
                owner_type,
                decorators: Vec::new(),
                base_classes: Vec::new(),
            },
        )
        .collect();

    // A class becomes an entrypoint via a type-level signal (`extends
    // ControllerBase`, `[ApiController]`, ...) — but a controller/handler class
    // with *no methods of its own* has no routes to serve and is not a real
    // entrypoint. eShopOnWeb's `BaseApiController` (an empty `{ }` body,
    // commented "No longer used") was surfaced purely on its base class. Gate
    // class candidates on owning at least one method: a method carries its
    // class name in `owner_type` (same file), so a class with no matching
    // method row is empty. Keyed by (file_id, class name).
    let classes_with_methods: std::collections::HashSet<(i64, String)> = candidates
        .iter()
        .filter(|c| c.kind == EntityKind::Function.as_i64())
        .filter_map(|c| {
            c.owner_type
                .as_deref()
                .filter(|o| !o.is_empty())
                .map(|o| (c.file_id, o.to_string()))
        })
        .collect();

    // Batch the per-candidate decorator/base-class joins into two repo-wide
    // scans keyed by (file_id, enclosing_function). The previous shape ran
    // two indexed lookups per candidate (`decorators_for` + `base_classes_for`),
    // an N+1 that dominated `detect` on large repos; folding them into two
    // grouped reads makes the cost O(rows) instead of O(candidates × lookups).
    let decorators_by_owner = signals_by_owner(conn, &[EntityKind::Decorator.as_i64()])?;
    let base_classes_by_owner = signals_by_owner(
        conn,
        &[
            EntityKind::Extends.as_i64(),
            EntityKind::Implements.as_i64(),
        ],
    )?;
    // HTTP method+path stamped onto route `Decorator` entities by the
    // extractors (`@app.get("/x")`, `[HttpGet("{id}")]`, `@GetMapping(...)`,
    // ...), keyed by the decorated declaration `(file_id, name)` — a method's
    // own name for an action decorator, the class's name for a class-level
    // prefix (`@RequestMapping("/api")`). Lets a decorator/annotation handler
    // be surfaced as `"<VERB> <path>"` like the call-based routes are.
    let route_meta_by_owner = route_meta_by_owner(conn)?;

    let mut results = Vec::new();
    for mut candidate in candidates {
        if is_generated_or_vendored_path(&candidate.path)
            || is_scaffold_template_path(&candidate.path)
            || is_frontend_asset_path(&candidate.path)
        {
            continue;
        }

        // A constructor (a method whose name equals its owning type) shares
        // the class's name, so the name-keyed decorator/base-class join below
        // would attach the *class's* type-level attributes (`[ApiController]`,
        // `extends ControllerBase`) to it — emitting the controller a second
        // time as a phantom entrypoint (and a duplicate flow tree). The class
        // entity itself carries no `owner_type`, so it is unaffected. This is
        // a no-op for languages whose constructors don't share the class name
        // (Python `__init__`, TS `constructor`).
        if candidate
            .owner_type
            .as_deref()
            .is_some_and(|owner| !owner.is_empty() && owner == candidate.name)
        {
            continue;
        }

        // Drop class candidates that own no methods: an empty controller/
        // handler class serves no routes and only bloats the (highest-priority)
        // entrypoints section. Method candidates are unaffected.
        if candidate.kind == EntityKind::Class.as_i64()
            && !classes_with_methods.contains(&(candidate.file_id, candidate.name.clone()))
        {
            continue;
        }

        let Some(rules) = rules_for_path(&candidate.path) else {
            continue;
        };

        candidate.decorators = decorators_by_owner
            .get(&(candidate.file_id, candidate.name.clone()))
            .cloned()
            .unwrap_or_default();
        candidate.base_classes = base_classes_by_owner
            .get(&(candidate.file_id, candidate.name.clone()))
            .cloned()
            .unwrap_or_default();
        // Try every (decorator, base_class) combination the candidate
        // carries — a stacked decorator or a class implementing more than
        // one interface must not lose a role-tag match just because it
        // isn't the first row returned. `None` stands in for "this signal
        // absent" so a candidate with, say, only a base class (no
        // decorators) still matches base-class-only rules.
        let decorator_candidates: Vec<Option<&str>> = if candidate.decorators.is_empty() {
            vec![None]
        } else {
            candidate
                .decorators
                .iter()
                .map(|d| Some(d.as_str()))
                .collect()
        };
        let base_class_candidates: Vec<Option<&str>> = if candidate.base_classes.is_empty() {
            vec![None]
        } else {
            candidate
                .base_classes
                .iter()
                .map(|b| Some(b.as_str()))
                .collect()
        };
        let role = decorator_candidates.iter().find_map(|&d| {
            base_class_candidates.iter().find_map(|&b| {
                role_tags::match_role_tag(rules, d, b, Some(candidate.path.as_str()))
            })
        });
        let Some(role) = role else {
            continue;
        };

        // Primary bootstrap signal: filename convention.
        if is_bootstrap_filename(&candidate.path) {
            continue;
        }
        // Supplementary bootstrap signal: strong fan-out/fan-in asymmetry.
        if is_bootstrap_by_fan_asymmetry(conn, candidate.entity_id)? {
            continue;
        }

        // Attach the HTTP verb+path when this is a decorator/annotation route
        // handler (combining any class-level base path). The symbol stays the
        // handler name so it remains a valid `explore` target and flow root.
        let (method, path) = route_fields(&candidate, &route_meta_by_owner);
        results.push(Entrypoint {
            entity_id: candidate.entity_id,
            file: candidate.path,
            symbol: candidate.name,
            role,
            flow_root: true,
            method,
            path,
        });
    }

    results.sort_by(|a, b| a.file.cmp(&b.file).then(a.symbol.cmp(&b.symbol)));
    Ok(results)
}

/// The `(method, path)` route fields for a decorator/annotation handler
/// `candidate`, from the route metadata stamped on its `Decorator` entities.
/// Only method/function candidates carry a route (a class contributes just a
/// base-path prefix, prepended here via the method's `owner_type`). Returns
/// `(None, None)` when the candidate is a class or carries no route metadata,
/// and drops a verb with no path anywhere (not informative on its own).
fn route_fields(
    candidate: &Candidate,
    meta: &RouteMetaByOwner,
) -> (Option<String>, Option<String>) {
    if candidate.kind != EntityKind::Function.as_i64() {
        return (None, None);
    }
    let Some((method, sub_path)) = meta.get(&(candidate.file_id, candidate.name.clone())) else {
        return (None, None);
    };
    let prefix = candidate
        .owner_type
        .as_deref()
        .filter(|c| !c.is_empty())
        .and_then(|c| meta.get(&(candidate.file_id, c.to_string())))
        .and_then(|(_, p)| p.as_deref());
    let full = join_route_path(prefix, sub_path.as_deref());
    match full {
        Some(p) => (method.as_ref().map(|m| m.to_uppercase()), Some(p)),
        None => (None, None),
    }
}

/// Join a class-level base path and a method-level sub-path into one
/// slash-normalized route (`Some("/api")` + `Some("{id}")` -> `"/api/{id}"`),
/// trimming stray slashes and dropping empty segments. `None` when neither
/// contributes a segment.
fn join_route_path(prefix: Option<&str>, sub: Option<&str>) -> Option<String> {
    let mut segs: Vec<&str> = Vec::new();
    for part in [prefix, sub].into_iter().flatten() {
        let trimmed = part.trim().trim_matches('/');
        if !trimmed.is_empty() {
            segs.push(trimmed);
        }
    }
    (!segs.is_empty()).then(|| format!("/{}", segs.join("/")))
}

/// Load route metadata (`method`, `path`) stamped on `Decorator` entities,
/// keyed by the decorated declaration `(file_id, enclosing_function)`. The
/// first decorator seen per owner wins — an action method carries at most one
/// route decorator, and a class its one base-path annotation.
fn route_meta_by_owner(conn: &Connection) -> Result<RouteMetaByOwner, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT file_id, enclosing_function, method, path FROM entities
             WHERE kind = ?1 AND enclosing_function IS NOT NULL
               AND (method IS NOT NULL OR path IS NOT NULL)",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([EntityKind::Decorator.as_i64()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(db_err)?;
    let mut map: RouteMetaByOwner = HashMap::new();
    for row in rows {
        let (file_id, owner, method, path) = row.map_err(db_err)?;
        map.entry((file_id, owner)).or_insert((method, path));
    }
    Ok(map)
}

/// File extensions whose HTTP routes are registered through *calls* rather
/// than decorators/annotations — so the extractor's `Route` entity is the only
/// entrypoint signal and it does not duplicate a role-tagged handler.
///
/// Python and Java are deliberately absent: their `Route` entities are emitted
/// from the very `@app.route` / `@GetMapping` decorator that already makes the
/// handler a role-tagged entrypoint in [`detect`], so surfacing them here too
/// would double-count. (Kotlin's `Route` comes from the Ktor DSL and C#'s from
/// `app.MapGet` minimal APIs — neither of which is decorator-role-tagged — so
/// both are included.)
fn route_is_call_based(path: &str) -> bool {
    let Some(ext) = path.rsplit('.').next() else {
        return false;
    };
    matches!(
        ext,
        "go" | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "php"
            | "rb"
            | "cs"
            | "kt"
            | "kts"
            // Elixir/Phoenix: routes are `get "/path", Ctrl, :action` macro
            // calls in the router module — call-based, not annotations. The
            // extractor already emits `Route` entities for them (audit F10).
            | "ex"
            | "exs"
    )
}

/// Detect call-based HTTP routes (Go/Express/Laravel/C# minimal-API/Ktor, ...)
/// as entrypoints, from the `Route` entities the extractors emit at the
/// registration site. Complements [`detect`], which only sees
/// decorator/base-class-role-tagged `Function`/`Class` handlers and never the
/// call-registered routes of frameworks without annotations.
///
/// The symbol is `"<METHOD> <path>"` (e.g. `"GET /users"`), or just the path
/// when the verb is unknown (net/http `HandleFunc` -> `"*"`). Test and
/// generated/vendored paths are excluded, and identical `(file, symbol)` pairs
/// are de-duplicated.
///
/// When the registration named its handler with a bare identifier (stashed on
/// the `Route` entity's `owner_type` by the extractor), the handler is resolved
/// to its `Function` entity — same file first, then a repo-wide unique-name
/// fallback — and the entrypoint is rooted at that function (`flow_root =
/// true`) so it gets a real flow tree. An unresolved / closure handler stays
/// rooted at the `Route` entity itself (`flow_root = false`), since a bare
/// registration site has no outgoing call edges.
pub fn detect_routes(conn: &Connection) -> Result<Vec<Entrypoint>, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT e.id, e.file_id, f.path, e.method, e.path, e.owner_type
             FROM entities e
             JOIN files f ON f.id = e.file_id
             WHERE e.kind = ?1 AND f.is_test_path = 0",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([EntityKind::Route.as_i64()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;

    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();
    for (route_id, file_id, file, method, route_path, handler) in rows {
        if !route_is_call_based(&file)
            || is_generated_or_vendored_path(&file)
            || is_scaffold_template_path(&file)
            || is_frontend_asset_path(&file)
        {
            continue;
        }
        // A route with no path carries nothing worth surfacing.
        let Some(path) = route_path.filter(|p| !p.is_empty()) else {
            continue;
        };
        let verb = match method.as_deref() {
            Some(m) if !m.is_empty() && m != "*" => Some(m.to_uppercase()),
            _ => None,
        };
        let symbol = match &verb {
            Some(m) => format!("{m} {path}"),
            None => path.clone(),
        };
        // Root the route at its handler function when we can name and resolve
        // it, so the entrypoint carries a real call graph.
        let handler_id = match handler.as_deref().filter(|h| !h.is_empty()) {
            Some(name) => resolve_handler(conn, name, file_id)?,
            None => None,
        };
        let (entity_id, flow_root) = match handler_id {
            Some(id) => (id, true),
            None => (route_id, false),
        };
        if seen.insert((file.clone(), symbol.clone())) {
            results.push(Entrypoint {
                entity_id,
                file,
                symbol,
                role: RoleTag::RouteHandler,
                flow_root,
                method: verb,
                path: Some(path),
            });
        }
    }

    results.sort_by(|a, b| a.file.cmp(&b.file).then(a.symbol.cmp(&b.symbol)));
    Ok(results)
}

/// The `main`/`Main` function name that marks a language's process entrypoint,
/// keyed by file extension. Deliberately narrow: only languages with a genuine
/// named-function program entry are listed, so a helper coincidentally named
/// `main` in an unrelated language (a Ruby method, a Python function) is not
/// mistaken for one. C# capitalizes `Main`; the rest use `main`. Python's
/// `if __name__ == "__main__"` guard and Node bin scripts have no named entry
/// function, so they are out of scope here.
fn process_main_name_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "rs" | "go" | "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "java" | "kt" | "kts" => {
            "main"
        }
        "cs" => "Main",
        _ => return None,
    })
}

/// Detect language process entrypoints — the `main`/`Main` function a program
/// starts at (Rust/Go/C/C++/Java/Kotlin `main`, C# `Main`). Complements
/// [`detect`], which only surfaces decorator/base-class-role-tagged web handlers
/// and deliberately *excludes* these bootstrap functions; this reinstates them
/// as their own [`RoleTag::ProcessMain`] category so a CLI/binary's true entry
/// point is navigable (and gets a real flow tree from `main`'s call graph).
///
/// Matched structurally by name + language (not a decorator), scoped to
/// `Function` entities; test, generated/vendored, and scaffold paths are
/// excluded, consistent with the other detectors.
pub fn detect_process_mains(conn: &Connection) -> Result<Vec<Entrypoint>, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT e.id, f.path, e.name
             FROM entities e
             JOIN files f ON f.id = e.file_id
             WHERE e.kind = ?1 AND f.is_test_path = 0
               AND e.owner_type IS NULL AND e.name IN ('main', 'Main')",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([EntityKind::Function.as_i64()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;

    let mut results = Vec::new();
    for (entity_id, file, name) in rows {
        if is_generated_or_vendored_path(&file)
            || is_scaffold_template_path(&file)
            || is_frontend_asset_path(&file)
        {
            continue;
        }
        // The name must be the *right* main for the file's language (C# `Main`,
        // everyone else `main`) — so a Rust struct method named `Main` or a C#
        // helper named `main` is not surfaced.
        if process_main_name_for_path(&file) != Some(name.as_str()) {
            continue;
        }
        results.push(Entrypoint {
            entity_id,
            file,
            symbol: name,
            role: RoleTag::ProcessMain,
            flow_root: true,
            method: None,
            path: None,
        });
    }

    results.sort_by(|a, b| a.file.cmp(&b.file).then(a.symbol.cmp(&b.symbol)));
    Ok(results)
}

/// Resolve a route handler's bare name to its `Function` entity id: prefer a
/// declaration in the same file as the registration, else a repo-wide unique
/// match. Returns `None` when the name is absent, unknown, or ambiguous
/// (matched by more than one function), so an unresolvable handler falls back
/// to a path-only route rather than pointing at the wrong function.
fn resolve_handler(conn: &Connection, name: &str, file_id: i64) -> Result<Option<i64>, ApiError> {
    let func = EntityKind::Function.as_i64();
    let same_file: Option<i64> = conn
        .prepare_cached(
            "SELECT id FROM entities
             WHERE kind = ?1 AND name = ?2 AND file_id = ?3
             ORDER BY id LIMIT 1",
        )
        .map_err(db_err)?
        .query_row(rusqlite::params![func, name, file_id], |r| r.get(0))
        .optional()
        .map_err(db_err)?;
    if same_file.is_some() {
        return Ok(same_file);
    }
    // Repo-wide: accept only a unique match (LIMIT 2 tells unique from ambiguous).
    let ids: Vec<i64> = conn
        .prepare_cached("SELECT id FROM entities WHERE kind = ?1 AND name = ?2 LIMIT 2")
        .map_err(db_err)?
        .query_map(rusqlite::params![func, name], |r| r.get(0))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    Ok((ids.len() == 1).then(|| ids[0]))
}

/// Load role-tag signal entities (`Decorator`, or `Extends`/`Implements`) of
/// the given `kinds` for the whole repo, grouped by their owning declaration
/// `(file_id, enclosing_function)` — the same join `resolve_type_hierarchy`
/// walks from an annotation/relationship entity back to its owning
/// declaration, only batched across every owner at once instead of one
/// indexed lookup per candidate. Rows are read in `id` order so a stacked
/// decorator or multi-interface class keeps every signal in declaration
/// order (matching the old per-candidate `ORDER BY id`); rows with a NULL
/// `enclosing_function` are skipped since they can't name an owner.
fn signals_by_owner(
    conn: &Connection,
    kinds: &[i64],
) -> Result<HashMap<(i64, String), Vec<String>>, ApiError> {
    let placeholders = vec!["?"; kinds.len()].join(", ");
    let sql = format!(
        "SELECT file_id, enclosing_function, name FROM entities
         WHERE kind IN ({placeholders}) AND enclosing_function IS NOT NULL
         ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(kinds.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(db_err)?;

    let mut by_owner: HashMap<(i64, String), Vec<String>> = HashMap::new();
    for row in rows {
        let (file_id, owner, name) = row.map_err(db_err)?;
        by_owner.entry((file_id, owner)).or_default().push(name);
    }
    Ok(by_owner)
}

/// Minimum resolved fan-out (calls made) required before the fan-out/fan-in
/// asymmetry heuristic considers an entity a bootstrap candidate. Kept small
/// since a `main`-shaped function typically wires up just a few things
/// (config, router, server start) but is itself never called.
const FAN_OUT_MIN: i64 = 2;

/// Supplementary bootstrap signal: an entity that calls at least
/// [`FAN_OUT_MIN`] other entities (resolved `Call` edges out) but is called
/// by zero other entities (resolved `Call` edges in) — the shape of a
/// process bootstrap function that isn't named per any filename convention.
/// Deliberately narrow (fan-in must be exactly zero) so this never overrides
/// a legitimate, already-role-tagged, frequently-called route
/// handler/component/etc.
fn is_bootstrap_by_fan_asymmetry(conn: &Connection, entity_id: i64) -> Result<bool, ApiError> {
    let call_kind = crate::resolve::EdgeKind::Call.as_i64();
    let fan_out: i64 = conn
        .prepare_cached(
            "SELECT COUNT(*) FROM resolved_edges
             WHERE kind = ?1 AND resolved = 1 AND from_entity_id = ?2",
        )
        .map_err(db_err)?
        .query_row(rusqlite::params![call_kind, entity_id], |r| r.get(0))
        .map_err(db_err)?;
    let fan_in: i64 = conn
        .prepare_cached(
            "SELECT COUNT(*) FROM resolved_edges
             WHERE kind = ?1 AND resolved = 1 AND to_entity_id = ?2",
        )
        .map_err(db_err)?
        .query_row(rusqlite::params![call_kind, entity_id], |r| r.get(0))
        .map_err(db_err)?;
    Ok(fan_out >= FAN_OUT_MIN && fan_in == 0)
}

#[cfg(test)]
mod semantic_entrypoint_tests {
    use super::*;

    #[test]
    fn route_is_call_based_covers_elixir_and_php_and_js() {
        // Frameworks whose routes are call/macro registrations, not
        // annotations (audit F10): Phoenix (`.ex`), Slim (`.php`), Express
        // (`.js`/`.ts`). Rust/Python are decorator/attribute-driven and must
        // stay out of the call-based route path.
        for p in [
            "lib/router.ex",
            "config/routes.exs",
            "src/routes.php",
            "app.ts",
        ] {
            assert!(route_is_call_based(p), "{p} should be call-based");
        }
        for p in ["src/main.rs", "app/views.py"] {
            assert!(!route_is_call_based(p), "{p} should NOT be call-based");
        }
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "varde-entrypoints-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp root creates");
        path
    }

    use crate::query::test_support::test_support::with_isolated_home;

    /// AC1: a fixture repo with one Flask `@app.route` handler and one plain
    /// `main()` bootstrap function (in a non-bootstrap-named file) → the
    /// route handler is in the output, `main()` is not.
    #[test]
    fn semantic_entrypoint_includes_route_handler_excludes_plain_main() {
        with_isolated_home("entrypoints", "route-vs-main", || {
            let root = temp_root("route-vs-main");
            std::fs::write(
                root.join("app.py"),
                "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/hello\")\ndef hello():\n    return \"hi\"\n\ndef main():\n    hello()\n    hello()\n",
            )
            .expect("write app.py");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "hello" && e.role == RoleTag::RouteHandler),
                "route handler `hello` must be present: {entrypoints:?}"
            );
            assert!(
                !entrypoints.iter().any(|e| e.symbol == "main"),
                "plain bootstrap `main()` must be excluded: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// AC2: a fixture whose route-handler-shaped function lives in a
    /// `__main__.py`-style file → no entity from that file appears in the
    /// results, even though the decorator would otherwise role-tag it.
    #[test]
    fn semantic_entrypoint_excludes_all_entities_in_bootstrap_named_file() {
        with_isolated_home("entrypoints", "bootstrap-filename", || {
            let root = temp_root("bootstrap-filename");
            std::fs::write(
                root.join("__main__.py"),
                "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/hello\")\ndef hello():\n    return \"hi\"\n",
            )
            .expect("write __main__.py");
            // A second, non-bootstrap file with its own route handler, to
            // confirm the filter is scoped to the bootstrap file only.
            std::fs::write(
                root.join("routes.py"),
                "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/ok\")\ndef ok():\n    return \"ok\"\n",
            )
            .expect("write routes.py");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                !entrypoints.iter().any(|e| e.file.ends_with("__main__.py")),
                "no entity from the __main__.py-style file may appear: {entrypoints:?}"
            );
            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "ok" && e.file.ends_with("routes.py")),
                "the non-bootstrap file's route handler must still be present: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// S9: language process mains (`fn main`, `func main`) are surfaced by
    /// [`detect_process_mains`] with the `ProcessMain` role — even though
    /// [`detect`] excludes them as bootstrap — while an owned C# method, a
    /// wrong-case name, and a test-file main are not.
    #[test]
    fn detect_process_mains_surfaces_named_program_entrypoints() {
        with_isolated_home("entrypoints", "process-mains", || {
            let root = temp_root("process-mains");
            std::fs::write(
                root.join("main.rs"),
                "fn main() {\n    helper();\n}\nfn helper() {}\n",
            )
            .expect("write main.rs");
            std::fs::write(
                root.join("cmd.go"),
                "package main\n\nfunc main() {\n    run()\n}\nfunc run() {}\n",
            )
            .expect("write cmd.go");
            std::fs::write(
                root.join("Program.cs"),
                "class Program {\n    static void Main(string[] args) {\n    }\n}\n",
            )
            .expect("write Program.cs");
            // Wrong case for the language: a Rust `fn Main` is not a process main.
            std::fs::write(root.join("other.rs"), "fn Main() {}\n").expect("write other.rs");
            // A `main` in a test file must be excluded.
            std::fs::write(
                root.join("main_test.go"),
                "package main\n\nfunc main() {}\n",
            )
            .expect("write test main");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let mains = detect_process_mains(&conn).expect("detect computes");
            let files: Vec<(&str, &str)> = mains
                .iter()
                .map(|e| (e.file.rsplit('/').next().unwrap_or(""), e.symbol.as_str()))
                .collect();

            assert!(
                mains
                    .iter()
                    .all(|e| e.role == RoleTag::ProcessMain && e.flow_root),
                "every process main is a ProcessMain flow root: {mains:?}"
            );
            assert!(files.contains(&("main.rs", "main")), "rust main: {mains:?}");
            assert!(files.contains(&("cmd.go", "main")), "go main: {mains:?}");
            assert!(
                !mains.iter().any(|e| e.file.ends_with("Program.cs")),
                "owned c# Main must be excluded: {mains:?}"
            );
            assert!(
                !mains.iter().any(|e| e.file.ends_with("other.rs")),
                "wrong-case `fn Main` in a .rs file is excluded: {mains:?}"
            );
            assert!(
                !mains.iter().any(|e| e.file.ends_with("main_test.go")),
                "a main in a test file is excluded: {mains:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// A role-tagged handler living under a scaffold `templates/` directory is
    /// boilerplate stamped out by a project generator, not a real entrypoint of
    /// this repo → it is excluded, while a sibling handler outside `templates/`
    /// is still present.
    #[test]
    fn semantic_entrypoint_excludes_scaffold_template_handlers() {
        with_isolated_home("entrypoints", "scaffold-template", || {
            let root = temp_root("scaffold-template");
            let tmpl_dir = root.join("templates").join("crew");
            std::fs::create_dir_all(&tmpl_dir).expect("mkdir templates/crew");
            std::fs::write(
                tmpl_dir.join("scaffold.py"),
                "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/scaffold\")\ndef scaffold():\n    return \"boilerplate\"\n",
            )
            .expect("write templates/crew/scaffold.py");
            std::fs::write(
                root.join("routes.py"),
                "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/ok\")\ndef ok():\n    return \"ok\"\n",
            )
            .expect("write routes.py");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                !entrypoints.iter().any(|e| e.file.contains("templates")),
                "no handler from a scaffold templates/ dir may appear: {entrypoints:?}"
            );
            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "ok" && e.file.ends_with("routes.py")),
                "the non-template route handler must still be present: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Regression (review fix): `Class` entities must be candidates too, not
    /// just `Function` — a C# controller class extending `ControllerBase`
    /// (a base-class-keyed role-tag rule) must be detected even though it
    /// carries no decorator of its own.
    #[test]
    fn semantic_entrypoint_detects_class_via_base_class_rule() {
        with_isolated_home("entrypoints", "class-base-class", || {
            let root = temp_root("class-base-class");
            std::fs::write(
                root.join("UsersController.cs"),
                "public class UsersController : ControllerBase {\n    public void Get() {}\n}\n",
            )
            .expect("write UsersController.cs");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "UsersController" && e.role == RoleTag::RouteHandler),
                "class UsersController (extends ControllerBase) must be a route_handler entrypoint: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Regression: an ASP.NET controller with a constructor and write-verb
    /// actions. (1) `[HttpPut]`/`[HttpDelete]`/`[HttpPatch]` actions must be
    /// detected (previously only Get/Post were in the rule table). (2) The
    /// controller must appear exactly once — its constructor (a method sharing
    /// the class name) must NOT inherit the class-level `[ApiController]`
    /// attribute and produce a phantom duplicate entrypoint.
    #[test]
    fn semantic_entrypoint_csharp_controller_verbs_and_no_constructor_dup() {
        with_isolated_home("entrypoints", "cs-controller", || {
            let root = temp_root("cs-controller");
            std::fs::write(
                root.join("UsersController.cs"),
                "using Microsoft.AspNetCore.Mvc;\n\n\
                 [ApiController]\n\
                 [Route(\"api/[controller]\")]\n\
                 public class UsersController : ControllerBase\n\
                 {\n\
                 \x20\x20\x20\x20public UsersController() {}\n\
                 \x20\x20\x20\x20[HttpGet]\n\
                 \x20\x20\x20\x20public int GetAll() { return 0; }\n\
                 \x20\x20\x20\x20[HttpPut(\"{id}\")]\n\
                 \x20\x20\x20\x20public int Update(int id) { return id; }\n\
                 \x20\x20\x20\x20[HttpDelete(\"{id}\")]\n\
                 \x20\x20\x20\x20public void Delete(int id) {}\n\
                 \x20\x20\x20\x20[HttpPatch(\"{id}\")]\n\
                 \x20\x20\x20\x20public int Patch(int id) { return id; }\n\
                 }\n",
            )
            .expect("write UsersController.cs");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            for verb_action in ["GetAll", "Update", "Delete", "Patch"] {
                assert!(
                    entrypoints
                        .iter()
                        .any(|e| e.symbol == verb_action && e.role == RoleTag::RouteHandler),
                    "{verb_action} must be a route_handler entrypoint: {entrypoints:?}"
                );
            }

            let controller_count = entrypoints
                .iter()
                .filter(|e| e.symbol == "UsersController")
                .count();
            assert_eq!(
                controller_count, 1,
                "controller must appear once, not duplicated by its constructor: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// An empty controller class — the `[ApiController]`/`ControllerBase`
    /// signals are present but the body has no action methods — is not a real
    /// entrypoint (it serves no routes) and must be dropped, while a sibling
    /// controller that *does* declare an action is still surfaced. Mirrors
    /// eShopOnWeb's `BaseApiController` (an empty `{ }` reference stub).
    #[test]
    fn semantic_entrypoint_empty_controller_class_is_not_an_entrypoint() {
        with_isolated_home("entrypoints", "cs-empty-controller", || {
            let root = temp_root("cs-empty-controller");
            std::fs::write(
                root.join("BaseApiController.cs"),
                "using Microsoft.AspNetCore.Mvc;\n\n\
                 [ApiController]\n\
                 [Route(\"api/[controller]/[action]\")]\n\
                 public class BaseApiController : ControllerBase\n{ }\n",
            )
            .expect("write BaseApiController.cs");
            std::fs::write(
                root.join("CatalogController.cs"),
                "using Microsoft.AspNetCore.Mvc;\n\n\
                 [ApiController]\n\
                 public class CatalogController : ControllerBase\n\
                 {\n\
                 \x20\x20\x20\x20[HttpGet]\n\
                 \x20\x20\x20\x20public int List() { return 0; }\n\
                 }\n",
            )
            .expect("write CatalogController.cs");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                !entrypoints.iter().any(|e| e.symbol == "BaseApiController"),
                "empty controller class must NOT be an entrypoint: {entrypoints:?}"
            );
            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "CatalogController" && e.role == RoleTag::RouteHandler),
                "non-empty controller class must still be an entrypoint: {entrypoints:?}"
            );
            assert!(
                entrypoints.iter().any(|e| e.symbol == "List"),
                "the action method itself must still be an entrypoint: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// PHP Symfony: `#[Route]`/`#[Get]` attributes (now extracted as Decorator
    /// entities) and the `AbstractController` base class must surface a
    /// controller and its action as route-handler entrypoints.
    #[test]
    fn semantic_entrypoint_php_symfony_controller() {
        with_isolated_home("entrypoints", "php-symfony", || {
            let root = temp_root("php-symfony");
            std::fs::write(
                root.join("ApiController.php"),
                "<?php\n#[Route(\"/api\")]\nclass ApiController extends AbstractController {\n  #[Get(\"/items\")]\n  public function list() {}\n}\n",
            )
            .expect("write ApiController.php");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "list" && e.role == RoleTag::RouteHandler),
                "Symfony #[Get] action must be a route_handler: {entrypoints:?}"
            );
            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "ApiController" && e.role == RoleTag::RouteHandler),
                "Symfony controller (#[Route] / AbstractController) must be a route_handler: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// A PHP controller extending a *fully-qualified* base class
    /// (`\Symfony\...\AbstractController`, normalized to a `/`-path) must still
    /// role-tag, via last-segment matching against the bare `AbstractController`
    /// rule.
    #[test]
    fn semantic_entrypoint_php_fully_qualified_base_class() {
        with_isolated_home("entrypoints", "php-fqcn", || {
            let root = temp_root("php-fqcn");
            std::fs::write(
                root.join("HomeController.php"),
                "<?php\nclass HomeController extends \\Symfony\\Bundle\\FrameworkBundle\\Controller\\AbstractController {\n  public function index() {}\n}\n",
            )
            .expect("write HomeController.php");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "HomeController" && e.role == RoleTag::RouteHandler),
                "controller extending a fully-qualified AbstractController must role-tag: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Call-based routes (Go gin/echo `.GET`, net/http `.HandleFunc`) surface
    /// via `detect_routes`, while a Python Flask `@app.route` — whose `Route`
    /// entity is emitted from the same decorator that already role-tags the
    /// handler in `detect` — must NOT, so it isn't double-counted.
    #[test]
    fn detect_routes_surfaces_call_based_and_skips_decorator_frameworks() {
        with_isolated_home("entrypoints", "detect-routes", || {
            let root = temp_root("detect-routes");
            std::fs::write(
                root.join("router.go"),
                "package main\nfunc setup(r Router) {\n\tr.GET(\"/users\", list)\n\tr.HandleFunc(\"/legacy\", h)\n}\n",
            )
            .expect("write router.go");
            std::fs::write(
                root.join("views.py"),
                "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/hello\")\ndef hello():\n    return \"hi\"\n",
            )
            .expect("write views.py");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let routes = detect_routes(&conn).expect("detect_routes computes");

            assert!(
                routes
                    .iter()
                    .any(|e| e.symbol == "GET /users" && e.file.ends_with("router.go")),
                "Go gin route must surface: {routes:?}"
            );
            assert!(
                routes.iter().any(|e| e.symbol.ends_with("/legacy")),
                "Go net/http route must surface (verb unknown): {routes:?}"
            );
            assert!(
                !routes.iter().any(|e| e.file.ends_with(".py")),
                "Python decorator routes must not surface via detect_routes: {routes:?}"
            );
            // ...but the Flask handler is still a normal entrypoint via detect.
            let eps = detect(&conn).expect("detect computes");
            assert!(
                eps.iter().any(|e| e.symbol == "hello"),
                "Flask handler must still be a role-tagged entrypoint: {eps:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Handler linking: `r.GET("/users", listUsers)` where `listUsers` is a
    /// real function must root the route at that function (`flow_root = true`,
    /// `entity_id` = the function's id) so it carries a real flow tree — the
    /// tree must descend into what the handler calls (`fetchAll`).
    #[test]
    fn detect_routes_resolves_named_handler_and_roots_flow_tree() {
        with_isolated_home("entrypoints", "route-handler", || {
            let root = temp_root("route-handler");
            std::fs::write(
                root.join("server.go"),
                "package main\nfunc fetchAll() {}\nfunc listUsers() { fetchAll() }\nfunc setup(r Router) {\n\tr.GET(\"/users\", listUsers)\n}\n",
            )
            .expect("write server.go");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let handler_id: i64 = conn
                .query_row(
                    "SELECT id FROM entities WHERE kind = ?1 AND name = 'listUsers'",
                    [crate::model::EntityKind::Function.as_i64()],
                    |r| r.get(0),
                )
                .expect("listUsers function id");

            let routes = detect_routes(&conn).expect("detect_routes computes");
            let route = routes
                .iter()
                .find(|e| e.symbol == "GET /users")
                .unwrap_or_else(|| panic!("route not found: {routes:?}"));
            assert!(
                route.flow_root,
                "resolved handler route must be a flow root"
            );
            assert_eq!(
                route.entity_id, handler_id,
                "route must be rooted at the listUsers function entity"
            );

            let trees = crate::query::flows::build_flows(&conn, std::slice::from_ref(route))
                .expect("flows");
            let json = trees[0].to_json().to_string();
            assert!(
                json.contains("fetchAll"),
                "handler flow tree must reach fetchAll: {json}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Kotlin Spring: the extractor now emits `Decorator` entities for Kotlin
    /// annotations, so a `@RestController` class and its `@GetMapping` method
    /// must both surface as route-handler entrypoints (end-to-end proof of the
    /// annotation→Decorator→detect pipeline for a JVM language that carried no
    /// decorator extraction before).
    #[test]
    fn semantic_entrypoint_kotlin_spring_controller() {
        with_isolated_home("entrypoints", "kotlin-spring", || {
            let root = temp_root("kotlin-spring");
            std::fs::write(
                root.join("UserController.kt"),
                "@RestController\n\
                 @RequestMapping(\"/users\")\n\
                 class UserController {\n\
                 \x20\x20\x20\x20@GetMapping(\"/{id}\")\n\
                 \x20\x20\x20\x20fun getUser(id: Int): String { return \"\" }\n\
                 }\n",
            )
            .expect("write UserController.kt");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "getUser" && e.role == RoleTag::RouteHandler),
                "Kotlin @GetMapping method must be a route_handler: {entrypoints:?}"
            );
            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "UserController" && e.role == RoleTag::RouteHandler),
                "Kotlin @RestController class must be a route_handler: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// FastAPI routes (`@app.get` / `@app.patch`) must be detected via the
    /// receiver-agnostic suffix rules, and a `@patch` (unittest.mock) in a
    /// test file must NOT be — both because test files are excluded and
    /// because that would otherwise collide with the `patch` HTTP verb.
    #[test]
    fn semantic_entrypoint_fastapi_routes_and_excludes_test_file_patch() {
        with_isolated_home("entrypoints", "fastapi", || {
            let root = temp_root("fastapi");
            std::fs::write(
                root.join("api_routes.py"),
                "from fastapi import FastAPI\n\
                 app = FastAPI()\n\n\
                 @app.get(\"/items\")\n\
                 def list_items():\n\
                 \x20\x20\x20\x20return []\n\n\
                 @app.patch(\"/items/{id}\")\n\
                 def update_item(id):\n\
                 \x20\x20\x20\x20return id\n",
            )
            .expect("write api_routes.py");
            // A test file using unittest.mock's `@patch` — its final segment is
            // the HTTP verb `patch`, so without the test-path exclusion it would
            // be a false-positive route handler.
            std::fs::write(
                root.join("test_api.py"),
                "from unittest.mock import patch\n\n\
                 @patch(\"api_routes.app\")\n\
                 def test_list(mock):\n\
                 \x20\x20\x20\x20pass\n",
            )
            .expect("write test_api.py");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            for handler in ["list_items", "update_item"] {
                assert!(
                    entrypoints
                        .iter()
                        .any(|e| e.symbol == handler && e.role == RoleTag::RouteHandler),
                    "FastAPI route {handler} must be a route_handler: {entrypoints:?}"
                );
            }
            assert!(
                !entrypoints.iter().any(|e| e.symbol == "test_list"),
                "a @patch-decorated function in a test file must not be an entrypoint: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Ruby emits no `Decorator` entities, so a Rails controller can only be
    /// role-tagged via its superclass (`< ApplicationController` → `Extends`)
    /// and a Sidekiq worker via its mixin (`include Sidekiq::Job` →
    /// `Implements`). Both signals must reach the base-class matcher.
    #[test]
    fn semantic_entrypoint_ruby_rails_controller_and_sidekiq_worker() {
        with_isolated_home("entrypoints", "ruby-rails", || {
            let root = temp_root("ruby-rails");
            std::fs::write(
                root.join("users_controller.rb"),
                "class UsersController < ApplicationController\n  def index\n  end\nend\n",
            )
            .expect("write users_controller.rb");
            std::fs::write(
                root.join("hard_worker.rb"),
                "class HardWorker\n  include Sidekiq::Job\n  def perform\n  end\nend\n",
            )
            .expect("write hard_worker.rb");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "UsersController" && e.role == RoleTag::RouteHandler),
                "Rails controller (extends ApplicationController) must be a route_handler: {entrypoints:?}"
            );
            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "HardWorker" && e.role == RoleTag::BackgroundJob),
                "Sidekiq worker (include Sidekiq::Job) must be a background_job: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Route method+path plumbing (annotation/decorator frameworks): a handler
    /// keeps its function name as `symbol` (still a valid `explore` target /
    /// flow root) but now carries the HTTP `method` and full `path`, combining
    /// the class-level base path (`@RequestMapping`/`[Route]`) with the
    /// method-level path. Covers FastAPI (module-level, no prefix), Spring
    /// (Java), ASP.NET MVC (C#), and Kotlin Spring in one repo.
    #[test]
    fn route_method_and_path_captured_for_annotation_handlers() {
        with_isolated_home("entrypoints", "route-method-path", || {
            let root = temp_root("route-method-path");
            std::fs::write(
                root.join("api.py"),
                "from fastapi import FastAPI\n\
                 app = FastAPI()\n\n\
                 @app.get(\"/items\")\n\
                 def list_items():\n\
                 \x20\x20\x20\x20return []\n\n\
                 @app.post(\"/items\")\n\
                 def create_item(x):\n\
                 \x20\x20\x20\x20return x\n",
            )
            .expect("write api.py");
            std::fs::write(
                root.join("UserController.java"),
                "@RestController\n\
                 @RequestMapping(\"/users\")\n\
                 public class UserController {\n\
                 \x20\x20\x20\x20@GetMapping(\"/{id}\")\n\
                 \x20\x20\x20\x20public String getUser(int id) { return \"\"; }\n\
                 \x20\x20\x20\x20@PostMapping\n\
                 \x20\x20\x20\x20public String create() { return \"\"; }\n\
                 }\n",
            )
            .expect("write UserController.java");
            std::fs::write(
                root.join("CatalogController.cs"),
                "using Microsoft.AspNetCore.Mvc;\n\n\
                 [ApiController]\n\
                 [Route(\"api/catalog\")]\n\
                 public class CatalogController : ControllerBase\n\
                 {\n\
                 \x20\x20\x20\x20[HttpGet(\"{id}\")]\n\
                 \x20\x20\x20\x20public int GetById(int id) { return id; }\n\
                 }\n",
            )
            .expect("write CatalogController.cs");
            std::fs::write(
                root.join("PetController.kt"),
                "@RestController\n\
                 @RequestMapping(\"/pets\")\n\
                 class PetController {\n\
                 \x20\x20\x20\x20@GetMapping(\"/{id}\")\n\
                 \x20\x20\x20\x20fun getPet(id: Int): String { return \"\" }\n\
                 }\n",
            )
            .expect("write PetController.kt");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let eps = detect(&conn).expect("detect computes");
            let route = |symbol: &str| -> (Option<String>, Option<String>) {
                let e = eps
                    .iter()
                    .find(|e| e.symbol == symbol)
                    .unwrap_or_else(|| panic!("{symbol} entrypoint missing: {eps:?}"));
                (e.method.clone(), e.path.clone())
            };

            // FastAPI: module-level, no class prefix.
            assert_eq!(
                route("list_items"),
                (Some("GET".into()), Some("/items".into()))
            );
            assert_eq!(
                route("create_item"),
                (Some("POST".into()), Some("/items".into()))
            );
            // Spring: class @RequestMapping("/users") + method path; a bare
            // @PostMapping (no path) still yields the class prefix.
            assert_eq!(
                route("getUser"),
                (Some("GET".into()), Some("/users/{id}".into()))
            );
            assert_eq!(
                route("create"),
                (Some("POST".into()), Some("/users".into()))
            );
            // ASP.NET MVC: [Route("api/catalog")] + [HttpGet("{id}")].
            assert_eq!(
                route("GetById"),
                (Some("GET".into()), Some("/api/catalog/{id}".into()))
            );
            // Kotlin Spring: @RequestMapping("/pets") + @GetMapping("/{id}").
            assert_eq!(
                route("getPet"),
                (Some("GET".into()), Some("/pets/{id}".into()))
            );
            // The controller *class* entries carry no route (prefix only).
            for cls in ["UserController", "CatalogController", "PetController"] {
                let e = eps.iter().find(|e| e.symbol == cls).expect("class present");
                assert_eq!(
                    (e.method.as_deref(), e.path.as_deref()),
                    (None, None),
                    "class {cls} must not carry a route"
                );
            }

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// NestJS (TS): both the `@Controller('cats')` class (its decorator hangs
    /// off the wrapping `export_statement`) and the `@Get(':id')` method (its
    /// decorator is a preceding sibling in the class body, not a child) must be
    /// role-tagged, and the action must carry the combined `GET /cats/:id`.
    #[test]
    fn semantic_entrypoint_nestjs_controller_routes() {
        with_isolated_home("entrypoints", "nestjs", || {
            let root = temp_root("nestjs");
            std::fs::write(
                root.join("cats.controller.ts"),
                "@Controller('cats')\n\
                 export class CatsController {\n\
                 \x20\x20@Get(':id')\n\
                 \x20\x20findOne(id: string): string { return id; }\n\
                 \x20\x20@Post()\n\
                 \x20\x20create(): void {}\n\
                 }\n",
            )
            .expect("write cats.controller.ts");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let eps = detect(&conn).expect("detect computes");
            assert!(
                eps.iter()
                    .any(|e| e.symbol == "CatsController" && e.role == RoleTag::RouteHandler),
                "@Controller class must be a route_handler: {eps:?}"
            );
            let find_one = eps
                .iter()
                .find(|e| e.symbol == "findOne")
                .unwrap_or_else(|| panic!("findOne missing: {eps:?}"));
            assert_eq!(find_one.role, RoleTag::RouteHandler);
            assert_eq!(
                (find_one.method.as_deref(), find_one.path.as_deref()),
                (Some("GET"), Some("/cats/:id"))
            );
            let create = eps
                .iter()
                .find(|e| e.symbol == "create")
                .unwrap_or_else(|| panic!("create missing: {eps:?}"));
            assert_eq!(
                (create.method.as_deref(), create.path.as_deref()),
                (Some("POST"), Some("/cats"))
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn semantic_entrypoint_nestjs_excludes_services_but_keeps_middleware() {
        with_isolated_home("entrypoints", "nestjs-middleware", || {
            let root = temp_root("nestjs-middleware");
            std::fs::write(
                root.join("app.ts"),
                "@Injectable()\n\
                 export class UsersService {\n\
                 \x20\x20findOne(): string { return 'user'; }\n\
                 }\n\
                 \n\
                 @Injectable()\n\
                 export class LoggerMiddleware implements NestMiddleware {\n\
                 \x20\x20use(): void {}\n\
                 }\n",
            )
            .expect("write Nest services and middleware");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let eps = detect(&conn).expect("detect computes");
            assert!(
                !eps.iter().any(|e| e.symbol == "UsersService"),
                "ordinary @Injectable service must not be an entrypoint: {eps:?}"
            );
            assert!(
                eps.iter()
                    .any(|e| { e.symbol == "LoggerMiddleware" && e.role == RoleTag::Middleware }),
                "@Injectable class implementing NestMiddleware must remain listed: {eps:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Rails engines / namespaced controllers: a class extending Devise's
    /// `Devise::SessionsController`, an app-specific `Admin::BaseController`,
    /// or `ActionController::API` must all be route-handler entrypoints — the
    /// `*Controller` base-class suffix rule catches bases the exact rules miss.
    #[test]
    fn semantic_entrypoint_rails_engine_and_namespaced_controllers() {
        with_isolated_home("entrypoints", "rails-engine", || {
            let root = temp_root("rails-engine");
            std::fs::write(
                root.join("sessions_controller.rb"),
                "class Users::SessionsController < Devise::SessionsController\n  def create\n  end\nend\n",
            )
            .expect("write sessions");
            std::fs::write(
                root.join("admin_users_controller.rb"),
                "class Admin::UsersController < Admin::BaseController\n  def index\n  end\nend\n",
            )
            .expect("write admin");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let eps = detect(&conn).expect("detect computes");
            for ctrl in ["Users::SessionsController", "Admin::UsersController"] {
                assert!(
                    eps.iter()
                        .any(|e| e.symbol == ctrl && e.role == RoleTag::RouteHandler),
                    "{ctrl} must be a route_handler entrypoint: {eps:?}"
                );
            }

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    /// Regression (review fix): a stacked decorator (a non-role-tagged
    /// decorator listed before the role-tagging one) must not prevent the
    /// role-tagging decorator from matching — every decorator on the symbol
    /// is tried, not just the first one returned.
    #[test]
    fn semantic_entrypoint_matches_role_tag_decorator_even_when_stacked_after_another() {
        with_isolated_home("entrypoints", "stacked-decorator", || {
            let root = temp_root("stacked-decorator");
            std::fs::write(
                root.join("app.py"),
                "def login_required(f):\n    return f\n\n@login_required\n@app.route(\"/hello\")\ndef hello():\n    return \"hi\"\n",
            )
            .expect("write app.py");

            crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
            let db = crate::db::path::repo_db_path(&root);
            let conn = Connection::open(&db).expect("open db");

            let entrypoints = detect(&conn).expect("detect computes");

            assert!(
                entrypoints
                    .iter()
                    .any(|e| e.symbol == "hello" && e.role == RoleTag::RouteHandler),
                "hello must still match route_handler despite the non-matching login_required decorator stacked above it: {entrypoints:?}"
            );

            let _ = std::fs::remove_file(&db);
            let _ = std::fs::remove_dir_all(&root);
        });
    }
}
