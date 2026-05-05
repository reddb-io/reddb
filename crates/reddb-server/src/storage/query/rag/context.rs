//! Retrieval Context
//!
//! Represents the contextual information retrieved for a query,
//! including chunks from various sources with relevance scoring.

use std::collections::HashMap;

use super::EntityType;
use crate::storage::schema::Value;
use std::cmp::Ordering;

/// Complete context retrieved for a query
#[derive(Debug, Clone)]
pub struct RetrievalContext {
    /// Original query
    pub query: String,
    /// Retrieved context chunks
    pub chunks: Vec<ContextChunk>,
    /// Overall relevance score
    pub overall_relevance: f32,
    /// Sources used in retrieval
    pub sources_used: Vec<ChunkSource>,
    /// Total retrieval time in microseconds
    pub retrieval_time_us: u64,
    /// Explanation of retrieval strategy
    pub explanation: Option<String>,
}

impl RetrievalContext {
    /// Create a new retrieval context
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            chunks: Vec::new(),
            overall_relevance: 0.0,
            sources_used: Vec::new(),
            retrieval_time_us: 0,
            explanation: None,
        }
    }

    /// Add a chunk to the context
    pub fn add_chunk(&mut self, chunk: ContextChunk) {
        if !self.sources_used.contains(&chunk.source) {
            self.sources_used.push(chunk.source.clone());
        }
        self.chunks.push(chunk);
    }

    /// Sort chunks by relevance (descending)
    pub fn sort_by_relevance(&mut self) {
        self.chunks.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    let a_entity = a.entity_id.as_deref().unwrap_or("");
                    let b_entity = b.entity_id.as_deref().unwrap_or("");
                    a_entity.cmp(b_entity)
                })
                .then_with(|| a.source.name().cmp(b.source.name()))
                .then_with(|| a.content.cmp(&b.content))
        });
    }

    /// Calculate overall relevance from chunks
    pub fn calculate_overall_relevance(&mut self) {
        if self.chunks.is_empty() {
            self.overall_relevance = 0.0;
            return;
        }

        // Use weighted average, with top results weighted higher
        let total_weight: f32 = (1..=self.chunks.len()).map(|i| 1.0 / i as f32).sum();

        let weighted_sum: f32 = self
            .chunks
            .iter()
            .enumerate()
            .map(|(i, c)| c.relevance * (1.0 / (i + 1) as f32))
            .sum();

        self.overall_relevance = weighted_sum / total_weight;
    }

    /// Limit to top N chunks
    pub fn limit(&mut self, n: usize) {
        self.sort_by_relevance();
        self.chunks.truncate(n);
    }

    /// Get chunks for a specific entity type
    pub fn chunks_for_type(&self, entity_type: EntityType) -> Vec<&ContextChunk> {
        self.chunks
            .iter()
            .filter(|c| c.entity_type == Some(entity_type))
            .collect()
    }

    /// Get chunks from a specific source
    pub fn chunks_from_source(&self, source: &ChunkSource) -> Vec<&ContextChunk> {
        self.chunks.iter().filter(|c| &c.source == source).collect()
    }

    /// Check if context is empty
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Number of chunks
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Get the top chunk
    pub fn top_chunk(&self) -> Option<&ContextChunk> {
        self.chunks.first()
    }

    /// Convert to a text representation for LLM context
    pub fn to_context_string(&self) -> String {
        let mut s = String::new();

        for (i, chunk) in self.chunks.iter().enumerate() {
            s.push_str(&format!("[{}] ", i + 1));
            s.push_str(&chunk.to_text());
            s.push('\n');
        }

        s
    }

    /// Get entity IDs mentioned in the context
    pub fn entity_ids(&self) -> Vec<&str> {
        self.chunks
            .iter()
            .filter_map(|c| c.entity_id.as_deref())
            .collect()
    }

    /// Merge another context into this one
    pub fn merge(&mut self, other: RetrievalContext) {
        for chunk in other.chunks {
            self.add_chunk(chunk);
        }
        self.retrieval_time_us += other.retrieval_time_us;
    }

    /// Set explanation
    pub fn with_explanation(mut self, explanation: impl Into<String>) -> Self {
        self.explanation = Some(explanation.into());
        self
    }
}

/// A single chunk of context
#[derive(Debug, Clone)]
pub struct ContextChunk {
    /// The content of this chunk
    pub content: String,
    /// Source of this chunk
    pub source: ChunkSource,
    /// Relevance score (0.0-1.0)
    pub relevance: f32,
    /// Entity type if applicable
    pub entity_type: Option<EntityType>,
    /// Entity ID if applicable
    pub entity_id: Option<String>,
    /// Additional metadata
    pub metadata: HashMap<String, Value>,
    /// Distance/similarity score from vector search (if applicable)
    pub vector_distance: Option<f32>,
    /// Graph depth from query entity (if applicable)
    pub graph_depth: Option<u32>,
}

impl ContextChunk {
    /// Create a new chunk
    pub fn new(content: impl Into<String>, source: ChunkSource, relevance: f32) -> Self {
        Self {
            content: content.into(),
            source,
            relevance,
            entity_type: None,
            entity_id: None,
            metadata: HashMap::new(),
            vector_distance: None,
            graph_depth: None,
        }
    }

    /// Create from vector search result
    pub fn from_vector(
        content: impl Into<String>,
        collection: impl Into<String>,
        distance: f32,
        id: u64,
    ) -> Self {
        let relevance = 1.0 / (1.0 + distance); // Convert distance to relevance
        let mut chunk = Self::new(content, ChunkSource::Vector(collection.into()), relevance);
        chunk.vector_distance = Some(distance);
        chunk.entity_id = Some(id.to_string());
        chunk
    }

    /// Create from graph traversal
    pub fn from_graph(
        content: impl Into<String>,
        depth: u32,
        entity_type: EntityType,
        entity_id: impl Into<String>,
    ) -> Self {
        // Relevance decreases with depth
        let relevance = 1.0 / (1.0 + depth as f32);
        let mut chunk = Self::new(content, ChunkSource::Graph, relevance);
        chunk.graph_depth = Some(depth);
        chunk.entity_type = Some(entity_type);
        chunk.entity_id = Some(entity_id.into());
        chunk
    }

    /// Create from table query
    pub fn from_table(
        content: impl Into<String>,
        table: impl Into<String>,
        row_id: u64,
        relevance: f32,
    ) -> Self {
        let mut chunk = Self::new(content, ChunkSource::Table(table.into()), relevance);
        chunk.entity_id = Some(row_id.to_string());
        chunk
    }

    /// Set entity type
    pub fn with_entity_type(mut self, entity_type: EntityType) -> Self {
        self.entity_type = Some(entity_type);
        self
    }

    /// Set entity ID
    pub fn with_entity_id(mut self, id: impl Into<String>) -> Self {
        self.entity_id = Some(id.into());
        self
    }

    /// Add metadata
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Convert to text representation
    pub fn to_text(&self) -> String {
        let mut parts = Vec::new();

        // Source info
        parts.push(format!("[{}]", self.source.name()));

        // Entity info
        if let Some(ref id) = self.entity_id {
            if let Some(entity_type) = self.entity_type {
                parts.push(format!("{:?}:{}", entity_type, id));
            } else {
                parts.push(format!("id:{}", id));
            }
        }

        // Score info
        parts.push(format!("relevance:{:.2}", self.relevance));

        // Content
        format!("{}: {}", parts.join(" "), self.content)
    }
}

/// Source of a context chunk
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChunkSource {
    /// From vector similarity search
    Vector(String), // collection name
    /// From graph traversal
    Graph,
    /// From table/structured query
    Table(String), // table name
    /// From cross-reference expansion
    CrossRef,
    /// From intelligence layer
    Intelligence,
    /// Cached/previously retrieved
    Cache,
}

impl ChunkSource {
    /// Get a display name for the source
    pub fn name(&self) -> &str {
        match self {
            Self::Vector(_) => "vector",
            Self::Graph => "graph",
            Self::Table(_) => "table",
            Self::CrossRef => "cross-ref",
            Self::Intelligence => "intel",
            Self::Cache => "cache",
        }
    }

    /// Get the collection/table name if applicable
    pub fn collection(&self) -> Option<&str> {
        match self {
            Self::Vector(c) | Self::Table(c) => Some(c),
            _ => None,
        }
    }
}

// ============================================================================
// Context Builder
// ============================================================================

/// Builder for creating retrieval contexts
pub struct ContextBuilder {
    context: RetrievalContext,
}

impl ContextBuilder {
    /// Start building a new context
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            context: RetrievalContext::new(query),
        }
    }

    /// Add a chunk
    pub fn chunk(mut self, chunk: ContextChunk) -> Self {
        self.context.add_chunk(chunk);
        self
    }

    /// Add a vector result
    pub fn vector_result(
        mut self,
        content: impl Into<String>,
        collection: impl Into<String>,
        distance: f32,
        id: u64,
    ) -> Self {
        self.context
            .add_chunk(ContextChunk::from_vector(content, collection, distance, id));
        self
    }

    /// Add a graph result
    pub fn graph_result(
        mut self,
        content: impl Into<String>,
        depth: u32,
        entity_type: EntityType,
        entity_id: impl Into<String>,
    ) -> Self {
        self.context.add_chunk(ContextChunk::from_graph(
            content,
            depth,
            entity_type,
            entity_id,
        ));
        self
    }

    /// Add a table result
    pub fn table_result(
        mut self,
        content: impl Into<String>,
        table: impl Into<String>,
        row_id: u64,
        relevance: f32,
    ) -> Self {
        self.context
            .add_chunk(ContextChunk::from_table(content, table, row_id, relevance));
        self
    }

    /// Set retrieval time
    pub fn time_us(mut self, time: u64) -> Self {
        self.context.retrieval_time_us = time;
        self
    }

    /// Set explanation
    pub fn explanation(mut self, explanation: impl Into<String>) -> Self {
        self.context.explanation = Some(explanation.into());
        self
    }

    /// Build the context
    pub fn build(mut self) -> RetrievalContext {
        self.context.sort_by_relevance();
        self.context.calculate_overall_relevance();
        self.context
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_builder() {
        let context = ContextBuilder::new("test query")
            .vector_result(
                "CVE-2024-1234: SQL injection vulnerability",
                "vulns",
                0.1,
                1,
            )
            .vector_result("CVE-2024-5678: XSS vulnerability", "vulns", 0.3, 2)
            .graph_result("Host 192.168.1.1 runs nginx", 1, EntityType::Host, "h1")
            .time_us(1000)
            .build();

        assert_eq!(context.len(), 3);
        assert!(context.overall_relevance > 0.0);

        // Top chunk should be the closest vector result
        let top = context.top_chunk().unwrap();
        assert!(matches!(top.source, ChunkSource::Vector(_)));
    }

    #[test]
    fn test_relevance_calculation() {
        let mut context = RetrievalContext::new("test");
        context.add_chunk(ContextChunk::new("A", ChunkSource::Graph, 1.0));
        context.add_chunk(ContextChunk::new("B", ChunkSource::Graph, 0.5));
        context.add_chunk(ContextChunk::new("C", ChunkSource::Graph, 0.25));

        context.calculate_overall_relevance();

        // Weighted average should be between min and max
        assert!(context.overall_relevance > 0.25);
        assert!(context.overall_relevance < 1.0);
    }

    #[test]
    fn test_context_filtering() {
        let mut context = RetrievalContext::new("test");
        context.add_chunk(
            ContextChunk::new("Host info", ChunkSource::Graph, 0.9)
                .with_entity_type(EntityType::Host),
        );
        context.add_chunk(
            ContextChunk::new("Vuln info", ChunkSource::Graph, 0.8)
                .with_entity_type(EntityType::Vulnerability),
        );

        let hosts = context.chunks_for_type(EntityType::Host);
        assert_eq!(hosts.len(), 1);
        assert!(hosts[0].content.contains("Host"));
    }

    #[test]
    fn test_context_merge() {
        let mut context1 = RetrievalContext::new("test");
        context1.add_chunk(ContextChunk::new("A", ChunkSource::Graph, 0.9));
        context1.retrieval_time_us = 100;

        let mut context2 = RetrievalContext::new("test");
        context2.add_chunk(ContextChunk::new(
            "B",
            ChunkSource::Vector("v".to_string()),
            0.8,
        ));
        context2.retrieval_time_us = 200;

        context1.merge(context2);

        assert_eq!(context1.len(), 2);
        assert_eq!(context1.retrieval_time_us, 300);
    }

    #[test]
    fn test_to_context_string() {
        let context = ContextBuilder::new("test")
            .vector_result("Important finding", "vulns", 0.1, 1)
            .build();

        let text = context.to_context_string();
        assert!(text.contains("[1]"));
        assert!(text.contains("vector"));
        assert!(text.contains("Important finding"));
    }
}
