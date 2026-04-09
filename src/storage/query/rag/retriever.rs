//! Multi-Source Retriever
//!
//! Implements retrieval strategies that combine vector search,
//! graph traversal, and table queries for comprehensive context.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::storage::engine::distance::DistanceMetric;
use crate::storage::engine::graph_store::{GraphStore, StoredNode};
use crate::storage::engine::graph_table_index::GraphTableIndex;
use crate::storage::engine::unified_index::UnifiedIndex;
use crate::storage::engine::vector_store::VectorStore;
use crate::storage::query::unified::ExecutionError;

use super::context::{ChunkSource, ContextChunk, RetrievalContext};
use super::{EntityType, QueryAnalysis, RagConfig, SimilarEntity};

/// Retrieval strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalStrategy {
    /// Use vector search as primary source
    VectorFirst,
    /// Use graph traversal as primary source
    GraphFirst,
    /// Combine vector and graph equally (hybrid)
    Hybrid,
    /// Only use vector search
    VectorOnly,
    /// Only use graph traversal
    GraphOnly,
    /// Table/structured query only
    TableOnly,
}

/// Multi-source retriever that combines vector, graph, and table queries
pub struct MultiSourceRetriever {
    /// Graph store
    graph: Arc<GraphStore>,
    /// Graph-table index
    index: Arc<GraphTableIndex>,
    /// Vector store
    vector_store: Arc<VectorStore>,
    /// Cross-reference index
    unified_index: Arc<UnifiedIndex>,
}

impl MultiSourceRetriever {
    /// Create a new multi-source retriever
    pub fn new(
        graph: Arc<GraphStore>,
        index: Arc<GraphTableIndex>,
        vector_store: Arc<VectorStore>,
        unified_index: Arc<UnifiedIndex>,
    ) -> Self {
        Self {
            graph,
            index,
            vector_store,
            unified_index,
        }
    }

    /// Retrieve context based on query analysis
    pub fn retrieve(
        &self,
        query: &str,
        analysis: &QueryAnalysis,
        config: &RagConfig,
    ) -> Result<RetrievalContext, ExecutionError> {
        let start = std::time::Instant::now();
        let mut context = RetrievalContext::new(query);

        // Execute based on primary strategy
        match analysis.primary_strategy {
            RetrievalStrategy::VectorFirst | RetrievalStrategy::VectorOnly => {
                self.retrieve_vector(query, analysis, config, &mut context)?;

                // Add graph context if not vector-only
                if analysis.primary_strategy != RetrievalStrategy::VectorOnly {
                    self.expand_with_graph(&mut context, config)?;
                }
            }
            RetrievalStrategy::GraphFirst | RetrievalStrategy::GraphOnly => {
                self.retrieve_graph(query, analysis, config, &mut context)?;

                // Add vector context if not graph-only
                if analysis.primary_strategy != RetrievalStrategy::GraphOnly {
                    self.expand_with_vectors(&mut context, config)?;
                }
            }
            RetrievalStrategy::Hybrid => {
                // Execute both in parallel conceptually, then merge
                self.retrieve_vector(query, analysis, config, &mut context)?;
                self.retrieve_graph(query, analysis, config, &mut context)?;
            }
            RetrievalStrategy::TableOnly => {
                self.retrieve_table(query, analysis, config, &mut context)?;
            }
        }

        // Cross-reference expansion if enabled
        if config.expand_cross_refs {
            self.expand_cross_refs(&mut context, config)?;
        }

        // Finalize
        context.sort_by_relevance();
        context.limit(config.max_total_chunks);
        context.calculate_overall_relevance();
        context.retrieval_time_us = start.elapsed().as_micros() as u64;

        // Add explanation
        let explanation = format!(
            "Retrieved {} chunks using {} strategy. Sources: {:?}",
            context.len(),
            match analysis.primary_strategy {
                RetrievalStrategy::VectorFirst => "vector-first",
                RetrievalStrategy::GraphFirst => "graph-first",
                RetrievalStrategy::Hybrid => "hybrid",
                RetrievalStrategy::VectorOnly => "vector-only",
                RetrievalStrategy::GraphOnly => "graph-only",
                RetrievalStrategy::TableOnly => "table-only",
            },
            context.sources_used
        );
        context.explanation = Some(explanation);

        Ok(context)
    }

    /// Retrieve context using vector search
    fn retrieve_vector(
        &self,
        query: &str,
        analysis: &QueryAnalysis,
        config: &RagConfig,
        context: &mut RetrievalContext,
    ) -> Result<(), ExecutionError> {
        // Determine which collections to search based on entity types
        let collections: Vec<&str> = if analysis.entity_types.is_empty() {
            // Search all relevant collections
            vec!["vulnerabilities", "hosts", "services"]
        } else {
            analysis
                .entity_types
                .iter()
                .map(|t| t.collection_name())
                .collect()
        };

        // For each collection, execute vector search
        for collection in collections {
            // Check if collection exists
            if let Some(coll) = self.vector_store.get(collection) {
                // Note: In a real implementation, we'd need to embed the query text
                // For now, we'll look for pre-embedded entities that might match

                // Get recent/relevant vectors from the collection
                // This is a simplified approach - real RAG would embed the query
                let results = self.search_collection_by_keywords(
                    collection,
                    &analysis.keywords,
                    config.max_chunks_per_source,
                );

                for (id, content, relevance) in results {
                    let chunk = ContextChunk::from_vector(
                        content,
                        collection,
                        1.0 - relevance, // Convert relevance to distance
                        id,
                    )
                    .with_entity_type(EntityType::from_str(collection));

                    context.add_chunk(chunk);
                }
            }
        }

        Ok(())
    }

    /// Search a collection by keywords (simplified - would use embeddings in real impl)
    fn search_collection_by_keywords(
        &self,
        collection: &str,
        keywords: &[String],
        limit: usize,
    ) -> Vec<(u64, String, f32)> {
        // This is a placeholder - in a real implementation:
        // 1. Embed the keywords using an embedding model
        // 2. Search the vector collection
        // 3. Return results with actual content

        // For now, return empty - the vector store would need
        // a metadata-based search or we'd need embeddings
        Vec::new()
    }

    /// Retrieve context using graph traversal
    fn retrieve_graph(
        &self,
        query: &str,
        analysis: &QueryAnalysis,
        config: &RagConfig,
        context: &mut RetrievalContext,
    ) -> Result<(), ExecutionError> {
        // Find starting nodes based on entity types and keywords
        let start_nodes = self.find_graph_start_nodes(analysis, config);

        // Traverse from each start node
        for (node_id, node_type) in start_nodes {
            self.traverse_and_collect(
                &node_id,
                node_type,
                config.graph_depth,
                context,
                &mut HashSet::new(),
            )?;
        }

        Ok(())
    }

    /// Find starting nodes for graph traversal
    fn find_graph_start_nodes(
        &self,
        analysis: &QueryAnalysis,
        config: &RagConfig,
    ) -> Vec<(String, EntityType)> {
        let mut nodes = Vec::new();

        // Look for nodes matching keywords
        for keyword in &analysis.keywords {
            // Check if keyword looks like a CVE
            if keyword.to_uppercase().starts_with("CVE-") {
                if let Some(node) = self.graph.get_node(&keyword.to_uppercase()) {
                    nodes.push((node.id.clone(), EntityType::Vulnerability));
                }
            }

            // Check if keyword looks like an IP
            if keyword.contains('.') && keyword.chars().all(|c| c.is_ascii_digit() || c == '.') {
                if let Some(node) = self.graph.get_node(keyword) {
                    nodes.push((node.id.clone(), EntityType::Host));
                }
            }
        }

        // Limit number of start nodes
        nodes.truncate(config.max_chunks_per_source);
        nodes
    }

    /// Traverse graph from a node and collect context
    fn traverse_and_collect(
        &self,
        node_id: &str,
        entity_type: EntityType,
        max_depth: u32,
        context: &mut RetrievalContext,
        visited: &mut HashSet<String>,
    ) -> Result<(), ExecutionError> {
        if max_depth == 0 || visited.contains(node_id) {
            return Ok(());
        }

        visited.insert(node_id.to_string());

        // Get node information
        if let Some(node) = self.graph.get_node(node_id) {
            // Create content string from node
            let content = self.node_to_content(&node);

            let chunk = ContextChunk::from_graph(
                content,
                max_depth - 1, // Depth from start (lower = closer)
                entity_type,
                node_id,
            );

            context.add_chunk(chunk);

            // Get outgoing edges and continue traversal
            let edges = self.graph.outgoing_edges(node_id);
            for (edge_type, target_id, _weight) in edges {
                if !visited.contains(&target_id) {
                    // Determine target entity type from edge type
                    let target_type = self.infer_entity_type_from_edge(edge_type.as_str());

                    self.traverse_and_collect(
                        &target_id,
                        target_type,
                        max_depth - 1,
                        context,
                        visited,
                    )?;
                }
            }
        }

        Ok(())
    }

    /// Convert node to content string
    fn node_to_content(&self, node: &StoredNode) -> String {
        // StoredNode has id, label, node_type but no properties HashMap
        // Just use the available fields
        format!(
            "{}: {} (label: {})",
            node.node_type.as_str(),
            node.id,
            node.label
        )
    }

    /// Infer entity type from edge type
    fn infer_entity_type_from_edge(&self, edge_type: &str) -> EntityType {
        match edge_type.to_lowercase().as_str() {
            "runs" | "hosts" => EntityType::Service,
            "has_vuln" | "affects" => EntityType::Vulnerability,
            "uses" | "depends_on" => EntityType::Technology,
            "owns" | "created_by" => EntityType::User,
            "connects_to" | "routes_to" => EntityType::Network,
            "has_cert" | "secured_by" => EntityType::Certificate,
            "resolves_to" | "has_domain" => EntityType::Domain,
            _ => EntityType::Unknown,
        }
    }

    /// Retrieve from table queries
    fn retrieve_table(
        &self,
        _query: &str,
        _analysis: &QueryAnalysis,
        _config: &RagConfig,
        _context: &mut RetrievalContext,
    ) -> Result<(), ExecutionError> {
        // Table retrieval would use the GraphTableIndex to find relevant rows
        // This is a placeholder for the full implementation
        Ok(())
    }

    /// Expand context with vector similarity
    fn expand_with_vectors(
        &self,
        context: &mut RetrievalContext,
        _config: &RagConfig,
    ) -> Result<(), ExecutionError> {
        // For entities found via graph, find similar vectors
        let entity_ids: Vec<(String, EntityType)> = context
            .chunks
            .iter()
            .filter(|c| matches!(c.source, ChunkSource::Graph))
            .filter_map(|c| {
                c.entity_id
                    .as_ref()
                    .map(|id| (id.clone(), c.entity_type.unwrap_or(EntityType::Unknown)))
            })
            .collect();

        for (entity_id, _entity_type) in entity_ids {
            // Check if this entity has vectors in unified index
            let vec_refs = self.unified_index.get_node_vectors(&entity_id);
            for vec_ref in vec_refs {
                // Search for similar vectors
                if let Some(_coll) = self.vector_store.get(&vec_ref.collection) {
                    // Would search for similar vectors here
                    // This requires the vector data which we'd get from the collection
                }
            }
        }

        Ok(())
    }

    /// Expand context with graph relationships
    fn expand_with_graph(
        &self,
        context: &mut RetrievalContext,
        _config: &RagConfig,
    ) -> Result<(), ExecutionError> {
        // For entities found via vector search, traverse graph relationships
        let vector_entities: Vec<(u64, String)> = context
            .chunks
            .iter()
            .filter(|c| matches!(c.source, ChunkSource::Vector(_)))
            .filter_map(|c| {
                c.entity_id
                    .as_ref()
                    .and_then(|id| id.parse().ok())
                    .map(|id| (id, c.source.collection().unwrap_or("unknown").to_string()))
            })
            .collect();

        for (vector_id, collection) in vector_entities {
            // Check if this vector is linked to a graph node
            if let Some(node_id) = self.unified_index.get_vector_node(&collection, vector_id) {
                let _entity_type = EntityType::from_str(&collection);

                // Get immediate neighbors via outgoing edges
                let edges = self.graph.outgoing_edges(&node_id);
                for (edge_type, target_id, _weight) in edges.into_iter().take(3) {
                    if let Some(target_node) = self.graph.get_node(&target_id) {
                        let content = self.node_to_content(&target_node);
                        let target_type = self.infer_entity_type_from_edge(edge_type.as_str());

                        let chunk = ContextChunk::from_graph(
                            format!("{} -> {}: {}", edge_type.as_str(), target_node.id, content),
                            1,
                            target_type,
                            &target_node.id,
                        );

                        context.add_chunk(chunk);
                    }
                }
            }
        }

        Ok(())
    }

    /// Expand context using cross-references
    fn expand_cross_refs(
        &self,
        context: &mut RetrievalContext,
        _config: &RagConfig,
    ) -> Result<(), ExecutionError> {
        // Find cross-references for existing chunks
        let existing_ids: Vec<(String, ChunkSource)> = context
            .chunks
            .iter()
            .filter_map(|c| {
                c.entity_id
                    .as_ref()
                    .map(|id| (id.clone(), c.source.clone()))
            })
            .collect();

        for (id, source) in existing_ids {
            match source {
                ChunkSource::Vector(collection) => {
                    // Vector -> check for linked node and row
                    if let Ok(id_num) = id.parse::<u64>() {
                        if let Some(row_key) =
                            self.unified_index.get_vector_row(&collection, id_num)
                        {
                            let chunk = ContextChunk::new(
                                format!("Linked row: {}:{}", row_key.table, row_key.row_id),
                                ChunkSource::CrossRef,
                                0.5,
                            );
                            context.add_chunk(chunk);
                        }
                    }
                }
                ChunkSource::Graph => {
                    // Graph -> check for linked vectors (returns Vec)
                    let vec_refs = self.unified_index.get_node_vectors(&id);
                    if let Some(vec_ref) = vec_refs.first() {
                        let chunk = ContextChunk::new(
                            format!("Has embedding in collection: {}", vec_ref.collection),
                            ChunkSource::CrossRef,
                            0.5,
                        );
                        context.add_chunk(chunk);
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Retrieve context by vector directly
    pub fn retrieve_by_vector(
        &self,
        vector: &[f32],
        collection: &str,
        k: usize,
        config: &RagConfig,
    ) -> Result<RetrievalContext, ExecutionError> {
        let start = std::time::Instant::now();
        let mut context = RetrievalContext::new(format!("vector search in {}", collection));

        // Execute vector search
        if let Some(coll) = self.vector_store.get(collection) {
            let results = coll.search_with_filter(vector, k, None);

            for result in results {
                // Skip if below threshold
                let relevance = 1.0 / (1.0 + result.distance);
                if relevance < config.min_relevance {
                    continue;
                }

                // Get content from metadata or generate placeholder
                let content = result
                    .metadata
                    .as_ref()
                    .and_then(|m| m.strings.get("content").cloned())
                    .unwrap_or_else(|| format!("Vector {} in {}", result.id, collection));

                let chunk =
                    ContextChunk::from_vector(content, collection, result.distance, result.id)
                        .with_entity_type(EntityType::from_str(collection));

                context.add_chunk(chunk);
            }
        }

        // Expand with graph context if enabled
        if config.expand_cross_refs {
            self.expand_with_graph(&mut context, config)?;
        }

        context.sort_by_relevance();
        context.calculate_overall_relevance();
        context.retrieval_time_us = start.elapsed().as_micros() as u64;

        Ok(context)
    }

    /// Expand context around a known entity
    pub fn expand_context(
        &self,
        entity_id: &str,
        entity_type: EntityType,
        depth: u32,
        config: &RagConfig,
    ) -> Result<RetrievalContext, ExecutionError> {
        let start = std::time::Instant::now();
        let mut context = RetrievalContext::new(format!(
            "expand {}:{}",
            entity_type.collection_name(),
            entity_id
        ));

        // Traverse graph from entity
        self.traverse_and_collect(
            entity_id,
            entity_type,
            depth,
            &mut context,
            &mut HashSet::new(),
        )?;

        // Add vector similarity if entity has embedding
        let vec_refs = self.unified_index.get_node_vectors(entity_id);
        if !vec_refs.is_empty() {
            // Would search for similar vectors here
            // Requires getting the vector data first
        }

        context.sort_by_relevance();
        context.calculate_overall_relevance();
        context.retrieval_time_us = start.elapsed().as_micros() as u64;

        Ok(context)
    }

    /// Find similar entities by vector
    pub fn find_similar(
        &self,
        collection: &str,
        entity_id: u64,
        k: usize,
    ) -> Result<Vec<SimilarEntity>, ExecutionError> {
        // Get the vector for this entity
        let coll = self
            .vector_store
            .get(collection)
            .ok_or_else(|| ExecutionError::new(format!("Collection not found: {}", collection)))?;

        // Would need to get vector by ID - this requires extending VectorCollection
        // For now, return empty
        Ok(Vec::new())
    }
}

// ============================================================================
// In-Memory Retriever for Testing
// ============================================================================

/// In-memory retriever for testing without full storage backends
pub struct InMemoryRetriever {
    /// Stored chunks
    chunks: Vec<StoredChunk>,
    /// Simple vector index
    vectors: HashMap<String, Vec<(u64, Vec<f32>, String)>>,
}

struct StoredChunk {
    content: String,
    source: ChunkSource,
    entity_type: Option<EntityType>,
    entity_id: Option<String>,
    keywords: Vec<String>,
}

impl InMemoryRetriever {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            vectors: HashMap::new(),
        }
    }

    /// Add a chunk
    pub fn add_chunk(
        &mut self,
        content: &str,
        source: ChunkSource,
        entity_type: Option<EntityType>,
        keywords: Vec<String>,
    ) {
        self.chunks.push(StoredChunk {
            content: content.to_string(),
            source,
            entity_type,
            entity_id: None,
            keywords,
        });
    }

    /// Add a vector
    pub fn add_vector(&mut self, collection: &str, id: u64, vector: Vec<f32>, content: &str) {
        self.vectors
            .entry(collection.to_string())
            .or_insert_with(Vec::new)
            .push((id, vector, content.to_string()));
    }

    /// Search by keywords
    pub fn search_keywords(&self, keywords: &[String], limit: usize) -> RetrievalContext {
        let mut context = RetrievalContext::new(keywords.join(" "));

        for chunk in &self.chunks {
            let matches: usize = keywords
                .iter()
                .filter(|kw| {
                    chunk.keywords.contains(kw)
                        || chunk.content.to_lowercase().contains(&kw.to_lowercase())
                })
                .count();

            if matches > 0 {
                let relevance = matches as f32 / keywords.len().max(1) as f32;
                let ctx_chunk = ContextChunk::new(&chunk.content, chunk.source.clone(), relevance)
                    .with_entity_type(chunk.entity_type.unwrap_or(EntityType::Unknown));

                context.add_chunk(ctx_chunk);
            }
        }

        context.sort_by_relevance();
        context.limit(limit);
        context.calculate_overall_relevance();
        context
    }

    /// Vector search
    pub fn search_vector(&self, collection: &str, query: &[f32], k: usize) -> RetrievalContext {
        let mut context = RetrievalContext::new(format!("vector search {}", collection));

        if let Some(vectors) = self.vectors.get(collection) {
            let mut distances: Vec<(u64, f32, &str)> = vectors
                .iter()
                .map(|(id, vec, content)| {
                    let dist =
                        crate::storage::engine::distance::distance(query, vec, DistanceMetric::L2);
                    (*id, dist, content.as_str())
                })
                .collect();

            distances.sort_by(|a, b| {
                a.1.partial_cmp(&b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });

            for (id, dist, content) in distances.into_iter().take(k) {
                let chunk = ContextChunk::from_vector(content, collection, dist, id);
                context.add_chunk(chunk);
            }
        }

        context.calculate_overall_relevance();
        context
    }
}

impl Default for InMemoryRetriever {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_keyword_search() {
        let mut retriever = InMemoryRetriever::new();

        retriever.add_chunk(
            "CVE-2024-1234 is a critical SQL injection vulnerability in nginx",
            ChunkSource::Intelligence,
            Some(EntityType::Vulnerability),
            vec!["cve".to_string(), "sql".to_string(), "nginx".to_string()],
        );

        retriever.add_chunk(
            "Host 192.168.1.1 runs nginx web server",
            ChunkSource::Graph,
            Some(EntityType::Host),
            vec!["host".to_string(), "nginx".to_string()],
        );

        let context = retriever.search_keywords(&["nginx".to_string()], 10);
        assert_eq!(context.len(), 2);

        let context = retriever.search_keywords(&["cve".to_string(), "sql".to_string()], 10);
        assert_eq!(context.len(), 1);
    }

    #[test]
    fn test_in_memory_vector_search() {
        let mut retriever = InMemoryRetriever::new();

        retriever.add_vector("vulns", 1, vec![1.0, 0.0, 0.0], "CVE-2024-1234");
        retriever.add_vector("vulns", 2, vec![0.9, 0.1, 0.0], "CVE-2024-5678");
        retriever.add_vector("vulns", 3, vec![0.0, 1.0, 0.0], "CVE-2024-9999");

        let context = retriever.search_vector("vulns", &[1.0, 0.0, 0.0], 2);
        assert_eq!(context.len(), 2);

        // First result should be the exact match
        let top = context.top_chunk().unwrap();
        assert!(top.content.contains("1234"));
    }

    #[test]
    fn test_retrieval_strategy() {
        assert_eq!(
            RetrievalStrategy::VectorFirst,
            RetrievalStrategy::VectorFirst
        );
        assert_ne!(
            RetrievalStrategy::VectorFirst,
            RetrievalStrategy::GraphFirst
        );
    }
}
