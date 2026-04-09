//! Vector Store
//!
//! Segment-based vector storage with HNSW indexing and metadata support.
//! Inspired by Chroma and Milvus architectures.
//!
//! # Architecture
//!
//! - **Collection**: A named set of vectors with a fixed dimension
//! - **Segment**: A unit of storage (Growing → Sealed → Flushed)
//! - **HNSW Index**: Built when segment is sealed
//! - **Metadata**: Per-vector key-value pairs with filtering
//!
//! # Segment Lifecycle
//!
//! 1. **Growing**: Accepts writes, no HNSW index (brute-force search)
//! 2. **Sealed**: Immutable, HNSW index is built
//! 3. **Flushed**: Written to disk (future)

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::distance::{distance_simd, DistanceMetric};
use super::hnsw::{HnswConfig, HnswIndex, NodeId};
use super::vector_metadata::{MetadataEntry, MetadataFilter, MetadataStore};

/// Unique identifier for a segment
pub type SegmentId = u64;

/// Unique identifier for a vector within a collection
pub type VectorId = u64;

/// Segment state in its lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentState {
    /// Accepts writes, linear search
    Growing,
    /// Immutable, HNSW indexed
    Sealed,
    /// Persisted to disk
    Flushed,
}

/// Configuration for vector segments
#[derive(Debug, Clone)]
pub struct SegmentConfig {
    /// Maximum vectors before auto-sealing
    pub max_vectors: usize,
    /// HNSW configuration
    pub hnsw_config: HnswConfig,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            max_vectors: 10_000,
            hnsw_config: HnswConfig::default(),
        }
    }
}

fn cmp_distance(a: f32, b: f32) -> Ordering {
    match a.partial_cmp(&b) {
        Some(order) => order,
        None => {
            if a.is_nan() && b.is_nan() {
                Ordering::Equal
            } else if a.is_nan() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
    }
}

/// A vector segment containing vectors, metadata, and optional index
pub struct VectorSegment {
    /// Segment ID
    pub id: SegmentId,
    /// Current state
    pub state: SegmentState,
    /// Vector dimension
    pub dimension: usize,
    /// Distance metric
    pub metric: DistanceMetric,
    /// Raw vector data: vector_id -> vector
    vectors: HashMap<VectorId, Vec<f32>>,
    /// Metadata store
    metadata: MetadataStore,
    /// HNSW index (built when sealed)
    hnsw_index: Option<HnswIndex>,
    /// ID mapping for HNSW: vector_id -> hnsw_node_id
    id_to_hnsw: HashMap<VectorId, NodeId>,
    /// Reverse mapping: hnsw_node_id -> vector_id
    hnsw_to_id: HashMap<NodeId, VectorId>,
    /// Creation timestamp
    pub created_at: u64,
    /// Last modified timestamp
    pub updated_at: u64,
}

impl VectorSegment {
    /// Create a new growing segment
    pub fn new(id: SegmentId, dimension: usize, metric: DistanceMetric) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            id,
            state: SegmentState::Growing,
            dimension,
            metric,
            vectors: HashMap::new(),
            metadata: MetadataStore::new(),
            hnsw_index: None,
            id_to_hnsw: HashMap::new(),
            hnsw_to_id: HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Get the number of vectors in this segment
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Check if segment is empty
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// Check if segment can accept writes
    pub fn can_write(&self) -> bool {
        self.state == SegmentState::Growing
    }

    /// Insert a vector with metadata
    pub fn insert(
        &mut self,
        id: VectorId,
        vector: Vec<f32>,
        metadata: MetadataEntry,
    ) -> Result<(), VectorStoreError> {
        if !self.can_write() {
            return Err(VectorStoreError::SegmentSealed);
        }

        if vector.len() != self.dimension {
            return Err(VectorStoreError::DimensionMismatch {
                expected: self.dimension,
                got: vector.len(),
            });
        }

        self.vectors.insert(id, vector);
        self.metadata.insert(id, metadata);
        self.update_timestamp();

        Ok(())
    }

    /// Get a vector by ID
    pub fn get_vector(&self, id: VectorId) -> Option<&Vec<f32>> {
        self.vectors.get(&id)
    }

    /// Get metadata for a vector
    pub fn get_metadata(&self, id: VectorId) -> Option<&MetadataEntry> {
        self.metadata.get(id)
    }

    /// Delete a vector (only in growing state)
    pub fn delete(&mut self, id: VectorId) -> Result<bool, VectorStoreError> {
        if !self.can_write() {
            return Err(VectorStoreError::SegmentSealed);
        }

        let existed = self.vectors.remove(&id).is_some();
        if existed {
            self.metadata.remove(id);
            self.update_timestamp();
        }

        Ok(existed)
    }

    /// Search for k nearest neighbors
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&MetadataFilter>,
    ) -> Vec<SearchResult> {
        if query.len() != self.dimension {
            return Vec::new();
        }

        match self.state {
            SegmentState::Growing => self.brute_force_search(query, k, filter),
            SegmentState::Sealed | SegmentState::Flushed => self.hnsw_search(query, k, filter),
        }
    }

    /// Seal the segment (build HNSW index)
    pub fn seal(&mut self, config: &HnswConfig) {
        if self.state != SegmentState::Growing {
            return;
        }

        // Build HNSW index
        let mut hnsw = HnswIndex::new(self.dimension, config.clone());

        for (&vector_id, vector) in &self.vectors {
            let hnsw_id = hnsw.insert(vector.clone());
            self.id_to_hnsw.insert(vector_id, hnsw_id);
            self.hnsw_to_id.insert(hnsw_id, vector_id);
        }

        self.hnsw_index = Some(hnsw);
        self.state = SegmentState::Sealed;
        self.update_timestamp();
    }

    // =========================================================================
    // Private methods
    // =========================================================================

    fn update_timestamp(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
    }

    fn brute_force_search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&MetadataFilter>,
    ) -> Vec<SearchResult> {
        // Get allowed IDs from filter
        let allowed: Option<HashSet<VectorId>> = filter.map(|f| self.metadata.filter(f));

        // Compute distances for all vectors
        let mut results: Vec<SearchResult> = self
            .vectors
            .iter()
            .filter(|(id, _)| allowed.as_ref().map(|a| a.contains(id)).unwrap_or(true))
            .map(|(&id, vector)| {
                let dist = distance_simd(query, vector, self.metric);
                SearchResult {
                    id,
                    distance: dist,
                    vector: Some(vector.clone()),
                    metadata: self.metadata.get(id).cloned(),
                }
            })
            .collect();

        // Sort by distance and take top k
        results.sort_by(|a, b| cmp_distance(a.distance, b.distance).then_with(|| a.id.cmp(&b.id)));
        results.truncate(k);

        results
    }

    fn hnsw_search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&MetadataFilter>,
    ) -> Vec<SearchResult> {
        let hnsw = match &self.hnsw_index {
            Some(h) => h,
            None => return self.brute_force_search(query, k, filter),
        };

        let hnsw_results = if let Some(f) = filter {
            // Get filtered vector IDs and map to HNSW IDs
            let allowed_vector_ids = self.metadata.filter(f);
            let allowed_hnsw_ids: HashSet<NodeId> = allowed_vector_ids
                .iter()
                .filter_map(|vid| self.id_to_hnsw.get(vid))
                .copied()
                .collect();

            hnsw.search_filtered(query, k, &allowed_hnsw_ids)
        } else {
            hnsw.search(query, k)
        };

        // Convert HNSW results to SearchResults
        hnsw_results
            .into_iter()
            .filter_map(|r| {
                let vector_id = self.hnsw_to_id.get(&r.id)?;
                Some(SearchResult {
                    id: *vector_id,
                    distance: r.distance,
                    vector: self.vectors.get(vector_id).cloned(),
                    metadata: self.metadata.get(*vector_id).cloned(),
                })
            })
            .collect()
    }
}

/// Result of a vector search
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Vector ID
    pub id: VectorId,
    /// Distance to query
    pub distance: f32,
    /// The vector (optional)
    pub vector: Option<Vec<f32>>,
    /// Metadata (optional)
    pub metadata: Option<MetadataEntry>,
}

/// Vector collection containing multiple segments
pub struct VectorCollection {
    /// Collection name
    pub name: String,
    /// Vector dimension
    pub dimension: usize,
    /// Distance metric
    pub metric: DistanceMetric,
    /// Configuration
    config: SegmentConfig,
    /// All segments
    segments: HashMap<SegmentId, VectorSegment>,
    /// Currently growing segment
    growing_segment: Option<SegmentId>,
    /// Next segment ID
    next_segment_id: AtomicU64,
    /// Next vector ID
    next_vector_id: AtomicU64,
    /// Vector ID to segment mapping
    vector_to_segment: HashMap<VectorId, SegmentId>,
}

impl VectorCollection {
    /// Create a new collection
    pub fn new(name: impl Into<String>, dimension: usize) -> Self {
        Self::with_config(name, dimension, SegmentConfig::default())
    }

    /// Create with custom configuration
    pub fn with_config(name: impl Into<String>, dimension: usize, config: SegmentConfig) -> Self {
        let metric = config.hnsw_config.metric;

        Self {
            name: name.into(),
            dimension,
            metric,
            config,
            segments: HashMap::new(),
            growing_segment: None,
            next_segment_id: AtomicU64::new(0),
            next_vector_id: AtomicU64::new(0),
            vector_to_segment: HashMap::new(),
        }
    }

    /// Set distance metric
    pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
        self.metric = metric;
        self.config.hnsw_config.metric = metric;
        self
    }

    /// Get total vector count across all segments
    pub fn len(&self) -> usize {
        self.segments.values().map(|s| s.len()).sum()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get segment count
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Insert a vector with optional metadata
    pub fn insert(
        &mut self,
        vector: Vec<f32>,
        metadata: Option<MetadataEntry>,
    ) -> Result<VectorId, VectorStoreError> {
        if vector.len() != self.dimension {
            return Err(VectorStoreError::DimensionMismatch {
                expected: self.dimension,
                got: vector.len(),
            });
        }

        // Get or create growing segment
        let segment_id = self.ensure_growing_segment();
        let segment = self.segments.get_mut(&segment_id).unwrap();

        let vector_id = self.next_vector_id.fetch_add(1, AtomicOrdering::SeqCst);
        segment.insert(vector_id, vector, metadata.unwrap_or_default())?;

        self.vector_to_segment.insert(vector_id, segment_id);

        // Auto-seal if segment is full
        if segment.len() >= self.config.max_vectors {
            self.seal_segment(segment_id);
        }

        Ok(vector_id)
    }

    /// Insert with a specific ID
    pub fn insert_with_id(
        &mut self,
        id: VectorId,
        vector: Vec<f32>,
        metadata: Option<MetadataEntry>,
    ) -> Result<(), VectorStoreError> {
        if vector.len() != self.dimension {
            return Err(VectorStoreError::DimensionMismatch {
                expected: self.dimension,
                got: vector.len(),
            });
        }

        let segment_id = self.ensure_growing_segment();
        let segment = self.segments.get_mut(&segment_id).unwrap();

        segment.insert(id, vector, metadata.unwrap_or_default())?;
        self.vector_to_segment.insert(id, segment_id);

        // Update next_id if necessary
        let current_next = self.next_vector_id.load(AtomicOrdering::SeqCst);
        if id >= current_next {
            self.next_vector_id.store(id + 1, AtomicOrdering::SeqCst);
        }

        // Auto-seal if segment is full
        if segment.len() >= self.config.max_vectors {
            self.seal_segment(segment_id);
        }

        Ok(())
    }

    /// Get a vector by ID
    pub fn get(&self, id: VectorId) -> Option<&Vec<f32>> {
        let segment_id = self.vector_to_segment.get(&id)?;
        self.segments.get(segment_id)?.get_vector(id)
    }

    /// Get metadata for a vector
    pub fn get_metadata(&self, id: VectorId) -> Option<&MetadataEntry> {
        let segment_id = self.vector_to_segment.get(&id)?;
        self.segments.get(segment_id)?.get_metadata(id)
    }

    /// Search for k nearest neighbors
    pub fn search(&self, query: &[f32], k: usize) -> Vec<SearchResult> {
        self.search_with_filter(query, k, None)
    }

    /// Search with metadata filter
    pub fn search_with_filter(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&MetadataFilter>,
    ) -> Vec<SearchResult> {
        if query.len() != self.dimension {
            return Vec::new();
        }

        // Search all segments
        let mut all_results: Vec<SearchResult> = Vec::new();
        for segment in self.segments.values() {
            let segment_results = segment.search(query, k, filter);
            all_results.extend(segment_results);
        }

        // Sort and take top k
        all_results
            .sort_by(|a, b| cmp_distance(a.distance, b.distance).then_with(|| a.id.cmp(&b.id)));
        all_results.truncate(k);

        all_results
    }

    /// Delete a vector by ID
    pub fn delete(&mut self, id: VectorId) -> Result<bool, VectorStoreError> {
        let segment_id = match self.vector_to_segment.get(&id) {
            Some(&sid) => sid,
            None => return Ok(false),
        };

        let segment = match self.segments.get_mut(&segment_id) {
            Some(s) => s,
            None => return Ok(false),
        };

        // Can only delete from growing segments
        if !segment.can_write() {
            return Err(VectorStoreError::SegmentSealed);
        }

        let deleted = segment.delete(id)?;
        if deleted {
            self.vector_to_segment.remove(&id);
        }

        Ok(deleted)
    }

    /// Seal the current growing segment
    pub fn seal_growing(&mut self) {
        if let Some(segment_id) = self.growing_segment.take() {
            self.seal_segment(segment_id);
        }
    }

    /// Force seal a specific segment
    fn seal_segment(&mut self, segment_id: SegmentId) {
        if let Some(segment) = self.segments.get_mut(&segment_id) {
            segment.seal(&self.config.hnsw_config);
        }
        if self.growing_segment == Some(segment_id) {
            self.growing_segment = None;
        }
    }

    /// Get or create a growing segment
    fn ensure_growing_segment(&mut self) -> SegmentId {
        if let Some(id) = self.growing_segment {
            return id;
        }

        let segment_id = self.next_segment_id.fetch_add(1, AtomicOrdering::SeqCst);
        let segment = VectorSegment::new(segment_id, self.dimension, self.metric);
        self.segments.insert(segment_id, segment);
        self.growing_segment = Some(segment_id);

        segment_id
    }
}

/// Vector store errors
#[derive(Debug, Clone)]
pub enum VectorStoreError {
    /// Dimension mismatch
    DimensionMismatch { expected: usize, got: usize },
    /// Segment is sealed and cannot accept writes
    SegmentSealed,
    /// Vector not found
    VectorNotFound(VectorId),
    /// Collection not found
    CollectionNotFound(String),
}

impl std::fmt::Display for VectorStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DimensionMismatch { expected, got } => {
                write!(f, "Dimension mismatch: expected {}, got {}", expected, got)
            }
            Self::SegmentSealed => write!(f, "Segment is sealed"),
            Self::VectorNotFound(id) => write!(f, "Vector not found: {}", id),
            Self::CollectionNotFound(name) => write!(f, "Collection not found: {}", name),
        }
    }
}

impl std::error::Error for VectorStoreError {}

/// Multi-collection vector store
pub struct VectorStore {
    /// Collections by name
    collections: HashMap<String, VectorCollection>,
}

impl VectorStore {
    /// Create a new vector store
    pub fn new() -> Self {
        Self {
            collections: HashMap::new(),
        }
    }

    /// Create a collection
    pub fn create_collection(
        &mut self,
        name: impl Into<String>,
        dimension: usize,
    ) -> &mut VectorCollection {
        let name = name.into();
        self.collections
            .entry(name.clone())
            .or_insert_with(|| VectorCollection::new(name.clone(), dimension))
    }

    /// Get a collection by name
    pub fn get(&self, name: &str) -> Option<&VectorCollection> {
        self.collections.get(name)
    }

    /// Get a mutable collection by name
    pub fn get_mut(&mut self, name: &str) -> Option<&mut VectorCollection> {
        self.collections.get_mut(name)
    }

    /// Drop a collection
    pub fn drop_collection(&mut self, name: &str) -> bool {
        self.collections.remove(name).is_some()
    }

    /// List all collection names
    pub fn list_collections(&self) -> Vec<&str> {
        self.collections.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for VectorStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::MetadataValue;

    fn random_vector(dim: usize, seed: u64) -> Vec<f32> {
        let mut state = seed;
        (0..dim)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state as f32) / (u64::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn test_collection_basic() {
        let mut collection = VectorCollection::new("test", 3);

        let id1 = collection.insert(vec![1.0, 0.0, 0.0], None).unwrap();
        let id2 = collection.insert(vec![0.0, 1.0, 0.0], None).unwrap();
        let id3 = collection.insert(vec![0.0, 0.0, 1.0], None).unwrap();

        assert_eq!(collection.len(), 3);
        assert!(collection.get(id1).is_some());
        assert!(collection.get(id2).is_some());
        assert!(collection.get(id3).is_some());
    }

    #[test]
    fn test_collection_search() {
        let mut collection = VectorCollection::new("test", 2);

        collection.insert(vec![0.0, 0.0], None).unwrap();
        collection.insert(vec![1.0, 0.0], None).unwrap();
        collection.insert(vec![2.0, 0.0], None).unwrap();
        collection.insert(vec![3.0, 0.0], None).unwrap();

        let results = collection.search(&[0.9, 0.0], 2);
        assert_eq!(results.len(), 2);
        // First result should be closest to query
        assert!(results[0].distance <= results[1].distance);
    }

    #[test]
    fn test_collection_search_with_filter() {
        let mut collection = VectorCollection::new("test", 2);

        for i in 0..10 {
            let mut metadata = MetadataEntry::new();
            metadata.insert("index", MetadataValue::Integer(i));
            metadata.insert("even", MetadataValue::Bool(i % 2 == 0));
            collection
                .insert(vec![i as f32, 0.0], Some(metadata))
                .unwrap();
        }

        // Search for even numbers only
        let filter = MetadataFilter::eq("even", true);
        let results = collection.search_with_filter(&[5.0, 0.0], 3, Some(&filter));

        assert_eq!(results.len(), 3);
        for result in &results {
            let meta = result.metadata.as_ref().unwrap();
            assert_eq!(meta.get("even"), Some(MetadataValue::Bool(true)));
        }
    }

    #[test]
    fn test_segment_seal() {
        let mut segment = VectorSegment::new(0, 3, DistanceMetric::L2);

        for i in 0..100 {
            segment
                .insert(i, random_vector(3, i), MetadataEntry::new())
                .unwrap();
        }

        assert!(segment.can_write());
        assert_eq!(segment.state, SegmentState::Growing);

        segment.seal(&HnswConfig::default());

        assert!(!segment.can_write());
        assert_eq!(segment.state, SegmentState::Sealed);
        assert!(segment.hnsw_index.is_some());

        // Search should still work
        let results = segment.search(&random_vector(3, 12345), 5, None);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_auto_seal() {
        let config = SegmentConfig {
            max_vectors: 10,
            hnsw_config: HnswConfig::default(),
        };
        let mut collection = VectorCollection::with_config("test", 3, config);

        // Insert more than max_vectors
        for i in 0..15 {
            collection.insert(random_vector(3, i), None).unwrap();
        }

        // Should have created 2 segments
        assert!(collection.segment_count() >= 1);

        // First segment should be sealed
        let sealed_count = collection
            .segments
            .values()
            .filter(|s| s.state == SegmentState::Sealed)
            .count();
        assert!(sealed_count >= 1);
    }

    #[test]
    fn test_vector_store() {
        let mut store = VectorStore::new();

        store.create_collection("hosts", 128);
        store.create_collection("vulnerabilities", 256);

        assert_eq!(store.list_collections().len(), 2);

        let hosts = store.get_mut("hosts").unwrap();
        hosts.insert(random_vector(128, 0), None).unwrap();
        hosts.insert(random_vector(128, 1), None).unwrap();

        assert_eq!(store.get("hosts").unwrap().len(), 2);
        assert_eq!(store.get("vulnerabilities").unwrap().len(), 0);

        store.drop_collection("vulnerabilities");
        assert_eq!(store.list_collections().len(), 1);
    }

    #[test]
    fn test_dimension_mismatch() {
        let mut collection = VectorCollection::new("test", 3);

        let result = collection.insert(vec![1.0, 2.0], None);
        assert!(matches!(
            result,
            Err(VectorStoreError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn test_search_handles_nan() {
        let mut collection = VectorCollection::new("test", 2);
        collection.insert(vec![0.0, 0.0], None).unwrap();
        collection.insert(vec![f32::NAN, 0.0], None).unwrap();

        let results = collection.search(&[0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_handles_nan_after_seal() {
        let mut collection = VectorCollection::new("test", 2);
        collection.insert(vec![0.0, 0.0], None).unwrap();
        collection.insert(vec![f32::NAN, 0.0], None).unwrap();

        collection.seal_growing();

        let results = collection.search(&[0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_delete() {
        let mut collection = VectorCollection::new("test", 3);

        let id1 = collection.insert(vec![1.0, 0.0, 0.0], None).unwrap();
        let id2 = collection.insert(vec![0.0, 1.0, 0.0], None).unwrap();

        assert_eq!(collection.len(), 2);

        collection.delete(id1).unwrap();
        assert_eq!(collection.len(), 1);
        assert!(collection.get(id1).is_none());
        assert!(collection.get(id2).is_some());
    }

    #[test]
    fn test_cosine_metric() {
        let mut collection = VectorCollection::new("test", 3).with_metric(DistanceMetric::Cosine);

        // Insert normalized vectors
        collection.insert(vec![1.0, 0.0, 0.0], None).unwrap();
        collection.insert(vec![0.0, 1.0, 0.0], None).unwrap();
        collection.insert(vec![0.707, 0.707, 0.0], None).unwrap();

        // Search for 45-degree vector
        let results = collection.search(&[0.707, 0.707, 0.0], 1);
        assert_eq!(results.len(), 1);
        assert!(results[0].distance < 0.01); // Should be very close
    }

    #[test]
    fn test_metadata_complex_filter() {
        let mut collection = VectorCollection::new("test", 2);

        for i in 0..20 {
            let mut metadata = MetadataEntry::new();
            metadata.insert("score", MetadataValue::Integer(i));
            metadata.insert(
                "type",
                MetadataValue::String(if i < 10 { "low" } else { "high" }.to_string()),
            );
            collection
                .insert(vec![i as f32, 0.0], Some(metadata))
                .unwrap();
        }

        // Find high-scoring entries above 15
        let filter = MetadataFilter::and(vec![
            MetadataFilter::eq("type", "high"),
            MetadataFilter::gt("score", MetadataValue::Integer(15)),
        ]);

        let results = collection.search_with_filter(&[17.0, 0.0], 5, Some(&filter));

        // Should find 16, 17, 18, 19
        assert!(results.len() <= 4);
        for result in &results {
            let meta = result.metadata.as_ref().unwrap();
            let score = match meta.get("score") {
                Some(MetadataValue::Integer(s)) => s,
                _ => panic!("Expected integer score"),
            };
            assert!(score > 15);
        }
    }
}
