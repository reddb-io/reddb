//! Unified Segment System
//!
//! Implements the Growing → Sealed segment lifecycle pattern inspired by
//! Milvus and ChromaDB. Segments are the fundamental unit of storage
//! that handle entities of all types.
//!
//! # Lifecycle
//!
//! ```text
//! Growing (in-memory, accepts writes)
//!    ↓ seal() when full or manually triggered
//! Sealed (immutable, fully indexed)
//!    ↓ flush() for persistence
//! Flushed (on disk, can be mmap'd)
//!    ↓ archive() for cold storage
//! Archived (compressed, infrequently accessed)
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::entity::{CrossRef, EntityData, EntityId, EntityKind, RefType, UnifiedEntity};
use super::memtable::Memtable;
use super::metadata::{Metadata, MetadataStorage};
use crate::storage::primitives::bloom::BloomFilter;

/// Unique identifier for a segment
pub type SegmentId = u64;

/// Segment state in its lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentState {
    /// Accepts writes, partial/no index
    Growing,
    /// Transitioning, building indices
    Sealing,
    /// Immutable, fully indexed
    Sealed,
    /// Persisted to disk
    Flushed,
    /// Compressed, cold storage
    Archived,
}

impl SegmentState {
    /// Check if segment accepts writes
    pub fn is_writable(&self) -> bool {
        matches!(self, Self::Growing)
    }

    /// Check if segment is queryable
    pub fn is_queryable(&self) -> bool {
        !matches!(self, Self::Sealing)
    }

    /// Check if segment is immutable
    pub fn is_immutable(&self) -> bool {
        matches!(self, Self::Sealed | Self::Flushed | Self::Archived)
    }
}

/// Configuration for segments
#[derive(Debug, Clone)]
pub struct SegmentConfig {
    /// Maximum entities before auto-sealing
    pub max_entities: usize,
    /// Maximum memory bytes before auto-sealing
    pub max_bytes: usize,
    /// Maximum age in seconds before auto-sealing
    pub max_age_secs: u64,
    /// Enable vector indexing when sealed
    pub build_vector_index: bool,
    /// Enable graph indexing when sealed
    pub build_graph_index: bool,
    /// Compression level for archived segments (0-9)
    pub compression_level: u8,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            max_entities: 100_000,
            max_bytes: 256 * 1024 * 1024, // 256 MB
            max_age_secs: 3600,           // 1 hour
            build_vector_index: true,
            build_graph_index: true,
            compression_level: 6,
        }
    }
}

/// Segment statistics
#[derive(Debug, Clone, Default)]
pub struct SegmentStats {
    /// Number of entities
    pub entity_count: usize,
    /// Number of deleted entities
    pub deleted_count: usize,
    /// Approximate memory usage in bytes
    pub memory_bytes: usize,
    /// Number of vectors
    pub vector_count: usize,
    /// Number of graph nodes
    pub node_count: usize,
    /// Number of graph edges
    pub edge_count: usize,
    /// Number of table rows
    pub row_count: usize,
    /// Number of cross-references
    pub cross_ref_count: usize,
}

/// Segment error types
#[derive(Debug, Clone)]
pub enum SegmentError {
    /// Segment is not writable
    NotWritable,
    /// Entity not found
    NotFound(EntityId),
    /// Entity already exists
    AlreadyExists(EntityId),
    /// Segment is full
    Full,
    /// Invalid operation for current state
    InvalidState(SegmentState),
    /// Internal error
    Internal(String),
}

impl std::fmt::Display for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWritable => write!(f, "segment is not writable"),
            Self::NotFound(id) => write!(f, "entity not found: {}", id),
            Self::AlreadyExists(id) => write!(f, "entity already exists: {}", id),
            Self::Full => write!(f, "segment is full"),
            Self::InvalidState(state) => write!(f, "invalid operation for state: {:?}", state),
            Self::Internal(msg) => write!(f, "internal error: {}", msg),
        }
    }
}

impl std::error::Error for SegmentError {}

/// A unified segment that stores all entity types
pub trait UnifiedSegment: Send + Sync {
    /// Get segment ID
    fn id(&self) -> SegmentId;

    /// Get current state
    fn state(&self) -> SegmentState;

    /// Get collection/namespace name
    fn collection(&self) -> &str;

    /// Get statistics
    fn stats(&self) -> SegmentStats;

    /// O(1) live entity count (entities minus tombstones)
    fn entity_count(&self) -> usize;

    /// Check if entity exists
    fn contains(&self, id: EntityId) -> bool;

    /// Get an entity by ID
    fn get(&self, id: EntityId) -> Option<&UnifiedEntity>;

    /// Get mutable reference to entity
    fn get_mut(&mut self, id: EntityId) -> Option<&mut UnifiedEntity>;

    /// Insert a new entity
    fn insert(&mut self, entity: UnifiedEntity) -> Result<EntityId, SegmentError>;

    /// Update an existing entity
    fn update(&mut self, entity: UnifiedEntity) -> Result<(), SegmentError>;

    /// Delete an entity
    fn delete(&mut self, id: EntityId) -> Result<bool, SegmentError>;

    /// Get metadata for an entity
    fn get_metadata(&self, id: EntityId) -> Option<Metadata>;

    /// Set metadata for an entity
    fn set_metadata(&mut self, id: EntityId, metadata: Metadata) -> Result<(), SegmentError>;

    /// Seal the segment (make immutable)
    fn seal(&mut self) -> Result<(), SegmentError>;

    /// Check if should auto-seal based on config
    fn should_seal(&self, config: &SegmentConfig) -> bool;

    /// Iterate over all entities
    fn iter(&self) -> Box<dyn Iterator<Item = &UnifiedEntity> + '_>;

    /// Iterate over entities of a specific kind
    fn iter_kind(&self, kind_filter: &str) -> Box<dyn Iterator<Item = &UnifiedEntity> + '_>;

    /// Search entities by metadata filter
    fn filter_metadata(
        &self,
        filters: &[(String, super::metadata::MetadataFilter)],
    ) -> Vec<EntityId>;
}

/// Growing segment implementation (in-memory, writable)
pub struct GrowingSegment {
    /// Segment ID
    id: SegmentId,
    /// Collection/namespace name
    collection: String,
    /// Current state
    state: SegmentState,
    /// Creation timestamp
    created_at: u64,
    /// Last write timestamp
    last_write_at: u64,

    /// Entity storage
    entities: HashMap<EntityId, UnifiedEntity>,
    /// Deleted entity IDs (tombstones)
    deleted: HashSet<EntityId>,
    /// Metadata storage (type-aware)
    metadata: MetadataStorage,

    /// Primary key index: (collection, pk_value) → EntityId
    pk_index: BTreeMap<(String, String), EntityId>,
    /// Type index: kind → EntityIds
    kind_index: HashMap<String, HashSet<EntityId>>,
    /// Cross-reference index: source → Vec<(target, ref_type)>
    cross_ref_forward: HashMap<EntityId, Vec<(EntityId, RefType)>>,
    /// Reverse cross-reference index: target → Vec<(source, ref_type)>
    cross_ref_reverse: HashMap<EntityId, Vec<(EntityId, RefType)>>,

    /// Bloom filter for fast negative key lookups
    bloom: BloomFilter,

    /// Write buffer for absorbing write spikes (sorted by key)
    memtable: Memtable,

    /// Sequence counter for ordering
    sequence: AtomicU64,
    /// Approximate memory usage
    memory_bytes: AtomicU64,
}

impl GrowingSegment {
    /// Direct iteration without Box<dyn> trait dispatch. Returns false to stop early.
    #[inline]
    pub fn for_each_fast<F>(&self, mut f: F) -> bool
    where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        if self.deleted.is_empty() {
            for entity in self.entities.values() {
                if !f(entity) {
                    return false;
                }
            }
        } else {
            for entity in self.entities.values() {
                if self.deleted.contains(&entity.id) {
                    continue;
                }
                if !f(entity) {
                    return false;
                }
            }
        }
        true
    }

    /// Create a new growing segment
    pub fn new(id: SegmentId, collection: impl Into<String>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            id,
            collection: collection.into(),
            state: SegmentState::Growing,
            created_at: now,
            last_write_at: now,
            entities: HashMap::new(),
            deleted: HashSet::new(),
            metadata: MetadataStorage::new(),
            pk_index: BTreeMap::new(),
            kind_index: HashMap::new(),
            cross_ref_forward: HashMap::new(),
            cross_ref_reverse: HashMap::new(),
            bloom: BloomFilter::with_capacity(100_000, 0.01),
            memtable: Memtable::new(),
            sequence: AtomicU64::new(0),
            memory_bytes: AtomicU64::new(0),
        }
    }

    /// Get next sequence number
    fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::SeqCst)
    }

    /// Update memory estimate
    fn add_memory(&self, bytes: usize) {
        self.memory_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Estimate memory for an entity
    fn estimate_entity_size(entity: &UnifiedEntity) -> usize {
        let mut size = std::mem::size_of::<UnifiedEntity>();

        // Add data size
        size += match &entity.data {
            EntityData::Row(row) => row.columns.len() * 64, // Rough estimate
            EntityData::Node(node) => node.properties.len() * 128,
            EntityData::Edge(edge) => edge.properties.len() * 128,
            EntityData::Vector(vec) => {
                vec.dense.len() * 4 + vec.sparse.as_ref().map_or(0, |s| s.indices.len() * 8)
            }
            EntityData::TimeSeries(_) => 64,
            EntityData::QueueMessage(_) => 128,
        };

        // Add embeddings
        for emb in &entity.embeddings {
            size += emb.vector.len() * 4 + emb.name.len() + emb.model.len();
        }

        // Add cross-refs
        size += entity.cross_refs.len() * std::mem::size_of::<CrossRef>();

        size
    }

    /// Index an entity
    fn index_entity(&mut self, entity: &UnifiedEntity) {
        // Kind index
        let kind_key = entity.kind.storage_type().to_string();
        self.kind_index
            .entry(kind_key)
            .or_default()
            .insert(entity.id);

        // Bloom filter: insert entity ID bytes for fast negative lookups
        let id_bytes = entity.id.raw().to_le_bytes();
        self.bloom.insert(&id_bytes);

        // Primary key index (if applicable)
        if let EntityData::Row(row) = &entity.data {
            if let Some(first_col) = row.columns.first() {
                let pk_str = format!("{:?}", first_col);
                // Also add PK to bloom filter
                self.bloom.insert(pk_str.as_bytes());
                self.pk_index
                    .insert((entity.kind.collection().to_string(), pk_str), entity.id);
            }
        }

        // Cross-reference indices
        for cross_ref in &entity.cross_refs {
            self.cross_ref_forward
                .entry(cross_ref.source)
                .or_default()
                .push((cross_ref.target, cross_ref.ref_type));

            self.cross_ref_reverse
                .entry(cross_ref.target)
                .or_default()
                .push((cross_ref.source, cross_ref.ref_type));
        }
    }

    /// Check if an entity ID might exist in this segment via bloom filter.
    /// Returns `false` means *definitely not here*. `true` means *maybe here*.
    pub fn bloom_might_contain_id(&self, id: EntityId) -> bool {
        let id_bytes = id.raw().to_le_bytes();
        self.bloom.contains(&id_bytes)
    }

    /// Check if a primary key value might exist in this segment via bloom filter.
    pub fn bloom_might_contain_key(&self, key: &[u8]) -> bool {
        self.bloom.contains(key)
    }

    /// Get bloom filter statistics
    pub fn bloom_stats(&self) -> (f64, u32) {
        (self.bloom.fill_ratio(), self.bloom.count_set_bits())
    }

    /// Remove entity from indices
    fn unindex_entity(&mut self, entity: &UnifiedEntity) {
        // Kind index
        let kind_key = entity.kind.storage_type().to_string();
        if let Some(set) = self.kind_index.get_mut(&kind_key) {
            set.remove(&entity.id);
        }

        // Primary key index
        if let EntityData::Row(row) = &entity.data {
            if let Some(first_col) = row.columns.first() {
                let pk_str = format!("{:?}", first_col);
                self.pk_index
                    .remove(&(entity.kind.collection().to_string(), pk_str));
            }
        }

        // Cross-reference indices
        self.cross_ref_forward.remove(&entity.id);
        // Note: reverse refs from this entity still need cleanup
    }

    /// Get entities referencing the given entity
    pub fn get_references_to(&self, id: EntityId) -> Vec<(EntityId, RefType)> {
        self.cross_ref_reverse.get(&id).cloned().unwrap_or_default()
    }

    /// Get entities referenced by the given entity
    pub fn get_references_from(&self, id: EntityId) -> Vec<(EntityId, RefType)> {
        self.cross_ref_forward.get(&id).cloned().unwrap_or_default()
    }

    /// Get memtable statistics
    pub fn memtable_stats(&self) -> super::memtable::MemtableStats {
        self.memtable.stats()
    }

    /// Check if memtable should be flushed
    pub fn memtable_should_flush(&self) -> bool {
        self.memtable.should_flush()
    }

    /// Get age in seconds
    pub fn age_secs(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now.saturating_sub(self.created_at)
    }

    /// Get time since last write
    pub fn idle_secs(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now.saturating_sub(self.last_write_at)
    }

    /// Turbo bulk insert — minimal allocations per entity.
    ///
    /// Optimizations vs normal insert:
    /// - Skips bloom filter, memtable, cross-refs, memory tracking
    /// - Computes kind_key ONCE (not per entity)
    /// - Pre-allocates kind_index HashSet
    /// - Skips contains_key check (caller guarantees unique IDs)
    /// - Uses Relaxed ordering for sequence counter
    pub fn bulk_insert(
        &mut self,
        entities: Vec<UnifiedEntity>,
    ) -> Result<Vec<EntityId>, SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        let n = entities.len();
        self.entities.reserve(n);

        // Compute kind_key ONCE (all entities in a bulk are the same kind)
        let kind_key = if let Some(first) = entities.first() {
            first.kind.storage_type().to_string()
        } else {
            return Ok(Vec::new());
        };

        // Pre-allocate kind_index set
        let kind_set = self.kind_index.entry(kind_key).or_default();
        kind_set.reserve(n);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Base sequence for the batch — single atomic add
        let base_seq = self.sequence.fetch_add(n as u64, Ordering::Relaxed);

        // Collect IDs and batch-prepare entities
        let mut ids = Vec::with_capacity(n);
        let mut pairs = Vec::with_capacity(n);
        for (i, mut entity) in entities.into_iter().enumerate() {
            entity.sequence_id = base_seq + i as u64;
            let id = entity.id;
            kind_set.insert(id);
            ids.push(id);
            pairs.push((id, entity));
        }
        // Batch insert into HashMap (extend is faster than individual inserts)
        self.entities.extend(pairs);

        self.last_write_at = now;
        Ok(ids)
    }
}

impl UnifiedSegment for GrowingSegment {
    fn id(&self) -> SegmentId {
        self.id
    }

    fn state(&self) -> SegmentState {
        self.state
    }

    fn collection(&self) -> &str {
        &self.collection
    }

    fn stats(&self) -> SegmentStats {
        let mut stats = SegmentStats {
            entity_count: self.entities.len(),
            deleted_count: self.deleted.len(),
            memory_bytes: self.memory_bytes.load(Ordering::Relaxed) as usize,
            ..Default::default()
        };

        for entity in self.entities.values() {
            match &entity.kind {
                EntityKind::TableRow { .. } => stats.row_count += 1,
                EntityKind::GraphNode { .. } => stats.node_count += 1,
                EntityKind::GraphEdge { .. } => stats.edge_count += 1,
                EntityKind::Vector { .. } => stats.vector_count += 1,
                EntityKind::TimeSeriesPoint { .. } => stats.row_count += 1,
                EntityKind::QueueMessage { .. } => stats.row_count += 1,
            }
            stats.cross_ref_count += entity.cross_refs.len();
        }

        stats
    }

    fn entity_count(&self) -> usize {
        self.entities.len().saturating_sub(self.deleted.len())
    }

    fn contains(&self, id: EntityId) -> bool {
        self.entities.contains_key(&id) && !self.deleted.contains(&id)
    }

    fn get(&self, id: EntityId) -> Option<&UnifiedEntity> {
        if self.deleted.contains(&id) {
            return None;
        }
        self.entities.get(&id)
    }

    fn get_mut(&mut self, id: EntityId) -> Option<&mut UnifiedEntity> {
        if self.deleted.contains(&id) || !self.state.is_writable() {
            return None;
        }
        self.entities.get_mut(&id)
    }

    fn insert(&mut self, mut entity: UnifiedEntity) -> Result<EntityId, SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        if self.entities.contains_key(&entity.id) {
            return Err(SegmentError::AlreadyExists(entity.id));
        }

        // Assign sequence ID
        entity.sequence_id = self.next_sequence();

        // Estimate and track memory
        let size = Self::estimate_entity_size(&entity);
        self.add_memory(size);

        // Index the entity
        self.index_entity(&entity);

        // Write to memtable (write buffer for sorted flush)
        let id = entity.id;
        let key = id.raw().to_le_bytes();
        self.memtable.put(&key, &key); // key=entityId, value=entityId (pointer)

        // Store
        self.entities.insert(id, entity);

        // Update write timestamp
        self.last_write_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Ok(id)
    }

    fn update(&mut self, entity: UnifiedEntity) -> Result<(), SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        // Remove old entity (also unindexes)
        let old = self.entities.remove(&entity.id);
        if old.is_none() {
            return Err(SegmentError::NotFound(entity.id));
        }

        // Unindex old entity
        if let Some(ref old_entity) = old {
            self.unindex_entity(old_entity);
        }

        // Index new entity
        self.index_entity(&entity);

        // Insert new version
        self.entities.insert(entity.id, entity);

        self.last_write_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Ok(())
    }

    fn delete(&mut self, id: EntityId) -> Result<bool, SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        // Remove entity
        let entity = self.entities.remove(&id);
        if entity.is_none() {
            return Ok(false);
        }

        // Unindex
        if let Some(ref e) = entity {
            self.unindex_entity(e);
        }

        // Remove metadata
        self.metadata.remove_all(id);

        // Tombstone in memtable
        let key = id.raw().to_le_bytes();
        self.memtable.delete(&key);

        // Mark as deleted (tombstone)
        self.deleted.insert(id);

        Ok(true)
    }

    fn get_metadata(&self, id: EntityId) -> Option<Metadata> {
        if self.deleted.contains(&id) || !self.entities.contains_key(&id) {
            return None;
        }
        Some(self.metadata.get_all(id))
    }

    fn set_metadata(&mut self, id: EntityId, metadata: Metadata) -> Result<(), SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        if !self.entities.contains_key(&id) {
            return Err(SegmentError::NotFound(id));
        }

        self.metadata.set_all(id, &metadata);
        Ok(())
    }

    fn seal(&mut self) -> Result<(), SegmentError> {
        if self.state != SegmentState::Growing {
            return Err(SegmentError::InvalidState(self.state));
        }

        self.state = SegmentState::Sealing;

        // Flush memtable: drain sorted entries for potential B-tree bulk insert
        let memtable_stats = self.memtable.stats();
        if memtable_stats.entry_count > 0 {
            // The memtable entries are entity ID keys in sorted order.
            // This ordering enables efficient sequential I/O for persistence.
            self.memtable.clear();
        }

        // Build indices on the sealed data:
        // - Bloom filter is already populated from insert()
        // - HNSW/IVF for vectors (future)
        // - B-tree for sorted access (future)
        // - Inverted index for text search (future)

        self.state = SegmentState::Sealed;
        Ok(())
    }

    fn should_seal(&self, config: &SegmentConfig) -> bool {
        // Check entity count
        if self.entities.len() >= config.max_entities {
            return true;
        }

        // Check memory usage
        if self.memory_bytes.load(Ordering::Relaxed) as usize >= config.max_bytes {
            return true;
        }

        // Check age
        if self.age_secs() >= config.max_age_secs {
            return true;
        }

        false
    }

    fn iter(&self) -> Box<dyn Iterator<Item = &UnifiedEntity> + '_> {
        if self.deleted.is_empty() {
            Box::new(self.entities.values())
        } else {
            Box::new(
                self.entities
                    .values()
                    .filter(|e| !self.deleted.contains(&e.id)),
            )
        }
    }

    fn iter_kind(&self, kind_filter: &str) -> Box<dyn Iterator<Item = &UnifiedEntity> + '_> {
        let ids = self.kind_index.get(kind_filter).cloned();
        Box::new(self.entities.values().filter(move |e| {
            if self.deleted.contains(&e.id) {
                return false;
            }
            if let Some(ref ids) = ids {
                ids.contains(&e.id)
            } else {
                false
            }
        }))
    }

    fn filter_metadata(
        &self,
        filters: &[(String, super::metadata::MetadataFilter)],
    ) -> Vec<EntityId> {
        // For growing segments, we iterate and filter
        self.entities
            .keys()
            .filter(|id| {
                if self.deleted.contains(id) {
                    return false;
                }
                let metadata = self.metadata.get_all(**id);
                metadata.matches_all(filters)
            })
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Value;
    use crate::storage::unified::MetadataValue;

    #[test]
    fn test_growing_segment_basic() {
        let mut segment = GrowingSegment::new(1, "test");

        let entity = UnifiedEntity::table_row(
            EntityId::new(1),
            "users",
            1,
            vec![Value::Text("Alice".to_string())],
        );

        let id = segment.insert(entity).unwrap();
        assert_eq!(id, EntityId::new(1));
        assert!(segment.contains(id));

        let stats = segment.stats();
        assert_eq!(stats.entity_count, 1);
        assert_eq!(stats.row_count, 1);
    }

    #[test]
    fn test_segment_metadata() {
        let mut segment = GrowingSegment::new(1, "test");

        let entity = UnifiedEntity::table_row(
            EntityId::new(1),
            "users",
            1,
            vec![Value::Text("Alice".to_string())],
        );
        segment.insert(entity).unwrap();

        let mut meta = Metadata::new();
        meta.set("role", MetadataValue::String("admin".to_string()));
        meta.set("level", MetadataValue::Int(5));

        segment.set_metadata(EntityId::new(1), meta).unwrap();

        let retrieved = segment.get_metadata(EntityId::new(1)).unwrap();
        assert_eq!(
            retrieved.get("role"),
            Some(&MetadataValue::String("admin".to_string()))
        );
    }

    #[test]
    fn test_segment_seal() {
        let mut segment = GrowingSegment::new(1, "test");

        let entity = UnifiedEntity::vector(EntityId::new(1), "embeddings", vec![0.1, 0.2, 0.3]);
        segment.insert(entity).unwrap();

        // Can write before sealing
        assert!(segment.state().is_writable());

        // Seal the segment
        segment.seal().unwrap();
        assert_eq!(segment.state(), SegmentState::Sealed);

        // Cannot write after sealing
        let entity2 = UnifiedEntity::vector(EntityId::new(2), "embeddings", vec![0.4, 0.5, 0.6]);
        assert!(segment.insert(entity2).is_err());
    }

    #[test]
    fn test_should_seal() {
        let mut segment = GrowingSegment::new(1, "test");

        let config = SegmentConfig {
            max_entities: 2,
            ..Default::default()
        };

        assert!(!segment.should_seal(&config));

        segment
            .insert(UnifiedEntity::vector(EntityId::new(1), "v", vec![0.1]))
            .unwrap();
        assert!(!segment.should_seal(&config));

        segment
            .insert(UnifiedEntity::vector(EntityId::new(2), "v", vec![0.2]))
            .unwrap();
        assert!(segment.should_seal(&config));
    }

    #[test]
    fn test_cross_references() {
        let mut segment = GrowingSegment::new(1, "test");

        let mut entity1 = UnifiedEntity::table_row(
            EntityId::new(1),
            "hosts",
            1,
            vec![Value::Text("192.168.1.1".to_string())],
        );
        entity1.add_cross_ref(CrossRef::new(
            EntityId::new(1),
            EntityId::new(2),
            "nodes",
            RefType::RowToNode,
        ));
        segment.insert(entity1).unwrap();

        let refs_from = segment.get_references_from(EntityId::new(1));
        assert_eq!(refs_from.len(), 1);
        assert_eq!(refs_from[0], (EntityId::new(2), RefType::RowToNode));

        let refs_to = segment.get_references_to(EntityId::new(2));
        assert_eq!(refs_to.len(), 1);
        assert_eq!(refs_to[0], (EntityId::new(1), RefType::RowToNode));
    }
}
