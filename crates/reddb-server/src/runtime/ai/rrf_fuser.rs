//! `RrfFuser` — pure Reciprocal Rank Fusion for ASK hybrid retrieval.
//!
//! Issue #398 (PRD #391): the ASK pipeline retrieves candidates from
//! multiple buckets — BM25 text search, vector similarity, graph
//! traversal — and needs a single ranked list to feed the prompt
//! assembler. RRF is the standard, parameter-light way to combine
//! ranked lists from heterogeneous scorers whose raw scores are not
//! directly comparable.
//!
//! Deep module: no I/O, no transport, no global state. The caller
//! hands in pre-ranked per-bucket lists, optional per-bucket
//! `min_score` filters, the RRF constant `k`, and the final cap.
//! The fuser returns the fused, deterministically-tied list capped
//! to `total_k`.
//!
//! ## Formula
//!
//! For each item `d` and each ranker `r`:
//!
//! ```text
//! rrf_score(d) = Σ_r 1 / (k + rank_r(d))
//! ```
//!
//! where `rank_r(d)` is 1-indexed (best item in list r is rank 1).
//! Items absent from a list contribute nothing. The convention `k=60`
//! comes from Cormack, Clarke & Büttcher 2009 and is the value used
//! by published RRF baselines (Elasticsearch, Weaviate, Qdrant) — we
//! keep it as the default but expose it for tests.
//!
//! ## Per-bucket filtering
//!
//! Each bucket carries native scores (BM25 score, cosine similarity,
//! graph traversal weight). `min_score` is applied per bucket *before*
//! fusion, because the natural threshold differs by ranker (cosine 0.7
//! ≠ BM25 0.7). Filtered items are dropped entirely; they do not
//! contribute to any other bucket's ranks.
//!
//! ## Tie-break
//!
//! When two items share an RRF score, the fused order is determined
//! by the item id (lexicographic for strings, natural for ints). This
//! makes the fuser a pure function: identical inputs produce
//! byte-identical outputs, which the ASK determinism contract (#400)
//! relies on.

use std::collections::HashMap;
use std::hash::Hash;

/// One candidate inside a per-bucket ranked list, with its native
/// score. The score is only used for `min_score` filtering — RRF
/// itself looks only at rank, not at score magnitude.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate<Id> {
    pub id: Id,
    pub score: f64,
}

/// A ranked list from one retriever (a "bucket"). Order matters —
/// position 0 is best, position 1 is second, etc.
#[derive(Debug, Clone)]
pub struct Bucket<Id> {
    pub candidates: Vec<Candidate<Id>>,
    /// Per-bucket score floor. `None` means "no filter". Applied
    /// before fusion. Use this so that BM25 0.4 and cosine 0.7 can
    /// coexist sensibly.
    pub min_score: Option<f64>,
}

/// Output of fusion: one row per surviving id, sorted by `rrf_score`
/// descending, with deterministic tie-break by id.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedItem<Id> {
    pub id: Id,
    pub rrf_score: f64,
}

/// The canonical RRF constant from Cormack et al. 2009. Exposed for
/// tests; production code should pass this through unchanged.
pub const RRF_K_DEFAULT: u32 = 60;

/// Fuse per-bucket ranked lists into a single ranked list capped at
/// `total_k`. Pure function — no I/O, no clock.
///
/// `k` is the RRF constant (use [`RRF_K_DEFAULT`] = 60 in production).
/// `total_k` is the maximum number of items to return; if zero, the
/// result is empty.
pub fn fuse<Id>(buckets: &[Bucket<Id>], k: u32, total_k: usize) -> Vec<FusedItem<Id>>
where
    Id: Clone + Eq + Hash + Ord,
{
    if total_k == 0 {
        return Vec::new();
    }

    let k_f = f64::from(k);
    let mut scores: HashMap<Id, f64> = HashMap::new();

    for bucket in buckets {
        let mut rank: u32 = 0;
        for cand in &bucket.candidates {
            if let Some(floor) = bucket.min_score {
                if cand.score < floor {
                    continue;
                }
            }
            rank += 1;
            let contribution = 1.0 / (k_f + f64::from(rank));
            scores
                .entry(cand.id.clone())
                .and_modify(|s| *s += contribution)
                .or_insert(contribution);
        }
    }

    let mut fused: Vec<FusedItem<Id>> = scores
        .into_iter()
        .map(|(id, rrf_score)| FusedItem { id, rrf_score })
        .collect();

    // Descending by score; ties broken by id ascending. partial_cmp
    // is safe here because rrf_score is always a finite positive sum
    // of positive reciprocals — no NaN possible.
    fused.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    fused.truncate(total_k);
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand<Id>(id: Id, score: f64) -> Candidate<Id> {
        Candidate { id, score }
    }

    fn bucket_no_floor<Id: Clone>(cs: Vec<Candidate<Id>>) -> Bucket<Id> {
        Bucket {
            candidates: cs,
            min_score: None,
        }
    }

    // ---- Reference values ---------------------------------------------

    #[test]
    fn rrf_single_list_matches_reference() {
        // Single list, k=60. Expected scores by rank: 1/61, 1/62, 1/63.
        let bucket = bucket_no_floor(vec![cand("a", 1.0), cand("b", 0.5), cand("c", 0.1)]);
        let out = fuse(&[bucket], 60, 10);
        assert_eq!(out.len(), 3);
        assert!((out[0].rrf_score - 1.0 / 61.0).abs() < 1e-12);
        assert!((out[1].rrf_score - 1.0 / 62.0).abs() < 1e-12);
        assert!((out[2].rrf_score - 1.0 / 63.0).abs() < 1e-12);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[1].id, "b");
        assert_eq!(out[2].id, "c");
    }

    #[test]
    fn rrf_two_lists_sums_contributions() {
        // 'a' is rank 1 in both → 2/61.
        // 'b' is rank 2 in both → 2/62.
        // 'c' only in list 1 at rank 3 → 1/63.
        // 'd' only in list 2 at rank 3 → 1/63.
        let b1 = bucket_no_floor(vec![cand("a", 1.0), cand("b", 0.9), cand("c", 0.8)]);
        let b2 = bucket_no_floor(vec![cand("a", 0.95), cand("b", 0.85), cand("d", 0.7)]);
        let out = fuse(&[b1, b2], 60, 10);
        let by_id: std::collections::HashMap<_, _> =
            out.iter().map(|f| (f.id, f.rrf_score)).collect();
        assert!((by_id["a"] - 2.0 / 61.0).abs() < 1e-12);
        assert!((by_id["b"] - 2.0 / 62.0).abs() < 1e-12);
        assert!((by_id["c"] - 1.0 / 63.0).abs() < 1e-12);
        assert!((by_id["d"] - 1.0 / 63.0).abs() < 1e-12);
        // Order: a > b > (c,d) with c first by id tie-break.
        assert_eq!(out[0].id, "a");
        assert_eq!(out[1].id, "b");
        assert_eq!(out[2].id, "c");
        assert_eq!(out[3].id, "d");
    }

    #[test]
    fn rrf_k_default_is_60() {
        assert_eq!(RRF_K_DEFAULT, 60);
    }

    #[test]
    fn alternate_k_changes_scores() {
        // k=1 → rank 1 contribution is 1/2. Sanity check the constant
        // is actually wired in.
        let bucket = bucket_no_floor(vec![cand("a", 1.0)]);
        let out = fuse(&[bucket], 1, 10);
        assert!((out[0].rrf_score - 0.5).abs() < 1e-12);
    }

    // ---- LIMIT total_k ------------------------------------------------

    #[test]
    fn total_k_caps_output() {
        let bucket = bucket_no_floor(vec![
            cand("a", 1.0),
            cand("b", 0.9),
            cand("c", 0.8),
            cand("d", 0.7),
        ]);
        let out = fuse(&[bucket], 60, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[1].id, "b");
    }

    #[test]
    fn total_k_zero_returns_empty() {
        let bucket = bucket_no_floor(vec![cand("a", 1.0)]);
        let out = fuse(&[bucket], 60, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn total_k_larger_than_candidates_returns_all() {
        let bucket = bucket_no_floor(vec![cand("a", 1.0), cand("b", 0.5)]);
        let out = fuse(&[bucket], 60, 100);
        assert_eq!(out.len(), 2);
    }

    // ---- MIN_SCORE per-bucket -----------------------------------------

    #[test]
    fn min_score_drops_items_before_ranking() {
        // 'b' fails the 0.5 floor — must be dropped, AND 'c' must
        // then be promoted to rank 2 (not rank 3).
        let bucket = Bucket {
            candidates: vec![cand("a", 0.9), cand("b", 0.4), cand("c", 0.6)],
            min_score: Some(0.5),
        };
        let out = fuse(&[bucket], 60, 10);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a");
        assert!((out[0].rrf_score - 1.0 / 61.0).abs() < 1e-12);
        assert_eq!(out[1].id, "c");
        // c is rank 2 after filter, not rank 3.
        assert!((out[1].rrf_score - 1.0 / 62.0).abs() < 1e-12);
    }

    #[test]
    fn min_score_independent_per_bucket() {
        // bm25-bucket uses min_score 0.4, vector-bucket uses 0.7.
        let bm25 = Bucket {
            candidates: vec![cand("x", 0.5), cand("y", 0.3)],
            min_score: Some(0.4),
        };
        let vec_bucket = Bucket {
            candidates: vec![cand("x", 0.85), cand("y", 0.6)],
            min_score: Some(0.7),
        };
        let out = fuse(&[bm25, vec_bucket], 60, 10);
        // y is dropped from vector bucket (0.6 < 0.7) and from bm25
        // bucket (0.3 < 0.4). x survives both → 2/61.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "x");
        assert!((out[0].rrf_score - 2.0 / 61.0).abs() < 1e-12);
    }

    #[test]
    fn min_score_none_keeps_everything() {
        let bucket = bucket_no_floor(vec![cand("a", -10.0), cand("b", 0.0)]);
        let out = fuse(&[bucket], 60, 10);
        assert_eq!(out.len(), 2);
    }

    // ---- Tie-break determinism ----------------------------------------

    #[test]
    fn tie_break_is_id_ascending() {
        // Three items each appearing once at rank 1 across three
        // separate buckets — all share the same rrf_score 1/61.
        let b1 = bucket_no_floor(vec![cand("zebra", 1.0)]);
        let b2 = bucket_no_floor(vec![cand("apple", 1.0)]);
        let b3 = bucket_no_floor(vec![cand("mango", 1.0)]);
        let out = fuse(&[b1, b2, b3], 60, 10);
        assert_eq!(
            out.iter().map(|f| f.id).collect::<Vec<_>>(),
            vec!["apple", "mango", "zebra"]
        );
    }

    #[test]
    fn fuse_is_deterministic_across_calls() {
        // Same inputs → byte-equal outputs. Required by the ASK
        // determinism contract (#400).
        let b1 = bucket_no_floor(vec![cand("a", 1.0), cand("b", 0.5)]);
        let b2 = bucket_no_floor(vec![cand("b", 0.9), cand("c", 0.4)]);
        let a = fuse(&[b1.clone(), b2.clone()], 60, 10);
        let c = fuse(&[b1, b2], 60, 10);
        assert_eq!(a, c);
    }

    #[test]
    fn fuse_is_order_independent_across_buckets() {
        // The bucket order on input should not affect the fused
        // output — RRF is commutative across rankers.
        let b1 = bucket_no_floor(vec![cand("a", 1.0), cand("b", 0.5)]);
        let b2 = bucket_no_floor(vec![cand("b", 0.9), cand("c", 0.4)]);
        let forward = fuse(&[b1.clone(), b2.clone()], 60, 10);
        let reverse = fuse(&[b2, b1], 60, 10);
        assert_eq!(forward, reverse);
    }

    // ---- Edge cases ---------------------------------------------------

    #[test]
    fn empty_buckets_returns_empty() {
        let buckets: Vec<Bucket<&'static str>> = vec![];
        let out = fuse(&buckets, 60, 10);
        assert!(out.is_empty());
    }

    #[test]
    fn all_empty_buckets_returns_empty() {
        let buckets: Vec<Bucket<&'static str>> =
            vec![bucket_no_floor(vec![]), bucket_no_floor(vec![])];
        let out = fuse(&buckets, 60, 10);
        assert!(out.is_empty());
    }

    #[test]
    fn duplicate_id_within_one_bucket_keeps_both_ranks() {
        // Realistically a retriever should not emit the same id twice,
        // but if it does, the later occurrence keeps a lower rank.
        // Document the behavior so it isn't a future surprise: both
        // contributions accumulate.
        let bucket = bucket_no_floor(vec![cand("a", 1.0), cand("a", 0.5)]);
        let out = fuse(&[bucket], 60, 10);
        assert_eq!(out.len(), 1);
        assert!((out[0].rrf_score - (1.0 / 61.0 + 1.0 / 62.0)).abs() < 1e-12);
    }

    #[test]
    fn integer_ids_supported() {
        // The fuser is generic over id type; ints are valid.
        let b1 = bucket_no_floor(vec![cand(1u64, 1.0), cand(2u64, 0.5)]);
        let b2 = bucket_no_floor(vec![cand(2u64, 0.9), cand(3u64, 0.4)]);
        let out = fuse(&[b1, b2], 60, 10);
        assert_eq!(out[0].id, 2);
    }
}
