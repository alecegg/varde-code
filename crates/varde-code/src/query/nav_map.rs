//! `nav_map` — assembled session-start repo orientation output.
//!
//! Wires the 7 standalone nav-map section modules built by earlier tasks
//! (see `memory-bank/working/plans/2026-09-03-nav-map-draft/plan.md`) into
//! one query mode: `entrypoints`, `foundational_files`, `module_layers`,
//! `subsystems`, `symbols`, `flows`, `hotspots`. No section logic lives
//! here — this module only opens the db connection, calls each section's
//! existing public function, and assembles the results into one JSON
//! object.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::extract::langs::role_tags::RoleTag;
use crate::resolve::Community;

use super::{ApiError, db_err, open_db};

/// Assemble the full nav_map output: all 7 sections in one JSON object.
///
/// Inputs: `repoRoot`/`dbPath` only (same convention as every other mode).
/// Output: `{"entrypoints", "foundational_files", "module_layers",
/// "subsystems", "symbols", "flows", "hotspots"}`.
pub fn nav_map(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    super::freshen_for_mode("nav_map", input)?;
    let conn = open_db(input)?;

    let entrypoints = super::entrypoints::detect(&conn)?;
    let entrypoints_json: Vec<serde_json::Value> =
        entrypoints.iter().map(|e| e.to_json()).collect();

    let foundational_files = super::foundational_files::leaderboard(&conn, None)?;

    let module_layers_json = module_layers_section(&conn)?;

    let subsystems_json = subsystems_section(&conn, &entrypoints)?;

    // Reuse the entrypoint set already computed above — the symbols
    // leaderboard only needs it to dedup entrypoint symbols out, and
    // recomputing `entrypoints::detect` here was doubling nav_map's cost.
    let entrypoint_ids: std::collections::HashSet<i64> =
        entrypoints.iter().map(|e| e.entity_id).collect();
    let symbols = super::symbols_section::leaderboard(&conn, &entrypoint_ids, None)?;

    let flows = super::flows::build_flows(&conn, &entrypoints)?;
    let flows_json: Vec<serde_json::Value> = flows.iter().map(|t| t.to_json()).collect();

    let hotspots = super::mapping::hotspots_on(&conn)?;

    Ok(serde_json::json!({
        "entrypoints": entrypoints_json,
        "foundational_files": foundational_files,
        "module_layers": module_layers_json,
        "subsystems": subsystems_json,
        "symbols": symbols,
        "flows": flows_json,
        "hotspots": hotspots,
    }))
}

/// File-level resolved edges as `(from_path, to_path)` pairs, the input
/// shape [`super::module_layers::compute_module_layers`] expects.
fn module_layers_section(conn: &Connection) -> Result<serde_json::Value, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT f1.path, f2.path
             FROM resolved_edges re
             JOIN files f1 ON f1.id = re.from_file_id
             JOIN files f2 ON f2.id = re.to_file_id
             WHERE re.resolved = 1 AND re.to_file_id IS NOT NULL",
        )
        .map_err(db_err)?;
    let pairs: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;

    let edge_refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let layers = super::module_layers::compute_module_layers(edge_refs);
    serde_json::to_value(&layers).map_err(|e| ApiError::new("serialize_error", e.to_string()))
}

/// Read the persisted Louvain partition (same `community_members` table the
/// `clusters` mode reads) and hand it to [`super::subsystems::name_clusters`]
/// with a per-file dominant-role-tag map derived from the already-computed
/// entrypoints (first role tag seen per file).
fn subsystems_section(
    conn: &Connection,
    entrypoints: &[super::entrypoints::Entrypoint],
) -> Result<serde_json::Value, ApiError> {
    let mut stmt = conn
        .prepare("SELECT cm.community_id, f.path FROM community_members cm JOIN files f ON f.id = cm.file_id")
        .map_err(db_err)?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;

    let mut by_community: HashMap<i64, Vec<String>> = HashMap::new();
    for (community_id, path) in rows {
        by_community.entry(community_id).or_default().push(path);
    }
    let communities: Vec<Community> = by_community
        .into_iter()
        .map(|(id, members)| Community {
            id: id as u32,
            members,
        })
        .collect();

    let mut role_tags: HashMap<String, RoleTag> = HashMap::new();
    for ep in entrypoints {
        role_tags.entry(ep.file.clone()).or_insert(ep.role);
    }

    let named = super::subsystems::name_clusters(&communities, &role_tags);
    Ok(serde_json::json!(
        named
            .into_iter()
            .map(|c| serde_json::json!({
                "id": c.id,
                "name": c.name,
                "members": c.members,
            }))
            .collect::<Vec<_>>()
    ))
}

#[cfg(test)]
mod nav_map_tests {
    use super::*;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "varde-nav-map-{label}-{}-{}",
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

    /// AC1: `nav_map` against a built fixture repo returns all 7 section
    /// keys, no error.
    #[test]
    fn nav_map_returns_all_seven_sections() {
        with_isolated_home(
            "nav-map",
            "all-sections",
            nav_map_returns_all_seven_sections_inner,
        );
    }

    fn nav_map_returns_all_seven_sections_inner() {
        let root = temp_root("all-sections");
        std::fs::write(
            root.join("app.py"),
            "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/hello\")\ndef hello():\n    return \"hi\"\n",
        )
        .expect("write app.py");

        crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");

        let input = serde_json::json!({ "repoRoot": root.to_str().unwrap() });
        let data = nav_map(&input).expect("nav_map computes");

        for key in [
            "entrypoints",
            "foundational_files",
            "module_layers",
            "subsystems",
            "symbols",
            "flows",
            "hotspots",
        ] {
            assert!(
                data.get(key).is_some(),
                "nav_map output missing section {key:?}: {data}"
            );
        }

        let db = crate::db::path::repo_db_path(&root);
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// AC2: a missing/broken index (repo path doesn't exist, so
    /// build-on-read auto-freshening itself fails) returns the uniform
    /// error envelope, not a panic or a bespoke shape.
    #[test]
    fn nav_map_missing_index_returns_error() {
        let root = temp_root("missing-index");
        let nonexistent = root.join("does-not-exist");
        let input = serde_json::json!({ "repoRoot": nonexistent.to_str().unwrap() });

        let result = nav_map(&input);
        assert!(
            result.is_err(),
            "missing/broken index must error, not succeed: {result:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
