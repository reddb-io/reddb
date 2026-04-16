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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::entity::{CrossRef, EntityData, EntityId, EntityKind, RefType, UnifiedEntity};
use super::memtable::Memtable;
use super::metadata::{Metadata, MetadataStorage};
use crate::storage::primitives::bloom::BloomFilter;
use crate::storage::query::value_compare::partial_compare_values;
use crate::storage::schema::{value_to_canonical_key, CanonicalKey, Value};

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

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

const SEALED_MULTI_ZONE_MAX_INTERVALS: usize = 4;

#[derive(Debug, Clone)]
struct UpdateIndexSnapshot {
    pk_column_name: Option<String>,
    pk_value: Option<Value>,
    pk_index_key: Option<(String, String)>,
    cross_refs: Vec<CrossRef>,
}

impl UpdateIndexSnapshot {
    fn from_entity(entity: &UnifiedEntity) -> Self {
        let (pk_column_name, pk_value) = match &entity.data {
            EntityData::Row(row) => (
                row.schema
                    .as_deref()
                    .and_then(|schema| schema.first().cloned()),
                row.columns.first().cloned(),
            ),
            _ => (None, None),
        };
        let pk_index_key = pk_value
            .as_ref()
            .map(|value| (entity.kind.collection().to_string(), format!("{:?}", value)));
        Self {
            pk_column_name,
            pk_value,
            pk_index_key,
            cross_refs: entity.cross_refs().to_vec(),
        }
    }
}

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

    /// HOT-update: like update but receives the set of field names that actually
    /// changed. Allows skipping index work when indexed columns are unaffected.
    /// Default: falls back to full update.
    fn update_hot(
        &mut self,
        entity: UnifiedEntity,
        modified_columns: &[String],
    ) -> Result<(), SegmentError> {
        let _ = modified_columns;
        self.update(entity)
    }

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

// ─────────────────────────────────────────────────────────────────────────────
// Zone map: per-column min/max for segment pruning
// ─────────────────────────────────────────────────────────────────────────────

/// Tracks the min and max observed `Value` for one column in a segment.
/// Used to skip segments that cannot satisfy a range or equality predicate.
#[derive(Debug, Clone)]
pub struct ColZone {
    pub min: Value,
    pub max: Value,
    min_key: Option<CanonicalKey>,
    max_key: Option<CanonicalKey>,
}

impl ColZone {
    fn new(v: Value) -> Self {
        Self {
            min_key: value_to_canonical_key(&v),
            max_key: value_to_canonical_key(&v),
            min: v.clone(),
            max: v,
        }
    }

    fn with_bounds(min: Value, max: Value) -> Self {
        Self {
            min_key: value_to_canonical_key(&min),
            max_key: value_to_canonical_key(&max),
            min,
            max,
        }
    }

    fn update(&mut self, v: &Value) {
        if compare_zone_values(v, None, &self.min, self.min_key.as_ref())
            .map(|o| o == std::cmp::Ordering::Less)
            .unwrap_or(false)
        {
            self.min = v.clone();
            self.min_key = value_to_canonical_key(v);
        }
        if compare_zone_values(v, None, &self.max, self.max_key.as_ref())
            .map(|o| o == std::cmp::Ordering::Greater)
            .unwrap_or(false)
        {
            self.max = v.clone();
            self.max_key = value_to_canonical_key(v);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MultiColZone {
    pub intervals: Vec<ColZone>,
}

impl MultiColZone {
    fn can_skip(&self, pred: &ZoneColPred<'_>) -> bool {
        !self.intervals.is_empty() && self.intervals.iter().all(|zone| pred.can_skip(zone))
    }
}

fn compare_zone_values(
    left: &Value,
    left_key: Option<&CanonicalKey>,
    right: &Value,
    right_key: Option<&CanonicalKey>,
) -> Option<std::cmp::Ordering> {
    partial_compare_values(left, right).or_else(|| {
        let left_key = left_key.cloned().or_else(|| value_to_canonical_key(left))?;
        let right_key = right_key
            .cloned()
            .or_else(|| value_to_canonical_key(right))?;
        (left_key.family() == right_key.family()).then(|| left_key.cmp(&right_key))
    })
}

/// Tag-only variant of `ZoneColPred` — used where the Value is stored separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneColPredKind {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
}

/// A predicate on a single column that can be checked against a `ColZone`.
#[derive(Debug, Clone)]
pub enum ZoneColPred<'a> {
    Eq(&'a Value),
    Gt(&'a Value),
    Gte(&'a Value),
    Lt(&'a Value),
    Lte(&'a Value),
}

impl<'a> ZoneColPred<'a> {
    /// Returns `true` when the entire segment can be skipped (no row can match).
    pub fn can_skip(&self, zone: &ColZone) -> bool {
        match self {
            // Equality: skip if val < min OR val > max
            ZoneColPred::Eq(val) => {
                compare_zone_values(val, None, &zone.min, zone.min_key.as_ref())
                    .map(|o| o == std::cmp::Ordering::Less)
                    .unwrap_or(false)
                    || compare_zone_values(val, None, &zone.max, zone.max_key.as_ref())
                        .map(|o| o == std::cmp::Ordering::Greater)
                        .unwrap_or(false)
            }
            // col > val: skip if max <= val (all rows have col ≤ val, none > val)
            ZoneColPred::Gt(val) => {
                compare_zone_values(&zone.max, zone.max_key.as_ref(), val, None)
                    .map(|o| o != std::cmp::Ordering::Greater)
                    .unwrap_or(false)
            }
            // col >= val: skip if max < val
            ZoneColPred::Gte(val) => {
                compare_zone_values(&zone.max, zone.max_key.as_ref(), val, None)
                    .map(|o| o == std::cmp::Ordering::Less)
                    .unwrap_or(false)
            }
            // col < val: skip if min >= val
            ZoneColPred::Lt(val) => {
                compare_zone_values(&zone.min, zone.min_key.as_ref(), val, None)
                    .map(|o| o != std::cmp::Ordering::Less)
                    .unwrap_or(false)
            }
            // col <= val: skip if min > val
            ZoneColPred::Lte(val) => {
                compare_zone_values(&zone.min, zone.min_key.as_ref(), val, None)
                    .map(|o| o == std::cmp::Ordering::Greater)
                    .unwrap_or(false)
            }
        }
    }
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

    /// Entity storage (HashMap for random access)
    entities: HashMap<EntityId, UnifiedEntity>,
    /// Flat entity storage for bulk inserts (no HashMap overhead, O(1) by offset)
    /// Used when entity IDs are sequential from base_entity_id
    flat_entities: Vec<UnifiedEntity>,
    /// Base entity ID for flat_entities (flat_entities[0].id == base_entity_id)
    base_entity_id: u64,
    /// Whether flat storage is active (bulk insert mode)
    use_flat: bool,
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

    /// Per-column zone maps: col_name → (min, max) for segment pruning
    col_zones: HashMap<String, ColZone>,
    /// Sealed-only minmax-multi summaries built from canonical ordering.
    sealed_col_zones: HashMap<String, MultiColZone>,

    /// Sequence counter for ordering
    sequence: AtomicU64,
    /// Approximate memory usage
    memory_bytes: AtomicU64,

    /// Epoch counter for lock-free reads of `flat_entities`.
    ///
    /// Updated with `Release` ordering after every flat-mode insert so that
    /// readers can safely access `flat_entities[0..published_flat_len]` by
    /// loading with `Acquire` ordering, without holding the segment RwLock.
    /// Only meaningful when `use_flat == true`; always 0 in HashMap mode.
    pub(crate) published_flat_len: AtomicUsize,
}

impl GrowingSegment {
    /// Direct iteration without Box<dyn> trait dispatch. Returns false to stop early.
    /// Uses concrete iterator types to avoid heap allocation per call.
    #[inline]
    pub fn for_each_fast<F>(&self, mut f: F) -> bool
    where
        F: FnMut(&UnifiedEntity) -> bool,
    {
        if self.use_flat {
            // Sequential Vec — best cache locality
            if self.deleted.is_empty() {
                for entity in &self.flat_entities {
                    if !f(entity) {
                        return false;
                    }
                }
            } else {
                for entity in &self.flat_entities {
                    if self.deleted.contains(&entity.id) {
                        continue;
                    }
                    if !f(entity) {
                        return false;
                    }
                }
            }
        } else {
            // HashMap values — random order, no boxing
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
        }
        true
    }

    /// Create a new growing segment
    pub fn new(id: SegmentId, collection: impl Into<String>) -> Self {
        let now = current_unix_secs();

        Self {
            id,
            collection: collection.into(),
            state: SegmentState::Growing,
            created_at: now,
            last_write_at: now,
            entities: HashMap::new(),
            flat_entities: Vec::new(),
            base_entity_id: 0,
            use_flat: false,
            deleted: HashSet::new(),
            metadata: MetadataStorage::new(),
            pk_index: BTreeMap::new(),
            kind_index: HashMap::new(),
            cross_ref_forward: HashMap::new(),
            cross_ref_reverse: HashMap::new(),
            bloom: BloomFilter::with_capacity(100_000, 0.01),
            memtable: Memtable::new(),
            col_zones: HashMap::new(),
            sealed_col_zones: HashMap::new(),
            sequence: AtomicU64::new(0),
            memory_bytes: AtomicU64::new(0),
            published_flat_len: AtomicUsize::new(0),
        }
    }

    /// Get next sequence number
    fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::SeqCst)
    }

    fn has_live_entity(&self, id: EntityId) -> bool {
        if self.deleted.contains(&id) {
            return false;
        }
        if self.use_flat {
            let raw = id.raw();
            if raw < self.base_entity_id {
                return false;
            }
            let idx = (raw - self.base_entity_id) as usize;
            self.flat_entities
                .get(idx)
                .is_some_and(|entity| entity.id == id)
        } else {
            self.entities.contains_key(&id)
        }
    }

    fn update_existing_entity_in_place(
        &mut self,
        entity: &UnifiedEntity,
    ) -> Result<UpdateIndexSnapshot, SegmentError> {
        if self.use_flat {
            let raw = entity.id.raw();
            if raw < self.base_entity_id {
                return Err(SegmentError::NotFound(entity.id));
            }
            let idx = (raw - self.base_entity_id) as usize;
            let Some(slot) = self.flat_entities.get_mut(idx) else {
                return Err(SegmentError::NotFound(entity.id));
            };
            if slot.id != entity.id {
                return Err(SegmentError::NotFound(entity.id));
            }
            let snapshot = UpdateIndexSnapshot::from_entity(slot);
            slot.clone_from(entity);
            Ok(snapshot)
        } else {
            let Some(slot) = self.entities.get_mut(&entity.id) else {
                return Err(SegmentError::NotFound(entity.id));
            };
            let snapshot = UpdateIndexSnapshot::from_entity(slot);
            slot.clone_from(entity);
            Ok(snapshot)
        }
    }

    fn apply_hot_update_with_metadata(
        &mut self,
        entity: &UnifiedEntity,
        modified_columns: &[String],
        metadata: Option<&Metadata>,
    ) -> Result<(), SegmentError> {
        let old = self.update_existing_entity_in_place(entity)?;
        self.reindex_for_update(&old, entity, Some(modified_columns));
        self.update_col_zones_from_entity(entity);
        if let Some(metadata) = metadata {
            self.metadata.set_all(entity.id, metadata);
        }
        Ok(())
    }

    fn apply_update_with_metadata(
        &mut self,
        entity: &UnifiedEntity,
        metadata: Option<&Metadata>,
    ) -> Result<(), SegmentError> {
        let old = self.update_existing_entity_in_place(entity)?;
        self.reindex_for_update(&old, entity, None);
        self.update_col_zones_from_entity(entity);
        if let Some(metadata) = metadata {
            self.metadata.set_all(entity.id, metadata);
        }
        Ok(())
    }

    pub fn update_hot_batch_with_metadata<'a, I>(&mut self, items: I) -> Result<(), SegmentError>
    where
        I: IntoIterator<Item = (&'a UnifiedEntity, &'a [String], Option<&'a Metadata>)>,
    {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        let items: Vec<(&UnifiedEntity, &[String], Option<&Metadata>)> =
            items.into_iter().collect();
        if items.is_empty() {
            return Ok(());
        }

        for (entity, _, _) in &items {
            if !self.has_live_entity(entity.id) {
                return Err(SegmentError::NotFound(entity.id));
            }
        }

        for (entity, modified_columns, metadata) in items {
            self.apply_hot_update_with_metadata(entity, modified_columns, metadata)?;
        }

        self.last_write_at = current_unix_secs();
        Ok(())
    }

    pub fn delete_batch(&mut self, ids: &[EntityId]) -> Result<Vec<EntityId>, SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut deleted_ids = Vec::with_capacity(ids.len());

        if self.use_flat {
            for &id in ids {
                let raw = id.raw();
                if raw < self.base_entity_id {
                    continue;
                }
                let idx = (raw - self.base_entity_id) as usize;
                if idx < self.flat_entities.len()
                    && self.flat_entities[idx].id == id
                    && !self.deleted.contains(&id)
                {
                    self.metadata.remove_all(id);
                    self.deleted.insert(id);
                    deleted_ids.push(id);
                }
            }
        } else {
            for &id in ids {
                if let Some(entity) = self.entities.remove(&id) {
                    self.unindex_entity(&entity);
                    self.metadata.remove_all(id);
                    self.deleted.insert(id);
                    deleted_ids.push(id);
                }
            }
        }

        if !deleted_ids.is_empty() {
            self.last_write_at = current_unix_secs();
        }

        Ok(deleted_ids)
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
        for emb in entity.embeddings() {
            size += emb.vector.len() * 4 + emb.name.len() + emb.model.len();
        }

        // Add cross-refs
        size += std::mem::size_of_val(entity.cross_refs());

        size
    }

    /// Update per-column zone maps from a newly inserted entity's fields.
    ///
    /// Handles both insert paths:
    /// - **Named** (`row.named`): individual inserts where fields are a `HashMap<String, Value>`
    /// - **Positional** (`row.columns` + `row.schema`): bulk-inserted entities stored as `Vec<Value>`
    ///   keyed by the shared schema. Previously this path was silently skipped, meaning zone maps
    ///   were always empty for bulk-loaded tables and segment pruning never fired.
    fn update_col_zones_from_entity(&mut self, entity: &UnifiedEntity) {
        if let EntityData::Row(row) = &entity.data {
            if let Some(named) = &row.named {
                // Individual insert path — HashMap fields
                for (col, val) in named {
                    if matches!(val, Value::Null) {
                        continue;
                    }
                    self.col_zones
                        .entry(col.clone())
                        .and_modify(|z| z.update(val))
                        .or_insert_with(|| ColZone::new(val.clone()));
                }
            } else if let Some(schema) = &row.schema {
                // Bulk-insert (columnar) path — positional Vec<Value> + shared schema.
                // Previously skipped: zone maps were always empty for bulk-loaded tables.
                for (col, val) in schema.iter().zip(row.columns.iter()) {
                    if matches!(val, Value::Null) {
                        continue;
                    }
                    self.col_zones
                        .entry(col.clone())
                        .and_modify(|z| z.update(val))
                        .or_insert_with(|| ColZone::new(val.clone()));
                }
            }
        }
    }

    fn rebuild_sealed_col_zones(&mut self) {
        let mut values_by_col: HashMap<String, Vec<(CanonicalKey, Value)>> = HashMap::new();
        let mut family_by_col: HashMap<String, crate::storage::schema::CanonicalKeyFamily> =
            HashMap::new();
        let mut mixed_family_cols = HashSet::new();
        let mut unsupported_cols = HashSet::new();

        let mut observe_row = |row: &super::entity::RowData| {
            for (col, value) in row.iter_fields() {
                if matches!(value, Value::Null) {
                    continue;
                }
                let Some(key) = value_to_canonical_key(value) else {
                    unsupported_cols.insert(col.to_string());
                    continue;
                };
                match family_by_col.get(col).copied() {
                    Some(existing) if existing != key.family() => {
                        mixed_family_cols.insert(col.to_string());
                    }
                    None => {
                        family_by_col.insert(col.to_string(), key.family());
                    }
                    _ => {}
                }
                values_by_col
                    .entry(col.to_string())
                    .or_default()
                    .push((key, value.clone()));
            }
        };

        if self.use_flat {
            for entity in &self.flat_entities {
                if self.deleted.contains(&entity.id) {
                    continue;
                }
                if let EntityData::Row(row) = &entity.data {
                    observe_row(row);
                }
            }
        } else {
            for entity in self.entities.values() {
                if self.deleted.contains(&entity.id) {
                    continue;
                }
                if let EntityData::Row(row) = &entity.data {
                    observe_row(row);
                }
            }
        }

        let mut sealed_col_zones = HashMap::new();
        for (col, mut entries) in values_by_col {
            if mixed_family_cols.contains(&col)
                || unsupported_cols.contains(&col)
                || entries.is_empty()
            {
                continue;
            }
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            entries.dedup_by(|left, right| left.0 == right.0);

            let intervals = build_minmax_multi_intervals(&entries, SEALED_MULTI_ZONE_MAX_INTERVALS);
            if intervals.len() > 1 {
                sealed_col_zones.insert(col, MultiColZone { intervals });
            }
        }

        self.sealed_col_zones = sealed_col_zones;
    }

    /// Returns `true` when this segment can be entirely skipped for the given predicates.
    /// A segment is skipped only if ALL predicates say so (conservative: any non-skippable
    /// predicate forces the scan to proceed).
    pub fn can_skip_zone_preds(&self, preds: &[(&str, ZoneColPred<'_>)]) -> bool {
        if preds.is_empty() {
            return false;
        }
        for (col, pred) in preds {
            if let Some(zone) = self.sealed_col_zones.get(*col) {
                if zone.can_skip(pred) {
                    return true;
                }
                continue;
            }
            if let Some(zone) = self.col_zones.get(*col) {
                if pred.can_skip(zone) {
                    return true; // ONE predicate suffices to skip
                }
            }
        }
        false
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
        for cross_ref in entity.cross_refs() {
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

    /// Selective re-index for updates.
    ///
    /// Skips index work that is not needed:
    /// - `kind_index`: entity kind is immutable, so remove+reinsert is always
    ///   a no-op; we skip it entirely and keep the existing entry.
    /// - `pk_index`: only updated when the primary-key column (first column of
    ///   a Row entity) actually changed. When `modified_columns` is provided,
    ///   we check membership; otherwise we compare old vs new pk value.
    /// - `bloom`: add-only by design, so we only insert the new pk when it
    ///   genuinely changes (old entry is a benign false positive).
    /// - `cross_ref`: only rebuilt when the refs actually differ.
    fn reindex_for_update(
        &mut self,
        old: &UpdateIndexSnapshot,
        new: &UnifiedEntity,
        modified_columns: Option<&[String]>,
    ) {
        // kind_index: kind is immutable — the existing entry is already correct.
        // No remove + reinsert needed.

        // bloom: entity ID never changes; already present from insert.

        // pk_index: only update when pk column is touched
        let pk_changed = match &new.data {
            EntityData::Row(new_row) => {
                if let Some(cols) = modified_columns {
                    // Caller told us exactly what changed — check if first schema column modified
                    // pk is the first column; check by name against the schema or by position 0
                    let pk_col_name = old.pk_column_name.as_deref().or_else(|| {
                        new_row
                            .schema
                            .as_deref()
                            .and_then(|schema| schema.first().map(|name| name.as_str()))
                    });
                    match pk_col_name {
                        Some(pk_name) => cols.iter().any(|c| c.eq_ignore_ascii_case(pk_name)),
                        // No schema — fall back to value comparison
                        None => old.pk_value.as_ref() != new_row.columns.first(),
                    }
                } else {
                    old.pk_value.as_ref() != new_row.columns.first()
                }
            }
            // Non-row types don't use pk_index
            _ => false,
        };

        if pk_changed {
            // Remove old pk entry
            if let Some((collection, pk_str)) = &old.pk_index_key {
                self.pk_index.remove(&(collection.clone(), pk_str.clone()));
            }
            // Insert new pk entry
            if let EntityData::Row(row) = &new.data {
                if let Some(first_col) = row.columns.first() {
                    let pk_str = format!("{:?}", first_col);
                    self.bloom.insert(pk_str.as_bytes());
                    self.pk_index
                        .insert((new.kind.collection().to_string(), pk_str), new.id);
                }
            }
        }

        // cross_ref: only rebuild when refs actually changed
        let new_refs = new.cross_refs();
        if old.cross_refs.as_slice() != new_refs {
            // Remove stale forward refs
            self.cross_ref_forward.remove(&new.id);
            // Prune stale entries from reverse index
            for cross_ref in &old.cross_refs {
                if let Some(rev) = self.cross_ref_reverse.get_mut(&cross_ref.target) {
                    rev.retain(|(src, _)| *src != new.id);
                }
            }
            // Add new refs
            for cross_ref in new_refs {
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
        let now = current_unix_secs();
        now.saturating_sub(self.created_at)
    }

    /// Get time since last write
    pub fn idle_secs(&self) -> u64 {
        let now = current_unix_secs();
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

        // Compute kind_key ONCE
        let kind_key = if let Some(first) = entities.first() {
            first.kind.storage_type().to_string()
        } else {
            return Ok(Vec::new());
        };

        let kind_set = self.kind_index.entry(kind_key).or_default();
        kind_set.reserve(n);

        let now = current_unix_secs();

        let base_seq = self.sequence.fetch_add(n as u64, Ordering::Relaxed);

        let mut ids = Vec::with_capacity(n);

        // Use flat storage (Vec) instead of HashMap — saves ~80 bytes/entity overhead
        if self.flat_entities.is_empty() && self.entities.is_empty() {
            // First bulk insert: initialize flat storage
            self.base_entity_id = entities.first().map(|e| e.id.raw()).unwrap_or(0);
            self.use_flat = true;
        }

        // Collect (col, value) pairs for zone updates BEFORE the mutable borrow
        // on `kind_set` is released (Rust can't split field borrows through `self`).
        // Use RowData::iter_fields() so both named and columnar rows feed pruning.
        let mut zone_updates: Vec<(String, Value)> = Vec::new();

        if self.use_flat {
            self.flat_entities.reserve(n);
            for (i, mut entity) in entities.into_iter().enumerate() {
                entity.sequence_id = base_seq + i as u64;
                let id = entity.id;
                kind_set.insert(id);
                ids.push(id);
                // Collect zone data from this entity
                if let EntityData::Row(row) = &entity.data {
                    for (col, val) in row.iter_fields() {
                        if !matches!(val, Value::Null) {
                            zone_updates.push((col.to_string(), val.clone()));
                        }
                    }
                }
                self.flat_entities.push(entity);
            }
        } else {
            // Fallback to HashMap for non-sequential inserts
            self.entities.reserve(n);
            let mut pairs = Vec::with_capacity(n);
            for (i, mut entity) in entities.into_iter().enumerate() {
                entity.sequence_id = base_seq + i as u64;
                let id = entity.id;
                kind_set.insert(id);
                ids.push(id);
                if let EntityData::Row(row) = &entity.data {
                    for (col, val) in row.iter_fields() {
                        if !matches!(val, Value::Null) {
                            zone_updates.push((col.to_string(), val.clone()));
                        }
                    }
                }
                pairs.push((id, entity));
            }
            self.entities.extend(pairs);
        }

        // Apply zone updates now that kind_set borrow is released
        let _ = kind_set;
        for (col, val) in zone_updates {
            self.col_zones
                .entry(col)
                .and_modify(|z| z.update(&val))
                .or_insert_with(|| ColZone::new(val));
        }

        self.last_write_at = now;

        // Publish the new flat length so lock-free readers can see the new entities.
        if self.use_flat {
            self.published_flat_len
                .store(self.flat_entities.len(), Ordering::Release);
        }

        Ok(ids)
    }

    /// Delete from this segment regardless of its seal state.
    /// Used to mutate sealed segments when DELETE touches bulk-inserted entities.
    pub(crate) fn force_delete(&mut self, id: EntityId) -> bool {
        if self.use_flat {
            let raw = id.raw();
            if raw >= self.base_entity_id {
                let idx = (raw - self.base_entity_id) as usize;
                if idx < self.flat_entities.len() && self.flat_entities[idx].id == id {
                    self.deleted.insert(id);
                    self.metadata.remove_all(id);
                    return true;
                }
            }
            return false;
        }

        if let Some(entity) = self.entities.remove(&id) {
            self.unindex_entity(&entity);
            self.metadata.remove_all(id);
            self.deleted.insert(id);
            true
        } else {
            false
        }
    }

    /// Update an entity in this segment regardless of its seal state.
    /// Used to mutate sealed segments when UPDATE touches bulk-inserted entities.
    pub(crate) fn force_update_with_metadata(
        &mut self,
        entity: &UnifiedEntity,
        modified_columns: &[String],
        metadata: Option<&Metadata>,
    ) -> Result<(), SegmentError> {
        self.apply_hot_update_with_metadata(entity, modified_columns, metadata)
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
                EntityKind::GraphNode(_) => stats.node_count += 1,
                EntityKind::GraphEdge(_) => stats.edge_count += 1,
                EntityKind::Vector { .. } => stats.vector_count += 1,
                EntityKind::TimeSeriesPoint(_) => stats.row_count += 1,
                EntityKind::QueueMessage { .. } => stats.row_count += 1,
            }
            stats.cross_ref_count += entity.cross_refs().len();
        }

        stats
    }

    fn entity_count(&self) -> usize {
        let total = if self.use_flat {
            self.flat_entities.len()
        } else {
            self.entities.len()
        };
        total.saturating_sub(self.deleted.len())
    }

    fn contains(&self, id: EntityId) -> bool {
        self.has_live_entity(id)
    }

    fn get(&self, id: EntityId) -> Option<&UnifiedEntity> {
        if self.deleted.contains(&id) {
            return None;
        }
        if self.use_flat {
            let raw = id.raw();
            if raw < self.base_entity_id {
                return None;
            }
            let idx = (raw - self.base_entity_id) as usize;
            self.flat_entities.get(idx).filter(|e| e.id == id)
        } else {
            self.entities.get(&id)
        }
    }

    fn get_mut(&mut self, id: EntityId) -> Option<&mut UnifiedEntity> {
        if self.deleted.contains(&id) || !self.state.is_writable() {
            return None;
        }
        if self.use_flat {
            let raw = id.raw();
            if raw < self.base_entity_id {
                return None;
            }
            let idx = (raw - self.base_entity_id) as usize;
            self.flat_entities.get_mut(idx).filter(|e| e.id == id)
        } else {
            self.entities.get_mut(&id)
        }
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

        // Update column zone maps for range-based segment pruning
        self.update_col_zones_from_entity(&entity);

        // Store
        let id = entity.id;
        self.entities.insert(id, entity);

        // Update write timestamp
        self.last_write_at = current_unix_secs();

        Ok(id)
    }

    fn update(&mut self, entity: UnifiedEntity) -> Result<(), SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        self.apply_update_with_metadata(&entity, None)?;
        self.last_write_at = current_unix_secs();

        Ok(())
    }

    fn update_hot(
        &mut self,
        entity: UnifiedEntity,
        modified_columns: &[String],
    ) -> Result<(), SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        self.apply_hot_update_with_metadata(&entity, modified_columns, None)?;
        self.last_write_at = current_unix_secs();
        Ok(())
    }

    fn delete(&mut self, id: EntityId) -> Result<bool, SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        // For flat storage, use tombstone (don't remove from Vec to keep indices valid)
        if self.use_flat {
            let raw = id.raw();
            if raw >= self.base_entity_id {
                let idx = (raw - self.base_entity_id) as usize;
                if idx < self.flat_entities.len() && self.flat_entities[idx].id == id {
                    self.deleted.insert(id);
                    return Ok(true);
                }
            }
            return Ok(false);
        }

        // Remove entity from HashMap
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

        // Mark as deleted (tombstone)
        self.deleted.insert(id);

        Ok(true)
    }

    fn get_metadata(&self, id: EntityId) -> Option<Metadata> {
        if !self.has_live_entity(id) {
            return None;
        }
        Some(self.metadata.get_all(id))
    }

    fn set_metadata(&mut self, id: EntityId, metadata: Metadata) -> Result<(), SegmentError> {
        if !self.state.is_writable() {
            return Err(SegmentError::NotWritable);
        }

        if !self.has_live_entity(id) {
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
        self.rebuild_sealed_col_zones();

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
        let base: Box<dyn Iterator<Item = &UnifiedEntity>> = if self.use_flat {
            Box::new(self.flat_entities.iter())
        } else {
            Box::new(self.entities.values())
        };
        if self.deleted.is_empty() {
            base
        } else {
            Box::new(base.filter(|e| !self.deleted.contains(&e.id)))
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

fn build_minmax_multi_intervals(
    entries: &[(CanonicalKey, Value)],
    max_intervals: usize,
) -> Vec<ColZone> {
    if entries.is_empty() {
        return Vec::new();
    }
    if entries.len() == 1 || max_intervals <= 1 {
        return vec![ColZone::with_bounds(
            entries[0].1.clone(),
            entries[entries.len() - 1].1.clone(),
        )];
    }

    let mut split_points = if entries.len() <= max_intervals {
        (1..entries.len()).collect::<Vec<_>>()
    } else {
        let target_splits = max_intervals - 1;
        let mut selected = select_gap_split_points(entries, target_splits);
        if selected.len() < target_splits {
            for bucket in 1..max_intervals {
                let idx = bucket * entries.len() / max_intervals;
                if idx == 0 || idx >= entries.len() || selected.contains(&idx) {
                    continue;
                }
                selected.push(idx);
                if selected.len() >= target_splits {
                    break;
                }
            }
        }
        selected.sort_unstable();
        selected.dedup();
        selected
    };

    split_points.push(entries.len());

    let mut out = Vec::with_capacity(split_points.len());
    let mut start = 0usize;
    for end in split_points {
        if end <= start {
            continue;
        }
        out.push(ColZone::with_bounds(
            entries[start].1.clone(),
            entries[end - 1].1.clone(),
        ));
        start = end;
    }

    if out.is_empty() {
        out.push(ColZone::with_bounds(
            entries[0].1.clone(),
            entries[entries.len() - 1].1.clone(),
        ));
    }

    out
}

fn select_gap_split_points(entries: &[(CanonicalKey, Value)], max_splits: usize) -> Vec<usize> {
    let mut gaps = Vec::new();
    for idx in 1..entries.len() {
        if let Some(score) = canonical_gap_score(&entries[idx - 1].0, &entries[idx].0) {
            if score > 0.0 {
                gaps.push((score, idx));
            }
        }
    }
    gaps.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.cmp(&right.1))
    });
    gaps.into_iter()
        .take(max_splits)
        .map(|(_, idx)| idx)
        .collect()
}

fn canonical_gap_score(left: &CanonicalKey, right: &CanonicalKey) -> Option<f64> {
    if left.family() != right.family() {
        return None;
    }
    match (left, right) {
        (CanonicalKey::Signed(_, l), CanonicalKey::Signed(_, r)) => {
            Some(r.saturating_sub(*l) as f64)
        }
        (CanonicalKey::Unsigned(_, l), CanonicalKey::Unsigned(_, r)) => {
            Some(r.saturating_sub(*l) as f64)
        }
        (CanonicalKey::Float(l), CanonicalKey::Float(r)) => {
            Some((f64::from_bits(*r) - f64::from_bits(*l)).abs())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Value;
    use crate::storage::unified::entity::RowData;
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

    #[test]
    fn test_zone_predicate_uses_canonical_fallback_for_email_values() {
        let mut zone = ColZone::new(Value::Email("bravo@example.com".to_string()));
        zone.update(&Value::Email("delta@example.com".to_string()));

        let probe = Value::Email("alpha@example.com".to_string());
        assert!(ZoneColPred::Eq(&probe).can_skip(&zone));

        let in_range = Value::Email("charlie@example.com".to_string());
        assert!(!ZoneColPred::Eq(&in_range).can_skip(&zone));
    }

    #[test]
    fn test_sealed_multi_zone_prunes_numeric_gap_outlier() {
        let mut segment = GrowingSegment::new(1, "test");

        for (row_id, age) in [(1_u64, 1_i64), (2, 2), (3, 3), (4, 1000)] {
            let entity = UnifiedEntity::new(
                EntityId::new(row_id),
                EntityKind::TableRow {
                    table: "users".into(),
                    row_id,
                },
                EntityData::Row(RowData::with_names(
                    vec![Value::Integer(age)],
                    vec!["age".to_string()],
                )),
            );
            segment.insert(entity).unwrap();
        }

        segment.seal().unwrap();

        let miss = Value::Integer(500);
        assert!(segment.can_skip_zone_preds(&[("age", ZoneColPred::Eq(&miss))]));

        let hit = Value::Integer(1000);
        assert!(!segment.can_skip_zone_preds(&[("age", ZoneColPred::Eq(&hit))]));
    }
}
