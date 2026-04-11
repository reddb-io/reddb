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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use super::entity::{EntityId, UnifiedEntity};
use super::metadata::{Metadata, MetadataFilter};
use super::segment::{
    GrowingSegment, SegmentConfig, SegmentError, SegmentId, SegmentState, SegmentStats,
    UnifiedSegment,
};

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
    /// Currently active growing segment
    growing: RwLock<Option<Arc<RwLock<GrowingSegment>>>>,
    /// Sealed segments (immutable, queryable)
    sealed: RwLock<Vec<Arc<RwLock<GrowingSegment>>>>,
    /// Archived segment IDs (stored externally)
    archived: RwLock<Vec<SegmentId>>,
    /// Entity to segment mapping (for fast lookups)
    entity_segment: RwLock<HashMap<EntityId, SegmentId>>,
    /// Statistics
    stats: RwLock<ManagerStats>,
    /// Event listeners (simplified - would be channels in production)
    events: RwLock<Vec<LifecycleEvent>>,
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
            growing: RwLock::new(None),
            sealed: RwLock::new(Vec::new()),
            archived: RwLock::new(Vec::new()),
            entity_segment: RwLock::new(HashMap::new()),
            stats: RwLock::new(ManagerStats::default()),
            events: RwLock::new(Vec::new()),
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

    /// Get statistics
    pub fn stats(&self) -> ManagerStats {
        self.stats.read().unwrap().clone()
    }

    /// Generate a new entity ID
    pub fn next_entity_id(&self) -> EntityId {
        EntityId::new(self.next_entity_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Get or create the active growing segment
    fn get_or_create_growing(&self) -> Arc<RwLock<GrowingSegment>> {
        let mut growing = self.growing.write().unwrap();

        if growing.is_none() {
            let id = self.next_segment_id.fetch_add(1, Ordering::SeqCst);
            let segment = GrowingSegment::new(id, &self.collection);
            let segment_arc = Arc::new(RwLock::new(segment));
            *growing = Some(Arc::clone(&segment_arc));

            self.emit(LifecycleEvent::SegmentCreated(id));

            let mut stats = self.stats.write().unwrap();
            stats.growing_count += 1;
        }

        Arc::clone(growing.as_ref().unwrap())
    }

    /// Insert a new entity
    pub fn insert(&self, mut entity: UnifiedEntity) -> Result<EntityId, SegmentError> {
        // Check if we need to seal the current segment first
        self.maybe_seal_growing()?;

        let segment_arc = self.get_or_create_growing();
        let mut segment = segment_arc.write().unwrap();

        // Assign entity ID if not already set
        if entity.id.raw() == 0 {
            entity.id = self.next_entity_id();
        }

        let entity_id = entity.id;
        let segment_id = segment.id();

        segment.insert(entity)?;

        // Track entity location
        self.entity_segment
            .write()
            .unwrap()
            .insert(entity_id, segment_id);

        // Update stats
        {
            let mut stats = self.stats.write().unwrap();
            stats.total_entities += 1;
        }

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
        // Assign IDs
        for entity in &mut entities {
            if entity.id.raw() == 0 {
                entity.id = self.next_entity_id();
            }
        }

        let segment_arc = self.get_or_create_growing();
        let mut segment = segment_arc.write().unwrap();
        let segment_id = segment.id();

        // Single call to GrowingSegment.bulk_insert (one lock, no bloom/memtable)
        let ids = segment.bulk_insert(entities)?;

        // Batch update entity-segment mapping
        {
            let mut map = self.entity_segment.write().unwrap();
            map.reserve(ids.len());
            for &id in &ids {
                map.insert(id, segment_id);
            }
        }

        // Batch update stats
        {
            let mut stats = self.stats.write().unwrap();
            stats.total_entities += ids.len();
        }

        Ok(ids)
    }

    /// Get an entity by ID
    pub fn get(&self, id: EntityId) -> Option<UnifiedEntity> {
        // Check entity-segment mapping
        let segment_id = self.entity_segment.read().unwrap().get(&id).copied();

        if let Some(seg_id) = segment_id {
            // Try growing segment first
            if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
                let growing = growing_arc.read().unwrap();
                if growing.id() == seg_id {
                    return growing.get(id).cloned();
                }
            }

            // Try sealed segments
            let sealed = self.sealed.read().unwrap();
            for segment in sealed.iter() {
                if segment.id() == seg_id {
                    return segment.get(id).cloned();
                }
            }
        }

        // Fall back to scanning all segments
        self.scan_for_entity(id)
    }

    /// Scan all segments for an entity
    fn scan_for_entity(&self, id: EntityId) -> Option<UnifiedEntity> {
        // Check growing
        if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
            let growing = growing_arc.read().unwrap();
            if let Some(entity) = growing.get(id) {
                return Some(entity.clone());
            }
        }

        // Check sealed
        let sealed = self.sealed.read().unwrap();
        for segment in sealed.iter() {
            if let Some(entity) = segment.get(id) {
                return Some(entity.clone());
            }
        }

        None
    }

    /// Update an entity
    pub fn update(&self, entity: UnifiedEntity) -> Result<(), SegmentError> {
        let segment_id = self.entity_segment.read().unwrap().get(&entity.id).copied();

        if let Some(seg_id) = segment_id {
            // Try growing segment
            if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
                let mut growing = growing_arc.write().unwrap();
                if growing.id() == seg_id && growing.state().is_writable() {
                    return growing.update(entity);
                }
            }

            // For sealed segments, we need to delete + re-insert
            // This is a simplification; production would use MVCC
            return Err(SegmentError::NotWritable);
        }

        Err(SegmentError::NotFound(entity.id))
    }

    /// Delete an entity
    pub fn delete(&self, id: EntityId) -> Result<bool, SegmentError> {
        let segment_id = self.entity_segment.read().unwrap().get(&id).copied();

        if let Some(seg_id) = segment_id {
            // Try growing segment
            if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
                let mut growing = growing_arc.write().unwrap();
                if growing.id() == seg_id && growing.state().is_writable() {
                    let deleted = growing.delete(id)?;
                    if deleted {
                        self.entity_segment.write().unwrap().remove(&id);
                        {
                            let mut stats = self.stats.write().unwrap();
                            stats.total_entities = stats.total_entities.saturating_sub(1);
                        }
                        self.emit(LifecycleEvent::EntityDeleted(id, seg_id));
                    }
                    return Ok(deleted);
                }
            }

            // For sealed segments, add tombstone (not implemented here)
            return Err(SegmentError::NotWritable);
        }

        Ok(false)
    }

    /// Get metadata for an entity
    pub fn get_metadata(&self, id: EntityId) -> Option<Metadata> {
        let segment_id = self.entity_segment.read().unwrap().get(&id).copied();

        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
                let growing = growing_arc.read().unwrap();
                if growing.id() == seg_id {
                    return growing.get_metadata(id);
                }
            }

            let sealed = self.sealed.read().unwrap();
            for segment in sealed.iter() {
                if segment.id() == seg_id {
                    return segment.get_metadata(id);
                }
            }
        }

        None
    }

    /// Set metadata for an entity
    pub fn set_metadata(&self, id: EntityId, metadata: Metadata) -> Result<(), SegmentError> {
        let segment_id = self.entity_segment.read().unwrap().get(&id).copied();

        if let Some(seg_id) = segment_id {
            if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
                let mut growing = growing_arc.write().unwrap();
                if growing.id() == seg_id && growing.state().is_writable() {
                    return growing.set_metadata(id, metadata);
                }
            }

            return Err(SegmentError::NotWritable);
        }

        Err(SegmentError::NotFound(id))
    }

    /// Check if growing segment should be sealed
    fn maybe_seal_growing(&self) -> Result<(), SegmentError> {
        let should_seal = {
            let growing_opt = self.growing.read().unwrap();
            if let Some(growing_arc) = growing_opt.as_ref() {
                let growing = growing_arc.read().unwrap();
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
        let growing_opt = self.growing.write().unwrap().take();

        if let Some(growing_arc) = growing_opt {
            let mut growing = growing_arc.write().unwrap();
            let seg_id = growing.id();

            // Seal it
            growing.seal()?;

            // Move to sealed list (we need to extract it from the Arc)
            drop(growing); // Release write lock

            // In a real implementation, we'd convert to SealedSegment here
            // For now, we keep it as-is since GrowingSegment implements UnifiedSegment
            self.sealed.write().unwrap().push(growing_arc);

            // Update stats
            {
                let mut stats = self.stats.write().unwrap();
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
        let has_growing = self.growing.read().unwrap().is_some();
        if has_growing {
            Ok(Some(self.seal_current()?))
        } else {
            Ok(None)
        }
    }

    /// Query across all segments. Uses parallel scanning for sealed segments
    /// when more than one sealed segment exists.
    pub fn query_all<F>(&self, filter: F) -> Vec<UnifiedEntity>
    where
        F: Fn(&UnifiedEntity) -> bool + Sync,
    {
        let mut results = Vec::new();

        // Query growing segment (single, in-memory, fast)
        if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
            let growing = growing_arc.read().unwrap();
            results.extend(growing.iter().filter(|e| filter(e)).cloned());
        }

        // Query sealed segments — parallel when multiple exist AND multi-core
        let sealed = self.sealed.read().unwrap();
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
                                .unwrap()
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
                let seg = segment.read().unwrap();
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

        if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
            let growing = growing_arc.read().unwrap();
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
        let sealed = self.sealed.read().unwrap();
        for segment_arc in sealed.iter() {
            let segment = segment_arc.read().unwrap();
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
        if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
            let growing = growing_arc.read().unwrap();
            results.extend(growing.filter_metadata(filters));
        }

        // Sealed segments
        let sealed = self.sealed.read().unwrap();
        for segment in sealed.iter() {
            results.extend(segment.filter_metadata(filters));
        }

        results
    }

    /// Get entities by kind
    pub fn get_by_kind(&self, kind: &str) -> Vec<UnifiedEntity> {
        let mut results = Vec::new();

        // Growing segment
        if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
            let growing = growing_arc.read().unwrap();
            for entity in growing.iter_kind(kind) {
                results.push(entity.clone());
            }
        }

        // Sealed segments
        let sealed = self.sealed.read().unwrap();
        for segment in sealed.iter() {
            for entity in segment.iter_kind(kind) {
                results.push(entity.clone());
            }
        }

        results
    }

    /// Count entities
    pub fn count(&self) -> usize {
        self.stats.read().unwrap().total_entities
    }

    /// Get all segment IDs
    pub fn segment_ids(&self) -> Vec<SegmentId> {
        let mut ids = Vec::new();

        if let Some(growing_arc) = self.growing.read().unwrap().as_ref() {
            ids.push(growing_arc.read().unwrap().id());
        }

        let sealed = self.sealed.read().unwrap();
        for segment in sealed.iter() {
            ids.push(segment.id());
        }

        ids.extend(self.archived.read().unwrap().iter().copied());

        ids
    }

    /// Emit a lifecycle event
    fn emit(&self, event: LifecycleEvent) {
        self.events.write().unwrap().push(event);
    }

    /// Drain events (for testing/monitoring)
    pub fn drain_events(&self) -> Vec<LifecycleEvent> {
        std::mem::take(&mut *self.events.write().unwrap())
    }

    /// Run maintenance (would be called periodically in production)
    pub fn run_maintenance(&self) -> Result<(), SegmentError> {
        // Auto-seal idle segments
        self.maybe_seal_growing()?;

        // Compact if too many sealed segments
        if self.config.enable_compaction {
            let sealed_count = self.sealed.read().unwrap().len();
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
        self.read().unwrap().id()
    }

    fn state(&self) -> SegmentState {
        self.read().unwrap().state()
    }

    fn collection(&self) -> &str {
        // This is a limitation - we'd need to store collection in the Arc wrapper
        "unknown"
    }

    fn stats(&self) -> SegmentStats {
        self.read().unwrap().stats()
    }

    fn contains(&self, id: EntityId) -> bool {
        self.read().unwrap().contains(id)
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
        self.write().unwrap().insert(entity)
    }

    fn update(&mut self, entity: UnifiedEntity) -> Result<(), SegmentError> {
        self.write().unwrap().update(entity)
    }

    fn delete(&mut self, id: EntityId) -> Result<bool, SegmentError> {
        self.write().unwrap().delete(id)
    }

    fn get_metadata(&self, id: EntityId) -> Option<Metadata> {
        self.read().unwrap().get_metadata(id)
    }

    fn set_metadata(&mut self, id: EntityId, metadata: Metadata) -> Result<(), SegmentError> {
        self.write().unwrap().set_metadata(id, metadata)
    }

    fn seal(&mut self) -> Result<(), SegmentError> {
        self.write().unwrap().seal()
    }

    fn should_seal(&self, config: &SegmentConfig) -> bool {
        self.read().unwrap().should_seal(config)
    }

    fn iter(&self) -> Box<dyn Iterator<Item = &UnifiedEntity> + '_> {
        // Cannot return iterator with RwLock
        Box::new(std::iter::empty())
    }

    fn iter_kind(&self, _kind_filter: &str) -> Box<dyn Iterator<Item = &UnifiedEntity> + '_> {
        Box::new(std::iter::empty())
    }

    fn filter_metadata(&self, filters: &[(String, MetadataFilter)]) -> Vec<EntityId> {
        self.read().unwrap().filter_metadata(filters)
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
            vec![Value::Text("Alice".to_string())],
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
                vec![Value::Text("192.168.1.1".to_string())],
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
