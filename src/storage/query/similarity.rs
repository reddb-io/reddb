//! Similarity Search Integration for Query Engine
//!
//! Provides vector similarity search capabilities integrated with
//! the query engine for semantic search and nearest neighbor queries.

use super::filter::Filter;
use super::sort::QueryLimits;
use crate::storage::engine::distance::DistanceMetric;
use crate::storage::engine::vector_store::{SearchResult, VectorCollection, VectorId};
use crate::storage::schema::Value;
use std::collections::HashMap;

/// Dense vector wrapper for similarity queries
#[derive(Debug, Clone)]
pub struct DenseVector {
    values: Vec<f32>,
}

impl DenseVector {
    pub fn new(values: Vec<f32>) -> Self {
        Self { values }
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.values
    }
}

impl From<Vec<f32>> for DenseVector {
    fn from(values: Vec<f32>) -> Self {
        Self { values }
    }
}

/// Similarity query parameters
#[derive(Debug, Clone)]
pub struct SimilarityQuery {
    /// Query vector
    pub vector: DenseVector,
    /// Number of neighbors to find
    pub k: usize,
    /// Distance metric
    pub distance: DistanceMetric,
    /// Optional filter to apply before similarity search
    pub filter: Option<Filter>,
    /// Number of probes for IVF index (if applicable)
    pub n_probes: Option<usize>,
    /// Distance threshold (for range queries)
    pub distance_threshold: Option<f32>,
}

impl SimilarityQuery {
    /// Create a new similarity query
    pub fn new(vector: DenseVector, k: usize) -> Self {
        Self {
            vector,
            k,
            distance: DistanceMetric::Cosine,
            filter: None,
            n_probes: None,
            distance_threshold: None,
        }
    }

    /// Set distance metric
    pub fn with_distance(mut self, distance: DistanceMetric) -> Self {
        self.distance = distance;
        self
    }

    /// Set pre-filter
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Set number of IVF probes
    pub fn with_probes(mut self, n_probes: usize) -> Self {
        self.n_probes = Some(n_probes);
        self
    }

    /// Set distance threshold for range query
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.distance_threshold = Some(threshold);
        self
    }
}

/// Similarity search result with metadata
#[derive(Debug, Clone)]
pub struct SimilarityResult {
    /// Vector ID
    pub id: VectorId,
    /// Distance to query
    pub distance: f32,
    /// Similarity score (1 - normalized_distance for bounded metrics)
    pub score: f32,
    /// Associated metadata (optional)
    pub metadata: Option<HashMap<String, Value>>,
}

impl SimilarityResult {
    /// Create a new result
    pub fn new(id: VectorId, distance: f32) -> Self {
        Self {
            id,
            distance,
            score: 1.0 / (1.0 + distance), // Simple similarity transform
            metadata: None,
        }
    }

    /// Create with score conversion based on distance metric
    pub fn with_metric(id: VectorId, distance: f32, metric: DistanceMetric) -> Self {
        let score = match metric {
            DistanceMetric::Cosine => 1.0 - distance, // Cosine: 0 = identical, 2 = opposite
            DistanceMetric::InnerProduct => -distance, // Negated dot product
            DistanceMetric::L2 => 1.0 / (1.0 + distance),
        };

        Self {
            id,
            distance,
            score: score.max(0.0),
            metadata: None,
        }
    }

    /// Add metadata
    pub fn with_metadata(mut self, metadata: HashMap<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Result set from similarity search
#[derive(Debug, Clone)]
pub struct SimilarityResultSet {
    /// Results sorted by distance
    pub results: Vec<SimilarityResult>,
    /// Query vector dimension
    pub dimension: usize,
    /// Distance metric used
    pub distance: DistanceMetric,
    /// Total vectors searched (for approximate search)
    pub vectors_searched: Option<usize>,
    /// Search time in microseconds
    pub search_time_us: u64,
}

impl SimilarityResultSet {
    /// Create empty result set
    pub fn empty(dimension: usize, distance: DistanceMetric) -> Self {
        Self {
            results: Vec::new(),
            dimension,
            distance,
            vectors_searched: None,
            search_time_us: 0,
        }
    }

    /// Create from search results
    pub fn from_results(
        results: Vec<SearchResult>,
        dimension: usize,
        distance: DistanceMetric,
    ) -> Self {
        let similarity_results = results
            .into_iter()
            .map(|r| SimilarityResult::with_metric(r.id, r.distance, distance))
            .collect();

        Self {
            results: similarity_results,
            dimension,
            distance,
            vectors_searched: None,
            search_time_us: 0,
        }
    }

    /// Get number of results
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Get top-k IDs
    pub fn top_ids(&self, k: usize) -> Vec<VectorId> {
        self.results.iter().take(k).map(|r| r.id).collect()
    }

    /// Get results above score threshold
    pub fn above_score(&self, threshold: f32) -> Vec<&SimilarityResult> {
        self.results
            .iter()
            .filter(|r| r.score >= threshold)
            .collect()
    }

    /// Apply limits
    pub fn apply_limits(mut self, limits: QueryLimits) -> Self {
        self.results = limits.apply(self.results);
        self
    }
}

/// Trait for vector index that supports similarity search
pub trait VectorIndex: Send + Sync {
    /// Search for k nearest neighbors
    fn search(&self, query: &DenseVector, k: usize) -> Vec<SearchResult>;

    /// Search with optional parameters
    fn search_with_params(
        &self,
        query: &DenseVector,
        k: usize,
        n_probes: Option<usize>,
    ) -> Vec<SearchResult>;

    /// Get vector by ID
    fn get(&self, id: VectorId) -> Option<DenseVector>;

    /// Get dimension
    fn dimension(&self) -> usize;

    /// Get distance metric
    fn distance_metric(&self) -> DistanceMetric;

    /// Get number of indexed vectors
    fn len(&self) -> usize;

    /// Check if empty
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl VectorIndex for VectorCollection {
    fn search(&self, query: &DenseVector, k: usize) -> Vec<SearchResult> {
        VectorCollection::search(self, query.as_slice(), k)
    }

    fn search_with_params(
        &self,
        query: &DenseVector,
        k: usize,
        _n_probes: Option<usize>,
    ) -> Vec<SearchResult> {
        VectorCollection::search(self, query.as_slice(), k)
    }

    fn get(&self, id: VectorId) -> Option<DenseVector> {
        VectorCollection::get(self, id).map(|vec| DenseVector::new(vec.clone()))
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn distance_metric(&self) -> DistanceMetric {
        self.metric
    }

    fn len(&self) -> usize {
        self.len()
    }
}

/// Execute similarity search
pub fn execute_similarity_search(
    index: &dyn VectorIndex,
    query: &SimilarityQuery,
) -> SimilarityResultSet {
    let start = std::time::Instant::now();

    // Perform search
    let results = if let Some(threshold) = query.distance_threshold {
        // Range query: get more results then filter
        let candidates = index.search_with_params(&query.vector, query.k * 10, query.n_probes);
        candidates
            .into_iter()
            .filter(|r| r.distance <= threshold)
            .take(query.k)
            .collect()
    } else {
        index.search_with_params(&query.vector, query.k, query.n_probes)
    };

    let search_time = start.elapsed().as_micros() as u64;

    let mut result_set =
        SimilarityResultSet::from_results(results, index.dimension(), index.distance_metric());
    result_set.search_time_us = search_time;
    result_set.vectors_searched = Some(index.len());

    result_set
}

/// Hybrid search combining filter and similarity
pub fn execute_hybrid_search<F>(
    index: &dyn VectorIndex,
    query: &SimilarityQuery,
    get_metadata: F,
    filter_matches: impl Fn(VectorId, &Filter) -> bool,
) -> SimilarityResultSet
where
    F: Fn(VectorId) -> Option<HashMap<String, Value>>,
{
    let start = std::time::Instant::now();

    // Get more candidates than needed to account for filtering
    let over_fetch = if query.filter.is_some() { 10 } else { 1 };
    let candidates = index.search_with_params(&query.vector, query.k * over_fetch, query.n_probes);

    // Apply filter and collect results
    let results: Vec<SimilarityResult> = candidates
        .into_iter()
        .filter(|r| {
            if let Some(filter) = &query.filter {
                filter_matches(r.id, filter)
            } else {
                true
            }
        })
        .take(query.k)
        .map(|r| {
            let mut result =
                SimilarityResult::with_metric(r.id, r.distance, index.distance_metric());
            if let Some(meta) = get_metadata(r.id) {
                result = result.with_metadata(meta);
            }
            result
        })
        .collect();

    let search_time = start.elapsed().as_micros() as u64;

    SimilarityResultSet {
        results,
        dimension: index.dimension(),
        distance: index.distance_metric(),
        vectors_searched: Some(index.len()),
        search_time_us: search_time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_index() -> VectorCollection {
        let mut collection = VectorCollection::new("test", 3).with_metric(DistanceMetric::Cosine);

        // Add test vectors
        let _ = collection.insert(vec![1.0, 0.0, 0.0], None);
        let _ = collection.insert(vec![0.0, 1.0, 0.0], None);
        let _ = collection.insert(vec![0.0, 0.0, 1.0], None);
        let _ = collection.insert(vec![0.7, 0.7, 0.0], None);
        let _ = collection.insert(vec![0.5, 0.5, 0.7], None);

        collection
    }

    #[test]
    fn test_similarity_query_basic() {
        let index = create_test_index();

        let query = SimilarityQuery::new(DenseVector::new(vec![1.0, 0.0, 0.0]), 3);
        let results = execute_similarity_search(&index, &query);

        assert_eq!(results.len(), 3);
        assert_eq!(results.results[0].id, 0); // Exact match
        assert!(results.results[0].distance < 0.01);
    }

    #[test]
    fn test_similarity_result_score() {
        // Cosine distance 0 = identical
        let result = SimilarityResult::with_metric(1, 0.0, DistanceMetric::Cosine);
        assert!((result.score - 1.0).abs() < 0.01);

        // Cosine distance 1 = orthogonal
        let result = SimilarityResult::with_metric(1, 1.0, DistanceMetric::Cosine);
        assert!(result.score < 0.01);
    }

    #[test]
    fn test_similarity_result_set_top_ids() {
        let index = create_test_index();

        let query = SimilarityQuery::new(DenseVector::new(vec![1.0, 0.0, 0.0]), 5);
        let results = execute_similarity_search(&index, &query);

        let top3 = results.top_ids(3);
        assert_eq!(top3.len(), 3);
        assert_eq!(top3[0], 0);
    }

    #[test]
    fn test_similarity_threshold() {
        let index = create_test_index();

        // Query with distance threshold
        let query =
            SimilarityQuery::new(DenseVector::new(vec![1.0, 0.0, 0.0]), 10).with_threshold(0.5);

        let results = execute_similarity_search(&index, &query);

        // Only vectors within threshold should be returned
        for result in &results.results {
            assert!(result.distance <= 0.5);
        }
    }

    #[test]
    fn test_vector_index_trait() {
        let index = create_test_index();

        let index_ref: &dyn VectorIndex = &index;

        assert_eq!(index_ref.dimension(), 3);
        assert_eq!(index_ref.len(), 5);
        assert!(!index_ref.is_empty());

        let vec = index_ref.get(0).unwrap();
        assert_eq!(vec.as_slice(), &[1.0, 0.0, 0.0]);
    }

    #[test]
    fn test_above_score_filter() {
        let results = SimilarityResultSet {
            results: vec![
                SimilarityResult::new(1, 0.1), // score ~0.91
                SimilarityResult::new(2, 0.5), // score ~0.67
                SimilarityResult::new(3, 2.0), // score ~0.33
            ],
            dimension: 3,
            distance: DistanceMetric::L2,
            vectors_searched: Some(100),
            search_time_us: 100,
        };

        let above_05 = results.above_score(0.5);
        assert_eq!(above_05.len(), 2); // 0.91 and 0.67 are >= 0.5
    }

    #[test]
    fn test_similarity_query_builder() {
        let query = SimilarityQuery::new(DenseVector::new(vec![1.0, 0.0, 0.0]), 10)
            .with_distance(DistanceMetric::L2)
            .with_probes(5)
            .with_threshold(1.0);

        assert_eq!(query.k, 10);
        assert_eq!(query.distance, DistanceMetric::L2);
        assert_eq!(query.n_probes, Some(5));
        assert_eq!(query.distance_threshold, Some(1.0));
    }

    #[test]
    fn test_hybrid_search_with_filter() {
        let index = create_test_index();

        // Mock metadata
        let metadata: HashMap<VectorId, HashMap<String, Value>> = [
            (
                1,
                [("category".to_string(), Value::text("A".to_string()))]
                    .into_iter()
                    .collect(),
            ),
            (
                2,
                [("category".to_string(), Value::text("B".to_string()))]
                    .into_iter()
                    .collect(),
            ),
            (
                3,
                [("category".to_string(), Value::text("A".to_string()))]
                    .into_iter()
                    .collect(),
            ),
            (
                4,
                [("category".to_string(), Value::text("B".to_string()))]
                    .into_iter()
                    .collect(),
            ),
            (
                5,
                [("category".to_string(), Value::text("A".to_string()))]
                    .into_iter()
                    .collect(),
            ),
        ]
        .into_iter()
        .collect();

        let filter = Filter::eq("category", Value::text("A".to_string()));
        let query = SimilarityQuery::new(DenseVector::new(vec![1.0, 0.0, 0.0]), 5)
            .with_filter(filter.clone());

        let results = execute_hybrid_search(
            &index,
            &query,
            |id| metadata.get(&id).cloned(),
            |id, filter| {
                if let Some(meta) = metadata.get(&id) {
                    filter.evaluate(&|col| meta.get(col).cloned())
                } else {
                    false
                }
            },
        );

        // Should only return vectors with category "A"
        assert!(results.len() <= 3); // Only 3 vectors have category A
        for result in &results.results {
            if let Some(meta) = &result.metadata {
                assert_eq!(meta.get("category"), Some(&Value::text("A".to_string())));
            }
        }
    }

    #[test]
    fn test_apply_limits() {
        let results = SimilarityResultSet {
            results: (0..10)
                .map(|i| SimilarityResult::new(i, i as f32 * 0.1))
                .collect(),
            dimension: 3,
            distance: DistanceMetric::L2,
            vectors_searched: Some(100),
            search_time_us: 100,
        };

        let limited = results.apply_limits(QueryLimits::none().offset(2).limit(3));
        assert_eq!(limited.len(), 3);
        assert_eq!(limited.results[0].id, 2);
    }
}
