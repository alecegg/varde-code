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

/// Cap for the nav_map `hotspots` section. nav_map is a session-start
/// orientation summary, so it surfaces the top risk hotspots rather than the
/// full ranked list (the standalone `hotspots` mode stays unbounded). Without
/// a cap this section listed every source file — 917 rows on a reference repo.
const HOTSPOTS_SECTION_LIMIT: usize = 50;

/// Cap for the nav_map `foundational_files` section, same orientation-summary
/// rationale as [`HOTSPOTS_SECTION_LIMIT`]: surface the most-depended-on files,
/// not the full fan-in leaderboard (562 rows on a reference repo). The
/// standalone `foundational_files` computation stays unbounded (`None`).
const FOUNDATIONAL_FILES_SECTION_LIMIT: usize = 50;

/// Cap for the nav_map `symbols` section. The symbols leaderboard is the
/// largest section (1333 rows on a reference repo); nav_map surfaces the top
/// symbols for orientation while the standalone `symbols` mode stays unbounded.
const SYMBOLS_SECTION_LIMIT: usize = 100;

/// Total token budget for the whole nav_map output (audit F1). nav_map is
/// injected at session start, so it must be a *fixed-cost* orientation, not
/// proportional to repo size — unbounded it reached 140K tokens on a
/// mainstream C# repo. Spent section-by-section in priority order by
/// [`budget_nav_map`]; overridable via the `maxTokensEstimate` input, mirroring
/// `context_pack`.
const NAV_MAP_DEFAULT_MAX_TOKENS: usize = 8000;

/// Per-subsystem member-list cap. One community can list hundreds of files
/// (C#: 298 subsystems, some huge), so each subsystem shows its first few
/// members plus a `membersOmitted` count rather than the full roster.
const SUBSYSTEM_MEMBER_CAP: usize = 8;

/// Caps for the `module_layers` object's inner arrays — it serializes the
/// whole resolved import graph (`edges`) and every cycle, which alone reached
/// 222 KB on the C# repo.
const MODULE_LAYERS_EDGE_CAP: usize = 50;
const MODULE_LAYERS_CYCLE_CAP: usize = 20;

/// Assemble the full nav_map output: all 7 sections in one JSON object.
///
/// Inputs: `repoRoot`/`dbPath` only (same convention as every other mode).
/// Output: `{"entrypoints", "foundational_files", "module_layers",
/// "subsystems", "symbols", "flows", "hotspots"}`.
pub fn nav_map(input: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    super::freshen_for_mode("nav_map", input)?;
    let conn = open_db(input)?;

    // Role-tagged handlers (decorators/base classes) feed the flow trees —
    // they are the callable roots. Call-based routes (Go/Express/Laravel/...)
    // are registration sites with no outgoing calls, so they enrich the
    // entrypoint listing and subsystem role map but are excluded from flows.
    let entrypoints = super::entrypoints::detect(&conn)?;
    let routes = super::entrypoints::detect_routes(&conn)?;
    // Language process mains (`fn main`, `func main`, `static void Main`, ...) —
    // the CLI/binary entry points `detect` deliberately excludes as bootstrap.
    // They are flow roots, so they also seed the flow trees below.
    let process_mains = super::entrypoints::detect_process_mains(&conn)?;
    let listed: Vec<super::entrypoints::Entrypoint> = entrypoints
        .iter()
        .chain(routes.iter())
        .chain(process_mains.iter())
        .cloned()
        .collect();
    let entrypoints_json: Vec<serde_json::Value> = listed.iter().map(|e| e.to_json()).collect();

    let foundational_files =
        super::foundational_files::leaderboard(&conn, Some(FOUNDATIONAL_FILES_SECTION_LIMIT))?;

    let module_layers_json = module_layers_section(&conn)?;

    let subsystems_json = subsystems_section(&conn, &listed)?;

    // Reuse the entrypoint set already computed above — the symbols
    // leaderboard only needs it to dedup entrypoint symbols out, and
    // recomputing `entrypoints::detect` here was doubling nav_map's cost.
    let entrypoint_ids: std::collections::HashSet<i64> =
        listed.iter().map(|e| e.entity_id).collect();
    let symbols =
        super::symbols_section::leaderboard(&conn, &entrypoint_ids, Some(SYMBOLS_SECTION_LIMIT))?;

    // Flow roots: role-tagged handlers plus any call-based route that resolved
    // to a real handler function (`flow_root`). Path-only routes (no resolvable
    // handler) are excluded — they have no outgoing call edges.
    let flow_roots: Vec<super::entrypoints::Entrypoint> =
        listed.iter().filter(|e| e.flow_root).cloned().collect();
    let flows = super::flows::build_flows(&conn, &flow_roots)?;
    // F7: a flow tree whose root has no children is a pure restatement of the
    // entrypoint it wraps — it adds nothing over the `entrypoints` section.
    // Real call trees are sparse in most repos (call resolution rarely
    // produces outgoing edges for a detected handler), so unfiltered this
    // section was mostly single-node wrappers that duplicated `entrypoints`
    // at up to ~130 KB. Keep only genuine multi-node trees.
    let flows_json: Vec<serde_json::Value> = flows
        .iter()
        .filter(|t| !t.root.children.is_empty())
        .map(|t| t.to_json())
        .collect();

    let hotspots = super::mapping::hotspots_on(&conn, Some(HOTSPOTS_SECTION_LIMIT))?;

    let mut data = serde_json::json!({
        "entrypoints": entrypoints_json,
        "foundational_files": foundational_files,
        "module_layers": module_layers_json,
        "subsystems": subsystems_json,
        "symbols": symbols,
        "flows": flows_json,
        "hotspots": hotspots,
    });

    let max_tokens = input
        .get("maxTokensEstimate")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(NAV_MAP_DEFAULT_MAX_TOKENS);
    // Trim to the token budget in priority order and attach a `guide` that
    // tells the agent exactly what was cut and how to fetch the rest (F1).
    let guide = budget_nav_map(&mut data, max_tokens);
    data["guide"] = guide;
    Ok(data)
}

/// Rough token estimate (`chars / 4`), the same convention `context_pack`'s
/// budgeter and this audit use.
fn est_tokens(v: &serde_json::Value) -> usize {
    serde_json::to_string(v).map(|s| s.len() / 4).unwrap_or(0)
}

/// Trim the assembled nav_map to `max_tokens`, spending the budget across
/// sections in a fixed **priority order** (most valuable for orientation
/// first), and return a `guide` describing what was truncated (F1).
///
/// Priority rationale — an agent orienting in a repo wants, in order: where
/// execution starts (`entrypoints`), what everything depends on
/// (`foundational_files`), the architectural partition (`subsystems`), the
/// core interfaces (`symbols`), where the risk is (`hotspots`), the dependency
/// layering (`module_layers`), and last the call trees (`flows`, currently the
/// thinnest section). Each section also has a hard item cap so no single one
/// can eat the whole budget. There is no forced per-section minimum: a section
/// reached with an exhausted budget keeps 0 items (and is marked truncated),
/// which is what stops one multi-thousand-token `flows` call-tree from blowing
/// the whole map.
///
/// Discoverability: each truncated section reports `{shown, total, more}` where
/// `more` names the follow-up that returns the full data — a dedicated query
/// mode where one exists (`hotspots`, `filter_symbols`), otherwise re-running
/// nav_map with a larger `maxTokensEstimate`. Per-subsystem member truncation
/// is reported inline as `membersOmitted` on the subsystem.
fn budget_nav_map(data: &mut serde_json::Value, max_tokens: usize) -> serde_json::Value {
    let mut truncated = serde_json::Map::new();

    // 1. Cap each subsystem's member list (independent of the token budget:
    //    one community can list hundreds of files).
    if let Some(subs) = data["subsystems"].as_array_mut() {
        for s in subs.iter_mut() {
            let total = s["members"].as_array().map(|m| m.len()).unwrap_or(0);
            if total > SUBSYSTEM_MEMBER_CAP {
                if let Some(members) = s["members"].as_array_mut() {
                    members.truncate(SUBSYSTEM_MEMBER_CAP);
                }
                s["membersOmitted"] = serde_json::json!(total - SUBSYSTEM_MEMBER_CAP);
            }
        }
    }

    // 2. Cap the module_layers inner arrays (edges = the whole resolved import
    //    graph; cycles).
    for (key, cap) in [
        ("edges", MODULE_LAYERS_EDGE_CAP),
        ("cycles", MODULE_LAYERS_CYCLE_CAP),
    ] {
        let total = data["module_layers"]
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if total > cap {
            if let Some(arr) = data["module_layers"]
                .get_mut(key)
                .and_then(|v| v.as_array_mut())
            {
                arr.truncate(cap);
            }
            truncated.insert(
                format!("module_layers.{key}"),
                serde_json::json!({
                    "shown": cap,
                    "total": total,
                    "more": "raise maxTokensEstimate; module_layers holds the resolved import graph",
                }),
            );
        }
    }

    // 3. Spend the token budget across sections in priority order.
    let order: [(&str, usize, &str); 7] = [
        (
            "entrypoints",
            40,
            "raise maxTokensEstimate to list more entrypoints",
        ),
        (
            "foundational_files",
            25,
            "code_query dependents on a specific file, or raise maxTokensEstimate",
        ),
        (
            "subsystems",
            20,
            "raise maxTokensEstimate to list more subsystems",
        ),
        (
            "symbols",
            40,
            "code_query mode=filter_symbols for the full symbol list",
        ),
        (
            "hotspots",
            25,
            "code_query mode=hotspots for the full ranked list",
        ),
        ("module_layers", usize::MAX, "raise maxTokensEstimate"),
        ("flows", 15, "raise maxTokensEstimate to list more flows"),
    ];

    let mut remaining = max_tokens as isize;
    for (name, item_cap, more) in order {
        if let Some(arr) = data[name].as_array_mut() {
            let total = arr.len();
            let mut kept = 0usize;
            let mut used = 0usize;
            for item in arr.iter() {
                if kept >= item_cap {
                    break;
                }
                let cost = est_tokens(item).max(1);
                // Keep only what fits the remaining budget — no forced minimum.
                // Sections run high-priority first, so the important ones get
                // their items while budget is plentiful; a section reached with
                // an exhausted budget keeps 0 and is marked truncated. (This is
                // what stops one 5k-token `flows` call-tree from blowing the
                // whole map.)
                if (used + cost) as isize > remaining {
                    break;
                }
                used += cost;
                kept += 1;
            }
            if kept < total {
                arr.truncate(kept);
                truncated.insert(
                    name.to_string(),
                    serde_json::json!({ "shown": kept, "total": total, "more": more }),
                );
            }
            remaining -= used as isize;
        } else {
            // module_layers is an object; charge its (already-capped) cost and,
            // if it overruns, drop the heavy edge list but keep a pointer.
            let cost = est_tokens(&data[name]) as isize;
            if cost > remaining {
                let total_edges = data[name]
                    .get("edges")
                    .and_then(|e| e.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                if let Some(edges) = data[name].get_mut("edges") {
                    *edges = serde_json::json!([]);
                }
                truncated.insert(
                    format!("{name}.edges"),
                    serde_json::json!({
                        "shown": 0,
                        "total": total_edges,
                        "more": more,
                    }),
                );
            }
            remaining -= cost;
        }
    }

    serde_json::json!({ "budgetTokens": max_tokens, "truncated": truncated })
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
///
/// Test files are excluded here (via the authoritative `is_test_path`
/// generated column) so a test directory never forms or pads a subsystem —
/// matching the entrypoints, foundational-files, symbols and hotspots
/// sections. Generated/vendored exclusion stays in `name_clusters`.
fn subsystems_section(
    conn: &Connection,
    entrypoints: &[super::entrypoints::Entrypoint],
) -> Result<serde_json::Value, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT cm.community_id, f.path FROM community_members cm
             JOIN files f ON f.id = cm.file_id
             WHERE f.is_test_path = 0",
        )
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

    /// F1: `budget_nav_map` trims sections to the token budget in priority
    /// order, caps per-subsystem members inline, and reports every cut in a
    /// `guide.truncated` block with shown/total + a follow-up hint.
    #[test]
    fn budget_trims_low_priority_first_and_reports_truncation() {
        // 30 foundational files, a subsystem with 50 members, 40 symbols,
        // and a fat flows tree — far more than a tight budget allows.
        let founds: Vec<_> = (0..30)
            .map(|i| serde_json::json!({"file": format!("src/f{i}.rs"), "dependents": i}))
            .collect();
        let members: Vec<_> = (0..50).map(|i| format!("src/m{i}.rs")).collect();
        // Above the symbols item cap (40), so it is always truncated.
        let symbols: Vec<_> = (0..200)
            .map(|i| serde_json::json!({"symbol": format!("s{i}"), "count": i}))
            .collect();
        let mut data = serde_json::json!({
            "entrypoints": [{"symbol": "main", "file": "src/main.rs", "role": "process_main"}],
            "foundational_files": founds,
            "module_layers": {"edges": [], "cycles": []},
            "subsystems": [{"id": 1, "name": "core", "members": members}],
            "symbols": symbols,
            // One flows tree bigger than the whole budget, so it can never fit.
            "flows": [{"entrypoint": "main", "root": {"symbol": "main", "children":
                (0..1000).map(|i| serde_json::json!({"symbol": format!("child_symbol_{i}")})).collect::<Vec<_>>()}}],
            "hotspots": [{"file": "src/h.rs", "score": 9}],
        });

        let guide = budget_nav_map(&mut data, 1500);
        let trunc = &guide["truncated"];

        // High-priority entrypoints survive; low-priority flows is dropped
        // (its one item is huge and the budget is gone by the time it runs).
        assert_eq!(data["entrypoints"].as_array().unwrap().len(), 1);
        assert_eq!(data["flows"].as_array().unwrap().len(), 0);
        assert_eq!(trunc["flows"]["shown"], 0);
        assert!(trunc["flows"]["total"].as_u64().unwrap() >= 1);
        assert!(
            trunc["flows"]["more"]
                .as_str()
                .unwrap()
                .contains("maxTokensEstimate")
        );

        // Subsystem members are capped inline with a membersOmitted count.
        let sub = &data["subsystems"][0];
        assert_eq!(
            sub["members"].as_array().unwrap().len(),
            SUBSYSTEM_MEMBER_CAP
        );
        assert_eq!(
            sub["membersOmitted"],
            serde_json::json!(50 - SUBSYSTEM_MEMBER_CAP)
        );

        // symbols' follow-up points at the dedicated query mode.
        assert!(
            trunc["symbols"]["more"]
                .as_str()
                .unwrap()
                .contains("filter_symbols")
        );
        assert_eq!(guide["budgetTokens"], serde_json::json!(1500));
    }

    /// The subsystems section drops test-file community members via the
    /// `is_test_path` column, so a test directory never pads or forms a
    /// subsystem. `src/api/helper.test.ts` (a test path) is excluded while
    /// its sibling production files remain.
    #[test]
    fn subsystems_section_excludes_test_file_members() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::db::schema_ddl()).expect("schema");

        for path in [
            "src/api/users.rs",
            "src/api/orders.rs",
            "src/api/helper.test.ts",
        ] {
            conn.execute(
                "INSERT INTO files (path) VALUES (?1)",
                rusqlite::params![path],
            )
            .expect("insert file");
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO community_members (community_id, file_id) VALUES (1, ?1)",
                rusqlite::params![file_id],
            )
            .expect("insert community member");
        }

        let result = subsystems_section(&conn, &[]).expect("subsystems computes");
        let members: Vec<String> = result
            .as_array()
            .expect("array")
            .iter()
            .flat_map(|c| c["members"].as_array().expect("members array").clone())
            .map(|m| m.as_str().expect("member path").to_string())
            .collect();

        assert!(
            !members.iter().any(|m| m == "src/api/helper.test.ts"),
            "test file must be excluded from subsystem members: {members:?}"
        );
        assert!(
            members.iter().any(|m| m == "src/api/users.rs"),
            "production file should remain a member: {members:?}"
        );
    }
}
