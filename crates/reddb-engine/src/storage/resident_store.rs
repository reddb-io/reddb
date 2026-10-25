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
use super::error::StoreError;
use super::manager::ManagerConfig;
use super::manager::SegmentManager;
use super::metadata::MetadataFilter;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Resident state of one store. This type supplies no durable commit boundary.
#[doc(hidden)]
pub struct ResidentStore {
    /// Whether the paged registry on page 1 must be rewritten before the next flush.
    #[doc(hidden)]
    pub paged_registry_dirty: AtomicBool,
    /// Counts how often `unindex_cross_refs_batch` took the read-only fast
    /// path (no inbound refs, no outbound refs for any deleted id) and so
    /// avoided acquiring the `cross_refs` / `reverse_refs` write locks.
    /// Used by tests to pin the early-exit; cheap relaxed counter otherwise.
    #[doc(hidden)]
    pub unindex_cross_refs_fast_path: AtomicU64,

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
            paged_registry_dirty: AtomicBool::new(false),
            unindex_cross_refs_fast_path: AtomicU64::new(0),
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

impl ResidentStore {
    pub fn index_cross_refs(
        &self,
        entity: &UnifiedEntity,
        collection: &str,
    ) -> Result<(), StoreError> {
        let mut registry_dirty = false;
        for cross_ref in entity.cross_refs() {
            if cross_ref.target_collection.is_empty() {
                continue;
            }
            {
                let mut forward = self.cross_refs.write();
                let refs = forward.entry(cross_ref.source).or_default();
                let inserted = !refs.iter().any(|(id, kind, coll)| {
                    *id == cross_ref.target
                        && *kind == cross_ref.ref_type
                        && coll == &cross_ref.target_collection
                });
                if inserted {
                    refs.push((
                        cross_ref.target,
                        cross_ref.ref_type,
                        cross_ref.target_collection.clone(),
                    ));
                    registry_dirty = true;
                }
            }

            {
                let mut reverse = self.reverse_refs.write();
                let refs = reverse.entry(cross_ref.target).or_default();
                let inserted = !refs.iter().any(|(id, kind, coll)| {
                    *id == cross_ref.source && *kind == cross_ref.ref_type && coll == collection
                });
                if inserted {
                    refs.push((cross_ref.source, cross_ref.ref_type, collection.to_string()));
                    registry_dirty = true;
                }
            }
        }

        if registry_dirty {
            self.mark_paged_registry_dirty();
        }

        Ok(())
    }

    pub fn unindex_cross_refs(&self, id: EntityId) -> Result<(), StoreError> {
        // Remove forward refs
        self.cross_refs.write().remove(&id);

        // Remove from reverse refs (scan all)
        let mut reverse = self.reverse_refs.write();
        for refs in reverse.values_mut() {
            refs.retain(|(source, _, _)| *source != id);
        }
        reverse.remove(&id);
        self.mark_paged_registry_dirty();

        Ok(())
    }

    pub fn unindex_cross_refs_batch(&self, ids: &[EntityId]) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }

        let id_set: std::collections::HashSet<EntityId> = ids.iter().copied().collect();

        // Read-only probe: under steady-state delete workloads most rows have
        // no cross-refs at all, so both maps are entirely cold for the batch.
        // Acquiring the write lock to scan `reverse_refs.values_mut()` over an
        // unrelated dictionary was the dominant cost in #85's `delete_sequential`
        // profile. The read-lock probe below is O(|ids|) hashmap lookups against
        // each map, vs. O(|reverse_refs|) for the write-path scan it replaces.
        //
        // Decision: skip the write path iff
        //   - no deleted id is a key in `cross_refs`  (no outbound refs to drop,
        //     and so no `(deleted_id, _, _)` source entry exists anywhere in
        //     `reverse_refs.values()` either — the two maps are kept in sync by
        //     `add_cross_ref`), AND
        //   - no deleted id is a key in `reverse_refs` (no inbound refs to drop).
        let needs_forward_cleanup = {
            let forward = self.cross_refs.read();
            id_set.iter().any(|id| forward.contains_key(id))
        };
        let needs_reverse_cleanup = {
            let reverse = self.reverse_refs.read();
            id_set.iter().any(|id| reverse.contains_key(id))
        };

        if !needs_forward_cleanup && !needs_reverse_cleanup {
            self.unindex_cross_refs_fast_path
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        if needs_forward_cleanup {
            let mut forward = self.cross_refs.write();
            for id in &id_set {
                forward.remove(id);
            }
        }

        if needs_reverse_cleanup || needs_forward_cleanup {
            // The inner `values_mut()` scan is only needed when at least one
            // deleted id was a *source* somewhere — which, by the invariant
            // above, can only happen when it had outbound forward refs.
            let mut reverse = self.reverse_refs.write();
            if needs_forward_cleanup {
                for refs in reverse.values_mut() {
                    refs.retain(|(source, _, _)| !id_set.contains(source));
                }
            }
            reverse.retain(|target, refs| !id_set.contains(target) && !refs.is_empty());
        }
        self.mark_paged_registry_dirty();

        Ok(())
    }

    pub fn unindex_cross_refs_fast_path_hits(&self) -> u64 {
        self.unindex_cross_refs_fast_path.load(Ordering::Relaxed)
    }

    pub fn mark_paged_registry_dirty(&self) {
        self.paged_registry_dirty.store(true, Ordering::Release);
    }

    pub fn create_collection_in_memory(
        &self,
        name: &str,
        manager_config: &ManagerConfig,
    ) -> Result<(), StoreError> {
        let mut collections = self.collections.write();
        if collections.contains_key(name) {
            return Ok(());
        }
        let manager = SegmentManager::with_config(name, manager_config.clone());
        collections.insert(name.to_string(), Arc::new(manager));
        self.mark_paged_registry_dirty();
        Ok(())
    }
}

impl ResidentStore {
    pub fn apply_replayed_upsert(
        &self,
        collection: &str,
        record: &[u8],
        format_version: u32,
        manager_config: &ManagerConfig,
    ) -> Result<EntityId, StoreError> {
        self.create_collection_in_memory(collection, manager_config)?;
        let (entity, metadata) =
            super::metadata_codec::deserialize_entity_record(record, format_version)
                .map_err(StoreError::Serialization)?;
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        self.register_entity_id(entity.id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            manager.register_row_id(*row_id);
        }

        self.context_index.remove_entity(entity.id);
        let _ = self.unindex_cross_refs(entity.id);
        self.remove_from_graph_label_index(collection, entity.id);

        if manager.get(entity.id).is_some() {
            manager
                .update_with_metadata(entity.clone(), metadata.as_ref())
                .map_err(StoreError::from)?;
        } else {
            manager.restore_snapshot_entity(entity.clone())?;
            if let Some(metadata) = metadata.as_ref() {
                manager.set_metadata(entity.id, metadata.clone())?;
            }
        }

        self.context_index.index_entity(collection, &entity);
        if let EntityKind::GraphNode(node) = &entity.kind {
            self.update_graph_label_index(collection, &node.label, entity.id);
        }
        self.index_cross_refs(&entity, collection)?;

        Ok(entity.id)
    }
}

impl ResidentStore {
    pub fn drop_collection_in_memory(
        &self,
        name: &str,
        retire_index: impl FnOnce(),
    ) -> Result<(), StoreError> {
        let manager = {
            let mut collections = self.collections.write();
            match collections.remove(name) {
                Some(manager) => manager,
                None => return Ok(()),
            }
        };

        let entities = manager.query_all(|_| true);
        let entity_ids: Vec<EntityId> = entities.iter().map(|entity| entity.id).collect();
        for entity_id in &entity_ids {
            self.context_index.remove_entity(*entity_id);
            let _ = self.unindex_cross_refs(*entity_id);
        }
        retire_index();
        self.entity_cache.retain(|entity_id, (collection, _)| {
            collection != name && !entity_ids.iter().any(|id| id.raw() == entity_id)
        });
        self.remove_from_graph_label_index_batch(name, &entity_ids);
        self.mark_paged_registry_dirty();
        Ok(())
    }

    pub fn apply_replayed_delete(
        &self,
        collection: &str,
        id: EntityId,
        update_index: impl FnOnce(),
    ) -> Result<(), StoreError> {
        self.entity_cache.remove(id.raw());
        if let Some(manager) = self.get_collection(collection) {
            let deleted = manager.delete(id)?;
            if !deleted {
                return Ok(());
            }
        } else {
            return Ok(());
        }

        update_index();

        let _ = self.unindex_cross_refs(id);
        self.remove_from_graph_label_index(collection, id);
        self.context_index.remove_entity(id);
        Ok(())
    }

    pub fn swap_collection_state(
        &self,
        name: &str,
        new_manager: Arc<SegmentManager>,
        prepared: &[UnifiedEntity],
    ) {
        {
            let mut collections = self.collections.write();
            collections.insert(name.to_string(), new_manager);
        }

        self.entity_cache
            .retain(|_, (collection, _)| collection != name);
        self.remove_from_graph_label_index_batch(
            name,
            &prepared.iter().map(|e| e.id).collect::<Vec<_>>(),
        );
    }

    pub fn prepare_replayed_refresh_collection(
        &self,
        collection: &str,
        records: &[Vec<u8>],
        format_version: u32,
        manager_config: &ManagerConfig,
    ) -> Result<(Arc<SegmentManager>, Vec<UnifiedEntity>), StoreError> {
        let new_manager = Arc::new(SegmentManager::with_config(
            collection,
            manager_config.clone(),
        ));

        let mut prepared: Vec<UnifiedEntity> = Vec::with_capacity(records.len());
        for record in records {
            let (entity, _metadata) =
                super::metadata_codec::deserialize_entity_record(record, format_version)
                    .map_err(StoreError::Serialization)?;
            self.register_entity_id(entity.id);
            if let EntityKind::TableRow { row_id, .. } = &entity.kind {
                new_manager.register_row_id(*row_id);
            }
            prepared.push(entity);
        }

        if !prepared.is_empty() {
            new_manager.bulk_insert(prepared.clone()).map_err(|e| {
                StoreError::Io(std::io::Error::other(format!(
                    "replay refresh_collection: bulk_insert failed: {e}"
                )))
            })?;
        }

        Ok((new_manager, prepared))
    }
}

impl Default for ResidentStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ResidentStore {
    /// Replay without a paged index. Hosts retain their existing private-recovery
    /// publication fence and rebuild or update durable indexes separately.
    pub fn apply_replayed_action(
        &self,
        action: &super::store_wal::StoreWalAction,
        format_version: u32,
        manager_config: &ManagerConfig,
    ) -> Result<(), StoreError> {
        super::store_wal::apply_replayed_action(
            &ResidentReplayTarget {
                store: self,
                format_version,
                manager_config,
            },
            action,
        )
    }
}

struct ResidentReplayTarget<'a> {
    store: &'a ResidentStore,
    format_version: u32,
    manager_config: &'a ManagerConfig,
}

impl super::store_wal::StoreReplayTarget for ResidentReplayTarget<'_> {
    fn has_collection(&self, name: &str) -> bool {
        self.store.get_collection(name).is_some()
    }
    fn create_collection(&self, name: &str) -> Result<(), StoreError> {
        self.store
            .create_collection_in_memory(name, self.manager_config)
    }
    fn drop_collection(&self, name: &str) -> Result<(), StoreError> {
        self.store.drop_collection_in_memory(name, || {})
    }
    fn upsert(&self, collection: &str, record: &[u8]) -> Result<(), StoreError> {
        self.store.apply_replayed_upsert(
            collection,
            record,
            self.format_version,
            self.manager_config,
        )?;
        Ok(())
    }
    fn delete(&self, collection: &str, id: EntityId) -> Result<(), StoreError> {
        self.store.apply_replayed_delete(collection, id, || {})
    }
    fn refresh(&self, collection: &str, records: &[Vec<u8>]) -> Result<(), StoreError> {
        let (manager, prepared) = self.store.prepare_replayed_refresh_collection(
            collection,
            records,
            self.format_version,
            self.manager_config,
        )?;
        self.store
            .swap_collection_state(collection, manager, &prepared);
        Ok(())
    }
}
