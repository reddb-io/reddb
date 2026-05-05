//! Unified Store Adapter for RAG Engine
//!
//! Bridges the unified RedDB store with the existing RAG retrieval infrastructure,
//! enabling queries that seamlessly combine tables, graphs, and vectors.

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;
use crate::storage::schema::Value;
use crate::storage::{CrossRef, EntityData, EntityId, EntityKind, RefType, Store, UnifiedEntity};

use super::context::{ChunkSource, ContextChunk, RetrievalContext};
use super::RagConfig;

/// Result from a unified multi-modal query
#[derive(Debug, Clone)]
pub struct UnifiedQueryResult {
    /// Matched entities (rows, nodes, edges, vectors)
    pub entities: Vec<MatchedEntity>,
    /// Query statistics
    pub stats: UnifiedQueryStats,
}

impl UnifiedQueryResult {
    pub fn new() -> Self {
        Self {
            entities: Vec::new(),
            stats: UnifiedQueryStats::default(),
        }
    }

    pub fn push(&mut self, entity: MatchedEntity) {
        self.entities.push(entity);
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

impl Default for UnifiedQueryResult {
    fn default() -> Self {
        Self::new()
    }
}

/// A matched entity with relevance score and source information
#[derive(Debug, Clone)]
pub struct MatchedEntity {
    /// The entity itself
    pub entity: UnifiedEntity,
    /// Relevance score (0.0 - 1.0)
    pub score: f32,
    /// Source of the match
    pub source: MatchSource,
    /// Cross-references followed to reach this entity
    pub via_refs: Vec<CrossRef>,
}

impl MatchedEntity {
    pub fn new(entity: UnifiedEntity, score: f32, source: MatchSource) -> Self {
        Self {
            entity,
            score,
            source,
            via_refs: Vec::new(),
        }
    }

    pub fn with_refs(mut self, refs: Vec<CrossRef>) -> Self {
        self.via_refs = refs;
        self
    }
}

/// Source of a match in unified query
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchSource {
    /// Direct vector similarity search
    VectorSimilarity,
    /// Graph pattern match
    GraphPattern,
    /// Table filter match
    TableFilter,
    /// Cross-reference expansion
    CrossReference,
    /// Hybrid scoring
    Hybrid,
}

/// Statistics for unified query execution
#[derive(Debug, Clone, Default)]
pub struct UnifiedQueryStats {
    /// Number of vector comparisons
    pub vector_comparisons: usize,
    /// Number of graph patterns checked
    pub graph_patterns_checked: usize,
    /// Number of table rows scanned
    pub table_rows_scanned: usize,
    /// Number of cross-refs followed
    pub cross_refs_followed: usize,
    /// Execution time in microseconds
    pub execution_time_us: u64,
}

/// Adapter that connects the store to RAG queries
pub struct UnifiedStoreAdapter {
    /// The store
    store: Arc<Store>,
}

impl UnifiedStoreAdapter {
    /// Create a new adapter for the given store
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Search vectors across all collections
    pub fn vector_search(
        &self,
        query_vector: &[f32],
        collections: Option<&[&str]>,
        k: usize,
        _metadata_filter: Option<MetadataQuery>,
    ) -> Result<UnifiedQueryResult, ExecutionError> {
        let start = std::time::Instant::now();
        let mut result = UnifiedQueryResult::new();

        // Get all collections if not specified
        let collection_names: Vec<String> = if let Some(cols) = collections {
            cols.iter().map(|s| s.to_string()).collect()
        } else {
            self.store.list_collections()
        };

        // Search each collection using query_all
        for col_name in &collection_names {
            let manager = match self.store.get_collection(col_name) {
                Some(m) => m,
                None => continue,
            };

            // Use query_all to scan entities
            let entities = manager.query_all(|_| true);
            for entity in entities {
                // Check if it's a vector entity
                if let EntityData::Vector(ref vec_data) = entity.data {
                    let similarity = cosine_similarity(query_vector, &vec_data.dense);
                    if similarity > 0.0 {
                        result.push(MatchedEntity::new(
                            entity.clone(),
                            similarity,
                            MatchSource::VectorSimilarity,
                        ));
                        result.stats.vector_comparisons += 1;
                    }
                }

                // Also check embeddings in any entity type
                for slot in entity.embeddings() {
                    let similarity = cosine_similarity(query_vector, &slot.vector);
                    if similarity > 0.5 {
                        result.push(MatchedEntity::new(
                            entity.clone(),
                            similarity,
                            MatchSource::VectorSimilarity,
                        ));
                        result.stats.vector_comparisons += 1;
                    }
                }
            }
        }

        // Sort by score and take top k
        result.entities.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entity.id.cmp(&b.entity.id))
        });
        result.entities.truncate(k);

        result.stats.execution_time_us = start.elapsed().as_micros() as u64;
        Ok(result)
    }

    /// Find entities by cross-reference type
    pub fn find_by_cross_ref(
        &self,
        source_id: EntityId,
        ref_type: RefType,
        max_depth: u32,
    ) -> Result<UnifiedQueryResult, ExecutionError> {
        let start = std::time::Instant::now();
        let mut result = UnifiedQueryResult::new();
        let mut visited = std::collections::HashSet::new();
        let mut frontier = vec![(source_id, 0u32, vec![])];

        while let Some((current_id, depth, path)) = frontier.pop() {
            if depth > max_depth || visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id);

            // Find the entity
            if let Some((col_name, entity)) = self.store.get_any(current_id) {
                // Add to results if not the source
                if current_id != source_id {
                    let matched = MatchedEntity::new(
                        entity.clone(),
                        1.0 - (depth as f32 * 0.2),
                        MatchSource::CrossReference,
                    )
                    .with_refs(path.clone());
                    result.push(matched);
                }

                // Expand cross-refs of matching type
                for (target_id, link_type, target_collection) in
                    self.store.get_refs_from(current_id)
                {
                    if link_type == ref_type || matches!(ref_type, RefType::RelatedTo) {
                        let mut new_path = path.clone();
                        new_path.push(CrossRef::new(
                            current_id,
                            target_id,
                            target_collection,
                            link_type,
                        ));
                        frontier.push((target_id, depth + 1, new_path));
                    }
                }

                result.stats.cross_refs_followed += 1;
            }
        }

        result.stats.execution_time_us = start.elapsed().as_micros() as u64;
        Ok(result)
    }

    /// Execute a multi-modal query combining vector, graph, and table filters
    pub fn multi_modal_query(
        &self,
        query: MultiModalQuery,
    ) -> Result<UnifiedQueryResult, ExecutionError> {
        let start = std::time::Instant::now();
        let mut result = UnifiedQueryResult::new();

        // 1. Vector search if query vector provided
        let mut vector_results = HashMap::new();
        if let Some(ref qvec) = query.query_vector {
            let vec_result = self.vector_search(
                qvec,
                query.collections.as_deref(),
                query.vector_k.unwrap_or(10),
                query.metadata_filter.clone(),
            )?;
            for m in vec_result.entities {
                vector_results.insert(m.entity.id, m.score);
            }
        }

        // 2. Pattern matching for graph entities
        let mut graph_matches = std::collections::HashSet::new();
        if let Some(ref pattern) = query.graph_pattern {
            self.match_graph_pattern(pattern, &mut graph_matches)?;
        }

        // 3. Scan all collections and score entities
        for col_name in &self.store.list_collections() {
            if let Some(cols) = &query.collections {
                if !cols.contains(&col_name.as_str()) {
                    continue;
                }
            }

            let manager = match self.store.get_collection(col_name) {
                Some(m) => m,
                None => continue,
            };

            // Use query_all to get entities
            let entities = manager.query_all(|_| true);
            for entity in entities {
                let mut score = 0.0f32;
                let mut sources = vec![];

                // Vector similarity score
                if let Some(&vec_score) = vector_results.get(&entity.id) {
                    score += vec_score * query.vector_weight.unwrap_or(0.5);
                    sources.push(MatchSource::VectorSimilarity);
                }

                // Graph pattern match
                if graph_matches.contains(&entity.id) {
                    score += 0.8 * query.graph_weight.unwrap_or(0.3);
                    sources.push(MatchSource::GraphPattern);
                }

                // Metadata filter match - check entity properties
                if let Some(ref filter) = query.metadata_filter {
                    if self.matches_metadata(&entity, filter) {
                        score += 0.5 * query.table_weight.unwrap_or(0.2);
                        sources.push(MatchSource::TableFilter);
                    }
                }

                // Add if score is above threshold
                if score >= query.min_score.unwrap_or(0.1) {
                    let source = if sources.len() > 1 {
                        MatchSource::Hybrid
                    } else {
                        sources.first().copied().unwrap_or(MatchSource::Hybrid)
                    };

                    result.push(MatchedEntity::new(entity, score, source));
                }
            }
        }

        // Sort by score
        result.entities.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entity.id.cmp(&b.entity.id))
        });

        // Apply limit
        if let Some(limit) = query.limit {
            result.entities.truncate(limit);
        }

        result.stats.execution_time_us = start.elapsed().as_micros() as u64;
        Ok(result)
    }

    /// Expand context around an entity by following cross-refs
    pub fn expand_entity_context(
        &self,
        entity_id: EntityId,
        config: &RagConfig,
    ) -> Result<RetrievalContext, ExecutionError> {
        let mut context = RetrievalContext::new(format!("expand:{}", entity_id.0));

        // Find the entity first
        let (collection, entity) = self
            .store
            .get_any(entity_id)
            .ok_or_else(|| ExecutionError::new(format!("Entity {} not found", entity_id.0)))?;

        // Add the entity itself as a chunk
        context.add_chunk(entity_to_chunk(&entity, &collection, 1.0));

        // Follow cross-refs up to configured depth
        let refs_result =
            self.find_by_cross_ref(entity_id, RefType::RelatedTo, config.graph_depth)?;
        for matched in refs_result.entities {
            context.add_chunk(entity_to_chunk(&matched.entity, "cross_ref", matched.score));
        }

        // If the entity has embeddings, find similar vectors
        if !entity.embeddings().is_empty() && config.expand_cross_refs {
            let primary_vec = &entity.embeddings()[0].vector;
            let similar = self.vector_search(primary_vec, None, 5, None)?;
            for matched in similar.entities {
                if matched.entity.id != entity_id {
                    context.add_chunk(entity_to_chunk(
                        &matched.entity,
                        "similar",
                        matched.score * 0.8,
                    ));
                }
            }
        }

        Ok(context)
    }

    /// Check if an entity matches metadata filter by checking properties
    fn matches_metadata(&self, entity: &UnifiedEntity, filter: &MetadataQuery) -> bool {
        // Extract properties from entity data
        let properties: HashMap<String, Value> = match &entity.data {
            EntityData::Node(node) => node.properties.clone(),
            EntityData::Edge(edge) => edge.properties.clone(),
            EntityData::Row(row) => row.named.clone().unwrap_or_default(),
            EntityData::Vector(_) => HashMap::new(),
            EntityData::TimeSeries(_) => HashMap::new(),
            EntityData::QueueMessage(_) => HashMap::new(),
        };

        for (key, expected) in &filter.conditions {
            let prop_val = properties.get(key);
            let matches = match (prop_val, expected) {
                (Some(Value::Text(s)), QueryCondition::Equals(QueryValue::String(exp))) => {
                    &**s == exp.as_str()
                }
                (Some(Value::Integer(i)), QueryCondition::Equals(QueryValue::Int(exp))) => {
                    *i == *exp
                }
                (Some(Value::Float(f)), QueryCondition::Equals(QueryValue::Float(exp))) => {
                    *f == *exp
                }
                (Some(Value::Boolean(b)), QueryCondition::Equals(QueryValue::Bool(exp))) => {
                    *b == *exp
                }
                (Some(Value::Integer(i)), QueryCondition::GreaterThan(QueryValue::Int(n))) => {
                    *i > *n
                }
                (Some(Value::Float(f)), QueryCondition::GreaterThan(QueryValue::Float(n))) => {
                    *f > *n
                }
                (Some(Value::Integer(i)), QueryCondition::LessThan(QueryValue::Int(n))) => *i < *n,
                (Some(Value::Float(f)), QueryCondition::LessThan(QueryValue::Float(n))) => *f < *n,
                (Some(Value::Text(s)), QueryCondition::Contains(substr)) => {
                    s.contains(substr.as_str())
                }
                _ => false,
            };
            if !matches {
                return false;
            }
        }
        true
    }

    /// Match graph pattern against entities
    fn match_graph_pattern(
        &self,
        pattern: &GraphQueryPattern,
        matches: &mut std::collections::HashSet<EntityId>,
    ) -> Result<(), ExecutionError> {
        for col_name in &self.store.list_collections() {
            let manager = match self.store.get_collection(col_name) {
                Some(m) => m,
                None => continue,
            };

            let entities = manager.query_all(|_| true);
            for entity in entities {
                let is_match = match (&entity.kind, &pattern.node_pattern) {
                    (EntityKind::GraphNode(ref node), Some(pat)) => {
                        let label_match = pat.label.as_ref().is_none_or(|l| &node.label == l);
                        let type_match =
                            pat.node_type.as_ref().is_none_or(|t| &node.node_type == t);
                        label_match && type_match
                    }
                    (EntityKind::GraphEdge(ref edge), Some(pat)) => {
                        pat.label.as_ref() == Some(&edge.label)
                    }
                    (_, None) => true,
                    _ => false,
                };

                if is_match {
                    matches.insert(entity.id);
                }
            }
        }

        Ok(())
    }
}

/// Multi-modal query specification
#[derive(Debug, Clone, Default)]
pub struct MultiModalQuery {
    /// Query vector for similarity search
    pub query_vector: Option<Vec<f32>>,
    /// Collections to search (None = all)
    pub collections: Option<Vec<&'static str>>,
    /// Number of vectors to retrieve
    pub vector_k: Option<usize>,
    /// Graph pattern to match
    pub graph_pattern: Option<GraphQueryPattern>,
    /// Metadata filter conditions
    pub metadata_filter: Option<MetadataQuery>,
    /// Weight for vector similarity (0.0-1.0)
    pub vector_weight: Option<f32>,
    /// Weight for graph pattern match (0.0-1.0)
    pub graph_weight: Option<f32>,
    /// Weight for table/metadata filter (0.0-1.0)
    pub table_weight: Option<f32>,
    /// Minimum combined score
    pub min_score: Option<f32>,
    /// Maximum results to return
    pub limit: Option<usize>,
}

impl MultiModalQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_vector(mut self, vector: Vec<f32>, k: usize) -> Self {
        self.query_vector = Some(vector);
        self.vector_k = Some(k);
        self
    }

    pub fn with_graph_pattern(mut self, pattern: GraphQueryPattern) -> Self {
        self.graph_pattern = Some(pattern);
        self
    }

    pub fn with_metadata(mut self, filter: MetadataQuery) -> Self {
        self.metadata_filter = Some(filter);
        self
    }

    pub fn with_weights(mut self, vector: f32, graph: f32, table: f32) -> Self {
        self.vector_weight = Some(vector);
        self.graph_weight = Some(graph);
        self.table_weight = Some(table);
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// Graph pattern for matching
#[derive(Debug, Clone, Default)]
pub struct GraphQueryPattern {
    /// Node pattern (label, type)
    pub node_pattern: Option<NodePattern>,
    /// Edge patterns to match
    pub edge_patterns: Vec<EdgePatternSpec>,
}

/// Node pattern
#[derive(Debug, Clone)]
pub struct NodePattern {
    pub label: Option<String>,
    pub node_type: Option<String>,
}

/// Edge pattern
#[derive(Debug, Clone)]
pub struct EdgePatternSpec {
    pub label: Option<String>,
    pub direction: EdgeDirection,
}

#[derive(Debug, Clone, Copy)]
pub enum EdgeDirection {
    Outgoing,
    Incoming,
    Any,
}

/// Metadata query filter
#[derive(Debug, Clone, Default)]
pub struct MetadataQuery {
    pub conditions: HashMap<String, QueryCondition>,
}

impl MetadataQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn eq(mut self, key: impl Into<String>, value: impl Into<QueryValue>) -> Self {
        self.conditions
            .insert(key.into(), QueryCondition::Equals(value.into()));
        self
    }

    pub fn gt(mut self, key: impl Into<String>, value: impl Into<QueryValue>) -> Self {
        self.conditions
            .insert(key.into(), QueryCondition::GreaterThan(value.into()));
        self
    }

    pub fn lt(mut self, key: impl Into<String>, value: impl Into<QueryValue>) -> Self {
        self.conditions
            .insert(key.into(), QueryCondition::LessThan(value.into()));
        self
    }

    pub fn contains(mut self, key: impl Into<String>, substr: impl Into<String>) -> Self {
        self.conditions
            .insert(key.into(), QueryCondition::Contains(substr.into()));
        self
    }
}

#[derive(Debug, Clone)]
pub enum QueryCondition {
    Equals(QueryValue),
    GreaterThan(QueryValue),
    LessThan(QueryValue),
    Contains(String),
}

#[derive(Debug, Clone)]
pub enum QueryValue {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
}

impl From<i64> for QueryValue {
    fn from(v: i64) -> Self {
        QueryValue::Int(v)
    }
}

impl From<f64> for QueryValue {
    fn from(v: f64) -> Self {
        QueryValue::Float(v)
    }
}

impl From<&str> for QueryValue {
    fn from(v: &str) -> Self {
        QueryValue::String(v.to_string())
    }
}

impl From<String> for QueryValue {
    fn from(v: String) -> Self {
        QueryValue::String(v)
    }
}

impl From<bool> for QueryValue {
    fn from(v: bool) -> Self {
        QueryValue::Bool(v)
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Calculate cosine similarity between two vectors
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom > 0.0 {
        dot / denom
    } else {
        0.0
    }
}

/// Convert an entity to a context chunk
fn entity_to_chunk(entity: &UnifiedEntity, collection: &str, score: f32) -> ContextChunk {
    let content = match &entity.data {
        EntityData::Row(row) => {
            let fields: Vec<String> = row
                .columns
                .iter()
                .enumerate()
                .map(|(i, v)| format!("col{}: {:?}", i, v))
                .collect();
            fields.join(", ")
        }
        EntityData::Node(node) => {
            let props: Vec<String> = node
                .properties
                .iter()
                .map(|(k, v)| format!("{}: {:?}", k, v))
                .collect();
            format!("Node: {}", props.join(", "))
        }
        EntityData::Edge(edge) => {
            format!("Edge: weight={}", edge.weight)
        }
        EntityData::Vector(vec) => {
            format!(
                "Vector: dim={}, sparse={}",
                vec.dense.len(),
                vec.sparse.is_some()
            )
        }
        EntityData::TimeSeries(ts) => {
            format!("TimeSeries: metric={}, value={}", ts.metric, ts.value)
        }
        EntityData::QueueMessage(msg) => {
            format!(
                "QueueMessage: attempts={}, acked={}",
                msg.attempts, msg.acked
            )
        }
    };

    let (source, entity_type) = match &entity.kind {
        EntityKind::TableRow { table, .. } => (
            ChunkSource::Table(table.to_string()),
            Some(super::EntityType::Unknown), // Generic table row
        ),
        EntityKind::GraphNode(ref node) => (
            ChunkSource::Graph,
            // Try to map node_type to EntityType
            Some(match node.node_type.to_lowercase().as_str() {
                "host" => super::EntityType::Host,
                "service" => super::EntityType::Service,
                "port" => super::EntityType::Port,
                "vulnerability" | "vuln" => super::EntityType::Vulnerability,
                "credential" | "cred" => super::EntityType::Credential,
                "user" => super::EntityType::User,
                "certificate" | "cert" => super::EntityType::Certificate,
                "domain" => super::EntityType::Domain,
                "network" => super::EntityType::Network,
                "technology" | "tech" => super::EntityType::Technology,
                "endpoint" => super::EntityType::Endpoint,
                _ => super::EntityType::Unknown,
            }),
        ),
        EntityKind::GraphEdge(_) => (
            ChunkSource::Graph,
            Some(super::EntityType::Unknown), // Edges don't have a direct type mapping
        ),
        EntityKind::Vector { collection: col } => (
            ChunkSource::Vector(col.clone()),
            Some(super::EntityType::Unknown), // Vectors don't have a direct type mapping
        ),
        EntityKind::TimeSeriesPoint(ref ts) => (
            ChunkSource::Table(ts.series.clone()),
            Some(super::EntityType::Unknown),
        ),
        EntityKind::QueueMessage { queue, .. } => (
            ChunkSource::Table(queue.clone()),
            Some(super::EntityType::Unknown),
        ),
    };

    ContextChunk {
        content,
        source,
        relevance: score,
        entity_type,
        entity_id: Some(entity.id.0.to_string()),
        metadata: HashMap::new(),
        vector_distance: Some(1.0 - score), // Convert similarity to distance
        graph_depth: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

        let c = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &c).abs() < 0.001);

        let d = vec![1.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &d);
        assert!(sim > 0.7 && sim < 0.72);
    }

    #[test]
    fn test_metadata_query_builder() {
        let query = MetadataQuery::new()
            .eq("type", "host")
            .gt("score", 0.5f64)
            .contains("name", "server");

        assert_eq!(query.conditions.len(), 3);
    }

    #[test]
    fn test_multi_modal_query_builder() {
        let query = MultiModalQuery::new()
            .with_vector(vec![1.0, 0.0, 0.0], 10)
            .with_weights(0.6, 0.3, 0.1)
            .with_limit(20);

        assert!(query.query_vector.is_some());
        assert_eq!(query.vector_k, Some(10));
        assert_eq!(query.limit, Some(20));
    }
}
