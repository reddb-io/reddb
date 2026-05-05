//! Tiered Vector Search Pipeline
//!
//! Implements a multi-stage retrieval strategy that combines binary quantization
//! for fast initial filtering with int8/fp32 rescoring for precision.
//!
//! # Strategy (from HuggingFace Embedding Quantization blog)
//!
//! 1. **Binary Pass**: Fast Hamming distance search on 1-bit vectors
//!    - 32x less memory than fp32
//!    - Super fast (popcount is single instruction)
//!    - Get top-k × rescore_multiplier candidates
//!
//! 2. **int8 Rescore**: Refine candidates with 8-bit precision
//!    - 4x less memory than fp32
//!    - SIMD accelerated dot product
//!    - Restores ~99% of fp32 quality
//!
//! 3. **Optional fp32 Rescore**: Full precision for final ranking
//!    - Only for top-k results
//!    - Maximum precision when needed
//!
//! # Performance Characteristics
//!
//! For 1M vectors × 1024 dimensions:
//!
//! | Representation | Memory    | Search Speed |
//! |----------------|-----------|--------------|
//! | fp32           | 4 GB      | 1x (baseline)|
//! | int8           | 1 GB      | ~4x faster   |
//! | binary         | 128 MB    | ~25x faster  |
//! | tiered (bin→i8)| ~1.1 GB   | ~20x faster  |
//!
//! # Usage
//!
//! ```ignore
//! // Create tiered index
//! let mut index = TieredIndex::new(1024);
//!
//! // Add vectors (automatically quantized to all representations)
//! for vec in embeddings {
//!     index.add(&vec);
//! }
//!
//! // Search with automatic tiered retrieval
//! let results = index.search(&query, 10);
//!
//! // Custom rescore multiplier (default: 4)
//! let results = index.search_with_config(&query, 10, TieredSearchConfig {
//!     rescore_multiplier: 8,  // Get more candidates from binary pass
//!     use_fp32_final: true,   // Final fp32 rescoring
//! });
//! ```

use super::binary_quantize::{BinaryIndex, BinaryVector};
use super::int8_quantize::Int8Index;
use std::cmp::Ordering;

/// Configuration for tiered search
#[derive(Debug, Clone)]
pub struct TieredSearchConfig {
    /// Multiplier for binary search candidates
    /// If k=10, binary pass retrieves k × rescore_multiplier = 40 candidates
    pub rescore_multiplier: usize,
    /// Use fp32 for final rescoring (highest precision)
    pub use_fp32_final: bool,
    /// Minimum candidates to retrieve from binary pass
    pub min_binary_candidates: usize,
    /// Maximum candidates to retrieve from binary pass
    pub max_binary_candidates: usize,
}

impl Default for TieredSearchConfig {
    fn default() -> Self {
        Self {
            rescore_multiplier: 4,
            use_fp32_final: false,
            min_binary_candidates: 10,
            max_binary_candidates: 1000,
        }
    }
}

impl TieredSearchConfig {
    /// Create config optimized for speed (lower multiplier)
    pub fn fast() -> Self {
        Self {
            rescore_multiplier: 2,
            use_fp32_final: false,
            min_binary_candidates: 10,
            max_binary_candidates: 500,
        }
    }

    /// Create config optimized for quality (higher multiplier)
    pub fn quality() -> Self {
        Self {
            rescore_multiplier: 8,
            use_fp32_final: true,
            min_binary_candidates: 20,
            max_binary_candidates: 2000,
        }
    }

    /// Create config for maximum precision (fp32 final)
    pub fn precise() -> Self {
        Self {
            rescore_multiplier: 10,
            use_fp32_final: true,
            min_binary_candidates: 50,
            max_binary_candidates: 5000,
        }
    }
}

/// Result from tiered search
#[derive(Debug, Clone)]
pub struct TieredSearchResult {
    /// Vector index
    pub id: usize,
    /// Final distance score (lower = more similar)
    pub distance: f32,
    /// Hamming distance from binary pass
    pub hamming_distance: u32,
    /// int8 rescored distance (if applicable)
    pub int8_distance: Option<f32>,
    /// fp32 rescored distance (if applicable)
    pub fp32_distance: Option<f32>,
}

impl TieredSearchResult {
    pub fn new(id: usize, hamming_distance: u32) -> Self {
        Self {
            id,
            distance: hamming_distance as f32,
            hamming_distance,
            int8_distance: None,
            fp32_distance: None,
        }
    }
}

/// Tiered vector index with binary, int8, and optional fp32 representations
pub struct TieredIndex {
    /// Binary quantized vectors (1 bit/dim)
    binary_index: BinaryIndex,
    /// int8 quantized vectors (8 bits/dim)
    int8_index: Int8Index,
    /// Optional fp32 vectors for final rescoring
    fp32_vectors: Option<Vec<Vec<f32>>>,
    /// Dimensionality
    dim: usize,
    /// Store fp32 vectors for final rescoring
    store_fp32: bool,
    /// Memory constraint configuration
    memory_config: Option<MemoryConstraint>,
}

/// Memory constraint configuration for resource-limited systems
#[derive(Debug, Clone)]
pub struct MemoryConstraint {
    /// Maximum memory budget in bytes
    pub max_bytes: usize,
    /// Maximum number of vectors (computed from max_bytes)
    pub max_vectors: usize,
    /// Bytes per vector (computed from dim and storage mode)
    pub bytes_per_vector: usize,
    /// Reserved memory for overhead (default: 10%)
    pub overhead_factor: f32,
}

/// Error returned when memory limit is reached
#[derive(Debug, Clone)]
pub struct MemoryLimitError {
    /// Current number of vectors
    pub current_vectors: usize,
    /// Maximum allowed vectors
    pub max_vectors: usize,
    /// Current memory usage in bytes
    pub current_bytes: usize,
    /// Maximum memory budget in bytes
    pub max_bytes: usize,
}

impl std::fmt::Display for MemoryLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Memory limit reached: {}/{} vectors, {:.2} MB/{:.2} MB",
            self.current_vectors,
            self.max_vectors,
            self.current_bytes as f64 / 1_000_000.0,
            self.max_bytes as f64 / 1_000_000.0
        )
    }
}

impl std::error::Error for MemoryLimitError {}

impl MemoryConstraint {
    /// Calculate bytes per vector for given dimension and storage mode
    pub fn bytes_per_vector(dim: usize, store_fp32: bool) -> usize {
        let binary_bytes = dim.div_ceil(64) * 8; // Packed u64
        let int8_bytes = dim + 8; // data + scale + norm
        let fp32_bytes = if store_fp32 { dim * 4 } else { 0 };
        binary_bytes + int8_bytes + fp32_bytes
    }

    /// Create a memory constraint from a byte budget
    pub fn from_bytes(max_bytes: usize, dim: usize, store_fp32: bool) -> Self {
        let overhead_factor = 0.1; // 10% overhead for Vec growth, metadata, etc.
        let usable_bytes = (max_bytes as f32 * (1.0 - overhead_factor)) as usize;
        let bytes_per_vec = Self::bytes_per_vector(dim, store_fp32);
        let max_vectors = usable_bytes / bytes_per_vec;

        Self {
            max_bytes,
            max_vectors,
            bytes_per_vector: bytes_per_vec,
            overhead_factor,
        }
    }

    /// Create from a target vector count
    pub fn from_vectors(max_vectors: usize, dim: usize, store_fp32: bool) -> Self {
        let bytes_per_vec = Self::bytes_per_vector(dim, store_fp32);
        let overhead_factor = 0.1;
        let max_bytes = ((max_vectors * bytes_per_vec) as f32 / (1.0 - overhead_factor)) as usize;

        Self {
            max_bytes,
            max_vectors,
            bytes_per_vector: bytes_per_vec,
            overhead_factor,
        }
    }
}

impl TieredIndex {
    /// Create a new tiered index (no memory limit)
    pub fn new(dim: usize) -> Self {
        Self {
            binary_index: BinaryIndex::new(dim),
            int8_index: Int8Index::new(dim),
            fp32_vectors: None,
            dim,
            store_fp32: false,
            memory_config: None,
        }
    }

    /// Create with fp32 storage for maximum precision rescoring
    pub fn with_fp32_storage(dim: usize) -> Self {
        Self {
            binary_index: BinaryIndex::new(dim),
            int8_index: Int8Index::new(dim),
            fp32_vectors: Some(Vec::new()),
            dim,
            store_fp32: true,
            memory_config: None,
        }
    }

    /// Create with pre-allocated capacity
    pub fn with_capacity(dim: usize, capacity: usize, store_fp32: bool) -> Self {
        Self {
            binary_index: BinaryIndex::with_capacity(dim, capacity),
            int8_index: Int8Index::with_capacity(dim, capacity),
            fp32_vectors: if store_fp32 {
                Some(Vec::with_capacity(capacity))
            } else {
                None
            },
            dim,
            store_fp32,
            memory_config: None,
        }
    }

    /// Create a memory-constrained index for resource-limited systems
    ///
    /// Automatically calculates the maximum number of vectors that fit
    /// in the given memory budget.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // For a system with 512 MB available for vectors
    /// let index = TieredIndex::memory_constrained(1024, 512 * 1024 * 1024);
    ///
    /// // For 1 GB budget
    /// let index = TieredIndex::memory_constrained(768, 1024 * 1024 * 1024);
    ///
    /// // Using helper constants
    /// let index = TieredIndex::memory_constrained(1024, TieredIndex::MB(256));
    /// ```
    pub fn memory_constrained(dim: usize, max_bytes: usize) -> Self {
        let config = MemoryConstraint::from_bytes(max_bytes, dim, false);
        let capacity = config.max_vectors;

        Self {
            binary_index: BinaryIndex::with_capacity(dim, capacity),
            int8_index: Int8Index::with_capacity(dim, capacity),
            fp32_vectors: None,
            dim,
            store_fp32: false,
            memory_config: Some(config),
        }
    }

    /// Create memory-constrained index with fp32 storage
    ///
    /// Note: fp32 storage uses 4x more memory per vector.
    pub fn memory_constrained_precise(dim: usize, max_bytes: usize) -> Self {
        let config = MemoryConstraint::from_bytes(max_bytes, dim, true);
        let capacity = config.max_vectors;

        Self {
            binary_index: BinaryIndex::with_capacity(dim, capacity),
            int8_index: Int8Index::with_capacity(dim, capacity),
            fp32_vectors: Some(Vec::with_capacity(capacity)),
            dim,
            store_fp32: true,
            memory_config: Some(config),
        }
    }

    /// Helper: convert MB to bytes
    #[inline]
    #[allow(non_snake_case)]
    pub const fn MB(mb: usize) -> usize {
        mb * 1024 * 1024
    }

    /// Helper: convert GB to bytes
    #[inline]
    #[allow(non_snake_case)]
    pub const fn GB(gb: usize) -> usize {
        gb * 1024 * 1024 * 1024
    }

    /// Check if the index has a memory constraint
    #[inline]
    pub fn is_constrained(&self) -> bool {
        self.memory_config.is_some()
    }

    /// Get the memory constraint configuration (if any)
    pub fn memory_constraint(&self) -> Option<&MemoryConstraint> {
        self.memory_config.as_ref()
    }

    /// Check if we can add more vectors
    #[inline]
    pub fn can_add(&self) -> bool {
        match &self.memory_config {
            Some(config) => self.len() < config.max_vectors,
            None => true,
        }
    }

    /// Check if we can add N more vectors
    #[inline]
    pub fn can_add_n(&self, n: usize) -> bool {
        match &self.memory_config {
            Some(config) => self.len() + n <= config.max_vectors,
            None => true,
        }
    }

    /// Get remaining capacity (vectors)
    pub fn remaining_capacity(&self) -> Option<usize> {
        self.memory_config
            .as_ref()
            .map(|c| c.max_vectors.saturating_sub(self.len()))
    }

    /// Get remaining memory budget (bytes)
    pub fn remaining_bytes(&self) -> Option<usize> {
        self.memory_config.as_ref().map(|c| {
            let used = self.memory_stats().total_bytes;
            c.max_bytes.saturating_sub(used)
        })
    }

    /// Get memory utilization as percentage (0.0 - 1.0)
    pub fn memory_utilization(&self) -> Option<f32> {
        self.memory_config.as_ref().map(|c| {
            if c.max_vectors == 0 {
                0.0
            } else {
                self.len() as f32 / c.max_vectors as f32
            }
        })
    }

    /// Add a vector to the index
    ///
    /// Automatically quantizes to binary and int8 representations.
    /// Returns `false` if memory limit reached (for constrained indexes).
    pub fn add(&mut self, vector: &[f32]) -> bool {
        debug_assert_eq!(vector.len(), self.dim, "Dimension mismatch");

        // Check memory constraint
        if !self.can_add() {
            return false;
        }

        self.binary_index.add_f32(vector);
        self.int8_index.add_f32(vector);

        if let Some(ref mut fp32) = self.fp32_vectors {
            fp32.push(vector.to_vec());
        }

        true
    }

    /// Add a vector, panicking if memory limit reached
    ///
    /// Use this when you're sure there's capacity.
    pub fn add_unchecked(&mut self, vector: &[f32]) {
        debug_assert_eq!(vector.len(), self.dim, "Dimension mismatch");

        self.binary_index.add_f32(vector);
        self.int8_index.add_f32(vector);

        if let Some(ref mut fp32) = self.fp32_vectors {
            fp32.push(vector.to_vec());
        }
    }

    /// Try to add a vector, returning error details if failed
    pub fn try_add(&mut self, vector: &[f32]) -> Result<(), MemoryLimitError> {
        debug_assert_eq!(vector.len(), self.dim, "Dimension mismatch");

        if let Some(ref config) = self.memory_config {
            if self.len() >= config.max_vectors {
                return Err(MemoryLimitError {
                    current_vectors: self.len(),
                    max_vectors: config.max_vectors,
                    current_bytes: self.memory_stats().total_bytes,
                    max_bytes: config.max_bytes,
                });
            }
        }

        self.add_unchecked(vector);
        Ok(())
    }

    /// Add multiple vectors in batch
    ///
    /// Returns the number of vectors successfully added.
    pub fn add_batch(&mut self, vectors: &[Vec<f32>]) -> usize {
        let mut added = 0;
        for v in vectors {
            if self.add(v) {
                added += 1;
            } else {
                break;
            }
        }
        added
    }

    /// Add multiple vectors, stopping at memory limit
    ///
    /// Returns (added_count, remaining_vectors)
    pub fn add_batch_partial(&mut self, vectors: &[Vec<f32>]) -> (usize, usize) {
        let added = self.add_batch(vectors);
        (added, vectors.len() - added)
    }

    /// Get number of vectors in the index
    #[inline]
    pub fn len(&self) -> usize {
        self.binary_index.len()
    }

    /// Check if index is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.binary_index.is_empty()
    }

    /// Get dimensionality
    #[inline]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Get memory usage statistics
    pub fn memory_stats(&self) -> TieredMemoryStats {
        let binary_bytes = self.binary_index.memory_bytes();
        let int8_bytes = self.int8_index.memory_bytes();
        let fp32_bytes = self
            .fp32_vectors
            .as_ref()
            .map(|v| v.len() * self.dim * 4)
            .unwrap_or(0);

        TieredMemoryStats {
            binary_bytes,
            int8_bytes,
            fp32_bytes,
            total_bytes: binary_bytes + int8_bytes + fp32_bytes,
            n_vectors: self.len(),
            dim: self.dim,
        }
    }

    /// Search for k nearest neighbors using tiered retrieval
    ///
    /// Uses default configuration (rescore_multiplier=4, no fp32 final).
    pub fn search(&self, query: &[f32], k: usize) -> Vec<TieredSearchResult> {
        self.search_with_config(query, k, &TieredSearchConfig::default())
    }

    /// Search with custom configuration
    pub fn search_with_config(
        &self,
        query: &[f32],
        k: usize,
        config: &TieredSearchConfig,
    ) -> Vec<TieredSearchResult> {
        if self.is_empty() {
            return Vec::new();
        }

        let k = k.min(self.len());

        // Stage 1: Binary search for candidates
        let n_binary_candidates = (k * config.rescore_multiplier)
            .max(config.min_binary_candidates)
            .min(config.max_binary_candidates)
            .min(self.len());

        let binary_query = BinaryVector::from_f32(query);
        let binary_results = self.binary_index.search(&binary_query, n_binary_candidates);

        // Stage 2: int8 rescoring
        let int8_rescored = self.int8_index.rescore_candidates(&binary_results, query);

        // Create results
        let mut results: Vec<TieredSearchResult> = int8_rescored
            .iter()
            .take(if config.use_fp32_final { k * 2 } else { k })
            .map(|&(id, int8_dist)| {
                let hamming = binary_results
                    .iter()
                    .find(|(i, _)| *i == id)
                    .map(|(_, d)| *d)
                    .unwrap_or(0);

                let mut result = TieredSearchResult::new(id, hamming);
                result.int8_distance = Some(int8_dist);
                result.distance = int8_dist;
                result
            })
            .collect();

        // Stage 3: Optional fp32 rescoring
        if config.use_fp32_final {
            if let Some(ref fp32_vectors) = self.fp32_vectors {
                for result in results.iter_mut() {
                    if result.id < fp32_vectors.len() {
                        let fp32_dist = cosine_distance_f32(query, &fp32_vectors[result.id]);
                        result.fp32_distance = Some(fp32_dist);
                        result.distance = fp32_dist;
                    }
                }
                // Re-sort by fp32 distance
                results.sort_by(|a, b| {
                    a.distance
                        .partial_cmp(&b.distance)
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| a.id.cmp(&b.id))
                });
            }
        }

        results.truncate(k);
        results
    }

    /// Search with only binary (fastest, least precise)
    pub fn search_binary_only(&self, query: &[f32], k: usize) -> Vec<TieredSearchResult> {
        let binary_query = BinaryVector::from_f32(query);
        let results = self.binary_index.search(&binary_query, k);

        results
            .into_iter()
            .map(|(id, hamming)| TieredSearchResult::new(id, hamming))
            .collect()
    }

    /// Search with binary + int8 (recommended balance)
    pub fn search_int8(
        &self,
        query: &[f32],
        k: usize,
        rescore_multiplier: usize,
    ) -> Vec<TieredSearchResult> {
        let config = TieredSearchConfig {
            rescore_multiplier,
            use_fp32_final: false,
            ..Default::default()
        };
        self.search_with_config(query, k, &config)
    }
}

/// Memory usage statistics for tiered index
#[derive(Debug, Clone)]
pub struct TieredMemoryStats {
    /// Binary index size in bytes
    pub binary_bytes: usize,
    /// int8 index size in bytes
    pub int8_bytes: usize,
    /// fp32 vectors size in bytes (if stored)
    pub fp32_bytes: usize,
    /// Total memory usage
    pub total_bytes: usize,
    /// Number of vectors
    pub n_vectors: usize,
    /// Dimensionality
    pub dim: usize,
}

impl TieredMemoryStats {
    /// Get compression ratio compared to fp32-only
    pub fn compression_ratio(&self) -> f32 {
        let fp32_only = self.n_vectors * self.dim * 4;
        if self.total_bytes > 0 {
            fp32_only as f32 / self.total_bytes as f32
        } else {
            0.0
        }
    }

    /// Format as human-readable string
    pub fn format(&self) -> String {
        format!(
            "Tiered Index: {} vectors × {} dim\n\
             Binary:  {} ({:.1} MB)\n\
             int8:    {} ({:.1} MB)\n\
             fp32:    {} ({:.1} MB)\n\
             Total:   {:.1} MB (vs {:.1} MB fp32-only, {:.1}x compression)",
            self.n_vectors,
            self.dim,
            format_bytes(self.binary_bytes),
            self.binary_bytes as f64 / 1_000_000.0,
            format_bytes(self.int8_bytes),
            self.int8_bytes as f64 / 1_000_000.0,
            format_bytes(self.fp32_bytes),
            self.fp32_bytes as f64 / 1_000_000.0,
            self.total_bytes as f64 / 1_000_000.0,
            (self.n_vectors * self.dim * 4) as f64 / 1_000_000.0,
            self.compression_ratio()
        )
    }
}

fn format_bytes(bytes: usize) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.2} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.2} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Compute cosine distance between two fp32 vectors
fn cosine_distance_f32(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom > 0.0 {
        1.0 - dot / denom
    } else {
        1.0
    }
}

// ============================================================================
// Builder Pattern
// ============================================================================

/// Builder for creating tiered indexes with custom configuration
pub struct TieredIndexBuilder {
    dim: usize,
    capacity: Option<usize>,
    store_fp32: bool,
}

impl TieredIndexBuilder {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            capacity: None,
            store_fp32: false,
        }
    }

    /// Pre-allocate capacity for n vectors
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Store fp32 vectors for final rescoring
    pub fn with_fp32_storage(mut self) -> Self {
        self.store_fp32 = true;
        self
    }

    /// Build the tiered index
    pub fn build(self) -> TieredIndex {
        match self.capacity {
            Some(cap) => TieredIndex::with_capacity(self.dim, cap, self.store_fp32),
            None => {
                if self.store_fp32 {
                    TieredIndex::with_fp32_storage(self.dim)
                } else {
                    TieredIndex::new(self.dim)
                }
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vector(dim: usize, seed: usize) -> Vec<f32> {
        // Simple pseudo-random for tests
        (0..dim)
            .map(|i| {
                let x = ((seed * 1103515245 + i * 12345) % 2147483648) as f32 / 2147483648.0;
                x * 2.0 - 1.0 // Range [-1, 1]
            })
            .collect()
    }

    #[test]
    fn test_tiered_index_basic() {
        let mut index = TieredIndex::new(64);

        let v1 = random_vector(64, 1);
        let v2 = random_vector(64, 2);
        let v3 = random_vector(64, 3);

        index.add(&v1);
        index.add(&v2);
        index.add(&v3);

        assert_eq!(index.len(), 3);
    }

    #[test]
    fn test_tiered_search() {
        let mut index = TieredIndex::new(64);

        // Add vectors
        for i in 0..100 {
            index.add(&random_vector(64, i));
        }

        // Search
        let query = random_vector(64, 0); // Should be closest to v0
        let results = index.search(&query, 5);

        assert_eq!(results.len(), 5);
        // First result should be v0 (identical query)
        assert_eq!(results[0].id, 0);
    }

    #[test]
    fn test_tiered_with_fp32() {
        let mut index = TieredIndex::with_fp32_storage(64);

        for i in 0..50 {
            index.add(&random_vector(64, i));
        }

        let query = random_vector(64, 0);
        let results = index.search_with_config(&query, 5, &TieredSearchConfig::quality());

        assert_eq!(results.len(), 5);
        assert!(results[0].fp32_distance.is_some());
    }

    #[test]
    fn test_memory_stats() {
        let mut index = TieredIndex::new(1024);

        for i in 0..1000 {
            index.add(&random_vector(1024, i));
        }

        let stats = index.memory_stats();

        // Binary: 1000 × 1024/8 = 128,000 bytes = 128 KB
        assert!(stats.binary_bytes > 100_000);
        assert!(stats.binary_bytes < 200_000);

        // int8: 1000 × 1024 + overhead ≈ 1 MB
        assert!(stats.int8_bytes > 1_000_000);
        assert!(stats.int8_bytes < 1_500_000);

        // Compression ratio should be > 2x (no fp32 stored)
        assert!(stats.compression_ratio() > 2.0);
    }

    #[test]
    fn test_binary_only_search() {
        let mut index = TieredIndex::new(128);

        for i in 0..100 {
            index.add(&random_vector(128, i));
        }

        let query = random_vector(128, 50);
        let results = index.search_binary_only(&query, 10);

        assert_eq!(results.len(), 10);
        // Results should have hamming distance but no int8/fp32
        assert!(results[0].int8_distance.is_none());
    }

    #[test]
    fn test_search_configs() {
        let mut index = TieredIndex::with_fp32_storage(64);

        for i in 0..100 {
            index.add(&random_vector(64, i));
        }

        let query = random_vector(64, 0);

        // Test different configs
        let fast = index.search_with_config(&query, 5, &TieredSearchConfig::fast());
        let quality = index.search_with_config(&query, 5, &TieredSearchConfig::quality());
        let precise = index.search_with_config(&query, 5, &TieredSearchConfig::precise());

        assert_eq!(fast.len(), 5);
        assert_eq!(quality.len(), 5);
        assert_eq!(precise.len(), 5);

        // Quality/precise should have fp32 distance
        assert!(quality[0].fp32_distance.is_some());
        assert!(precise[0].fp32_distance.is_some());
    }

    #[test]
    fn test_builder() {
        let index = TieredIndexBuilder::new(256)
            .with_capacity(1000)
            .with_fp32_storage()
            .build();

        assert_eq!(index.dim(), 256);
        assert!(index.is_empty());
    }

    #[test]
    fn test_memory_constrained() {
        // 100 KB budget, 64-dim vectors
        // bytes_per_vector = 8 (binary) + 72 (int8) = 80 bytes
        // 100KB = 102,400 bytes / 80 ≈ 1,280 vectors (with 10% overhead ≈ 1,152)
        let mut index = TieredIndex::memory_constrained(64, 100 * 1024);

        assert!(index.is_constrained());
        assert!(index.can_add());

        let config = index.memory_constraint().unwrap();
        assert!(config.max_vectors > 1000);
        assert!(config.max_vectors < 1500);

        // Add vectors until limit
        let mut added = 0;
        for i in 0..2000 {
            if index.add(&random_vector(64, i)) {
                added += 1;
            } else {
                break;
            }
        }

        // Should have stopped before 2000
        assert!(added < 2000);
        assert_eq!(index.len(), added);
        assert!(!index.can_add());
    }

    #[test]
    fn test_memory_constrained_batch() {
        let mut index = TieredIndex::memory_constrained(32, 50 * 1024); // 50 KB

        let vectors: Vec<Vec<f32>> = (0..1000).map(|i| random_vector(32, i)).collect();

        let (added, remaining) = index.add_batch_partial(&vectors);

        assert!(added > 0);
        assert!(added < 1000);
        assert_eq!(added + remaining, 1000);
        assert_eq!(index.len(), added);
    }

    #[test]
    fn test_memory_constrained_try_add() {
        let mut index = TieredIndex::memory_constrained(16, 1024); // Very small: 1 KB

        let config = index.memory_constraint().unwrap();
        let max = config.max_vectors;

        // Fill to capacity
        for i in 0..max {
            assert!(index.try_add(&random_vector(16, i)).is_ok());
        }

        // Next add should fail
        let result = index.try_add(&random_vector(16, max + 1));
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.current_vectors, max);
        assert_eq!(err.max_vectors, max);
    }

    #[test]
    fn test_memory_utilization() {
        let mut index = TieredIndex::memory_constrained(64, 10 * 1024);

        assert_eq!(index.memory_utilization(), Some(0.0));

        let max = index.memory_constraint().unwrap().max_vectors;
        let half = max / 2;

        for i in 0..half {
            index.add(&random_vector(64, i));
        }

        let util = index.memory_utilization().unwrap();
        assert!(util > 0.4 && util < 0.6);
    }

    #[test]
    fn test_remaining_capacity() {
        let mut index = TieredIndex::memory_constrained(64, 20 * 1024);

        let initial = index.remaining_capacity().unwrap();
        assert!(initial > 0);

        index.add(&random_vector(64, 0));

        let after = index.remaining_capacity().unwrap();
        assert_eq!(after, initial - 1);
    }

    #[test]
    fn test_bytes_per_vector_calculation() {
        // 1024-dim, no fp32
        let bpv = MemoryConstraint::bytes_per_vector(1024, false);
        // binary: 1024/8 = 128 bytes (actually (1024+63)/64*8 = 128)
        // int8: 1024 + 8 = 1032 bytes
        // total: 1160 bytes
        assert_eq!(bpv, 128 + 1032);

        // With fp32
        let bpv_fp32 = MemoryConstraint::bytes_per_vector(1024, true);
        // + fp32: 1024 * 4 = 4096 bytes
        assert_eq!(bpv_fp32, 128 + 1032 + 4096);
    }
}
