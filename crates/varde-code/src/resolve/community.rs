//! Louvain community detection over the file graph.
//!
//! Faithful port of varde's TS `louvain()` in `community-detection.ts`:
//! greedy modularity optimization via local moving, with a resolution
//! parameter (1.0 = standard modularity). Deterministic: nodes processed in
//! id order, ties broken toward the smallest community id.

use std::collections::{BTreeMap, HashMap};

use crate::model::Entity;
use crate::resolve::{Community, EdgeTarget, FileNode, ResolvedEdge};

const MAX_PASSES: usize = 50;

/// Tolerance for the "same modularity gain" tie-break below. Two gains that
/// are mathematically equal can still differ by a few ULPs of floating-point
/// rounding; an exact `==` would pick whichever community was visited first
/// instead of the smallest id, breaking the "deterministic partition"
/// guarantee. The candidate map (`edges_to_comm`) is a `BTreeMap` so the
/// selection loop visits communities in ascending-id order and, combined with
/// this epsilon + id tie-break, produces bit-for-bit reproducible partitions
/// across runs (a `HashMap`'s per-run seed would otherwise vary the order).
const GAIN_EPSILON: f64 = 1e-9;

/// Detect communities over the file graph and assign `community_id` on each
/// node. Edges considered: every resolved file-to-file connection — import
/// edges (`EdgeTarget::File`) directly, call edges (`EdgeTarget::Entity`)
/// via the invoked entity's declaring file — weighted symmetrically (matches
/// the fan-metric semantics).
///
/// Returns communities keyed by a stable id (lex-smallest member path wins
/// the label, then sorted).
pub fn detect(
    nodes: &mut [FileNode],
    edges: &[ResolvedEdge],
    entities: &[Entity],
) -> Vec<Community> {
    let n = nodes.len();
    if n == 0 {
        return Vec::new();
    }

    // Weighted undirected adjacency between file ids.
    let mut adj: Vec<HashMap<usize, f64>> = vec![HashMap::new(); n];
    let mut total_edges = 0.0;
    for edge in edges {
        if !edge.resolved {
            continue;
        }
        let to = match edge.to {
            EdgeTarget::File(to) => to as usize,
            EdgeTarget::Entity(entity_id) => match entities.get(entity_id as usize) {
                Some(e) => e.file_id as usize,
                None => continue,
            },
            EdgeTarget::Unknown => continue,
        };
        let (u, v) = (edge.from as usize, to);
        if u == v {
            continue;
        }
        *adj[u].entry(v).or_insert(0.0) += 1.0;
        *adj[v].entry(u).or_insert(0.0) += 1.0;
        total_edges += 1.0;
    }

    if total_edges == 0.0 {
        // No connections: every file is its own community.
        let communities: Vec<Community> = nodes
            .iter()
            .enumerate()
            .map(|(i, node)| Community {
                id: i as u32,
                members: vec![node.path.clone()],
            })
            .collect();
        for (i, node) in nodes.iter_mut().enumerate() {
            node.community_id = Some(i as u32);
        }
        return communities;
    }

    let m = total_edges;
    let two_m_sq = 2.0 * m * m;

    // degree[i] = sum of incident edge weights.
    let degree: Vec<f64> = adj
        .iter()
        .map(|neighbors| neighbors.values().sum())
        .collect();

    // community[i] = current community of node i (starts as its own).
    let mut community: Vec<usize> = (0..n).collect();
    // sigma_in[c] = 2 × internal edge weight of community c.
    let mut sigma_in = vec![0.0f64; n];
    // sigma_tot[c] = sum of degrees in community c.
    let mut sigma_tot = degree.clone();

    let mut changed = true;
    let mut pass = 0usize;
    while changed && pass < MAX_PASSES {
        changed = false;
        pass += 1;

        for i in 0..n {
            let curr_comm = community[i];
            let ki = degree[i];
            let curr_sigma_tot = sigma_tot[curr_comm];

            // Accumulate edge weights from i to each neighbouring community.
            // `BTreeMap` (not `HashMap`) so the selection loop below iterates
            // in deterministic ascending-community-id order across runs.
            let mut edges_to_comm: BTreeMap<usize, f64> = BTreeMap::new();
            for (j, w) in &adj[i] {
                let jc = community[*j];
                *edges_to_comm.entry(jc).or_insert(0.0) += w;
            }

            let ki_in_curr = edges_to_comm.get(&curr_comm).copied().unwrap_or(0.0);

            // Gain from removing i from curr_comm.
            let gain_removal = -ki_in_curr / m + ki * (curr_sigma_tot - ki) / two_m_sq;

            let mut best_gain = 0.0;
            let mut best_comm = curr_comm;

            for (target_comm, ki_in_target) in &edges_to_comm {
                if *target_comm == curr_comm {
                    continue;
                }
                let target_sigma_tot = sigma_tot.get(*target_comm).copied().unwrap_or(0.0);
                let total_gain = gain_removal + ki_in_target / m - ki * target_sigma_tot / two_m_sq;
                if total_gain > best_gain + GAIN_EPSILON
                    || ((total_gain - best_gain).abs() <= GAIN_EPSILON && *target_comm < best_comm)
                {
                    best_gain = total_gain;
                    best_comm = *target_comm;
                }
            }

            if best_comm != curr_comm {
                sigma_in[curr_comm] -= 2.0 * ki_in_curr;
                sigma_tot[curr_comm] -= ki;

                let ki_in_best = edges_to_comm.get(&best_comm).copied().unwrap_or(0.0);
                sigma_in[best_comm] += 2.0 * ki_in_best;
                sigma_tot[best_comm] += ki;

                community[i] = best_comm;
                changed = true;
            }
        }
    }

    // Stable labels: lex-smallest member path per community.
    let mut comm_label: HashMap<usize, String> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        let c = community[i];
        let entry = comm_label.entry(c).or_insert_with(|| node.path.clone());
        if node.path < *entry {
            *entry = node.path.clone();
        }
    }

    // Renumber communities by sorted label; assign ids.
    assign_communities(nodes, &community, &comm_label)
}

/// Renumber the raw community assignment into stable `Community`s and set
/// each node's `community_id`. Labels are the lex-smallest member path;
/// ids are assigned in sorted-label order.
fn assign_communities(
    nodes: &mut [FileNode],
    community: &[usize],
    comm_label: &HashMap<usize, String>,
) -> Vec<Community> {
    let mut labels: Vec<&str> = comm_label.values().map(String::as_str).collect();
    labels.sort_unstable();
    let mut id_by_label: HashMap<&str, u32> = HashMap::new();
    for (id, label) in labels.into_iter().enumerate() {
        id_by_label.insert(label, id as u32);
    }

    let mut members_by_id: HashMap<u32, Vec<String>> = HashMap::new();
    for (i, node) in nodes.iter_mut().enumerate() {
        let id = id_by_label[comm_label[&community[i]].as_str()];
        node.community_id = Some(id);
        members_by_id.entry(id).or_default().push(node.path.clone());
    }

    let mut communities: Vec<Community> = members_by_id
        .into_iter()
        .map(|(id, mut members)| {
            members.sort();
            Community { id, members }
        })
        .collect();
    communities.sort_by_key(|c| c.id);
    communities
}
