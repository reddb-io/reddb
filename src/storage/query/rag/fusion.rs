//! Context Fusion and Re-ranking
//!
//! Advanced algorithms for combining and ranking results from multiple
//! retrieval sources (vectors, graphs, tables) to produce optimal RAG context.
//!
//! # Algorithms
//!
//! - **Reciprocal Rank Fusion (RRF)**: Combines rankings from multiple sources
//! - **Graph-Aware Re-ranking**: Boosts entities connected to high-scoring ones
//! - **Deduplication**: Removes semantically similar chunks
//! - **Diversification**: Ensures variety in entity types and sources

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::context::{ChunkSource, ContextChunk, RetrievalContext};
use super::EntityType;
use crate::storage::{EntityId, RefType, Store};

/// Configuration for context fusion
#[derive(Debug, Clone)]
pub struct FusionConfig {
    /// RRF constant k (typically 60)
    pub rrf_k: f32,
    /// Weight for vector similarity scores
    pub vector_weight: f32,
    /// Weight for graph-based scores
    pub graph_weight: f32,
    /// Weight for table/structured matches
    pub table_weight: f32,
    /// Cross-reference boost factor
    pub cross_ref_boost: f32,
    /// Minimum similarity for deduplication
    pub dedup_threshold: f32,
    /// Enable diversification
    pub diversify: bool,
    /// Maximum chunks per entity type
    pub max_per_type: usize,
    /// Enable graph-aware re-ranking
    pub graph_rerank: bool,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60.0,
            vector_weight: 0.5,
            graph_weight: 0.3,
            table_weight: 0.2,
            cross_ref_boost: 0.15,
            dedup_threshold: 0.85,
            diversify: true,
            max_per_type: 5,
            graph_rerank: true,
        }
    }
}

/// Context fusion engine
pub struct ContextFusion {
    /// Fusion configuration
    config: FusionConfig,
    /// Optional store for cross-reference lookup
    store: Option<Arc<Store>>,
}

impl ContextFusion {
    /// Create a new fusion engine with default config
    pub fn new() -> Self {
        Self {
            config: FusionConfig::default(),
            store: None,
        }
    }

    /// Create with custom config
    pub fn with_config(config: FusionConfig) -> Self {
        Self {
            config,
            store: None,
        }
    }

    /// Attach store for graph-aware operations
    pub fn with_store(mut self, store: Arc<Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Apply full fusion pipeline to a context
    pub fn fuse(&self, context: &mut RetrievalContext) {
        // 1. Normalize scores per source
        self.normalize_scores(context);

        // 2. Apply RRF if multiple sources
        if context.sources_used.len() > 1 {
            self.apply_rrf(context);
        }

        // 3. Graph-aware re-ranking
        if self.config.graph_rerank {
            self.graph_rerank(context);
        }

        // 4. Deduplicate similar chunks
        self.deduplicate(context);

        // 5. Diversify results
        if self.config.diversify {
            self.diversify(context);
        }

        // 6. Final sort
        context.sort_by_relevance();
    }

    /// Normalize scores within each source type to [0, 1]
    fn normalize_scores(&self, context: &mut RetrievalContext) {
        // Group by source type
        let mut vector_chunks: Vec<usize> = Vec::new();
        let mut graph_chunks: Vec<usize> = Vec::new();
        let mut table_chunks: Vec<usize> = Vec::new();
        let mut other_chunks: Vec<usize> = Vec::new();

        for (i, chunk) in context.chunks.iter().enumerate() {
            match chunk.source {
                ChunkSource::Vector(_) => vector_chunks.push(i),
                ChunkSource::Graph => graph_chunks.push(i),
                ChunkSource::Table(_) => table_chunks.push(i),
                _ => other_chunks.push(i),
            }
        }

        // Normalize each group
        self.normalize_group(&mut context.chunks, &vector_chunks);
        self.normalize_group(&mut context.chunks, &graph_chunks);
        self.normalize_group(&mut context.chunks, &table_chunks);
    }

    /// Normalize a group of chunks by index
    fn normalize_group(&self, chunks: &mut [ContextChunk], indices: &[usize]) {
        if indices.is_empty() {
            return;
        }

        let max_score = indices
            .iter()
            .map(|&i| chunks[i].relevance)
            .fold(f32::NEG_INFINITY, f32::max);
        let min_score = indices
            .iter()
            .map(|&i| chunks[i].relevance)
            .fold(f32::INFINITY, f32::min);

        let range = max_score - min_score;
        if range > 0.0001 {
            for &i in indices {
                chunks[i].relevance = (chunks[i].relevance - min_score) / range;
            }
        }
    }

    /// Apply Reciprocal Rank Fusion across sources
    fn apply_rrf(&self, context: &mut RetrievalContext) {
        // Build rankings per source
        let mut vector_rankings: HashMap<String, usize> = HashMap::new();
        let mut graph_rankings: HashMap<String, usize> = HashMap::new();
        let mut table_rankings: HashMap<String, usize> = HashMap::new();

        // Sort by relevance within each source and assign ranks
        let mut by_source: HashMap<String, Vec<(usize, f32)>> = HashMap::new();
        for (i, chunk) in context.chunks.iter().enumerate() {
            let source_key = match &chunk.source {
                ChunkSource::Vector(c) => format!("vector:{}", c),
                ChunkSource::Graph => "graph".to_string(),
                ChunkSource::Table(t) => format!("table:{}", t),
                _ => "other".to_string(),
            };
            by_source
                .entry(source_key)
                .or_default()
                .push((i, chunk.relevance));
        }

        // Assign ranks
        for (source, mut items) in by_source {
            items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (rank, (idx, _)) in items.iter().enumerate() {
                let key = format!("chunk_{}", idx);
                if source.starts_with("vector") {
                    vector_rankings.insert(key, rank + 1);
                } else if source == "graph" {
                    graph_rankings.insert(key, rank + 1);
                } else if source.starts_with("table") {
                    table_rankings.insert(key, rank + 1);
                }
            }
        }

        // Calculate RRF scores
        let k = self.config.rrf_k;
        for (i, chunk) in context.chunks.iter_mut().enumerate() {
            let key = format!("chunk_{}", i);

            let mut rrf_score = 0.0;

            if let Some(&rank) = vector_rankings.get(&key) {
                rrf_score += self.config.vector_weight * (1.0 / (k + rank as f32));
            }
            if let Some(&rank) = graph_rankings.get(&key) {
                rrf_score += self.config.graph_weight * (1.0 / (k + rank as f32));
            }
            if let Some(&rank) = table_rankings.get(&key) {
                rrf_score += self.config.table_weight * (1.0 / (k + rank as f32));
            }

            // Blend RRF with original relevance
            chunk.relevance = 0.6 * chunk.relevance + 0.4 * rrf_score * 100.0;
        }
    }

    /// Re-rank based on graph relationships
    fn graph_rerank(&self, context: &mut RetrievalContext) {
        let store = match &self.store {
            Some(s) => s,
            None => return,
        };

        // Build entity ID to chunk index mapping
        let mut entity_chunks: HashMap<EntityId, Vec<usize>> = HashMap::new();
        for (i, chunk) in context.chunks.iter().enumerate() {
            if let Some(ref id_str) = chunk.entity_id {
                if let Ok(id) = id_str.parse::<u64>() {
                    entity_chunks.entry(EntityId(id)).or_default().push(i);
                }
            }
        }

        // For each entity, boost chunks connected to it
        let mut boosts: HashMap<usize, f32> = HashMap::new();

        for (entity_id, chunk_indices) in &entity_chunks {
            // Get cross-references from this entity
            let refs_from = store.get_refs_from(*entity_id);

            for (target_id, ref_type, _collection) in refs_from {
                if let Some(target_chunks) = entity_chunks.get(&target_id) {
                    // Calculate boost based on reference type and source relevance
                    let source_relevance: f32 = chunk_indices
                        .iter()
                        .map(|&i| context.chunks[i].relevance)
                        .sum::<f32>()
                        / chunk_indices.len() as f32;

                    let type_multiplier = match ref_type {
                        RefType::RelatedTo | RefType::DerivesFrom => 1.0,
                        RefType::Mentions | RefType::Contains => 0.8,
                        RefType::DependsOn => 0.7,
                        RefType::SimilarTo => 0.5,
                        _ => 0.3,
                    };

                    let boost = self.config.cross_ref_boost * source_relevance * type_multiplier;

                    for &chunk_idx in target_chunks {
                        *boosts.entry(chunk_idx).or_insert(0.0) += boost;
                    }
                }
            }
        }

        // Apply boosts
        for (idx, boost) in boosts {
            context.chunks[idx].relevance += boost;
        }
    }

    /// Remove semantically similar chunks
    fn deduplicate(&self, context: &mut RetrievalContext) {
        if context.chunks.len() < 2 {
            return;
        }

        let mut to_remove: HashSet<usize> = HashSet::new();
        let threshold = self.config.dedup_threshold;

        for i in 0..context.chunks.len() {
            if to_remove.contains(&i) {
                continue;
            }

            for j in (i + 1)..context.chunks.len() {
                if to_remove.contains(&j) {
                    continue;
                }

                let similarity =
                    self.content_similarity(&context.chunks[i].content, &context.chunks[j].content);

                if similarity > threshold {
                    // Keep the one with higher relevance
                    if context.chunks[i].relevance >= context.chunks[j].relevance {
                        to_remove.insert(j);
                    } else {
                        to_remove.insert(i);
                        break;
                    }
                }
            }
        }

        // Remove duplicates (in reverse order to maintain indices)
        let mut indices: Vec<usize> = to_remove.into_iter().collect();
        indices.sort_by(|a, b| b.cmp(a));
        for idx in indices {
            context.chunks.remove(idx);
        }
    }

    /// Calculate content similarity using Jaccard on n-grams
    fn content_similarity(&self, a: &str, b: &str) -> f32 {
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }

        let ngrams_a = self.extract_ngrams(a, 3);
        let ngrams_b = self.extract_ngrams(b, 3);

        if ngrams_a.is_empty() || ngrams_b.is_empty() {
            return 0.0;
        }

        let intersection = ngrams_a.intersection(&ngrams_b).count();
        let union = ngrams_a.union(&ngrams_b).count();

        if union == 0 {
            0.0
        } else {
            intersection as f32 / union as f32
        }
    }

    /// Extract character n-grams from text
    fn extract_ngrams(&self, text: &str, n: usize) -> HashSet<String> {
        let text = text.to_lowercase();
        let chars: Vec<char> = text.chars().collect();

        if chars.len() < n {
            return HashSet::new();
        }

        (0..=chars.len() - n)
            .map(|i| chars[i..i + n].iter().collect())
            .collect()
    }

    /// Diversify results by entity type
    fn diversify(&self, context: &mut RetrievalContext) {
        let max_per_type = self.config.max_per_type;

        // Count by entity type
        let mut type_counts: HashMap<EntityType, usize> = HashMap::new();
        let mut to_remove: HashSet<usize> = HashSet::new();

        // Process in relevance order (already sorted)
        for (i, chunk) in context.chunks.iter().enumerate() {
            let entity_type = chunk.entity_type.unwrap_or(EntityType::Unknown);
            let count = type_counts.entry(entity_type).or_insert(0);

            if *count >= max_per_type {
                to_remove.insert(i);
            } else {
                *count += 1;
            }
        }

        // Remove excess chunks
        let mut indices: Vec<usize> = to_remove.into_iter().collect();
        indices.sort_by(|a, b| b.cmp(a));
        for idx in indices {
            context.chunks.remove(idx);
        }
    }
}

impl Default for ContextFusion {
    fn default() -> Self {
        Self::new()
    }
}

/// Result re-ranker for final scoring
pub struct ResultReranker {
    /// Weights for different scoring factors
    pub relevance_weight: f32,
    pub recency_weight: f32,
    pub connection_weight: f32,
    pub type_priority: HashMap<EntityType, f32>,
}

impl Default for ResultReranker {
    fn default() -> Self {
        let mut type_priority = HashMap::new();
        type_priority.insert(EntityType::Vulnerability, 1.0);
        type_priority.insert(EntityType::Host, 0.9);
        type_priority.insert(EntityType::Service, 0.85);
        type_priority.insert(EntityType::Credential, 0.95);
        type_priority.insert(EntityType::Certificate, 0.7);
        type_priority.insert(EntityType::Domain, 0.75);
        type_priority.insert(EntityType::Unknown, 0.5);

        Self {
            relevance_weight: 0.6,
            recency_weight: 0.2,
            connection_weight: 0.2,
            type_priority,
        }
    }
}

impl ResultReranker {
    /// Rerank chunks with multiple factors
    pub fn rerank(&self, context: &mut RetrievalContext) {
        for chunk in &mut context.chunks {
            let mut final_score = self.relevance_weight * chunk.relevance;

            // Type priority boost
            let type_boost = chunk
                .entity_type
                .and_then(|t| self.type_priority.get(&t))
                .unwrap_or(&0.5);
            final_score += 0.1 * type_boost;

            // Connection bonus (from graph depth)
            if let Some(depth) = chunk.graph_depth {
                // Closer connections score higher
                let connection_score = 1.0 / (1.0 + depth as f32);
                final_score += self.connection_weight * connection_score;
            }

            chunk.relevance = final_score;
        }

        context.sort_by_relevance();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_similarity() {
        let fusion = ContextFusion::new();

        let sim1 = fusion.content_similarity("This is a test string", "This is a test string");
        assert!((sim1 - 1.0).abs() < 0.001);

        let sim2 = fusion.content_similarity("completely different", "nothing alike");
        assert!(sim2 < 0.5);

        let sim3 = fusion.content_similarity("vulnerability in nginx", "vulnerability in apache");
        assert!(sim3 > 0.3 && sim3 < 0.8);
    }

    #[test]
    fn test_ngram_extraction() {
        let fusion = ContextFusion::new();

        let ngrams = fusion.extract_ngrams("hello", 3);
        assert!(ngrams.contains("hel"));
        assert!(ngrams.contains("ell"));
        assert!(ngrams.contains("llo"));
        assert_eq!(ngrams.len(), 3);
    }

    #[test]
    fn test_fusion_config_defaults() {
        let config = FusionConfig::default();
        assert_eq!(config.rrf_k, 60.0);
        assert!(config.diversify);
        assert!(config.graph_rerank);
    }
}
