//! Conservative query-lifetime credits. Admission always runs outside storage locks.
use super::*;
use crate::runtime::memory_admission::MemoryReservation;
use crate::storage::memory_pools::MemoryPool;

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

/// Query-local scopes spend the same admitted credits. Temporary scopes refund
/// their usage only after their payloads/buffers have been dropped.
pub(crate) struct GraphMemory<'a> {
    credits: std::rc::Rc<std::cell::RefCell<GraphMemoryCredits<'a>>>,
    refundable_bytes: Option<u64>,
}

struct GraphMemoryCredits<'a> {
    runtime: &'a RedDBRuntime,
    guards: Vec<MemoryReservation<'a>>,
    reserved_bytes: u64,
    available_bytes: u64,
}

impl Drop for GraphMemory<'_> {
    fn drop(&mut self) {
        if let Some(bytes) = self.refundable_bytes {
            let mut credits = self.credits.borrow_mut();
            credits.available_bytes += bytes;
            assert!(
                credits.available_bytes <= credits.reserved_bytes,
                "invariant: temporary scopes return only admitted credits"
            );
        }
    }
}

impl<'a> GraphMemory<'a> {
    pub(crate) fn new(runtime: &'a RedDBRuntime) -> Self {
        Self {
            credits: std::rc::Rc::new(std::cell::RefCell::new(GraphMemoryCredits {
                runtime,
                guards: Vec::new(),
                reserved_bytes: 0,
                available_bytes: 0,
            })),
            refundable_bytes: None,
        }
    }

    /// The caller must retain this scope until all its temporary allocations
    /// are gone. Permanent identity/adjacency state stays on the enclosing scope.
    pub(super) fn scratch(&self) -> Self {
        Self {
            credits: std::rc::Rc::clone(&self.credits),
            refundable_bytes: Some(0),
        }
    }

    /// Transfer a temporary scope's admitted allocations into query-owned
    /// state without cloning or reserving the same payload a second time.
    pub(super) fn retain_for_query(mut self) {
        assert!(
            self.refundable_bytes.take().is_some(),
            "invariant: only temporary credits transfer into query ownership"
        );
    }

    pub(super) fn admit(&mut self, bytes: usize) -> RedDBResult<()> {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        let mut credits = self.credits.borrow_mut();
        if bytes > credits.available_bytes {
            let needed = bytes - credits.available_bytes;
            let (guard, reserved) = match credits
                .runtime
                .try_reserve_memory_growth(needed.max(64 * 1024))
            {
                Some(guard) => (guard, needed.max(64 * 1024)),
                None => (
                    credits.runtime.admit_non_evictable_growth(
                        MemoryPool::IndexMemory,
                        "context graph expansion",
                        needed,
                    )?,
                    needed,
                ),
            };
            credits.available_bytes += reserved;
            credits.reserved_bytes += reserved;
            credits.guards.push(guard);
        }
        credits.available_bytes -= bytes;
        if let Some(ref mut refundable) = self.refundable_bytes {
            *refundable += bytes;
        }
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

    #[cfg(test)]
    pub(super) fn entity(
        &mut self,
        store: &UnifiedStore,
        collection: &str,
        id: EntityId,
    ) -> RedDBResult<Option<UnifiedEntity>> {
        self.entity_if(store, collection, id, |_| true)
    }

    /// The predicate is a pure visibility check under the owning segment lock.
    /// Hidden retained versions must not allocate payloads or consume credits.
    pub(super) fn entity_if(
        &mut self,
        store: &UnifiedStore,
        collection: &str,
        id: EntityId,
        visible: impl Fn(&UnifiedEntity) -> bool,
    ) -> RedDBResult<Option<UnifiedEntity>> {
        let Some(manager) = store.get_collection(collection) else {
            return Ok(None);
        };
        let mut admitted = 0;
        loop {
            function_budget::charge(1)?;
            let result = manager.get_with(id, |entity| {
                if !visible(entity) {
                    return Ok(None);
                }
                let bytes = payload_bytes(entity);
                if bytes > admitted {
                    Err(bytes)
                } else {
                    Ok(Some(entity.clone()))
                }
            });
            match result {
                None => return Ok(None),
                Some(Ok(entity)) => return Ok(entity),
                Some(Err(bytes)) => {
                    // A concurrent replacement may have grown since the size probe.
                    self.admit(bytes - admitted)?;
                    admitted = bytes;
                }
            }
        }
    }
}
