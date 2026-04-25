//! Segment Manager
//!
//! Manages the lifecycle of unified segments: creation, sealing, compaction,
//! and archival. Coordinates writes to growing segments and queries across
//! all segments.
//!
//! # Responsibilities
//!
//! - Route writes to the active growing segment
//! - Auto-seal segments when thresholds are met
//! - Coordinate queries across multiple segments
//! - Background compaction of sealed segments
//! - Archive old segments to cold storage

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::entity::{EntityId, UnifiedEntity};
use super::metadata::{Metadata, MetadataFilter};
use super::segment::{
    GrowingSegment, SegmentConfig, SegmentError, SegmentId, SegmentState, SegmentStats,
    UnifiedSegment, ZoneColPred, ZoneColPredKind,
};
use crate::storage::btree::visibility_map::VisibilityMap;

/// Configuration for the segment manager
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Segment configuration
    pub segment_config: SegmentConfig,
    /// Maximum number of sealed segments before compaction
    pub max_sealed_segments: usize,
    /// Idle time (seconds) before auto-sealing
    pub idle_seal_secs: u64,
    /// Enable background compaction
    pub enable_compaction: bool,
    /// Enable background archival
    pub enable_archival: bool,
    /// Age threshold for archival (seconds)
    pub archive_age_secs: u64,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            segment_config: SegmentConfig::default(),
            max_sealed_segments: 10,
            idle_seal_secs: 300, // 5 minutes
            enable_compaction: true,
            enable_archival: true,
            archive_age_secs: 86400 * 7, // 7 days
        }
    }
}

/// Manager statistics
#[derive(Debug, Clone, Default)]
pub struct ManagerStats {
    /// Total entities across all segments
    pub total_entities: usize,
    /// Number of growing segments
    pub growing_count: usize,
    /// Number of sealed segments
    pub sealed_count: usize,
    /// Number of archived segments
    pub archived_count: usize,
    /// Total memory usage
    pub total_memory_bytes: usize,
    /// Number of seal operations
    pub seal_ops: u64,
    /// Number of compaction operations
    pub compact_ops: u64,
}

/// Lifecycle events for monitoring
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    SegmentCreated(SegmentId),
    SegmentSealed(SegmentId),
    SegmentCompacted {
        source: Vec<SegmentId>,
        target: SegmentId,
    },
    SegmentArchived(SegmentId),
    EntityInserted(EntityId, SegmentId),
    EntityDeleted(EntityId, SegmentId),
}

/// Segment manager for a collection
pub struct SegmentManager {
    /// Collection name
    collection: String,
    /// Configuration
    config: ManagerConfig,
    /// Next segment ID counter
    next_segment_id: AtomicU64,
    /// Next entity ID counter
    next_entity_id: AtomicU64,
    /// Per-table auto-increment row ID (1, 2, 3... per collection)
    next_row_id: AtomicU64,
    /// Hot-path entity counter — lock-free, updated by every insert/delete.
    /// Replaces stats.total_entities on the write path to eliminate a lock
    /// acquisition per row (from 4 lock ops per insert → 2).
    total_entities_atomic: AtomicU64,
    /// Currently active growing segment
    growing: RwLock<Option<Arc<RwLock<GrowingSegment>>>>,
    /// Sealed segments (immutable, queryable)
    sealed: RwLock<Vec<Arc<RwLock<GrowingSegment>>>>,
    /// Archived segment IDs (stored externally)
    archived: RwLock<Vec<SegmentId>>,
    /// Entity to segment mapping (for fast lookups by individually-inserted entities).
    /// Bulk-inserted entities skip this map; their segment is found by sequential scan
    /// of growing + sealed segments in get()/update()/delete().
    entity_segment: RwLock<HashMap<EntityId, SegmentId>>,
    /// Shared column schema: column_name → index in Vec<Value>.
    /// Populated on first bulk_insert. Enables columnar storage (Vec instead of HashMap per row).
    column_schema: RwLock<Option<Arc<Vec<String>>>>,
    /// Statistics (slow path — not updated on every insert).
    stats: RwLock<ManagerStats>,
    /// Event listeners (simplified - would be channels in production)
    events: RwLock<Vec<LifecycleEvent>>,
    /// Visibility map: sealed segment entity ranges marked as all-visible.
    /// Growing segment is never all-visible (writes are in-flight).
    /// Used by index-only scan decisions.
    visibility_map: VisibilityMap,
}

impl SegmentManager {
    /// Create a new segment manager
    pub fn new(collection: impl Into<String>) -> Self {
        Self::with_config(collection, ManagerConfig::default())
    }

    /// Create with custom configuration
    pub fn with_config(collection: impl Into<String>, config: ManagerConfig) -> Self {
        Self {
            collection: collection.into(),
            config,
            next_segment_id: AtomicU64::new(1),
            next_entity_id: AtomicU64::new(1),
            next_row_id: AtomicU64::new(1),
            total_entities_atomic: AtomicU64::new(0),
            growing: RwLock::new(None),
            sealed: RwLock::new(Vec::new()),
            archived: RwLock::new(Vec::new()),
            entity_segment: RwLock::new(HashMap::new()),
            column_schema: RwLock::new(None),
            stats: RwLock::new(ManagerStats::default()),
            events: RwLock::new(Vec::new()),
            visibility_map: VisibilityMap::new(),
        }
    }

    /// Get or create the shared column schema from first row's named fields.
    pub fn get_or_init_schema(
        &self,
        named: &HashMap<String, crate::storage::schema::Value>,
    ) -> Arc<Vec<String>> {
        {
            let schema = self.column_schema.read();
            if let Some(ref s) = *schema {
                return Arc::clone(s);
            }
        }
        let cols: Vec<String> = named.keys().cloned().collect();
        let arc = Arc::new(cols);
        *self.column_schema.write() = Some(Arc::clone(&arc));
        arc
    }

    /// Get the column schema if it exists.
    pub fn column_schema(&self) -> Option<Arc<Vec<String>>> {
        self.column_schema.read().clone()
    }

    /// Get collection name
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// Get configuration
    pub fn config(&self) -> &ManagerConfig {
        &self.config
    }

    /// Get statistics. total_entities is read from the lock-free atomic;
    /// other fields come from the slow-path stats struct.
    pub fn stats(&self) -> ManagerStats {
        let mut s = self.stats.read().clone();
        s.total_entities = self.total_entities_atomic.load(Ordering::Relaxed) as usize;
        s
    }

    /// Generate a new entity ID
    pub fn next_entity_id(&self) -> EntityId {
        EntityId::new(self.next_entity_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Generate a per-table sequential row ID (1, 2, 3... per collection)
    pub fn next_row_id(&self) -> u64 {
        self.next_row_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Reserve `n` contiguous per-table row IDs with one atomic
    /// fetch_add. Caller assigns `row_id = start + i` per entity.
    /// Saves N-1 atomic RMWs on bulk inserts (25k atomics → 1).
    pub fn reserve_row_ids(&self, n: u64) -> std::ops::Range<u64> {
        let start = self.next_row_id.fetch_add(n, Ordering::SeqCst);
        start..start + n
    }

    /// Advance the per-table row_id counter to at least `id + 1`.
    /// Called during load to restore the counter from existing data.
    pub fn register_row_id(&self, id: u64) {
        let candidate = id.saturating_add(1);
        let mut current = self.next_row_id.load(Ordering::SeqCst);
        while candidate > current {
            match self.next_row_id.compare_exchange(
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

    /// Get or create the active growing segment.
    ///
    /// Fast path: read lock only — no write contention when the segment already exists.
    /// Concurrent writers each clone the `Arc` under a shared read lock, then compete
    /// on the segment's own write lock. This eliminates the global write-lock serialisation
    /// that previously throttled concurrent inserts to ~238 ops/s.
    fn get_or_create_growing(&self) -> Arc<RwLock<GrowingSegment>> {
        // Common case: segment already exists — shared read lock, zero contention.
        {
            let growing = self.growing.read();
            if let Some(segment) = growing.as_ref() {
                return Arc::clone(segment);
            }
        }

        // Slow path: segment missing — take exclusive write lock to create it.
        let mut growing = self.growing.write();
        // Double-check: another thread may have created it between the two lock acquisitions.
        if let Some(segment) = growing.as_ref() {
            return Arc::clone(segment);
        }

        let id = self.next_segment_id.fetch_add(1, Ordering::SeqCst);
        let segment = GrowingSegment::new(id, &self.collection);
        let segment_arc = Arc::new(RwLock::new(segment));
        *growing = Some(Arc::clone(&segment_arc));

        self.emit(LifecycleEvent::SegmentCreated(id));

        // Update growing_count in the slow-path stats struct.
        // This is the rare segment-creation path — locking is fine here.
        self.stats.write().growing_count += 1;

        segment_arc
    }

    /// Insert a new entity
    pub fn insert(&self, mut entity: UnifiedEntity) -> Result<EntityId, SegmentError> {
        // Check if we need to seal the current segment first
        self.maybe_seal_growing()?;

        let segment_arc = self.get_or_create_growing();
        let mut segment = segment_arc.write();

        // Assign entity ID if not already set
        if entity.id.raw() == 0 {
            entity.id = self.next_entity_id();
        }

        let entity_id = entity.id;
        let segment_id = segment.id();

        segment.insert(entity)?;

        // Lock-free counter update — eliminates the stats write lock on the hot path.
        self.total_entities_atomic.fetch_add(1, Ordering::Relaxed);

        // entity_segment map is intentionally NOT updated here.
        // update() and update_hot() first probe the growing segment directly
        // (growing.contains(entity.id)) before consulting this map, so entities
        // that were just inserted are found without entity_segment. The map is
        // only consulted for entities that may have been moved to sealed segments,
        // which can't be updated anyway (state().is_writable() == false).
        // Skipping this write removes one exclusive HashMap lock per insert.

        self.emit(LifecycleEvent::EntityInserted(entity_id, segment_id));

        Ok(entity_id)
    }

    /// Insert multiple entities (batch) — sequential, one lock per item.
    pub fn insert_batch(
        &self,
        entities: Vec<UnifiedEntity>,
    ) -> Result<Vec<EntityId>, SegmentError> {
        let mut ids = Vec::with_capacity(entities.len());
        for entity in entities {
            ids.push(self.insert(entity)?);
        }
        Ok(ids)
    }

    /// Turbo bulk insert — single lock acquisition for the entire batch.
    /// Skips bloom filter, memtable, and cross-ref indexing for maximum speed.
    pub fn bulk_insert(
        &self,
        mut entities: Vec<UnifiedEntity>,
    ) -> Result<Vec<EntityId>, SegmentError> {
        // Assign IDs and per-table row_ids.
        for entity in &mut entities {
            if entity.id.raw() == 0 {
                entity.id = self.next_entity_id();
            }
            if let super::entity::EntityKind::TableRow { ref mut row_id, .. } = entity.kind {
                if *row_id == 0 {
                    *row_id = self.next_row_id();
                } else {
                    self.register_row_id(*row_id);
                }
            }
        }

        // Convert named HashMap → positional Vec (compact memory representation)
        // The schema (column order) is shared across all rows in the collection.
        if let Some(first_row) = entities.first() {
            if let super::entity::EntityData::Row(ref row) = first_row.data {
                if let Some(ref named) = row.named {
                    let schema = self.get_or_init_schema(named);
                    for entity in &mut entities {
                        if let super::entity::EntityData::Row(ref mut row) = entity.data {
                            if let Some(named) = row.named.take() {
                                let mut cols = Vec::with_capacity(schema.len());
                                for col_name in schema.iter() {
                                    cols.push(
                                        named
                                            .get(col_name)
                                            .cloned()
                                            .unwrap_or(crate::storage::schema::Value::Null),
                                    );
                                }
                                row.columns = cols;
                                row.schema = Some(Arc::clone(&schema));
                            }
                        }
                    }
                }
            }
        }

        let segment_arc = self.get_or_create_growing();
        let mut segment = segment_arc.write();
        let segment_id = segment.id();

        // Single call to GrowingSegment.bulk_insert (one lock, no bloom/memtable)
        let ids = segment.bulk_insert(entities)?;

        // Skip entity-segment mapping for bulk inserts (saves ~56 bytes/entity).
        // The get() method scans growing+sealed segments directly.

        // Lock-free batch counter update.
        self.total_entities_atomic
            .fetch_add(ids.len() as u64, Ordering::Relaxed);

        Ok(ids)
    }

    /// Get an entity by ID — scans growing then sealed segments.
    pub fn get(&self, id: EntityId) -> Option<UnifiedEntity> {
        // Growing segment first (most likely for recent inserts)
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = growing_arc.read();
            if let Some(entity) = growing.get(id) {
                return Some(entity.clone());
            }
        }

        // Then sealed segments
        let sealed = self.sealed.read();
        for segment in sealed.iter() {
            let seg = segment.read();
            if let Some(entity) = seg.get(id) {
                return Some(entity.clone());
            }
        }

        None
    }

    /// Batch-fetch multiple entities by ID in a single lock acquisition per segment.
    ///
    /// For indexed-scan result sets (up to ~5000 ids from range/bitmap lookup) this
    /// is 2-3 lock acquisitions total vs N×3 with individual `get()` calls.
    pub fn get_many(&self, ids: &[EntityId]) -> Vec<Option<UnifiedEntity>> {
        let mut out: Vec<Option<UnifiedEntity>> = vec![None; ids.len()];
        let mut remaining: Vec<usize> = (0..ids.len()).collect(); // indices still unfound

        // Growing segment — one read lock for the entire batch.
        // Non-blocking first: if a writer is active, fall back to blocking.
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            remaining.retain(|&i| {
                if let Some(entity) = growing.get(ids[i]) {
                    out[i] = Some(entity.clone());
                    false // remove from remaining
                } else {
                    true // keep — not found yet
                }
            });
        }

        if remaining.is_empty() {
            return out;
        }

        // Sealed segments — one read lock per segment
        let sealed = self.sealed.read();
        for segment in sealed.iter() {
            if remaining.is_empty() {
                break;
            }
            let seg = segment.read();
            remaining.retain(|&i| {
                if let Some(entity) = seg.get(ids[i]) {
                    out[i] = Some(entity.clone());
                    false
                } else {
                    true
                }
            });
        }

        out
    }

    /// Visitor-pattern batch fetch. Invokes `f(&UnifiedEntity, usize_index)`
    /// for each id that resolves, never cloning the entity.
    ///
    /// Used by scan hot paths (select_range, select_filtered) that
    /// materialize each entity into an output record and don't need
    /// an owned `UnifiedEntity`. Eliminates ~20% of scan CPU spent in
    /// `UnifiedEntity::clone` when `get_batch` is followed by
    /// `runtime_table_record_lean(entity)`.
    ///
    /// The closure runs while the segment read lock is held, so it
    /// must be short — avoid doing I/O or taking unrelated locks in
    /// `f`.
    pub fn for_each_id<F>(&self, ids: &[EntityId], mut f: F)
    where
        F: FnMut(usize, &UnifiedEntity),
    {
        // Thread-local scratch buffer for the "pending" index list.
        // Previous code allocated a fresh `Vec<usize>` of capacity
        // N on every call — 4200 × 1000 queries / scenario on the
        // select_range bench path. Take-and-restore pattern (vs
        // RefCell::borrow_mut) so user closures that recurse into
        // another `for_each_id` don't panic on a re-borrow; worst
        // case they allocate a fresh buffer and we lose the caching
        // win for that nested call.
        thread_local! {
            static REMAINING_SCRATCH: std::cell::Cell<Vec<usize>> =
                const { std::cell::Cell::new(Vec::new()) };
        }

        let mut remaining: Vec<usize> = REMAINING_SCRATCH.with(|cell| cell.take());
        remaining.clear();

        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            for (i, id) in ids.iter().enumerate() {
                if let Some(entity) = growing.get(*id) {
                    f(i, entity);
                } else {
                    remaining.push(i);
                }
            }
        } else {
            remaining.reserve(ids.len());
            remaining.extend(0..ids.len());
        }

        if !remaining.is_empty() {
            let sealed = self.sealed.read();
            for segment in sealed.iter() {
                if remaining.is_empty() {
                    break;
                }
                let seg = segment.read();
                remaining.retain(|&i| {
                    if let Some(entity) = seg.get(ids[i]) {
                        f(i, entity);
                        false
                    } else {
                        true
                    }
                });
            }
        }

        REMAINING_SCRATCH.with(|cell| cell.set(remaining));
    }

    /// Scan all segments for an entity
    fn scan_for_entity(&self, id: EntityId) -> Option<UnifiedEntity> {
        // Check growing
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = growing_arc.read();
            if let Some(entity) = growing.get(id) {
                return Some(entity.clone());
            }
        }

        // Check sealed
        let sealed = self.sealed.read();
        for segment in sealed.iter() {
            if let Some(entity) = segment.get(id) {
                return Some(entity.clone());
            }
        }

        None
    }

    fn find_sealed_segment_arc(&self, id: EntityId) -> Option<Arc<RwLock<GrowingSegment>>> {
        let sealed = self.sealed.read();
        sealed
            .iter()
            .find(|segment_arc| segment_arc.read().contains(id))
            .map(Arc::clone)
    }

    fn rewrite_sealed_entity_into_growing(
        &self,
        entity: UnifiedEntity,
        metadata: Option<&Metadata>,
    ) -> Result<(), SegmentError> {
        let entity_id = entity.id;
        let sealed_arc = self
            .find_sealed_segment_arc(entity_id)
            .ok_or(SegmentError::NotFound(entity_id))?;

        let metadata_to_apply = {
            let mut sealed = sealed_arc.write();
            let existing_metadata = sealed.get_metadata(entity_id);
            if !sealed.force_delete(entity_id) {
                return Err(SegmentError::NotFound(entity_id));
            }
            metadata.cloned().or(existing_metadata)
        };

        let growing_arc = self.get_or_create_growing();
        let growing_id = {
            let mut growing = growing_arc.write();
            growing.insert(entity)?;
            if let Some(metadata) = metadata_to_apply {
                growing.set_metadata(entity_id, metadata)?;
            }
            growing.id()
        };

        self.entity_segment.write().insert(entity_id, growing_id);
        Ok(())
    }

    /// Update an entity
    pub fn update(&self, entity: UnifiedEntity) -> Result<(), SegmentError> {
        let entity_id = entity.id;
        let mut entity = Some(entity);

        // Try growing segment directly (covers bulk-inserted entities without entity_segment map)
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let mut growing = growing_arc.write();
            if growing.contains(entity_id) && growing.state().is_writable() {
                return growing.update(entity.take().expect("entity already moved"));
            }
        }

        // Try entity_segment mapping for individually inserted entities
        let segment_id = self.entity_segment.read().get(&entity_id).copied();
        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().as_ref() {
                let mut growing = growing_arc.write();
                if growing.id() == seg_id && growing.state().is_writable() {
                    return growing.update(entity.take().expect("entity already moved"));
                }
            }
        }

        if let Some(entity) = entity.take() {
            return self.rewrite_sealed_entity_into_growing(entity, None);
        }

        Err(SegmentError::NotFound(entity_id))
    }

    /// Update an entity and, optionally, replace its metadata while holding
    /// the segment write lock only once.
    pub fn update_with_metadata(
        &self,
        entity: UnifiedEntity,
        metadata: Option<&Metadata>,
    ) -> Result<(), SegmentError> {
        let entity_id = entity.id;
        let mut entity = Some(entity);

        // Try growing segment directly (covers bulk-inserted entities without entity_segment map)
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let mut growing = growing_arc.write();
            if growing.contains(entity_id) && growing.state().is_writable() {
                growing.update(entity.take().expect("entity already moved"))?;
                if let Some(metadata) = metadata {
                    growing.set_metadata(entity_id, metadata.clone())?;
                }
                return Ok(());
            }
        }

        // Try entity_segment mapping for individually inserted entities
        let segment_id = self.entity_segment.read().get(&entity_id).copied();
        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().as_ref() {
                let mut growing = growing_arc.write();
                if growing.id() == seg_id && growing.state().is_writable() {
                    growing.update(entity.take().expect("entity already moved"))?;
                    if let Some(metadata) = metadata {
                        growing.set_metadata(entity_id, metadata.clone())?;
                    }
                    return Ok(());
                }
            }
        }

        if let Some(entity) = entity.take() {
            return self.rewrite_sealed_entity_into_growing(entity, metadata);
        }

        Err(SegmentError::NotFound(entity_id))
    }

    /// HOT-update: like update but skips index work for unchanged columns.
    /// `modified_columns` is the list of column names actually changed by the
    /// UPDATE statement — lets us skip pk_index and cross_ref when safe.
    pub fn update_hot(
        &self,
        entity: UnifiedEntity,
        modified_columns: &[String],
    ) -> Result<(), SegmentError> {
        let entity_id = entity.id;
        let mut entity = Some(entity);

        if let Some(growing_arc) = self.growing.read().as_ref() {
            let mut growing = growing_arc.write();
            if growing.contains(entity_id) && growing.state().is_writable() {
                return growing.update_hot(
                    entity.take().expect("entity already moved"),
                    modified_columns,
                );
            }
        }

        let segment_id = self.entity_segment.read().get(&entity_id).copied();
        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().as_ref() {
                let mut growing = growing_arc.write();
                if growing.id() == seg_id && growing.state().is_writable() {
                    return growing.update_hot(
                        entity.take().expect("entity already moved"),
                        modified_columns,
                    );
                }
            }
        }

        if let Some(entity) = entity.take() {
            return self.rewrite_sealed_entity_into_growing(entity, None);
        }

        Err(SegmentError::NotFound(entity_id))
    }

    /// HOT-update an entity and, optionally, replace its metadata while
    /// holding the segment write lock only once.
    pub fn update_hot_with_metadata(
        &self,
        entity: UnifiedEntity,
        modified_columns: &[String],
        metadata: Option<&Metadata>,
    ) -> Result<(), SegmentError> {
        let entity_id = entity.id;
        let mut entity = Some(entity);

        if let Some(growing_arc) = self.growing.read().as_ref() {
            let mut growing = growing_arc.write();
            if growing.contains(entity_id) && growing.state().is_writable() {
                growing.update_hot(
                    entity.take().expect("entity already moved"),
                    modified_columns,
                )?;
                if let Some(metadata) = metadata {
                    growing.set_metadata(entity_id, metadata.clone())?;
                }
                return Ok(());
            }
        }

        let segment_id = self.entity_segment.read().get(&entity_id).copied();
        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().as_ref() {
                let mut growing = growing_arc.write();
                if growing.id() == seg_id && growing.state().is_writable() {
                    growing.update_hot(
                        entity.take().expect("entity already moved"),
                        modified_columns,
                    )?;
                    if let Some(metadata) = metadata {
                        growing.set_metadata(entity_id, metadata.clone())?;
                    }
                    return Ok(());
                }
            }
        }

        if let Some(entity) = entity.take() {
            return self.rewrite_sealed_entity_into_growing(entity, metadata);
        }

        Err(SegmentError::NotFound(entity_id))
    }

    /// Batch HOT-update multiple entities while holding the growing-segment
    /// write lock only once when possible.
    pub fn update_hot_batch_with_metadata<'a, I>(&self, items: I) -> Result<(), SegmentError>
    where
        I: IntoIterator<Item = (&'a UnifiedEntity, &'a [String], Option<&'a Metadata>)>,
    {
        let items: Vec<(&UnifiedEntity, &[String], Option<&Metadata>)> =
            items.into_iter().collect();
        if items.is_empty() {
            return Ok(());
        }

        if let Some(growing_arc) = self.growing.read().as_ref() {
            let mut growing = growing_arc.write();
            if growing.state().is_writable() {
                match growing.update_hot_batch_with_metadata(items.iter().copied()) {
                    Ok(()) => return Ok(()),
                    Err(SegmentError::NotFound(_)) => {}
                    Err(other) => return Err(other),
                }
            }
        }

        for (entity, modified_columns, metadata) in items {
            self.update_hot_with_metadata(entity.clone(), modified_columns, metadata)?;
        }
        Ok(())
    }

    /// Delete an entity
    pub fn delete(&self, id: EntityId) -> Result<bool, SegmentError> {
        // Fast path: probe the growing segment directly — covers entities inserted via
        // insert() which no longer writes to entity_segment, and bulk-inserted entities.
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let mut growing = growing_arc.write();
            if growing.contains(id) && growing.state().is_writable() {
                let seg_id = growing.id();
                let deleted = growing.delete(id)?;
                if deleted {
                    self.entity_segment.write().remove(&id);
                    self.total_entities_atomic.fetch_sub(1, Ordering::Relaxed);
                    self.emit(LifecycleEvent::EntityDeleted(id, seg_id));
                }
                return Ok(deleted);
            }
        }

        // Fallback: check entity_segment map (populated for older insert() paths
        // or entities that were in a previous growing segment).
        let segment_id = self.entity_segment.read().get(&id).copied();

        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().as_ref() {
                let mut growing = growing_arc.write();
                if growing.id() == seg_id && growing.state().is_writable() {
                    let deleted = growing.delete(id)?;
                    if deleted {
                        self.entity_segment.write().remove(&id);
                        self.total_entities_atomic.fetch_sub(1, Ordering::Relaxed);
                        self.emit(LifecycleEvent::EntityDeleted(id, seg_id));
                    }
                    return Ok(deleted);
                }
            }
        }

        // Fallback: entity is in a sealed segment (bulk-inserted, not in entity_segment map).
        // Single write-lock per segment to avoid TOCTOU race between contains() and force_delete().
        {
            let sealed = self.sealed.read();
            for segment_arc in sealed.iter() {
                let mut seg = segment_arc.write();
                let seg_id = seg.id();
                if seg.contains(id) {
                    let deleted = seg.force_delete(id);
                    drop(seg);
                    if deleted {
                        self.entity_segment.write().remove(&id);
                        self.total_entities_atomic.fetch_sub(1, Ordering::Relaxed);
                        self.emit(LifecycleEvent::EntityDeleted(id, seg_id));
                    }
                    return Ok(deleted);
                }
            }
        }

        Ok(false)
    }

    pub fn delete_batch(&self, ids: &[EntityId]) -> Result<Vec<EntityId>, SegmentError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut deleted_ids = Vec::with_capacity(ids.len());

        if let Some(growing_arc) = self.growing.read().as_ref() {
            let mut growing = growing_arc.write();
            if growing.state().is_writable() {
                let seg_id = growing.id();
                let deleted = growing.delete_batch(ids)?;
                if !deleted.is_empty() {
                    {
                        let mut entity_segment = self.entity_segment.write();
                        for id in &deleted {
                            entity_segment.remove(id);
                        }
                    }
                    self.total_entities_atomic
                        .fetch_sub(deleted.len() as u64, Ordering::Relaxed);
                    for id in &deleted {
                        self.emit(LifecycleEvent::EntityDeleted(*id, seg_id));
                    }
                    deleted_ids.extend(deleted);
                }
            }
        }

        if deleted_ids.len() == ids.len() {
            return Ok(deleted_ids);
        }

        let deleted_set: std::collections::HashSet<EntityId> =
            deleted_ids.iter().copied().collect();
        for &id in ids {
            if deleted_set.contains(&id) {
                continue;
            }
            if self.delete(id)? {
                deleted_ids.push(id);
            }
        }

        Ok(deleted_ids)
    }

    /// Get metadata for an entity
    pub fn get_metadata(&self, id: EntityId) -> Option<Metadata> {
        // Fast path: probe growing segment directly (no entity_segment needed).
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = growing_arc.read();
            if growing.contains(id) {
                return growing.get_metadata(id);
            }
        }

        // Fallback: entity_segment map (for pre-existing or sealed entities)
        let segment_id = self.entity_segment.read().get(&id).copied();

        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().as_ref() {
                let growing = growing_arc.read();
                if growing.id() == seg_id {
                    return growing.get_metadata(id);
                }
            }

            let sealed = self.sealed.read();
            for segment in sealed.iter() {
                if segment.id() == seg_id {
                    return segment.get_metadata(id);
                }
            }
        }

        if let Some(segment_arc) = self.find_sealed_segment_arc(id) {
            return segment_arc.read().get_metadata(id);
        }

        None
    }

    /// Set metadata for an entity
    pub fn set_metadata(&self, id: EntityId, metadata: Metadata) -> Result<(), SegmentError> {
        // Fast path: probe growing segment directly — covers entities inserted via
        // insert() which no longer writes to entity_segment.
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let mut growing = growing_arc.write();
            if growing.contains(id) && growing.state().is_writable() {
                return growing.set_metadata(id, metadata);
            }
        }

        // Fallback: entity_segment map (sealed or pre-atomic-path entities)
        let segment_id = self.entity_segment.read().get(&id).copied();

        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().as_ref() {
                let mut growing = growing_arc.write();
                if growing.id() == seg_id && growing.state().is_writable() {
                    return growing.set_metadata(id, metadata);
                }
            }
        }

        if let Some(entity) = self.get(id) {
            return self.rewrite_sealed_entity_into_growing(entity, Some(&metadata));
        }

        Err(SegmentError::NotFound(id))
    }

    /// Check if growing segment should be sealed
    fn maybe_seal_growing(&self) -> Result<(), SegmentError> {
        let should_seal = {
            let growing_opt = self.growing.read();
            if let Some(growing_arc) = growing_opt.as_ref() {
                let growing = growing_arc.read();
                growing.should_seal(&self.config.segment_config)
                    || growing.idle_secs() >= self.config.idle_seal_secs
            } else {
                false
            }
        };

        if should_seal {
            self.seal_current()?;
        }

        Ok(())
    }

    /// Seal the current growing segment
    pub fn seal_current(&self) -> Result<SegmentId, SegmentError> {
        let growing_opt = self.growing.write().take();

        if let Some(growing_arc) = growing_opt {
            let mut growing = growing_arc.write();
            let seg_id = growing.id();
            let entity_count = growing.stats().entity_count as u64;

            // Seal it
            growing.seal()?;

            // Move to sealed list (we need to extract it from the Arc)
            drop(growing); // Release write lock

            // In a real implementation, we'd convert to SealedSegment here
            // For now, we keep it as-is since GrowingSegment implements UnifiedSegment
            self.sealed.write().push(growing_arc);

            // Mark sealed segment pages all-visible — they're now immutable
            self.mark_sealed_pages_visible(entity_count);

            // Update stats
            {
                let mut stats = self.stats.write();
                stats.growing_count = stats.growing_count.saturating_sub(1);
                stats.sealed_count += 1;
                stats.seal_ops += 1;
            }

            self.emit(LifecycleEvent::SegmentSealed(seg_id));

            return Ok(seg_id);
        }

        Err(SegmentError::InvalidState(SegmentState::Sealed))
    }

    /// Force seal (for testing/manual control)
    pub fn force_seal(&self) -> Result<Option<SegmentId>, SegmentError> {
        let has_growing = self.growing.read().is_some();
        if has_growing {
            Ok(Some(self.seal_current()?))
        } else {
            Ok(None)
        }
    }

    /// Fraction of "pages" in sealed segments that are marked all-visible.
    ///
    /// Sealed segments are immutable so all their rows are safe for
    /// index-only scans. The growing segment is never counted (writes
    /// may be in-flight). Uses `rows_per_page = 256` (matching 8 KB pages
    /// with ~32-byte rows).
    ///
    /// Returns a value in `[0.0, 1.0]`. 1.0 when all sealed rows are
    /// visible; 0.0 when there are no sealed segments.
    pub fn all_visible_fraction(&self) -> f64 {
        const ROWS_PER_PAGE: u32 = 256;
        let sealed = self.sealed.read();
        if sealed.is_empty() {
            return 0.0;
        }
        let mut total_pages: u64 = 0;
        for seg_arc in sealed.iter() {
            let seg = seg_arc.read();
            let entity_count = seg.stats().entity_count as u64;
            let pages = (entity_count + ROWS_PER_PAGE as u64 - 1) / ROWS_PER_PAGE as u64;
            total_pages += pages;
        }
        if total_pages == 0 {
            return 0.0;
        }
        let visible = self.visibility_map.all_visible_count();
        (visible as f64 / total_pages as f64).min(1.0)
    }

    /// Mark all pages of newly sealed segments as all-visible in the
    /// visibility map. Called internally after `seal_current`.
    fn mark_sealed_pages_visible(&self, seg_entity_count: u64) {
        const ROWS_PER_PAGE: u32 = 256;
        let existing_visible = self.visibility_map.all_visible_count();
        // Append pages starting after the last known visible page
        let start_page = existing_visible as u32;
        let new_pages = (seg_entity_count + ROWS_PER_PAGE as u64 - 1) / ROWS_PER_PAGE as u64;
        let end_page = start_page + new_pages as u32;
        self.visibility_map.mark_range_visible(start_page, end_page);
    }

    /// Iterate over all entities in-place without collecting into a Vec.
    ///
    /// The callback receives a reference to each entity. Return `true` to
    /// continue iteration, `false` to stop early (e.g. when a LIMIT is reached).
    /// This avoids the allocation and cloning overhead of `query_all`.
    pub fn for_each_entity<F>(&self, mut callback: F)
    where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        // Growing segment — direct iteration (no Box<dyn>)
        // Try non-blocking read first; fall back to blocking only when a writer
        // is actively holding the write lock (rare in read-heavy workloads).
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            if !growing.for_each_fast(&mut callback) {
                return;
            }
        }

        // Sealed segments
        let sealed = self.sealed.read();
        for segment_arc in sealed.iter() {
            let segment = segment_arc.read();
            if !segment.for_each_fast(&mut callback) {
                return;
            }
        }
    }

    /// Parallel fold across all entities. Each sealed segment is
    /// processed on its own rayon task; the growing segment stays on
    /// the caller thread (its read lock is briefly held).
    ///
    /// - `init` builds a fresh accumulator per thread.
    /// - `fold` mutates an accumulator with one entity at a time.
    /// - `reduce` combines two accumulators into one.
    ///
    /// The returned value is the reduction of every per-thread
    /// accumulator. Use this for aggregate-shape workloads (GROUP BY)
    /// where per-thread partial state can be merged cheaply.
    ///
    /// NOTE: when there are 0 or 1 sealed segments, the parallel path
    /// is skipped and the work runs sequentially to avoid rayon
    /// overhead on tiny tables.
    pub fn fold_entities_parallel<T, FInit, FFold, FReduce>(
        &self,
        init: FInit,
        fold: FFold,
        reduce: FReduce,
    ) -> T
    where
        T: Send,
        FInit: Fn() -> T + Send + Sync,
        FFold: Fn(T, &UnifiedEntity) -> T + Send + Sync,
        FReduce: Fn(T, T) -> T + Send + Sync,
    {
        use rayon::prelude::*;

        // Growing segment — always sequential (single writer lock,
        // usually small working set).
        let mut acc = init();
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            growing.for_each_fast(|entity| {
                acc = fold(std::mem::replace(&mut acc, init()), entity);
                true
            });
        }

        // Sealed segments — snapshot the Arc list under the read lock,
        // then drop the lock so rayon workers can fan out without
        // blocking writers.
        let segments: Vec<_> = {
            let sealed = self.sealed.read();
            sealed.iter().cloned().collect()
        };

        if segments.len() <= 1 {
            for seg_arc in &segments {
                let seg = seg_arc.read();
                seg.for_each_fast(|entity| {
                    acc = fold(std::mem::replace(&mut acc, init()), entity);
                    true
                });
            }
            return acc;
        }

        let sealed_acc = segments
            .into_par_iter()
            .map(|seg_arc| {
                let mut local = init();
                let seg = seg_arc.read();
                seg.for_each_fast(|entity| {
                    local = fold(std::mem::replace(&mut local, init()), entity);
                    true
                });
                local
            })
            .reduce(&init, &reduce);

        reduce(acc, sealed_acc)
    }

    /// Zone-map-aware iteration across all segments.
    ///
    /// Like `for_each_entity`, but checks `zone_preds` against each segment's
    /// column zone maps before iterating. Segments where any predicate can
    /// definitively prove no rows match are skipped entirely.
    ///
    /// `zone_preds`: slice of `(column_name, ZoneColPred)` extracted from the WHERE clause.
    /// Empty slice → same behaviour as `for_each_entity` (no pruning).
    pub fn for_each_entity_zoned<F>(&self, zone_preds: &[(&str, ZoneColPred<'_>)], mut callback: F)
    where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        // Growing segment — never skip (it's receiving writes, zones are partial).
        // Try a non-blocking read first: if a writer is currently inserting
        // (holding the write lock), try_read() returns None and we fall back to
        // the blocking read.  In low-contention workloads (reads far outnumber
        // writes) the try_read() almost always succeeds and readers never stall.
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            if !growing.for_each_fast(&mut callback) {
                return;
            }
        }

        // Sealed segments — check zone maps before iterating
        let sealed = self.sealed.read();
        for segment_arc in sealed.iter() {
            let segment = segment_arc.read();
            if !zone_preds.is_empty() && segment.can_skip_zone_preds(zone_preds) {
                continue; // entire segment pruned
            }
            if !segment.for_each_fast(&mut callback) {
                return;
            }
        }
    }

    /// Zone-map-aware parallel query.
    ///
    /// Like `query_all` but applies `zone_preds` on the main thread to
    /// prune sealed segments before spawning workers — segments that
    /// provably contain no matching rows are skipped entirely.
    ///
    /// Zone check runs single-threaded (it reads per-segment metadata,
    /// not row data), so it's cheap. Surviving segments are then scanned
    /// in parallel using `std::thread::scope` when there are > 1 of them.
    pub fn query_all_zoned<F>(
        &self,
        zone_preds: &[(&str, ZoneColPred<'_>)],
        filter: F,
    ) -> Vec<UnifiedEntity>
    where
        F: Fn(&UnifiedEntity) -> bool + Sync,
    {
        let mut results = Vec::new();

        // Growing segment — always scan, no zone skip (zones are partial).
        // Non-blocking try_read() avoids stalling behind in-progress inserts.
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            results.extend(growing.iter().filter(|e| filter(e)).cloned());
        }

        // Sealed segments: zone-prune on main thread, then scan in parallel.
        let sealed = self.sealed.read();
        // Collect only the segments that survive zone-predicate pruning.
        let surviving: Vec<_> = sealed
            .iter()
            .filter(|seg_arc| {
                if zone_preds.is_empty() {
                    return true;
                }
                let seg = seg_arc.read();
                !seg.can_skip_zone_preds(zone_preds)
            })
            .collect();

        let use_parallel = surviving.len() > 1 && crate::runtime::SystemInfo::should_parallelize();

        if use_parallel {
            let filter_ref = &filter;
            let segment_results: Vec<Vec<UnifiedEntity>> = std::thread::scope(|s| {
                surviving
                    .iter()
                    .map(|segment| {
                        s.spawn(move || {
                            segment
                                .read()
                                .iter()
                                .filter(|e| filter_ref(e))
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|handle| handle.join().unwrap_or_default())
                    .collect()
            });
            for batch in segment_results {
                results.extend(batch);
            }
        } else {
            for segment_arc in surviving {
                let seg = segment_arc.read();
                results.extend(seg.iter().filter(|e| filter(e)).cloned());
            }
        }

        results
    }

    /// Query across all segments. Uses parallel scanning for sealed segments
    /// when more than one sealed segment exists.
    pub fn query_all<F>(&self, filter: F) -> Vec<UnifiedEntity>
    where
        F: Fn(&UnifiedEntity) -> bool + Sync,
    {
        let mut results = Vec::new();

        // Query growing segment — try non-blocking read first (avoids stalling
        // behind an in-progress insert; falls back to blocking if writer is active).
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            results.extend(growing.iter().filter(|e| filter(e)).cloned());
        }

        // Query sealed segments — parallel when multiple exist AND multi-core
        let sealed = self.sealed.read();
        let use_parallel = sealed.len() > 1 && crate::runtime::SystemInfo::should_parallelize();
        if use_parallel {
            let filter_ref = &filter;
            let segment_results: Vec<Vec<UnifiedEntity>> = std::thread::scope(|s| {
                sealed
                    .iter()
                    .map(|segment| {
                        s.spawn(move || {
                            segment
                                .read()
                                .iter()
                                .filter(|e| filter_ref(e))
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|handle| handle.join().unwrap_or_default())
                    .collect()
            });
            for batch in segment_results {
                results.extend(batch);
            }
        } else {
            for segment in sealed.iter() {
                let seg = segment.read();
                results.extend(seg.iter().filter(|e| filter(e)).cloned());
            }
        }

        results
    }

    /// Query with bloom filter hint: skip the growing segment when bloom says key is absent.
    ///
    /// This is the integration point for bloom filter pruning.
    /// When a query has an equality predicate on a known key, the executor
    /// can call this instead of `query_all` to avoid scanning when the
    /// bloom filter proves the key doesn't exist.
    ///
    /// Returns (results, bloom_pruned) where bloom_pruned indicates if the
    /// segment was skipped.
    pub fn query_with_bloom_hint<F>(
        &self,
        key_hint: Option<&[u8]>,
        filter: F,
    ) -> (Vec<UnifiedEntity>, bool)
    where
        F: Fn(&UnifiedEntity) -> bool,
    {
        let mut results = Vec::new();
        let mut bloom_pruned = false;

        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = growing_arc.read();
            if let Some(key) = key_hint {
                if !growing.bloom_might_contain_key(key) {
                    bloom_pruned = true;
                    return (results, bloom_pruned);
                }
            }
            for entity in growing.iter() {
                if filter(entity) {
                    results.push(entity.clone());
                }
            }
        }

        // Sealed segments (currently empty iter, but future-proofed)
        let sealed = self.sealed.read();
        for segment_arc in sealed.iter() {
            let segment = segment_arc.read();
            if let Some(key) = key_hint {
                if !segment.bloom_might_contain_key(key) {
                    bloom_pruned = true;
                    continue;
                }
            }
            for entity in segment.iter() {
                if filter(entity) {
                    results.push(entity.clone());
                }
            }
        }

        (results, bloom_pruned)
    }

    /// Filter by metadata across all segments
    pub fn filter_metadata(&self, filters: &[(String, MetadataFilter)]) -> Vec<EntityId> {
        let mut results = Vec::new();

        // Growing segment
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = growing_arc.read();
            results.extend(growing.filter_metadata(filters));
        }

        // Sealed segments
        let sealed = self.sealed.read();
        for segment in sealed.iter() {
            results.extend(segment.filter_metadata(filters));
        }

        results
    }

    /// Get entities by kind
    pub fn get_by_kind(&self, kind: &str) -> Vec<UnifiedEntity> {
        let mut results = Vec::new();

        // Growing segment
        if let Some(growing_arc) = self.growing.read().as_ref() {
            let growing = growing_arc.read();
            for entity in growing.iter_kind(kind) {
                results.push(entity.clone());
            }
        }

        // Sealed segments
        let sealed = self.sealed.read();
        for segment in sealed.iter() {
            for entity in segment.iter_kind(kind) {
                results.push(entity.clone());
            }
        }

        results
    }

    /// Count entities
    pub fn count(&self) -> usize {
        self.total_entities_atomic.load(Ordering::Relaxed) as usize
    }

    /// Get all segment IDs
    pub fn segment_ids(&self) -> Vec<SegmentId> {
        let mut ids = Vec::new();

        if let Some(growing_arc) = self.growing.read().as_ref() {
            ids.push(growing_arc.read().id());
        }

        let sealed = self.sealed.read();
        for segment in sealed.iter() {
            ids.push(segment.id());
        }

        ids.extend(self.archived.read().iter().copied());

        ids
    }

    /// Emit a lifecycle event.
    ///
    /// Perf: this used to push onto a `RwLock<Vec<LifecycleEvent>>`
    /// on every insert / delete / seal. Nobody consumes that vec
    /// today (no subscription API, `drain_events` has no callers),
    /// so the write lock + push was pure tax — and the vec grew
    /// unbounded in long-running processes.
    ///
    /// Current behaviour: no-op. If we ever want the hooks back,
    /// replace this with a bounded channel or an actual subscriber
    /// registry; the callers (`insert`, `delete`, `maybe_seal_growing`)
    /// already pass well-typed events.
    #[inline]
    #[allow(clippy::unused_self)]
    fn emit(&self, _event: LifecycleEvent) {}

    /// Drain events. Kept for API compatibility; always returns
    /// empty because `emit` no longer buffers.
    pub fn drain_events(&self) -> Vec<LifecycleEvent> {
        std::mem::take(&mut *self.events.write())
    }

    /// Run maintenance (would be called periodically in production)
    pub fn run_maintenance(&self) -> Result<(), SegmentError> {
        // Auto-seal idle segments
        self.maybe_seal_growing()?;

        // Compact if too many sealed segments
        if self.config.enable_compaction {
            let sealed_count = self.sealed.read().len();
            if sealed_count > self.config.max_sealed_segments {
                // In production, we'd trigger background compaction here
                // For now, just log that compaction is needed
            }
        }

        Ok(())
    }
}

// Implement the Arc<RwLock<GrowingSegment>> as UnifiedSegment
// This is needed because we store growing segments in the sealed list after sealing
impl UnifiedSegment for Arc<RwLock<GrowingSegment>> {
    fn id(&self) -> SegmentId {
        self.read().id()
    }

    fn state(&self) -> SegmentState {
        self.read().state()
    }

    fn collection(&self) -> &str {
        // This is a limitation - we'd need to store collection in the Arc wrapper
        "unknown"
    }

    fn stats(&self) -> SegmentStats {
        self.read().stats()
    }

    fn entity_count(&self) -> usize {
        self.read().entity_count()
    }

    fn contains(&self, id: EntityId) -> bool {
        self.read().contains(id)
    }

    fn get(&self, id: EntityId) -> Option<&UnifiedEntity> {
        // This is tricky with RwLock - we can't return a reference
        // In production, we'd use a different approach
        None
    }

    fn get_mut(&mut self, _id: EntityId) -> Option<&mut UnifiedEntity> {
        None
    }

    fn insert(&mut self, entity: UnifiedEntity) -> Result<EntityId, SegmentError> {
        self.write().insert(entity)
    }

    fn update(&mut self, entity: UnifiedEntity) -> Result<(), SegmentError> {
        self.write().update(entity)
    }

    fn update_hot(
        &mut self,
        entity: UnifiedEntity,
        modified_columns: &[String],
    ) -> Result<(), SegmentError> {
        self.write().update_hot(entity, modified_columns)
    }

    fn delete(&mut self, id: EntityId) -> Result<bool, SegmentError> {
        self.write().delete(id)
    }

    fn get_metadata(&self, id: EntityId) -> Option<Metadata> {
        self.read().get_metadata(id)
    }

    fn set_metadata(&mut self, id: EntityId, metadata: Metadata) -> Result<(), SegmentError> {
        self.write().set_metadata(id, metadata)
    }

    fn seal(&mut self) -> Result<(), SegmentError> {
        self.write().seal()
    }

    fn should_seal(&self, config: &SegmentConfig) -> bool {
        self.read().should_seal(config)
    }

    fn iter(&self) -> Box<dyn Iterator<Item = &UnifiedEntity> + '_> {
        // Cannot return iterator with RwLock
        Box::new(std::iter::empty())
    }

    fn iter_kind(&self, _kind_filter: &str) -> Box<dyn Iterator<Item = &UnifiedEntity> + '_> {
        Box::new(std::iter::empty())
    }

    fn filter_metadata(&self, filters: &[(String, MetadataFilter)]) -> Vec<EntityId> {
        self.read().filter_metadata(filters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Value;

    #[test]
    fn test_manager_basic() {
        let manager = SegmentManager::new("test_collection");

        let entity = UnifiedEntity::table_row(
            manager.next_entity_id(),
            "users",
            1,
            vec![Value::text("Alice".to_string())],
        );

        let id = manager.insert(entity).unwrap();
        assert!(manager.get(id).is_some());
        assert_eq!(manager.count(), 1);
    }

    #[test]
    fn test_manager_auto_seal() {
        let config = ManagerConfig {
            segment_config: SegmentConfig {
                max_entities: 2,
                ..Default::default()
            },
            ..Default::default()
        };

        let manager = SegmentManager::with_config("test", config);

        // Insert first entity
        manager
            .insert(UnifiedEntity::vector(
                manager.next_entity_id(),
                "v",
                vec![0.1],
            ))
            .unwrap();

        // Insert second entity (triggers seal check)
        manager
            .insert(UnifiedEntity::vector(
                manager.next_entity_id(),
                "v",
                vec![0.2],
            ))
            .unwrap();

        // Insert third entity (should trigger auto-seal of first segment)
        manager
            .insert(UnifiedEntity::vector(
                manager.next_entity_id(),
                "v",
                vec![0.3],
            ))
            .unwrap();

        let stats = manager.stats();
        assert_eq!(stats.total_entities, 3);
    }

    #[test]
    fn test_manager_delete() {
        let manager = SegmentManager::new("test");

        let id = manager
            .insert(UnifiedEntity::vector(
                manager.next_entity_id(),
                "v",
                vec![0.1],
            ))
            .unwrap();

        assert!(manager.get(id).is_some());
        assert!(manager.delete(id).unwrap());
        assert!(manager.get(id).is_none());
    }

    #[test]
    fn test_manager_metadata() {
        let manager = SegmentManager::new("test");

        let id = manager
            .insert(UnifiedEntity::table_row(
                manager.next_entity_id(),
                "hosts",
                1,
                vec![Value::text("192.168.1.1".to_string())],
            ))
            .unwrap();

        let mut meta = Metadata::new();
        meta.set(
            "os",
            super::super::metadata::MetadataValue::String("linux".to_string()),
        );

        manager.set_metadata(id, meta).unwrap();

        let retrieved = manager.get_metadata(id).unwrap();
        assert!(retrieved.has("os"));
    }

    #[test]
    fn test_manager_query_by_kind() {
        let manager = SegmentManager::new("test");

        manager
            .insert(UnifiedEntity::table_row(
                manager.next_entity_id(),
                "hosts",
                1,
                vec![],
            ))
            .unwrap();

        manager
            .insert(UnifiedEntity::vector(
                manager.next_entity_id(),
                "embeddings",
                vec![0.1],
            ))
            .unwrap();

        manager
            .insert(UnifiedEntity::table_row(
                manager.next_entity_id(),
                "hosts",
                2,
                vec![],
            ))
            .unwrap();

        let rows = manager.get_by_kind("table");
        assert_eq!(rows.len(), 2);

        let vectors = manager.get_by_kind("vector");
        assert_eq!(vectors.len(), 1);
    }

    #[test]
    #[ignore = "lifecycle events intentionally no-op since the emit-channel refactor; drain_events returns empty — see SegmentManager::emit"]
    fn test_lifecycle_events() {
        let manager = SegmentManager::new("test");

        manager
            .insert(UnifiedEntity::vector(
                manager.next_entity_id(),
                "v",
                vec![0.1],
            ))
            .unwrap();

        let events = manager.drain_events();

        // Should have: SegmentCreated, EntityInserted
        assert!(events
            .iter()
            .any(|e| matches!(e, LifecycleEvent::SegmentCreated(_))));
        assert!(events
            .iter()
            .any(|e| matches!(e, LifecycleEvent::EntityInserted(_, _))));
    }
}
