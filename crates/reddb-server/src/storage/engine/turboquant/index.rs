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
use std::collections::HashMap;

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
    positions: HashMap<EntityId, usize>,
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
            positions: HashMap::new(),
            handles: Vec::new(),
            vectors: Vec::new(),
        }
    }

    pub fn insert(&mut self, entity_id: EntityId, vector: Vec<f32>) {
        if let Some(&pos) = self.positions.get(&entity_id) {
            // Reconciliation commonly sees the same vector in the snapshot,
            // entity store and WAL. Only re-encode if its value has changed.
            if self.vectors[pos] != vector {
                self.handles[pos] = self.codec.encode_into(&mut self.storage, &vector);
            }
            self.vectors[pos] = vector;
            return;
        }
        let handle = self.codec.encode_into(&mut self.storage, &vector);
        self.positions.insert(entity_id, self.ids.len());
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
        match self.search_checked(query, k, metric, |_| Ok::<(), std::convert::Infallible>(())) {
            Ok(results) => results,
            Err(never) => match never {},
        }
    }

    pub(crate) fn search_checked<E>(
        &self,
        query: &[f32],
        k: usize,
        metric: DistanceMetric,
        mut check: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Vec<TurboSearchResult>, E> {
        if query.len() != self.codec.dim() {
            return Ok(Vec::new());
        }

        let scores = self
            .codec
            .score_many_checked(query, &self.storage, metric, &mut check)?;
        let mut results = self
            .ids
            .iter()
            .zip(&self.handles)
            .zip(&self.vectors)
            .filter(|((_, _), vector)| vector.len() == query.len())
            .map(|((entity_id, handle), _)| {
                check(1)?;
                let idx = handle.block_idx as usize * BLOCK_LANES + handle.lane as usize;
                Ok(TurboSearchResult {
                    entity_id: *entity_id,
                    score: scores[idx],
                })
            })
            .collect::<Result<Vec<_>, E>>()?;
        check(0)?;
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.entity_id.raw().cmp(&right.entity_id.raw()))
        });
        results.truncate(k);
        check(0)?;
        Ok(results)
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

    #[test]
    fn replacing_a_vector_updates_its_search_code() {
        let mut index = TurboQuantIndex::new(2, 1);
        index.insert(EntityId::new(1), vec![0.0, 1.0]);
        index.insert(EntityId::new(2), vec![0.0, 1.0]);
        index.insert(EntityId::new(1), vec![1.0, 0.0]);
        index.insert(EntityId::new(1), vec![1.0, 0.0]);
        assert_eq!(index.len(), 2);
        let results = index.search(&[1.0, 0.0], 2, DistanceMetric::Cosine);
        assert_eq!(results[0].entity_id, EntityId::new(1));
        assert!(results[0].score > results[1].score);
    }
}
