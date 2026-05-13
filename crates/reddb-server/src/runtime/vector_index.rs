use std::cmp::Ordering;

use crate::storage::engine::distance::{distance, DistanceMetric};
use crate::storage::{EntityId, SimilarResult, UnifiedEntity};

#[derive(Debug, Clone)]
pub(crate) struct VectorIndexEntry {
    pub entity_id: EntityId,
    pub vector: Vec<f32>,
    pub entity: UnifiedEntity,
}

#[derive(Debug, Default)]
pub(crate) struct BruteForceVectorIndex {
    entries: Vec<VectorIndexEntry>,
}

impl BruteForceVectorIndex {
    pub(crate) fn upsert(&mut self, entry: VectorIndexEntry) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|existing| existing.entity_id == entry.entity_id)
        {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
    }

    pub(crate) fn delete(&mut self, entity_id: EntityId) {
        self.entries.retain(|entry| entry.entity_id != entity_id);
    }

    pub(crate) fn search(
        &self,
        query: &[f32],
        k: usize,
        metric: DistanceMetric,
        threshold: Option<f32>,
    ) -> Vec<SimilarResult> {
        let mut results: Vec<SimilarResult> = self
            .entries
            .iter()
            .filter(|entry| entry.vector.len() == query.len())
            .filter_map(|entry| {
                let raw_distance = distance(query, &entry.vector, metric);
                let score = vector_score(raw_distance, metric);
                if !within_threshold(score, raw_distance, metric, threshold) {
                    return None;
                }
                Some(SimilarResult {
                    entity_id: entry.entity_id,
                    score,
                    distance: raw_distance,
                    entity: entry.entity.clone(),
                })
            })
            .collect();

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
}

fn vector_score(distance: f32, metric: DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::Cosine => 1.0 - distance,
        DistanceMetric::InnerProduct | DistanceMetric::L2 => -distance,
    }
}

fn within_threshold(
    score: f32,
    distance: f32,
    metric: DistanceMetric,
    threshold: Option<f32>,
) -> bool {
    let Some(threshold) = threshold else {
        return true;
    };
    match metric {
        DistanceMetric::L2 => distance <= threshold,
        DistanceMetric::Cosine | DistanceMetric::InnerProduct => score >= threshold,
    }
}
