//! Distance Metrics for Vector Similarity
//!
//! Provides distance/similarity metrics for comparing vectors.
//! All metrics are optimized with loop unrolling for better performance.

use super::types::DenseVector;

/// Distance metric type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distance {
    /// Euclidean distance (L2 norm of difference)
    L2,
    /// Squared Euclidean distance (avoids sqrt for faster computation)
    L2Squared,
    /// Cosine distance (1 - cosine similarity)
    Cosine,
    /// Dot product (negative for distance interpretation)
    DotProduct,
    /// Manhattan distance (L1 norm)
    Manhattan,
}

impl Distance {
    /// Compute distance between two vectors
    #[inline]
    pub fn compute(&self, a: &DenseVector, b: &DenseVector) -> f32 {
        match self {
            Distance::L2 => l2_distance(a.as_slice(), b.as_slice()),
            Distance::L2Squared => l2_squared_distance(a.as_slice(), b.as_slice()),
            Distance::Cosine => cosine_distance(a.as_slice(), b.as_slice()),
            Distance::DotProduct => -dot_product(a.as_slice(), b.as_slice()),
            Distance::Manhattan => manhattan_distance(a.as_slice(), b.as_slice()),
        }
    }

    /// Check if smaller distance means more similar
    #[inline]
    pub fn is_smaller_better(&self) -> bool {
        true // All our metrics are distances (lower = more similar)
    }
}

/// Trait for custom distance metrics
pub trait DistanceMetric: Send + Sync {
    /// Compute distance between two vectors
    fn distance(&self, a: &[f32], b: &[f32]) -> f32;

    /// Name of the metric for display
    fn name(&self) -> &str;
}

/// L2 (Euclidean) distance
///
/// d(a, b) = sqrt(sum((a_i - b_i)^2))
#[inline]
pub fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    l2_squared_distance(a, b).sqrt()
}

/// Squared L2 distance (faster, avoids sqrt)
///
/// d(a, b) = sum((a_i - b_i)^2)
#[inline]
pub fn l2_squared_distance(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());

    let len = a.len();
    let mut sum = 0.0f32;

    // Process 4 elements at a time (loop unrolling)
    let chunks = len / 4;
    for i in 0..chunks {
        let base = i * 4;
        let d0 = a[base] - b[base];
        let d1 = a[base + 1] - b[base + 1];
        let d2 = a[base + 2] - b[base + 2];
        let d3 = a[base + 3] - b[base + 3];
        sum += d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3;
    }

    // Handle remaining elements
    for i in (chunks * 4)..len {
        let d = a[i] - b[i];
        sum += d * d;
    }

    sum
}

/// Cosine distance
///
/// d(a, b) = 1 - (a · b) / (||a|| * ||b||)
///
/// Range: [0, 2] where 0 = identical direction, 2 = opposite direction
#[inline]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());

    let len = a.len();
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    // Process 4 elements at a time
    let chunks = len / 4;
    for i in 0..chunks {
        let base = i * 4;

        dot += a[base] * b[base]
            + a[base + 1] * b[base + 1]
            + a[base + 2] * b[base + 2]
            + a[base + 3] * b[base + 3];

        norm_a += a[base] * a[base]
            + a[base + 1] * a[base + 1]
            + a[base + 2] * a[base + 2]
            + a[base + 3] * a[base + 3];

        norm_b += b[base] * b[base]
            + b[base + 1] * b[base + 1]
            + b[base + 2] * b[base + 2]
            + b[base + 3] * b[base + 3];
    }

    // Handle remaining elements
    for i in (chunks * 4)..len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom < 1e-10 {
        return 1.0; // Both vectors are zero
    }

    let cosine_sim = dot / denom;
    // Clamp to handle floating point errors
    1.0 - cosine_sim.clamp(-1.0, 1.0)
}

/// Dot product (inner product)
///
/// a · b = sum(a_i * b_i)
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());

    let len = a.len();
    let mut sum = 0.0f32;

    // Process 4 elements at a time
    let chunks = len / 4;
    for i in 0..chunks {
        let base = i * 4;
        sum += a[base] * b[base]
            + a[base + 1] * b[base + 1]
            + a[base + 2] * b[base + 2]
            + a[base + 3] * b[base + 3];
    }

    // Handle remaining elements
    for i in (chunks * 4)..len {
        sum += a[i] * b[i];
    }

    sum
}

/// Manhattan distance (L1 distance)
///
/// d(a, b) = sum(|a_i - b_i|)
#[inline]
pub fn manhattan_distance(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());

    let len = a.len();
    let mut sum = 0.0f32;

    // Process 4 elements at a time
    let chunks = len / 4;
    for i in 0..chunks {
        let base = i * 4;
        sum += (a[base] - b[base]).abs()
            + (a[base + 1] - b[base + 1]).abs()
            + (a[base + 2] - b[base + 2]).abs()
            + (a[base + 3] - b[base + 3]).abs();
    }

    // Handle remaining elements
    for i in (chunks * 4)..len {
        sum += (a[i] - b[i]).abs();
    }

    sum
}

/// Cosine similarity (not distance)
///
/// sim(a, b) = (a · b) / (||a|| * ||b||)
///
/// Range: [-1, 1] where 1 = identical direction, -1 = opposite direction
#[inline]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    1.0 - cosine_distance(a, b)
}

/// Normalize a vector in-place
#[inline]
pub fn normalize_vector(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-10 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l2_distance() {
        let a = [1.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];

        let dist = l2_distance(&a, &b);
        assert!((dist - std::f32::consts::SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn test_l2_distance_same() {
        let a = [1.0, 2.0, 3.0];
        let dist = l2_distance(&a, &a);
        assert!(dist.abs() < 1e-6);
    }

    #[test]
    fn test_l2_squared_distance() {
        let a = [3.0, 0.0];
        let b = [0.0, 4.0];

        let dist = l2_squared_distance(&a, &b);
        assert!((dist - 25.0).abs() < 1e-6); // 3^2 + 4^2 = 25
    }

    #[test]
    fn test_cosine_distance_orthogonal() {
        let a = [1.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];

        let dist = cosine_distance(&a, &b);
        assert!((dist - 1.0).abs() < 1e-6); // Orthogonal = cosine similarity 0 = distance 1
    }

    #[test]
    fn test_cosine_distance_same_direction() {
        let a = [1.0, 2.0, 3.0];
        let b = [2.0, 4.0, 6.0]; // Same direction, different magnitude

        let dist = cosine_distance(&a, &b);
        assert!(dist.abs() < 1e-6); // Same direction = distance 0
    }

    #[test]
    fn test_cosine_distance_opposite() {
        let a = [1.0, 0.0, 0.0];
        let b = [-1.0, 0.0, 0.0];

        let dist = cosine_distance(&a, &b);
        assert!((dist - 2.0).abs() < 1e-6); // Opposite direction = distance 2
    }

    #[test]
    fn test_dot_product() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];

        let dot = dot_product(&a, &b);
        assert!((dot - 32.0).abs() < 1e-6); // 1*4 + 2*5 + 3*6 = 32
    }

    #[test]
    fn test_manhattan_distance() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 6.0, 3.0];

        let dist = manhattan_distance(&a, &b);
        assert!((dist - 7.0).abs() < 1e-6); // |1-4| + |2-6| + |3-3| = 3 + 4 + 0 = 7
    }

    #[test]
    fn test_cosine_similarity() {
        let a = [1.0, 0.0];
        let b = [1.0, 0.0];

        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_vector() {
        let mut v = [3.0, 4.0];
        normalize_vector(&mut v);

        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);

        // Check norm is 1
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_enum() {
        let a = DenseVector::new(vec![1.0, 0.0, 0.0]);
        let b = DenseVector::new(vec![0.0, 1.0, 0.0]);

        let l2 = Distance::L2.compute(&a, &b);
        assert!((l2 - std::f32::consts::SQRT_2).abs() < 1e-6);

        let cosine = Distance::Cosine.compute(&a, &b);
        assert!((cosine - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_large_vector_performance() {
        // Test with 384-dim vector (common embedding size)
        let a: Vec<f32> = (0..384).map(|i| i as f32 / 384.0).collect();
        let b: Vec<f32> = (0..384).map(|i| (383 - i) as f32 / 384.0).collect();

        // Just ensure it runs without panic
        let _ = l2_distance(&a, &b);
        let _ = cosine_distance(&a, &b);
        let _ = dot_product(&a, &b);
        let _ = manhattan_distance(&a, &b);
    }
}
