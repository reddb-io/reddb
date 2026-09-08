use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::storage::{EntityId, SimilarResult, UnifiedEntity};

/// A bounded selection heap. The worst retained result is at the root.
/// Ties prefer the smaller physical entity id, independent of scan order.
pub(crate) struct VectorTopK {
    limit: usize,
    entries: BinaryHeap<RankedVector>,
}

struct RankedVector(SimilarResult);

fn rank(left_score: f32, left_id: EntityId, right_score: f32, right_id: EntityId) -> Ordering {
    if left_score == right_score {
        left_id.raw().cmp(&right_id.raw())
    } else {
        right_score
            .total_cmp(&left_score)
            .then_with(|| left_id.raw().cmp(&right_id.raw()))
    }
}

impl PartialEq for RankedVector {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for RankedVector {}
impl PartialOrd for RankedVector {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RankedVector {
    fn cmp(&self, other: &Self) -> Ordering {
        rank(
            self.0.score,
            self.0.entity_id,
            other.0.score,
            other.0.entity_id,
        )
    }
}

impl VectorTopK {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            entries: BinaryHeap::new(),
        }
    }

    pub(crate) fn consider(&mut self, entity: &UnifiedEntity, score: f32, distance: f32) {
        if self.limit == 0 {
            return;
        }
        if self.entries.len() == self.limit {
            let worst = self.entries.peek().expect("nonempty bounded heap");
            if !rank(score, entity.id, worst.0.score, worst.0.entity_id).is_lt() {
                return;
            }
            self.entries.pop();
        }
        self.entries.push(RankedVector(SimilarResult {
            entity_id: entity.id,
            score,
            distance,
            entity: entity.clone(),
        }));
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn finish(self) -> Vec<SimilarResult> {
        self.entries
            .into_sorted_vec()
            .into_iter()
            .map(|entry| entry.0)
            .collect()
    }
}
