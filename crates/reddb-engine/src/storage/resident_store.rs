//! Canonical resident store state and reads shared by native and browser hosts.
//!
//! Hosts own catalog publication, durable mutations, pager fallback and WAL policy.
//! Internal fields remain available to host mutation adapters so their existing
//! lock/publication order is preserved during the store extraction.
#![deny(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use super::context_index::ContextIndex;
use super::entity::{EntityId, EntityKind, RefType, UnifiedEntity};
use super::entity_cache::EntityCache;
use super::manager::SegmentManager;
use super::metadata::MetadataFilter;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Resident state of one store. This type supplies no durable commit boundary.
#[doc(hidden)]
pub struct ResidentStore {
    /// Global entity ID counter
    #[doc(hidden)]
    pub next_entity_id: AtomicU64,
    /// Collections by name
    #[doc(hidden)]
    pub collections: RwLock<HashMap<String, Arc<SegmentManager>>>,
    /// Forward cross-references: source_id → [(target_id, ref_type, target_collection)]
    #[doc(hidden)]
    pub cross_refs: RwLock<HashMap<EntityId, Vec<(EntityId, RefType, String)>>>,
    /// Reverse cross-references: target_id → [(source_id, ref_type, source_collection)]
    #[doc(hidden)]
    pub reverse_refs: RwLock<HashMap<EntityId, Vec<(EntityId, RefType, String)>>>,
    /// Cross-structure context index for unified search
    #[doc(hidden)]
    pub context_index: ContextIndex,
    /// Hot entity cache — sharded bounded LRU for `get_any` lookups.
    /// See `entity_cache.rs` for the rationale; this replaced a single
    /// `RwLock<HashMap>` that serialised every `delete_batch` invalidation.
    #[doc(hidden)]
    pub entity_cache: EntityCache,
    /// Graph node label index: (collection, label) → Vec<EntityId>.
    /// O(1) lookup for MATCH (n:Label) graph patterns — avoids full collection scan.
    #[doc(hidden)]
    pub graph_label_index: RwLock<HashMap<(String, String), Vec<EntityId>>>,
    /// Opaque store-level auxiliary metadata persisted inside the binary dump
    /// (store format V10+). RedDB uses this to carry collection contracts
    /// through the single-file artifact so a collection's `declared_model`
    /// (e.g. `kv`) survives a restart instead of being re-inferred as a table.
    /// The store treats the bytes as opaque; only RedDB interprets them.
    #[doc(hidden)]
    pub aux_metadata: RwLock<Vec<u8>>,
}

impl ResidentStore {
    pub fn new() -> Self {
        Self {
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            context_index: ContextIndex::new(),
            entity_cache: EntityCache::new(),
            graph_label_index: RwLock::new(HashMap::new()),
            aux_metadata: RwLock::new(Vec::new()),
        }
    }

    pub fn next_entity_id(&self) -> EntityId {
        EntityId::new(self.next_entity_id.fetch_add(1, Ordering::SeqCst))
    }

    pub fn reserve_entity_ids(&self, n: u64) -> std::ops::Range<u64> {
        let start = self.next_entity_id.fetch_add(n, Ordering::SeqCst);
        start..start + n
    }

    pub fn register_entity_id(&self, id: EntityId) {
        let candidate = id.raw().saturating_add(1);
        let mut current = self.next_entity_id.load(Ordering::SeqCst);
        while candidate > current {
            match self.next_entity_id.compare_exchange(
                current,
                candidate,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(updated) => current = updated,
            }
        }
    }

    pub fn update_graph_label_index(&self, collection: &str, label: &str, entity_id: EntityId) {
        let key = (collection.to_string(), label.to_string());
        let mut idx = self.graph_label_index.write();
        idx.entry(key).or_default().push(entity_id);
    }

    pub fn remove_from_graph_label_index(&self, collection: &str, entity_id: EntityId) {
        let mut idx = self.graph_label_index.write();
        for ((col, _), ids) in idx.iter_mut() {
            if col == collection {
                ids.retain(|&id| id != entity_id);
            }
        }
        // Prune empty entries to keep the index compact
        idx.retain(|_, ids| !ids.is_empty());
    }

    pub fn remove_from_graph_label_index_batch(&self, collection: &str, entity_ids: &[EntityId]) {
        if entity_ids.is_empty() {
            return;
        }
        let id_set: std::collections::HashSet<EntityId> = entity_ids.iter().copied().collect();
        let mut idx = self.graph_label_index.write();
        for ((col, _), ids) in idx.iter_mut() {
            if col == collection {
                ids.retain(|id| !id_set.contains(id));
            }
        }
        idx.retain(|_, ids| !ids.is_empty());
    }

    pub fn lookup_graph_nodes_by_label(&self, label: &str) -> Vec<EntityId> {
        let idx = self.graph_label_index.read();
        idx.iter()
            .filter(|((_, l), _)| l == label)
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect()
    }

    pub fn lookup_graph_nodes_by_label_in(&self, collection: &str, label: &str) -> Vec<EntityId> {
        let idx = self.graph_label_index.read();
        idx.get(&(collection.to_string(), label.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_collection(&self, name: &str) -> Option<Arc<SegmentManager>> {
        self.collections.read().get(name).map(Arc::clone)
    }

    pub fn context_index(&self) -> &ContextIndex {
        &self.context_index
    }

    pub fn set_aux_metadata(&self, bytes: Vec<u8>) {
        *self.aux_metadata.write() = bytes;
    }

    pub fn aux_metadata(&self) -> Vec<u8> {
        self.aux_metadata.read().clone()
    }

    pub fn list_collections(&self) -> Vec<String> {
        self.collections.read().keys().cloned().collect()
    }

    pub fn table_row_versions_by_logical_id(
        &self,
        collection: &str,
        logical_id: EntityId,
    ) -> Vec<UnifiedEntity> {
        self.get_collection(collection)
            .map(|manager| {
                manager.query_all(|entity| {
                    matches!(entity.kind, EntityKind::TableRow { .. })
                        && entity.logical_id() == logical_id
                })
            })
            .unwrap_or_default()
    }

    pub fn get_any(&self, id: EntityId) -> Option<(String, UnifiedEntity)> {
        // Sharded LRU probe: hits / misses are counted by `EntityCache::get`,
        // so external observers can read `entity_cache_hit_rate()` without
        // any extra wiring on the call site.
        if let Some(cached) = self.entity_cache.get(id.raw()) {
            return Some(cached);
        }

        // Cache miss — fall back to the full collection scan, then memoise.
        let collections = self.collections.read();
        for (name, manager) in collections.iter() {
            if let Some(entity) = manager.get(id) {
                let result = (name.clone(), entity);
                // Drop the collections read guard before taking any cache
                // lock — the cache shards are independent, but releasing
                // here keeps `collections` contention low.
                drop(collections);
                self.entity_cache.insert(id.raw(), result.clone());
                return Some(result);
            }
        }
        None
    }

    pub fn entity_cache_hit_rate(&self) -> Option<f64> {
        self.entity_cache.hit_rate()
    }

    pub fn entity_cache_stats(&self) -> crate::storage::entity_cache::EntityCacheStats {
        self.entity_cache.stats()
    }

    pub fn get_refs_from(&self, id: EntityId) -> Vec<(EntityId, RefType, String)> {
        self.cross_refs.read().get(&id).cloned().unwrap_or_default()
    }

    pub fn get_refs_to(&self, id: EntityId) -> Vec<(EntityId, RefType, String)> {
        self.reverse_refs
            .read()
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn query_all<F>(&self, filter: F) -> Vec<(String, UnifiedEntity)>
    where
        F: Fn(&UnifiedEntity) -> bool + Clone + Send + Sync,
    {
        // Issue #815 — snapshot the (name, manager) handles under a brief
        // read lock, then release the `collections` lock *before* the
        // potentially long per-collection scan. Holding `collections.read()`
        // across a full-store scan let a large `/catalog` snapshot block (and
        // be blocked by) the replica apply loop's collection-creation writes:
        // under parking_lot a writer waiting behind the long read parks every
        // subsequent reader, so `/catalog` and readiness wedged during a big
        // WAL re-apply. The managers are `Arc`-shared with their own internal
        // locks, so scanning them off the map lock is safe and keeps the HTTP
        // surface responsive.
        let pairs: Vec<(String, Arc<SegmentManager>)> = {
            let collections = self.collections.read();
            collections
                .iter()
                .map(|(name, mgr)| (name.clone(), Arc::clone(mgr)))
                .collect()
        };

        let use_parallel = pairs.len() > 1 && crate::execution::should_parallelize();
        if !use_parallel {
            // Single collection — no parallelism overhead
            return pairs
                .into_iter()
                .flat_map(|(name, mgr)| {
                    mgr.query_all(filter.clone())
                        .into_iter()
                        .map(move |e| (name.clone(), e))
                })
                .collect();
        }

        // Multiple collections — scan in parallel
        let filter_ref = &filter;
        let collection_results: Vec<Vec<(String, UnifiedEntity)>> = std::thread::scope(|s| {
            pairs
                .iter()
                .map(|(name, manager)| {
                    let name = (*name).clone();
                    s.spawn(move || {
                        manager
                            .query_all(|e| filter_ref(e))
                            .into_iter()
                            .map(|e| (name.clone(), e))
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });

        collection_results.into_iter().flatten().collect()
    }

    pub fn scan<F>(
        &self,
        snapshot: Option<&crate::transaction::read::SnapshotContext>,
        filter: F,
    ) -> Vec<(String, UnifiedEntity)>
    where
        F: Fn(&UnifiedEntity) -> bool + Clone + Send + Sync,
    {
        let pairs: Vec<(String, Arc<SegmentManager>)> = {
            let collections = self.collections.read();
            collections
                .iter()
                .map(|(name, manager)| (name.clone(), Arc::clone(manager)))
                .collect()
        };

        let use_parallel = pairs.len() > 1 && crate::execution::should_parallelize();
        if !use_parallel {
            return pairs
                .into_iter()
                .flat_map(|(name, manager)| {
                    manager
                        .scan(snapshot, filter.clone())
                        .into_iter()
                        .map(move |entity| (name.clone(), entity))
                })
                .collect();
        }

        let filter_ref = &filter;
        let collection_results: Vec<Vec<(String, UnifiedEntity)>> = std::thread::scope(|scope| {
            pairs
                .iter()
                .map(|(name, manager)| {
                    let name = name.clone();
                    scope.spawn(move || {
                        manager
                            .scan(snapshot, |entity| filter_ref(entity))
                            .into_iter()
                            .map(|entity| (name.clone(), entity))
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap_or_default())
                .collect()
        });

        collection_results.into_iter().flatten().collect()
    }

    pub fn filter_metadata_all(
        &self,
        filters: &[(String, MetadataFilter)],
    ) -> Vec<(String, EntityId)> {
        let mut results = Vec::new();
        let collections = self.collections.read();

        for (name, manager) in collections.iter() {
            for id in manager.filter_metadata(filters) {
                results.push((name.clone(), id));
            }
        }

        results
    }
}

impl Default for ResidentStore {
    fn default() -> Self {
        Self::new()
    }
}
