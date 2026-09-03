//! Dependency-graph construction over resolved calls/imports.

use crate::model::Entity;
use crate::resolve::{EdgeTarget, FileNode, ResolvedEdge};

/// Build the `FileNode`s for the distinct files present in the input.
///
/// `files` is already sorted, deduped, and index-aligned with every
/// `Entity`/`Symbol::file_id` (scan assigns file ids from this same table),
/// so this is a direct pass-through rather than a re-derivation from entity
/// data.
pub fn build_nodes(files: &[String]) -> Vec<FileNode> {
    files
        .iter()
        .map(|path| FileNode {
            path: path.clone(),
            community_id: None,
            fan_in: 0,
            fan_out: 0,
        })
        .collect()
}

/// Fill in fan-in/fan-out counts on each node, counting only `resolved` edges.
///
/// Every resolved edge contributes 1 to the `from` file's fan-out; its target
/// contributes 1 to the target file's fan-in — `EdgeTarget::File` targets the
/// file directly, `EdgeTarget::Entity` targets the entity's declaring file.
pub fn compute_fan_metrics(nodes: &mut [FileNode], edges: &[ResolvedEdge], entities: &[Entity]) {
    let mut fan_in = vec![0u32; nodes.len()];
    let mut fan_out = vec![0u32; nodes.len()];

    for edge in edges {
        if !edge.resolved {
            continue;
        }
        fan_out[edge.from as usize] += 1;
        match edge.to {
            EdgeTarget::File(to) => fan_in[to as usize] += 1,
            EdgeTarget::Entity(entity_id) => {
                if let Some(e) = entities.get(entity_id as usize) {
                    fan_in[e.file_id as usize] += 1;
                }
            }
            EdgeTarget::Unknown => {}
        }
    }

    for (node, (fi, fo)) in nodes.iter_mut().zip(fan_in.into_iter().zip(fan_out)) {
        node.fan_in = fi;
        node.fan_out = fo;
    }
}
