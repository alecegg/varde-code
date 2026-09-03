//! Subsystem naming on top of the existing Louvain clustering.
//!
//! Community detection itself lives in `crate::resolve::community` (Louvain
//! port, see `detect()`); this module does not reimplement it. It only
//! takes already-detected `crate::resolve::Community` values (the same
//! shape the `clusters` query mode reads back from the persisted
//! `communities`/`community_members` tables) and derives a human-meaningful
//! name for each one:
//!
//! - primary signal: directory-path overlap. If a supermajority of members
//!   share a parent directory, the name is that directory's last path
//!   segment (e.g. `src/auth/` -> `auth`).
//! - fallback signal: when no directory prefix dominates, the dominant
//!   role tag among members (see `crate::extract::langs::role_tags`) is
//!   used to derive a name instead (e.g. `route_handlers`).
//!
//! Generated/vendored members (`crate::query::noise_filter`) are dropped
//! from a cluster's membership before naming runs, so noise never
//! influences the name or shows up in the named cluster's member list.

use std::collections::HashMap;

use crate::extract::langs::role_tags::RoleTag;
use crate::query::noise_filter::is_generated_or_vendored_path;
use crate::resolve::Community;

/// Fraction of (noise-filtered) members that must agree on a signal before
/// it is considered dominant enough to name a cluster after.
const SUPERMAJORITY: f64 = 2.0 / 3.0;

/// A Louvain community with a derived human-meaningful name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedCluster {
    pub id: u32,
    pub name: String,
    /// Members after noise-filtering (generated/vendored paths removed).
    pub members: Vec<String>,
}

/// Name every cluster, filtering generated/vendored members out first.
///
/// `role_tags` maps a member file path to the role tag it should be
/// considered to carry for the purposes of the fallback naming signal
/// (callers typically derive this from per-symbol role-tag matches,
/// collapsed to one dominant tag per file).
pub fn name_clusters(
    clusters: &[Community],
    role_tags: &HashMap<String, RoleTag>,
) -> Vec<NamedCluster> {
    clusters
        .iter()
        .filter_map(|cluster| {
            let members: Vec<String> = cluster
                .members
                .iter()
                .filter(|path| !is_generated_or_vendored_path(path))
                .cloned()
                .collect();
            if members.is_empty() {
                return None;
            }
            let name = name_for_members(&members, role_tags);
            Some(NamedCluster {
                id: cluster.id,
                name,
                members,
            })
        })
        .collect()
}

fn name_for_members(members: &[String], role_tags: &HashMap<String, RoleTag>) -> String {
    if let Some(name) = dominant_directory_name(members) {
        return name;
    }
    if let Some(name) = dominant_role_tag_name(members, role_tags) {
        return name;
    }
    "cluster".to_string()
}

/// Primary signal: if a supermajority of members share the same parent
/// directory, return that directory's last path segment.
fn dominant_directory_name(members: &[String]) -> Option<String> {
    let total = members.len();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for path in members {
        let dir = parent_dir(path);
        *counts.entry(dir).or_insert(0) += 1;
    }
    let (dir, count) = counts.into_iter().max_by_key(|(_, count)| *count)?;
    if (count as f64) / (total as f64) < SUPERMAJORITY {
        return None;
    }
    last_segment(dir).map(|s| s.to_string())
}

/// Fallback signal: if a supermajority of members carry the same role tag,
/// derive a name from that tag (e.g. `RoleTag::RouteHandler` -> `route_handlers`).
fn dominant_role_tag_name(
    members: &[String],
    role_tags: &HashMap<String, RoleTag>,
) -> Option<String> {
    let total = members.len();
    let mut counts: HashMap<RoleTag, usize> = HashMap::new();
    for path in members {
        if let Some(tag) = role_tags.get(path) {
            *counts.entry(*tag).or_insert(0) += 1;
        }
    }
    let (tag, count) = counts.into_iter().max_by_key(|(_, count)| *count)?;
    if (count as f64) / (total as f64) < SUPERMAJORITY {
        return None;
    }
    Some(role_tag_cluster_name(tag))
}

fn role_tag_cluster_name(tag: RoleTag) -> String {
    let snake = match tag {
        RoleTag::RouteHandler => "route_handler",
        RoleTag::PageComponent => "page_component",
        RoleTag::CliCommand => "cli_command",
        RoleTag::BackgroundJob => "background_job",
        RoleTag::EventListener => "event_listener",
        RoleTag::Middleware => "middleware",
    };
    format!("{snake}s")
}

/// Parent directory of a `/`-separated path, or `""` for a bare filename.
fn parent_dir(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((dir, _file)) => dir,
        None => "",
    }
}

/// Last path segment of a (possibly empty) directory string.
fn last_segment(dir: &str) -> Option<&str> {
    if dir.is_empty() {
        return None;
    }
    dir.rsplit('/').next()
}

#[cfg(test)]
mod subsystems_clustering_tests {
    use super::*;

    #[test]
    fn subsystems_clustering_names_cluster_by_shared_directory_prefix() {
        let clusters = vec![Community {
            id: 0,
            members: vec![
                "src/auth/login.rs".to_string(),
                "src/auth/session.rs".to_string(),
                "src/auth/token.rs".to_string(),
            ],
        }];
        let role_tags = HashMap::new();

        let named = name_clusters(&clusters, &role_tags);

        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, "auth");
        assert_eq!(named[0].members.len(), 3);
    }

    #[test]
    fn subsystems_clustering_falls_back_to_dominant_role_tag_when_no_directory_majority() {
        let clusters = vec![Community {
            id: 1,
            members: vec![
                "src/api/users.rs".to_string(),
                "src/web/orders.rs".to_string(),
                "src/handlers/payments.rs".to_string(),
            ],
        }];
        let mut role_tags = HashMap::new();
        role_tags.insert("src/api/users.rs".to_string(), RoleTag::RouteHandler);
        role_tags.insert("src/web/orders.rs".to_string(), RoleTag::RouteHandler);
        role_tags.insert(
            "src/handlers/payments.rs".to_string(),
            RoleTag::RouteHandler,
        );

        let named = name_clusters(&clusters, &role_tags);

        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, "route_handlers");
    }

    #[test]
    fn subsystems_clustering_excludes_generated_and_vendored_members_before_naming() {
        let clusters = vec![Community {
            id: 2,
            members: vec![
                "src/auth/login.rs".to_string(),
                "src/auth/session.rs".to_string(),
                "node_modules/some-pkg/index.js".to_string(),
            ],
        }];
        let role_tags = HashMap::new();

        let named = name_clusters(&clusters, &role_tags);

        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, "auth");
        assert_eq!(named[0].members.len(), 2);
        assert!(
            named[0]
                .members
                .iter()
                .all(|m| !m.starts_with("node_modules"))
        );
    }
}
