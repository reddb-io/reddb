use super::*;

impl IntegratedIndexManager {
    /// Create a new index manager with default config
    pub fn new() -> Self {
        Self::with_config(IntegratedIndexConfig::default())
    }

    /// Create with custom configuration
    pub fn with_config(config: IntegratedIndexConfig) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut status = HashMap::new();

        // Initialize status for each index type based on config
        if config.enable_hnsw {
            status.insert((IndexType::Hnsw, None), IndexStatus::Ready);
        } else {
            status.insert((IndexType::Hnsw, None), IndexStatus::Disabled);
        }
        if config.enable_fulltext {
            status.insert((IndexType::Fulltext, None), IndexStatus::Ready);
        } else {
            status.insert((IndexType::Fulltext, None), IndexStatus::Disabled);
        }
        if config.enable_metadata {
            status.insert((IndexType::Metadata, None), IndexStatus::Ready);
        } else {
            status.insert((IndexType::Metadata, None), IndexStatus::Disabled);
        }
        if config.enable_graph {
            status.insert((IndexType::Graph, None), IndexStatus::Ready);
        } else {
            status.insert((IndexType::Graph, None), IndexStatus::Disabled);
        }

        Self {
            config,
            text_index: InvertedIndex::new(),
            metadata_index: RwLock::new(MetadataStorage::new()),
            hnsw_indices: RwLock::new(HashMap::new()),
            graph_index: GraphAdjacencyIndex::new(),
            index_status: RwLock::new(status),
            event_history: RwLock::new(Vec::new()),
            created_at: now,
        }
    }

    /// Index a vector for similarity search
    pub fn index_vector(&self, collection: &str, id: EntityId, vector: &[f32]) {
        if !self.config.enable_hnsw {
            return;
        }

        {
            let mut indices = self.hnsw_indices.write();
            let info = indices
                .entry(collection.to_string())
                .or_insert_with(|| HnswIndexInfo {
                    dimension: vector.len(),
                    vectors: HashMap::new(),
                    entry_point: None,
                });

            // Verify dimension
            if info.dimension != vector.len() && !info.vectors.is_empty() {
                return; // Dimension mismatch
            }

            info.vectors.insert(id, vector.to_vec());
            if info.entry_point.is_none() {
                info.entry_point = Some(id);
            }
        }
    }

    /// Search for similar vectors
    pub fn search_similar(
        &self,
        collection: &str,
        query: &[f32],
        k: usize,
    ) -> Vec<VectorSearchResult> {
        let indices = self.hnsw_indices.read();

        let info = match indices.get(collection) {
            Some(i) => i,
            None => return Vec::new(),
        };

        if query.len() != info.dimension {
            return Vec::new();
        }

        // Simple brute-force for now (in production, use actual HNSW)
        let mut results: Vec<VectorSearchResult> = info
            .vectors
            .iter()
            .map(|(id, vec)| {
                let similarity = cosine_similarity(query, vec);
                VectorSearchResult {
                    entity_id: *id,
                    collection: collection.to_string(),
                    similarity,
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entity_id.cmp(&b.entity_id))
        });
        results.truncate(k);
        results
    }

    /// Index text content for full-text search
    pub fn index_text(&self, collection: &str, id: EntityId, field: &str, content: &str) {
        if !self.config.enable_fulltext {
            return;
        }
        self.text_index
            .index_document(collection, id, field, content);
    }

    /// Full-text search
    pub fn search_text(&self, query: &str, limit: usize) -> Vec<TextSearchResult> {
        self.text_index.search(query, limit)
    }

    /// Prefix search for autocomplete
    pub fn autocomplete(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.text_index.search_prefix(prefix, limit)
    }

    /// Index metadata for range queries
    pub fn index_metadata(&self, _collection: &str, id: EntityId, metadata: &Metadata) {
        if !self.config.enable_metadata {
            return;
        }
        // MetadataStorage handles this internally via set()
        {
            let mut storage = self.metadata_index.write();
            for (key, value) in &metadata.fields {
                storage.set(id, key.clone(), value.clone());
            }
        }
    }

    /// Query metadata with filters
    pub fn query_metadata(&self, key: &str, filter: MetadataQueryFilter) -> Vec<EntityId> {
        let storage = self.metadata_index.read();

        match filter {
            MetadataQueryFilter::Equals(ref value) => storage.filter_eq(key, value),
            MetadataQueryFilter::Range { min, max } => {
                // Handle int ranges
                let min_int = min.as_ref().and_then(|v| {
                    if let MetadataValue::Int(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                });
                let max_int = max.as_ref().and_then(|v| {
                    if let MetadataValue::Int(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                });
                if min_int.is_some() || max_int.is_some() {
                    return storage.filter_int_range(key, min_int, max_int);
                }
                Vec::new()
            }
            MetadataQueryFilter::Contains(ref substring) => {
                storage.filter_string_prefix(key, substring)
            }
            MetadataQueryFilter::In(ref values) => {
                // Collect entities matching any value
                let mut result = Vec::new();
                for value in values {
                    result.extend(storage.filter_eq(key, value));
                }
                result.sort();
                result.dedup();
                result
            }
        }
    }

    /// Remove entity from all indices
    pub fn remove_entity(&self, id: EntityId) {
        // Remove from text index
        self.text_index.remove_document(id);

        // Remove from vector indices
        {
            let mut indices = self.hnsw_indices.write();
            for info in indices.values_mut() {
                info.vectors.remove(&id);
            }
        }

        // Remove from graph index (if it's an edge)
        self.graph_index.remove_edge(id);

        // Metadata removal handled by storage layer
    }

    // =========================================================================
    // Graph Index Operations
    // =========================================================================

    /// Index an edge in the graph adjacency index
    pub fn index_edge(
        &self,
        edge_id: EntityId,
        source_id: EntityId,
        target_id: EntityId,
        label: &str,
        weight: f32,
    ) {
        if !self.config.enable_graph {
            return;
        }
        self.graph_index
            .index_edge(edge_id, source_id, target_id, label, weight);
    }

    /// Get neighbors of a node in a given direction
    pub fn get_neighbors(
        &self,
        node_id: EntityId,
        direction: EdgeDirection,
        label_filter: Option<&str>,
    ) -> Vec<AdjacencyEntry> {
        self.graph_index
            .get_neighbors(node_id, direction, label_filter)
    }

    /// Get all edges with a specific label
    pub fn get_edges_by_label(&self, label: &str) -> Vec<EntityId> {
        self.graph_index.get_edges_by_label(label)
    }

    /// Get degree of a node
    pub fn node_degree(&self, node_id: EntityId, direction: EdgeDirection) -> usize {
        match direction {
            EdgeDirection::Outgoing => self.graph_index.out_degree(node_id),
            EdgeDirection::Incoming => self.graph_index.in_degree(node_id),
            EdgeDirection::Both => self.graph_index.degree(node_id),
        }
    }

    /// Get a reference to the graph adjacency index
    pub fn graph_index(&self) -> &GraphAdjacencyIndex {
        &self.graph_index
    }

    // =========================================================================
    // Index Lifecycle Management
    // =========================================================================

    /// Create a new index of the specified type for a collection
    pub fn create_index(
        &self,
        index_type: IndexType,
        collection: Option<&str>,
    ) -> Result<(), String> {
        let key = (index_type, collection.map(|s| s.to_string()));

        // Check if already exists
        {
            let status = self.index_status.read();
            if let Some(IndexStatus::Ready) = status.get(&key) {
                return Err(format!("Index {:?} already exists", index_type));
            }
        }

        // Set status to building
        self.index_status.write().insert(key.clone(), IndexStatus::Building { progress: 0.0 });

        // For now, just mark as ready (actual building would be async)
        self.index_status.write().insert(key.clone(), IndexStatus::Ready);

        // Record event
        self.record_event(IndexEvent {
            index_type,
            collection: collection.map(|s| s.to_string()),
            event: IndexEventKind::Created,
            timestamp: Self::now(),
        });

        Ok(())
    }

    /// Drop an index
    pub fn drop_index(
        &self,
        index_type: IndexType,
        collection: Option<&str>,
    ) -> Result<(), String> {
        let key = (index_type, collection.map(|s| s.to_string()));

        // Clear the index data
        match index_type {
            IndexType::Hnsw => {
                if let Some(coll) = collection {
                    self.hnsw_indices.write().remove(coll);
                } else {
                    self.hnsw_indices.write().clear();
                }
            }
            IndexType::Graph => {
                self.graph_index.clear();
            }
            // Fulltext and Metadata don't support per-collection drop yet
            _ => {}
        }

        // Update status
        self.index_status.write().remove(&key);

        // Record event
        self.record_event(IndexEvent {
            index_type,
            collection: collection.map(|s| s.to_string()),
            event: IndexEventKind::Dropped,
            timestamp: Self::now(),
        });

        Ok(())
    }

    /// Rebuild an index (clear and recreate)
    pub fn rebuild_index(
        &self,
        index_type: IndexType,
        collection: Option<&str>,
    ) -> Result<(), String> {
        let key = (index_type, collection.map(|s| s.to_string()));

        // Set status to building
        self.index_status.write().insert(key.clone(), IndexStatus::Building { progress: 0.0 });

        // Clear existing data
        match index_type {
            IndexType::Hnsw => {
                if let Some(coll) = collection {
                    let mut indices = self.hnsw_indices.write();
                    if let Some(info) = indices.get_mut(coll) {
                        info.vectors.clear();
                        info.entry_point = None;
                    }
                }
            }
            IndexType::Graph => {
                self.graph_index.clear();
            }
            _ => {}
        }

        // Mark as ready (actual rebuild would re-index all entities)
        self.index_status.write().insert(key.clone(), IndexStatus::Ready);

        // Record event
        self.record_event(IndexEvent {
            index_type,
            collection: collection.map(|s| s.to_string()),
            event: IndexEventKind::Rebuilt,
            timestamp: Self::now(),
        });

        Ok(())
    }

    /// Get the status of an index
    pub fn index_status(&self, index_type: IndexType, collection: Option<&str>) -> IndexStatus {
        let key = (index_type, collection.map(|s| s.to_string()));
        self.index_status
            .read()
            .get(&key)
            .cloned()
            .unwrap_or(IndexStatus::Disabled)
    }

    /// Get all index statuses
    pub fn all_index_statuses(&self) -> HashMap<(IndexType, Option<String>), IndexStatus> {
        self.index_status.read().clone()
    }

    /// Get index event history
    pub fn event_history(&self) -> Vec<IndexEvent> {
        self.event_history.read().clone()
    }

    // =========================================================================
    // Statistics
    // =========================================================================

    /// Get index statistics
    pub fn stats(&self) -> IndexStats {
        let now = Self::now();

        let vector_count = self
            .hnsw_indices
            .read()
            .values()
            .map(|info| info.vectors.len())
            .sum();

        let (document_count, term_count) = {
            let i = self.text_index.index.read();
            let terms = i.len();
            let docs: HashSet<EntityId> = i
                .values()
                .flat_map(|postings| postings.iter().map(|p| p.entity_id))
                .collect();
            (docs.len(), terms)
        };

        IndexStats {
            vector_count,
            document_count,
            term_count,
            metadata_entries: 0, // Would require adding a count method to MetadataStorage
            graph_node_count: self.graph_index.node_count(),
            graph_edge_count: self.graph_index.edge_count(),
            created_at: self.created_at,
            updated_at: now,
        }
    }

    /// Get configuration
    pub fn config(&self) -> &IntegratedIndexConfig {
        &self.config
    }

    // =========================================================================
    // Internal Helpers
    // =========================================================================

    fn record_event(&self, event: IndexEvent) {
        let mut history = self.event_history.write();
        history.push(event);
        // Keep last 1000 events
        if history.len() > 1000 {
            history.drain(0..100);
        }
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}
