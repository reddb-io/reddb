//! Hybrid Search for RedDB
//!
//! Combines dense (vector) and sparse (keyword) search for improved retrieval:
//! - Dense search: Semantic similarity via HNSW
//! - Sparse search: BM25-style keyword matching
//! - Fusion: Reciprocal Rank Fusion (RRF), Linear Combination
//! - Filtering: Pre-filter and post-filter by metadata
//! - Re-ranking: Score adjustment pipeline
//!
//! # Example
//!
//! ```ignore
//! let hybrid = HybridSearch::new(&hnsw_index, &sparse_index);
//! let results = hybrid
//!     .query("CVE remote code execution")
//!     .with_vector(&query_embedding)
//!     .with_alpha(0.7)  // 70% dense, 30% sparse
//!     .filter(|meta| meta.get("severity") == Some(&"critical".into()))
//!     .top_k(10)
//!     .execute();
//! ```

use std::collections::{HashMap, HashSet};

use super::distance::DistanceResult;
use super::hnsw::{HnswIndex, NodeId};
use super::vector_metadata::{MetadataFilter, MetadataStore};

// ============================================================================
// Sparse Index (BM25-style)
// ============================================================================

/// BM25 parameters
#[derive(Clone, Debug)]
pub struct BM25Config {
    /// Term frequency saturation parameter (typically 1.2-2.0)
    pub k1: f32,
    /// Length normalization parameter (typically 0.75)
    pub b: f32,
}

impl Default for BM25Config {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

/// Sparse inverted index for keyword search
pub struct SparseIndex {
    /// Term -> list of (doc_id, term_frequency)
    postings: HashMap<String, Vec<(NodeId, f32)>>,
    /// Document lengths (number of terms)
    doc_lengths: HashMap<NodeId, usize>,
    /// Average document length
    avg_doc_length: f32,
    /// Number of documents
    doc_count: usize,
    /// BM25 configuration
    config: BM25Config,
}

impl SparseIndex {
    /// Create a new sparse index
    pub fn new() -> Self {
        Self {
            postings: HashMap::new(),
            doc_lengths: HashMap::new(),
            avg_doc_length: 0.0,
            doc_count: 0,
            config: BM25Config::default(),
        }
    }

    /// Create with custom BM25 config
    pub fn with_config(config: BM25Config) -> Self {
        Self {
            postings: HashMap::new(),
            doc_lengths: HashMap::new(),
            avg_doc_length: 0.0,
            doc_count: 0,
            config,
        }
    }

    /// Index a document with its terms
    pub fn index(&mut self, doc_id: NodeId, terms: &[String]) {
        // Count term frequencies
        let mut term_counts: HashMap<&str, usize> = HashMap::new();
        for term in terms {
            *term_counts.entry(term.as_str()).or_insert(0) += 1;
        }

        // Update postings
        for (term, count) in term_counts {
            self.postings
                .entry(term.to_lowercase())
                .or_default()
                .push((doc_id, count as f32));
        }

        // Update document length
        self.doc_lengths.insert(doc_id, terms.len());
        self.doc_count += 1;

        // Recalculate average document length
        let total_length: usize = self.doc_lengths.values().sum();
        self.avg_doc_length = total_length as f32 / self.doc_count as f32;
    }

    /// Index a document from text (tokenizes automatically)
    pub fn index_text(&mut self, doc_id: NodeId, text: &str) {
        let terms: Vec<String> = tokenize(text);
        self.index(doc_id, &terms);
    }

    /// Remove a document from the index
    pub fn remove(&mut self, doc_id: NodeId) {
        // Remove from postings
        for postings in self.postings.values_mut() {
            postings.retain(|(id, _)| *id != doc_id);
        }

        // Remove from doc_lengths
        if self.doc_lengths.remove(&doc_id).is_some() {
            self.doc_count = self.doc_count.saturating_sub(1);

            // Recalculate average
            if self.doc_count > 0 {
                let total_length: usize = self.doc_lengths.values().sum();
                self.avg_doc_length = total_length as f32 / self.doc_count as f32;
            } else {
                self.avg_doc_length = 0.0;
            }
        }
    }

    /// Search using BM25 scoring
    pub fn search(&self, query: &str, k: usize) -> Vec<SparseResult> {
        let query_terms = tokenize(query);

        if query_terms.is_empty() {
            return Vec::new();
        }

        // Calculate BM25 scores for each document
        let mut scores: HashMap<NodeId, f32> = HashMap::new();

        for term in &query_terms {
            let term_lower = term.to_lowercase();
            if let Some(postings) = self.postings.get(&term_lower) {
                // IDF component
                let df = postings.len() as f32;
                let idf = ((self.doc_count as f32 - df + 0.5) / (df + 0.5) + 1.0).ln();

                for &(doc_id, tf) in postings {
                    let doc_len = self.doc_lengths.get(&doc_id).copied().unwrap_or(1) as f32;

                    // BM25 TF component
                    let tf_component = (tf * (self.config.k1 + 1.0))
                        / (tf
                            + self.config.k1
                                * (1.0 - self.config.b
                                    + self.config.b * doc_len / self.avg_doc_length));

                    *scores.entry(doc_id).or_insert(0.0) += idf * tf_component;
                }
            }
        }

        // Sort by score descending
        let mut results: Vec<SparseResult> = scores
            .into_iter()
            .map(|(id, score)| SparseResult { id, score })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        results.truncate(k);

        results
    }

    /// Get number of indexed documents
    pub fn len(&self) -> usize {
        self.doc_count
    }

    /// Check if index is empty
    pub fn is_empty(&self) -> bool {
        self.doc_count == 0
    }

    /// Get vocabulary size
    pub fn vocab_size(&self) -> usize {
        self.postings.len()
    }
}

impl Default for SparseIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Result from sparse search
#[derive(Debug, Clone)]
pub struct SparseResult {
    pub id: NodeId,
    pub score: f32,
}

/// Simple tokenizer for text
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|s| s.len() >= 2) // Skip single characters
        .map(|s| s.to_lowercase())
        .collect()
}

// ============================================================================
// Fusion Methods
// ============================================================================

/// Method for combining dense and sparse scores
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FusionMethod {
    /// Reciprocal Rank Fusion with parameter k (default: 60)
    RRF(usize),
    /// Linear combination: alpha * dense + (1-alpha) * sparse
    Linear(f32),
    /// Distribution-Based Score Fusion
    DBSF,
}

impl Default for FusionMethod {
    fn default() -> Self {
        FusionMethod::RRF(60)
    }
}

/// Reciprocal Rank Fusion
///
/// RRF(d) = Σ 1/(k + rank(d))
/// Works well when scores aren't comparable across systems
pub fn reciprocal_rank_fusion(
    dense_results: &[DistanceResult],
    sparse_results: &[SparseResult],
    k: usize,
) -> Vec<HybridResult> {
    let mut scores: HashMap<NodeId, f32> = HashMap::new();
    let mut dense_scores: HashMap<NodeId, f32> = HashMap::new();
    let mut sparse_scores: HashMap<NodeId, f32> = HashMap::new();

    // Add dense scores
    for (rank, result) in dense_results.iter().enumerate() {
        let rrf_score = 1.0 / (k as f32 + rank as f32 + 1.0);
        *scores.entry(result.id).or_insert(0.0) += rrf_score;
        dense_scores.insert(result.id, result.distance);
    }

    // Add sparse scores
    for (rank, result) in sparse_results.iter().enumerate() {
        let rrf_score = 1.0 / (k as f32 + rank as f32 + 1.0);
        *scores.entry(result.id).or_insert(0.0) += rrf_score;
        sparse_scores.insert(result.id, result.score);
    }

    // Convert to results
    let mut results: Vec<HybridResult> = scores
        .into_iter()
        .map(|(id, score)| HybridResult {
            id,
            score,
            dense_score: dense_scores.get(&id).copied(),
            sparse_score: sparse_scores.get(&id).copied(),
        })
        .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
    results
}

/// Linear combination of scores
///
/// score = alpha * normalized_dense + (1 - alpha) * normalized_sparse
pub fn linear_fusion(
    dense_results: &[DistanceResult],
    sparse_results: &[SparseResult],
    alpha: f32,
) -> Vec<HybridResult> {
    let mut scores: HashMap<NodeId, (Option<f32>, Option<f32>)> = HashMap::new();

    // Normalize dense scores (distance to similarity: 1 / (1 + distance))
    let dense_min = dense_results
        .iter()
        .map(|r| r.distance)
        .fold(f32::INFINITY, f32::min);
    let dense_max = dense_results
        .iter()
        .map(|r| r.distance)
        .fold(f32::NEG_INFINITY, f32::max);
    let dense_range = (dense_max - dense_min).max(1e-6);

    for result in dense_results {
        // Convert distance to similarity (lower distance = higher similarity)
        let normalized = 1.0 - (result.distance - dense_min) / dense_range;
        scores.entry(result.id).or_insert((None, None)).0 = Some(normalized);
    }

    // Normalize sparse scores (already similarity-based)
    let sparse_max = sparse_results
        .iter()
        .map(|r| r.score)
        .fold(f32::NEG_INFINITY, f32::max);
    let sparse_max = sparse_max.max(1e-6);

    for result in sparse_results {
        let normalized = result.score / sparse_max;
        scores.entry(result.id).or_insert((None, None)).1 = Some(normalized);
    }

    // Combine scores
    let mut results: Vec<HybridResult> = scores
        .into_iter()
        .map(|(id, (dense, sparse))| {
            let dense_contrib = dense.unwrap_or(0.0) * alpha;
            let sparse_contrib = sparse.unwrap_or(0.0) * (1.0 - alpha);
            HybridResult {
                id,
                score: dense_contrib + sparse_contrib,
                dense_score: dense,
                sparse_score: sparse,
            }
        })
        .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
    results
}

/// Distribution-Based Score Fusion
///
/// Normalizes scores based on their distribution (z-score normalization)
pub fn dbsf_fusion(
    dense_results: &[DistanceResult],
    sparse_results: &[SparseResult],
) -> Vec<HybridResult> {
    let mut scores: HashMap<NodeId, (Option<f32>, Option<f32>)> = HashMap::new();

    // Z-score normalize dense (convert distance to similarity first)
    if !dense_results.is_empty() {
        let similarities: Vec<f32> = dense_results
            .iter()
            .map(|r| 1.0 / (1.0 + r.distance))
            .collect();
        let mean: f32 = similarities.iter().sum::<f32>() / similarities.len() as f32;
        let variance: f32 = similarities.iter().map(|s| (s - mean).powi(2)).sum::<f32>()
            / similarities.len() as f32;
        let std_dev = variance.sqrt().max(1e-6);

        for (result, sim) in dense_results.iter().zip(similarities.iter()) {
            let z_score = (sim - mean) / std_dev;
            scores.entry(result.id).or_insert((None, None)).0 = Some(z_score);
        }
    }

    // Z-score normalize sparse
    if !sparse_results.is_empty() {
        let mean: f32 =
            sparse_results.iter().map(|r| r.score).sum::<f32>() / sparse_results.len() as f32;
        let variance: f32 = sparse_results
            .iter()
            .map(|r| (r.score - mean).powi(2))
            .sum::<f32>()
            / sparse_results.len() as f32;
        let std_dev = variance.sqrt().max(1e-6);

        for result in sparse_results {
            let z_score = (result.score - mean) / std_dev;
            scores.entry(result.id).or_insert((None, None)).1 = Some(z_score);
        }
    }

    // Sum z-scores
    let mut results: Vec<HybridResult> = scores
        .into_iter()
        .map(|(id, (dense, sparse))| HybridResult {
            id,
            score: dense.unwrap_or(0.0) + sparse.unwrap_or(0.0),
            dense_score: dense,
            sparse_score: sparse,
        })
        .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
    results
}

// ============================================================================
// Hybrid Result
// ============================================================================

/// Result from hybrid search
#[derive(Debug, Clone)]
pub struct HybridResult {
    /// Document ID
    pub id: NodeId,
    /// Combined score
    pub score: f32,
    /// Score from dense search (if present)
    pub dense_score: Option<f32>,
    /// Score from sparse search (if present)
    pub sparse_score: Option<f32>,
}

// ============================================================================
// Hybrid Search
// ============================================================================

/// Hybrid search combining dense and sparse retrieval
pub struct HybridSearch<'a> {
    /// Dense index (HNSW)
    dense_index: &'a HnswIndex,
    /// Sparse index (BM25)
    sparse_index: &'a SparseIndex,
    /// Optional metadata store for filtering
    metadata: Option<&'a MetadataStore>,
}

impl<'a> HybridSearch<'a> {
    /// Create a new hybrid search
    pub fn new(dense_index: &'a HnswIndex, sparse_index: &'a SparseIndex) -> Self {
        Self {
            dense_index,
            sparse_index,
            metadata: None,
        }
    }

    /// Add metadata store for filtering
    pub fn with_metadata(mut self, metadata: &'a MetadataStore) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Create a query builder
    pub fn query(&'a self) -> HybridQueryBuilder<'a> {
        HybridQueryBuilder::new(self)
    }

    /// Execute hybrid search
    pub fn search(
        &self,
        query_vector: Option<&[f32]>,
        query_text: Option<&str>,
        k: usize,
        fusion: FusionMethod,
        pre_filter: Option<&HashSet<NodeId>>,
        post_filter: Option<&dyn Fn(&HybridResult) -> bool>,
    ) -> Vec<HybridResult> {
        // Fetch more results for filtering
        let fetch_k = k * 3;

        // Dense search
        let dense_results = if let Some(vector) = query_vector {
            if let Some(filter) = pre_filter {
                self.dense_index.search_filtered(vector, fetch_k, filter)
            } else {
                self.dense_index.search(vector, fetch_k)
            }
        } else {
            Vec::new()
        };

        // Sparse search
        let sparse_results = if let Some(text) = query_text {
            let mut results = self.sparse_index.search(text, fetch_k);
            // Apply pre-filter to sparse results
            if let Some(filter) = pre_filter {
                results.retain(|r| filter.contains(&r.id));
            }
            results
        } else {
            Vec::new()
        };

        // Fuse results
        let mut fused = match fusion {
            FusionMethod::RRF(k_param) => {
                reciprocal_rank_fusion(&dense_results, &sparse_results, k_param)
            }
            FusionMethod::Linear(alpha) => linear_fusion(&dense_results, &sparse_results, alpha),
            FusionMethod::DBSF => dbsf_fusion(&dense_results, &sparse_results),
        };

        // Apply post-filter
        if let Some(filter_fn) = post_filter {
            fused.retain(filter_fn);
        }

        // Return top k
        fused.truncate(k);
        fused
    }

    /// Dense-only search (for comparison)
    pub fn search_dense(&self, query_vector: &[f32], k: usize) -> Vec<DistanceResult> {
        self.dense_index.search(query_vector, k)
    }

    /// Sparse-only search (for comparison)
    pub fn search_sparse(&self, query_text: &str, k: usize) -> Vec<SparseResult> {
        self.sparse_index.search(query_text, k)
    }
}

// ============================================================================
// Query Builder
// ============================================================================

/// Builder for hybrid queries
pub struct HybridQueryBuilder<'a> {
    search: &'a HybridSearch<'a>,
    query_vector: Option<Vec<f32>>,
    query_text: Option<String>,
    k: usize,
    fusion: FusionMethod,
    pre_filter_ids: Option<HashSet<NodeId>>,
    metadata_filter: Option<MetadataFilter>,
}

impl<'a> HybridQueryBuilder<'a> {
    fn new(search: &'a HybridSearch<'a>) -> Self {
        Self {
            search,
            query_vector: None,
            query_text: None,
            k: 10,
            fusion: FusionMethod::default(),
            pre_filter_ids: None,
            metadata_filter: None,
        }
    }

    /// Set the query vector for dense search
    pub fn with_vector(mut self, vector: Vec<f32>) -> Self {
        self.query_vector = Some(vector);
        self
    }

    /// Set the query text for sparse search
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.query_text = Some(text.into());
        self
    }

    /// Set both vector and text
    pub fn with_both(self, vector: Vec<f32>, text: impl Into<String>) -> Self {
        self.with_vector(vector).with_text(text)
    }

    /// Set number of results to return
    pub fn top_k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }

    /// Set fusion method
    pub fn fusion(mut self, method: FusionMethod) -> Self {
        self.fusion = method;
        self
    }

    /// Use RRF fusion
    pub fn rrf(mut self, k: usize) -> Self {
        self.fusion = FusionMethod::RRF(k);
        self
    }

    /// Use linear fusion with alpha weight for dense
    pub fn linear(mut self, alpha: f32) -> Self {
        self.fusion = FusionMethod::Linear(alpha);
        self
    }

    /// Pre-filter by document IDs
    pub fn filter_ids(mut self, ids: HashSet<NodeId>) -> Self {
        self.pre_filter_ids = Some(ids);
        self
    }

    /// Pre-filter by metadata
    pub fn filter_metadata(mut self, filter: MetadataFilter) -> Self {
        self.metadata_filter = Some(filter);
        self
    }

    /// Execute the query
    pub fn execute(self) -> Vec<HybridResult> {
        // Build pre-filter from metadata if available
        let pre_filter = if let Some(meta_filter) = &self.metadata_filter {
            if let Some(meta_store) = self.search.metadata {
                // Use MetadataStore's filter method
                let matching_ids = meta_store.filter(meta_filter);

                // Intersect with explicit ID filter if present
                if let Some(ref explicit_ids) = self.pre_filter_ids {
                    Some(matching_ids.intersection(explicit_ids).copied().collect())
                } else {
                    Some(matching_ids)
                }
            } else {
                self.pre_filter_ids.clone()
            }
        } else {
            self.pre_filter_ids.clone()
        };

        self.search.search(
            self.query_vector.as_deref(),
            self.query_text.as_deref(),
            self.k,
            self.fusion,
            pre_filter.as_ref(),
            None,
        )
    }
}

// ============================================================================
// Re-ranking
// ============================================================================

/// Re-ranker for adjusting hybrid search results
pub trait Reranker: Send + Sync {
    /// Re-rank the results, returning adjusted scores
    fn rerank(&self, results: &[HybridResult], query: &str) -> Vec<(NodeId, f32)>;
}

/// Simple re-ranker that boosts exact matches
pub struct ExactMatchReranker {
    /// Boost factor for exact matches
    pub boost: f32,
}

impl Default for ExactMatchReranker {
    fn default() -> Self {
        Self { boost: 2.0 }
    }
}

impl Reranker for ExactMatchReranker {
    fn rerank(&self, results: &[HybridResult], _query: &str) -> Vec<(NodeId, f32)> {
        // This is a placeholder - real implementation would check document content
        results.iter().map(|r| (r.id, r.score)).collect()
    }
}

/// Re-ranking pipeline
pub struct RerankerPipeline {
    stages: Vec<Box<dyn Reranker>>,
}

impl RerankerPipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn add_stage(mut self, reranker: Box<dyn Reranker>) -> Self {
        self.stages.push(reranker);
        self
    }

    pub fn rerank(&self, mut results: Vec<HybridResult>, query: &str) -> Vec<HybridResult> {
        for stage in &self.stages {
            let reranked = stage.rerank(&results, query);
            let score_map: HashMap<NodeId, f32> = reranked.into_iter().collect();

            for result in &mut results {
                if let Some(&new_score) = score_map.get(&result.id) {
                    result.score = new_score;
                }
            }

            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.id.cmp(&b.id))
            });
        }

        results
    }
}

impl Default for RerankerPipeline {
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
    fn test_tokenize() {
        let tokens = tokenize("Hello, World! This is a test-case.");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"test-case".to_string()));
        assert!(!tokens.contains(&"a".to_string())); // Single char filtered
    }

    #[test]
    fn test_sparse_index() {
        let mut index = SparseIndex::new();

        index.index_text(0, "remote code execution vulnerability");
        index.index_text(1, "cross-site scripting XSS vulnerability");
        index.index_text(2, "SQL injection database vulnerability");

        assert_eq!(index.len(), 3);

        let results = index.search("code execution", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, 0); // Best match
    }

    #[test]
    fn test_sparse_remove() {
        let mut index = SparseIndex::new();

        index.index_text(0, "document one");
        index.index_text(1, "document two");

        assert_eq!(index.len(), 2);

        index.remove(0);
        assert_eq!(index.len(), 1);

        let results = index.search("document", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 1);
    }

    #[test]
    fn test_rrf_fusion() {
        let dense = vec![
            DistanceResult::new(1, 0.1),
            DistanceResult::new(2, 0.2),
            DistanceResult::new(3, 0.3),
        ];

        let sparse = vec![
            SparseResult { id: 2, score: 5.0 },
            SparseResult { id: 4, score: 4.0 },
            SparseResult { id: 1, score: 3.0 },
        ];

        let fused = reciprocal_rank_fusion(&dense, &sparse, 60);

        // IDs 1 and 2 should have highest scores (appear in both)
        let top_ids: Vec<NodeId> = fused.iter().take(2).map(|r| r.id).collect();
        assert!(top_ids.contains(&1));
        assert!(top_ids.contains(&2));
    }

    #[test]
    fn test_linear_fusion() {
        let dense = vec![
            DistanceResult::new(1, 0.1), // closest
            DistanceResult::new(2, 0.5),
        ];

        let sparse = vec![
            SparseResult { id: 2, score: 10.0 }, // best sparse
            SparseResult { id: 1, score: 5.0 },
        ];

        // With high alpha (dense-weighted)
        let fused_dense = linear_fusion(&dense, &sparse, 0.9);
        assert_eq!(fused_dense[0].id, 1); // Dense winner

        // With low alpha (sparse-weighted)
        let fused_sparse = linear_fusion(&dense, &sparse, 0.1);
        assert_eq!(fused_sparse[0].id, 2); // Sparse winner
    }

    #[test]
    fn test_bm25_scoring() {
        let mut index = SparseIndex::new();

        // Document with more relevant terms should score higher
        index.index_text(0, "vulnerability vulnerability vulnerability");
        index.index_text(1, "vulnerability in system");
        index.index_text(2, "no relevant terms here");

        let results = index.search("vulnerability", 10);

        // Doc 0 has highest TF
        assert_eq!(results[0].id, 0);
        assert!(results[0].score > results[1].score);
    }
}
