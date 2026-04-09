//! SIMD-Optimized Distance Functions
//!
//! Provides hardware-accelerated distance computations using x86_64 SIMD intrinsics.
//! Falls back to scalar implementation when SIMD is not available.
//!
//! # Supported Instructions
//!
//! - **SSE**: 128-bit vectors, 4 f32 operations in parallel
//! - **AVX**: 256-bit vectors, 8 f32 operations in parallel
//! - **FMA**: Fused multiply-add for better precision and performance
//!
//! # Runtime Detection
//!
//! Uses `is_x86_feature_detected!` to select the best available implementation at runtime.

use super::distance::DistanceMetric;

/// SIMD capability level detected at runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimdLevel {
    /// No SIMD available, use scalar
    Scalar,
    /// SSE 128-bit SIMD (4 x f32)
    Sse,
    /// AVX 256-bit SIMD (8 x f32)
    Avx,
    /// AVX with FMA (fused multiply-add)
    AvxFma,
}

impl SimdLevel {
    /// Detect the best available SIMD level at runtime
    #[inline]
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx") && is_x86_feature_detected!("fma") {
                SimdLevel::AvxFma
            } else if is_x86_feature_detected!("avx") {
                SimdLevel::Avx
            } else if is_x86_feature_detected!("sse") {
                SimdLevel::Sse
            } else {
                SimdLevel::Scalar
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            SimdLevel::Scalar
        }
    }
}

/// Global SIMD level (detected once at first use)
static SIMD_LEVEL: std::sync::OnceLock<SimdLevel> = std::sync::OnceLock::new();

/// Get the detected SIMD level
#[inline]
pub fn simd_level() -> SimdLevel {
    *SIMD_LEVEL.get_or_init(SimdLevel::detect)
}

// ============================================================================
// L2 Squared Distance
// ============================================================================

/// Compute L2 squared distance using the best available SIMD
#[inline]
pub fn l2_squared_simd(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vector dimensions must match");

    match simd_level() {
        #[cfg(target_arch = "x86_64")]
        SimdLevel::AvxFma => unsafe { l2_squared_avx_fma(a, b) },
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Avx => unsafe { l2_squared_avx(a, b) },
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse => unsafe { l2_squared_sse(a, b) },
        _ => l2_squared_scalar(a, b),
    }
}

/// Scalar fallback for L2 squared distance
#[inline]
fn l2_squared_scalar(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        let d = a[i] - b[i];
        sum += d * d;
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse")]
unsafe fn l2_squared_sse(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm_setzero_ps(); // 4 x f32 = 0.0

    // Process 4 elements at a time
    let chunks = len / 4;
    for i in 0..chunks {
        let idx = i * 4;
        let va = _mm_loadu_ps(a.as_ptr().add(idx));
        let vb = _mm_loadu_ps(b.as_ptr().add(idx));
        let diff = _mm_sub_ps(va, vb);
        let sq = _mm_mul_ps(diff, diff);
        sum = _mm_add_ps(sum, sq);
    }

    // Horizontal sum of the 4 lanes
    let mut result = horizontal_sum_sse(sum);

    // Handle remaining elements
    for i in (chunks * 4)..len {
        let d = a[i] - b[i];
        result += d * d;
    }

    result
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse")]
#[inline]
unsafe fn horizontal_sum_sse(v: std::arch::x86_64::__m128) -> f32 {
    use std::arch::x86_64::*;

    // v = [a, b, c, d]
    // Add pairs: [a+c, b+d, a+c, b+d]
    let shuf = _mm_movehdup_ps(v); // [b, b, d, d]
    let sums = _mm_add_ps(v, shuf); // [a+b, b+b, c+d, d+d]
    let shuf2 = _mm_movehl_ps(sums, sums); // [c+d, d+d, c+d, d+d]
    let sums2 = _mm_add_ss(sums, shuf2); // [a+b+c+d, ...]
    _mm_cvtss_f32(sums2)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn l2_squared_avx(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm256_setzero_ps(); // 8 x f32 = 0.0

    // Process 8 elements at a time
    let chunks = len / 8;
    for i in 0..chunks {
        let idx = i * 8;
        let va = _mm256_loadu_ps(a.as_ptr().add(idx));
        let vb = _mm256_loadu_ps(b.as_ptr().add(idx));
        let diff = _mm256_sub_ps(va, vb);
        let sq = _mm256_mul_ps(diff, diff);
        sum = _mm256_add_ps(sum, sq);
    }

    // Horizontal sum of the 8 lanes
    let mut result = horizontal_sum_avx(sum);

    // Handle remaining elements (up to 7)
    for i in (chunks * 8)..len {
        let d = a[i] - b[i];
        result += d * d;
    }

    result
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
#[inline]
unsafe fn horizontal_sum_avx(v: std::arch::x86_64::__m256) -> f32 {
    use std::arch::x86_64::*;

    // Extract high and low 128-bit halves and add them
    let high = _mm256_extractf128_ps(v, 1);
    let low = _mm256_castps256_ps128(v);
    let sum128 = _mm_add_ps(high, low);

    // Now do SSE horizontal sum
    horizontal_sum_sse(sum128)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx", enable = "fma")]
unsafe fn l2_squared_avx_fma(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm256_setzero_ps();

    // Process 8 elements at a time using FMA
    let chunks = len / 8;
    for i in 0..chunks {
        let idx = i * 8;
        let va = _mm256_loadu_ps(a.as_ptr().add(idx));
        let vb = _mm256_loadu_ps(b.as_ptr().add(idx));
        let diff = _mm256_sub_ps(va, vb);
        // FMA: sum = diff * diff + sum (fused, more accurate)
        sum = _mm256_fmadd_ps(diff, diff, sum);
    }

    let mut result = horizontal_sum_avx(sum);

    // Handle remaining elements
    for i in (chunks * 8)..len {
        let d = a[i] - b[i];
        result += d * d;
    }

    result
}

// ============================================================================
// Dot Product
// ============================================================================

/// Compute dot product using the best available SIMD
#[inline]
pub fn dot_product_simd(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vector dimensions must match");

    match simd_level() {
        #[cfg(target_arch = "x86_64")]
        SimdLevel::AvxFma => unsafe { dot_product_avx_fma(a, b) },
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Avx => unsafe { dot_product_avx(a, b) },
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse => unsafe { dot_product_sse(a, b) },
        _ => dot_product_scalar(a, b),
    }
}

#[inline]
fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        sum += a[i] * b[i];
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse")]
unsafe fn dot_product_sse(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm_setzero_ps();

    let chunks = len / 4;
    for i in 0..chunks {
        let idx = i * 4;
        let va = _mm_loadu_ps(a.as_ptr().add(idx));
        let vb = _mm_loadu_ps(b.as_ptr().add(idx));
        let prod = _mm_mul_ps(va, vb);
        sum = _mm_add_ps(sum, prod);
    }

    let mut result = horizontal_sum_sse(sum);

    for i in (chunks * 4)..len {
        result += a[i] * b[i];
    }

    result
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn dot_product_avx(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm256_setzero_ps();

    let chunks = len / 8;
    for i in 0..chunks {
        let idx = i * 8;
        let va = _mm256_loadu_ps(a.as_ptr().add(idx));
        let vb = _mm256_loadu_ps(b.as_ptr().add(idx));
        let prod = _mm256_mul_ps(va, vb);
        sum = _mm256_add_ps(sum, prod);
    }

    let mut result = horizontal_sum_avx(sum);

    for i in (chunks * 8)..len {
        result += a[i] * b[i];
    }

    result
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx", enable = "fma")]
unsafe fn dot_product_avx_fma(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let mut sum = _mm256_setzero_ps();

    let chunks = len / 8;
    for i in 0..chunks {
        let idx = i * 8;
        let va = _mm256_loadu_ps(a.as_ptr().add(idx));
        let vb = _mm256_loadu_ps(b.as_ptr().add(idx));
        // FMA: sum = va * vb + sum
        sum = _mm256_fmadd_ps(va, vb, sum);
    }

    let mut result = horizontal_sum_avx(sum);

    for i in (chunks * 8)..len {
        result += a[i] * b[i];
    }

    result
}

// ============================================================================
// L2 Norm (magnitude)
// ============================================================================

/// Compute L2 norm using the best available SIMD
#[inline]
pub fn l2_norm_simd(v: &[f32]) -> f32 {
    dot_product_simd(v, v).sqrt()
}

// ============================================================================
// Cosine Distance
// ============================================================================

/// Compute cosine distance using SIMD
#[inline]
pub fn cosine_distance_simd(a: &[f32], b: &[f32]) -> f32 {
    let dot = dot_product_simd(a, b);
    let norm_a = l2_norm_simd(a);
    let norm_b = l2_norm_simd(b);

    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }

    let similarity = (dot / (norm_a * norm_b)).clamp(-1.0, 1.0);
    1.0 - similarity
}

/// Compute inner product distance using SIMD
#[inline]
pub fn inner_product_distance_simd(a: &[f32], b: &[f32]) -> f32 {
    -dot_product_simd(a, b)
}

// ============================================================================
// Unified Distance Function
// ============================================================================

/// Compute distance using SIMD with the specified metric
#[inline]
pub fn distance_simd(a: &[f32], b: &[f32], metric: DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::L2 => l2_squared_simd(a, b),
        DistanceMetric::Cosine => cosine_distance_simd(a, b),
        DistanceMetric::InnerProduct => inner_product_distance_simd(a, b),
    }
}

// ============================================================================
// Batch Operations (for processing multiple vectors efficiently)
// ============================================================================

/// Compute distances from a query vector to multiple target vectors
/// Returns (index, distance) pairs sorted by distance
pub fn batch_distances(
    query: &[f32],
    targets: &[Vec<f32>],
    metric: DistanceMetric,
    top_k: usize,
) -> Vec<(usize, f32)> {
    let mut results: Vec<(usize, f32)> = targets
        .iter()
        .enumerate()
        .map(|(i, target)| (i, distance_simd(query, target, metric)))
        .collect();

    // Partial sort for top-k (more efficient than full sort)
    if top_k < results.len() {
        results.select_nth_unstable_by(top_k, |a, b| {
            a.1
                .partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        results.truncate(top_k);
    }

    results.sort_by(|a, b| {
        a.1
            .partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    results
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simd_level_detection() {
        let level = simd_level();
        println!("Detected SIMD level: {:?}", level);
        // Should detect at least scalar on any platform
        assert!(matches!(
            level,
            SimdLevel::Scalar | SimdLevel::Sse | SimdLevel::Avx | SimdLevel::AvxFma
        ));
    }

    #[test]
    fn test_l2_squared_simd_identical() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let b = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        assert!((l2_squared_simd(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_l2_squared_simd_simple() {
        let a = vec![0.0, 0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0, 0.0];
        assert!((l2_squared_simd(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_l2_squared_simd_vs_scalar() {
        let a: Vec<f32> = (0..256).map(|i| i as f32 * 0.1).collect();
        let b: Vec<f32> = (0..256).map(|i| (i + 1) as f32 * 0.1).collect();

        let simd_result = l2_squared_simd(&a, &b);
        let scalar_result = l2_squared_scalar(&a, &b);

        assert!(
            (simd_result - scalar_result).abs() < 1e-3,
            "SIMD: {}, Scalar: {}",
            simd_result,
            scalar_result
        );
    }

    #[test]
    fn test_dot_product_simd() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let b = vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let result = dot_product_simd(&a, &b);
        assert!((result - 36.0).abs() < 1e-6); // 1+2+3+4+5+6+7+8 = 36
    }

    #[test]
    fn test_dot_product_simd_vs_scalar() {
        let a: Vec<f32> = (0..256).map(|i| i as f32 * 0.1).collect();
        let b: Vec<f32> = (0..256).map(|i| (i + 1) as f32 * 0.1).collect();

        let simd_result = dot_product_simd(&a, &b);
        let scalar_result = dot_product_scalar(&a, &b);

        assert!(
            (simd_result - scalar_result).abs() < 1.0, // Larger tolerance for accumulated FP
            "SIMD: {}, Scalar: {}",
            simd_result,
            scalar_result
        );
    }

    #[test]
    fn test_l2_norm_simd() {
        let v = vec![3.0, 4.0, 0.0, 0.0];
        assert!((l2_norm_simd(&v) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_distance_simd_identical() {
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0, 0.0];
        assert!((cosine_distance_simd(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_distance_simd_orthogonal() {
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0, 0.0];
        assert!((cosine_distance_simd(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_batch_distances() {
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let targets = vec![
            vec![1.0, 0.0, 0.0, 0.0], // distance = 1.0
            vec![2.0, 0.0, 0.0, 0.0], // distance = 4.0
            vec![0.5, 0.0, 0.0, 0.0], // distance = 0.25
        ];

        let results = batch_distances(&query, &targets, DistanceMetric::L2, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 2); // Closest is index 2 (distance 0.25)
        assert_eq!(results[1].0, 0); // Second is index 0 (distance 1.0)
    }

    #[test]
    fn test_odd_length_vectors() {
        // Test vectors that don't align to SIMD width
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0]; // 5 elements
        let b = vec![5.0, 4.0, 3.0, 2.0, 1.0];

        let simd_result = l2_squared_simd(&a, &b);
        let expected = 16.0 + 4.0 + 0.0 + 4.0 + 16.0; // = 40.0
        assert!((simd_result - expected).abs() < 1e-6);
    }

    #[test]
    fn test_large_vectors() {
        // Test with large vectors (1536 dimensions like text-embedding-3-large)
        let a: Vec<f32> = (0..1536).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..1536).map(|i| (i as f32).cos()).collect();

        let simd_result = l2_squared_simd(&a, &b);
        let scalar_result = l2_squared_scalar(&a, &b);

        assert!(
            (simd_result - scalar_result).abs() / scalar_result.abs() < 1e-5,
            "Relative error too large: SIMD={}, Scalar={}",
            simd_result,
            scalar_result
        );
    }
}
