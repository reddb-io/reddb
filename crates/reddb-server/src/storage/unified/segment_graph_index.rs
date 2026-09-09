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
    ) -> impl Iterator<Item = EntityId> + '_ {
        let index = match kind {
            GraphEntityKind::Node => &self.nodes,
            GraphEntityKind::Edge => &self.edges,
        };
        index
            .range((key, EntityId::new(0))..=(key, EntityId::new(u64::MAX)))
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
