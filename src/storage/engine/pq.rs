//! Product Quantization (PQ) for Vector Compression
//!
//! Compresses high-dimensional vectors by splitting into sub-vectors
//! and quantizing each independently to a codebook.
//!
//! # Design
//!
//! - Split D-dimensional vector into M sub-vectors of D/M dimensions
//! - Each sub-vector is quantized to one of K centroids (codebook)
//! - Storage: M bytes per vector (with K=256)
//! - Distance: Asymmetric distance computation using lookup tables
//!
//! # Example
//!
//! ```ignore
//! let mut pq = ProductQuantizer::new(PQConfig {
//!     dimension: 384,
//!     n_subvectors: 48,      // 48 sub-vectors of 8 dimensions each
//!     n_centroids: 256,      // 8-bit codes
//! });
//!
//! // Train on sample vectors
//! pq.train(&training_vectors);
//!
//! // Encode vectors
//! let codes = pq.encode(&vector);
//!
//! // Compute distance using lookup table
//! let distances = pq.compute_distances(&query, &code_database);
//! ```

use std::collections::HashMap;

use super::distance::{cmp_f32, l2_squared_simd};
use super::hnsw::NodeId;

/// PQ configuration
#[derive(Clone, Debug)]
pub struct PQConfig {
    /// Full vector dimension
    pub dimension: usize,
    /// Number of sub-vectors (M)
    pub n_subvectors: usize,
    /// Number of centroids per sub-vector (K, typically 256)
    pub n_centroids: usize,
    /// Maximum k-means iterations during training
    pub max_iterations: usize,
}

impl Default for PQConfig {
    fn default() -> Self {
        Self {
            dimension: 128,
            n_subvectors: 8,
            n_centroids: 256,
            max_iterations: 25,
        }
    }
}

impl PQConfig {
    pub fn new(dimension: usize, n_subvectors: usize) -> Self {
        assert!(
            dimension % n_subvectors == 0,
            "dimension must be divisible by n_subvectors"
        );
        Self {
            dimension,
            n_subvectors,
            n_centroids: 256,
            max_iterations: 25,
        }
    }

    /// Get sub-vector dimension
    pub fn subvector_dim(&self) -> usize {
        self.dimension / self.n_subvectors
    }
}

/// Codebook for a single sub-vector
#[derive(Clone)]
struct Codebook {
    /// Centroids for this sub-vector [n_centroids x subvector_dim]
    centroids: Vec<Vec<f32>>,
    /// Sub-vector dimension
    dim: usize,
}

impl Codebook {
    fn new(dim: usize, n_centroids: usize) -> Self {
        Self {
            centroids: vec![vec![0.0; dim]; n_centroids],
            dim,
        }
    }

    /// Train codebook using k-means on sub-vectors
    fn train(&mut self, subvectors: &[Vec<f32>], max_iterations: usize) {
        if subvectors.is_empty() {
            return;
        }

        let k = self.centroids.len();

        // Initialize centroids using sampling
        let step = subvectors.len().max(1) / k.max(1);
        for (i, centroid) in self.centroids.iter_mut().enumerate() {
            let idx = (i * step).min(subvectors.len() - 1);
            *centroid = subvectors[idx].clone();
        }

        // Run k-means
        for _ in 0..max_iterations {
            // Assign to nearest centroid
            let mut assignments: Vec<Vec<usize>> = vec![Vec::new(); k];
            for (i, sv) in subvectors.iter().enumerate() {
                let nearest = self.find_nearest(sv);
                assignments[nearest].push(i);
            }

            // Update centroids
            let mut converged = true;
            for (ci, indices) in assignments.iter().enumerate() {
                if indices.is_empty() {
                    continue;
                }

                let mut new_centroid = vec![0.0f32; self.dim];
                for &idx in indices {
                    for (j, &val) in subvectors[idx].iter().enumerate() {
                        new_centroid[j] += val;
                    }
                }
                for val in &mut new_centroid {
                    *val /= indices.len() as f32;
                }

                // Check convergence
                let shift = l2_squared_simd(&new_centroid, &self.centroids[ci]).sqrt();
                if shift > 1e-4 {
                    converged = false;
                }

                self.centroids[ci] = new_centroid;
            }

            if converged {
                break;
            }
        }
    }

    /// Find nearest centroid index
    fn find_nearest(&self, subvector: &[f32]) -> usize {
        self.centroids
            .iter()
            .enumerate()
            .map(|(i, c)| (i, l2_squared_simd(subvector, c)))
            .min_by(|(_, a), (_, b)| cmp_f32(*a, *b))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    /// Compute distance lookup table for a query sub-vector
    fn compute_distance_table(&self, query_subvector: &[f32]) -> Vec<f32> {
        self.centroids
            .iter()
            .map(|c| l2_squared_simd(query_subvector, c))
            .collect()
    }
}

/// PQ code for a single vector (M bytes)
pub type PQCode = Vec<u8>;

/// Product Quantizer
pub struct ProductQuantizer {
    config: PQConfig,
    /// Codebooks for each sub-vector [M codebooks]
    codebooks: Vec<Codebook>,
    /// Whether the quantizer has been trained
    trained: bool,
}

impl ProductQuantizer {
    /// Create a new product quantizer
    pub fn new(config: PQConfig) -> Self {
        let subdim = config.subvector_dim();
        let codebooks = (0..config.n_subvectors)
            .map(|_| Codebook::new(subdim, config.n_centroids))
            .collect();

        Self {
            config,
            codebooks,
            trained: false,
        }
    }

    /// Create with default config for dimension
    pub fn with_dimension(dimension: usize) -> Self {
        // Choose sensible defaults
        let n_subvectors = if dimension >= 64 { 8 } else { 4 };
        Self::new(PQConfig::new(dimension, n_subvectors))
    }

    /// Train the product quantizer
    pub fn train(&mut self, vectors: &[Vec<f32>]) {
        if vectors.is_empty() {
            return;
        }

        let subdim = self.config.subvector_dim();

        // Train each codebook independently
        for (m, codebook) in self.codebooks.iter_mut().enumerate() {
            // Extract sub-vectors for this codebook
            let subvectors: Vec<Vec<f32>> = vectors
                .iter()
                .map(|v| v[m * subdim..(m + 1) * subdim].to_vec())
                .collect();

            codebook.train(&subvectors, self.config.max_iterations);
        }

        self.trained = true;
    }

    /// Encode a single vector to PQ codes
    pub fn encode(&self, vector: &[f32]) -> PQCode {
        let subdim = self.config.subvector_dim();

        self.codebooks
            .iter()
            .enumerate()
            .map(|(m, codebook)| {
                let subvector = &vector[m * subdim..(m + 1) * subdim];
                codebook.find_nearest(subvector) as u8
            })
            .collect()
    }

    /// Encode multiple vectors
    pub fn encode_batch(&self, vectors: &[Vec<f32>]) -> Vec<PQCode> {
        vectors.iter().map(|v| self.encode(v)).collect()
    }

    /// Decode PQ codes back to approximate vector
    pub fn decode(&self, code: &PQCode) -> Vec<f32> {
        let subdim = self.config.subvector_dim();
        let mut vector = Vec::with_capacity(self.config.dimension);

        for (m, &c) in code.iter().enumerate() {
            let centroid = &self.codebooks[m].centroids[c as usize];
            vector.extend_from_slice(centroid);
        }

        vector
    }

    /// Compute asymmetric distances from query to all codes
    pub fn compute_distances(&self, query: &[f32], codes: &[PQCode]) -> Vec<f32> {
        // Build distance lookup tables for each sub-vector
        let subdim = self.config.subvector_dim();
        let tables: Vec<Vec<f32>> = self
            .codebooks
            .iter()
            .enumerate()
            .map(|(m, codebook)| {
                let subquery = &query[m * subdim..(m + 1) * subdim];
                codebook.compute_distance_table(subquery)
            })
            .collect();

        // Compute distance for each code using table lookups
        codes
            .iter()
            .map(|code| {
                code.iter()
                    .enumerate()
                    .map(|(m, &c)| tables[m][c as usize])
                    .sum::<f32>()
                    .sqrt()
            })
            .collect()
    }

    /// Get compression ratio
    pub fn compression_ratio(&self) -> f32 {
        let original_bytes = self.config.dimension * 4; // f32 = 4 bytes
        let compressed_bytes = self.config.n_subvectors; // 1 byte per subvector
        original_bytes as f32 / compressed_bytes as f32
    }

    /// Get configuration
    pub fn config(&self) -> &PQConfig {
        &self.config
    }

    /// Check if trained
    pub fn is_trained(&self) -> bool {
        self.trained
    }
}

/// PQ-based index for compressed vector search
pub struct PQIndex {
    /// Product quantizer
    pq: ProductQuantizer,
    /// Stored codes
    codes: Vec<PQCode>,
    /// ID mapping
    ids: Vec<NodeId>,
    /// Reverse mapping
    id_to_idx: HashMap<NodeId, usize>,
    /// Original vectors (optional, for reranking)
    originals: Option<Vec<Vec<f32>>>,
    /// Next auto ID
    next_id: NodeId,
}

impl PQIndex {
    /// Create a new PQ index
    pub fn new(config: PQConfig) -> Self {
        Self {
            pq: ProductQuantizer::new(config),
            codes: Vec::new(),
            ids: Vec::new(),
            id_to_idx: HashMap::new(),
            originals: None,
            next_id: 0,
        }
    }

    /// Create and enable original vector storage for reranking
    pub fn with_originals(mut self) -> Self {
        self.originals = Some(Vec::new());
        self
    }

    /// Train the index
    pub fn train(&mut self, vectors: &[Vec<f32>]) {
        self.pq.train(vectors);
    }

    /// Add a vector
    pub fn add(&mut self, vector: Vec<f32>) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        self.add_with_id(id, vector);
        id
    }

    /// Add a vector with ID
    pub fn add_with_id(&mut self, id: NodeId, vector: Vec<f32>) {
        let code = self.pq.encode(&vector);
        let idx = self.codes.len();

        self.codes.push(code);
        self.ids.push(id);
        self.id_to_idx.insert(id, idx);

        if let Some(ref mut originals) = self.originals {
            originals.push(vector);
        }
    }

    /// Add multiple vectors
    pub fn add_batch(&mut self, vectors: Vec<Vec<f32>>) -> Vec<NodeId> {
        vectors.into_iter().map(|v| self.add(v)).collect()
    }

    /// Search for k nearest neighbors
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(NodeId, f32)> {
        if self.codes.is_empty() {
            return Vec::new();
        }

        let distances = self.pq.compute_distances(query, &self.codes);

        let mut results: Vec<(usize, f32)> = distances.into_iter().enumerate().collect();

        results.sort_by(|(_, a), (_, b)| cmp_f32(*a, *b));
        results.truncate(k);

        results
            .into_iter()
            .map(|(idx, dist)| (self.ids[idx], dist))
            .collect()
    }

    /// Search with reranking using original vectors
    pub fn search_rerank(&self, query: &[f32], k: usize, rerank_k: usize) -> Vec<(NodeId, f32)> {
        let originals = match &self.originals {
            Some(o) => o,
            None => return self.search(query, k),
        };

        // Get more candidates for reranking
        let candidates = self.search(query, rerank_k);

        // Rerank using original vectors
        let mut reranked: Vec<(NodeId, f32)> = candidates
            .into_iter()
            .map(|(id, _)| {
                let idx = self.id_to_idx[&id];
                let dist = l2_squared_simd(query, &originals[idx]).sqrt();
                (id, dist)
            })
            .collect();

        reranked.sort_by(|(_, a), (_, b)| cmp_f32(*a, *b));
        reranked.truncate(k);
        reranked
    }

    /// Get number of vectors
    pub fn len(&self) -> usize {
        self.codes.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Get compression ratio
    pub fn compression_ratio(&self) -> f32 {
        self.pq.compression_ratio()
    }

    /// Memory usage in bytes
    pub fn memory_usage(&self) -> usize {
        let code_bytes = self.codes.len() * self.pq.config.n_subvectors;
        let original_bytes = self
            .originals
            .as_ref()
            .map(|o| o.len() * self.pq.config.dimension * 4)
            .unwrap_or(0);
        code_bytes + original_bytes
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vector(dim: usize, seed: u64) -> Vec<f32> {
        (0..dim)
            .map(|i| ((seed * 1103515245 + i as u64 * 12345) % 1000) as f32 / 1000.0)
            .collect()
    }

    #[test]
    fn test_pq_encode_decode() {
        let config = PQConfig::new(16, 4);
        let mut pq = ProductQuantizer::new(config);

        // Generate training data
        let training: Vec<Vec<f32>> = (0..100).map(|i| random_vector(16, i)).collect();

        pq.train(&training);
        assert!(pq.is_trained());

        // Encode and decode
        let original = random_vector(16, 999);
        let code = pq.encode(&original);
        let decoded = pq.decode(&code);

        assert_eq!(code.len(), 4); // M sub-vectors
        assert_eq!(decoded.len(), 16);

        // Decoded should be an approximation (not exact)
        let reconstruction_error: f32 = original
            .iter()
            .zip(decoded.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum();
        assert!(reconstruction_error < 1.0); // Should be close
    }

    #[test]
    fn test_pq_compression_ratio() {
        let pq = ProductQuantizer::new(PQConfig::new(128, 8));
        // 128 floats * 4 bytes = 512 bytes original
        // 8 sub-vectors * 1 byte = 8 bytes compressed
        // Ratio = 512 / 8 = 64x
        assert_eq!(pq.compression_ratio(), 64.0);
    }

    #[test]
    fn test_pq_index_search() {
        let mut index = PQIndex::new(PQConfig::new(8, 4));

        // Training data
        let training: Vec<Vec<f32>> = (0..50).map(|i| random_vector(8, i)).collect();

        index.train(&training);

        // Add vectors
        for (i, v) in training.iter().enumerate() {
            index.add_with_id(i as u64, v.clone());
        }

        // Search
        let query = random_vector(8, 0);
        let results = index.search(&query, 5);

        assert_eq!(results.len(), 5);
        // First result should be the query itself (ID 0)
        assert_eq!(results[0].0, 0);
    }

    #[test]
    fn test_pq_distance_tables() {
        let config = PQConfig::new(8, 2);
        let mut pq = ProductQuantizer::new(config);

        let training: Vec<Vec<f32>> = vec![
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        ];

        pq.train(&training);

        let query = vec![0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
        let codes = pq.encode_batch(&training);
        let distances = pq.compute_distances(&query, &codes);

        assert_eq!(distances.len(), 2);
        // Distances should be approximately equal (equidistant from query)
        assert!((distances[0] - distances[1]).abs() < 0.1);
    }
}
