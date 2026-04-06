//! Distance Functions for Vector Operations
//!
//! Implements L2 (Euclidean), Cosine, and Inner Product distance metrics
//! from scratch - no external dependencies.
//!
//! # Distance Metrics
//!
//! - **L2 (Euclidean)**: sqrt(sum((a[i] - b[i])^2))
//! - **Cosine**: 1 - (a · b) / (||a|| * ||b||)
//! - **Inner Product**: -(a · b) (negated for min-heap compatibility)
//!
//! # SIMD Acceleration
//!
//! When compiled for x86_64, uses SIMD intrinsics (SSE/AVX/FMA) for
//! 4-8x faster distance computations. See [`super::simd_distance`] for details.

use std::cmp::Ordering;

// Re-export SIMD functions for direct access
pub use super::simd_distance::{
    batch_distances, cosine_distance_simd, distance_simd, dot_product_simd,
    inner_product_distance_simd, l2_norm_simd, l2_squared_simd, simd_level, SimdLevel,
};

/// Distance metric types supported by vector operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DistanceMetric {
    /// Euclidean (L2) distance - good for dense vectors
    L2,
    /// Cosine distance - good for normalized embeddings
    Cosine,
    /// Inner product (dot product) - for maximum inner product search
    InnerProduct,
}

impl Default for DistanceMetric {
    fn default() -> Self {
        Self::L2
    }
}

/// Compute L2 (Euclidean) squared distance between two vectors
///
/// Returns the squared distance to avoid expensive sqrt operation.
/// For ranking purposes, squared distance preserves order.
#[inline]
pub fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "Vector dimensions must match");

    let mut sum = 0.0f32;
    let len = a.len();

    // Process in chunks of 4 for better cache utilization
    let chunks = len / 4;
    for i in 0..chunks {
        let idx = i * 4;
        let d0 = a[idx] - b[idx];
        let d1 = a[idx + 1] - b[idx + 1];
        let d2 = a[idx + 2] - b[idx + 2];
        let d3 = a[idx + 3] - b[idx + 3];
        sum += d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3;
    }

    // Handle remaining elements
    for i in (chunks * 4)..len {
        let d = a[i] - b[i];
        sum += d * d;
    }

    sum
}

/// Compute L2 (Euclidean) distance between two vectors
#[inline]
pub fn l2(a: &[f32], b: &[f32]) -> f32 {
    l2_squared(a, b).sqrt()
}

/// Compute dot product (inner product) between two vectors
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "Vector dimensions must match");

    let mut sum = 0.0f32;
    let len = a.len();

    // Process in chunks of 4
    let chunks = len / 4;
    for i in 0..chunks {
        let idx = i * 4;
        sum += a[idx] * b[idx];
        sum += a[idx + 1] * b[idx + 1];
        sum += a[idx + 2] * b[idx + 2];
        sum += a[idx + 3] * b[idx + 3];
    }

    // Handle remaining elements
    for i in (chunks * 4)..len {
        sum += a[i] * b[i];
    }

    sum
}

/// Compute the L2 norm (magnitude) of a vector
#[inline]
pub fn l2_norm(v: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for &x in v {
        sum += x * x;
    }
    sum.sqrt()
}

/// Compute cosine distance between two vectors
///
/// Cosine distance = 1 - cosine_similarity
/// where cosine_similarity = (a · b) / (||a|| * ||b||)
#[inline]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let dot = dot_product(a, b);
    let norm_a = l2_norm(a);
    let norm_b = l2_norm(b);

    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0; // Maximum distance for zero vectors
    }

    let similarity = dot / (norm_a * norm_b);
    // Clamp to [-1, 1] to handle floating point errors
    let similarity = similarity.clamp(-1.0, 1.0);
    1.0 - similarity
}

/// Compute inner product distance (negated for min-heap compatibility)
///
/// For maximum inner product search, we negate the dot product
/// so that smaller values indicate higher similarity.
#[inline]
pub fn inner_product_distance(a: &[f32], b: &[f32]) -> f32 {
    -dot_product(a, b)
}

/// Compute distance between two vectors using the specified metric
#[inline]
pub fn distance(a: &[f32], b: &[f32], metric: DistanceMetric) -> f32 {
    match metric {
        DistanceMetric::L2 => l2_squared(a, b), // Use squared for efficiency
        DistanceMetric::Cosine => cosine_distance(a, b),
        DistanceMetric::InnerProduct => inner_product_distance(a, b),
    }
}

/// Normalize a vector to unit length (in-place)
pub fn normalize(v: &mut [f32]) {
    let norm = l2_norm(v);
    if norm > 0.0 {
        let inv_norm = 1.0 / norm;
        for x in v.iter_mut() {
            *x *= inv_norm;
        }
    }
}

/// Create a normalized copy of a vector
pub fn normalized(v: &[f32]) -> Vec<f32> {
    let mut result = v.to_vec();
    normalize(&mut result);
    result
}

pub fn cmp_f32(a: f32, b: f32) -> Ordering {
    match a.partial_cmp(&b) {
        Some(order) => order,
        None => {
            if a.is_nan() && b.is_nan() {
                Ordering::Equal
            } else if a.is_nan() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
    }
}

/// A distance value that can be compared and used in heaps
#[derive(Debug, Clone, Copy)]
pub struct Distance(pub f32);

impl PartialEq for Distance {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Distance {}

impl PartialOrd for Distance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Distance {
    fn cmp(&self, other: &Self) -> Ordering {
        // Handle NaN by treating it as greater than any other value
        self.0.partial_cmp(&other.0).unwrap_or(Ordering::Greater)
    }
}

/// Result of a distance computation with an ID
#[derive(Debug, Clone)]
pub struct DistanceResult {
    pub id: u64,
    pub distance: f32,
}

impl DistanceResult {
    pub fn new(id: u64, distance: f32) -> Self {
        Self { id, distance }
    }
}

impl PartialEq for DistanceResult {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance
    }
}

impl Eq for DistanceResult {}

impl PartialOrd for DistanceResult {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DistanceResult {
    fn cmp(&self, other: &Self) -> Ordering {
        // For min-heap: smaller distance = higher priority
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(Ordering::Equal)
    }
}

/// Reverse ordering for max-heap operations
#[derive(Debug, Clone)]
pub struct ReverseDistanceResult(pub DistanceResult);

impl PartialEq for ReverseDistanceResult {
    fn eq(&self, other: &Self) -> bool {
        self.0.distance == other.0.distance
    }
}

impl Eq for ReverseDistanceResult {}

impl PartialOrd for ReverseDistanceResult {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReverseDistanceResult {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: larger distance = higher priority (for max-heap)
        other
            .0
            .distance
            .partial_cmp(&self.0.distance)
            .unwrap_or(Ordering::Equal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l2_squared_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        assert_eq!(l2_squared(&a, &b), 0.0);
    }

    #[test]
    fn test_l2_squared_simple() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(l2_squared(&a, &b), 1.0);
    }

    #[test]
    fn test_l2_squared_3d() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 2.0];
        assert_eq!(l2_squared(&a, &b), 9.0); // 1 + 4 + 4 = 9
    }

    #[test]
    fn test_l2_distance() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 2.0];
        assert_eq!(l2(&a, &b), 3.0); // sqrt(9) = 3
    }

    #[test]
    fn test_dot_product() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        assert_eq!(dot_product(&a, &b), 32.0); // 1*4 + 2*5 + 3*6 = 32
    }

    #[test]
    fn test_l2_norm() {
        let v = vec![3.0, 4.0];
        assert_eq!(l2_norm(&v), 5.0); // sqrt(9 + 16) = 5
    }

    #[test]
    fn test_cosine_distance_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_distance(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_distance_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((cosine_distance(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_distance_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_distance(&a, &b) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
        assert!((l2_norm(&v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_inner_product_distance() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0];
        assert_eq!(inner_product_distance(&a, &b), -1.0);
    }

    #[test]
    fn test_distance_result_ordering() {
        let r1 = DistanceResult::new(1, 0.5);
        let r2 = DistanceResult::new(2, 1.0);
        assert!(r1 < r2); // Smaller distance is "less than"
    }

    #[test]
    fn test_long_vector() {
        // Test with vector length > 4 to exercise chunked processing
        let a: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..100).map(|i| (i + 1) as f32).collect();

        let dist = l2_squared(&a, &b);
        assert_eq!(dist, 100.0); // Each element differs by 1, so sum of 100 1^2 = 100
    }
}
