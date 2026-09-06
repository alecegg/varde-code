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
//! - last-resort signal: a scattered cluster with no supermajority is named
//!   after its *modal* (most common) parent directory rather than a generic
//!   `cluster` — informative even when members span several directories.
//!
//! Generated/vendored (and other non-source) members
//! (`crate::query::noise_filter`) are dropped from a cluster's membership
//! before naming runs, so noise never influences the name or shows up in the
//! named cluster's member list. Communities that hold fewer than two members
//! after that filtering are dropped entirely: a single file is not a subsystem
//! (on a reference repo 336 of 442 communities were such singletons).

use std::collections::HashMap;

use crate::extract::langs::role_tags::RoleTag;
use crate::query::noise_filter::{is_generated_or_vendored_path, is_non_source_path};
use crate::resolve::Community;

/// Fraction of (noise-filtered) members that must agree on a signal before
/// it is considered dominant enough to name a cluster after.
const SUPERMAJORITY: f64 = 2.0 / 3.0;

/// Minimum member count (after noise-filtering) for a community to count as a
/// subsystem. A single file clustered on its own is not a subsystem.
const MIN_SUBSYSTEM_MEMBERS: usize = 2;

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
                .filter(|path| !is_generated_or_vendored_path(path) && !is_non_source_path(path))
                .cloned()
                .collect();
            if members.len() < MIN_SUBSYSTEM_MEMBERS {
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
    if let Some(name) = plurality_directory_name(members) {
        return name;
    }
    "cluster".to_string()
}

/// Last-resort signal: name a scattered cluster after its *modal* parent
/// directory — the last segment of the directory shared by the most members,
/// even when it falls short of the [`SUPERMAJORITY`] threshold. Ties are
/// broken by directory string for determinism. Returns None only when no
/// member has a parent directory (all bare filenames), leaving the generic
/// `cluster` name. This keeps distinct scattered clusters from all collapsing
/// to one indistinguishable `cluster` label (46 of them on a reference repo).
fn plurality_directory_name(members: &[String]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for path in members {
        let dir = parent_dir(path);
        if !dir.is_empty() {
            *counts.entry(dir).or_insert(0) += 1;
        }
    }
    let (dir, _count) = counts
        .into_iter()
        .max_by(|(a_dir, a_n), (b_dir, b_n)| a_n.cmp(b_n).then_with(|| b_dir.cmp(a_dir)))?;
    last_segment(dir).map(|s| s.to_string())
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
        RoleTag::ProcessMain => "process_main",
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
    fn subsystems_clustering_drops_singleton_communities() {
        // A community of one file (after noise-filtering) is not a subsystem —
        // it just pads the list (336 of 442 on a reference repo). Drop it.
        let clusters = vec![
            Community {
                id: 0,
                members: vec![
                    "src/auth/login.rs".to_string(),
                    "src/auth/session.rs".to_string(),
                ],
            },
            Community {
                id: 1,
                members: vec!["src/lonely.rs".to_string()],
            },
            Community {
                id: 2,
                // one real source file + one vendored: collapses to a singleton
                // after noise-filtering, so it must also drop.
                members: vec![
                    "src/orphan.rs".to_string(),
                    "node_modules/pkg/index.js".to_string(),
                ],
            },
        ];
        let role_tags = HashMap::new();

        let named = name_clusters(&clusters, &role_tags);

        assert_eq!(
            named.len(),
            1,
            "only the 2-member community survives: {named:?}"
        );
        assert_eq!(named[0].id, 0);
    }

    #[test]
    fn subsystems_clustering_names_scattered_cluster_by_plurality_directory() {
        // No directory or role-tag reaches a 2/3 supermajority, but most files
        // live under `src/utils` — name it `utils`, not the dead-end `cluster`.
        let clusters = vec![Community {
            id: 3,
            members: vec![
                "src/utils/a.rs".to_string(),
                "src/utils/b.rs".to_string(),
                "src/api/c.rs".to_string(),
                "src/web/d.rs".to_string(),
            ],
        }];
        let role_tags = HashMap::new();

        let named = name_clusters(&clusters, &role_tags);

        assert_eq!(named.len(), 1);
        assert_eq!(
            named[0].name, "utils",
            "scattered cluster names after its modal directory: {named:?}"
        );
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
