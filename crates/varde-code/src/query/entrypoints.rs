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

use rusqlite::Connection;

use crate::extract::langs::role_tags::{self, RoleTag, RoleTagRule};
use crate::model::EntityKind;

use super::noise_filter::is_generated_or_vendored_path;
use super::{ApiError, db_err};

/// One detected semantic entrypoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entrypoint {
    pub entity_id: i64,
    pub file: String,
    pub symbol: String,
    pub role: RoleTag,
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
        }
    }
}

impl Entrypoint {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "entity_id": self.entity_id,
            "file": self.file,
            "symbol": self.symbol,
            "role": self.role.as_str(),
        })
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
            "SELECT e.id, e.file_id, f.path, e.name
             FROM entities e
             JOIN files f ON f.id = e.file_id
             WHERE e.kind IN (?1, ?2)",
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
                ))
            },
        )
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?
        .into_iter()
        .map(|(entity_id, file_id, path, name)| Candidate {
            entity_id,
            file_id,
            path,
            name,
            decorators: Vec::new(),
            base_classes: Vec::new(),
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

    let mut results = Vec::new();
    for mut candidate in candidates {
        if is_generated_or_vendored_path(&candidate.path) {
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

        results.push(Entrypoint {
            entity_id: candidate.entity_id,
            file: candidate.path,
            symbol: candidate.name,
            role,
        });
    }

    results.sort_by(|a, b| a.file.cmp(&b.file).then(a.symbol.cmp(&b.symbol)));
    Ok(results)
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
