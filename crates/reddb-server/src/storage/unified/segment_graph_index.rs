//! Physical candidates only: MVCC and RLS belong to the read boundary.
use super::entity::{EntityId, EntityKind, UnifiedEntity};
use super::segment::GraphEntityKind;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct SegmentGraphIndex {
    nodes: BTreeSet<(EntityId, EntityId)>,
    edges: BTreeSet<(EntityId, EntityId)>,
}

impl SegmentGraphIndex {
    pub(super) fn insert(&mut self, entity: &UnifiedEntity) {
        match &entity.kind {
            EntityKind::GraphNode(_) => {
                self.nodes.insert((entity.logical_id(), entity.id));
            }
            EntityKind::GraphEdge(edge) => {
                for endpoint in [&edge.from_node, &edge.to_node] {
                    if let Ok(endpoint) = endpoint.parse::<u64>() {
                        self.edges.insert((EntityId::new(endpoint), entity.id));
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn remove(&mut self, entity: &UnifiedEntity) {
        match &entity.kind {
            EntityKind::GraphNode(_) => {
                self.nodes.remove(&(entity.logical_id(), entity.id));
            }
            EntityKind::GraphEdge(edge) => {
                for endpoint in [&edge.from_node, &edge.to_node] {
                    if let Ok(endpoint) = endpoint.parse::<u64>() {
                        self.edges.remove(&(EntityId::new(endpoint), entity.id));
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn candidates(
        &self,
        kind: GraphEntityKind,
        key: EntityId,
    ) -> impl DoubleEndedIterator<Item = EntityId> + '_ {
        self.candidates_after(kind, key, None, EntityId::new(u64::MAX))
    }

    pub(super) fn candidates_after(
        &self,
        kind: GraphEntityKind,
        key: EntityId,
        after: Option<EntityId>,
        end: EntityId,
    ) -> impl DoubleEndedIterator<Item = EntityId> + '_ {
        use std::ops::Bound::{Excluded, Included};
        let index = match kind {
            GraphEntityKind::Node => &self.nodes,
            GraphEntityKind::Edge => &self.edges,
        };
        let start = after.map_or(Included((key, EntityId::new(0))), |id| Excluded((key, id)));
        index
            .range((start, Included((key, end))))
            .map(|(_, id)| *id)
    }

    pub(super) fn memory_bytes(&self) -> u64 {
        // Conservative estimate for 16-byte pairs plus B-tree occupancy slack
        // and node headers. This is accounting, not a hard allocation limit.
        (self.nodes.len() + self.edges.len()) as u64 * 64
    }

    pub(super) fn same_keys(old: &UnifiedEntity, new: &UnifiedEntity) -> bool {
        match (&old.kind, &new.kind) {
            (EntityKind::GraphNode(_), EntityKind::GraphNode(_)) => {
                old.logical_id() == new.logical_id()
            }
            (EntityKind::GraphEdge(old), EntityKind::GraphEdge(new)) => {
                old.from_node == new.from_node && old.to_node == new.to_node
            }
            (EntityKind::GraphNode(_) | EntityKind::GraphEdge(_), _)
            | (_, EntityKind::GraphNode(_) | EntityKind::GraphEdge(_)) => false,
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::unified::segment::{GrowingSegment, UnifiedSegment};
    use std::collections::HashMap;

    fn node(id: u64, logical: u64) -> UnifiedEntity {
        let mut node = UnifiedEntity::graph_node(EntityId::new(id), "node", "Node", HashMap::new());
        node.set_logical_id(EntityId::new(logical));
        node
    }

    fn candidates(segment: &GrowingSegment, kind: GraphEntityKind, key: u64) -> Vec<u64> {
        let mut ids = Vec::new();
        assert!(segment.visit_graph_index_candidates(
            kind,
            &[EntityId::new(key)],
            &mut || true,
            &mut |entity| {
                ids.push(entity.id.raw());
                true
            }
        ));
        ids
    }

    #[test]
    fn graph_index_cursor_seeks_once_per_batch_and_stops_at_opened_upper_id() {
        use crate::storage::unified::segment::SCAN_BATCH_SIZE;
        for maximum_id in [false, true] {
            let mut segment = GrowingSegment::new(1, "graph");
            segment
                .bulk_insert((1..=1025).map(|id| node(id, 10)).collect())
                .expect("history");
            if maximum_id {
                segment.insert(node(u64::MAX, 10)).expect("maximum ID");
            }
            let expected = candidates(&segment, GraphEntityKind::Node, 10);
            let keys = [EntityId::new(9), EntityId::new(10), EntityId::new(11)];
            let mut cursor = segment
                .graph_index_cursor(GraphEntityKind::Node, &keys, &mut || true)
                .expect("cursor");
            let mut actual = Vec::new();
            let mut work = 0;
            let mut batches = 0;
            while !cursor.finished(&keys) {
                let before = actual.len();
                assert!(segment.graph_index_batch(
                    GraphEntityKind::Node,
                    &keys,
                    &mut cursor,
                    &mut || {
                        work += 1;
                        true
                    },
                    &mut |id| actual.push(id.raw())
                ));
                assert!(actual.len() - before <= SCAN_BATCH_SIZE);
                batches += 1;
                if batches == 1 && !maximum_id {
                    segment
                        .insert(node(2000, 10))
                        .expect("append above opened bound");
                    segment.seal().expect("seal between batches");
                }
            }
            assert_eq!(actual, expected);
            assert_eq!(batches, 5);
            assert!(work <= expected.len() + 10, "no prefix rescans: {work}");
        }
    }

    #[test]
    fn graph_index_cursor_bounds_later_aliases_before_consuming_first_batch() {
        let mut segment = GrowingSegment::new(1, "graph");
        segment
            .bulk_insert((1..=1024).map(|id| node(id, 10)).collect())
            .expect("first key");
        segment.insert(node(2000, 20)).expect("later key");
        let keys = [EntityId::new(10), EntityId::new(20)];
        let mut cursor = segment
            .graph_index_cursor(GraphEntityKind::Node, &keys, &mut || true)
            .expect("capture all keys");
        let mut actual = Vec::new();
        assert!(segment.graph_index_batch(
            GraphEntityKind::Node,
            &keys,
            &mut cursor,
            &mut || true,
            &mut |id| actual.push(id.raw())
        ));
        segment
            .insert(node(3000, 20))
            .expect("append to unopened key");
        while !cursor.finished(&keys) {
            assert!(segment.graph_index_batch(
                GraphEntityKind::Node,
                &keys,
                &mut cursor,
                &mut || true,
                &mut |id| actual.push(id.raw())
            ));
        }
        assert_eq!(actual, (1..=1024).chain([2000]).collect::<Vec<_>>());
    }

    #[test]
    fn graph_index_keeps_history_through_bulk_seal_update_and_reclamation() {
        let mut segment = GrowingSegment::new(1, "graph");
        let edge =
            UnifiedEntity::graph_edge(EntityId::new(50), "step", "010", "20", 1.0, HashMap::new());
        segment
            .bulk_insert(vec![node(10, 10), node(20, 10), edge])
            .expect("mixed bulk with gaps");
        assert_eq!(candidates(&segment, GraphEntityKind::Node, 10), [10, 20]);
        assert_eq!(candidates(&segment, GraphEntityKind::Edge, 10), [50]);
        assert_eq!(candidates(&segment, GraphEntityKind::Edge, 20), [50]);
        segment.seal().expect("seal");
        let mut hidden = node(20, 10);
        hidden.set_xmax(99);
        segment
            .force_update_with_metadata(&hidden, &[], None)
            .expect("retain history");
        assert_eq!(
            candidates(&segment, GraphEntityKind::Node, 10),
            [10, 20],
            "index does not discard snapshot candidates"
        );
        let replacement =
            UnifiedEntity::graph_edge(EntityId::new(50), "step", "30", "30", 1.0, HashMap::new());
        segment
            .force_update_with_metadata(&replacement, &[], None)
            .expect("structural replacement");
        assert!(candidates(&segment, GraphEntityKind::Edge, 10).is_empty());
        assert_eq!(
            candidates(&segment, GraphEntityKind::Edge, 30),
            [50],
            "self-loop once"
        );
        segment
            .force_update_with_metadata(&node(20, 30), &[], None)
            .expect("logical identity replacement");
        assert_eq!(candidates(&segment, GraphEntityKind::Node, 10), [10]);
        assert_eq!(candidates(&segment, GraphEntityKind::Node, 30), [20]);
        assert!(segment.force_delete(EntityId::new(10)));
        assert!(candidates(&segment, GraphEntityKind::Node, 10).is_empty());
        let mut merged = GrowingSegment::new(2, "graph");
        merged.adopt_entity(node(20, 30), None);
        assert_eq!(candidates(&merged, GraphEntityKind::Node, 30), [20]);
        assert!(merged.evict_entity(EntityId::new(20)));
        assert!(candidates(&merged, GraphEntityKind::Node, 30).is_empty());
    }

    #[test]
    fn unrestricted_mutation_rebuild_is_cancellable_and_accounted() {
        let mut segment = GrowingSegment::new(1, "graph");
        segment.insert(node(10, 10)).expect("node");
        segment.insert(node(20, 20)).expect("node");
        *segment.get_mut(EntityId::new(10)).expect("mutable node") = node(10, 30);
        let unindexed_bytes = segment.memory_bytes();
        let mut work = 0;
        let mut returned = 0;
        assert!(!segment.visit_graph_index_candidates(
            GraphEntityKind::Node,
            &[EntityId::new(30)],
            &mut || {
                work += 1;
                work < 2
            },
            &mut |_| {
                returned += 1;
                true
            }
        ));
        assert_eq!(
            returned, 0,
            "a stopped rebuild publishes no partial results"
        );
        assert_eq!(candidates(&segment, GraphEntityKind::Node, 30), [10]);
        assert!(candidates(&segment, GraphEntityKind::Node, 10).is_empty());
        assert!(
            segment.memory_bytes() > unindexed_bytes,
            "rebuilt graph index is accounted"
        );
        segment.delete(EntityId::new(10)).expect("physical delete");
        assert!(candidates(&segment, GraphEntityKind::Node, 30).is_empty());
    }

    #[test]
    fn structural_updates_add_and_remove_graph_keys_in_mixed_segments() {
        let mut segment = GrowingSegment::new(1, "mixed");
        let id = EntityId::new(10);
        segment
            .insert(UnifiedEntity::vector(id, "mixed", vec![1.0]))
            .expect("vector");
        segment.update(node(10, 20)).expect("vector to node");
        assert_eq!(candidates(&segment, GraphEntityKind::Node, 20), [10]);
        segment
            .update(UnifiedEntity::graph_edge(
                id,
                "step",
                "20",
                "30",
                1.0,
                HashMap::new(),
            ))
            .expect("node to edge");
        assert!(candidates(&segment, GraphEntityKind::Node, 20).is_empty());
        assert_eq!(candidates(&segment, GraphEntityKind::Edge, 20), [10]);
        assert_eq!(candidates(&segment, GraphEntityKind::Edge, 30), [10]);
        segment
            .update(UnifiedEntity::vector(id, "mixed", vec![2.0]))
            .expect("edge to vector");
        assert!(candidates(&segment, GraphEntityKind::Edge, 20).is_empty());
        assert!(candidates(&segment, GraphEntityKind::Edge, 30).is_empty());
    }
}
