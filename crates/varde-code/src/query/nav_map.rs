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

/// Largest up-front allowance for one populated orientation section. The first
/// pass shares the available budget across populated sections and caps that
/// share here; priority spending receives every token left over.
const SECTION_RESERVE_TOKENS: usize = 200;

/// Depth/size caps for the per-flow summary nav_map embeds. `flows.rs` builds
/// each tree uncapped (thousands of nodes possible); nav_map is a fixed-cost
/// orientation, so it embeds only a bounded slice of each ranked flow — enough
/// to show the shape of the biggest call trees — and points at `explore` for
/// the full tree. `MAX_NODES` bounds total nodes across the whole summary;
/// `MAX_DEPTH` bounds how deep it descends.
const FLOW_SUMMARY_MAX_DEPTH: usize = 4;
const FLOW_SUMMARY_MAX_NODES: usize = 30;

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
    // The detector's ranking remains the semantic source of truth for flows
    // and subsystems. Only the orientation list alternates languages, so a
    // monorepo's first budgeted entries do not all come from one extension.
    let displayed_entrypoints = round_robin_entrypoints(&listed);
    let entrypoints_json: Vec<serde_json::Value> = displayed_entrypoints
        .iter()
        .map(|entrypoint| entrypoint.to_json())
        .collect();

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
    //
    // Rank the surviving flows biggest-first (by total node count) so the most
    // architecturally significant call trees lead the section, then embed a
    // *bounded summary* of each (see [`summarize_flow`]) rather than the full
    // uncapped tree — a single full flow can be thousands of nodes, far more
    // than the whole nav_map token budget, so unsummarized every flow was
    // dropped by [`budget_nav_map`]. Each summary carries the full `nodeCount`
    // and an `explore` follow-up handle for the complete tree.
    let mut flow_trees: Vec<&super::flows::FlowTree> = flows
        .iter()
        .filter(|t| !t.root.children.is_empty())
        .collect();
    flow_trees.sort_by_key(|t| std::cmp::Reverse(count_flow_nodes(&t.root)));
    // Collapse duplicate roots: an overloaded handler (C# MVC GET/POST action
    // pair, `EnableAuthenticator()` + `EnableAuthenticator(model)`) is two
    // distinct entities sharing one (file, symbol), so it produced two
    // near-identical flow trees that read as noise. Keep only the largest per
    // (file, symbol) — `sort_by_key` above is stable and descending, so the
    // first occurrence retained is the biggest tree.
    let mut seen_roots: std::collections::HashSet<(&str, &str)> = std::collections::HashSet::new();
    flow_trees.retain(|t| seen_roots.insert((t.root.file.as_str(), t.root.symbol.as_str())));
    let flows_json: Vec<serde_json::Value> = flow_trees.iter().map(|t| summarize_flow(t)).collect();

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

/// Interleave entrypoints by supported language while retaining detector rank
/// within each language. Language groups appear in the order first seen in
/// the ranked detector output.
fn round_robin_entrypoints(
    entrypoints: &[super::entrypoints::Entrypoint],
) -> Vec<super::entrypoints::Entrypoint> {
    let mut groups: Vec<(
        &str,
        std::collections::VecDeque<&super::entrypoints::Entrypoint>,
    )> = Vec::new();

    for entrypoint in entrypoints {
        let extension = entrypoint
            .file
            .rsplit_once('.')
            .map_or("", |(_, extension)| extension);
        let language = crate::parse::language_for_path(std::path::Path::new(&entrypoint.file))
            .map_or(extension, |language| crate::parse::language_name(&language));
        if let Some((_, group)) = groups.iter_mut().find(|(key, _)| *key == language) {
            group.push_back(entrypoint);
        } else {
            groups.push((language, std::collections::VecDeque::from([entrypoint])));
        }
    }

    let mut ordered = Vec::with_capacity(entrypoints.len());
    loop {
        let mut emitted = false;
        for (_, group) in &mut groups {
            if let Some(entrypoint) = group.pop_front() {
                ordered.push(entrypoint.clone());
                emitted = true;
            }
        }
        if !emitted {
            return ordered;
        }
    }
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
/// can eat the whole budget. A first pass gives every populated section a
/// bounded chance to retain one item. An item that cannot fit its share stays
/// omitted, so tiny budgets never force every section into the map.
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
    //    graph; cycles). `edges` is *not* reported as truncated here — the
    //    reserved-budget pass in step 4 owns edge truncation reporting so the
    //    `shown` count reflects what actually survived the token budget, not
    //    just this hard cap. Cycles participate in the later budget pass, so
    //    their disclosure is also deferred until their final shown count is
    //    known.
    let module_layers_total_edges = data["module_layers"]
        .get("edges")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if let Some(edges) = data["module_layers"]
        .get_mut("edges")
        .and_then(|v| v.as_array_mut())
    {
        edges.truncate(MODULE_LAYERS_EDGE_CAP);
    }
    let cycles_total = data["module_layers"]
        .get("cycles")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if cycles_total > MODULE_LAYERS_CYCLE_CAP
        && let Some(arr) = data["module_layers"]
            .get_mut("cycles")
            .and_then(|v| v.as_array_mut())
    {
        arr.truncate(MODULE_LAYERS_CYCLE_CAP);
    }

    // 3. Spend the token budget across the array sections in priority order.
    //    `module_layers.edges` participates in the first reserve pass, then
    //    receives priority overflow after the top-level array sections.
    let order: [(&str, usize, &str); 6] = [
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
        // flows ranks above symbols/hotspots: the biggest call trees are prime
        // orientation. Each item is a bounded summary (fixed cost), so a small
        // item cap surfaces the top few whole. Per-flow `more` handles point at
        // `explore` for the full tree; the section-level hint does too.
        (
            "flows",
            5,
            "code_query mode=explore on an entrypoint symbol (direction=outgoing) for its full call tree, or raise maxTokensEstimate to list more flows",
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
    ];
    let section_totals: [usize; 6] =
        std::array::from_fn(|index| data[order[index].0].as_array().map_or(0, Vec::len));

    // Charge only the module-layer object's structural overhead before
    // allocating items. Cycles are optional detail, so including their whole
    // payload here would let them consume every orientation-section reserve.
    let ml_more =
        "raise maxTokensEstimate; module_layers holds the resolved module-dependency graph";
    let fixed_cost = est_tokens(&serde_json::json!({ "edges": [], "cycles": [] }));
    let mut remaining = max_tokens.saturating_sub(fixed_cost);

    // First pass: each populated orientation section receives a bounded share.
    // A section only keeps its first item when that item fits the share. This
    // protects later sections without making them mandatory at tiny budgets.
    let populated = order
        .iter()
        .filter(|(name, _, _)| {
            data[*name]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        })
        .count()
        + usize::from(
            data["module_layers"]["edges"]
                .as_array()
                .is_some_and(|edges| !edges.is_empty()),
        );
    let section_reserve = remaining
        .checked_div(populated)
        .unwrap_or(0)
        .min(SECTION_RESERVE_TOKENS);
    let mut kept = [0usize; 6];
    for (index, (name, _, _)) in order.iter().enumerate() {
        if let Some(item) = data[*name].as_array().and_then(|items| items.first()) {
            let cost = est_tokens(item).max(1);
            if cost <= section_reserve {
                kept[index] = 1;
                remaining -= cost;
            }
        }
    }
    let mut kept_edges = 0usize;
    if let Some(edge) = data["module_layers"]["edges"]
        .as_array()
        .and_then(|edges| edges.first())
    {
        let cost = est_tokens(edge).max(1);
        if cost <= section_reserve {
            kept_edges = 1;
            remaining -= cost;
        }
    }

    // Priority overflow: retain the existing ranking and hard caps once every
    // populated section had its bounded opportunity.
    for (index, (name, item_cap, more)) in order.iter().enumerate() {
        if let Some(arr) = data[*name].as_array_mut() {
            let total = arr.len();
            while kept[index] < total && kept[index] < *item_cap {
                let cost = est_tokens(&arr[kept[index]]).max(1);
                if cost > remaining {
                    break;
                }
                remaining -= cost;
                kept[index] += 1;
            }
            if kept[index] < total {
                arr.truncate(kept[index]);
                truncated.insert(
                    (*name).to_string(),
                    serde_json::json!({ "shown": kept[index], "total": total, "more": *more }),
                );
            }
        }
    }

    // 4. Spend any remaining overflow on module edges after the established
    // priority order. Its fixed object overhead was already charged above.
    if let Some(edges) = data["module_layers"]
        .get("edges")
        .and_then(|v| v.as_array())
    {
        while kept_edges < edges.len() && kept_edges < MODULE_LAYERS_EDGE_CAP {
            let cost = est_tokens(&edges[kept_edges]).max(1);
            if cost > remaining {
                break;
            }
            remaining -= cost;
            kept_edges += 1;
        }
    }
    if let Some(edges) = data["module_layers"]
        .get_mut("edges")
        .and_then(|v| v.as_array_mut())
    {
        edges.truncate(kept_edges);
    }
    if kept_edges < module_layers_total_edges {
        truncated.insert(
            "module_layers.edges".to_string(),
            serde_json::json!({
                "shown": kept_edges,
                "total": module_layers_total_edges,
                "more": ml_more,
            }),
        );
    }

    // 5. Cycles are optional module-layer detail. Spend only overflow after
    // every orientation section and module edge received its reserve.
    let mut kept_cycles = 0usize;
    if let Some(cycles) = data["module_layers"]
        .get("cycles")
        .and_then(|v| v.as_array())
    {
        while kept_cycles < cycles.len() {
            let cost = est_tokens(&cycles[kept_cycles]).max(1);
            if cost > remaining {
                break;
            }
            remaining -= cost;
            kept_cycles += 1;
        }
    }
    if let Some(cycles) = data["module_layers"]
        .get_mut("cycles")
        .and_then(|v| v.as_array_mut())
    {
        cycles.truncate(kept_cycles);
    }
    if kept_cycles < cycles_total {
        truncated.insert(
            "module_layers.cycles".to_string(),
            serde_json::json!({
                "shown": kept_cycles,
                "total": cycles_total,
                "more": ml_more,
            }),
        );
    }

    // Item estimates omit JSON keys, arrays, and the guide itself. Enforce the
    // requested limit against the final rendered envelope by shedding the
    // lowest-priority retained item until it fits. If even the all-empty shape
    // plus its honest truncation guide cannot fit, report that structural lower
    // bound explicitly instead of claiming a false budget guarantee.
    let mut guide = nav_map_guide(max_tokens, &truncated, None);
    loop {
        let mut rendered = data.clone();
        rendered["guide"] = guide.clone();
        if est_tokens(&rendered) <= max_tokens {
            return guide;
        }

        let mut removed = false;
        for (path, total, more) in [
            ("cycles", cycles_total, ml_more),
            ("edges", module_layers_total_edges, ml_more),
        ] {
            let shown = {
                let items = data["module_layers"]
                    .get_mut(path)
                    .and_then(|v| v.as_array_mut());
                items.and_then(|items| items.pop().map(|_| items.len()))
            };
            if let Some(shown) = shown {
                truncated.insert(
                    format!("module_layers.{path}"),
                    serde_json::json!({ "shown": shown, "total": total, "more": more }),
                );
                removed = true;
                break;
            }
        }
        if !removed {
            for index in (0..order.len()).rev() {
                let (name, _, more) = order[index];
                let shown = data[name]
                    .as_array_mut()
                    .and_then(|items| items.pop().map(|_| items.len()));
                if let Some(shown) = shown {
                    truncated.insert(
                        name.to_string(),
                        serde_json::json!({
                            "shown": shown,
                            "total": section_totals[index],
                            "more": more,
                        }),
                    );
                    removed = true;
                    break;
                }
            }
        }
        if !removed {
            let mut minimum = est_tokens(&rendered);
            loop {
                guide = nav_map_guide(max_tokens, &truncated, Some(minimum));
                let mut minimum_rendered = data.clone();
                minimum_rendered["guide"] = guide.clone();
                let actual = est_tokens(&minimum_rendered);
                if actual == minimum {
                    return guide;
                }
                minimum = actual;
            }
        }
        guide = nav_map_guide(max_tokens, &truncated, None);
    }
}

fn nav_map_guide(
    max_tokens: usize,
    truncated: &serde_json::Map<String, serde_json::Value>,
    minimum_budget_tokens: Option<usize>,
) -> serde_json::Value {
    let mut guide = serde_json::Map::new();
    guide.insert("budgetTokens".to_string(), serde_json::json!(max_tokens));
    guide.insert(
        "truncated".to_string(),
        serde_json::Value::Object(truncated.clone()),
    );
    if let Some(minimum) = minimum_budget_tokens {
        guide.insert(
            "minimumBudgetTokens".to_string(),
            serde_json::json!(minimum),
        );
    }
    serde_json::Value::Object(guide)
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

    let mut named = super::subsystems::name_clusters(&communities, &role_tags);
    // Rank by significance so the budget's item cap surfaces the repo's biggest
    // real domains, not an arbitrary slice. `by_community` above is a HashMap,
    // so without this the emitted order — and thus which subsystems survive the
    // cap — is non-deterministic; on a large repo (a .NET repo yields ~275
    // named communities) that means orientation showed a random 20 instead of
    // the top ones. Sort by member count desc, then name, then id for a stable
    // order.
    named.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
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

/// Total number of nodes in a flow tree (the ranking metric — bigger trees
/// reach more of the codebase and lead the section). Backref/leaf nodes count
/// as one; `children` is empty on both.
fn count_flow_nodes(node: &super::flows::FlowNode) -> usize {
    1 + node.children.iter().map(count_flow_nodes).sum::<usize>()
}

/// Embed one flow as a bounded, self-describing summary: the entrypoint, its
/// file, the full `nodeCount`, a depth/size-capped `root` tree, and a `more`
/// handle naming the `explore` query that returns the complete tree. Bounding
/// here (not in `flows.rs`) keeps flows.rs the full-fidelity source while
/// nav_map stays fixed-cost.
fn summarize_flow(tree: &super::flows::FlowTree) -> serde_json::Value {
    let node_count = count_flow_nodes(&tree.root);
    // Whole-summary node budget, root included.
    let mut budget = FLOW_SUMMARY_MAX_NODES.saturating_sub(1);
    let root = summarize_flow_node(&tree.root, 0, &mut budget);
    let more = format!(
        "code_query mode=explore {{\"input\":\"{}\",\"direction\":\"outgoing\"}} for the full call tree",
        tree.entrypoint
    );
    serde_json::json!({
        "entrypoint": tree.entrypoint,
        "file": tree.root.file,
        "nodeCount": node_count,
        "root": root,
        "more": more,
    })
}

/// Recursively serialize a flow node to bounded JSON, spending a shared node
/// `budget` and stopping at [`FLOW_SUMMARY_MAX_DEPTH`]. A `backref_to` node
/// (cycle back-edge) is emitted as a `recurses` leaf without descending, so a
/// cyclic call graph can't loop. When children are dropped (depth cap, node
/// budget, or a mix), the count is reported inline as `childrenOmitted` so the
/// renderer and JSON consumers know the tree continues.
fn summarize_flow_node(
    node: &super::flows::FlowNode,
    depth: usize,
    budget: &mut usize,
) -> serde_json::Value {
    let mut obj = serde_json::json!({ "symbol": node.symbol, "file": node.file });
    if node.backref_to.is_some() {
        obj["recurses"] = serde_json::json!(true);
        return obj;
    }
    if node.children.is_empty() {
        return obj;
    }
    // Collapse repeated sibling backrefs to the same target. A dispatch-style
    // parent (a big match/switch calling one callee from many arms) otherwise
    // emits the same `(↑ recurses)` leaf N times, spending the summary's fixed
    // node budget on zero-information restatements and crowding out distinct
    // calls. Distinct children and first expansions are untouched; only
    // provable duplicate backrefs collapse, into one node carrying
    // `recursesCount`. `total_children` becomes the distinct count, so the
    // `childrenOmitted` accounting reflects what a reader would expect to see.
    let mut ordered: Vec<(&super::flows::FlowNode, usize)> = Vec::new();
    let mut backref_pos: HashMap<i64, usize> = HashMap::new();
    for child in &node.children {
        if let Some(br) = &child.backref_to {
            if let Some(&pos) = backref_pos.get(&br.entity_id) {
                ordered[pos].1 += 1;
                continue;
            }
            backref_pos.insert(br.entity_id, ordered.len());
        }
        ordered.push((child, 1));
    }
    let total_children = ordered.len();
    if depth + 1 >= FLOW_SUMMARY_MAX_DEPTH {
        obj["childrenOmitted"] = serde_json::json!(total_children);
        return obj;
    }
    let mut kids = Vec::new();
    for (child, count) in &ordered {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let mut child_json = summarize_flow_node(child, depth + 1, budget);
        if *count > 1 {
            child_json["recursesCount"] = serde_json::json!(count);
        }
        kids.push(child_json);
    }
    if kids.len() < total_children {
        obj["childrenOmitted"] = serde_json::json!(total_children - kids.len());
    }
    obj["children"] = serde_json::json!(kids);
    obj
}

#[cfg(test)]
mod nav_map_tests {
    use super::*;
    use crate::extract::langs::role_tags::RoleTag;
    use crate::query::entrypoints::Entrypoint;

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

    use crate::query::flows::{FlowNode, FlowTree};
    use crate::query::test_support::test_support::with_isolated_home;

    fn leaf(symbol: &str) -> FlowNode {
        FlowNode {
            entity_id: 0,
            file: format!("src/{symbol}.rs"),
            symbol: symbol.to_string(),
            children: Vec::new(),
            backref_to: None,
        }
    }

    /// `summarize_flow` reports the *full* node count for ranking, but embeds
    /// only a depth/size-bounded slice of the tree plus an `explore` follow-up
    /// handle — so one huge flow can't blow nav_map's fixed cost.
    #[test]
    fn summarize_flow_bounds_tree_but_reports_full_size() {
        // A wide root: far more children than FLOW_SUMMARY_MAX_NODES allows.
        let children: Vec<FlowNode> = (0..100).map(|i| leaf(&format!("c{i}"))).collect();
        let total = 1 + children.len();
        let tree = FlowTree {
            entrypoint: "handler".to_string(),
            root: FlowNode {
                entity_id: 1,
                file: "src/main.rs".to_string(),
                symbol: "handler".to_string(),
                children,
                backref_to: None,
            },
        };

        let summary = summarize_flow(&tree);

        // Full size is preserved for ranking, even though the tree is trimmed.
        assert_eq!(summary["nodeCount"], serde_json::json!(total));
        // The embedded tree is bounded: fewer children than the original 100.
        let shown = summary["root"]["children"].as_array().unwrap().len();
        assert!(
            shown <= FLOW_SUMMARY_MAX_NODES,
            "embedded tree must be bounded to the node budget (< the original 100): shown={shown}"
        );
        // The drop is reported inline so the reader knows it continues.
        assert_eq!(
            summary["root"]["childrenOmitted"],
            serde_json::json!(100 - shown)
        );
        // The follow-up handle names the explore query for the full tree.
        let more = summary["more"].as_str().unwrap();
        assert!(
            more.contains("explore") && more.contains("handler"),
            "{more}"
        );
    }

    /// Repeated sibling backrefs to the same target collapse into one node
    /// carrying `recursesCount`, so a dispatch-style parent doesn't spend the
    /// summary budget on identical `(↑ recurses)` leaves. Distinct children and
    /// the first full expansion of the target are preserved.
    #[test]
    fn summarize_flow_collapses_repeated_sibling_backrefs() {
        // A parent that calls `dispatch` (entity 7) from 20 arms — the first is
        // a full expansion, the rest are backrefs to it — plus one distinct
        // call to `other`.
        fn backref_to(entity_id: i64, symbol: &str) -> FlowNode {
            FlowNode {
                entity_id,
                file: format!("src/{symbol}.rs"),
                symbol: symbol.to_string(),
                children: Vec::new(),
                backref_to: Some(crate::query::flows::Backref {
                    entrypoint: "root".to_string(),
                    file: format!("src/{symbol}.rs"),
                    symbol: symbol.to_string(),
                    entity_id,
                }),
            }
        }
        let mut children = vec![other_leaf(9, "other")];
        for _ in 0..20 {
            children.push(backref_to(7, "dispatch"));
        }
        let tree = FlowTree {
            entrypoint: "root".to_string(),
            root: FlowNode {
                entity_id: 1,
                file: "src/main.rs".to_string(),
                symbol: "root".to_string(),
                children,
                backref_to: None,
            },
        };

        let summary = summarize_flow(&tree);
        let kids = summary["root"]["children"].as_array().unwrap();
        // 21 raw children collapse to 2 distinct: `other` + one `dispatch`.
        assert_eq!(kids.len(), 2, "sibling backrefs collapsed: {kids:?}");
        let dispatch = kids
            .iter()
            .find(|k| k["symbol"] == serde_json::json!("dispatch"))
            .expect("dispatch node present");
        assert_eq!(
            dispatch["recursesCount"],
            serde_json::json!(20),
            "collapsed backref carries its multiplier: {dispatch}"
        );
        // The distinct child is untouched (no bogus multiplier).
        let other = kids
            .iter()
            .find(|k| k["symbol"] == serde_json::json!("other"))
            .expect("other node present");
        assert!(other.get("recursesCount").is_none(), "{other}");
    }

    fn other_leaf(entity_id: i64, symbol: &str) -> FlowNode {
        FlowNode {
            entity_id,
            file: format!("src/{symbol}.rs"),
            symbol: symbol.to_string(),
            children: Vec::new(),
            backref_to: None,
        }
    }

    #[test]
    fn entrypoints_round_robin_source_extensions_preserving_rank() {
        let entrypoint = |entity_id, file: &str, symbol: &str| Entrypoint {
            entity_id,
            file: file.to_string(),
            symbol: symbol.to_string(),
            role: RoleTag::RouteHandler,
            flow_root: true,
            method: None,
            path: None,
        };
        let ranked = vec![
            entrypoint(1, "src/rust_first.rs", "rust_first"),
            entrypoint(2, "src/rust_second.rs", "rust_second"),
            entrypoint(3, "app/python_first.py", "python_first"),
            entrypoint(4, "web/typescript_first.ts", "typescript_first"),
            entrypoint(5, "app/python_second.py", "python_second"),
            entrypoint(6, "src/rust_third.rs", "rust_third"),
        ];

        let symbols: Vec<_> = round_robin_entrypoints(&ranked)
            .into_iter()
            .map(|entrypoint| entrypoint.symbol)
            .collect();

        assert_eq!(
            symbols,
            [
                "rust_first",
                "python_first",
                "typescript_first",
                "rust_second",
                "python_second",
                "rust_third",
            ]
        );
    }

    #[test]
    fn entrypoints_round_robin_groups_javascript_variants() {
        let entrypoint = |entity_id, file: &str, symbol: &str| Entrypoint {
            entity_id,
            file: file.to_string(),
            symbol: symbol.to_string(),
            role: RoleTag::RouteHandler,
            flow_root: true,
            method: None,
            path: None,
        };
        let ranked = vec![
            entrypoint(1, "web/first.js", "javascript_first"),
            entrypoint(2, "web/second.mjs", "javascript_second"),
            entrypoint(3, "web/third.cjs", "javascript_third"),
            entrypoint(4, "web/fourth.jsx", "javascript_fourth"),
            entrypoint(5, "api/app.py", "python_first"),
        ];

        let symbols: Vec<_> = round_robin_entrypoints(&ranked)
            .into_iter()
            .map(|entrypoint| entrypoint.symbol)
            .collect();

        assert_eq!(
            symbols,
            [
                "javascript_first",
                "python_first",
                "javascript_second",
                "javascript_third",
                "javascript_fourth",
            ]
        );
    }

    #[test]
    fn default_budget_keeps_later_entrypoint_languages_visible() {
        let entrypoint = |entity_id, file: String, symbol: String| Entrypoint {
            entity_id,
            file,
            symbol,
            role: RoleTag::RouteHandler,
            flow_root: true,
            method: None,
            path: None,
        };
        let mut ranked: Vec<_> = (0..1_000)
            .map(|id| {
                entrypoint(
                    id,
                    format!("crates/service/src/handler_{id}.rs"),
                    format!("rust_handler_{id}"),
                )
            })
            .collect();
        ranked.push(entrypoint(
            1_000,
            "services/api/app.py".to_string(),
            "python_handler".to_string(),
        ));
        ranked.push(entrypoint(
            1_001,
            "web/src/routes.ts".to_string(),
            "typescript_handler".to_string(),
        ));
        let entrypoints_json: Vec<_> = round_robin_entrypoints(&ranked)
            .iter()
            .map(Entrypoint::to_json)
            .collect();
        let mut data = serde_json::json!({
            "entrypoints": entrypoints_json,
            "foundational_files": [],
            "module_layers": {"edges": [], "cycles": []},
            "subsystems": [],
            "symbols": [],
            "flows": [],
            "hotspots": [],
        });

        budget_nav_map(&mut data, NAV_MAP_DEFAULT_MAX_TOKENS);

        let symbols: Vec<_> = data["entrypoints"]
            .as_array()
            .expect("entrypoints array")
            .iter()
            .map(|entrypoint| entrypoint["symbol"].as_str().expect("symbol"))
            .collect();
        assert!(
            symbols.len() < ranked.len(),
            "default budget truncates output"
        );
        assert_eq!(
            &symbols[..3],
            ["rust_handler_0", "python_handler", "typescript_handler"]
        );
    }

    /// Depth beyond `FLOW_SUMMARY_MAX_DEPTH` is cut with a `childrenOmitted`
    /// marker rather than descended, and `nodeCount` still counts the whole
    /// deep chain.
    #[test]
    fn summarize_flow_caps_depth() {
        // A single deep chain a > b > c > d > e > f.
        let mut node = leaf("f");
        for sym in ["e", "d", "c", "b", "a"] {
            let mut parent = leaf(sym);
            parent.children = vec![node];
            node = parent;
        }
        let tree = FlowTree {
            entrypoint: "a".to_string(),
            root: node,
        };

        let summary = summarize_flow(&tree);
        assert_eq!(summary["nodeCount"], serde_json::json!(6));

        // Walk the embedded chain; it must not exceed FLOW_SUMMARY_MAX_DEPTH
        // levels and the deepest rendered node must report an omitted child.
        let mut cur = &summary["root"];
        let mut depth = 1;
        while let Some(kids) = cur["children"].as_array() {
            if kids.is_empty() {
                break;
            }
            cur = &kids[0];
            depth += 1;
        }
        assert!(
            depth <= FLOW_SUMMARY_MAX_DEPTH,
            "embedded depth {depth} exceeds cap {FLOW_SUMMARY_MAX_DEPTH}"
        );
        assert!(
            cur.get("childrenOmitted").is_some(),
            "deepest rendered node flags the cut subtree: {cur}"
        );
    }

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

    /// Default-budget orientation keeps one useful HTTP route from each
    /// supported server framework. An ordinary Nest injectable service must
    /// not consume an entrypoint slot.
    #[test]
    fn default_budget_keeps_mixed_framework_routes_and_excludes_nest_service() {
        with_isolated_home(
            "nav-map",
            "mixed-framework-default-budget",
            default_budget_keeps_mixed_framework_routes_and_excludes_nest_service_inner,
        );
    }

    fn default_budget_keeps_mixed_framework_routes_and_excludes_nest_service_inner() {
        let root = temp_root("mixed-framework-default-budget");
        let mut nest_handlers = String::new();
        for index in 0..45 {
            nest_handlers.push_str(&format!(
                "    @Get('extra-{index}')\n    extra{index}(): string {{ return 'extra'; }}\n"
            ));
        }
        let mut cats_controller = String::from(
            "@Controller('cats')\n\
             export class CatsController {\n\
             \x20\x20@Get(':id')\n\
             \x20\x20aFindOne(): string { return 'cat'; }\n",
        );
        cats_controller.push_str(&nest_handlers);
        cats_controller.push_str("}\n");
        std::fs::write(root.join("cats.controller.ts"), cats_controller)
            .expect("write Nest controller");
        std::fs::write(
            root.join("cats.service.ts"),
            "@Injectable()\n\
             export class CatsService {\n\
             \x20\x20resolveCatFromCache(): string { return 'service'; }\n\
             }\n",
        )
        .expect("write Nest service");
        std::fs::write(
            root.join("UserController.java"),
            "@RestController\n\
             @RequestMapping(\"/users\")\n\
             public class UserController {\n\
             \x20\x20@GetMapping(\"/{id}\")\n\
             \x20\x20public String getUser(int id) { return \"\"; }\n\
             }\n",
        )
        .expect("write Spring controller");
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
        .expect("write ASP.NET controller");

        crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
        let data = nav_map(&serde_json::json!({ "repoRoot": root.to_str().unwrap() }))
            .expect("nav_map computes");
        let entrypoints = data["entrypoints"].as_array().expect("entrypoints array");

        for (symbol, method, path) in [
            ("aFindOne", "GET", "/cats/:id"),
            ("getUser", "GET", "/users/{id}"),
            ("GetById", "GET", "/api/catalog/{id}"),
        ] {
            assert!(
                entrypoints.iter().any(|entrypoint| {
                    entrypoint["symbol"] == symbol
                        && entrypoint["method"] == method
                        && entrypoint["path"] == path
                }),
                "missing {method} {path} route {symbol:?}: {entrypoints:?}"
            );
        }
        assert!(
            !entrypoints
                .iter()
                .any(|entrypoint| entrypoint["symbol"] == "CatsService"),
            "ordinary Nest service must not be an entrypoint: {entrypoints:?}"
        );
        assert!(
            !entrypoints
                .iter()
                .any(|entrypoint| entrypoint["symbol"] == "resolveCatFromCache"),
            "ordinary Nest service method must not be an entrypoint: {entrypoints:?}"
        );
        let entrypoints_truncation = &data["guide"]["truncated"]["entrypoints"];
        let shown = entrypoints_truncation["shown"]
            .as_u64()
            .expect("default map must truncate entrypoints") as usize;
        let total = entrypoints_truncation["total"]
            .as_u64()
            .expect("entrypoint truncation must include total") as usize;
        assert!(
            shown <= 40 && shown < total,
            "entrypoints must be capped and truncated: {entrypoints_truncation:?}"
        );

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

    /// Every populated orientation section gets a bounded first-item reserve
    /// before priority overflow spends the rest. This keeps later sections
    /// useful without forcing oversized items into a tiny map.
    #[test]
    fn budget_reserves_an_item_for_each_populated_orientation_section() {
        let entrypoints: Vec<_> = (0..3)
            .map(|i| {
                serde_json::json!({
                    "symbol": format!("entrypoint_{i}"),
                    "file": format!("src/entrypoint_{i}.rs"),
                    "role": "process_main",
                })
            })
            .collect();
        let foundational_files: Vec<_> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "file": format!("src/foundation_{i}.rs"),
                    "dependents": i,
                })
            })
            .collect();
        let subsystems: Vec<_> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "name": format!("subsystem_{i}"),
                    "members": [format!("src/subsystem_{i}.rs")],
                })
            })
            .collect();
        let flows: Vec<_> = (0..3)
            .map(|i| {
                serde_json::json!({
                    "entrypoint": format!("entrypoint_{i}"),
                    "root": {"symbol": format!("entrypoint_{i}")},
                })
            })
            .collect();
        let symbols: Vec<_> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "symbol": format!("symbol_{i}"),
                    "file": format!("src/symbol_{i}.rs"),
                })
            })
            .collect();
        let hotspots: Vec<_> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "file": format!("src/hotspot_{i}.rs"),
                    "score": i,
                })
            })
            .collect();
        let mut data = serde_json::json!({
            "entrypoints": entrypoints,
            "foundational_files": foundational_files,
            "module_layers": {"edges": [], "cycles": []},
            "subsystems": subsystems,
            "flows": flows,
            "symbols": symbols,
            "hotspots": hotspots,
        });

        let guide = budget_nav_map(&mut data, 1_200);

        for section in [
            "entrypoints",
            "foundational_files",
            "subsystems",
            "flows",
            "symbols",
            "hotspots",
        ] {
            assert!(
                !data[section].as_array().unwrap().is_empty(),
                "{section} must retain its bounded orientation reserve: {data}"
            );
        }
        if let Some(symbols) = guide["truncated"].get("symbols") {
            assert!(
                symbols["shown"].as_u64().unwrap() >= 1,
                "symbols must retain at least their reserve: {guide}"
            );
            assert_eq!(symbols["total"], serde_json::json!(5));
        }
        let retained_cost = [
            "entrypoints",
            "foundational_files",
            "subsystems",
            "flows",
            "symbols",
            "hotspots",
        ]
        .iter()
        .flat_map(|section| data[*section].as_array().unwrap())
        .map(est_tokens)
        .sum::<usize>()
            + est_tokens(&data["module_layers"]);
        assert!(
            retained_cost <= 1_200,
            "retained cost {retained_cost} exceeds budget"
        );
        data["guide"] = guide;
        assert!(
            est_tokens(&data) <= 1_200,
            "complete rendered map exceeds its budget: {data}"
        );
    }

    /// Regression (module_layers reservation): even when the higher-priority
    /// sections would exhaust the whole token budget, module_layers still
    /// surfaces its top edges instead of being starved to empty (the old
    /// all-or-nothing drop zeroed it on every non-trivial repo). The reserved
    /// allotment guarantees the architectural layering view is never silently
    /// dropped.
    #[test]
    fn module_layers_edges_survive_a_tight_budget() {
        // Priority sections large enough to blow the budget on their own.
        let founds: Vec<_> = (0..40)
            .map(|i| serde_json::json!({"file": format!("src/really/long/path/f{i}.rs"), "dependents": i}))
            .collect();
        let symbols: Vec<_> = (0..200)
            .map(|i| serde_json::json!({"symbol": format!("symbol_number_{i}"), "count": i}))
            .collect();
        // 15 real module-dependency edges.
        let edges: Vec<_> = (0..15)
            .map(|i| {
                serde_json::json!({
                    "from_module": format!("src/feature_{i}/endpoints"),
                    "to_module": format!("src/feature_{i}/core"),
                    "crossing_files": i + 3,
                })
            })
            .collect();
        let mut data = serde_json::json!({
            "entrypoints": [{"symbol": "main", "file": "src/main.rs", "role": "process_main"}],
            "foundational_files": founds,
            "module_layers": {"edges": edges, "cycles": []},
            "subsystems": [],
            "symbols": symbols,
            "flows": [],
            "hotspots": [{"file": "src/h.rs", "score": 9}],
        });

        let guide = budget_nav_map(&mut data, 1500);

        let kept = data["module_layers"]["edges"].as_array().unwrap().len();
        assert!(
            kept > 0,
            "module_layers edges must survive a tight budget via the reservation, got {kept}"
        );
        // If any were trimmed, the cut is reported honestly with a non-zero
        // shown count and a follow-up hint.
        if kept < 15 {
            let t = &guide["truncated"]["module_layers.edges"];
            assert_eq!(t["shown"], serde_json::json!(kept));
            assert_eq!(t["total"], serde_json::json!(15));
            assert!(t["more"].as_str().unwrap().contains("maxTokensEstimate"));
        }
    }

    /// Cycles are optional module-layer detail. Their payload must not consume
    /// the first-item reserve that keeps every populated orientation section
    /// visible under a tight budget.
    #[test]
    fn budgeted_cycles_do_not_starve_section_reserves() {
        let long_cycles: Vec<_> = (0..MODULE_LAYERS_CYCLE_CAP)
            .map(|i| serde_json::json!(format!("cycle-{i}-{}", "x".repeat(1_000))))
            .collect();
        let mut data = serde_json::json!({
            "entrypoints": [{"symbol": "main", "file": "src/main.rs"}],
            "foundational_files": [{"file": "src/core.rs", "dependents": 1}],
            "module_layers": {"edges": [{"from": "src/main.rs", "to": "src/core.rs"}], "cycles": long_cycles},
            "subsystems": [{"id": 1, "name": "core", "members": ["src/core.rs"]}],
            "flows": [{"entrypoint": "main", "root": {"symbol": "main"}}],
            "symbols": [{"symbol": "Core", "file": "src/core.rs"}],
            "hotspots": [{"file": "src/core.rs", "score": 1}],
        });

        let guide = budget_nav_map(&mut data, 1_200);

        for section in [
            "entrypoints",
            "foundational_files",
            "subsystems",
            "flows",
            "symbols",
            "hotspots",
        ] {
            assert!(
                !data[section].as_array().unwrap().is_empty(),
                "{section} lost its reserve to cycles: {data}"
            );
        }
        assert!(
            data["module_layers"]["edges"].as_array().unwrap().len() > 0,
            "module edges lost their reserve to cycles: {data}"
        );
        assert_eq!(
            guide["truncated"]["module_layers.cycles"]["total"],
            serde_json::json!(20)
        );
    }

    /// A normal budget covers the complete rendered map, including its guide.
    /// Below the structural lower bound, the guide explicitly reports that
    /// lower bound instead of falsely claiming the requested limit was met.
    #[test]
    fn budget_covers_rendered_map_or_reports_the_structural_lower_bound() {
        let mut data = serde_json::json!({
            "entrypoints": [{"symbol": "main", "file": "src/main.rs"}],
            "foundational_files": [],
            "module_layers": {"edges": [], "cycles": []},
            "subsystems": [],
            "flows": [],
            "symbols": [],
            "hotspots": [],
        });

        let guide = budget_nav_map(&mut data, 1);
        data["guide"] = guide;

        let minimum = data["guide"]["minimumBudgetTokens"]
            .as_u64()
            .expect("below the structural minimum, guide reports the lower bound")
            as usize;
        assert!(minimum > 1);
        assert_eq!(minimum, est_tokens(&data));
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

    /// Subsystems are emitted biggest-first so the budget's item cap surfaces
    /// the repo's largest real domains, not an arbitrary HashMap-order slice.
    /// A 3-file community must precede a 2-file one regardless of community id.
    #[test]
    fn subsystems_section_ranks_larger_domains_first() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::db::schema_ddl()).expect("schema");

        // Community 5: a small 2-file domain. Community 9: a larger 3-file one.
        // Higher id given to the bigger community so id order can't accidentally
        // produce the expected result.
        let seed = [
            (5, "src/small/a.rs"),
            (5, "src/small/b.rs"),
            (9, "src/big/x.rs"),
            (9, "src/big/y.rs"),
            (9, "src/big/z.rs"),
        ];
        for (cid, path) in seed {
            conn.execute("INSERT INTO files (path) VALUES (?1)", [path])
                .expect("insert file");
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO community_members (community_id, file_id) VALUES (?1, ?2)",
                rusqlite::params![cid, file_id],
            )
            .expect("insert member");
        }

        let result = subsystems_section(&conn, &[]).expect("subsystems computes");
        let subs = result.as_array().expect("array");
        assert_eq!(subs.len(), 2, "both multi-file domains named: {result}");
        assert_eq!(
            subs[0]["members"].as_array().unwrap().len(),
            3,
            "larger domain ranks first: {result}"
        );
        assert_eq!(
            subs[1]["members"].as_array().unwrap().len(),
            2,
            "smaller domain ranks second: {result}"
        );
    }
}
