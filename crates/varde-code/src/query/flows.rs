//! Flows: full reachable-tree walk from each semantic entrypoint through the
//! resolved call graph (nav-map "Flows" section — see
//! `memory-bank/working/plans/2026-09-03-nav-map-draft/plan.md`, "Sections
//! (output shape)" → "Flows", and the `flow`/`collapse-with-backref` Concept
//! definitions in `memory-bank/knowledge/definition/`).
//!
//! Reuses [`crate::resolve`]'s resolved call edges as persisted in the
//! `resolved_edges` table (`kind = Call`, `resolved = 1`,
//! `from_entity_id`/`to_entity_id` populated) rather than re-deriving the
//! call graph — the same table [`super::entrypoints::is_bootstrap_by_fan_asymmetry`]
//! reads.
//!
//! No depth/size cap (plan decision: "Flow-tree size" — JSON-primary output
//! is meant to be filtered/queried by the caller, not read whole). A node
//! reachable from more than one entrypoint's tree is fully expanded once (in
//! the first entrypoint's tree that reaches it, per the entrypoints'
//! deterministic order) and rendered as a collapse-with-backref node in
//! every subsequent tree that reaches it — no "shared utilities" hoisting
//! tier is built; collapse-with-backref is the only mechanism.
//!
//! Not yet wired into any nav-map CLI/dispatcher (`nav-map-dispatch-cli` is
//! a later task).

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use super::entrypoints::Entrypoint;
use super::{ApiError, db_err};

/// One node in a flow tree: either fully expanded (`backref_to == None`,
/// `children` populated by DFS over resolved call edges) or a
/// collapse-with-backref reference to a node already fully expanded under an
/// earlier entrypoint's tree (`backref_to == Some(..)`, `children` empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowNode {
    pub entity_id: i64,
    pub file: String,
    pub symbol: String,
    pub children: Vec<FlowNode>,
    pub backref_to: Option<Backref>,
}

/// Where a collapse-with-backref node's full expansion actually lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backref {
    /// Symbol of the entrypoint whose tree fully expanded this node.
    pub entrypoint: String,
    pub file: String,
    pub symbol: String,
    pub entity_id: i64,
}

/// One entrypoint's full reachable-tree walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowTree {
    pub entrypoint: String,
    pub root: FlowNode,
}

impl FlowNode {
    pub fn to_json(&self) -> serde_json::Value {
        match &self.backref_to {
            Some(b) => serde_json::json!({
                "entity_id": self.entity_id,
                "file": self.file,
                "symbol": self.symbol,
                "backref_to": {
                    "entrypoint": b.entrypoint,
                    "file": b.file,
                    "symbol": b.symbol,
                    "entity_id": b.entity_id,
                },
            }),
            None => serde_json::json!({
                "entity_id": self.entity_id,
                "file": self.file,
                "symbol": self.symbol,
                "children": self.children.iter().map(FlowNode::to_json).collect::<Vec<_>>(),
            }),
        }
    }
}

impl FlowTree {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "entrypoint": self.entrypoint,
            "root": self.root.to_json(),
        })
    }
}

/// Where a node has already been fully rendered, once cross-entrypoint state
/// is threaded across every tree built in this call.
struct RenderedRef {
    entrypoint: String,
    file: String,
    symbol: String,
}

/// (file path, symbol name) for one entity id. Only used by tests below —
/// production lookups go through [`CallGraph`], loaded once per `build_flows`
/// call rather than per node.
#[cfg(test)]
fn entity_info(conn: &Connection, entity_id: i64) -> Result<(String, String), ApiError> {
    conn.prepare_cached(
        "SELECT f.path, e.name FROM entities e JOIN files f ON f.id = e.file_id WHERE e.id = ?1",
    )
    .map_err(db_err)?
    .query_row([entity_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })
    .map_err(db_err)
}

/// In-memory call graph loaded once by [`CallGraph::load`] and shared across
/// every entrypoint's walk in `build_flows` — replaces the per-node
/// `entity_info` + `callees` query pair `expand` used to issue, which was an
/// O(nodes) round-trip cost inside a recursive descent (the same shape
/// `entrypoints::detect` was refactored away from).
struct CallGraph {
    /// entity_id -> (file path, symbol name)
    entity_info: HashMap<i64, (String, String)>,
    /// entity_id -> (file_id, name): the join key `resolved_edges`' call
    /// sites match on via `enclosing_function`/`file_id`
    /// ([`crate::resolve::resolve_calls`] stores the enclosing function by
    /// name, not id).
    entity_key: HashMap<i64, (i64, String)>,
    /// (file_id, enclosing function name) -> callee entity ids, in
    /// deterministic (`resolved_edges.id`) order.
    callees: HashMap<(i64, String), Vec<i64>>,
}

impl CallGraph {
    fn load(conn: &Connection) -> Result<Self, ApiError> {
        let mut entity_info = HashMap::new();
        let mut entity_key = HashMap::new();
        {
            let mut stmt = conn
                .prepare_cached("SELECT e.id, e.file_id, e.name, f.path FROM entities e JOIN files f ON f.id = e.file_id")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(db_err)?;
            for row in rows {
                let (id, file_id, name, path) = row.map_err(db_err)?;
                entity_info.insert(id, (path, name.clone()));
                entity_key.insert(id, (file_id, name));
            }
        }

        let call_kind = crate::resolve::EdgeKind::Call.as_i64();
        let mut callees: HashMap<(i64, String), Vec<i64>> = HashMap::new();
        {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT call_e.file_id, call_e.enclosing_function, re.to_entity_id
                     FROM resolved_edges re
                     JOIN entities call_e ON call_e.id = re.from_entity_id
                     WHERE re.kind = ?1 AND re.resolved = 1 AND re.to_entity_id IS NOT NULL
                     ORDER BY re.id",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map([call_kind], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(db_err)?;
            for row in rows {
                let (file_id, enclosing, to_id) = row.map_err(db_err)?;
                if let Some(name) = enclosing {
                    callees.entry((file_id, name)).or_default().push(to_id);
                }
            }
        }

        Ok(CallGraph {
            entity_info,
            entity_key,
            callees,
        })
    }

    fn info(&self, entity_id: i64) -> Result<(String, String), ApiError> {
        self.entity_info
            .get(&entity_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("entity {entity_id}")))
    }

    fn callees(&self, entity_id: i64) -> &[i64] {
        self.entity_key
            .get(&entity_id)
            .and_then(|k| self.callees.get(k))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Maximum call-chain depth `expand` will descend before rendering deeper
/// callees as leaves. Guards against stack overflow (an uncatchable abort) on
/// a pathologically deep acyclic call chain — cycles are already handled by
/// `path`, but a linear chain thousands deep is not. Well beyond any real
/// call graph's longest path.
const MAX_FLOW_DEPTH: u32 = 1000;

/// DFS over resolved call edges from `entity_id`, producing a full
/// reachable-tree node. `path` guards against call-graph cycles (a node
/// already on the current DFS path is not re-expanded, avoiding infinite
/// recursion); `rendered` tracks cross-entrypoint collapse-with-backref
/// state for nodes already fully expanded elsewhere.
fn expand(
    graph: &CallGraph,
    entity_id: i64,
    entrypoint: &str,
    rendered: &mut HashMap<i64, RenderedRef>,
    path: &mut HashSet<i64>,
    depth: u32,
) -> Result<FlowNode, ApiError> {
    let (file, symbol) = graph.info(entity_id)?;
    path.insert(entity_id);

    let mut children = Vec::new();
    for &callee_id in graph.callees(entity_id) {
        if path.contains(&callee_id) {
            // Call-graph cycle back onto the current DFS path: stop
            // expanding rather than recursing forever. Not a
            // collapse-with-backref case (that mechanism is for
            // cross-entrypoint reuse, not cycles).
            continue;
        }
        let (cf, cs) = graph.info(callee_id)?;
        if super::noise_filter::is_generated_or_vendored_path(&cf) {
            // Generated/vendored callees are dropped from the tree
            // entirely, matching every other nav-map section's
            // noise-filtering guarantee.
            continue;
        }
        if let Some(r) = rendered.get(&callee_id) {
            children.push(FlowNode {
                entity_id: callee_id,
                file: cf,
                symbol: cs,
                children: Vec::new(),
                backref_to: Some(Backref {
                    entrypoint: r.entrypoint.clone(),
                    file: r.file.clone(),
                    symbol: r.symbol.clone(),
                    entity_id: callee_id,
                }),
            });
            continue;
        }

        if depth + 1 >= MAX_FLOW_DEPTH {
            // Stop expanding before overflowing the stack on a pathologically
            // deep (but acyclic) call chain. Render the callee as a leaf; its
            // own callees are simply not descended into.
            children.push(FlowNode {
                entity_id: callee_id,
                file: cf,
                symbol: cs,
                children: Vec::new(),
                backref_to: None,
            });
            continue;
        }

        rendered.insert(
            callee_id,
            RenderedRef {
                entrypoint: entrypoint.to_string(),
                file: cf,
                symbol: cs,
            },
        );
        children.push(expand(
            graph,
            callee_id,
            entrypoint,
            rendered,
            path,
            depth + 1,
        )?);
    }

    path.remove(&entity_id);
    Ok(FlowNode {
        entity_id,
        file,
        symbol,
        children,
        backref_to: None,
    })
}

/// Build the full reachable-tree flow for every semantic entrypoint in
/// `entrypoints`, in the order given — the caller is responsible for a
/// deterministic order (e.g. [`super::entrypoints::detect`]'s file-then-symbol
/// sort) since that order decides which entrypoint's tree "wins" the full
/// expansion of a node reachable from more than one entrypoint.
pub fn build_flows(
    conn: &Connection,
    entrypoints: &[Entrypoint],
) -> Result<Vec<FlowTree>, ApiError> {
    let graph = CallGraph::load(conn)?;
    let mut rendered: HashMap<i64, RenderedRef> = HashMap::new();
    let mut trees = Vec::with_capacity(entrypoints.len());

    for ep in entrypoints {
        // The entrypoint's own root is always fully expanded in its own
        // tree, even if it happens to already be `rendered` (a node can be
        // both an entrypoint and reachable from an earlier entrypoint's
        // tree — this task doesn't special-case that; the root always gets
        // a full expansion here per AC1's "root node id equals the
        // entrypoint").
        rendered.insert(
            ep.entity_id,
            RenderedRef {
                entrypoint: ep.symbol.clone(),
                file: ep.file.clone(),
                symbol: ep.symbol.clone(),
            },
        );
        let mut path = HashSet::new();
        let root = expand(
            &graph,
            ep.entity_id,
            &ep.symbol,
            &mut rendered,
            &mut path,
            0,
        )?;
        trees.push(FlowTree {
            entrypoint: ep.symbol.clone(),
            root,
        });
    }

    Ok(trees)
}

#[cfg(test)]
mod flows_reachable_tree_tests {
    use super::*;
    use crate::extract::langs::role_tags::RoleTag;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "varde-flows-{label}-{}-{}",
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

    /// The `Function`-kind entity named `name` — filtered by kind since a
    /// function's own call sites are separate `Call`-kind entities that
    /// share the same `name` (the callee name) and would otherwise win an
    /// unfiltered `ORDER BY id LIMIT 1` lookup.
    fn entity_id_for(conn: &Connection, name: &str) -> i64 {
        conn.query_row(
            "SELECT id FROM entities WHERE name = ?1 AND kind = ?2 ORDER BY id LIMIT 1",
            rusqlite::params![name, crate::model::EntityKind::Function.as_i64()],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("function entity `{name}` present: {e}"))
    }

    fn flatten(node: &FlowNode, out: &mut Vec<String>) {
        out.push(node.symbol.clone());
        for c in &node.children {
            flatten(c, out);
        }
    }

    /// AC1: reachable-tree walk from entrypoint `handle_request` — root node
    /// id equals `handle_request` and every transitively reachable function
    /// in the fixture appears in the tree.
    #[test]
    fn flows_reachable_tree_full_walk_from_entrypoint() {
        with_isolated_home(
            "flows",
            "full-walk",
            flows_reachable_tree_full_walk_from_entrypoint_inner,
        );
    }

    fn flows_reachable_tree_full_walk_from_entrypoint_inner() {
        let root = temp_root("full-walk");
        std::fs::write(
            root.join("app.py"),
            "def handle_request():\n    step_one()\n    step_two()\n\ndef step_one():\n    helper()\n\ndef step_two():\n    pass\n\ndef helper():\n    pass\n",
        )
        .expect("write app.py");

        crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
        let db = crate::db::path::repo_db_path(&root);
        let conn = Connection::open(&db).expect("open db");

        let entrypoint = Entrypoint {
            entity_id: entity_id_for(&conn, "handle_request"),
            file: "app.py".to_string(),
            symbol: "handle_request".to_string(),
            role: RoleTag::RouteHandler,
        };

        let trees =
            build_flows(&conn, std::slice::from_ref(&entrypoint)).expect("build_flows computes");
        assert_eq!(trees.len(), 1);
        let tree = &trees[0];
        assert_eq!(tree.root.entity_id, entrypoint.entity_id);
        assert_eq!(
            tree.root.symbol, "handle_request",
            "root node id equals the entrypoint"
        );

        let mut symbols = Vec::new();
        flatten(&tree.root, &mut symbols);
        for expected in ["handle_request", "step_one", "step_two", "helper"] {
            assert!(
                symbols.contains(&expected.to_string()),
                "{expected} must be transitively reachable in the tree: {symbols:?}"
            );
        }

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// AC2: `util_fn` reachable from two entrypoints `A` and `B` — when `B`'s
    /// tree is rendered, `util_fn` appears as a collapse-with-backref node
    /// referencing `A`'s tree (by file path + symbol id), not re-expanded.
    #[test]
    fn flows_reachable_tree_collapse_with_backref_across_entrypoints() {
        with_isolated_home(
            "flows",
            "backref",
            flows_reachable_tree_collapse_with_backref_across_entrypoints_inner,
        );
    }

    fn flows_reachable_tree_collapse_with_backref_across_entrypoints_inner() {
        let root = temp_root("backref");
        std::fs::write(
            root.join("app.py"),
            "def A():\n    util_fn()\n\ndef B():\n    util_fn()\n\ndef util_fn():\n    pass\n",
        )
        .expect("write app.py");

        crate::build::run_with_force(root.to_str().unwrap(), true).expect("full build");
        let db = crate::db::path::repo_db_path(&root);
        let conn = Connection::open(&db).expect("open db");

        let util_id = entity_id_for(&conn, "util_fn");
        let a_id = entity_id_for(&conn, "A");
        let b_id = entity_id_for(&conn, "B");
        let (app_path, _) = entity_info(&conn, a_id).expect("A's file path");
        let entrypoints = vec![
            Entrypoint {
                entity_id: a_id,
                file: app_path.clone(),
                symbol: "A".to_string(),
                role: RoleTag::RouteHandler,
            },
            Entrypoint {
                entity_id: b_id,
                file: app_path.clone(),
                symbol: "B".to_string(),
                role: RoleTag::RouteHandler,
            },
        ];

        let trees = build_flows(&conn, &entrypoints).expect("build_flows computes");
        assert_eq!(trees.len(), 2);

        let tree_a = &trees[0];
        assert_eq!(tree_a.entrypoint, "A");
        let util_in_a = tree_a
            .root
            .children
            .iter()
            .find(|c| c.entity_id == util_id)
            .expect("util_fn reachable from A");
        assert!(
            util_in_a.backref_to.is_none(),
            "A's tree fully expands util_fn: {util_in_a:?}"
        );

        let tree_b = &trees[1];
        assert_eq!(tree_b.entrypoint, "B");
        let util_in_b = tree_b
            .root
            .children
            .iter()
            .find(|c| c.entity_id == util_id)
            .expect("util_fn reachable from B");
        assert!(
            util_in_b.children.is_empty(),
            "B's tree must not re-expand util_fn's children: {util_in_b:?}"
        );
        let backref = util_in_b
            .backref_to
            .as_ref()
            .expect("util_fn in B's tree is a collapse-with-backref node");
        assert_eq!(backref.entrypoint, "A");
        assert_eq!(
            backref.file, app_path,
            "backref references A's tree by file path"
        );
        assert_eq!(backref.symbol, "util_fn");
        assert_eq!(backref.entity_id, util_id);

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression (review fix): a callee defined in a generated/vendored
    /// path (`target/`, `node_modules/`, ...) must be dropped from the tree
    /// entirely, matching every other nav-map section's noise-filtering
    /// guarantee — built with direct SQL inserts (mirroring
    /// `symbols_section`'s test pattern) rather than a full extraction
    /// build, since the fixture only needs to exercise the DB-level join
    /// [`CallGraph::load`] performs.
    #[test]
    fn flows_reachable_tree_excludes_generated_or_vendored_callees() {
        let path = std::env::temp_dir().join(format!(
            "varde-flows-noise-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let entry_file = insert_file(&conn, "src/app.py");
        let vendored_file = insert_file(&conn, "target/generated.py");

        let entry_id = insert_function(&conn, entry_file, "handle_request");
        let vendored_callee_id = insert_function(&conn, vendored_file, "generated_helper");

        // A `Call`-kind entity is the call site: `file_id`/`enclosing_function`
        // identify the caller (the same join `CallGraph::load` performs), and
        // the resolved edge's `to_entity_id` names the callee.
        let call_site_id = insert_call_site(&conn, entry_file, "handle_request");
        insert_resolved_call(&conn, entry_file, call_site_id, vendored_callee_id);

        let entrypoint = Entrypoint {
            entity_id: entry_id,
            file: "src/app.py".to_string(),
            symbol: "handle_request".to_string(),
            role: RoleTag::RouteHandler,
        };

        let trees =
            build_flows(&conn, std::slice::from_ref(&entrypoint)).expect("build_flows computes");
        assert_eq!(trees.len(), 1);

        let mut symbols = Vec::new();
        flatten(&trees[0].root, &mut symbols);
        assert!(
            !symbols.contains(&"generated_helper".to_string()),
            "callee defined only in target/ must be excluded from the flow tree: {symbols:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Regression (review fix H3): `build_flows` must load the call graph
    /// once via `CallGraph::load` and walk it in memory, not issue a
    /// per-node SQL round-trip — the exact N+1 shape `entrypoints::detect`
    /// was refactored away from. Asserted by counting statements traced on
    /// the connection while walking a linear call chain: a per-node query
    /// count would grow with `CHAIN_LEN`, an O(1)-load count would not.
    #[test]
    fn build_flows_issues_a_bounded_number_of_statements_regardless_of_chain_length() {
        let path = std::env::temp_dir().join(format!(
            "varde-flows-nplus1-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let mut conn = crate::db::open_or_rebuild(&path).expect("schema creates");

        let file = insert_file(&conn, "src/chain.py");
        const CHAIN_LEN: usize = 25;
        let mut ids = Vec::with_capacity(CHAIN_LEN);
        for i in 0..CHAIN_LEN {
            ids.push(insert_function(&conn, file, &format!("f{i}")));
        }
        for i in 0..CHAIN_LEN - 1 {
            let call_site = insert_call_site(&conn, file, &format!("f{i}"));
            insert_resolved_call(&conn, file, call_site, ids[i + 1]);
        }

        static STATEMENT_COUNT: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        fn count_statement(_sql: &str) {
            STATEMENT_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        conn.trace(Some(count_statement));

        let entrypoint = Entrypoint {
            entity_id: ids[0],
            file: "src/chain.py".to_string(),
            symbol: "f0".to_string(),
            role: RoleTag::RouteHandler,
        };

        let trees =
            build_flows(&conn, std::slice::from_ref(&entrypoint)).expect("build_flows computes");
        conn.trace(None);

        let mut symbols = Vec::new();
        flatten(&trees[0].root, &mut symbols);
        assert_eq!(
            symbols.len(),
            CHAIN_LEN,
            "full chain must be reachable: {symbols:?}"
        );

        let statements = STATEMENT_COUNT.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            statements < CHAIN_LEN,
            "build_flows must load the call graph once (bounded statement count), not one query per chain node: {statements} statements for a {CHAIN_LEN}-node chain"
        );

        let _ = std::fs::remove_file(&path);
    }

    fn insert_file(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO files (path) VALUES (?1)",
            rusqlite::params![path],
        )
        .expect("insert file row");
        conn.last_insert_rowid()
    }

    fn insert_function(conn: &Connection, file_id: i64, name: &str) -> i64 {
        conn.execute(
            "INSERT INTO entities (kind, name, file_id, start_byte, end_byte, start_line, start_col, end_line, end_col)
             VALUES (?1, ?2, ?3, 0, 0, 0, 0, 0, 0)",
            rusqlite::params![crate::model::EntityKind::Function.as_i64(), name, file_id],
        )
        .expect("insert function entity");
        conn.last_insert_rowid()
    }

    fn insert_call_site(conn: &Connection, file_id: i64, enclosing_function: &str) -> i64 {
        conn.execute(
            "INSERT INTO entities (kind, name, file_id, start_byte, end_byte, start_line, start_col, end_line, end_col, enclosing_function)
             VALUES (?1, 'call', ?2, 0, 0, 0, 0, 0, 0, ?3)",
            rusqlite::params![crate::model::EntityKind::Call.as_i64(), file_id, enclosing_function],
        )
        .expect("insert call-site entity");
        conn.last_insert_rowid()
    }

    fn insert_resolved_call(
        conn: &Connection,
        from_file_id: i64,
        from_entity_id: i64,
        to_entity_id: i64,
    ) {
        conn.execute(
            "INSERT INTO resolved_edges (from_file_id, kind, resolved, from_entity_id, to_entity_id)
             VALUES (?1, ?2, 1, ?3, ?4)",
            rusqlite::params![from_file_id, crate::resolve::EdgeKind::Call.as_i64(), from_entity_id, to_entity_id],
        )
        .expect("insert resolved call edge");
    }
}
