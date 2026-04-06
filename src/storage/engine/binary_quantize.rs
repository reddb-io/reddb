//! Binary Quantization for Vector Embeddings
//!
//! Compresses fp32 vectors to binary (1 bit per dimension) for ultra-fast
//! approximate nearest neighbor search using Hamming distance.
//!
//! # Compression Ratio
//!
//! - fp32: 4 bytes per dimension
//! - binary: 1 bit per dimension = 32x compression
//!
//! Example: 1024-dim vector
//! - fp32: 4096 bytes
//! - binary: 128 bytes
//!
//! # Algorithm
//!
//! Simple sign-based quantization:
//! - positive values → 1
//! - negative/zero values → 0
//!
//! For normalized embeddings (e.g., from sentence transformers),
//! this preserves ~95-97% of retrieval quality.
//!
//! # Usage
//!
//! ```ignore
//! // Quantize a vector
//! let binary = BinaryVector::from_f32(&embedding);
//!
//! // Compute Hamming distance (number of differing bits)
//! let distance = binary.hamming_distance(&other);
//!
//! // For retrieval: lower Hamming distance = more similar
//! ```
//!
//! # References
//!
//! - "Embedding Quantization" - HuggingFace Blog
//! - Binary embedding with Matryoshka representation learning

use std::cmp::Ordering;

/// Binary quantized vector stored as packed u64 words
#[derive(Clone, Debug)]
pub struct BinaryVector {
    /// Packed binary data (each u64 holds 64 dimensions)
    data: Vec<u64>,
    /// Original dimensionality
    dim: usize,
}

impl BinaryVector {
    /// Create a binary vector from fp32 values using sign-based quantization
    ///
    /// Positive values become 1, negative/zero become 0
    pub fn from_f32(values: &[f32]) -> Self {
        let dim = values.len();
        let n_words = (dim + 63) / 64; // Ceiling division
        let mut data = vec![0u64; n_words];

        for (i, &v) in values.iter().enumerate() {
            if v > 0.0 {
                let word_idx = i / 64;
                let bit_idx = i % 64;
                data[word_idx] |= 1u64 << bit_idx;
            }
        }

        Self { data, dim }
    }

    /// Create a binary vector from threshold-based quantization
    ///
    /// Values above threshold become 1, below become 0
    pub fn from_f32_threshold(values: &[f32], threshold: f32) -> Self {
        let dim = values.len();
        let n_words = (dim + 63) / 64;
        let mut data = vec![0u64; n_words];

        for (i, &v) in values.iter().enumerate() {
            if v > threshold {
                let word_idx = i / 64;
                let bit_idx = i % 64;
                data[word_idx] |= 1u64 << bit_idx;
            }
        }

        Self { data, dim }
    }

    /// Create a binary vector from median-based quantization
    ///
    /// Values above median become 1, below become 0.
    /// Better for non-normalized vectors.
    pub fn from_f32_median(values: &[f32]) -> Self {
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let median = if sorted.len() % 2 == 0 {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        } else {
            sorted[sorted.len() / 2]
        };

        Self::from_f32_threshold(values, median)
    }

    /// Create from raw packed data
    pub fn from_raw(data: Vec<u64>, dim: usize) -> Self {
        Self { data, dim }
    }

    /// Get the dimensionality of the original vector
    #[inline]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Get the packed binary data
    #[inline]
    pub fn data(&self) -> &[u64] {
        &self.data
    }

    /// Get size in bytes
    #[inline]
    pub fn size_bytes(&self) -> usize {
        self.data.len() * 8
    }

    /// Compute Hamming distance to another binary vector
    ///
    /// Hamming distance = number of positions where bits differ.
    /// Uses popcount which is a single CPU instruction on modern x86.
    #[inline]
    pub fn hamming_distance(&self, other: &Self) -> u32 {
        debug_assert_eq!(self.dim, other.dim, "Dimensions must match");

        hamming_distance_simd(&self.data, &other.data)
    }

    /// Compute normalized Hamming distance (0.0 to 1.0)
    ///
    /// 0.0 = identical, 1.0 = completely different
    #[inline]
    pub fn hamming_distance_normalized(&self, other: &Self) -> f32 {
        let dist = self.hamming_distance(other) as f32;
        dist / self.dim as f32
    }

    /// Convert Hamming distance to approximate cosine similarity
    ///
    /// For normalized embeddings, there's a relationship between
    /// Hamming distance and cosine similarity:
    /// cos_sim ≈ 1 - 2 * (hamming_dist / dim)
    #[inline]
    pub fn approx_cosine_similarity(&self, other: &Self) -> f32 {
        let normalized_dist = self.hamming_distance_normalized(other);
        1.0 - 2.0 * normalized_dist
    }
}

// ============================================================================
// Hamming Distance with SIMD
// ============================================================================

/// Compute Hamming distance between two packed binary vectors using SIMD
#[inline]
pub fn hamming_distance_simd(a: &[u64], b: &[u64]) -> u32 {
    debug_assert_eq!(a.len(), b.len(), "Vectors must have same length");

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("popcnt") {
            return unsafe { hamming_distance_popcnt(a, b) };
        }
    }

    hamming_distance_scalar(a, b)
}

/// Scalar fallback for Hamming distance
#[inline]
fn hamming_distance_scalar(a: &[u64], b: &[u64]) -> u32 {
    let mut count = 0u32;
    for (x, y) in a.iter().zip(b.iter()) {
        count += (x ^ y).count_ones();
    }
    count
}

/// SIMD-accelerated Hamming distance using popcount instruction
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "popcnt")]
#[inline]
unsafe fn hamming_distance_popcnt(a: &[u64], b: &[u64]) -> u32 {
    use std::arch::x86_64::_popcnt64;

    let mut count = 0i32;

    // Process 4 u64s at a time for better instruction-level parallelism
    let chunks = a.len() / 4;
    for i in 0..chunks {
        let idx = i * 4;
        let xor0 = a[idx] ^ b[idx];
        let xor1 = a[idx + 1] ^ b[idx + 1];
        let xor2 = a[idx + 2] ^ b[idx + 2];
        let xor3 = a[idx + 3] ^ b[idx + 3];

        count += _popcnt64(xor0 as i64);
        count += _popcnt64(xor1 as i64);
        count += _popcnt64(xor2 as i64);
        count += _popcnt64(xor3 as i64);
    }

    // Handle remaining elements
    for i in (chunks * 4)..a.len() {
        count += _popcnt64((a[i] ^ b[i]) as i64);
    }

    count as u32
}

// ============================================================================
// Batch Operations
// ============================================================================

/// Index of binary vectors for fast batch search
#[derive(Clone)]
pub struct BinaryIndex {
    /// All binary vectors (flattened: n_vectors * n_words)
    vectors: Vec<u64>,
    /// Number of u64 words per vector
    words_per_vector: usize,
    /// Number of vectors
    n_vectors: usize,
    /// Original dimension
    dim: usize,
}

impl BinaryIndex {
    /// Create a new binary index
    pub fn new(dim: usize) -> Self {
        let words_per_vector = (dim + 63) / 64;
        Self {
            vectors: Vec::new(),
            words_per_vector,
            n_vectors: 0,
            dim,
        }
    }

    /// Create with pre-allocated capacity
    pub fn with_capacity(dim: usize, capacity: usize) -> Self {
        let words_per_vector = (dim + 63) / 64;
        Self {
            vectors: Vec::with_capacity(capacity * words_per_vector),
            words_per_vector,
            n_vectors: 0,
            dim,
        }
    }

    /// Add a vector to the index
    pub fn add(&mut self, vector: &BinaryVector) {
        debug_assert_eq!(vector.dim, self.dim, "Dimension mismatch");
        self.vectors.extend_from_slice(&vector.data);
        self.n_vectors += 1;
    }

    /// Add a fp32 vector (will be quantized)
    pub fn add_f32(&mut self, vector: &[f32]) {
        let binary = BinaryVector::from_f32(vector);
        self.add(&binary);
    }

    /// Get number of vectors in the index
    #[inline]
    pub fn len(&self) -> usize {
        self.n_vectors
    }

    /// Check if index is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.n_vectors == 0
    }

    /// Get memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        self.vectors.len() * 8
    }

    /// Get a vector by index
    pub fn get(&self, idx: usize) -> Option<BinaryVector> {
        if idx >= self.n_vectors {
            return None;
        }
        let start = idx * self.words_per_vector;
        let end = start + self.words_per_vector;
        Some(BinaryVector::from_raw(
            self.vectors[start..end].to_vec(),
            self.dim,
        ))
    }

    /// Search for k nearest neighbors using Hamming distance
    ///
    /// Returns (index, hamming_distance) pairs sorted by distance.
    pub fn search(&self, query: &BinaryVector, k: usize) -> Vec<(usize, u32)> {
        if self.n_vectors == 0 {
            return Vec::new();
        }

        let k = k.min(self.n_vectors);
        let mut results: Vec<(usize, u32)> = Vec::with_capacity(self.n_vectors);

        // Compute distances to all vectors
        for i in 0..self.n_vectors {
            let start = i * self.words_per_vector;
            let end = start + self.words_per_vector;
            let dist = hamming_distance_simd(&query.data, &self.vectors[start..end]);
            results.push((i, dist));
        }

        // Partial sort to get top-k
        if k < self.n_vectors {
            results.select_nth_unstable_by_key(k - 1, |&(_, d)| d);
            results.truncate(k);
        }
        results.sort_by_key(|&(_, d)| d);

        results
    }

    /// Search from fp32 query (will be quantized)
    pub fn search_f32(&self, query: &[f32], k: usize) -> Vec<(usize, u32)> {
        let binary_query = BinaryVector::from_f32(query);
        self.search(&binary_query, k)
    }

    /// Batch search for multiple queries
    ///
    /// More efficient than individual searches due to cache locality.
    pub fn batch_search(&self, queries: &[BinaryVector], k: usize) -> Vec<Vec<(usize, u32)>> {
        queries.iter().map(|q| self.search(q, k)).collect()
    }
}

// ============================================================================
// Distance Result for Integration
// ============================================================================

/// Result from binary search with rescoring capability
#[derive(Debug, Clone)]
pub struct BinarySearchResult {
    /// Vector index
    pub id: usize,
    /// Hamming distance (lower = more similar)
    pub hamming_distance: u32,
    /// Optional rescored distance (set during int8/fp32 rescoring)
    pub rescored_distance: Option<f32>,
}

impl BinarySearchResult {
    pub fn new(id: usize, hamming_distance: u32) -> Self {
        Self {
            id,
            hamming_distance,
            rescored_distance: None,
        }
    }

    /// Get the final distance (rescored if available, otherwise Hamming)
    pub fn final_distance(&self) -> f32 {
        self.rescored_distance
            .unwrap_or(self.hamming_distance as f32)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_quantization_positive() {
        let values = vec![1.0, -1.0, 0.5, -0.5, 0.0, 2.0, -2.0, 0.1];
        let binary = BinaryVector::from_f32(&values);

        // positive: indices 0, 2, 5, 7
        // Expected bits: 0b10100101 = 165
        assert_eq!(binary.data[0] & 0xFF, 0b10100101);
    }

    #[test]
    fn test_hamming_distance_identical() {
        let v1 = BinaryVector::from_f32(&[1.0, -1.0, 1.0, -1.0]);
        let v2 = BinaryVector::from_f32(&[1.0, -1.0, 1.0, -1.0]);
        assert_eq!(v1.hamming_distance(&v2), 0);
    }

    #[test]
    fn test_hamming_distance_opposite() {
        let v1 = BinaryVector::from_f32(&[1.0, 1.0, 1.0, 1.0]);
        let v2 = BinaryVector::from_f32(&[-1.0, -1.0, -1.0, -1.0]);
        assert_eq!(v1.hamming_distance(&v2), 4);
    }

    #[test]
    fn test_hamming_distance_partial() {
        let v1 = BinaryVector::from_f32(&[1.0, 1.0, -1.0, -1.0]);
        let v2 = BinaryVector::from_f32(&[1.0, -1.0, 1.0, -1.0]);
        assert_eq!(v1.hamming_distance(&v2), 2);
    }

    #[test]
    fn test_large_vector() {
        // Test 1024-dim vector (common embedding size)
        let v1: Vec<f32> = (0..1024)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let v2: Vec<f32> = (0..1024)
            .map(|i| if i % 3 == 0 { 1.0 } else { -1.0 })
            .collect();

        let b1 = BinaryVector::from_f32(&v1);
        let b2 = BinaryVector::from_f32(&v2);

        // Verify size: 1024 bits = 128 bytes = 16 u64s
        assert_eq!(b1.size_bytes(), 128);
        assert_eq!(b1.data.len(), 16);

        let dist = b1.hamming_distance(&b2);
        assert!(dist > 0 && dist < 1024);
    }

    #[test]
    fn test_binary_index_search() {
        let mut index = BinaryIndex::new(64);

        // Add some vectors
        let v1 = vec![1.0f32; 64];
        let v2 = vec![-1.0f32; 64];
        let v3: Vec<f32> = (0..64).map(|i| if i < 32 { 1.0 } else { -1.0 }).collect();

        index.add_f32(&v1);
        index.add_f32(&v2);
        index.add_f32(&v3);

        // Search for v1-like vector
        let query: Vec<f32> = (0..64).map(|i| if i < 60 { 1.0 } else { -1.0 }).collect();
        let results = index.search_f32(&query, 3);

        // v1 should be closest (only 4 bits different)
        assert_eq!(results[0].0, 0);
        assert_eq!(results[0].1, 4);
    }

    #[test]
    fn test_approx_cosine() {
        let v1 = BinaryVector::from_f32(&[1.0; 128]);
        let v2 = BinaryVector::from_f32(&[1.0; 128]);
        let sim = v1.approx_cosine_similarity(&v2);
        assert!((sim - 1.0).abs() < 0.001); // Identical = 1.0

        let v3 = BinaryVector::from_f32(&[-1.0; 128]);
        let sim2 = v1.approx_cosine_similarity(&v3);
        assert!((sim2 - (-1.0)).abs() < 0.001); // Opposite = -1.0
    }

    #[test]
    fn test_compression_ratio() {
        // fp32: 1024 * 4 = 4096 bytes
        // binary: 1024 / 8 = 128 bytes
        // ratio: 32x

        let fp32_size = 1024 * 4;
        let binary = BinaryVector::from_f32(&vec![1.0; 1024]);
        let binary_size = binary.size_bytes();

        assert_eq!(binary_size, 128);
        assert_eq!(fp32_size / binary_size, 32);
    }
}
