//! Conservative query-lifetime credits. Admission always runs outside storage locks.
use super::*;
use crate::runtime::memory_admission::MemoryReservation;
use crate::storage::memory_pools::MemoryPool;
use crate::storage::unified::manager::SegmentManager;

pub(super) fn payload_bytes(entity: &UnifiedEntity) -> usize {
    let strings = match &entity.kind {
        EntityKind::GraphNode(node) => node.label.len(),
        EntityKind::GraphEdge(edge) => edge
            .label
            .len()
            .saturating_add(edge.from_node.len())
            .saturating_add(edge.to_node.len()),
        _ => 0,
    };
    let properties_capacity = match &entity.data {
        EntityData::Node(node) => node.properties.capacity(),
        EntityData::Edge(edge) => edge.properties.capacity(),
        _ => 0,
    };
    let references = entity.cross_refs().iter().fold(0usize, |bytes, reference| {
        bytes.saturating_add(reference.target_collection.len())
    });
    crate::storage::unified::memory_size::entity_bytes(entity)
        .saturating_add(strings)
        .saturating_add(references)
        .saturating_add(std::mem::size_of_val(entity.embeddings()))
        .saturating_add(
            properties_capacity
                .saturating_mul(std::mem::size_of::<(String, Value)>().saturating_mul(2)),
        )
        // Boxed kind/data and optional auxiliary headers.
        .saturating_add(256)
}

pub(crate) struct GraphMemory<'a> {
    runtime: &'a RedDBRuntime,
    guards: Vec<MemoryReservation<'a>>,
    available_bytes: u64,
}

impl<'a> GraphMemory<'a> {
    pub(crate) fn new(runtime: &'a RedDBRuntime) -> Self {
        Self {
            runtime,
            guards: Vec::new(),
            available_bytes: 0,
        }
    }

    pub(super) fn admit(&mut self, bytes: usize) -> RedDBResult<()> {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        if bytes > self.available_bytes {
            let needed = bytes - self.available_bytes;
            let guard = match self
                .runtime
                .try_reserve_memory_growth(needed.max(64 * 1024))
            {
                Some(guard) => (guard, needed.max(64 * 1024)),
                None => (
                    self.runtime.admit_non_evictable_growth(
                        MemoryPool::IndexMemory,
                        "context graph expansion",
                        needed,
                    )?,
                    needed,
                ),
            };
            self.available_bytes += guard.1;
            self.guards.push(guard.0);
        }
        self.available_bytes -= bytes;
        Ok(())
    }

    /// Covers container occupancy/growth slack and independently owned strings.
    pub(super) fn entry<T>(&mut self, string_bytes: usize) -> RedDBResult<()> {
        self.admit(
            std::mem::size_of::<T>()
                .saturating_mul(4)
                .saturating_add(string_bytes.saturating_mul(2)),
        )
    }

    pub(super) fn entity(
        &mut self,
        store: &UnifiedStore,
        collection: &str,
        id: EntityId,
    ) -> RedDBResult<Option<UnifiedEntity>> {
        let Some(manager) = store.get_collection(collection) else {
            return Ok(None);
        };
        let mut admitted = 0;
        loop {
            function_budget::charge(1)?;
            let result = manager.get_with(id, |entity| {
                let bytes = payload_bytes(entity);
                if bytes > admitted {
                    Err(bytes)
                } else {
                    Ok(entity.clone())
                }
            });
            match result {
                None => return Ok(None),
                Some(Ok(entity)) => return Ok(Some(entity)),
                Some(Err(bytes)) => {
                    // A concurrent replacement may have grown since the size probe.
                    self.admit(bytes - admitted)?;
                    admitted = bytes;
                }
            }
        }
    }

    /// Never grow a buffer under segment locks: reserve outside, then retry an
    /// overflowing probe with twice the capacity. No prefix escapes on failure.
    pub(super) fn candidates(
        &mut self,
        manager: &SegmentManager,
        kind: GraphEntityKind,
        keys: &[EntityId],
        visible: impl Fn(&UnifiedEntity) -> bool,
    ) -> RedDBResult<Vec<(EntityId, bool)>> {
        let mut capacity = 32usize;
        let mut admitted = 0;
        loop {
            let bytes = capacity.saturating_mul(std::mem::size_of::<(EntityId, bool)>());
            self.admit(bytes.saturating_sub(admitted))?;
            admitted = bytes;
            let mut candidates = Vec::with_capacity(capacity);
            let mut overflow = false;
            manager.visit_graph_index_candidates(
                kind,
                keys,
                || function_budget::charge(1).is_ok(),
                |entity| {
                    if candidates.len() == capacity {
                        overflow = true;
                        return false;
                    }
                    candidates.push((entity.id, visible(entity)));
                    true
                },
            );
            function_budget::charge(0)?;
            if !overflow {
                return Ok(candidates);
            }
            drop(candidates);
            capacity = capacity.checked_mul(2).ok_or_else(|| {
                RedDBError::InvalidOperation("context graph candidate size overflow".into())
            })?;
        }
    }
}
