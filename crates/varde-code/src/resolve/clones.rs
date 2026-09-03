//! MinHash + LSH clone-band detection over shingled function tokens.
//!
//! The per-band MinHash signatures are computed at extract time
//! (`crate::extract::minhash`) and carried on each Function entity; this
//! module only buckets entities by signature. Parameters chosen for the
//! fixture bar: shingle size 5, 16 hash functions split into 4 bands of 4
//! rows.

use rayon::prelude::*;
use std::collections::HashMap;

use crate::extract::minhash::BANDS;
use crate::model::{Entity, EntityKind};
use crate::resolve::CloneBand;

/// Detect near-duplicate functions and group them into clone bands.
///
/// For each Function entity with a `body_minhash` signature, bucket entities
/// by band signature. A bucket with two or more members becomes a `CloneBand`
/// (members = entity ids).
pub fn detect_bands(entities: &[Entity]) -> Vec<CloneBand> {
    // entity id -> band signatures (one per band).
    let mut by_entity: Vec<(u32, &[u64])> = Vec::new();
    for (i, e) in entities.iter().enumerate() {
        if e.kind != EntityKind::Function {
            continue;
        }
        let Some(signatures) = e.body_minhash.as_deref() else {
            continue;
        };
        by_entity.push((i as u32, signatures));
    }

    // band -> signature -> entity ids. Bucketing is a commutative reduction:
    // member order within a bucket is irrelevant (members are sorted and
    // deduped below), so a parallel fold + merge is deterministic.
    let mut buckets: HashMap<usize, HashMap<u64, Vec<u32>>> = by_entity
        .par_iter()
        .fold(
            HashMap::<usize, HashMap<u64, Vec<u32>>>::new,
            |mut acc, (entity_id, signatures)| {
                for (band, sig) in signatures.iter().enumerate() {
                    acc.entry(band)
                        .or_default()
                        .entry(*sig)
                        .or_default()
                        .push(*entity_id);
                }
                acc
            },
        )
        .reduce(HashMap::new, |mut a, b| {
            for (band, m) in b {
                let dst = a.entry(band).or_default();
                for (sig, mut members) in m {
                    dst.entry(sig).or_default().append(&mut members);
                }
            }
            a
        });

    // Groups with >= 2 members become clone bands (deterministic order:
    // band index, then signature).
    let mut bands: Vec<CloneBand> = Vec::new();
    let mut next_id = 0u32;
    for band in 0..BANDS {
        // `buckets` is consumed here, so move each band's inner map out rather
        // than cloning its member `Vec`s.
        let Some(mut m) = buckets.remove(&band) else {
            continue;
        };
        let mut sigs: Vec<u64> = m.keys().copied().collect();
        sigs.sort_unstable();
        for sig in sigs {
            // `sig` was collected from this same `band`'s keys above, so the
            // lookup always hits — `.remove()` instead of indexing avoids a
            // panic path if that invariant is ever broken by a future edit.
            let Some(mut members) = m.remove(&sig) else {
                continue;
            };
            if members.len() < 2 {
                continue;
            }
            members.sort_unstable();
            members.dedup();
            if members.len() < 2 {
                continue;
            }
            bands.push(CloneBand {
                id: next_id,
                members,
            });
            next_id += 1;
        }
    }
    bands
}

#[cfg(test)]
mod clone_detection_minhash_lsh {
    use super::super::test_util::load_project;
    use super::*;

    #[test]
    fn near_duplicates_share_band_distinct_excluded() {
        let (entities, symbols, files) = load_project("rust/clones");
        let graph = super::super::resolve(&entities, &symbols, &files).expect("resolve succeeds");

        let entity_id = |name: &str| {
            entities
                .iter()
                // type-hierarchy: unchanged (test helper resolves Function entities by name; unrelated to Extends/Implements)
                .position(|e| e.kind == EntityKind::Function && e.name == name)
                .expect("function entity") as u32
        };
        let one = entity_id("helper_one");
        let two = entity_id("helper_two");
        let distinct = entity_id("unique_thing");

        // Both near-duplicates share at least one clone band.
        let shared: Vec<&CloneBand> = graph
            .clone_bands
            .iter()
            .filter(|b| b.members.contains(&one) && b.members.contains(&two))
            .collect();
        assert!(
            !shared.is_empty(),
            "near-duplicates must share a clone band, got {:#?}",
            graph.clone_bands
        );

        // The distinct function is in no band shared with either duplicate.
        for band in &graph.clone_bands {
            assert!(
                !band.members.contains(&distinct)
                    || (!band.members.contains(&one) && !band.members.contains(&two)),
                "distinct function must not share a band with the near-duplicates: {band:?}"
            );
        }
    }
}
