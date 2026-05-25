//! TurboQuant per-vector encode + per-collection score driver.
//!
//! `EncodedVector` is a [`BlockedCodeStorage`] handle — `(block_idx,
//! lane)` — per ADR 0024. The per-vector `Vec<u8>` of the rejected
//! layout is gone; encoded codes live in collection-owned blocked
//! buffers and are scored block-at-a-time through a
//! [`PerBlockScorer`].

use super::codebook::Codebook;
use super::rotation::RotationMatrix;
use super::scoring::{select_scorer, QueryLut};
use super::storage::{BlockHandle, BlockedCodeStorage, BLOCK_LANES};
use crate::storage::engine::distance;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedVector {
    pub block_idx: u32,
    pub lane: u8,
}

impl From<BlockHandle> for EncodedVector {
    fn from(h: BlockHandle) -> Self {
        Self {
            block_idx: h.block_idx,
            lane: h.lane,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Codec {
    dim: usize,
    rotation: RotationMatrix,
    codebook: Codebook,
}

impl Codec {
    pub fn new(dim: usize, seed: u64) -> Self {
        Self {
            dim,
            rotation: RotationMatrix::new(dim, seed),
            codebook: Codebook::for_dim_bits(dim, 4),
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn n_byte_groups(&self) -> usize {
        self.dim.div_ceil(2)
    }

    /// Encode `vector` and append it to `storage`. Returns a handle the
    /// caller can pass back to [`Self::score_many`] (indirectly through
    /// the storage itself).
    pub fn encode_into(&self, storage: &mut BlockedCodeStorage, vector: &[f32]) -> EncodedVector {
        assert_eq!(vector.len(), self.dim, "encode dimension must match codec",);
        let scale = distance::l2_norm(vector);
        let normalized = if scale > 0.0 {
            vector.iter().map(|v| *v / scale).collect::<Vec<_>>()
        } else {
            vec![0.0; vector.len()]
        };
        let rotated = self.rotation.rotate(&normalized);
        let mut packed = vec![0u8; self.n_byte_groups()];
        for (i, pair) in rotated.chunks(2).enumerate() {
            let lo = self.codebook.quantize(pair[0]) & 0x0f;
            let hi = pair
                .get(1)
                .map(|value| self.codebook.quantize(*value) & 0x0f)
                .unwrap_or(0);
            packed[i] = lo | (hi << 4);
        }
        storage.append(&packed, scale).into()
    }

    pub fn scalar_score(
        &self,
        query: &[f32],
        candidate: &[f32],
        metric: distance::DistanceMetric,
    ) -> f32 {
        let raw = distance::distance(query, candidate, metric);
        match metric {
            distance::DistanceMetric::Cosine => 1.0 - raw,
            distance::DistanceMetric::InnerProduct | distance::DistanceMetric::L2 => -raw,
        }
    }

    /// Score `query` against every vector currently in `storage`.
    /// Returns a `Vec<f32>` of length `storage.n_blocks() * BLOCK_LANES`
    /// indexed by `block_idx * BLOCK_LANES + lane`; entries in unused
    /// trailing-block lanes are filled with `f32::NEG_INFINITY`.
    pub fn score_many(
        &self,
        query: &[f32],
        storage: &BlockedCodeStorage,
        metric: distance::DistanceMetric,
    ) -> Vec<f32> {
        assert_eq!(query.len(), self.dim, "Vector dimensions must match");

        let n_blocks = storage.n_blocks();
        let mut scores = vec![f32::NEG_INFINITY; n_blocks * BLOCK_LANES];

        let query_norm = distance::l2_norm(query);
        if query_norm == 0.0 {
            for b in 0..n_blocks {
                let filled = storage.block_lanes_filled(b);
                for lane in 0..filled {
                    let s = storage.lane_scale(b, lane);
                    scores[b * BLOCK_LANES + lane] = match metric {
                        distance::DistanceMetric::L2 => -(s * s),
                        _ => 0.0,
                    };
                }
            }
            return scores;
        }

        let normalized: Vec<f32> = query.iter().map(|v| *v / query_norm).collect();
        let rotated = self.rotation.rotate(&normalized);
        let lut = QueryLut::build(&rotated, self.codebook.centroids());
        let scorer = select_scorer();

        let n_byte_groups = storage.n_byte_groups();
        let mut block_scores = [0.0f32; BLOCK_LANES];
        for b in 0..n_blocks {
            let filled = storage.block_lanes_filled(b);
            scorer.score_block(
                &lut,
                storage.block_codes(b),
                n_byte_groups,
                filled,
                &mut block_scores,
            );
            for lane in 0..filled {
                let unit_dot = block_scores[lane];
                let lane_scale = storage.lane_scale(b, lane);
                let raw_dot = unit_dot * query_norm * lane_scale;
                let metric_score = match metric {
                    distance::DistanceMetric::Cosine => {
                        if lane_scale > 0.0 {
                            unit_dot
                        } else {
                            0.0
                        }
                    }
                    distance::DistanceMetric::InnerProduct => raw_dot,
                    distance::DistanceMetric::L2 => {
                        -(query_norm * query_norm + lane_scale * lane_scale - 2.0 * raw_dot)
                    }
                };
                scores[b * BLOCK_LANES + lane] = metric_score;
            }
        }
        scores
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_is_bit_exact_for_frozen_vectors() {
        let codec = Codec::new(4, 7);
        let mut a = BlockedCodeStorage::new(codec.n_byte_groups());
        let mut b = BlockedCodeStorage::new(codec.n_byte_groups());
        let ha = codec.encode_into(&mut a, &[1.0, 0.0, -1.0, 0.5]);
        let hb = codec.encode_into(&mut b, &[1.0, 0.0, -1.0, 0.5]);
        assert_eq!(ha, hb);
        assert_eq!(
            a.decode_lane(ha.block_idx as usize, ha.lane as usize),
            b.decode_lane(hb.block_idx as usize, hb.lane as usize),
        );
    }

    #[test]
    fn score_many_layout_indexes_by_block_lane() {
        let codec = Codec::new(2, 11);
        let mut storage = BlockedCodeStorage::new(codec.n_byte_groups());
        let h0 = codec.encode_into(&mut storage, &[1.0, 0.0]);
        let h1 = codec.encode_into(&mut storage, &[0.0, 1.0]);
        let scores = codec.score_many(&[1.0, 0.0], &storage, distance::DistanceMetric::Cosine);
        let s0 = scores[h0.block_idx as usize * BLOCK_LANES + h0.lane as usize];
        let s1 = scores[h1.block_idx as usize * BLOCK_LANES + h1.lane as usize];
        // Self-similarity dominates the orthogonal entry under cosine.
        assert!(
            s0 >= s1,
            "expected vector aligned with query to outrank orthogonal one: s0={s0}, s1={s1}",
        );
    }
}
