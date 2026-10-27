//! Segment Manager
//!
//! Manages the lifecycle of unified segments: creation, sealing, consolidation,
//! and archival. Coordinates writes to growing segments and queries across
//! all segments.
//!
//! # Responsibilities
//!
//! - Route writes to the active growing segment
//! - Auto-seal segments when thresholds are met
//! - Coordinate queries across multiple segments
//! - Paced consolidation of sealed segments (ADR 0073 §5)
//! - Archive old segments to cold storage

#![allow(clippy::too_many_arguments)] // Relocated native host entrypoints.

mod graph_index_merge;

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use super::entity::{EntityId, EntityKind, UnifiedEntity};
use super::metadata::{Metadata, MetadataFilter};
use super::segment::{
    GraphEntityKind, GrowingSegment, SegmentConfig, SegmentError, SegmentId, SegmentScanCursor,
    SegmentState, UnifiedSegment, ZoneColPred,
};
use super::visibility_map::VisibilityMap;
use crate::storage::visibility::entity_visible_with_context;
use crate::transaction::read::SnapshotContext;

/// Fraction of a collection's sealed entities that must be tombstoned before
/// consolidation is worth its cost.
///
/// Tunable implementation constant, not a contract: it is surfaced read-only in
/// `red.stats` so an operator can see what the engine decided, and it may move
/// between releases without notice.
pub const CONSOLIDATION_TOMBSTONE_RATIO: f64 = 0.20;

/// Fraction of a collection's sealed bytes that consolidation would return to
/// the budget (dead entity payloads plus the tombstone sets that bury them)
/// before consolidation is worth its cost. See [`CONSOLIDATION_TOMBSTONE_RATIO`]
/// on tunability.
pub const CONSOLIDATION_FRAGMENTATION_RATIO: f64 = 0.30;

/// Entities copied into the merged segment per maintenance tick.
///
/// ADR 0038 §3: consolidation is paced. A tick copies at most this many
/// entities and returns; the half-built merged segment is carried to the next
/// tick. No consolidation ever runs to completion in one unbounded pass.
pub const CONSOLIDATION_ENTITIES_PER_TICK: usize = 4_096;

/// Configuration for the segment manager
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Segment configuration
    pub segment_config: SegmentConfig,
    /// Maximum number of sealed segments before consolidation
    pub max_sealed_segments: usize,
    /// Idle time (seconds) before auto-sealing
    pub idle_seal_secs: u64,
    /// Enable paced sealed-segment consolidation
    pub enable_consolidation: bool,
    /// Entities copied per maintenance tick while a consolidation is running
    pub consolidation_entities_per_tick: usize,
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
            enable_consolidation: true,
            consolidation_entities_per_tick: CONSOLIDATION_ENTITIES_PER_TICK,
            enable_archival: true,
            archive_age_secs: 86400 * 7, // 7 days
        }
    }
}

/// Consolidation counters for one collection (ADR 0073 §5).
///
/// These replace the `compact_ops` counter, which only ever counted a
/// do-nothing branch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConsolidationStats {
    /// Consolidations that crossed a threshold and began copying.
    pub runs_started: u64,
    /// Consolidations that finished the swap.
    pub runs_completed: u64,
    /// Source segments retired by a completed swap.
    pub segments_merged: u64,
    /// Tombstones garbage-collected by completed swaps.
    pub tombstones_reclaimed: u64,
    /// Logical source-to-merged footprint reduction; readers may retain sources.
    pub bytes_reclaimed: u64,
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
    /// Consolidation counters
    pub consolidation: ConsolidationStats,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentScanStats {
    pub segments_total: u64,
    pub segments_scanned: u64,
    pub segments_pruned: u64,
}

/// Lifecycle events for monitoring
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    SegmentCreated(SegmentId),
    SegmentSealed(SegmentId),
    SegmentConsolidated {
        source: Vec<SegmentId>,
        target: SegmentId,
    },
    SegmentArchived(SegmentId),
    EntityInserted(EntityId, SegmentId),
    EntityDeleted(EntityId, SegmentId),
}

/// A consolidation in flight, carried across maintenance ticks.
///
/// The merged segment is built off to the side and is invisible to readers
/// until the swap. Nothing here is durable: sealed segments are the truth (via
/// the normal checkpoint path), so a crash mid-consolidation simply drops this
/// state and the next boot starts over.
struct Consolidation {
    /// Source segments, frozen when the run started.
    sources: Vec<SourceProgress>,
    /// Id reserved for the merged segment.
    merged_id: SegmentId,
    /// The half-built merged segment.
    merged: GrowingSegment,
    /// Index into `sources` of the segment currently being copied.
    cursor: usize,
    /// Live ids of `sources[cursor]`, snapshotted when that source was opened.
    pending_ids: Vec<EntityId>,
    /// How many of `pending_ids` have been consumed.
    pending_cursor: usize,
    /// Entities copied so far, across every tick of this run.
    entities_copied: u64,
}

/// Per-source bookkeeping for a consolidation in flight.
struct SourceProgress {
    id: SegmentId,
    /// Ids copied out of this source into the merged segment.
    copied: Vec<EntityId>,
    /// The source's tombstone count when the run opened it, and `None` before
    /// that. Tombstones only ever grow, so a source whose count is unchanged at
    /// swap time saw no deletes after we started reading it — and every row we
    /// copied out of it is still live. Anything else needs a per-row check.
    ///
    /// Recording this when the source is *opened*, not when it is drained, is
    /// what makes the check sound: a row deleted between two copy ticks would
    /// otherwise be invisible to the swap and get resurrected.
    tombstones_at_open: Option<usize>,
}

struct PhysicalRootCacheEntry {
    segments: Vec<(u64, u64)>,
    seed: u64,
    root: u64,
}

/// Resumable physical maintenance positions. Weak sources let consolidation
/// release retired payloads; a later sweep revisits any replacement segments.
#[doc(hidden)]
pub struct PhysicalScanCursor {
    sources: Vec<(Weak<RwLock<GrowingSegment>>, SegmentScanCursor)>,
    source: usize,
}

impl PhysicalScanCursor {
    #[doc(hidden)]
    pub fn finished(&self) -> bool {
        self.source == self.sources.len()
    }
}

/// Segment manager for a collection
pub struct SegmentManager {
    /// Statement-scoped constraint admission. Reentrant because SQL upserts
    /// invoke the same typed mutation APIs while holding this collection gate.
    row_constraint_lock: Arc<parking_lot::ReentrantMutex<()>>,
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
    /// Currently active growing segment. Readers retain this topology gate across
    /// growing/sealed probes; sealing and relocation publish under its write gate.
    growing: RwLock<Option<Arc<RwLock<GrowingSegment>>>>,
    /// Sealed segments (immutable, queryable)
    sealed: RwLock<Vec<Arc<RwLock<GrowingSegment>>>>,
    /// Retired sources still owned by readers. Weak handles never prolong payload lifetime.
    retired: RwLock<Vec<Weak<RwLock<GrowingSegment>>>>,
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
    /// Consolidation in flight, if any. Only `run_maintenance` touches it, so
    /// at most one consolidation runs per collection at a time.
    consolidation: RwLock<Option<Consolidation>>,
    /// Event listeners (simplified - would be channels in production)
    events: RwLock<Vec<LifecycleEvent>>,
    /// Visibility map: sealed segment entity ranges marked as all-visible.
    /// Growing segment is never all-visible (writes are in-flight).
    /// Used by index-only scan decisions.
    visibility_map: VisibilityMap,
    physical_root_cache: parking_lot::Mutex<Option<PhysicalRootCacheEntry>>,
}

impl SegmentManager {
    /// Create a new segment manager
    pub fn new(collection: impl Into<String>) -> Self {
        Self::with_config(collection, ManagerConfig::default())
    }

    /// Create with custom configuration
    pub fn with_config(collection: impl Into<String>, config: ManagerConfig) -> Self {
        Self {
            row_constraint_lock: Arc::new(parking_lot::ReentrantMutex::new(())),
            physical_root_cache: parking_lot::Mutex::new(None),
            collection: collection.into(),
            config,
            next_segment_id: AtomicU64::new(1),
            next_entity_id: AtomicU64::new(1),
            next_row_id: AtomicU64::new(1),
            total_entities_atomic: AtomicU64::new(0),
            growing: RwLock::new(None),
            sealed: RwLock::new(Vec::new()),
            retired: RwLock::new(Vec::new()),
            archived: RwLock::new(Vec::new()),
            entity_segment: RwLock::new(HashMap::new()),
            column_schema: RwLock::new(None),
            stats: RwLock::new(ManagerStats::default()),
            consolidation: RwLock::new(None),
            events: RwLock::new(Vec::new()),
            visibility_map: VisibilityMap::new(),
        }
    }

    #[doc(hidden)]
    pub fn row_constraint_lock(&self) -> Arc<parking_lot::ReentrantMutex<()>> {
        Arc::clone(&self.row_constraint_lock)
    }

    /// Get or create the shared column schema from first row's named fields.
    pub fn get_or_init_schema(
        &self,
        named: &HashMap<String, reddb_types::Value>,
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

    // Positional file records omit the shared column names. Restore them on
    // owned reads before callers update fields or rebuild secondary indexes.
    // Existing per-row names remain authoritative; segment contents stay raw.
    fn restore_owned_row_schemas<'a>(&self, entities: impl Iterator<Item = &'a mut UnifiedEntity>) {
        let schema = self.column_schema();
        for entity in entities {
            if let super::entity::EntityData::Row(row) = &mut entity.data {
                if row.named.is_none() && row.schema.is_none() {
                    row.schema = schema.clone();
                }
            }
        }
    }

    #[doc(hidden)]
    pub fn set_column_schema_if_empty(&self, columns: Vec<String>) {
        if columns.is_empty() {
            return;
        }
        let mut schema = self.column_schema.write();
        if schema.is_none() {
            *schema = Some(Arc::new(columns));
        }
    }

    /// Get collection name
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// Get configuration
    pub fn config(&self) -> &ManagerConfig {
        &self.config
    }

    /// Get statistics. Entity counts cover active segments; memory also covers
    /// retained sources and unpublished consolidation work.
    pub fn stats(&self) -> ManagerStats {
        let mut s = self.stats.read().clone();
        s.total_entities = self.total_entities_atomic.load(Ordering::Relaxed) as usize;
        s.total_memory_bytes = self.resident_bytes() as usize;
        s
    }

    /// Approximate resident segment memory, including tombstones, retired sources,
    /// and the in-flight merge and its bookkeeping. This feeds budget admission.
    pub fn resident_bytes(&self) -> u64 {
        self.memory_usage().1
    }

    /// Resident segment payload/directory bytes and consolidation bookkeeping,
    /// including retired sources. Unlike `resident_bytes`, excludes tombstones.
    pub fn memory_bytes(&self) -> u64 {
        self.memory_usage().0
    }

    /// Sample payload and resident bytes under the publication lock order:
    /// consolidation, growing, sealed, retired. No entity scan or strong handle
    /// is retained beyond this call; expired weak entries are pruned in place.
    fn memory_usage(&self) -> (u64, u64) {
        let consolidation = self.consolidation.read();
        let growing = self.growing.read();
        let sealed = self.sealed.read();
        let mut retired = self.retired.write();
        let mut payload = 0u64;
        let mut resident = 0u64;
        {
            let mut add = |segment: &GrowingSegment| {
                payload = payload.saturating_add(segment.memory_bytes());
                resident = resident.saturating_add(segment.resident_bytes());
            };
            for segment in growing.iter().chain(sealed.iter()) {
                add(&segment.read());
            }
            retired.retain(|source| {
                if let Some(source) = source.upgrade() {
                    add(&source.read());
                    true
                } else {
                    false
                }
            });
            if let Some(run) = consolidation.as_ref() {
                add(&run.merged);
            }
        }
        let mut bookkeeping =
            (retired.capacity() * std::mem::size_of::<Weak<RwLock<GrowingSegment>>>()) as u64;
        if let Some(run) = consolidation.as_ref() {
            bookkeeping = bookkeeping
                .saturating_add(
                    (run.sources.capacity() * std::mem::size_of::<SourceProgress>()) as u64,
                )
                .saturating_add(
                    (run.pending_ids.capacity() * std::mem::size_of::<EntityId>()) as u64,
                );
            for source in &run.sources {
                bookkeeping = bookkeeping.saturating_add(
                    (source.copied.capacity() * std::mem::size_of::<EntityId>()) as u64,
                );
            }
        }
        (
            payload.saturating_add(bookkeeping),
            resident.saturating_add(bookkeeping),
        )
    }

    /// Bytes a pressure-triggered consolidation could plausibly return.
    pub fn reclaimable_bytes(&self) -> u64 {
        self.sealed
            .read()
            .iter()
            .map(|segment| segment.read().reclaimable_bytes())
            .fold(0, u64::saturating_add)
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

    // A tombstone belongs to its segment for the lifetime of captured cursors.
    // Restoring that physical ID must use a successor segment, retaining both
    // the original tombstone ordinal and the existing scan-position boundary.
    fn with_restoration_segment<T>(
        &self,
        id: EntityId,
        restore: impl FnOnce(&mut GrowingSegment) -> Result<T, SegmentError>,
    ) -> Result<T, SegmentError> {
        loop {
            let segment = self.get_or_create_growing();
            let mut growing = segment.write();
            if !growing.state().is_writable() {
                continue;
            }
            if !growing.is_tombstoned(id) {
                return restore(&mut growing);
            }
            drop(growing);
            self.force_seal()?;
        }
    }

    /// Restore a snapshot image without allocating a new sequence identity.
    #[doc(hidden)]
    pub fn restore_snapshot_entity(&self, entity: UnifiedEntity) -> Result<EntityId, SegmentError> {
        self.maybe_seal_growing()?;
        if self.get(entity.id).is_some() {
            return Err(SegmentError::AlreadyExists(entity.id));
        }
        let entity_id = entity.id;
        self.with_restoration_segment(entity_id, |segment| {
            let segment_id = segment.id();
            // Adoption retains sequence identity and advances its allocator.
            segment.adopt_entity(entity, None);
            self.total_entities_atomic.fetch_add(1, Ordering::Relaxed);
            self.emit(LifecycleEvent::EntityInserted(entity_id, segment_id));
            Ok(entity_id)
        })
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
    /// Skips bloom filter and cross-ref indexing for maximum speed.
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
                                            .unwrap_or(reddb_types::Value::Null),
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
        // Single call to GrowingSegment.bulk_insert (one lock, no bloom/cross-refs)
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
        let mut entity = self.get_with(id, Clone::clone)?;
        self.restore_owned_row_schemas(std::iter::once(&mut entity));
        Some(entity)
    }

    /// Inspect under the segment lock, for example to bound a payload clone.
    /// The callback must not re-enter storage or runtime memory admission.
    #[doc(hidden)]
    pub fn get_with<T>(&self, id: EntityId, visit: impl FnOnce(&UnifiedEntity) -> T) -> Option<T> {
        // Growing segment first (most likely for recent inserts)
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            let growing = growing_arc.read();
            if let Some(entity) = growing.get(id) {
                return Some(visit(entity));
            }
        }

        // Then sealed segments
        let sealed = self.sealed.read();
        for segment in sealed.iter() {
            let seg = segment.read();
            if let Some(entity) = seg.get(id) {
                return Some(visit(entity));
            }
        }

        None
    }

    /// Resolve a physical graph node reference without cloning historical payloads.
    /// This deliberately ignores MVCC: callers must authorize the visible logical
    /// node separately. The callback bounds every segment probe, including misses.
    #[doc(hidden)]
    pub fn graph_node_logical_id(
        &self,
        id: EntityId,
        mut before_segment: impl FnMut() -> bool,
    ) -> Option<EntityId> {
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            if !before_segment() {
                return None;
            }
            let growing = growing_arc.read();
            if let Some(entity) = growing.get(id) {
                return matches!(entity.kind, EntityKind::GraphNode(_))
                    .then(|| entity.logical_id());
            }
        }
        let sealed = self.sealed.read();
        for segment in sealed.iter() {
            if !before_segment() {
                return None;
            }
            let segment = segment.read();
            if let Some(entity) = segment.get(id) {
                return matches!(entity.kind, EntityKind::GraphNode(_))
                    .then(|| entity.logical_id());
            }
        }
        None
    }

    /// Pin the source inventory across sealing/consolidation. Consumers hydrate
    /// IDs through the current manager and run outside all storage locks.
    /// Admission is also outside storage locks; a growing inventory retries only
    /// its small handle array. Each indexed candidate buffer has a fixed size.
    #[doc(hidden)]
    pub fn visit_graph_index_batches<E>(
        &self,
        kind: GraphEntityKind,
        keys: &[EntityId],
        mut before_work: impl FnMut() -> bool,
        mut admit: impl FnMut(usize) -> Result<(), E>,
        mut consume: impl FnMut(&[EntityId]) -> Result<bool, E>,
    ) -> Result<bool, E> {
        use super::segment::{GraphIndexCursor, SCAN_BATCH_SIZE};
        admit(std::mem::size_of::<[EntityId; SCAN_BATCH_SIZE]>())?;
        let mut admitted = 0;
        let sources = loop {
            let count = {
                let growing = self.growing.read();
                let sealed = self.sealed.read();
                sealed.len() + usize::from(growing.is_some())
            };
            let bytes = count.saturating_mul(std::mem::size_of::<(
                Arc<RwLock<GrowingSegment>>,
                GraphIndexCursor,
            )>());
            admit(bytes.saturating_sub(admitted))?;
            admitted = admitted.max(bytes);
            let mut sources = Vec::with_capacity(count);
            let growing = self.growing.read();
            let sealed = self.sealed.read();
            if sealed.len() + usize::from(growing.is_some()) > sources.capacity() {
                continue;
            }
            for segment in growing.iter().chain(sealed.iter()) {
                if !before_work() {
                    return Ok(false);
                }
                let view = segment.read();
                if !view.may_contain_graph_kind(kind) {
                    continue;
                }
                let Some(cursor) = view.graph_index_cursor(kind, keys, &mut before_work) else {
                    return Ok(false);
                };
                sources.push((Arc::clone(segment), cursor));
            }
            break sources;
        };
        let mut ids = [EntityId::new(0); SCAN_BATCH_SIZE];
        for (source, mut cursor) in sources {
            while !cursor.finished(keys) {
                let mut count = 0;
                {
                    let segment = source.read();
                    if !segment.may_contain_graph_kind(kind) {
                        break;
                    }
                    if !segment.graph_index_batch(
                        kind,
                        keys,
                        &mut cursor,
                        &mut before_work,
                        &mut |id| {
                            ids[count] = id;
                            count += 1;
                        },
                    ) {
                        return Ok(false);
                    }
                }
                if !before_work() || !consume(&ids[..count])? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Batch-fetch multiple entities by ID in a single lock acquisition per segment.
    ///
    /// For indexed-scan result sets (up to ~5000 ids from range/bitmap lookup) this
    /// is 2-3 lock acquisitions total vs N×3 with individual `get()` calls.
    pub fn get_many(&self, ids: &[EntityId]) -> Vec<Option<UnifiedEntity>> {
        let schema = self.column_schema();
        let clone_row = |entity: &UnifiedEntity| {
            let mut owned = entity.clone();
            if let super::entity::EntityData::Row(row) = &mut owned.data {
                if row.schema.is_none() {
                    row.schema = schema.clone();
                }
            }
            owned
        };
        let mut out: Vec<Option<UnifiedEntity>> = vec![None; ids.len()];
        let mut remaining: Vec<usize> = (0..ids.len()).collect(); // indices still unfound

        // Growing segment — one read lock for the entire batch.
        // Non-blocking first: if a writer is active, fall back to blocking.
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            remaining.retain(|&i| {
                if let Some(entity) = growing.get(ids[i]) {
                    out[i] = Some(clone_row(entity));
                    false // remove from remaining
                } else {
                    true // keep — not found yet
                }
            });
        }

        if remaining.is_empty() {
            self.restore_owned_row_schemas(out.iter_mut().flatten());
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
                    out[i] = Some(clone_row(entity));
                    false
                } else {
                    true
                }
            });
        }

        drop(sealed);
        self.restore_owned_row_schemas(out.iter_mut().flatten());
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

        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
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
    #[allow(dead_code)] // Retain the existing native helper during extraction.
    fn scan_for_entity(&self, id: EntityId) -> Option<UnifiedEntity> {
        // Check growing
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
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
        let mut entity = Some(entity);
        loop {
            self.get_or_create_growing();
            // Publish the move under the same topology gate that readers keep
            // across growing and sealed. Never expose a removed source without
            // its replacement. Cost is one row, without cloning a collection.
            let growing_slot = self.growing.write();
            let Some(growing_arc) = growing_slot.as_ref() else {
                continue;
            };
            let mut growing = growing_arc.write();
            if growing.is_tombstoned(entity_id) {
                drop(growing);
                drop(growing_slot);
                self.force_seal()?;
                continue;
            }
            if growing.contains(entity_id) {
                // Another update may have moved it after the caller's probe.
                growing.update(entity.take().expect("unpublished relocation image"))?;
                if let Some(metadata) = metadata {
                    growing.set_metadata(entity_id, metadata.clone())?;
                }
                return Ok(());
            }
            // Pin membership through publication so consolidation cannot replace
            // the source between lookup and removal.
            let sealed_slots = self.sealed.read();
            let source = sealed_slots
                .iter()
                .find(|segment| segment.read().contains(entity_id))
                .ok_or(SegmentError::NotFound(entity_id))?;
            let mut source = source.write();
            if !source.contains(entity_id) {
                return Err(SegmentError::NotFound(entity_id));
            }
            let metadata = metadata.cloned().or_else(|| source.get_metadata(entity_id));
            growing
                .insert_preserving_sequence(entity.take().expect("unpublished relocation image"))?;
            if let Some(metadata) = metadata {
                growing
                    .set_metadata(entity_id, metadata)
                    .expect("inserted relocation image is writable and present");
            }
            assert!(
                source.force_delete(entity_id),
                "locked relocation source is present"
            );
            let growing_id = growing.id();
            drop(source);
            drop(sealed_slots);
            drop(growing);
            self.entity_segment.write().insert(entity_id, growing_id);
            return Ok(());
        }
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
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            let growing = growing_arc.read();
            if growing.contains(id) {
                return growing.get_metadata(id);
            }
        }

        // Fallback: entity_segment map (for pre-existing or sealed entities)
        let segment_id = self.entity_segment.read().get(&id).copied();

        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = growing_slot.as_ref() {
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
        // Keep topology publication atomic for cursors that capture growing
        // and sealed under the same lock order: growing, then sealed.
        let mut growing_slot = self.growing.write();
        if let Some(growing_arc) = growing_slot.take() {
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
            drop(growing_slot);

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
            let pages = entity_count.div_ceil(ROWS_PER_PAGE as u64);
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
        let new_pages = seg_entity_count.div_ceil(ROWS_PER_PAGE as u64);
        let end_page = start_page + new_pages as u32;
        self.visibility_map.mark_range_visible(start_page, end_page);
    }

    /// Probe a unique-key lookup in each segment under the same entity
    /// locks used by scans and writes. Lookup state is rebuilt lazily after
    /// recovery, consolidation, or a key-changing mutation.
    #[doc(hidden)]
    pub fn find_unique_key_conflict(
        &self,
        columns: &[Vec<String>],
        key_index: usize,
        signatures: &[String],
        exclude_id: Option<EntityId>,
        reserves_key: &impl Fn(&UnifiedEntity) -> bool,
    ) -> Option<EntityId> {
        let growing_slot = self.growing.read();
        if let Some(growing) = growing_slot.as_ref() {
            if let Some(id) = growing.read().find_unique_key_conflict(
                columns,
                key_index,
                signatures,
                exclude_id,
                reserves_key,
            ) {
                return Some(id);
            }
        }
        for segment in self.sealed.read().iter() {
            if let Some(id) = segment.read().find_unique_key_conflict(
                columns,
                key_index,
                signatures,
                exclude_id,
                reserves_key,
            ) {
                return Some(id);
            }
        }
        None
    }

    /// Iterate over all entities in-place without collecting into a Vec.
    ///
    /// The callback receives a reference to each entity. Return `true` to
    /// continue iteration, `false` to stop early (e.g. when a LIMIT is reached).
    /// This avoids the allocation and cloning overhead of `query_all`.
    pub fn for_each_entity<F>(&self, callback: F)
    where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        self.for_each_entity_in_segments(|_| true, || true, callback);
    }

    /// Segment identities and mutation counters, sampled without scanning rows.
    #[doc(hidden)]
    pub fn checkpoint_revisions(&self) -> Vec<(u64, u64)> {
        let growing = self.growing.read();
        let sealed = self.sealed.read();
        growing
            .iter()
            .chain(sealed.iter())
            .map(|segment| {
                let segment = segment.read();
                (segment.id(), segment.checkpoint_revision())
            })
            .collect()
    }

    #[doc(hidden)]
    pub fn checkpoint_mutations_since(&self, before: &[(u64, u64)]) -> Option<Vec<EntityId>> {
        let growing = self.growing.read();
        let sealed = self.sealed.read();
        // Sealing moves the same segment from growing to sealed, and a later
        // write creates a new growing segment. Neither operation invalidates
        // the old segments' mutation journals. Match identities rather than
        // positions so these ordinary lifecycle changes retain sparse proofs.
        let previous: HashMap<_, _> = before.iter().copied().collect();
        if previous.len() != before.len() {
            return None;
        }
        let mut retained_sources = 0;
        let mut changed = std::collections::BTreeSet::new();
        for segment in growing.iter().chain(sealed.iter()) {
            let segment = segment.read();
            let revision = previous.get(&segment.id()).copied();
            retained_sources += usize::from(revision.is_some());
            // A new segment must account for its entire history from revision
            // zero. Missing or exhausted journals still reject the fast path.
            let revision = revision.unwrap_or(0);
            changed.extend(segment.checkpoint_mutations_since(revision)?);
            if changed.len() > super::segment::CHECKPOINT_MUTATIONS_MAX {
                return None;
            }
        }
        // Consolidation or removal can hide changes from an old source. Keep
        // conservative full validation whenever a previous identity is absent.
        (retained_sources == previous.len()).then(|| changed.into_iter().collect())
    }

    #[doc(hidden)]
    pub fn checkpoint_tracking_bytes(&self) -> usize {
        let growing = self.growing.read();
        let sealed = self.sealed.read();
        growing
            .iter()
            .chain(sealed.iter())
            .map(|segment| segment.read().checkpoint_tracking_bytes())
            .sum()
    }

    /// Reuse the derived metadata fingerprint only while its rows and stats match.
    #[doc(hidden)]
    pub fn cached_physical_root(&self, seed: u64, compute: impl FnOnce() -> u64) -> u64 {
        let segments = self.checkpoint_revisions();
        let mut cache = self.physical_root_cache.lock();
        if let Some(previous) = cache.as_ref() {
            if previous.seed == seed && previous.segments == segments {
                return previous.root;
            }
        }
        let root = compute();
        *cache = Some(PhysicalRootCacheEntry {
            segments,
            seed,
            root,
        });
        root
    }

    /// Visit rows in the same stable ID order as sorting `query_all`, without
    /// cloning their payloads. The caller fences mutations while fingerprinting.
    /// Pin source segments, but hold only one segment guard during each visit.
    /// Callbacks must not re-enter storage while that guard is held.
    #[doc(hidden)]
    pub fn for_each_entity_in_id_order(&self, mut visit: impl FnMut(&UnifiedEntity) -> bool) {
        self.for_each_entity_source_in_id_order(|_, entity| visit(entity));
    }

    /// Keep the physical scan's global ID order while cloning only its page.
    #[doc(hidden)]
    pub fn page_in_id_order(&self, offset: usize, limit: usize) -> (usize, Vec<UnifiedEntity>) {
        let end = offset.saturating_add(limit);
        let mut total = 0;
        let mut items = Vec::with_capacity(limit.min(self.count()));
        self.for_each_entity_in_id_order(|entity| {
            if total >= offset && total < end {
                items.push(entity.clone());
            }
            total += 1;
            true
        });
        self.restore_owned_row_schemas(items.iter_mut());
        (total, items)
    }

    #[doc(hidden)]
    pub fn for_each_entity_with_metadata_in_id_order(
        &self,
        mut visit: impl FnMut(&UnifiedEntity, Option<Metadata>) -> bool,
    ) {
        self.for_each_entity_source_in_id_order(|segment, entity| {
            visit(entity, segment.get_metadata(entity.id))
        });
    }

    fn for_each_entity_source_in_id_order(
        &self,
        mut visit: impl FnMut(&GrowingSegment, &UnifiedEntity) -> bool,
    ) {
        let segments: Vec<_> = {
            let growing = self.growing.read();
            let sealed = self.sealed.read();
            growing.iter().chain(sealed.iter()).cloned().collect()
        };
        let mut order = Vec::new();
        for (source, segment) in segments.iter().enumerate() {
            segment.read().for_each_fast(|entity| {
                order.push((entity.id, source));
                true
            });
        }
        order.sort_by_key(|(id, _)| id.raw());
        for (id, source) in order {
            let segment = segments[source].read();
            if let Some(entity) = segment.get(id) {
                if !visit(&segment, entity) {
                    break;
                }
            }
        }
    }

    /// Visit borrowed rows with metadata under the same segment read guard.
    /// Callbacks must not re-enter this manager while the guard is held.
    #[doc(hidden)]
    pub fn for_each_entity_with_metadata(
        &self,
        mut callback: impl FnMut(&UnifiedEntity, Option<Metadata>) -> bool,
    ) {
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            let growing = growing_arc.read();
            if !growing.for_each_fast(|entity| callback(entity, growing.get_metadata(entity.id))) {
                return;
            }
        }
        let sealed = self.sealed.read();
        for segment_arc in sealed.iter() {
            let segment = segment_arc.read();
            if !segment.for_each_fast(|entity| callback(entity, segment.get_metadata(entity.id))) {
                return;
            }
        }
    }

    fn for_each_entity_in_segments<F>(
        &self,
        segment_filter: impl Fn(&GrowingSegment) -> bool,
        mut before_segment: impl FnMut() -> bool,
        mut callback: F,
    ) where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        // Growing segment — direct iteration (no Box<dyn>)
        // Try non-blocking read first; fall back to blocking only when a writer
        // is actively holding the write lock (rare in read-heavy workloads).
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            if !before_segment() {
                return;
            }
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            if segment_filter(&growing) && !growing.for_each_fast(&mut callback) {
                return;
            }
        }

        // Sealed segments
        let sealed = self.sealed.read();
        for segment_arc in sealed.iter() {
            if !before_segment() {
                return;
            }
            let segment = segment_arc.read();
            if segment_filter(&segment) && !segment.for_each_fast(&mut callback) {
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
        #[cfg(not(target_arch = "wasm32"))]
        use rayon::prelude::*;

        // Growing segment — always sequential (single writer lock,
        // usually small working set).
        let mut acc = init();
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
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

        #[cfg(not(target_arch = "wasm32"))]
        let segments = segments.into_par_iter();
        #[cfg(target_arch = "wasm32")]
        let segments = segments.into_iter();
        let partials = segments.map(|seg_arc| {
            let mut local = init();
            let seg = seg_arc.read();
            seg.for_each_fast(|entity| {
                local = fold(std::mem::replace(&mut local, init()), entity);
                true
            });
            local
        });
        #[cfg(not(target_arch = "wasm32"))]
        let sealed_acc = partials.reduce(&init, &reduce);
        #[cfg(target_arch = "wasm32")]
        let sealed_acc = partials.fold(init(), &reduce);

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
        let _ = self.for_each_entity_zoned_with_stats(zone_preds, &mut callback);
    }

    pub fn for_each_entity_zoned_with_stats<F>(
        &self,
        zone_preds: &[(&str, ZoneColPred<'_>)],
        mut callback: F,
    ) -> SegmentScanStats
    where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        let mut scan_stats = SegmentScanStats::default();
        // Growing segment — never skip (it's receiving writes, zones are partial).
        // Try a non-blocking read first: if a writer is currently inserting
        // (holding the write lock), try_read() returns None and we fall back to
        // the blocking read.  In low-contention workloads (reads far outnumber
        // writes) the try_read() almost always succeeds and readers never stall.
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            scan_stats.segments_total += 1;
            scan_stats.segments_scanned += 1;
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            if !growing.for_each_fast(&mut callback) {
                return scan_stats;
            }
        }

        // Sealed segments — check zone maps before iterating
        let sealed = self.sealed.read();
        for segment_arc in sealed.iter() {
            scan_stats.segments_total += 1;
            let segment = segment_arc.read();
            if !zone_preds.is_empty() && segment.can_skip_zone_preds(zone_preds) {
                scan_stats.segments_pruned += 1;
                continue; // entire segment pruned
            }
            scan_stats.segments_scanned += 1;
            if !segment.for_each_fast(&mut callback) {
                return scan_stats;
            }
        }
        scan_stats
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
        self.query_all_zoned_with_stats(zone_preds, filter).0
    }

    pub fn query_all_zoned_with_stats<F>(
        &self,
        zone_preds: &[(&str, ZoneColPred<'_>)],
        filter: F,
    ) -> (Vec<UnifiedEntity>, SegmentScanStats)
    where
        F: Fn(&UnifiedEntity) -> bool + Sync,
    {
        let mut results = Vec::new();
        let mut scan_stats = SegmentScanStats::default();

        // Growing segment — always scan, no zone skip (zones are partial).
        // Non-blocking try_read() avoids stalling behind in-progress inserts.
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            scan_stats.segments_total += 1;
            scan_stats.segments_scanned += 1;
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
                scan_stats.segments_total += 1;
                if zone_preds.is_empty() {
                    scan_stats.segments_scanned += 1;
                    return true;
                }
                let seg = seg_arc.read();
                if seg.can_skip_zone_preds(zone_preds) {
                    scan_stats.segments_pruned += 1;
                    false
                } else {
                    scan_stats.segments_scanned += 1;
                    true
                }
            })
            .collect();

        let use_parallel = surviving.len() > 1 && crate::execution::should_parallelize();

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

        (results, scan_stats)
    }

    /// Visibility predicate for the `scan_*` family. `Some` delegates to the
    /// canonical `entity_visible_with_context`; `None` supports low-level
    /// callers that intentionally scan without a statement snapshot and keeps
    /// the documented resolver fallback — moderation gate plus hiding
    /// superseded physical versions — instead of showing every version.
    fn scan_entity_visible(snapshot: Option<&SnapshotContext>, entity: &UnifiedEntity) -> bool {
        match snapshot {
            Some(ctx) => entity_visible_with_context(Some(ctx), entity),
            None => {
                !crate::storage::visibility::entity_moderation_hidden(entity) && entity.xmax == 0
            }
        }
    }

    /// Scan every segment under an explicit MVCC snapshot.
    ///
    /// Visibility is evaluated while each segment iterator is active, before
    /// the caller's filter. Unlike the raw iteration methods, this entry point
    /// does not consult thread-local snapshot state, so it preserves the same
    /// view when invoked from a different thread.
    pub fn scan<F>(&self, snapshot: Option<&SnapshotContext>, filter: F) -> Vec<UnifiedEntity>
    where
        F: Fn(&UnifiedEntity) -> bool + Sync,
    {
        self.query_all(|entity| Self::scan_entity_visible(snapshot, entity) && filter(entity))
    }

    /// Probe each segment's physical key index, retaining MVCC history candidates.
    /// Cost is O(segment count + versions of this key), rather than all collection rows.
    /// `project` runs under a segment read guard: keep it bounded and do not
    /// re-enter storage. Only the owned projection leaves the guard.
    #[doc(hidden)]
    pub fn project_key_entity<T>(
        &self,
        key: &str,
        snapshot: Option<&SnapshotContext>,
        versioned: bool,
        mut project: impl FnMut(&UnifiedEntity) -> T,
    ) -> Option<T> {
        let mut probe = |segment: &GrowingSegment, best: &mut Option<(u64, T)>| {
            if let Some(entity) = segment.find_key_entity(key, versioned, &mut |entity| {
                !versioned || Self::scan_entity_visible(snapshot, entity)
            }) {
                if best.as_ref().is_none_or(|(xmin, _)| entity.xmin >= *xmin) {
                    *best = Some((entity.xmin, project(entity)));
                }
            }
        };
        let mut best = None;
        let growing_slot = self.growing.read();
        if let Some(growing) = growing_slot.as_ref() {
            probe(&growing.read(), &mut best);
        }
        for segment in self.sealed.read().iter() {
            if !versioned && best.is_some() {
                break;
            }
            probe(&segment.read(), &mut best);
        }
        best.map(|(_, projected)| projected)
    }

    /// Collect visible graph items, pruning only under the segment read guard.
    #[doc(hidden)]
    pub fn scan_graph_kind(
        &self,
        snapshot: Option<&SnapshotContext>,
        kind: GraphEntityKind,
    ) -> Vec<UnifiedEntity> {
        self.query_all_in_segments(
            |segment| segment.may_contain_graph_kind(kind),
            |entity| Self::scan_entity_visible(snapshot, entity) && kind.matches(&entity.kind),
        )
    }

    /// Visit visible candidates in segments that may contain the requested kind.
    /// Non-graph items in those segments still reach the callback so work budgets
    /// charge every inspected visible candidate. RLS must run outside this callback.
    /// `before_segment` runs even for pruned segments; false stops the entire scan.
    #[doc(hidden)]
    pub fn scan_graph_candidates_for_each<F>(
        &self,
        snapshot: Option<&SnapshotContext>,
        kind: GraphEntityKind,
        before_segment: impl FnMut() -> bool,
        mut callback: F,
    ) where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        self.for_each_entity_in_segments(
            |segment| segment.may_contain_graph_kind(kind),
            before_segment,
            |entity| !Self::scan_entity_visible(snapshot, entity) || callback(entity),
        );
    }

    /// Visit every entity visible under an explicit MVCC snapshot.
    ///
    /// Returning `false` from `callback` stops the scan early. Hidden entities
    /// never reach the callback and therefore cannot stop the scan.
    pub fn scan_for_each<F>(&self, snapshot: Option<&SnapshotContext>, mut callback: F)
    where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        self.for_each_entity(|entity| {
            !Self::scan_entity_visible(snapshot, entity) || callback(entity)
        });
    }

    #[doc(hidden)]
    pub fn physical_scan_cursor(&self) -> PhysicalScanCursor {
        let growing = self.growing.read();
        let sealed = self.sealed.read();
        PhysicalScanCursor {
            sources: growing
                .iter()
                .chain(sealed.iter())
                .map(|segment| (Arc::downgrade(segment), segment.read().scan_cursor()))
                .collect(),
            source: 0,
        }
    }

    /// At most one small ID batch; every inspected slot or retired source
    /// consumes work, including deleted entries that produce no candidate.
    #[doc(hidden)]
    pub fn physical_scan_batch(
        &self,
        cursor: &mut PhysicalScanCursor,
        work_left: &mut usize,
    ) -> Vec<EntityId> {
        let mut ids = Vec::with_capacity(super::segment::SCAN_BATCH_SIZE.min(*work_left));
        while !cursor.finished() && *work_left > 0 {
            let (source, position) = &mut cursor.sources[cursor.source];
            if position.finished() {
                *work_left -= 1;
                cursor.source += 1;
                continue;
            }
            let Some(segment) = source.upgrade() else {
                *work_left -= 1;
                cursor.source += 1;
                continue;
            };
            segment.read().scan_batch(
                position,
                &mut || {
                    if *work_left == 0 {
                        return false;
                    }
                    *work_left -= 1;
                    true
                },
                &mut |id| ids.push(id),
            );
            if position.finished() {
                cursor.source += 1;
            }
            if !ids.is_empty() {
                break;
            }
        }
        ids
    }

    /// Capture segment membership and append boundaries, then hydrate bounded
    /// batches. The consumer runs outside topology and segment locks, so scoring
    /// and RLS may re-enter storage and writers can progress between batches.
    /// Retired sources pin positions only; hydration uses current segments so
    /// mutations after consolidation are visible to the snapshot predicate.
    #[doc(hidden)]
    pub fn scan_batches(
        &self,
        snapshot: Option<&SnapshotContext>,
        mut before_work: impl FnMut() -> bool,
        mut consume: impl FnMut(&[UnifiedEntity]) -> bool,
    ) -> bool {
        let segments = {
            let growing = self.growing.read();
            let sealed = self.sealed.read();
            let mut segments = Vec::with_capacity(sealed.len() + usize::from(growing.is_some()));
            for segment in growing.iter().chain(sealed.iter()) {
                if !before_work() {
                    return false;
                }
                let cursor = segment.read().scan_cursor();
                segments.push((Arc::clone(segment), cursor));
            }
            segments
        };
        let mut ids = Vec::new();
        let mut batch = Vec::new();
        for (segment, mut cursor) in segments {
            while !cursor.finished() {
                ids.clear();
                batch.clear();
                let batch_size = cursor.remaining().min(super::segment::SCAN_BATCH_SIZE);
                ids.reserve_exact(batch_size);
                batch.reserve_exact(batch_size);
                {
                    let segment = segment.read();
                    if !segment.scan_batch(&mut cursor, &mut before_work, &mut |id| ids.push(id)) {
                        return false;
                    }
                }
                // Sources can retire between batches. Consult authoritative storage
                // after releasing the source guard, just as the former ID-list scan did.
                if !before_work() {
                    return false;
                }
                self.for_each_id(&ids, |_, entity| {
                    if Self::scan_entity_visible(snapshot, entity) {
                        batch.push(entity.clone());
                    }
                });
                if !before_work() {
                    return false;
                }
                if !consume(&batch) {
                    return false;
                }
            }
        }
        true
    }

    /// Fold entities visible under an explicit MVCC snapshot in parallel.
    pub fn scan_fold_parallel<T, FInit, FFold, FReduce>(
        &self,
        snapshot: Option<&SnapshotContext>,
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
        self.fold_entities_parallel(
            init,
            |acc, entity| {
                if Self::scan_entity_visible(snapshot, entity) {
                    fold(acc, entity)
                } else {
                    acc
                }
            },
            reduce,
        )
    }

    /// Visit zone-map candidates visible under an explicit MVCC snapshot.
    pub fn scan_for_each_zoned_with_stats<F>(
        &self,
        snapshot: Option<&SnapshotContext>,
        zone_preds: &[(&str, ZoneColPred<'_>)],
        mut callback: F,
    ) -> SegmentScanStats
    where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        self.for_each_entity_zoned_with_stats(zone_preds, |entity| {
            !Self::scan_entity_visible(snapshot, entity) || callback(entity)
        })
    }

    /// Collect zone-map candidates visible under an explicit MVCC snapshot.
    pub fn scan_zoned_with_stats<F>(
        &self,
        snapshot: Option<&SnapshotContext>,
        zone_preds: &[(&str, ZoneColPred<'_>)],
        filter: F,
    ) -> (Vec<UnifiedEntity>, SegmentScanStats)
    where
        F: Fn(&UnifiedEntity) -> bool + Sync,
    {
        self.query_all_zoned_with_stats(zone_preds, |entity| {
            Self::scan_entity_visible(snapshot, entity) && filter(entity)
        })
    }

    /// Query across all segments. Uses parallel scanning for sealed segments
    /// when more than one sealed segment exists.
    pub fn query_all<F>(&self, filter: F) -> Vec<UnifiedEntity>
    where
        F: Fn(&UnifiedEntity) -> bool + Sync,
    {
        let mut entities = self.query_all_in_segments(|_| true, filter);
        self.restore_owned_row_schemas(entities.iter_mut());
        entities
    }

    fn query_all_in_segments<F>(
        &self,
        segment_filter: impl Fn(&GrowingSegment) -> bool + Sync,
        filter: F,
    ) -> Vec<UnifiedEntity>
    where
        F: Fn(&UnifiedEntity) -> bool + Sync,
    {
        let mut results = Vec::new();

        // Query growing segment — try non-blocking read first (avoids stalling
        // behind an in-progress insert; falls back to blocking if writer is active).
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            let growing = if let Some(g) = growing_arc.try_read() {
                g
            } else {
                growing_arc.read()
            };
            if segment_filter(&growing) {
                results.extend(growing.iter().filter(|e| filter(e)).cloned());
            }
        }

        // Query sealed segments — parallel when multiple exist AND multi-core
        let sealed = self.sealed.read();
        let use_parallel = sealed.len() > 1 && crate::execution::should_parallelize();
        if use_parallel {
            let filter_ref = &filter;
            let segment_filter_ref = &segment_filter;
            let segment_results: Vec<Vec<UnifiedEntity>> = std::thread::scope(|s| {
                sealed
                    .iter()
                    .map(|segment| {
                        s.spawn(move || {
                            let segment = segment.read();
                            if segment_filter_ref(&segment) {
                                segment
                                    .iter()
                                    .filter(|e| filter_ref(e))
                                    .cloned()
                                    .collect::<Vec<_>>()
                            } else {
                                Vec::new()
                            }
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
                if segment_filter(&seg) {
                    results.extend(seg.iter().filter(|e| filter(e)).cloned());
                }
            }
        }

        results
    }

    /// Probe exact segment membership for a key without materializing matching entities.
    ///
    /// Returns `false` only when every consulted segment proves absence.
    /// Missing segment state is conservative and returns `true`.
    pub fn bloom_may_contain_key(&self, key: &[u8]) -> bool {
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            let growing = growing_arc.read();
            if growing.may_contain_exact_key(key) {
                return true;
            }
        } else {
            return true;
        }

        let sealed = self.sealed.read();
        for segment_arc in sealed.iter() {
            let segment = segment_arc.read();
            if segment.may_contain_exact_key(key) {
                return true;
            }
        }

        false
    }

    /// Filter by metadata across all segments
    pub fn filter_metadata(&self, filters: &[(String, MetadataFilter)]) -> Vec<EntityId> {
        let mut results = Vec::new();

        // Growing segment
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
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
        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
            let growing = growing_arc.read();
            for entity in growing.iter_kind(kind) {
                results.push(entity.clone());
            }
        }

        // Sealed segments. Iterate through the read guard, not the
        // `Arc<RwLock<GrowingSegment>>` impl of `iter_kind` — that one cannot
        // hand out references across the lock, so it returns an empty iterator
        // and every sealed entity was silently missing from the result.
        let sealed = self.sealed.read();
        for segment_arc in sealed.iter() {
            let segment = segment_arc.read();
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

        let growing_slot = self.growing.read();
        if let Some(growing_arc) = growing_slot.as_ref() {
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

        if self.config.enable_consolidation {
            self.consolidation_tick();
        }

        Ok(())
    }

    // ========================================================================
    // Consolidation — ADR 0073 §5
    // ========================================================================

    /// One paced step of sealed-segment consolidation.
    ///
    /// Starts a run when a threshold has been crossed, copies at most
    /// `consolidation_entities_per_tick` live entities into the merged segment,
    /// and swaps once every source has been drained. Returns without finishing
    /// otherwise — the half-built merged segment waits for the next tick.
    fn consolidation_tick(&self) {
        // Drop the read guard before `start_consolidation` takes the write
        // guard: `parking_lot` locks are not reentrant.
        let running = self.consolidation.read().is_some();
        if !running && !self.start_consolidation() {
            return;
        }

        // Keep the accounting guard through publication: the merged allocation
        // must never disappear between the in-flight and active inventories.
        let mut guard = self.consolidation.write();
        let Some(run) = guard.as_mut() else {
            return;
        };
        self.copy_bounded(run);
        if run.cursor >= run.sources.len() {
            if let Some(run) = guard.take() {
                self.finish_consolidation(run);
            }
        }
    }

    /// Evaluate the trigger and, when it fires, open a consolidation run.
    ///
    /// Returns `true` when a run was started.
    fn start_consolidation(&self) -> bool {
        let sources = self.select_candidates();
        self.start_consolidation_from_sources(sources)
    }

    fn start_consolidation_from_sources(&self, sources: Vec<SegmentId>) -> bool {
        if sources.len() < 2 && !self.candidates_carry_tombstones(&sources) {
            return false;
        }

        let merged_id = self.next_segment_id.fetch_add(1, Ordering::SeqCst);
        let merged = GrowingSegment::new(merged_id, &self.collection);

        *self.consolidation.write() = Some(Consolidation {
            sources: sources
                .into_iter()
                .map(|id| SourceProgress {
                    id,
                    copied: Vec::new(),
                    tombstones_at_open: None,
                })
                .collect(),
            merged_id,
            merged,
            cursor: 0,
            pending_ids: Vec::new(),
            pending_cursor: 0,
            entities_copied: 0,
        });

        self.stats.write().consolidation.runs_started += 1;
        true
    }

    /// Run one paced consolidation tick because admission pressure needs
    /// reclaimable bytes, even if the background thresholds have not fired.
    ///
    /// Returns true when the call started or advanced a consolidation run.
    pub fn pressure_consolidation_tick(&self) -> bool {
        if !self.config.enable_consolidation {
            return false;
        }

        let running = self.consolidation.read().is_some();
        if !running {
            let sources = self.dirty_sealed_segments();
            if !self.start_consolidation_from_sources(sources) {
                return false;
            }
        }

        self.consolidation_tick();
        true
    }

    /// Which sealed segments this collection should merge, if any.
    ///
    /// Three thresholds, any of which arms consolidation:
    ///
    /// * **sealed-segment count** above `max_sealed_segments` — every sealed
    ///   segment merges, which is what that knob always claimed to do.
    /// * **tombstone ratio** — dead entities over total entities across the
    ///   sealed set, past [`CONSOLIDATION_TOMBSTONE_RATIO`].
    /// * **fragmentation** — reclaimable bytes over held bytes, past
    ///   [`CONSOLIDATION_FRAGMENTATION_RATIO`].
    ///
    /// The count trigger takes every sealed segment; the two budget triggers
    /// take only the segments that actually carry tombstones, so a clean
    /// segment is never rewritten for nothing.
    fn select_candidates(&self) -> Vec<SegmentId> {
        let sealed = self.sealed.read();
        if sealed.is_empty() {
            return Vec::new();
        }

        if sealed.len() > self.config.max_sealed_segments {
            return sealed.iter().map(|segment| segment.read().id()).collect();
        }

        let mut live = 0usize;
        let mut tombstones = 0usize;
        let mut held = 0u64;
        let mut reclaimable = 0u64;
        let mut dirty = Vec::new();

        for segment_arc in sealed.iter() {
            let segment = segment_arc.read();
            live += segment.live_entity_count();
            tombstones += segment.tombstone_count();
            held += segment.resident_bytes();
            reclaimable += segment.reclaimable_bytes();
            if segment.tombstone_count() > 0 {
                dirty.push(segment.id());
            }
        }

        let total = live + tombstones;
        let tombstone_ratio = if total == 0 {
            0.0
        } else {
            tombstones as f64 / total as f64
        };
        let fragmentation = if held == 0 {
            0.0
        } else {
            reclaimable as f64 / held as f64
        };

        if tombstone_ratio >= CONSOLIDATION_TOMBSTONE_RATIO
            || fragmentation >= CONSOLIDATION_FRAGMENTATION_RATIO
        {
            return dirty;
        }

        Vec::new()
    }

    fn dirty_sealed_segments(&self) -> Vec<SegmentId> {
        self.sealed
            .read()
            .iter()
            .filter_map(|segment_arc| {
                let segment = segment_arc.read();
                (segment.reclaimable_bytes() > 0).then(|| segment.id())
            })
            .collect()
    }

    /// A single candidate is still worth merging when it carries tombstones —
    /// the merge drops them. A single clean candidate is not.
    fn candidates_carry_tombstones(&self, sources: &[SegmentId]) -> bool {
        if sources.is_empty() {
            return false;
        }
        let sealed = self.sealed.read();
        sealed.iter().any(|segment_arc| {
            let segment = segment_arc.read();
            sources.contains(&segment.id()) && segment.tombstone_count() > 0
        })
    }

    fn sealed_segment(&self, id: SegmentId) -> Option<Arc<RwLock<GrowingSegment>>> {
        let sealed = self.sealed.read();
        sealed
            .iter()
            .find(|segment| segment.read().id() == id)
            .map(Arc::clone)
    }

    /// Copy at most one tick's worth of live entities into the merged segment.
    ///
    /// The per-tick bound is the pacing contract: a tick's cost is O(bound),
    /// independent of how many entities the run still has to move.
    fn copy_bounded(&self, run: &mut Consolidation) {
        let mut budget = self.config.consolidation_entities_per_tick;

        while budget > 0 && run.cursor < run.sources.len() {
            let source_id = run.sources[run.cursor].id;
            let Some(source_arc) = self.sealed_segment(source_id) else {
                // The source vanished. Only consolidation removes sealed
                // segments and it is single-threaded per collection, so this
                // cannot happen today — skip it rather than resurrect its rows.
                run.cursor += 1;
                run.pending_ids.clear();
                run.pending_cursor = 0;
                continue;
            };

            if run.sources[run.cursor].tombstones_at_open.is_none() {
                let source = source_arc.read();
                run.pending_ids = source.live_entity_ids();
                run.pending_cursor = 0;
                run.sources[run.cursor].tombstones_at_open = Some(source.tombstone_count());
            }

            {
                let source = source_arc.read();
                while budget > 0 && run.pending_cursor < run.pending_ids.len() {
                    let id = run.pending_ids[run.pending_cursor];
                    run.pending_cursor += 1;
                    budget -= 1;

                    // Tombstoned between the snapshot and now — it must not be
                    // resurrected into the merged segment.
                    let Some(entity) = source.get(id) else {
                        continue;
                    };
                    let metadata = source.get_metadata(id);
                    run.merged.adopt_entity(entity.clone(), metadata);
                    run.sources[run.cursor].copied.push(id);
                    run.entities_copied += 1;
                }
            }

            if run.pending_cursor >= run.pending_ids.len() {
                run.cursor += 1;
                run.pending_ids.clear();
                run.pending_cursor = 0;
            }
        }
    }

    /// Atomically replace the source segments with the merged one.
    ///
    /// The `sealed` write lock is the boundary: a reader holds the read lock
    /// for the whole of its scan, so it observes either the old set or the new
    /// set, never a gap and never both copies of a row.
    fn finish_consolidation(&self, mut run: Consolidation) {
        let mut sealed = self.sealed.write();

        let mut positions = Vec::with_capacity(run.sources.len());
        for source in &run.sources {
            if let Some(index) = sealed
                .iter()
                .position(|segment| segment.read().id() == source.id)
            {
                positions.push(index);
            }
        }
        if positions.is_empty() {
            return;
        }
        positions.sort_unstable();

        // Rows deleted from a source after we copied them must not come back.
        // A source whose tombstone count is unchanged saw no deletes while the
        // merge ran, so its copied rows need no per-row check.
        let mut stale = Vec::new();
        for source in &run.sources {
            let Some(source_arc) = sealed
                .iter()
                .find(|segment| segment.read().id() == source.id)
            else {
                continue;
            };
            let segment = source_arc.read();
            if source.tombstones_at_open == Some(segment.tombstone_count()) {
                continue;
            }
            stale.extend(
                source
                    .copied
                    .iter()
                    .copied()
                    .filter(|id| !segment.contains(*id)),
            );
        }
        for id in stale {
            run.merged.evict_entity(id);
        }

        let reclaimed_bytes: u64 = positions
            .iter()
            .map(|&index| sealed[index].read().resident_bytes())
            .sum();
        let reclaimed_tombstones: u64 = positions
            .iter()
            .map(|&index| sealed[index].read().tombstone_count() as u64)
            .sum();

        if run.merged.seal().is_err() {
            return;
        }
        let merged_bytes = run.merged.resident_bytes();
        let merged_arc = Arc::new(RwLock::new(run.merged));

        {
            let mut retired = self.retired.write();
            retired.retain(|source| source.strong_count() > 0);
            for &index in positions.iter().rev() {
                let source = sealed.remove(index);
                if Arc::strong_count(&source) > 1 {
                    retired.push(Arc::downgrade(&source));
                }
            }
        }
        let insert_at = positions[0].min(sealed.len());
        sealed.insert(insert_at, Arc::clone(&merged_arc));
        drop(sealed);

        // Remap the entity→segment hints that pointed at the retired sources.
        let source_ids: Vec<SegmentId> = run.sources.iter().map(|source| source.id).collect();
        {
            let mut entity_segment = self.entity_segment.write();
            for segment_id in entity_segment.values_mut() {
                if source_ids.contains(segment_id) {
                    *segment_id = run.merged_id;
                }
            }
        }

        {
            let mut stats = self.stats.write();
            stats.sealed_count = stats
                .sealed_count
                .saturating_sub(positions.len())
                .saturating_add(1);
            let consolidation = &mut stats.consolidation;
            consolidation.runs_completed += 1;
            consolidation.segments_merged += positions.len() as u64;
            consolidation.tombstones_reclaimed += reclaimed_tombstones;
            consolidation.bytes_reclaimed += reclaimed_bytes.saturating_sub(merged_bytes);
        }

        self.emit(LifecycleEvent::SegmentConsolidated {
            source: source_ids,
            target: run.merged_id,
        });
    }

    /// Entities copied by the consolidation currently in flight, or `None` when
    /// none is running. Exposed for the pacing tests.
    #[cfg(test)]
    fn consolidation_progress(&self) -> Option<u64> {
        self.consolidation
            .read()
            .as_ref()
            .map(|run| run.entities_copied)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::storage::entity::{EntityData, EntityKind, RowData};
    use crate::transaction::snapshot::{Snapshot, SnapshotManager};
    use reddb_types::Value;

    #[test]
    fn physical_pages_keep_global_order_across_segments_and_deleted_slots() {
        let manager = SegmentManager::new("pages");
        for group in [[50, 10], [30, 20]] {
            for id in group {
                let mut entity = UnifiedEntity::table_row(EntityId::new(id), "pages", id, vec![]);
                entity.data = EntityData::Row(RowData::with_names(
                    vec![Value::Integer(id as i64)],
                    vec!["id".into()],
                ));
                manager.insert(entity).unwrap();
            }
            manager.force_seal().unwrap();
        }
        manager
            .insert(UnifiedEntity::table_row(
                EntityId::new(40),
                "pages",
                40,
                vec![],
            ))
            .unwrap();
        let (total, page) = manager.page_in_id_order(1, 2);
        assert_eq!(total, 5);
        assert_eq!(
            page.iter()
                .map(|entity| entity.id.raw())
                .collect::<Vec<_>>(),
            [20, 30]
        );
        let EntityData::Row(row) = &page[0].data else {
            panic!("row")
        };
        assert_eq!(row.get_field("id"), Some(&Value::Integer(20)));
        manager.delete(EntityId::new(30)).unwrap();
        let (total, page) = manager.page_in_id_order(1, 2);
        assert_eq!(total, 4);
        assert_eq!(
            page.iter()
                .map(|entity| entity.id.raw())
                .collect::<Vec<_>>(),
            [20, 40]
        );
        for offset in [4, usize::MAX] {
            let (total, page) = manager.page_in_id_order(offset, 2);
            assert_eq!(total, 4);
            assert!(page.is_empty());
        }
    }

    fn lookup_version(id: u64, key: &str, xmin: u64, xmax: u64) -> UnifiedEntity {
        let mut entity = UnifiedEntity::table_row(EntityId::new(id), "keys", id, vec![]);
        entity.data = EntityData::Row(RowData::with_names(
            vec![Value::text(key)],
            vec!["key".into()],
        ));
        entity.set_xmin(xmin);
        entity.set_xmax(xmax);
        entity
    }

    #[test]
    fn snapshot_scan_does_not_miss_a_sealed_entity_moving_into_growing() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let manager = Arc::new(SegmentManager::new("keys"));
        manager
            .insert(lookup_version(1, "sealed", 3, 0))
            .expect("sealed preimage");
        manager.force_seal().expect("seal preimage");
        let source = Arc::clone(&manager.sealed.read()[0]);
        manager
            .insert(lookup_version(2, "growing", 3, 0))
            .expect("growing row");
        let snapshot = SnapshotContext {
            snapshot: Snapshot {
                xid: 10,
                in_progress: HashSet::new(),
            },
            manager: Arc::new(SnapshotManager::new()),
            own_xids: HashSet::new(),
            requires_index_fallback: false,
            serializable_reader: None,
        };
        let (begin, requested) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        let writer_manager = Arc::clone(&manager);
        let writer = std::thread::spawn(move || {
            requested.recv().expect("reader reached growing row");
            started.send(()).expect("writer attempts relocation");
            // This is update's real fallback after its growing probe missed.
            writer_manager
                .rewrite_sealed_entity_into_growing(lookup_version(1, "sealed", 3, 12), None)
                .expect("relocate the still-visible preimage");
        });
        let mut begin = Some(begin);
        let mut ids = Vec::new();
        manager.scan_for_each(Some(&snapshot), |entity| {
            ids.push(entity.id.raw());
            if entity.id.raw() == 2 {
                begin
                    .take()
                    .expect("one growing row")
                    .send(())
                    .expect("start writer");
                ready
                    .recv_timeout(Duration::from_secs(5))
                    .expect("writer started");
                // A safe relocation may wait for this scan. The old path instead
                // removes the sealed preimage before waiting for growing's lock.
                let deadline = Instant::now() + Duration::from_secs(2);
                while source.read().contains(EntityId::new(1)) && Instant::now() < deadline {
                    std::thread::yield_now();
                }
            }
            true
        });
        writer.join().expect("writer finished");
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2], "relocation hid a row from a live snapshot");
    }

    #[test]
    fn key_lookup_matches_snapshot_scan_across_segments() {
        let manager = Arc::new(SegmentManager::new("keys"));
        for entity in [
            lookup_version(1, "changed", 3, 12),
            lookup_version(2, "deleted", 3, 7),
        ] {
            manager.insert(entity).expect("old version");
        }
        manager.force_seal().expect("seal old versions");
        for entity in [
            lookup_version(3, "changed", 12, 0),
            lookup_version(4, "pending", 4, 0),
            lookup_version(5, "own", 15, 0),
            lookup_version(6, "live", 3, 0),
        ] {
            manager.insert(entity).expect("new version");
        }
        std::thread::spawn(move || {
            let snapshot = SnapshotContext {
                snapshot: Snapshot {
                    xid: 10,
                    in_progress: HashSet::from([4]),
                },
                manager: Arc::new(SnapshotManager::new()),
                own_xids: HashSet::from([15]),
                requires_index_fallback: false,
                serializable_reader: None,
            };
            for snapshot in [Some(&snapshot), None] {
                for key in ["changed", "deleted", "pending", "own", "live", "absent"] {
                    let expected = manager
                        .scan(snapshot, |entity| {
                            crate::storage::segment_key_lookup::SegmentKeyLookup::key(entity)
                                == Some(key)
                        })
                        .into_iter()
                        .max_by_key(|entity| entity.xmin)
                        .map(|entity| entity.id);
                    assert_eq!(
                        manager.project_key_entity(key, snapshot, true, |entity| entity.id),
                        expected,
                        "key={key}"
                    );
                }
            }
            assert_eq!(
                manager.project_key_entity("changed", Some(&snapshot), true, |entity| entity.id),
                Some(EntityId::new(1))
            );
            assert_eq!(
                manager.project_key_entity("changed", None, true, |entity| entity.id),
                Some(EntityId::new(3))
            );
            assert!(manager
                .project_key_entity("deleted", None, true, |entity| entity.id)
                .is_none());
        })
        .join()
        .expect("snapshot worker");
    }

    #[test]
    fn key_lookup_survives_paced_consolidation() {
        let manager = consolidating_manager(1);
        manager
            .bulk_insert(
                (1..=10)
                    .map(|id| lookup_version(id, &format!("key-{id}"), 1, 0))
                    .collect(),
            )
            .expect("rows");
        manager.force_seal().expect("seal");
        for id in 1..=4 {
            manager.delete(EntityId::new(id)).expect("delete");
        }
        assert!(manager.pressure_consolidation_tick());
        drain_maintenance(&manager);
        for id in 1..=10 {
            assert_eq!(
                manager.project_key_entity(&format!("key-{id}"), None, false, |entity| entity.id),
                (id > 4).then_some(EntityId::new(id))
            );
        }
    }

    #[test]
    #[ignore = "diagnostic of segment fragmentation and retained history; no timing assertions"]
    fn key_lookup_fragmentation_history_diagnostic() {
        use std::{hint::black_box, time::Instant};
        for segments in [1, 16, 64] {
            for versions in [1, 100, 1000] {
                let manager = SegmentManager::new("keys");
                let mut id = 1;
                for segment in 0..segments {
                    let mut rows: Vec<_> = (0..128)
                        .map(|_| {
                            let entity = lookup_version(id, &format!("other-{id}"), 1, 0);
                            id += 1;
                            entity
                        })
                        .collect();
                    for version in (segment..versions).step_by(segments) {
                        rows.push(lookup_version(
                            id,
                            "history",
                            version as u64 + 1,
                            if version + 1 == versions {
                                0
                            } else {
                                version as u64 + 2
                            },
                        ));
                        id += 1;
                    }
                    manager.bulk_insert(rows).expect("rows");
                    manager.force_seal().expect("seal");
                }
                let started = Instant::now();
                for _ in 0..2000 {
                    let xmin =
                        manager.project_key_entity(black_box("history"), None, true, |entity| {
                            entity.xmin
                        });
                    assert_eq!(xmin, Some(versions as u64));
                }
                eprintln!("segments={segments} versions={versions} reads=2000 elapsed_us={} resident_bytes={}", started.elapsed().as_micros(), manager.resident_bytes());
            }
        }
    }

    #[test]
    fn ordered_fingerprint_scan_matches_legacy_sort_without_cloning_rows() {
        let manager = SegmentManager::new("fingerprint");
        let row = |id, label: &str| {
            UnifiedEntity::table_row(
                EntityId::new(id),
                "fingerprint",
                id,
                vec![Value::text(label)],
            )
        };
        manager
            .bulk_insert(vec![row(8, "eight"), row(2, "sealed-two"), row(5, "five")])
            .expect("sealed rows");
        manager.force_seal().expect("seal");
        manager
            .bulk_insert(vec![row(7, "seven"), row(2, "growing-two"), row(1, "one")])
            .expect("growing rows");

        let mut expected = manager.query_all(|_| true);
        expected.sort_by_key(|entity| entity.id.raw());
        let expected: Vec<_> = expected
            .iter()
            .map(|entity| format!("{entity:?}"))
            .collect();
        let mut stored_addresses = HashSet::new();
        manager.for_each_entity(|entity| {
            stored_addresses.insert(entity as *const UnifiedEntity as usize);
            true
        });

        let mut actual = Vec::new();
        manager.for_each_entity_in_id_order(|entity| {
            assert!(stored_addresses.contains(&(entity as *const UnifiedEntity as usize)));
            assert!(
                manager.growing.try_write().is_some(),
                "topology guard retained"
            );
            assert!(
                manager.sealed.try_write().is_some(),
                "topology guard retained"
            );
            actual.push(format!("{entity:?}"));
            true
        });
        assert_eq!(
            actual, expected,
            "equal IDs retain their original segment order"
        );
    }

    #[test]
    fn ordered_fingerprint_scan_stops_early_and_releases_sources() {
        let manager = SegmentManager::new("fingerprint");
        for id in [3, 1, 2] {
            manager
                .insert(UnifiedEntity::table_row(
                    EntityId::new(id),
                    "fingerprint",
                    id,
                    vec![Value::Integer(id as i64)],
                ))
                .expect("row");
        }
        let source = manager.growing.read().as_ref().expect("source").clone();
        let handles = Arc::strong_count(&source);
        let mut seen = Vec::new();
        manager.for_each_entity_in_id_order(|entity| {
            seen.push(entity.id.raw());
            false
        });
        assert_eq!(seen, vec![1]);
        assert_eq!(Arc::strong_count(&source), handles);
        assert!(
            source.try_write().is_some(),
            "segment guard retained after early stop"
        );
    }

    #[test]
    fn bounded_scan_survives_sealing_consolidation_and_new_writes() {
        let manager = SegmentManager::with_config(
            "vectors",
            ManagerConfig {
                max_sealed_segments: 1,
                consolidation_entities_per_tick: 10_000,
                ..Default::default()
            },
        );
        for part in 0..3 {
            manager
                .bulk_insert(
                    (1..=300)
                        .map(|offset| {
                            UnifiedEntity::vector(
                                EntityId::new(part * 300 + offset),
                                "vectors",
                                vec![1.0, 0.0],
                            )
                        })
                        .collect(),
                )
                .expect("batch");
            if part < 2 {
                manager.force_seal().expect("seal");
            }
        }
        let original_growing = manager.growing.read().as_ref().expect("growing").clone();
        let mut changed = false;
        let mut seen = Vec::new();
        assert!(manager.scan_batches(
            None,
            || true,
            |batch| {
                assert!(batch.len() <= super::super::segment::SCAN_BATCH_SIZE);
                assert!(
                    manager.growing.try_write().is_some(),
                    "consumer holds no topology guard"
                );
                assert!(
                    original_growing.try_write().is_some(),
                    "consumer holds no segment guard"
                );
                for entity in batch {
                    assert!(manager.get(entity.id).is_some(), "reentrant storage lookup");
                    seen.push(entity.id.raw());
                }
                if !changed {
                    changed = true;
                    manager.force_seal().expect("seal during scan");
                    manager
                        .insert(UnifiedEntity::vector(
                            EntityId::new(5000),
                            "vectors",
                            vec![1.0, 0.0],
                        ))
                        .expect("new write");
                    drain_maintenance(&manager);
                    assert!(manager.stats().consolidation.runs_completed > 0);
                    assert!(manager
                        .delete(EntityId::new(1))
                        .expect("delete after source retirement"));
                    assert!(manager.get(EntityId::new(1)).is_none());
                }
                true
            }
        ));
        seen.sort_unstable();
        assert_eq!(
            seen,
            (2..=900).collect::<Vec<_>>(),
            "no duplicate, missing or late candidates"
        );
        let mut after = Vec::new();
        assert!(manager.scan_batches(
            None,
            || true,
            |batch| {
                after.extend(batch.iter().map(|entity| entity.id.raw()));
                true
            }
        ));
        after.sort_unstable();
        let mut expected: Vec<_> = (2..=900).collect();
        expected.push(5000);
        assert_eq!(after, expected, "adopted entities have scan positions");
    }

    #[test]
    fn bounded_scan_applies_snapshot_to_relocated_versions() {
        for (consolidate, bulk) in [(false, false), (false, true), (true, false), (true, true)] {
            let manager = SegmentManager::with_config(
                "vectors",
                ManagerConfig {
                    max_sealed_segments: 1,
                    ..Default::default()
                },
            );
            for ids in [[1, 2], [3, 4]] {
                let entities = ids.map(|id| {
                    let mut entity = UnifiedEntity::vector(EntityId::new(id), "vectors", vec![1.0]);
                    entity.set_xmin(3);
                    entity
                });
                if bulk {
                    manager
                        .bulk_insert(entities.into())
                        .expect("initial versions");
                } else {
                    for entity in entities {
                        manager.insert(entity).expect("initial version");
                    }
                }
                manager.force_seal().expect("seal source");
            }
            let snapshot = SnapshotContext {
                snapshot: Snapshot {
                    xid: 10,
                    in_progress: HashSet::new(),
                },
                manager: Arc::new(SnapshotManager::new()),
                own_xids: HashSet::new(),
                requires_index_fallback: false,
                serializable_reader: None,
            };
            let mut changed = false;
            let mut seen = Vec::new();
            assert!(manager.scan_batches(
                Some(&snapshot),
                || true,
                |batch| {
                    seen.extend(batch.iter().map(|entity| entity.id.raw()));
                    if !changed {
                        changed = true;
                        if consolidate {
                            drain_maintenance(&manager);
                            assert!(manager.stats().consolidation.runs_completed > 0);
                        }
                        for (id, xmax) in [(3, 8), (4, 12)] {
                            let mut entity =
                                manager.get(EntityId::new(id)).expect("retained version");
                            entity.set_xmax(xmax);
                            manager
                                .update(entity)
                                .expect("update after source retirement");
                        }
                    }
                    true
                }
            ));
            seen.sort_unstable();
            assert_eq!(seen, vec![1, 2, 4], "snapshot uses authoritative xmax");
            let mut again = Vec::new();
            assert!(manager.scan_batches(
                Some(&snapshot),
                || true,
                |batch| {
                    again.extend(batch.iter().map(|entity| entity.id.raw()));
                    true
                }
            ));
            again.sort_unstable();
            assert_eq!(
                again,
                vec![1, 2, 4],
                "old slots do not duplicate relocated versions"
            );
            let mut current = Vec::new();
            assert!(manager.scan_batches(
                None,
                || true,
                |batch| {
                    current.extend(batch.iter().map(|entity| entity.id.raw()));
                    true
                }
            ));
            current.sort_unstable();
            assert_eq!(
                current,
                vec![1, 2],
                "current view excludes both deleted versions"
            );
        }
    }

    #[test]
    fn bounded_scan_keeps_flat_and_hash_positions_through_mutation() {
        let manager = SegmentManager::new("mixed");
        let vector = |id| UnifiedEntity::vector(EntityId::new(id), "mixed", vec![1.0]);
        manager
            .bulk_insert((1..=300).map(vector).collect())
            .expect("flat");
        manager.insert(vector(500)).expect("hash insert");
        manager
            .bulk_insert(vec![vector(502), vector(505)])
            .expect("gapped bulk");
        manager.delete(EntityId::new(2)).expect("flat delete");
        manager.delete(EntityId::new(505)).expect("hash delete");
        let mut changed = false;
        let mut seen = Vec::new();
        assert!(manager.scan_batches(
            None,
            || true,
            |batch| {
                seen.extend(batch.iter().map(|entity| entity.id.raw()));
                if !changed {
                    changed = true;
                    manager
                        .insert(vector(700))
                        .expect("append after cursor boundary");
                    manager
                        .update(UnifiedEntity::graph_node(
                            EntityId::new(500),
                            "mixed",
                            "Node",
                            HashMap::new(),
                        ))
                        .expect("structural update");
                }
                true
            }
        ));
        seen.sort_unstable();
        let mut expected: Vec<_> = (1..=300).filter(|id| *id != 2).collect();
        expected.extend([500, 502]);
        assert_eq!(seen, expected);
    }

    #[test]
    fn bounded_scan_preserves_snapshot_and_discards_interrupted_batches() {
        let manager = SegmentManager::new("vectors");
        let mut entities = Vec::new();
        for (id, xmin, xmax) in [(1, 3, 12), (2, 12, 0), (3, 3, 0), (4, 3, 4)] {
            let mut entity = UnifiedEntity::vector(EntityId::new(id), "vectors", vec![1.0]);
            entity.set_xmin(xmin);
            entity.set_xmax(xmax);
            entities.push(entity);
        }
        manager.bulk_insert(entities).expect("versions");
        manager.force_seal().expect("seal");
        let snapshot = SnapshotContext {
            snapshot: Snapshot {
                xid: 10,
                in_progress: HashSet::from([4]),
            },
            manager: Arc::new(SnapshotManager::new()),
            own_xids: HashSet::new(),
            requires_index_fallback: false,
            serializable_reader: None,
        };
        let mut work = 0;
        let mut callbacks = 0;
        assert!(!manager.scan_batches(
            Some(&snapshot),
            || {
                work += 1;
                work <= 2
            },
            |_| {
                callbacks += 1;
                true
            }
        ));
        assert_eq!(callbacks, 0, "a stopped batch is not consumed");
        for (view, expected) in [(Some(&snapshot), vec![1, 3, 4]), (None, vec![2, 3])] {
            let mut seen = Vec::new();
            assert!(manager.scan_batches(
                view,
                || true,
                |batch| {
                    seen.extend(batch.iter().map(|entity| entity.id.raw()));
                    true
                }
            ));
            seen.sort_unstable();
            assert_eq!(seen, expected);
        }
    }

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
    fn graph_pruning_preserves_physical_versions_and_charges_only_selected_segments() {
        let manager = Arc::new(SegmentManager::new("mixed"));
        manager
            .bulk_insert(
                (1..=128)
                    .map(|id| UnifiedEntity::vector(EntityId::new(id), "mixed", vec![]))
                    .collect(),
            )
            .expect("vector-only segment");
        manager.force_seal().expect("seal vectors");
        let mut old =
            UnifiedEntity::graph_node(EntityId::new(200), "mixed", "person", Default::default());
        old.set_xmin(3);
        old.set_xmax(12);
        let mut new =
            UnifiedEntity::graph_node(EntityId::new(201), "mixed", "person", Default::default());
        new.set_xmin(12);
        let edge = UnifiedEntity::graph_edge(
            EntityId::new(202),
            "mixed",
            "a",
            "b",
            1.0,
            Default::default(),
        );
        manager
            .bulk_insert(vec![old, new, edge])
            .expect("physical graph versions");
        manager.force_seal().expect("seal graph");
        manager
            .insert(UnifiedEntity::graph_node(
                EntityId::new(203),
                "mixed",
                "person",
                Default::default(),
            ))
            .expect("growing node");
        let snapshot = SnapshotContext {
            snapshot: Snapshot {
                xid: 10,
                in_progress: HashSet::new(),
            },
            manager: Arc::new(SnapshotManager::new()),
            own_xids: HashSet::new(),
            requires_index_fallback: false,
            serializable_reader: None,
        };
        std::thread::spawn(move || {
            for view in [None, Some(&snapshot)] {
                for kind in [GraphEntityKind::Node, GraphEntityKind::Edge] {
                    let expected: HashSet<_> = manager
                        .scan(view, |item| kind.matches(&item.kind))
                        .into_iter()
                        .map(|item| item.id)
                        .collect();
                    let actual: HashSet<_> = manager
                        .scan_graph_kind(view, kind)
                        .into_iter()
                        .map(|item| item.id)
                        .collect();
                    assert_eq!(actual, expected);
                    let mut visited = 0;
                    let mut candidates = HashSet::new();
                    manager.scan_graph_candidates_for_each(
                        view,
                        kind,
                        || true,
                        |item| {
                            visited += 1;
                            if kind.matches(&item.kind) {
                                candidates.insert(item.id);
                            }
                            true
                        },
                    );
                    assert_eq!(candidates, expected);
                    assert_eq!(
                        visited,
                        if matches!(kind, GraphEntityKind::Node) {
                            3
                        } else {
                            2
                        }
                    );
                }
            }
            let ids: HashSet<_> = manager
                .scan_graph_kind(Some(&snapshot), GraphEntityKind::Node)
                .into_iter()
                .map(|item| item.id.raw())
                .collect();
            assert_eq!(ids, HashSet::from([200, 203]));
            let ids: HashSet<_> = manager
                .scan_graph_kind(None, GraphEntityKind::Node)
                .into_iter()
                .map(|item| item.id.raw())
                .collect();
            assert_eq!(ids, HashSet::from([201, 203]));
            let mut visits = 0;
            manager.scan_graph_candidates_for_each(
                None,
                GraphEntityKind::Node,
                || true,
                |_| {
                    visits += 1;
                    false
                },
            );
            assert_eq!(
                visits, 1,
                "early termination must also stop subsequent segments"
            );
        })
        .join()
        .expect("snapshot worker");
    }

    #[test]
    fn snapshot_scan_applies_table_driven_visibility_on_fresh_thread() {
        let manager = Arc::new(SegmentManager::new("accounts"));
        let cases = [
            ("legacy row", 0, 0, true),
            ("committed creator", 3, 0, true),
            ("future creator", 11, 0, false),
            ("in-progress creator", 4, 0, false),
            ("committed deleter", 3, 7, false),
            ("future deleter", 3, 11, true),
            ("in-progress deleter", 3, 4, true),
        ];

        for (index, (_, xmin, xmax, _)) in cases.iter().enumerate() {
            let row_id = index as u64 + 1;
            let mut entity = UnifiedEntity::table_row(
                EntityId::new(row_id),
                "accounts",
                row_id,
                vec![Value::Integer(row_id as i64)],
            );
            entity.set_xmin(*xmin);
            entity.set_xmax(*xmax);
            manager.insert(entity).unwrap();
        }

        let snapshot = SnapshotContext {
            snapshot: Snapshot {
                xid: 10,
                in_progress: HashSet::from([4]),
            },
            manager: Arc::new(SnapshotManager::new()),
            own_xids: HashSet::new(),
            requires_index_fallback: false,
            serializable_reader: None,
        };

        let raw_visible_row_ids: HashSet<u64> = manager
            .query_all(|entity| entity_visible_with_context(Some(&snapshot), entity))
            .into_iter()
            .filter_map(|entity| match entity.kind {
                EntityKind::TableRow { row_id, .. } => Some(row_id),
                _ => None,
            })
            .collect();

        let scanned = std::thread::spawn(move || manager.scan(Some(&snapshot), |_| true))
            .join()
            .unwrap();
        let visible_row_ids: HashSet<u64> = scanned
            .into_iter()
            .filter_map(|entity| match entity.kind {
                EntityKind::TableRow { row_id, .. } => Some(row_id),
                _ => None,
            })
            .collect();
        assert_eq!(visible_row_ids, raw_visible_row_ids);

        for (index, (name, _, _, expected_visible)) in cases.iter().enumerate() {
            let row_id = index as u64 + 1;
            assert_eq!(
                visible_row_ids.contains(&row_id),
                *expected_visible,
                "{name} visibility mismatch"
            );
        }
    }

    #[test]
    fn frameless_scan_hides_superseded_versions_and_moderated_rows() {
        // A `None` snapshot is the autocommit / prepared-fast-path read: it
        // must keep the resolver fallback (xmax == 0), never show every
        // version, and never panic (issue #2136 review).
        let manager = SegmentManager::new("accounts");
        let mut live =
            UnifiedEntity::table_row(EntityId::new(1), "accounts", 1, vec![Value::Integer(1)]);
        live.set_xmin(2);
        live.set_xmax(0);
        manager.insert(live).unwrap();

        let mut superseded =
            UnifiedEntity::table_row(EntityId::new(2), "accounts", 2, vec![Value::Integer(2)]);
        superseded.set_xmin(2);
        superseded.set_xmax(9);
        manager.insert(superseded).unwrap();

        let rows: Vec<u64> = manager
            .scan(None, |_| true)
            .into_iter()
            .filter_map(|entity| match entity.kind {
                EntityKind::TableRow { row_id, .. } => Some(row_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            vec![1],
            "frameless scan must hide superseded versions"
        );
    }

    #[test]
    fn bloom_may_contain_key_probes_inserted_rid_bytes() {
        let manager = SegmentManager::new("test_collection");

        let entity = UnifiedEntity::table_row(
            manager.next_entity_id(),
            "users",
            1,
            vec![Value::text("Alice".to_string())],
        );

        let id = manager.insert(entity).unwrap();

        assert!(manager.bloom_may_contain_key(&id.raw().to_le_bytes()));
        assert!(manager.bloom_may_contain_key(id.raw().to_string().as_bytes()));
        assert!(!manager.bloom_may_contain_key(&u64::MAX.to_le_bytes()));

        assert!(manager.delete(id).unwrap());
        assert!(!manager.bloom_may_contain_key(&id.raw().to_le_bytes()));
    }

    #[test]
    fn zoned_scan_reports_pruned_segments_and_preserves_results() {
        let config = ManagerConfig {
            segment_config: SegmentConfig {
                max_entities: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let manager = SegmentManager::with_config("events", config);

        for (idx, value) in [10, 20, 100, 110, 200].into_iter().enumerate() {
            let row = RowData::with_names(vec![Value::Integer(value)], vec!["ts".to_string()]);
            let entity = UnifiedEntity::new(
                manager.next_entity_id(),
                EntityKind::TableRow {
                    table: "events".into(),
                    row_id: (idx + 1) as u64,
                },
                EntityData::Row(row),
            );
            manager.insert(entity).unwrap();
        }

        let full = manager.query_all(|entity| match &entity.data {
            EntityData::Row(row) => matches!(row.get_field("ts"), Some(Value::Integer(100))),
            _ => false,
        });
        let probe = Value::Integer(100);
        let (pruned, stats) =
            manager.query_all_zoned_with_stats(&[("ts", ZoneColPred::Eq(&probe))], |entity| {
                match &entity.data {
                    EntityData::Row(row) => {
                        matches!(row.get_field("ts"), Some(Value::Integer(100)))
                    }
                    _ => false,
                }
            });

        assert_eq!(pruned.len(), full.len());
        assert_eq!(pruned[0].id, full[0].id);
        assert_eq!(stats.segments_total, 3);
        assert_eq!(stats.segments_scanned, 2);
        assert_eq!(stats.segments_pruned, 1);
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

    // ========================================================================
    // Consolidation — ADR 0073 §5
    // ========================================================================

    /// A manager whose sealed set never trips the count trigger, so tests can
    /// exercise the tombstone/fragmentation triggers in isolation.
    fn consolidating_manager(entities_per_tick: usize) -> SegmentManager {
        SegmentManager::with_config(
            "test",
            ManagerConfig {
                max_sealed_segments: 1_000,
                consolidation_entities_per_tick: entities_per_tick,
                ..Default::default()
            },
        )
    }

    /// Bulk-load `count` rows into a fresh segment and seal it. Bulk-inserted
    /// entities land in flat storage, where a tombstone keeps its payload
    /// resident — which is the memory consolidation reclaims.
    fn seal_bulk_segment(manager: &SegmentManager, count: usize) -> Vec<EntityId> {
        let entities: Vec<UnifiedEntity> = (0..count)
            .map(|i| {
                UnifiedEntity::table_row(
                    manager.next_entity_id(),
                    "users",
                    i as u64,
                    vec![Value::Integer(i as i64), Value::text(format!("row-{i}"))],
                )
            })
            .collect();
        let ids = manager.bulk_insert(entities).expect("bulk insert");
        manager.force_seal().expect("seal");
        ids
    }

    fn live_ids(manager: &SegmentManager) -> Vec<u64> {
        let mut ids: Vec<u64> = manager
            .query_all(|_| true)
            .into_iter()
            .map(|entity| entity.id.raw())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn drain_maintenance(manager: &SegmentManager) -> usize {
        let mut ticks = 0;
        while manager.consolidation.read().is_some() || ticks == 0 {
            manager.run_maintenance().expect("maintenance");
            ticks += 1;
            assert!(ticks < 1_000, "consolidation failed to converge");
        }
        ticks
    }

    #[test]
    fn graph_index_batches_pin_sources_and_hydrate_current_after_consolidation() {
        let manager = consolidating_manager(CONSOLIDATION_ENTITIES_PER_TICK);
        let ids = manager
            .bulk_insert(
                (1..=1024)
                    .map(|id| {
                        UnifiedEntity::graph_edge(
                            EntityId::new(id),
                            "step",
                            "2000",
                            "3000",
                            1.0,
                            HashMap::new(),
                        )
                    })
                    .collect(),
            )
            .expect("edges");
        manager.force_seal().expect("seal");
        for id in ids.iter().take(300) {
            manager.delete(*id).expect("fragment source");
        }
        let source = Arc::downgrade(&manager.sealed.read()[0]);
        let mut seen = Vec::new();
        let mut changed = false;
        let mut metadata_bytes = 0;
        assert!(manager.visit_graph_index_batches(GraphEntityKind::Edge, &[EntityId::new(2000)],
            || true,
            |bytes| { metadata_bytes += bytes; Ok::<_, ()>(()) },
            |batch| {
                assert!(batch.len() <= super::super::segment::SCAN_BATCH_SIZE);
                assert!(manager.growing.try_write().is_some(), "no growing topology guard");
                assert!(manager.sealed.try_write().is_some(), "no sealed topology guard");
                assert!(source.upgrade().expect("source pinned").try_write().is_some(), "no source guard");
                if !changed {
                    changed = true;
                    drain_maintenance(&manager);
                    assert!(manager.stats().consolidation.runs_completed > 0);
                    assert!(source.upgrade().is_some(), "retired cursor source remains pinned");
                    let mut updated = manager.get(EntityId::new(1024)).expect("future edge");
                    if let EntityKind::GraphEdge(edge) = &mut updated.kind { edge.weight = 2000; }
                    manager.update(updated).expect("update authoritative merged copy");
                }
                for id in batch {
                    let entity = manager.get(*id).expect("hydrate current manager");
                    if id.raw() == 1024 {
                        assert!(matches!(&entity.kind, EntityKind::GraphEdge(edge) if edge.weight == 2000));
                    }
                    seen.push(id.raw());
                }
                Ok(true)
            }).expect("cursor"));
        assert_eq!(seen, (301..=1024).collect::<Vec<_>>());
        assert!(
            source.upgrade().is_none(),
            "finished cursor releases retired source"
        );
        assert!(
            metadata_bytes < 4096,
            "fixed ID buffer plus source handles: {metadata_bytes}"
        );
    }

    #[test]
    fn graph_index_batches_propagate_admission_and_cancel_before_partial_delivery() {
        let manager = SegmentManager::new("graph");
        manager
            .bulk_insert(
                (1..=1024)
                    .map(|id| {
                        UnifiedEntity::graph_edge(
                            EntityId::new(id),
                            "step",
                            "2000",
                            "3000",
                            1.0,
                            HashMap::new(),
                        )
                    })
                    .collect(),
            )
            .expect("edges");
        let mut delivered = 0;
        let denied = manager.visit_graph_index_batches(
            GraphEntityKind::Edge,
            &[EntityId::new(2000)],
            || true,
            |_| Err("out of memory"),
            |ids| {
                delivered += ids.len();
                Ok(true)
            },
        );
        assert_eq!(denied, Err("out of memory"));
        assert_eq!(delivered, 0);
        let mut work = 0;
        assert!(!manager
            .visit_graph_index_batches(
                GraphEntityKind::Edge,
                &[EntityId::new(2000)],
                || {
                    work += 1;
                    work < 100
                },
                |_| Ok::<_, ()>(()),
                |ids| {
                    delivered += ids.len();
                    Ok(true)
                }
            )
            .expect("cancelled cursor"));
        assert_eq!(
            delivered, 0,
            "cancelled partial batch cannot reach consumer"
        );
        assert!(!manager
            .visit_graph_index_batches(
                GraphEntityKind::Edge,
                &[EntityId::new(2000)],
                || true,
                |_| Ok::<_, ()>(()),
                |ids| {
                    delivered += ids.len();
                    Ok(false)
                }
            )
            .expect("consumer stop"));
        assert_eq!(delivered, super::super::segment::SCAN_BATCH_SIZE);
    }

    #[test]
    fn retained_memory_tracks_two_readers_until_the_last_cursor_finishes() {
        let manager = consolidating_manager(CONSOLIDATION_ENTITIES_PER_TICK);
        let ids = seal_bulk_segment(&manager, 100);
        for id in ids.iter().take(40) {
            manager.delete(*id).expect("delete before scan");
        }
        let source_bytes = manager.resident_bytes();
        let source = Arc::downgrade(&manager.sealed.read()[0]);
        std::thread::scope(|scope| {
            let (ready, waiting) = std::sync::mpsc::channel();
            let (release_first, first_wait) = std::sync::mpsc::channel();
            let (release_last, last_wait) = std::sync::mpsc::channel();
            let reader = |resume: std::sync::mpsc::Receiver<()>,
                          ready: std::sync::mpsc::Sender<()>| {
                assert!(manager.scan_batches(
                    None,
                    || true,
                    |_| {
                        ready.send(()).expect("signal captured cursor");
                        let _ = resume.recv();
                        true
                    }
                ));
            };
            let ready_first = ready.clone();
            let first = scope.spawn(move || reader(first_wait, ready_first));
            let last = scope.spawn(move || reader(last_wait, ready));
            for _ in 0..2 {
                waiting
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("reader ready");
            }
            drain_maintenance(&manager);
            assert!(
                source.upgrade().is_some(),
                "both cursors pin the retired source"
            );
            let active_bytes = manager.sealed.read()[0].read().resident_bytes();
            let held = manager.resident_bytes();
            assert!(
                held >= active_bytes + source_bytes,
                "retired source remains resident"
            );
            release_first.send(()).expect("release first reader");
            first.join().expect("first reader");
            assert_eq!(
                manager.resident_bytes(),
                held,
                "one reader still pins the source"
            );
            release_last.send(()).expect("release last reader");
            last.join().expect("last reader");
            assert!(
                source.upgrade().is_none(),
                "accounting must not retain payloads"
            );
            assert_eq!(held - manager.resident_bytes(), source_bytes);
            assert_eq!(
                manager.stats().total_memory_bytes as u64,
                manager.resident_bytes()
            );
        });
    }

    #[test]
    fn retained_memory_includes_an_inflight_consolidation_copy() {
        let manager = consolidating_manager(1);
        let ids = seal_bulk_segment(&manager, 100);
        let source = Arc::downgrade(&manager.sealed.read()[0]);
        for id in ids.iter().take(40) {
            manager.delete(*id).expect("delete");
        }
        let before = manager.resident_bytes();
        assert!(manager.pressure_consolidation_tick());
        let copied = manager
            .consolidation
            .read()
            .as_ref()
            .expect("paced merge")
            .merged
            .resident_bytes();
        assert!(copied > 0);
        assert!(
            manager.resident_bytes() >= before + copied,
            "unpublished copy consumes memory"
        );
        drain_maintenance(&manager);
        assert!(
            source.upgrade().is_none(),
            "completed merge releases the old allocation, independently of directory estimates"
        );
        assert_eq!(manager.query_all(|_| true).len(), 60);
    }

    #[test]
    fn delete_heavy_workload_reclaims_tombstoned_memory_and_counts_it() {
        let manager = consolidating_manager(CONSOLIDATION_ENTITIES_PER_TICK);
        let ids = seal_bulk_segment(&manager, 100);
        let source = Arc::downgrade(&manager.sealed.read()[0]);

        let bytes_before_delete = manager.resident_bytes();

        // 40% tombstones — past CONSOLIDATION_TOMBSTONE_RATIO.
        for id in ids.iter().take(40) {
            assert!(manager.delete(*id).expect("delete"));
        }

        // Tombstones cost memory: the payload stays resident in flat storage
        // and the tombstone set grows.
        let bytes_with_tombstones = manager.resident_bytes();
        assert!(bytes_with_tombstones > bytes_before_delete);

        let survivors = live_ids(&manager);
        assert_eq!(survivors.len(), 60);

        drain_maintenance(&manager);

        let stats = manager.stats();
        assert_eq!(stats.consolidation.runs_started, 1);
        assert_eq!(stats.consolidation.runs_completed, 1);
        assert_eq!(stats.consolidation.segments_merged, 1);
        assert_eq!(stats.consolidation.tombstones_reclaimed, 40);
        assert!(source.upgrade().is_none(), "source storage is released");

        // This counter describes conservative source/destination estimates.
        // A flat source and hash destination can have different overhead;
        // the independent allocator regression verifies real reclamation.
        let bytes_after = manager.resident_bytes();
        assert_eq!(
            bytes_with_tombstones.saturating_sub(bytes_after),
            stats.consolidation.bytes_reclaimed
        );
        assert_eq!(stats.total_memory_bytes, bytes_after as usize);

        assert_eq!(live_ids(&manager), survivors);
        assert_eq!(manager.sealed.read().len(), 1);
        assert_eq!(manager.sealed.read()[0].read().tombstone_count(), 0);
    }

    #[test]
    fn query_results_are_identical_before_during_and_after_consolidation() {
        let manager = consolidating_manager(3);
        let ids = seal_bulk_segment(&manager, 20);
        seal_bulk_segment(&manager, 20);
        for id in ids.iter().take(10) {
            assert!(manager.delete(*id).expect("delete"));
        }

        let before = live_ids(&manager);
        let survivor = *ids.last().expect("at least one row");
        let tombstoned = ids[0];

        // Every mid-tick observation matches the pre-consolidation snapshot:
        // point reads, full scans, and the kind index (rebuilt by the merge).
        let mut mid_tick_observations = 0;
        while manager.consolidation.read().is_some() || mid_tick_observations == 0 {
            manager.run_maintenance().expect("maintenance");
            assert_eq!(live_ids(&manager), before);
            assert!(manager.get(survivor).is_some());
            assert!(manager.get(tombstoned).is_none());
            assert_eq!(manager.get_by_kind("table").len(), before.len());
            mid_tick_observations += 1;
            assert!(mid_tick_observations < 1_000);
        }

        assert!(
            mid_tick_observations > 2,
            "expected the run to span several ticks, saw {mid_tick_observations}"
        );
        assert_eq!(live_ids(&manager), before);
        assert!(manager.get(survivor).is_some());
        assert!(manager.get(tombstoned).is_none());
        assert_eq!(manager.get_by_kind("table").len(), before.len());
    }

    #[test]
    fn get_by_kind_reaches_sealed_segments() {
        let manager = consolidating_manager(CONSOLIDATION_ENTITIES_PER_TICK);
        let ids = seal_bulk_segment(&manager, 5);
        assert!(manager.delete(ids[0]).expect("delete"));

        // The sealed list is walked through the segment read guard; the
        // `Arc<RwLock<GrowingSegment>>` impl of `iter_kind` cannot hand out
        // references across the lock and yields nothing.
        assert_eq!(manager.get_by_kind("table").len(), 4);
        assert_eq!(manager.get_by_kind("vector").len(), 0);
    }

    #[test]
    fn consolidation_is_paced_by_the_per_tick_entity_bound() {
        const PER_TICK: usize = 4;
        let manager = consolidating_manager(PER_TICK);
        let ids = seal_bulk_segment(&manager, 60);
        for id in ids.iter().take(30) {
            assert!(manager.delete(*id).expect("delete"));
        }

        // First tick opens the run and copies exactly one bound's worth.
        manager.run_maintenance().expect("maintenance");
        assert_eq!(manager.consolidation_progress(), Some(PER_TICK as u64));

        // Each subsequent tick advances by no more than the bound, and the run
        // is still in flight — it never completes in one unbounded pass.
        let mut previous = PER_TICK as u64;
        let mut ticks = 1;
        while let Some(copied) = manager.consolidation_progress() {
            assert!(
                copied - previous <= PER_TICK as u64,
                "tick copied {} entities, bound is {PER_TICK}",
                copied - previous
            );
            previous = copied;
            manager.run_maintenance().expect("maintenance");
            ticks += 1;
            assert!(ticks < 100);
        }

        // 30 survivors at 4 per tick: the run cannot have finished before the
        // eighth tick.
        assert!(ticks >= 8, "consolidation completed in only {ticks} ticks");
        assert_eq!(manager.stats().consolidation.runs_completed, 1);
        assert_eq!(live_ids(&manager).len(), 30);
    }

    #[test]
    fn sealed_segment_count_above_the_limit_triggers_a_merge() {
        let manager = SegmentManager::with_config(
            "test",
            ManagerConfig {
                max_sealed_segments: 3,
                ..Default::default()
            },
        );
        for _ in 0..4 {
            seal_bulk_segment(&manager, 5);
        }
        assert_eq!(manager.sealed.read().len(), 4);

        let before = live_ids(&manager);
        drain_maintenance(&manager);

        // The knob finally does what it claimed: the sealed set collapses even
        // though not a single row was deleted.
        assert_eq!(manager.sealed.read().len(), 1);
        assert_eq!(manager.stats().consolidation.segments_merged, 4);
        assert_eq!(manager.stats().consolidation.tombstones_reclaimed, 0);
        assert_eq!(live_ids(&manager), before);
    }

    #[test]
    fn a_clean_sealed_set_below_the_limit_is_never_consolidated() {
        let manager = consolidating_manager(CONSOLIDATION_ENTITIES_PER_TICK);
        seal_bulk_segment(&manager, 10);
        seal_bulk_segment(&manager, 10);

        for _ in 0..5 {
            manager.run_maintenance().expect("maintenance");
        }

        assert_eq!(manager.stats().consolidation.runs_started, 0);
        assert_eq!(manager.sealed.read().len(), 2);
    }

    #[test]
    fn concurrent_readers_never_observe_a_missing_or_duplicated_entity() {
        use std::sync::atomic::AtomicBool;

        let manager = Arc::new(consolidating_manager(2));
        let ids = seal_bulk_segment(&manager, 40);
        seal_bulk_segment(&manager, 40);
        for id in ids.iter().take(20) {
            assert!(manager.delete(*id).expect("delete"));
        }

        let expected = live_ids(&manager);
        let stop = Arc::new(AtomicBool::new(false));

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let manager = Arc::clone(&manager);
                let stop = Arc::clone(&stop);
                let expected = expected.clone();
                std::thread::spawn(move || {
                    let mut scans = 0u64;
                    // Do-while: every reader scans at least once even when the
                    // drain finishes before this thread is first scheduled.
                    loop {
                        let mut seen: Vec<u64> = manager
                            .query_all(|_| true)
                            .into_iter()
                            .map(|entity| entity.id.raw())
                            .collect();
                        seen.sort_unstable();
                        // Sorting is not enough: a duplicated row would keep
                        // the set equal but change the length.
                        assert_eq!(seen, expected, "reader saw a torn sealed set");
                        scans += 1;
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    scans
                })
            })
            .collect();

        drain_maintenance(&manager);
        stop.store(true, Ordering::Relaxed);

        for reader in readers {
            let scans = reader.join().expect("reader thread");
            assert!(scans > 0, "reader never got to scan");
        }

        assert_eq!(manager.stats().consolidation.runs_completed, 1);
        assert_eq!(live_ids(&manager), expected);
    }

    #[test]
    fn a_row_deleted_mid_consolidation_is_not_resurrected_by_the_swap() {
        let manager = consolidating_manager(4);
        let ids = seal_bulk_segment(&manager, 20);
        for id in ids.iter().take(6) {
            assert!(manager.delete(*id).expect("delete"));
        }

        // Copy the first bound's worth, then delete a row we just copied.
        manager.run_maintenance().expect("maintenance");
        assert_eq!(manager.consolidation_progress(), Some(4));
        let doomed = ids[6];
        assert!(manager.delete(doomed).expect("delete"));

        let expected = live_ids(&manager);
        drain_maintenance(&manager);

        assert!(manager.get(doomed).is_none(), "deleted row came back");
        assert_eq!(live_ids(&manager), expected);
    }

    #[test]
    fn discarding_an_in_flight_consolidation_leaves_the_data_untouched() {
        let manager = consolidating_manager(3);
        let ids = seal_bulk_segment(&manager, 30);
        for id in ids.iter().take(12) {
            assert!(manager.delete(*id).expect("delete"));
        }

        let before = live_ids(&manager);
        let sealed_before: Vec<SegmentId> = manager
            .sealed
            .read()
            .iter()
            .map(|segment| segment.read().id())
            .collect();

        manager.run_maintenance().expect("maintenance");
        manager.run_maintenance().expect("maintenance");
        assert!(manager.consolidation.read().is_some());

        // A crash between ticks drops the half-built merged segment. Sealed
        // segments are the durable truth, so nothing was lost — and the run is
        // simply started over.
        *manager.consolidation.write() = None;

        assert_eq!(live_ids(&manager), before);
        let sealed_after: Vec<SegmentId> = manager
            .sealed
            .read()
            .iter()
            .map(|segment| segment.read().id())
            .collect();
        assert_eq!(sealed_after, sealed_before);
        assert_eq!(manager.stats().consolidation.runs_completed, 0);

        drain_maintenance(&manager);
        assert_eq!(live_ids(&manager), before);
        assert_eq!(manager.stats().consolidation.runs_completed, 1);
    }

    #[test]
    fn consolidation_preserves_entity_payload_metadata_and_sequence() {
        let manager = consolidating_manager(CONSOLIDATION_ENTITIES_PER_TICK);

        // Metadata must be attached before the seal: `set_metadata` on a sealed
        // row rewrites it into the growing segment, which would take it out of
        // the segment under test.
        let entities: Vec<UnifiedEntity> = (0..10)
            .map(|i| {
                UnifiedEntity::table_row(
                    manager.next_entity_id(),
                    "users",
                    i as u64,
                    vec![Value::Integer(i as i64)],
                )
            })
            .collect();
        let ids = manager.bulk_insert(entities).expect("bulk insert");

        let mut meta = Metadata::new();
        meta.set(
            "os",
            super::super::metadata::MetadataValue::String("linux".to_string()),
        );
        manager.set_metadata(ids[9], meta).expect("set metadata");
        manager.force_seal().expect("seal");

        assert!(manager.delete(ids[0]).expect("delete"));
        assert!(manager.delete(ids[1]).expect("delete"));
        assert!(manager.delete(ids[2]).expect("delete"));

        let before: Vec<UnifiedEntity> = {
            let mut entities = manager.query_all(|_| true);
            entities.sort_by_key(|entity| entity.id.raw());
            entities
        };

        drain_maintenance(&manager);

        let after: Vec<UnifiedEntity> = {
            let mut entities = manager.query_all(|_| true);
            entities.sort_by_key(|entity| entity.id.raw());
            entities
        };

        let columns = |entity: &UnifiedEntity| match &entity.data {
            super::super::entity::EntityData::Row(row) => row.columns.clone(),
            _ => Vec::new(),
        };

        assert_eq!(before.len(), after.len());
        for (old, new) in before.iter().zip(after.iter()) {
            assert_eq!(old.id, new.id);
            assert_eq!(columns(old), columns(new));
            assert_eq!(
                old.sequence_id, new.sequence_id,
                "merge must not re-issue sequence ids"
            );
        }
        assert!(manager
            .get_metadata(ids[9])
            .expect("metadata survives the merge")
            .has("os"));
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
