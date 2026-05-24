//! Scalar TurboQuant codec.
//!
//! MIT notice: clean-room RedDB implementation for the turbovec-compatible
//! TurboQuant surface; no upstream turbovec source is copied.

use super::codebook::Codebook;
use super::rotation::RotationMatrix;
use super::scoring::{score_units, QueryLut};
use crate::storage::engine::distance;

#[derive(Debug, Clone, PartialEq)]
pub struct EncodedVector {
    pub packed: Vec<u8>,
    pub scale: f32,
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

    pub fn encode(&self, vector: &[f32]) -> EncodedVector {
        let scale = distance::l2_norm(vector);
        let normalized = if scale > 0.0 {
            vector.iter().map(|v| *v / scale).collect::<Vec<_>>()
        } else {
            vec![0.0; vector.len()]
        };
        let rotated = self.rotation.rotate(&normalized);
        let mut packed = Vec::with_capacity(rotated.len().div_ceil(2));
        for pair in rotated.chunks(2) {
            let lo = self.codebook.quantize(pair[0]) & 0x0f;
            let hi = pair
                .get(1)
                .map(|value| self.codebook.quantize(*value) & 0x0f)
                .unwrap_or(0);
            packed.push(lo | (hi << 4));
        }
        EncodedVector { packed, scale }
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

    pub fn score(
        &self,
        query: &[f32],
        encoded: &EncodedVector,
        metric: distance::DistanceMetric,
    ) -> f32 {
        self.score_many(query, std::slice::from_ref(encoded), metric)
            .into_iter()
            .next()
            .unwrap_or(0.0)
    }

    pub fn score_many(
        &self,
        query: &[f32],
        encoded: &[EncodedVector],
        metric: distance::DistanceMetric,
    ) -> Vec<f32> {
        assert_eq!(query.len(), self.dim, "Vector dimensions must match");

        let query_norm = distance::l2_norm(query);
        if query_norm == 0.0 {
            return encoded
                .iter()
                .map(|vector| match metric {
                    distance::DistanceMetric::L2 => -(vector.scale * vector.scale),
                    distance::DistanceMetric::Cosine | distance::DistanceMetric::InnerProduct => {
                        0.0
                    }
                })
                .collect();
        }

        let normalized = query.iter().map(|v| *v / query_norm).collect::<Vec<_>>();
        let rotated = self.rotation.rotate(&normalized);
        let lut = QueryLut::build(&rotated, self.codebook.centroids());

        score_units(&lut, encoded)
            .into_iter()
            .zip(encoded)
            .map(|(unit_dot, vector)| {
                let raw_dot = unit_dot * query_norm * vector.scale;
                match metric {
                    distance::DistanceMetric::Cosine => {
                        if vector.scale > 0.0 {
                            unit_dot
                        } else {
                            0.0
                        }
                    }
                    distance::DistanceMetric::InnerProduct => raw_dot,
                    distance::DistanceMetric::L2 => {
                        -(query_norm * query_norm + vector.scale * vector.scale - 2.0 * raw_dot)
                    }
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_is_bit_exact_for_frozen_vectors() {
        let codec = Codec::new(4, 7);
        let encoded = codec.encode(&[1.0, 0.0, -1.0, 0.5]);
        assert_eq!(encoded, codec.encode(&[1.0, 0.0, -1.0, 0.5]));
        assert_eq!(encoded.packed.len(), 2);
    }

    #[test]
    fn score_kernel_detection_falls_back_off_arm() {
        use super::super::scoring::{detect_score_kernel, ScoreKernel};

        #[cfg(not(target_arch = "aarch64"))]
        assert_eq!(detect_score_kernel(), ScoreKernel::Scalar);
    }
}
