//! int8 Quantization for Vector Embeddings
//!
//! Compresses fp32 vectors to int8 (8 bits per dimension) for efficient
//! storage and fast approximate distance computation.
//!
//! # Compression Ratio
//!
//! - fp32: 4 bytes per dimension
//! - int8: 1 byte per dimension = 4x compression
//!
//! Example: 1024-dim vector
//! - fp32: 4096 bytes
//! - int8: 1024 bytes
//!
//! # Quantization Methods
//!
//! ## Symmetric Quantization
//! Maps [-max_abs, +max_abs] → [-127, +127]
//! - scale = max(|v|) / 127
//! - quantized = round(v / scale)
//!
//! ## Asymmetric Quantization
//! Maps [min, max] → [0, 255]
//! - scale = (max - min) / 255
//! - zero_point = round(-min / scale)
//! - quantized = round(v / scale) + zero_point
//!
//! # Usage
//!
//! ```ignore
//! // Quantize a vector (symmetric)
//! let int8 = Int8Vector::from_f32(&embedding);
//!
//! // Compute dot product (SIMD accelerated)
//! let dot = int8.dot_product(&other);
//!
//! // Rescore binary search candidates
//! let rescored = int8.rescore_candidates(&binary_results, &query);
//! ```

use std::cmp::Ordering;

/// int8 quantized vector with scale factor
#[derive(Clone, Debug)]
pub struct Int8Vector {
    /// Quantized values (-127 to +127 for symmetric)
    data: Vec<i8>,
    /// Scale factor for dequantization
    scale: f32,
    /// Original L2 norm (for normalized dot product)
    norm: f32,
}

impl Int8Vector {
    /// Create int8 vector from fp32 using symmetric quantization
    ///
    /// Best for normalized embeddings centered around 0.
    pub fn from_f32(values: &[f32]) -> Self {
        if values.is_empty() {
            return Self {
                data: Vec::new(),
                scale: 1.0,
                norm: 0.0,
            };
        }

        // Find maximum absolute value
        let max_abs = values
            .iter()
            .map(|v| v.abs())
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
            .unwrap_or(1.0);

        // Compute scale (avoid division by zero)
        let scale = if max_abs > 0.0 { max_abs / 127.0 } else { 1.0 };

        // Quantize
        let data: Vec<i8> = values
            .iter()
            .map(|&v| {
                let quantized = (v / scale).round();
                quantized.clamp(-127.0, 127.0) as i8
            })
            .collect();

        // Compute original norm
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();

        Self { data, scale, norm }
    }

    /// Create int8 vector with pre-computed scale
    pub fn from_f32_with_scale(values: &[f32], scale: f32) -> Self {
        let data: Vec<i8> = values
            .iter()
            .map(|&v| {
                let quantized = (v / scale).round();
                quantized.clamp(-127.0, 127.0) as i8
            })
            .collect();

        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();

        Self { data, scale, norm }
    }

    /// Create from raw quantized data
    pub fn from_raw(data: Vec<i8>, scale: f32, norm: f32) -> Self {
        Self { data, scale, norm }
    }

    /// Get the dimensionality
    #[inline]
    pub fn dim(&self) -> usize {
        self.data.len()
    }

    /// Get the quantized data
    #[inline]
    pub fn data(&self) -> &[i8] {
        &self.data
    }

    /// Get the scale factor
    #[inline]
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Get size in bytes
    #[inline]
    pub fn size_bytes(&self) -> usize {
        self.data.len() + 8 // data + scale (f32) + norm (f32)
    }

    /// Dequantize to fp32
    pub fn to_f32(&self) -> Vec<f32> {
        self.data.iter().map(|&v| v as f32 * self.scale).collect()
    }

    /// Compute dot product with another int8 vector
    ///
    /// Returns scaled dot product in fp32.
    #[inline]
    pub fn dot_product(&self, other: &Self) -> f32 {
        debug_assert_eq!(self.data.len(), other.data.len(), "Dimensions must match");

        let raw_dot = dot_product_i8_simd(&self.data, &other.data);
        raw_dot as f32 * self.scale * other.scale
    }

    /// Compute dot product with fp32 query (asymmetric)
    ///
    /// Query stays in fp32 for better precision.
    /// This is the recommended approach for rescoring.
    #[inline]
    pub fn dot_product_f32(&self, query: &[f32]) -> f32 {
        debug_assert_eq!(self.data.len(), query.len(), "Dimensions must match");

        dot_product_i8_f32_simd(&self.data, query) * self.scale
    }

    /// Compute L2 squared distance to another int8 vector
    #[inline]
    pub fn l2_squared(&self, other: &Self) -> f32 {
        debug_assert_eq!(self.data.len(), other.data.len(), "Dimensions must match");

        let raw_dist = l2_squared_i8_simd(&self.data, &other.data);
        raw_dist as f32 * self.scale * other.scale
    }

    /// Compute cosine distance using normalized dot product
    ///
    /// Assumes vectors were normalized before quantization.
    #[inline]
    pub fn cosine_distance(&self, other: &Self) -> f32 {
        let dot = self.dot_product(other);
        let denom = self.norm * other.norm;
        if denom > 0.0 {
            1.0 - (dot / denom)
        } else {
            1.0
        }
    }
}

// ============================================================================
// SIMD Operations
// ============================================================================

/// Compute dot product of two i8 vectors using SIMD
#[inline]
pub fn dot_product_i8_simd(a: &[i8], b: &[i8]) -> i32 {
    debug_assert_eq!(a.len(), b.len(), "Vectors must have same length");

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { dot_product_i8_avx2(a, b) };
        }
        if is_x86_feature_detected!("sse4.1") {
            return unsafe { dot_product_i8_sse4(a, b) };
        }
    }

    dot_product_i8_scalar(a, b)
}

/// Scalar fallback for i8 dot product
#[inline]
fn dot_product_i8_scalar(a: &[i8], b: &[i8]) -> i32 {
    let mut sum = 0i32;
    for (x, y) in a.iter().zip(b.iter()) {
        sum += (*x as i32) * (*y as i32);
    }
    sum
}

/// AVX2 implementation of i8 dot product
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn dot_product_i8_avx2(a: &[i8], b: &[i8]) -> i32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm256_setzero_si256();

    // Process 32 elements at a time
    let chunks = len / 32;
    for i in 0..chunks {
        let idx = i * 32;
        let va = _mm256_loadu_si256(a.as_ptr().add(idx) as *const __m256i);
        let vb = _mm256_loadu_si256(b.as_ptr().add(idx) as *const __m256i);

        // Split into low and high 128-bit lanes and convert to i16
        let va_lo = _mm256_cvtepi8_epi16(_mm256_castsi256_si128(va));
        let va_hi = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(va, 1));
        let vb_lo = _mm256_cvtepi8_epi16(_mm256_castsi256_si128(vb));
        let vb_hi = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(vb, 1));

        // Multiply and accumulate
        let prod_lo = _mm256_madd_epi16(va_lo, vb_lo);
        let prod_hi = _mm256_madd_epi16(va_hi, vb_hi);

        sum = _mm256_add_epi32(sum, prod_lo);
        sum = _mm256_add_epi32(sum, prod_hi);
    }

    // Horizontal sum
    let sum128 = _mm_add_epi32(
        _mm256_castsi256_si128(sum),
        _mm256_extracti128_si256(sum, 1),
    );
    let sum64 = _mm_add_epi32(sum128, _mm_srli_si128(sum128, 8));
    let sum32 = _mm_add_epi32(sum64, _mm_srli_si128(sum64, 4));
    let mut result = _mm_cvtsi128_si32(sum32);

    // Handle remaining elements
    for i in (chunks * 32)..len {
        result += (a[i] as i32) * (b[i] as i32);
    }

    result
}

/// SSE4.1 implementation of i8 dot product
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
unsafe fn dot_product_i8_sse4(a: &[i8], b: &[i8]) -> i32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm_setzero_si128();

    // Process 16 elements at a time
    let chunks = len / 16;
    for i in 0..chunks {
        let idx = i * 16;
        let va = _mm_loadu_si128(a.as_ptr().add(idx) as *const __m128i);
        let vb = _mm_loadu_si128(b.as_ptr().add(idx) as *const __m128i);

        // Convert to i16 (low and high)
        let va_lo = _mm_cvtepi8_epi16(va);
        let va_hi = _mm_cvtepi8_epi16(_mm_srli_si128(va, 8));
        let vb_lo = _mm_cvtepi8_epi16(vb);
        let vb_hi = _mm_cvtepi8_epi16(_mm_srli_si128(vb, 8));

        // Multiply and accumulate
        let prod_lo = _mm_madd_epi16(va_lo, vb_lo);
        let prod_hi = _mm_madd_epi16(va_hi, vb_hi);

        sum = _mm_add_epi32(sum, prod_lo);
        sum = _mm_add_epi32(sum, prod_hi);
    }

    // Horizontal sum
    let sum64 = _mm_add_epi32(sum, _mm_srli_si128(sum, 8));
    let sum32 = _mm_add_epi32(sum64, _mm_srli_si128(sum64, 4));
    let mut result = _mm_cvtsi128_si32(sum32);

    // Handle remaining elements
    for i in (chunks * 16)..len {
        result += (a[i] as i32) * (b[i] as i32);
    }

    result
}

/// Compute dot product of i8 vector with f32 query (asymmetric)
#[inline]
pub fn dot_product_i8_f32_simd(a: &[i8], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "Vectors must have same length");

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { dot_product_i8_f32_avx2(a, b) };
        }
    }

    dot_product_i8_f32_scalar(a, b)
}

/// Scalar fallback for i8-f32 dot product
#[inline]
fn dot_product_i8_f32_scalar(a: &[i8], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        sum += (*x as f32) * y;
    }
    sum
}

/// AVX2 implementation of i8-f32 dot product
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn dot_product_i8_f32_avx2(a: &[i8], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm256_setzero_ps();

    // Process 8 elements at a time
    let chunks = len / 8;
    for i in 0..chunks {
        let idx = i * 8;

        // Load 8 i8 values and convert to f32
        let va_i8 = _mm_loadl_epi64(a.as_ptr().add(idx) as *const __m128i);
        let va_i16 = _mm_cvtepi8_epi16(va_i8);
        let va_i32 = _mm256_cvtepi16_epi32(va_i16);
        let va_f32 = _mm256_cvtepi32_ps(va_i32);

        // Load 8 f32 values
        let vb = _mm256_loadu_ps(b.as_ptr().add(idx));

        // Multiply and accumulate
        sum = _mm256_fmadd_ps(va_f32, vb, sum);
    }

    // Horizontal sum
    let sum128 = _mm_add_ps(_mm256_castps256_ps128(sum), _mm256_extractf128_ps(sum, 1));
    let sum64 = _mm_add_ps(sum128, _mm_movehl_ps(sum128, sum128));
    let sum32 = _mm_add_ss(sum64, _mm_shuffle_ps(sum64, sum64, 1));
    let mut result = _mm_cvtss_f32(sum32);

    // Handle remaining elements
    for i in (chunks * 8)..len {
        result += (a[i] as f32) * b[i];
    }

    result
}

/// Compute L2 squared distance between two i8 vectors
#[inline]
pub fn l2_squared_i8_simd(a: &[i8], b: &[i8]) -> i32 {
    debug_assert_eq!(a.len(), b.len(), "Vectors must have same length");

    // For now, use scalar. Can be SIMD optimized later.
    let mut sum = 0i32;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = (*x as i32) - (*y as i32);
        sum += d * d;
    }
    sum
}

// ============================================================================
// Storage Index
// ============================================================================

/// Index of int8 vectors for batch operations
#[derive(Clone)]
pub struct Int8Index {
    /// All quantized vectors (flattened)
    vectors: Vec<i8>,
    /// Scale factors for each vector
    scales: Vec<f32>,
    /// L2 norms for each vector
    norms: Vec<f32>,
    /// Dimensionality
    dim: usize,
    /// Number of vectors
    n_vectors: usize,
}

impl Int8Index {
    /// Create a new int8 index
    pub fn new(dim: usize) -> Self {
        Self {
            vectors: Vec::new(),
            scales: Vec::new(),
            norms: Vec::new(),
            dim,
            n_vectors: 0,
        }
    }

    /// Create with pre-allocated capacity
    pub fn with_capacity(dim: usize, capacity: usize) -> Self {
        Self {
            vectors: Vec::with_capacity(capacity * dim),
            scales: Vec::with_capacity(capacity),
            norms: Vec::with_capacity(capacity),
            dim,
            n_vectors: 0,
        }
    }

    /// Add a vector to the index
    pub fn add(&mut self, vector: &Int8Vector) {
        debug_assert_eq!(vector.dim(), self.dim, "Dimension mismatch");
        self.vectors.extend_from_slice(&vector.data);
        self.scales.push(vector.scale);
        self.norms.push(vector.norm);
        self.n_vectors += 1;
    }

    /// Add a fp32 vector (will be quantized)
    pub fn add_f32(&mut self, vector: &[f32]) {
        let int8 = Int8Vector::from_f32(vector);
        self.add(&int8);
    }

    /// Get number of vectors
    #[inline]
    pub fn len(&self) -> usize {
        self.n_vectors
    }

    /// Check if empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.n_vectors == 0
    }

    /// Get memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        self.vectors.len() + self.scales.len() * 4 + self.norms.len() * 4
    }

    /// Get a vector by index
    pub fn get(&self, idx: usize) -> Option<Int8Vector> {
        if idx >= self.n_vectors {
            return None;
        }
        let start = idx * self.dim;
        let end = start + self.dim;
        Some(Int8Vector::from_raw(
            self.vectors[start..end].to_vec(),
            self.scales[idx],
            self.norms[idx],
        ))
    }

    /// Get raw data slice for a vector
    #[inline]
    pub fn get_data(&self, idx: usize) -> &[i8] {
        let start = idx * self.dim;
        let end = start + self.dim;
        &self.vectors[start..end]
    }

    /// Compute dot product with fp32 query for a specific vector
    #[inline]
    pub fn dot_product_f32(&self, idx: usize, query: &[f32]) -> f32 {
        let data = self.get_data(idx);
        let scale = self.scales[idx];
        dot_product_i8_f32_simd(data, query) * scale
    }

    /// Rescore candidates from binary search using int8 dot product
    ///
    /// Takes (index, hamming_distance) pairs and returns (index, rescored_distance).
    pub fn rescore_candidates(
        &self,
        candidates: &[(usize, u32)],
        query: &[f32],
    ) -> Vec<(usize, f32)> {
        let mut results: Vec<(usize, f32)> = candidates
            .iter()
            .filter_map(|&(idx, _)| {
                if idx < self.n_vectors {
                    // Use negative dot product as distance (higher dot = lower distance)
                    let dot = self.dot_product_f32(idx, query);
                    Some((idx, -dot))
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        results
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_int8_quantization() {
        let values = vec![1.0, -1.0, 0.5, -0.5, 0.0];
        let int8 = Int8Vector::from_f32(&values);

        // Max abs is 1.0, scale = 1.0/127 ≈ 0.00787
        assert_eq!(int8.data[0], 127); // 1.0 -> 127
        assert_eq!(int8.data[1], -127); // -1.0 -> -127
        assert_eq!(int8.data[4], 0); // 0.0 -> 0
    }

    #[test]
    fn test_dot_product_identical() {
        let v1 = Int8Vector::from_f32(&[1.0, 2.0, 3.0, 4.0]);
        let v2 = Int8Vector::from_f32(&[1.0, 2.0, 3.0, 4.0]);

        let dot = v1.dot_product(&v2);
        let expected = 1.0 + 4.0 + 9.0 + 16.0; // 30.0
        assert!((dot - expected).abs() < 1.0); // Allow quantization error
    }

    #[test]
    fn test_dot_product_f32() {
        let int8 = Int8Vector::from_f32(&[1.0, 0.0, -1.0, 0.5]);
        let query = vec![1.0, 1.0, 1.0, 1.0];

        let dot = int8.dot_product_f32(&query);
        // 1*1 + 0*1 + (-1)*1 + 0.5*1 = 0.5
        assert!((dot - 0.5).abs() < 0.1);
    }

    #[test]
    fn test_compression_ratio() {
        // fp32: 1024 * 4 = 4096 bytes
        // int8: 1024 * 1 + 8 = 1032 bytes
        // ratio: ~4x

        let fp32_size = 1024 * 4;
        let int8 = Int8Vector::from_f32(&vec![1.0; 1024]);
        let int8_size = int8.size_bytes();

        assert_eq!(int8_size, 1032);
        assert!(fp32_size / int8_size >= 3); // At least 3x compression
    }

    #[test]
    fn test_index_rescore() {
        let mut index = Int8Index::new(4);

        index.add_f32(&[1.0, 0.0, 0.0, 0.0]);
        index.add_f32(&[0.0, 1.0, 0.0, 0.0]);
        index.add_f32(&[0.0, 0.0, 1.0, 0.0]);

        let query = vec![1.0, 0.0, 0.0, 0.0];

        // Simulate binary search results
        let binary_candidates = vec![(0, 10), (1, 20), (2, 30)];

        let rescored = index.rescore_candidates(&binary_candidates, &query);

        // Vector 0 should be closest (highest dot product = lowest distance)
        assert_eq!(rescored[0].0, 0);
    }

    #[test]
    fn test_simd_vs_scalar() {
        let a: Vec<i8> = (0..128).map(|i| (i % 127) as i8).collect();
        let b: Vec<i8> = (0..128).map(|i| ((127 - i) % 127) as i8).collect();

        let scalar = dot_product_i8_scalar(&a, &b);

        #[cfg(target_arch = "x86_64")]
        {
            let simd = dot_product_i8_simd(&a, &b);
            assert_eq!(scalar, simd);
        }
    }
}
