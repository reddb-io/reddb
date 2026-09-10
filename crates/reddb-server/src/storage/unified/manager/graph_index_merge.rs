//! Merge physical graph candidates; visibility and authorization remain at the read boundary.
use super::*;
use crate::storage::unified::segment::{GraphIndexCursor, SCAN_BATCH_SIZE};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

// Bound each source/key read-ahead independently of its incident degree.
const GRAPH_MERGE_BATCH_SIZE: usize = 32;

type GraphSource = (usize, Arc<RwLock<GrowingSegment>>);
type GraphHead = Reverse<(EntityId, usize, usize)>;

struct GraphIndexMergeStream {
    source_index: usize,
    key: EntityId,
    cursor: GraphIndexCursor,
    ids: [EntityId; GRAPH_MERGE_BATCH_SIZE],
    position: usize,
    count: usize,
}

impl SegmentManager {
    /// Emit candidates in physical-ID, collection-ordinal order. Duplicate
    /// occurrences stay adjacent, including across aliases and segments. The
    /// caller decides which collection has the first visible copy before RLS.
    pub(crate) fn visit_ordered_graph_index_batches<E>(
        managers: &[(usize, Arc<Self>)],
        kind: GraphEntityKind,
        keys: &[EntityId],
        mut before_work: impl FnMut() -> bool,
        mut admit: impl FnMut(usize) -> Result<(), E>,
        mut consume: impl FnMut(&[(EntityId, usize)]) -> Result<bool, E>,
    ) -> Result<bool, E> {
        assert!(
            managers.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "collection ordinals follow scope order"
        );
        admit(
            std::mem::size_of::<[(EntityId, usize); SCAN_BATCH_SIZE]>()
                + std::mem::size_of::<GraphIndexMergeStream>(),
        )?;
        let mut sources = Vec::new();
        for (collection, manager) in managers {
            if !manager.capture_graph_merge_sources(
                *collection,
                &mut sources,
                &mut before_work,
                &mut admit,
            )? {
                return Ok(false);
            }
        }
        let mut streams = Vec::new();
        let mut heads = BinaryHeap::new();
        // Capture every key's bound and initial buffer before any consumer can
        // mutate the store. Empty keys do not allocate persistent stream slots.
        for (source_index, (collection, source)) in sources.iter().enumerate() {
            if !source.read().may_contain_graph_kind(kind) {
                continue;
            }
            for key in keys {
                if !before_work() {
                    return Ok(false);
                }
                let stream = {
                    let segment = source.read();
                    let Some(cursor) = segment.graph_index_cursor(
                        kind,
                        std::slice::from_ref(key),
                        &mut before_work,
                    ) else {
                        return Ok(false);
                    };
                    let mut stream = GraphIndexMergeStream {
                        source_index,
                        key: *key,
                        cursor,
                        ids: [EntityId::new(0); GRAPH_MERGE_BATCH_SIZE],
                        position: 0,
                        count: 0,
                    };
                    if !stream.refill(&segment, kind, &mut before_work) {
                        return Ok(false);
                    }
                    stream
                };
                if stream.count == 0 {
                    continue;
                }
                // Includes Vec/heap growth and simultaneous old/new backing.
                admit(
                    4 * (std::mem::size_of::<GraphIndexMergeStream>()
                        + std::mem::size_of::<GraphHead>()),
                )?;
                heads.push(Reverse((stream.ids[0], *collection, streams.len())));
                streams.push(stream);
            }
        }
        let mut output = [(EntityId::new(0), 0); SCAN_BATCH_SIZE];
        let mut count = 0;
        while let Some(Reverse((id, collection, index))) = heads.pop() {
            if !before_work() {
                return Ok(false);
            }
            output[count] = (id, collection);
            count += 1;
            let stream = &mut streams[index];
            stream.position += 1;
            if stream.position == stream.count
                && !stream.cursor.finished(std::slice::from_ref(&stream.key))
            {
                let segment = sources[stream.source_index].1.read();
                if !stream.refill(&segment, kind, &mut before_work) {
                    return Ok(false);
                }
            }
            if stream.position < stream.count {
                heads.push(Reverse((stream.ids[stream.position], collection, index)));
            }
            if count == SCAN_BATCH_SIZE || heads.is_empty() {
                // Cancelled partial buffers never reach the consumer. No source
                // or topology lock is held during admission or delivery.
                if !before_work() || !consume(&output[..count])? {
                    return Ok(false);
                }
                count = 0;
            }
        }
        Ok(before_work())
    }

    fn capture_graph_merge_sources<E>(
        &self,
        collection: usize,
        sources: &mut Vec<GraphSource>,
        before_work: &mut impl FnMut() -> bool,
        admit: &mut impl FnMut(usize) -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut admitted = 0;
        loop {
            if !before_work() {
                return Ok(false);
            }
            let count = {
                let growing = self.growing.read();
                let sealed = self.sealed.read();
                sealed.len() + usize::from(growing.is_some())
            };
            let bytes = count.saturating_mul(4 * std::mem::size_of::<GraphSource>());
            admit(bytes.saturating_sub(admitted))?;
            admitted = admitted.max(bytes);
            sources.reserve_exact(count);
            let growing = self.growing.read();
            let sealed = self.sealed.read();
            if sources.len() + sealed.len() + usize::from(growing.is_some()) > sources.capacity() {
                continue;
            }
            for source in growing.iter().chain(sealed.iter()) {
                if !before_work() {
                    return Ok(false);
                }
                sources.push((collection, Arc::clone(source)));
            }
            return Ok(true);
        }
    }
}

impl GraphIndexMergeStream {
    fn refill(
        &mut self,
        segment: &GrowingSegment,
        kind: GraphEntityKind,
        before_work: &mut impl FnMut() -> bool,
    ) -> bool {
        self.position = 0;
        self.count = 0;
        if !segment.may_contain_graph_kind(kind) {
            return true;
        }
        let ids = &mut self.ids;
        let count = &mut self.count;
        segment.graph_index_batch_with_limit(
            kind,
            std::slice::from_ref(&self.key),
            &mut self.cursor,
            before_work,
            &mut |id| {
                ids[*count] = id;
                *count += 1;
            },
            GRAPH_MERGE_BATCH_SIZE,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(id: u64, from: &str, to: &str) -> UnifiedEntity {
        UnifiedEntity::graph_edge(EntityId::new(id), "step", from, to, 1.0, HashMap::new())
    }

    fn complete(
        managers: &[(usize, Arc<SegmentManager>)],
        keys: &[EntityId],
    ) -> Vec<(EntityId, usize)> {
        let mut expected = Vec::new();
        for (collection, manager) in managers {
            assert!(manager
                .visit_graph_index_batches(
                    GraphEntityKind::Edge,
                    keys,
                    || true,
                    |_| Ok::<_, ()>(()),
                    |batch| {
                        expected.extend(batch.iter().map(|id| (*id, *collection)));
                        Ok(true)
                    },
                )
                .expect("complete cursor"));
        }
        expected.sort_unstable();
        expected
    }

    #[test]
    fn graph_merge_matches_complete_reader_across_aliases_segments_and_collections() {
        let managers: Vec<_> = [2, 7]
            .into_iter()
            .map(|index| {
                let manager = Arc::new(SegmentManager::new(format!("graph{index}")));
                manager
                    .bulk_insert(
                        (1..=600)
                            .map(|id| edge(id, if id % 3 == 0 { "10" } else { "20" }, "10"))
                            .collect(),
                    )
                    .expect("edges");
                manager.force_seal().expect("seal");
                manager
                    .insert(edge(5, "20", "10"))
                    .expect("duplicate physical ID in growing source");
                manager
                    .insert(edge(u64::MAX, "20", "10"))
                    .expect("maximum physical ID");
                (index, manager)
            })
            .collect();
        let keys = [EntityId::new(10), EntityId::new(20), EntityId::new(9999)];
        let expected = complete(&managers, &keys);
        let mut actual = Vec::new();
        assert!(SegmentManager::visit_ordered_graph_index_batches(
            &managers,
            GraphEntityKind::Edge,
            &keys,
            || true,
            |_| Ok::<_, ()>(()),
            |batch| {
                assert!(batch.len() <= SCAN_BATCH_SIZE);
                actual.extend_from_slice(batch);
                Ok(true)
            },
        )
        .expect("merge"));
        assert_eq!(actual, expected);
        assert!(
            actual.windows(2).any(|pair| pair[0] == pair[1]),
            "alias/segment duplicates retained"
        );
        assert_eq!(actual.last(), Some(&(EntityId::new(u64::MAX), 7)));
    }

    #[test]
    fn graph_merge_captures_all_alias_and_collection_bounds_before_delivery() {
        let first = Arc::new(SegmentManager::new("first"));
        let second = Arc::new(SegmentManager::new("second"));
        first
            .bulk_insert((1..=1024).map(|id| edge(id, "10", "20")).collect())
            .expect("edges");
        second
            .insert(edge(2048, "30", "40"))
            .expect("later collection");
        let managers = [(0, Arc::clone(&first)), (1, Arc::clone(&second))];
        let keys = [10, 20, 30, 9999].map(EntityId::new);
        let expected = complete(&managers, &keys);
        let mut actual = Vec::new();
        let mut changed = false;
        assert!(SegmentManager::visit_ordered_graph_index_batches(
            &managers,
            GraphEntityKind::Edge,
            &keys,
            || true,
            |_| Ok::<_, ()>(()),
            |batch| {
                if !changed {
                    changed = true;
                    first.insert(edge(4096, "10", "20")).expect("append");
                    second
                        .insert(edge(4097, "30", "40"))
                        .expect("append to later collection");
                    second
                        .insert(edge(4098, "9999", "40"))
                        .expect("append to initially empty alias");
                    first.force_seal().expect("seal first");
                    second.force_seal().expect("seal second");
                }
                actual.extend_from_slice(batch);
                Ok(true)
            },
        )
        .expect("captured merge"));
        assert_eq!(actual, expected);
    }

    #[test]
    fn graph_merge_admission_and_cancellation_release_sources_without_partial_delivery() {
        let manager = Arc::new(SegmentManager::new("graph"));
        manager
            .bulk_insert((1..=1024).map(|id| edge(id, "10", "20")).collect())
            .expect("edges");
        let source = Arc::downgrade(manager.growing.read().as_ref().expect("growing source"));
        let owners = source.strong_count();
        let managers = [(0, Arc::clone(&manager))];
        let keys = [EntityId::new(10), EntityId::new(20)];
        let denied = SegmentManager::visit_ordered_graph_index_batches(
            &managers,
            GraphEntityKind::Edge,
            &keys,
            || true,
            |_| Err("memory denied"),
            |_| panic!("no delivery after admission failure"),
        );
        assert_eq!(denied, Err("memory denied"));
        let mut work = 0;
        let mut delivered = 0;
        assert!(!SegmentManager::visit_ordered_graph_index_batches(
            &managers,
            GraphEntityKind::Edge,
            &keys,
            || {
                work += 1;
                work < 200
            },
            |_| Ok::<_, ()>(()),
            |batch| {
                delivered += batch.len();
                Ok(true)
            },
        )
        .expect("cancelled"));
        assert_eq!(delivered, 0, "cancelled partial batch stays private");
        let stopped = SegmentManager::visit_ordered_graph_index_batches(
            &managers,
            GraphEntityKind::Edge,
            &keys,
            || true,
            |_| {
                assert!(manager.growing.try_write().is_some());
                assert!(manager.sealed.try_write().is_some());
                assert!(source.upgrade().expect("source").try_write().is_some());
                Ok(())
            },
            |batch| {
                delivered += batch.len();
                Err("consumer error")
            },
        );
        assert_eq!(stopped, Err("consumer error"));
        assert_eq!(delivered, SCAN_BATCH_SIZE);
        assert_eq!(
            source.strong_count(),
            owners,
            "error releases pinned sources"
        );
        assert!(!SegmentManager::visit_ordered_graph_index_batches(
            &managers,
            GraphEntityKind::Edge,
            &keys,
            || true,
            |_| Ok::<_, ()>(()),
            |_| Ok(false),
        )
        .expect("early stop"));
        assert_eq!(
            source.strong_count(),
            owners,
            "early stop releases pinned sources"
        );
    }

    #[test]
    fn graph_merge_pins_retired_sources_and_hydrates_live_after_consolidation() {
        let manager = Arc::new(SegmentManager::with_config(
            "graph",
            ManagerConfig {
                max_sealed_segments: 1000,
                consolidation_entities_per_tick: 64,
                ..Default::default()
            },
        ));
        manager
            .bulk_insert((1..=1024).map(|id| edge(id, "10", "20")).collect())
            .expect("edges");
        manager.force_seal().expect("seal");
        for id in 1..=300 {
            manager.delete(EntityId::new(id)).expect("fragment");
        }
        let source = Arc::downgrade(&manager.sealed.read()[0]);
        let mut changed = false;
        let mut actual = Vec::new();
        assert!(SegmentManager::visit_ordered_graph_index_batches(
            &[(0, Arc::clone(&manager))], GraphEntityKind::Edge, &[EntityId::new(10), EntityId::new(20)],
            || true, |_| Ok::<_, ()>(()),
            |batch| {
                assert!(manager.growing.try_write().is_some());
                assert!(manager.sealed.try_write().is_some());
                assert!(source.upgrade().expect("source pinned").try_write().is_some());
                if !changed {
                    changed = true;
                    let mut ticks = 0;
                    while ticks == 0 || manager.consolidation.read().is_some() {
                        manager.run_maintenance().expect("maintenance");
                        ticks += 1;
                        assert!(ticks < 1000, "consolidation converges");
                    }
                    assert!(manager.stats().consolidation.runs_completed > 0);
                    let mut updated = manager.get(EntityId::new(1024)).expect("future edge");
                    if let EntityKind::GraphEdge(edge) = &mut updated.kind { edge.weight = 2000; }
                    manager.update(updated).expect("update authoritative merged copy");
                }
                for (id, _) in batch {
                    let entity = manager.get(*id).expect("live hydration");
                    if id.raw() == 1024 {
                        assert!(matches!(&entity.kind, EntityKind::GraphEdge(edge) if edge.weight == 2000));
                    }
                    actual.push(id.raw());
                }
                Ok(true)
            },
        ).expect("merge with consolidation"));
        let expected: Vec<_> = (301..=1024).flat_map(|id| [id, id]).collect();
        assert_eq!(actual, expected);
        assert!(
            source.upgrade().is_none(),
            "completion releases retired source"
        );
    }
}
