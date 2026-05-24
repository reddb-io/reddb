//! Scalar TurboQuant index composition.
//!
//! MIT notice: clean-room RedDB implementation for the turbovec-compatible
//! TurboQuant surface; no upstream turbovec source is copied.

use super::codec::{Codec, EncodedVector};
use crate::storage::engine::distance::DistanceMetric;
use crate::storage::EntityId;
use std::cmp::Ordering;

#[derive(Debug, Clone)]
pub struct TurboSearchResult {
    pub entity_id: EntityId,
    pub score: f32,
}

#[derive(Debug)]
pub struct TurboQuantIndex {
    codec: Codec,
    ids: Vec<EntityId>,
    encoded: Vec<EncodedVector>,
    vectors: Vec<Vec<f32>>,
}

impl TurboQuantIndex {
    pub fn new(dim: usize, seed: u64) -> Self {
        Self {
            codec: Codec::new(dim, seed),
            ids: Vec::new(),
            encoded: Vec::new(),
            vectors: Vec::new(),
        }
    }

    pub fn insert(&mut self, entity_id: EntityId, vector: Vec<f32>) {
        let encoded = self.codec.encode(&vector);
        if let Some(pos) = self.ids.iter().position(|id| *id == entity_id) {
            self.encoded[pos] = encoded;
            self.vectors[pos] = vector;
        } else {
            self.ids.push(entity_id);
            self.encoded.push(encoded);
            self.vectors.push(vector);
        }
    }

    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        metric: DistanceMetric,
    ) -> Vec<TurboSearchResult> {
        if query.len() != self.codec.dim() {
            return Vec::new();
        }

        let scores = self.codec.score_many(query, &self.encoded, metric);
        let mut results = self
            .ids
            .iter()
            .zip(&self.vectors)
            .zip(scores)
            .filter(|((_, vector), _)| vector.len() == query.len())
            .map(|((entity_id, _), score)| TurboSearchResult {
                entity_id: *entity_id,
                score,
            })
            .collect::<Vec<_>>();
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.entity_id.raw().cmp(&right.entity_id.raw()))
        });
        results.truncate(k);
        results
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_index_returns_exact_top_k_for_unit_vectors() {
        let mut index = TurboQuantIndex::new(2, 1);
        index.insert(EntityId::new(1), vec![1.0, 0.0]);
        index.insert(EntityId::new(2), vec![0.0, 1.0]);
        index.insert(EntityId::new(3), vec![0.8, 0.2]);
        let results = index.search(&[1.0, 0.0], 2, DistanceMetric::Cosine);
        assert_eq!(results[0].entity_id, EntityId::new(1));
        assert_eq!(results[1].entity_id, EntityId::new(3));
    }
}
