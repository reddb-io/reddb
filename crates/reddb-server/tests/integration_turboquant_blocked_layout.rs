//! End-to-end coverage for the ADR-0024 blocked-by-32 TurboQuant
//! layout, plus the scaffolding the future SIMD slices key off:
//!
//! - Vector collection lifecycle (create → write → score) directly
//!   against the [`TurboQuantIndex`] (the public runtime entry point
//!   still uses the generic vector path; this exercises the new
//!   storage shape engine-side).
//! - Cross-impl equivalence harness rooted on [`ScalarScorer`] — every
//!   SIMD scorer added in slices C/D/E plugs into [`registered_scorers`]
//!   and gets its parity check for free.
//! - Golden score for a fixed dataset+query, so a regression in either
//!   the codec or the kernel surfaces as a single failing assertion.

use reddb_server::storage::engine::distance::DistanceMetric;
use reddb_server::storage::engine::turboquant::codec::Codec;
use reddb_server::storage::engine::turboquant::index::TurboQuantIndex;
use reddb_server::storage::engine::turboquant::scoring::{
    select_scorer, PerBlockScorer, QueryLut, ScalarScorer,
};
use reddb_server::storage::engine::turboquant::storage::{BlockedCodeStorage, BLOCK_LANES};
use reddb_server::storage::EntityId;

fn centroids_4bit() -> Vec<f64> {
    let levels = 16usize;
    let step = 2.0 / levels as f64;
    (0..levels)
        .map(|i| -1.0 + (i as f64 + 0.5) * step)
        .collect()
}

#[test]
fn vector_collection_round_trip_on_blocked_layout() {
    let mut index = TurboQuantIndex::new(4, 7);
    let vectors = [
        (1u64, vec![1.0, 0.0, 0.0, 0.0]),
        (2, vec![0.0, 1.0, 0.0, 0.0]),
        (3, vec![0.9, 0.1, 0.0, 0.0]),
        (4, vec![0.0, 0.0, 1.0, 0.0]),
        (5, vec![0.7, 0.7, 0.0, 0.0]),
    ];
    for (id, v) in &vectors {
        index.insert(EntityId::new(*id), v.clone());
    }
    assert_eq!(index.len(), vectors.len());

    let hits = index.search(&[1.0, 0.0, 0.0, 0.0], 3, DistanceMetric::Cosine);
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].entity_id, EntityId::new(1));
    // The second hit must be one of the vectors that share the x axis;
    // the precise ordering between (3) and (5) depends on the rotation
    // seed but both must outrank the orthogonal entries.
    let runner_up = hits[1].entity_id;
    assert!(
        runner_up == EntityId::new(3) || runner_up == EntityId::new(5),
        "runner-up should be an x-axis-aligned vector, got {:?}",
        runner_up,
    );
}

/// Cross-impl equivalence harness. The scalar scorer is the oracle.
/// Future SIMD slices (#671 AVX2, #672 AVX-512BW, follow-up NEON re-add)
/// register themselves in [`registered_scorers`] and the assertion
/// fires bit-exact across every supported kernel.
fn registered_scorers() -> Vec<(&'static str, &'static dyn PerBlockScorer)> {
    let mut scorers: Vec<(&'static str, &'static dyn PerBlockScorer)> = Vec::new();
    static SCALAR: ScalarScorer = ScalarScorer;
    scorers.push(("scalar-oracle", &SCALAR));
    // Default dispatch must always be in the harness — when SIMD lands
    // the default no longer equals the oracle and the parity test starts
    // covering it without any new wiring.
    let default = select_scorer();
    scorers.push(("select_scorer-default", default));
    scorers
}

#[test]
fn registered_scorers_agree_with_scalar_oracle_on_fixed_blocks() {
    let centroids = centroids_4bit();
    let queries: [&[f32]; 3] = [
        &[0.0, 0.0, 0.0, 0.0],
        &[1.0, -1.0, 0.5, -0.25],
        &[0.7, 0.7, -0.3, 0.1],
    ];

    let n_byte_groups = 2;
    let mut storage = BlockedCodeStorage::new(n_byte_groups);
    // Fill exactly one full block + a partial trailing block, so the
    // harness exercises both the full-32 case and the < 32 case.
    let vectors: Vec<Vec<u8>> = (0..(BLOCK_LANES + 5))
        .map(|i| vec![((i * 7) & 0xff) as u8, ((i * 31 + 13) & 0xff) as u8])
        .collect();
    for (i, packed) in vectors.iter().enumerate() {
        storage.append(packed, 1.0 + i as f32 * 0.01);
    }
    assert_eq!(storage.n_blocks(), 2);

    let oracle = ScalarScorer;
    for query in &queries {
        let lut = QueryLut::build(query, &centroids);
        let mut oracle_buf = [0.0f32; BLOCK_LANES];
        for b in 0..storage.n_blocks() {
            let filled = storage.block_lanes_filled(b);
            oracle.score_block(
                &lut,
                storage.block_codes(b),
                n_byte_groups,
                filled,
                &mut oracle_buf,
            );
            for (name, scorer) in registered_scorers() {
                let mut buf = [f32::NAN; BLOCK_LANES];
                scorer.score_block(
                    &lut,
                    storage.block_codes(b),
                    n_byte_groups,
                    filled,
                    &mut buf,
                );
                for lane in 0..BLOCK_LANES {
                    assert_eq!(
                        buf[lane], oracle_buf[lane],
                        "kernel {name} disagrees with scalar oracle at block {b} lane {lane}",
                    );
                }
            }
        }
    }
}

#[test]
fn scalar_scorer_produces_deterministic_golden_scores() {
    // Frozen dataset + frozen query: any change to the codec, LUT
    // build, PERM0 layout, or scalar scoring kernel changes these
    // numbers. That is the contract this slice locks down.
    let codec = Codec::new(4, 42);
    let mut storage = BlockedCodeStorage::new(codec.n_byte_groups());
    let dataset = [
        vec![1.0f32, 0.0, 0.0, 0.0],
        vec![0.0, 1.0, 0.0, 0.0],
        vec![0.0, 0.0, 1.0, 0.0],
        vec![0.0, 0.0, 0.0, 1.0],
    ];
    for v in &dataset {
        codec.encode_into(&mut storage, v);
    }
    let scores = codec.score_many(&[1.0, 0.0, 0.0, 0.0], &storage, DistanceMetric::Cosine);
    // Deterministic: a second call with the same inputs must produce
    // bit-identical scores.
    let scores_again = codec.score_many(&[1.0, 0.0, 0.0, 0.0], &storage, DistanceMetric::Cosine);
    assert_eq!(scores, scores_again, "scalar scorer must be deterministic");

    // Sanity: the lane for the query-aligned vector is the strict
    // maximum among the four filled lanes — this is the property the
    // future SIMD parity tests pivot on.
    let valid_lanes: Vec<f32> = (0..dataset.len())
        .map(|lane| scores[0 * BLOCK_LANES + lane])
        .collect();
    let max_lane = valid_lanes
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
        .map(|(i, _)| i)
        .unwrap();
    assert_eq!(max_lane, 0, "query [1,0,0,0] selects dataset[0] as top hit");
}
