//! Flat Index for Exact k-NN Search
//!
//! Provides brute-force exact nearest neighbor search.
//! Best for small to medium datasets (< 100K vectors).

use super::dense::DenseVectorStorage;
use super::distance::{
    cosine_distance, dot_product, l2_squared_distance, manhattan_distance, Distance,
};
use super::types::{DenseVector, SearchResult, VectorId};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Flat index for exact k-NN search
///
/// Performs brute-force search over all vectors.
/// Guarantees exact results but O(n) query time.
pub struct FlatIndex {
    /// Vector storage
    storage: DenseVectorStorage,
    /// Distance metric
    distance: Distance,
}

/// Entry in the search priority queue
#[derive(Clone)]
struct HeapEntry {
    id: VectorId,
    distance: f32,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order for max-heap (we want smallest distances)
        other
            .distance
            .partial_cmp(&self.distance)
            .unwrap_or(Ordering::Equal)
    }
}

impl FlatIndex {
    /// Create a new flat index with given dimension and distance metric
    pub fn new(dim: usize, distance: Distance) -> Self {
        Self {
            storage: DenseVectorStorage::new(dim),
            distance,
        }
    }

    /// Create with pre-allocated capacity
    pub fn with_capacity(dim: usize, distance: Distance, capacity: usize) -> Self {
        Self {
            storage: DenseVectorStorage::with_capacity(dim, capacity),
            distance,
        }
    }

    /// Get vector dimension
    #[inline]
    pub fn dim(&self) -> usize {
        self.storage.dim()
    }

    /// Get number of indexed vectors
    #[inline]
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    /// Check if index is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    /// Get distance metric
    #[inline]
    pub fn distance_metric(&self) -> Distance {
        self.distance
    }

    /// Add a vector to the index
    ///
    /// Returns true if added, false if ID exists or dimension mismatch.
    pub fn add(&mut self, id: VectorId, vector: DenseVector) -> bool {
        self.storage.add(id, &vector)
    }

    /// Add multiple vectors in batch
    pub fn add_batch(&mut self, vectors: &[(VectorId, DenseVector)]) -> usize {
        self.storage.add_batch(vectors)
    }

    /// Get vector by ID
    pub fn get(&self, id: VectorId) -> Option<DenseVector> {
        self.storage.get(id)
    }

    /// Check if ID exists
    #[inline]
    pub fn contains(&self, id: VectorId) -> bool {
        self.storage.contains(id)
    }

    /// Remove vector by ID
    pub fn remove(&mut self, id: VectorId) -> bool {
        self.storage.remove(id)
    }

    /// Search for k nearest neighbors
    ///
    /// Returns up to k results sorted by distance (closest first).
    pub fn search(&self, query: &DenseVector, k: usize) -> Vec<SearchResult> {
        if k == 0 || self.is_empty() {
            return Vec::new();
        }

        let query_slice = query.as_slice();
        let _dim = self.storage.dim();

        // Use max-heap to maintain k smallest distances
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(k + 1);

        // Compute distance to all vectors
        for (id, vec_slice) in self.storage.iter() {
            let dist = self.compute_distance(query_slice, vec_slice);

            if heap.len() < k {
                heap.push(HeapEntry { id, distance: dist });
            } else if let Some(top) = heap.peek() {
                if dist < top.distance {
                    heap.pop();
                    heap.push(HeapEntry { id, distance: dist });
                }
            }
        }

        // Convert heap to sorted results
        let mut results: Vec<SearchResult> = heap
            .into_iter()
            .map(|e| SearchResult::new(e.id, e.distance))
            .collect();

        // Sort by distance (ascending)
        results.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });

        results
    }

    /// Search with a distance threshold
    ///
    /// Returns all vectors within the given distance threshold.
    pub fn search_range(&self, query: &DenseVector, threshold: f32) -> Vec<SearchResult> {
        if self.is_empty() {
            return Vec::new();
        }

        let query_slice = query.as_slice();
        let mut results = Vec::new();

        for (id, vec_slice) in self.storage.iter() {
            let dist = self.compute_distance(query_slice, vec_slice);
            if dist <= threshold {
                results.push(SearchResult::new(id, dist));
            }
        }

        // Sort by distance
        results.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });

        results
    }

    /// Batch search for multiple queries
    pub fn search_batch(&self, queries: &[DenseVector], k: usize) -> Vec<Vec<SearchResult>> {
        queries.iter().map(|q| self.search(q, k)).collect()
    }

    /// Compute distance between two vectors
    #[inline]
    fn compute_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.distance {
            Distance::L2 => l2_squared_distance(a, b).sqrt(),
            Distance::L2Squared => l2_squared_distance(a, b),
            Distance::Cosine => cosine_distance(a, b),
            Distance::DotProduct => -dot_product(a, b),
            Distance::Manhattan => manhattan_distance(a, b),
        }
    }

    /// Get underlying storage (for serialization)
    pub fn storage(&self) -> &DenseVectorStorage {
        &self.storage
    }

    /// Get mutable underlying storage
    pub fn storage_mut(&mut self) -> &mut DenseVectorStorage {
        &mut self.storage
    }

    /// Clear all vectors
    pub fn clear(&mut self) {
        self.storage.clear();
    }

    /// Get memory usage in bytes
    pub fn memory_usage(&self) -> usize {
        self.storage.memory_usage()
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Header: distance metric (1 byte)
        bytes.push(self.distance as u8);

        // Storage data
        bytes.extend_from_slice(&self.storage.to_bytes());

        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }

        let distance = match bytes[0] {
            0 => Distance::L2,
            1 => Distance::L2Squared,
            2 => Distance::Cosine,
            3 => Distance::DotProduct,
            4 => Distance::Manhattan,
            _ => return None,
        };

        let storage = DenseVectorStorage::from_bytes(&bytes[1..])?;

        Some(Self { storage, distance })
    }
}

impl Clone for FlatIndex {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            distance: self.distance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_vectors() -> Vec<(VectorId, DenseVector)> {
        vec![
            (1, DenseVector::new(vec![1.0, 0.0, 0.0])),
            (2, DenseVector::new(vec![0.0, 1.0, 0.0])),
            (3, DenseVector::new(vec![0.0, 0.0, 1.0])),
            (4, DenseVector::new(vec![1.0, 1.0, 0.0])),
            (5, DenseVector::new(vec![1.0, 1.0, 1.0])),
        ]
    }

    #[test]
    fn test_flat_index_basic() {
        let mut index = FlatIndex::new(3, Distance::L2);

        for (id, vec) in create_test_vectors() {
            assert!(index.add(id, vec));
        }

        assert_eq!(index.len(), 5);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_flat_index_search_l2() {
        let mut index = FlatIndex::new(3, Distance::L2);

        for (id, vec) in create_test_vectors() {
            index.add(id, vec);
        }

        // Query close to vector 1 (1, 0, 0)
        let query = DenseVector::new(vec![0.9, 0.1, 0.0]);
        let results = index.search(&query, 3);

        assert_eq!(results.len(), 3);
        // Vector 1 should be closest
        assert_eq!(results[0].id, 1);
    }

    #[test]
    fn test_flat_index_search_cosine() {
        let mut index = FlatIndex::new(3, Distance::Cosine);

        index.add(1, DenseVector::new(vec![1.0, 0.0, 0.0]));
        index.add(2, DenseVector::new(vec![0.0, 1.0, 0.0]));
        index.add(3, DenseVector::new(vec![1.0, 1.0, 0.0])); // 45 degree angle from both

        // Query along x-axis
        let query = DenseVector::new(vec![2.0, 0.0, 0.0]); // Same direction as vector 1
        let results = index.search(&query, 2);

        assert_eq!(results.len(), 2);
        // Vector 1 should be closest (same direction)
        assert_eq!(results[0].id, 1);
        assert!(results[0].distance < 0.01); // Near zero distance
    }

    #[test]
    fn test_flat_index_search_dot_product() {
        let mut index = FlatIndex::new(3, Distance::DotProduct);

        index.add(1, DenseVector::new(vec![1.0, 0.0, 0.0]));
        index.add(2, DenseVector::new(vec![2.0, 0.0, 0.0])); // Larger magnitude
        index.add(3, DenseVector::new(vec![0.0, 1.0, 0.0]));

        let query = DenseVector::new(vec![1.0, 0.0, 0.0]);
        let results = index.search(&query, 2);

        // Vector 2 has higher dot product (larger magnitude in same direction)
        assert_eq!(results[0].id, 2);
    }

    #[test]
    fn test_flat_index_search_range() {
        let mut index = FlatIndex::new(3, Distance::L2);

        for (id, vec) in create_test_vectors() {
            index.add(id, vec);
        }

        let query = DenseVector::new(vec![1.0, 0.0, 0.0]);
        let results = index.search_range(&query, 0.5);

        // Only vector 1 should be within distance 0.5
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 1);
    }

    #[test]
    fn test_flat_index_search_empty() {
        let index = FlatIndex::new(3, Distance::L2);

        let query = DenseVector::new(vec![1.0, 0.0, 0.0]);
        let results = index.search(&query, 5);

        assert!(results.is_empty());
    }

    #[test]
    fn test_flat_index_search_k_zero() {
        let mut index = FlatIndex::new(3, Distance::L2);
        index.add(1, DenseVector::new(vec![1.0, 0.0, 0.0]));

        let query = DenseVector::new(vec![1.0, 0.0, 0.0]);
        let results = index.search(&query, 0);

        assert!(results.is_empty());
    }

    #[test]
    fn test_flat_index_duplicate_id() {
        let mut index = FlatIndex::new(3, Distance::L2);

        assert!(index.add(1, DenseVector::new(vec![1.0, 0.0, 0.0])));
        assert!(!index.add(1, DenseVector::new(vec![0.0, 1.0, 0.0]))); // Duplicate

        assert_eq!(index.len(), 1);
    }

    #[test]
    fn test_flat_index_get() {
        let mut index = FlatIndex::new(3, Distance::L2);

        index.add(42, DenseVector::new(vec![1.0, 2.0, 3.0]));

        let vec = index.get(42).unwrap();
        assert_eq!(vec.as_slice(), &[1.0, 2.0, 3.0]);

        assert!(index.get(999).is_none());
    }

    #[test]
    fn test_flat_index_remove() {
        let mut index = FlatIndex::new(3, Distance::L2);

        for (id, vec) in create_test_vectors() {
            index.add(id, vec);
        }

        assert!(index.remove(3));
        assert_eq!(index.len(), 4);
        assert!(!index.contains(3));

        // Search should not return removed vector
        let query = DenseVector::new(vec![0.0, 0.0, 1.0]);
        let results = index.search(&query, 5);
        assert!(results.iter().all(|r| r.id != 3));
    }

    #[test]
    fn test_flat_index_batch_search() {
        let mut index = FlatIndex::new(3, Distance::L2);

        for (id, vec) in create_test_vectors() {
            index.add(id, vec);
        }

        let queries = vec![
            DenseVector::new(vec![1.0, 0.0, 0.0]),
            DenseVector::new(vec![0.0, 1.0, 0.0]),
        ];

        let all_results = index.search_batch(&queries, 1);

        assert_eq!(all_results.len(), 2);
        assert_eq!(all_results[0][0].id, 1); // Closest to (1,0,0)
        assert_eq!(all_results[1][0].id, 2); // Closest to (0,1,0)
    }

    #[test]
    fn test_flat_index_roundtrip() {
        let mut index = FlatIndex::new(3, Distance::Cosine);

        for (id, vec) in create_test_vectors() {
            index.add(id, vec);
        }

        let bytes = index.to_bytes();
        let recovered = FlatIndex::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.dim(), index.dim());
        assert_eq!(recovered.len(), index.len());
        assert_eq!(recovered.distance_metric(), index.distance_metric());

        // Verify search still works
        let query = DenseVector::new(vec![1.0, 0.0, 0.0]);
        let original_results = index.search(&query, 3);
        let recovered_results = recovered.search(&query, 3);

        assert_eq!(original_results.len(), recovered_results.len());
        for (orig, rec) in original_results.iter().zip(recovered_results.iter()) {
            assert_eq!(orig.id, rec.id);
        }
    }

    #[test]
    fn test_flat_index_clear() {
        let mut index = FlatIndex::new(3, Distance::L2);

        for (id, vec) in create_test_vectors() {
            index.add(id, vec);
        }

        index.clear();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_flat_index_large_k() {
        let mut index = FlatIndex::new(3, Distance::L2);

        // Add only 3 vectors
        index.add(1, DenseVector::new(vec![1.0, 0.0, 0.0]));
        index.add(2, DenseVector::new(vec![0.0, 1.0, 0.0]));
        index.add(3, DenseVector::new(vec![0.0, 0.0, 1.0]));

        // Request 10 neighbors
        let query = DenseVector::new(vec![1.0, 1.0, 1.0]);
        let results = index.search(&query, 10);

        // Should return only 3 (all available)
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_flat_index_search_order() {
        let mut index = FlatIndex::new(2, Distance::L2);

        // Add vectors at varying distances from origin
        index.add(1, DenseVector::new(vec![1.0, 0.0])); // dist = 1.0
        index.add(2, DenseVector::new(vec![2.0, 0.0])); // dist = 2.0
        index.add(3, DenseVector::new(vec![3.0, 0.0])); // dist = 3.0
        index.add(4, DenseVector::new(vec![0.5, 0.0])); // dist = 0.5

        let query = DenseVector::new(vec![0.0, 0.0]);
        let results = index.search(&query, 4);

        // Should be sorted by distance
        assert_eq!(results[0].id, 4); // 0.5
        assert_eq!(results[1].id, 1); // 1.0
        assert_eq!(results[2].id, 2); // 2.0
        assert_eq!(results[3].id, 3); // 3.0
    }
}
