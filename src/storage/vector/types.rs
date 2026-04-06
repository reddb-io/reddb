//! Vector Type Definitions
//!
//! Provides dense and sparse vector types for similarity search.

use std::fmt;

/// Vector identifier type
pub type VectorId = u64;

/// Dense vector with fixed dimensions
///
/// Stores all dimensions explicitly, optimal for vectors where most values are non-zero.
#[derive(Debug, Clone, PartialEq)]
pub struct DenseVector {
    /// Vector data as f32 array
    data: Vec<f32>,
}

impl DenseVector {
    /// Create a new dense vector
    pub fn new(data: Vec<f32>) -> Self {
        Self { data }
    }

    /// Create a zero vector of given dimension
    pub fn zeros(dim: usize) -> Self {
        Self {
            data: vec![0.0; dim],
        }
    }

    /// Create a vector from bytes (little-endian f32)
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let dim = bytes.len() / 4;
        let mut data = Vec::with_capacity(dim);
        for i in 0..dim {
            let offset = i * 4;
            let value = f32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);
            data.push(value);
        }
        Some(Self { data })
    }

    /// Get vector dimension
    #[inline]
    pub fn dim(&self) -> usize {
        self.data.len()
    }

    /// Get vector data as slice
    #[inline]
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// Get mutable vector data
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.data
    }

    /// Get element at index
    #[inline]
    pub fn get(&self, index: usize) -> Option<f32> {
        self.data.get(index).copied()
    }

    /// Set element at index
    #[inline]
    pub fn set(&mut self, index: usize, value: f32) {
        if index < self.data.len() {
            self.data[index] = value;
        }
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.data.len() * 4);
        for &value in &self.data {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    /// Compute L2 norm (magnitude)
    pub fn norm(&self) -> f32 {
        self.data.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    /// Normalize vector to unit length
    pub fn normalize(&mut self) {
        let norm = self.norm();
        if norm > 0.0 {
            for x in &mut self.data {
                *x /= norm;
            }
        }
    }

    /// Return a normalized copy
    pub fn normalized(&self) -> Self {
        let mut copy = self.clone();
        copy.normalize();
        copy
    }

    /// Add another vector element-wise
    pub fn add(&mut self, other: &DenseVector) {
        debug_assert_eq!(self.dim(), other.dim());
        for (a, b) in self.data.iter_mut().zip(other.data.iter()) {
            *a += b;
        }
    }

    /// Subtract another vector element-wise
    pub fn sub(&mut self, other: &DenseVector) {
        debug_assert_eq!(self.dim(), other.dim());
        for (a, b) in self.data.iter_mut().zip(other.data.iter()) {
            *a -= b;
        }
    }

    /// Scale by a constant
    pub fn scale(&mut self, factor: f32) {
        for x in &mut self.data {
            *x *= factor;
        }
    }

    /// Compute dot product with another vector
    pub fn dot(&self, other: &DenseVector) -> f32 {
        debug_assert_eq!(self.dim(), other.dim());
        self.data
            .iter()
            .zip(other.data.iter())
            .map(|(a, b)| a * b)
            .sum()
    }
}

impl fmt::Display for DenseVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        for (i, val) in self.data.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            if i >= 5 && self.data.len() > 6 {
                write!(f, "... ({} more)", self.data.len() - 5)?;
                break;
            }
            write!(f, "{:.4}", val)?;
        }
        write!(f, "]")
    }
}

impl From<Vec<f32>> for DenseVector {
    fn from(data: Vec<f32>) -> Self {
        Self::new(data)
    }
}

impl From<&[f32]> for DenseVector {
    fn from(data: &[f32]) -> Self {
        Self::new(data.to_vec())
    }
}

/// Sparse vector with explicit indices
///
/// Stores only non-zero values with their indices, optimal for high-dimensional
/// vectors with mostly zero values.
#[derive(Debug, Clone, PartialEq)]
pub struct SparseVector {
    /// Dimension of the full vector
    dim: usize,
    /// Indices of non-zero elements (sorted)
    indices: Vec<u32>,
    /// Values at those indices
    values: Vec<f32>,
}

impl SparseVector {
    /// Create a new sparse vector
    pub fn new(dim: usize, indices: Vec<u32>, values: Vec<f32>) -> Self {
        debug_assert_eq!(indices.len(), values.len());
        debug_assert!(indices.iter().all(|&i| (i as usize) < dim));
        Self {
            dim,
            indices,
            values,
        }
    }

    /// Create from dense vector (keeping only non-zero values)
    pub fn from_dense(dense: &DenseVector, threshold: f32) -> Self {
        let mut indices = Vec::new();
        let mut values = Vec::new();

        for (i, &val) in dense.as_slice().iter().enumerate() {
            if val.abs() > threshold {
                indices.push(i as u32);
                values.push(val);
            }
        }

        Self {
            dim: dense.dim(),
            indices,
            values,
        }
    }

    /// Get vector dimension
    #[inline]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Get number of non-zero elements
    #[inline]
    pub fn nnz(&self) -> usize {
        self.indices.len()
    }

    /// Get indices slice
    #[inline]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// Get values slice
    #[inline]
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Get value at index (returns 0 if not present)
    pub fn get(&self, index: usize) -> f32 {
        match self.indices.binary_search(&(index as u32)) {
            Ok(pos) => self.values[pos],
            Err(_) => 0.0,
        }
    }

    /// Convert to dense vector
    pub fn to_dense(&self) -> DenseVector {
        let mut data = vec![0.0; self.dim];
        for (&idx, &val) in self.indices.iter().zip(self.values.iter()) {
            data[idx as usize] = val;
        }
        DenseVector::new(data)
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Dimension (4 bytes)
        bytes.extend_from_slice(&(self.dim as u32).to_le_bytes());

        // Number of non-zero elements (4 bytes)
        bytes.extend_from_slice(&(self.indices.len() as u32).to_le_bytes());

        // Indices
        for &idx in &self.indices {
            bytes.extend_from_slice(&idx.to_le_bytes());
        }

        // Values
        for &val in &self.values {
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }

        let dim = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let nnz = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;

        let expected_len = 8 + nnz * 4 + nnz * 4;
        if bytes.len() < expected_len {
            return None;
        }

        let mut offset = 8;
        let mut indices = Vec::with_capacity(nnz);
        for _ in 0..nnz {
            let idx = u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);
            indices.push(idx);
            offset += 4;
        }

        let mut values = Vec::with_capacity(nnz);
        for _ in 0..nnz {
            let val = f32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);
            values.push(val);
            offset += 4;
        }

        Some(Self {
            dim,
            indices,
            values,
        })
    }

    /// Compute L2 norm
    pub fn norm(&self) -> f32 {
        self.values.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    /// Compute dot product with dense vector
    pub fn dot_dense(&self, dense: &DenseVector) -> f32 {
        debug_assert_eq!(self.dim, dense.dim());
        self.indices
            .iter()
            .zip(self.values.iter())
            .map(|(&idx, &val)| val * dense.as_slice()[idx as usize])
            .sum()
    }

    /// Compute dot product with another sparse vector
    pub fn dot_sparse(&self, other: &SparseVector) -> f32 {
        debug_assert_eq!(self.dim, other.dim);

        let mut result = 0.0;
        let mut i = 0;
        let mut j = 0;

        while i < self.indices.len() && j < other.indices.len() {
            if self.indices[i] == other.indices[j] {
                result += self.values[i] * other.values[j];
                i += 1;
                j += 1;
            } else if self.indices[i] < other.indices[j] {
                i += 1;
            } else {
                j += 1;
            }
        }

        result
    }
}

impl fmt::Display for SparseVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SparseVector(dim={}, nnz={})", self.dim, self.nnz())
    }
}

/// Search result containing vector ID and distance
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchResult {
    /// Vector identifier
    pub id: VectorId,
    /// Distance to query vector
    pub distance: f32,
}

impl SearchResult {
    /// Create a new search result
    pub fn new(id: VectorId, distance: f32) -> Self {
        Self { id, distance }
    }
}

impl Eq for SearchResult {}

impl PartialOrd for SearchResult {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SearchResult {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse ordering for max-heap (we want smallest distances first)
        other
            .distance
            .partial_cmp(&self.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dense_vector_basic() {
        let v = DenseVector::new(vec![1.0, 2.0, 3.0]);
        assert_eq!(v.dim(), 3);
        assert_eq!(v.get(0), Some(1.0));
        assert_eq!(v.get(2), Some(3.0));
        assert_eq!(v.get(5), None);
    }

    #[test]
    fn test_dense_vector_zeros() {
        let v = DenseVector::zeros(5);
        assert_eq!(v.dim(), 5);
        assert!(v.as_slice().iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_dense_vector_norm() {
        let v = DenseVector::new(vec![3.0, 4.0]);
        assert!((v.norm() - 5.0).abs() < 1e-6);
    }

    #[test]
    fn test_dense_vector_normalize() {
        let mut v = DenseVector::new(vec![3.0, 4.0]);
        v.normalize();
        assert!((v.norm() - 1.0).abs() < 1e-6);
        assert!((v.get(0).unwrap() - 0.6).abs() < 1e-6);
        assert!((v.get(1).unwrap() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_dense_vector_dot() {
        let v1 = DenseVector::new(vec![1.0, 2.0, 3.0]);
        let v2 = DenseVector::new(vec![4.0, 5.0, 6.0]);
        assert!((v1.dot(&v2) - 32.0).abs() < 1e-6); // 1*4 + 2*5 + 3*6 = 32
    }

    #[test]
    fn test_dense_vector_add_sub() {
        let mut v1 = DenseVector::new(vec![1.0, 2.0, 3.0]);
        let v2 = DenseVector::new(vec![4.0, 5.0, 6.0]);

        v1.add(&v2);
        assert_eq!(v1.as_slice(), &[5.0, 7.0, 9.0]);

        v1.sub(&v2);
        assert_eq!(v1.as_slice(), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_dense_vector_scale() {
        let mut v = DenseVector::new(vec![1.0, 2.0, 3.0]);
        v.scale(2.0);
        assert_eq!(v.as_slice(), &[2.0, 4.0, 6.0]);
    }

    #[test]
    fn test_dense_vector_roundtrip() {
        let v = DenseVector::new(vec![1.5, -2.5, 3.5, 0.0]);
        let bytes = v.to_bytes();
        let recovered = DenseVector::from_bytes(&bytes).unwrap();
        assert_eq!(v, recovered);
    }

    #[test]
    fn test_sparse_vector_basic() {
        let v = SparseVector::new(10, vec![0, 5, 9], vec![1.0, 2.0, 3.0]);
        assert_eq!(v.dim(), 10);
        assert_eq!(v.nnz(), 3);
        assert_eq!(v.get(0), 1.0);
        assert_eq!(v.get(5), 2.0);
        assert_eq!(v.get(9), 3.0);
        assert_eq!(v.get(3), 0.0); // Not present
    }

    #[test]
    fn test_sparse_vector_from_dense() {
        let dense = DenseVector::new(vec![1.0, 0.0, 0.0, 2.0, 0.0]);
        let sparse = SparseVector::from_dense(&dense, 1e-6);

        assert_eq!(sparse.dim(), 5);
        assert_eq!(sparse.nnz(), 2);
        assert_eq!(sparse.indices(), &[0, 3]);
        assert_eq!(sparse.values(), &[1.0, 2.0]);
    }

    #[test]
    fn test_sparse_vector_to_dense() {
        let sparse = SparseVector::new(5, vec![1, 3], vec![2.0, 4.0]);
        let dense = sparse.to_dense();

        assert_eq!(dense.as_slice(), &[0.0, 2.0, 0.0, 4.0, 0.0]);
    }

    #[test]
    fn test_sparse_vector_roundtrip() {
        let v = SparseVector::new(100, vec![5, 20, 50, 99], vec![1.0, 2.0, 3.0, 4.0]);
        let bytes = v.to_bytes();
        let recovered = SparseVector::from_bytes(&bytes).unwrap();

        assert_eq!(v.dim(), recovered.dim());
        assert_eq!(v.indices(), recovered.indices());
        assert_eq!(v.values(), recovered.values());
    }

    #[test]
    fn test_sparse_dot_dense() {
        let sparse = SparseVector::new(5, vec![0, 2, 4], vec![1.0, 2.0, 3.0]);
        let dense = DenseVector::new(vec![1.0, 1.0, 1.0, 1.0, 1.0]);

        assert!((sparse.dot_dense(&dense) - 6.0).abs() < 1e-6);
    }

    #[test]
    fn test_sparse_dot_sparse() {
        let v1 = SparseVector::new(10, vec![0, 2, 5], vec![1.0, 2.0, 3.0]);
        let v2 = SparseVector::new(10, vec![2, 5, 7], vec![4.0, 5.0, 6.0]);

        // Only indices 2 and 5 overlap: 2*4 + 3*5 = 23
        assert!((v1.dot_sparse(&v2) - 23.0).abs() < 1e-6);
    }

    #[test]
    fn test_search_result_ordering() {
        use std::collections::BinaryHeap;

        let r1 = SearchResult::new(1, 0.5);
        let r2 = SearchResult::new(2, 0.3);
        let r3 = SearchResult::new(3, 0.7);

        // Test with BinaryHeap (max-heap with reversed Ord = min-heap behavior)
        let mut heap = BinaryHeap::new();
        heap.push(r1);
        heap.push(r2);
        heap.push(r3);

        // Pop order should be smallest distance first due to reverse Ord
        let first = heap.pop().unwrap();
        let second = heap.pop().unwrap();
        let third = heap.pop().unwrap();

        assert_eq!(first.id, 2); // 0.3 (smallest)
        assert_eq!(second.id, 1); // 0.5
        assert_eq!(third.id, 3); // 0.7 (largest)
    }

    #[test]
    fn test_dense_vector_display() {
        let v = DenseVector::new(vec![1.0, 2.0, 3.0]);
        let s = format!("{}", v);
        assert!(s.contains("1.0000"));
        assert!(s.contains("2.0000"));
    }
}
