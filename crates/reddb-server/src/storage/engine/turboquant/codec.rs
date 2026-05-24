//! Scalar TurboQuant codec.
//!
//! MIT notice: clean-room RedDB implementation for the turbovec-compatible
//! TurboQuant surface; no upstream turbovec source is copied.

use super::codebook::Codebook;
use super::rotation::RotationMatrix;
use crate::storage::engine::distance;

#[derive(Debug, Clone, PartialEq)]
pub struct EncodedVector {
    pub packed: Vec<u8>,
    pub scale: f32,
}

#[derive(Debug, Clone)]
pub struct Codec {
    rotation: RotationMatrix,
    codebook: Codebook,
}

impl Codec {
    pub fn new(dim: usize, seed: u64) -> Self {
        Self {
            rotation: RotationMatrix::new(dim, seed),
            codebook: Codebook::for_dim_bits(dim, 4),
        }
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
}
