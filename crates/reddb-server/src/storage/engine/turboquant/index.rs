//! Scalar TurboQuant index composition.
//!
//! Owns the blocked-by-32 [`BlockedCodeStorage`] plus the id and raw
//! vector tables. Search routes through [`Codec::score_many`], which
//! dispatches to the scalar [`super::scoring::PerBlockScorer`] today
//! and will admit SIMD kernels in later slices without further
//! changes here.

use super::codec::{Codec, EncodedVector};
use super::storage::{BlockedCodeStorage, BLOCK_LANES};
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
    storage: BlockedCodeStorage,
    ids: Vec<EntityId>,
    handles: Vec<EncodedVector>,
    vectors: Vec<Vec<f32>>,
}

impl TurboQuantIndex {
    pub fn new(dim: usize, seed: u64) -> Self {
        let codec = Codec::new(dim, seed);
        let storage = BlockedCodeStorage::new(codec.n_byte_groups());
        Self {
            codec,
            storage,
            ids: Vec::new(),
            handles: Vec::new(),
            vectors: Vec::new(),
        }
    }

    pub fn insert(&mut self, entity_id: EntityId, vector: Vec<f32>) {
        // In this slice the storage layer is append-only — there is no
        // in-place lane rewrite yet (background rebuild is owned by
        // PRD #688 / issue #673). Duplicate ids are accepted but only
        // their latest raw vector is remembered; the search filter
        // below picks them up by handle.
        if let Some(pos) = self.ids.iter().position(|id| *id == entity_id) {
            self.vectors[pos] = vector;
            return;
        }
        let handle = self.codec.encode_into(&mut self.storage, &vector);
        self.ids.push(entity_id);
        self.handles.push(handle);
        self.vectors.push(vector);
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

        let scores = self.codec.score_many(query, &self.storage, metric);
        let mut results = self
            .ids
            .iter()
            .zip(&self.handles)
            .zip(&self.vectors)
            .filter(|((_, _), vector)| vector.len() == query.len())
            .map(|((entity_id, handle), _)| {
                let idx = handle.block_idx as usize * BLOCK_LANES + handle.lane as usize;
                TurboSearchResult {
                    entity_id: *entity_id,
                    score: scores[idx],
                }
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

    /// Encode `vector` to the packed-codes + LE f32-norm byte layout
    /// that the TurboExtent persists. Used by the INSERT path
    /// (#693) so the extent and the in-memory `BlockedCodeStorage`
    /// see the same bytes for the same vector.
    pub fn encode_for_extent(&self, vector: &[f32]) -> Vec<u8> {
        let (mut packed, scale) = self.codec.encode_packed(vector);
        packed.extend_from_slice(&scale.to_le_bytes());
        packed
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn dim(&self) -> usize {
        self.codec.dim()
    }

    /// `(entity_id, vector)` pairs in insertion order — the order
    /// `insert` saw, which is also the order block/lane placement was
    /// chosen. Feeding the same sequence into a fresh
    /// `TurboQuantIndex::new(dim, seed)` reproduces byte-identical
    /// encoded codes. Used by the `.tv` snapshot writer (#674).
    pub fn iter_persisted(&self) -> impl Iterator<Item = (crate::storage::EntityId, &[f32])> {
        self.ids
            .iter()
            .copied()
            .zip(self.vectors.iter().map(|v| v.as_slice()))
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
