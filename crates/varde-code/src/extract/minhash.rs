//! MinHash + LSH band signatures over shingled function tokens.
//!
//! Clone-band detection groups near-duplicate functions by their per-band
//! MinHash signatures (see `resolve::clones::detect_bands`). The signatures
//! are computed here, at extract time, so that only the small fixed-size
//! signature vector is carried on the entity and persisted — not the raw
//! token stream (which summed to ~67 MB of JSON per build on large repos).
//!
//! Parameters: shingle size 5, 16 FNV-1a hash functions split into 4 bands
//! of 4 rows. Hashing is deterministic (FNV-1a with a per-function seed), so
//! results are reproducible across runs.

/// Window size for the token shingles.
pub(crate) const SHINGLE_SIZE: usize = 5;
/// Number of independent hash functions.
const NUM_HASHES: usize = 16;
/// Number of LSH bands (the persisted signature is one `u64` per band).
pub(crate) const BANDS: usize = 4;
/// Hash functions per band.
const ROWS: usize = NUM_HASHES / BANDS;
/// FNV-1a offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
/// FNV-1a prime.
const FNV_PRIME: u64 = 0x100000001b3;
/// Seed mixing multiplier (per-hash-function distinctness).
const SEED_MIX: u64 = 0x9e3779b97f4a7c15;

/// Per-hash-function FNV-1a offset bases, one per [`NUM_HASHES`] — each
/// function folds the shingle bytes from a distinct starting point instead of
/// all 16 being derived (via [`mix_hash`]'s bijection) from one shared FNV
/// pass. A bijection of a single base hash carries no more entropy than that
/// base hash: given any one derived value the bijection is invertible, so the
/// base hash — and every other derived value — is fully recoverable from it.
/// That collapsed the 16 "independent" hash functions LSH banding assumes to
/// one real 64-bit hash's worth of entropy, degrading clone-detection recall.
/// Seeding the offset basis instead makes each function a genuinely distinct
/// FNV-1a computation over the full shingle content.
const HASH_SEEDS: [u64; NUM_HASHES] = {
    let mut seeds = [0u64; NUM_HASHES];
    let mut i = 0;
    while i < NUM_HASHES {
        seeds[i] = FNV_OFFSET_BASIS ^ ((i as u64 + 1).wrapping_mul(SEED_MIX));
        i += 1;
    }
    seeds
};

/// Compute the per-band MinHash signatures for a Function body, or `None`
/// when the body has too few tokens to shingle (fewer than [`SHINGLE_SIZE`]).
///
/// The result has exactly [`BANDS`] entries; it is the value persisted in
/// `entities.body_minhash` and consumed by `resolve::clones::detect_bands`.
///
/// Shingles are derived directly from the borrowed body text — token
/// boundaries are located as maximal alphanumeric runs and hashed in place,
/// never materializing a `Vec<String>` of tokens or of space-joined shingles.
pub(crate) fn body_minhash(text: &str) -> Option<Vec<u64>> {
    // Locate the byte ranges of each maximal alphanumeric run (token).
    let mut starts: Vec<usize> = Vec::new();
    let mut ends: Vec<usize> = Vec::new();
    let mut run_start: Option<usize> = None;
    for (off, c) in text.char_indices() {
        if c.is_alphanumeric() {
            run_start.get_or_insert(off);
        } else if let Some(s) = run_start.take() {
            starts.push(s);
            ends.push(off);
        }
    }
    if let Some(s) = run_start.take() {
        starts.push(s);
        ends.push(text.len());
    }

    let n = starts.len();
    if n < SHINGLE_SIZE {
        return None;
    }

    let bytes = text.as_bytes();
    let mut signatures = vec![u64::MAX; BANDS];
    // For each shingle (window of SHINGLE_SIZE tokens), independently hash
    // the joined space-separated lowercased tokens once per hash function —
    // each starting from its own seeded offset basis ([`HASH_SEEDS`]) rather
    // than derived from one shared FNV pass.
    for w in 0..=n - SHINGLE_SIZE {
        for (h, &seed) in HASH_SEEDS.iter().enumerate().take(NUM_HASHES) {
            let mut hash = seed;
            for k in 0..SHINGLE_SIZE {
                if k > 0 {
                    hash = fnv1a64_update(hash, b' ');
                }
                for &b in &bytes[starts[w + k]..ends[w + k]] {
                    hash = fnv1a64_update(hash, b.to_ascii_lowercase());
                }
            }
            let band = h / ROWS;
            if hash < signatures[band] {
                signatures[band] = hash;
            }
        }
    }
    Some(signatures)
}

/// One FNV-1a 64-bit round: fold a byte into the running hash.
#[inline]
fn fnv1a64_update(hash: u64, b: u8) -> u64 {
    (hash ^ b as u64).wrapping_mul(FNV_PRIME)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Triangular pairwise comparison: the `.enumerate()` rewrite clippy
    // suggests reads worse than the two index ranges here.
    #[allow(clippy::needless_range_loop)]
    #[test]
    fn hash_seeds_are_pairwise_distinct() {
        for i in 0..NUM_HASHES {
            for j in (i + 1)..NUM_HASHES {
                assert_ne!(HASH_SEEDS[i], HASH_SEEDS[j], "seed {i} and {j} collide");
            }
        }
    }

    /// Regression (review fix M2): before this fix, all 16 per-hash-function
    /// values were derived from one shared FNV pass via a bijection
    /// (invertible, so every derived value was fully determined by any one
    /// other) — meaning every band necessarily agreed on which shingle "won"
    /// (had the smallest hash), collapsing the 16-hash design's effective
    /// entropy to that of one hash function. Real independent hash functions
    /// can disagree on the winner per band; this asserts they now do.
    // `band` indexes into the inner signature vec (`sigs[i][band]`), which the
    // iterator rewrite can't express cleanly.
    #[allow(clippy::needless_range_loop)]
    #[test]
    fn bands_can_pick_different_winning_shingles_independently() {
        let texts = [
            "alpha beta gamma delta one",
            "alpha beta gamma delta two",
            "alpha beta gamma delta three",
        ];
        let sigs: Vec<Vec<u64>> = texts
            .iter()
            .map(|t| body_minhash(t).expect("shingles"))
            .collect();
        let mut orderings = std::collections::HashSet::new();
        for band in 0..BANDS {
            let mut order: Vec<usize> = (0..texts.len()).collect();
            order.sort_by_key(|&i| sigs[i][band]);
            orderings.insert(order);
        }
        assert!(
            orderings.len() > 1,
            "expected bands to disagree on relative ordering across texts, all matched: {sigs:?}"
        );
    }
}
