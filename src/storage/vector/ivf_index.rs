//! IVF (Inverted File) Index for Approximate k-NN Search
//!
//! Implements IVF-Flat: clusters vectors using k-means, then searches
//! only the closest clusters (probes) for faster approximate results.
//!
//! Trade-off: n_probes controls accuracy vs speed
//! - More probes = better recall, slower search
//! - Fewer probes = worse recall, faster search

use super::dense::DenseVectorStorage;
use super::distance::{
    cosine_distance, dot_product, l2_squared_distance, manhattan_distance, Distance,
};
use super::types::{DenseVector, SearchResult, VectorId};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// IVF-Flat index for approximate k-NN search
///
/// Uses k-means clustering to partition the vector space.
/// Search probes only the closest clusters for speed.
pub struct IvfIndex {
    /// Vector dimension
    dim: usize,
    /// Distance metric
    distance: Distance,
    /// Number of clusters (centroids)
    n_clusters: usize,
    /// Cluster centroids
    centroids: Vec<DenseVector>,
    /// Inverted lists: cluster_id -> vector storage
    inverted_lists: Vec<DenseVectorStorage>,
    /// Total number of vectors
    total_vectors: usize,
    /// Whether the index has been trained
    is_trained: bool,
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
        // Reverse order for max-heap
        other
            .distance
            .partial_cmp(&self.distance)
            .unwrap_or(Ordering::Equal)
    }
}

/// Cluster assignment during k-means
struct ClusterAssignment {
    cluster_id: usize,
    distance: f32,
}

impl IvfIndex {
    /// Create a new IVF index
    ///
    /// # Arguments
    /// * `dim` - Vector dimension
    /// * `n_clusters` - Number of clusters (typically sqrt(n_vectors))
    /// * `distance` - Distance metric
    pub fn new(dim: usize, n_clusters: usize, distance: Distance) -> Self {
        let n_clusters = n_clusters.max(1);

        Self {
            dim,
            distance,
            n_clusters,
            centroids: Vec::with_capacity(n_clusters),
            inverted_lists: Vec::new(),
            total_vectors: 0,
            is_trained: false,
        }
    }

    /// Get vector dimension
    #[inline]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Get number of indexed vectors
    #[inline]
    pub fn len(&self) -> usize {
        self.total_vectors
    }

    /// Check if index is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.total_vectors == 0
    }

    /// Check if index is trained
    #[inline]
    pub fn is_trained(&self) -> bool {
        self.is_trained
    }

    /// Get number of clusters
    #[inline]
    pub fn n_clusters(&self) -> usize {
        self.n_clusters
    }

    /// Train the index using k-means clustering
    ///
    /// # Arguments
    /// * `training_vectors` - Vectors to use for clustering
    /// * `max_iterations` - Maximum k-means iterations
    pub fn train(&mut self, training_vectors: &[DenseVector], max_iterations: usize) -> bool {
        if training_vectors.is_empty() {
            return false;
        }

        // Adjust n_clusters if we have fewer training vectors
        let actual_clusters = self.n_clusters.min(training_vectors.len());

        // Initialize centroids using k-means++ style initialization
        self.centroids = self.initialize_centroids(training_vectors, actual_clusters);

        // Run k-means iterations
        for _ in 0..max_iterations {
            // Assign each vector to nearest centroid
            let assignments = self.assign_to_clusters(training_vectors);

            // Update centroids
            let changed = self.update_centroids(training_vectors, &assignments);

            if !changed {
                break; // Converged
            }
        }

        // Initialize inverted lists
        self.inverted_lists.clear();
        for _ in 0..self.centroids.len() {
            self.inverted_lists.push(DenseVectorStorage::new(self.dim));
        }

        self.n_clusters = self.centroids.len();
        self.is_trained = true;
        true
    }

    /// Initialize centroids using k-means++ style
    fn initialize_centroids(&self, vectors: &[DenseVector], k: usize) -> Vec<DenseVector> {
        if vectors.is_empty() || k == 0 {
            return Vec::new();
        }

        let mut centroids = Vec::with_capacity(k);

        // First centroid: random (use first vector for determinism)
        centroids.push(vectors[0].clone());

        // Remaining centroids: pick vectors far from existing centroids
        for _ in 1..k {
            let mut max_min_dist = 0.0f32;
            let mut best_idx = 0;

            for (idx, vec) in vectors.iter().enumerate() {
                // Find minimum distance to any existing centroid
                let min_dist = centroids
                    .iter()
                    .map(|c| self.compute_distance(vec.as_slice(), c.as_slice()))
                    .fold(f32::MAX, |a, b| a.min(b));

                if min_dist > max_min_dist {
                    max_min_dist = min_dist;
                    best_idx = idx;
                }
            }

            centroids.push(vectors[best_idx].clone());
        }

        centroids
    }

    /// Assign each vector to nearest centroid
    fn assign_to_clusters(&self, vectors: &[DenseVector]) -> Vec<ClusterAssignment> {
        vectors
            .iter()
            .map(|vec| {
                let mut best_cluster = 0;
                let mut best_dist = f32::MAX;

                for (i, centroid) in self.centroids.iter().enumerate() {
                    let dist = self.compute_distance(vec.as_slice(), centroid.as_slice());
                    if dist < best_dist {
                        best_dist = dist;
                        best_cluster = i;
                    }
                }

                ClusterAssignment {
                    cluster_id: best_cluster,
                    distance: best_dist,
                }
            })
            .collect()
    }

    /// Update centroids based on assignments
    fn update_centroids(
        &mut self,
        vectors: &[DenseVector],
        assignments: &[ClusterAssignment],
    ) -> bool {
        let mut new_centroids: Vec<Vec<f32>> = vec![vec![0.0; self.dim]; self.centroids.len()];
        let mut counts = vec![0usize; self.centroids.len()];

        // Sum all vectors in each cluster
        for (vec, assignment) in vectors.iter().zip(assignments.iter()) {
            let cid = assignment.cluster_id;
            counts[cid] += 1;
            for (i, &val) in vec.as_slice().iter().enumerate() {
                new_centroids[cid][i] += val;
            }
        }

        // Compute mean
        let mut changed = false;
        for (i, centroid_data) in new_centroids.iter_mut().enumerate() {
            if counts[i] > 0 {
                for val in centroid_data.iter_mut() {
                    *val /= counts[i] as f32;
                }

                // Check if changed significantly
                let old = &self.centroids[i];
                let dist = l2_squared_distance(old.as_slice(), centroid_data);
                if dist > 1e-6 {
                    changed = true;
                }

                self.centroids[i] = DenseVector::new(centroid_data.clone());
            }
        }

        changed
    }

    /// Add a vector to the index
    ///
    /// Index must be trained first.
    pub fn add(&mut self, id: VectorId, vector: DenseVector) -> bool {
        if !self.is_trained || vector.dim() != self.dim {
            return false;
        }

        // Find nearest cluster
        let cluster_id = self.find_nearest_cluster(&vector);

        // Add to inverted list
        if self.inverted_lists[cluster_id].add(id, &vector) {
            self.total_vectors += 1;
            true
        } else {
            false
        }
    }

    /// Add multiple vectors in batch
    pub fn add_batch(&mut self, vectors: &[(VectorId, DenseVector)]) -> usize {
        let mut added = 0;
        for (id, vec) in vectors {
            if self.add(*id, vec.clone()) {
                added += 1;
            }
        }
        added
    }

    /// Find the nearest cluster for a vector
    fn find_nearest_cluster(&self, vector: &DenseVector) -> usize {
        let mut best_cluster = 0;
        let mut best_dist = f32::MAX;

        for (i, centroid) in self.centroids.iter().enumerate() {
            let dist = self.compute_distance(vector.as_slice(), centroid.as_slice());
            if dist < best_dist {
                best_dist = dist;
                best_cluster = i;
            }
        }

        best_cluster
    }

    /// Find the n closest clusters for a vector
    fn find_closest_clusters(&self, vector: &DenseVector, n_probes: usize) -> Vec<usize> {
        let mut distances: Vec<(usize, f32)> = self
            .centroids
            .iter()
            .enumerate()
            .map(|(i, c)| (i, self.compute_distance(vector.as_slice(), c.as_slice())))
            .collect();

        distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));

        distances
            .into_iter()
            .take(n_probes.min(self.n_clusters))
            .map(|(i, _)| i)
            .collect()
    }

    /// Search for k nearest neighbors
    ///
    /// # Arguments
    /// * `query` - Query vector
    /// * `k` - Number of neighbors to return
    /// * `n_probes` - Number of clusters to search (more = better recall)
    pub fn search(&self, query: &DenseVector, k: usize, n_probes: usize) -> Vec<SearchResult> {
        if !self.is_trained || k == 0 || self.is_empty() {
            return Vec::new();
        }

        let query_slice = query.as_slice();

        // Find closest clusters to probe
        let clusters_to_search = self.find_closest_clusters(query, n_probes);

        // Search within selected clusters
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(k + 1);

        for cluster_id in clusters_to_search {
            let storage = &self.inverted_lists[cluster_id];

            for (id, vec_slice) in storage.iter() {
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
        }

        // Convert to sorted results
        let mut results: Vec<SearchResult> = heap
            .into_iter()
            .map(|e| SearchResult::new(e.id, e.distance))
            .collect();

        results.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });

        results
    }

    /// Batch search for multiple queries
    pub fn search_batch(
        &self,
        queries: &[DenseVector],
        k: usize,
        n_probes: usize,
    ) -> Vec<Vec<SearchResult>> {
        queries
            .iter()
            .map(|q| self.search(q, k, n_probes))
            .collect()
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

    /// Get cluster sizes
    pub fn cluster_sizes(&self) -> Vec<usize> {
        self.inverted_lists.iter().map(|s| s.len()).collect()
    }

    /// Get centroid for a cluster
    pub fn centroid(&self, cluster_id: usize) -> Option<&DenseVector> {
        self.centroids.get(cluster_id)
    }

    /// Clear all vectors (keeps centroids)
    pub fn clear_vectors(&mut self) {
        for storage in &mut self.inverted_lists {
            storage.clear();
        }
        self.total_vectors = 0;
    }

    /// Clear everything including centroids
    pub fn clear(&mut self) {
        self.centroids.clear();
        self.inverted_lists.clear();
        self.total_vectors = 0;
        self.is_trained = false;
    }

    /// Get memory usage in bytes
    pub fn memory_usage(&self) -> usize {
        let centroid_size = self.centroids.len() * self.dim * 4;
        let lists_size: usize = self.inverted_lists.iter().map(|s| s.memory_usage()).sum();
        centroid_size + lists_size
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Header
        bytes.extend_from_slice(&(self.dim as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.n_clusters as u32).to_le_bytes());
        bytes.push(self.distance as u8);
        bytes.push(if self.is_trained { 1 } else { 0 });
        bytes.extend_from_slice(&(self.total_vectors as u64).to_le_bytes());

        // Centroids
        bytes.extend_from_slice(&(self.centroids.len() as u32).to_le_bytes());
        for centroid in &self.centroids {
            for &val in centroid.as_slice() {
                bytes.extend_from_slice(&val.to_le_bytes());
            }
        }

        // Inverted lists
        bytes.extend_from_slice(&(self.inverted_lists.len() as u32).to_le_bytes());
        for storage in &self.inverted_lists {
            let storage_bytes = storage.to_bytes();
            bytes.extend_from_slice(&(storage_bytes.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&storage_bytes);
        }

        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 18 {
            return None;
        }

        let mut offset = 0;

        // Header
        let dim = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        offset += 4;

        let n_clusters = u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        offset += 4;

        let distance = match bytes[offset] {
            0 => Distance::L2,
            1 => Distance::L2Squared,
            2 => Distance::Cosine,
            3 => Distance::DotProduct,
            4 => Distance::Manhattan,
            _ => return None,
        };
        offset += 1;

        let is_trained = bytes[offset] == 1;
        offset += 1;

        let total_vectors = u64::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        offset += 8;

        // Centroids
        if bytes.len() < offset + 4 {
            return None;
        }
        let centroid_count = u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        offset += 4;

        let mut centroids = Vec::with_capacity(centroid_count);
        for _ in 0..centroid_count {
            let mut data = Vec::with_capacity(dim);
            for _ in 0..dim {
                if bytes.len() < offset + 4 {
                    return None;
                }
                let val = f32::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ]);
                data.push(val);
                offset += 4;
            }
            centroids.push(DenseVector::new(data));
        }

        // Inverted lists
        if bytes.len() < offset + 4 {
            return None;
        }
        let list_count = u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        offset += 4;

        let mut inverted_lists = Vec::with_capacity(list_count);
        for _ in 0..list_count {
            if bytes.len() < offset + 4 {
                return None;
            }
            let storage_len = u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]) as usize;
            offset += 4;

            if bytes.len() < offset + storage_len {
                return None;
            }
            let storage = DenseVectorStorage::from_bytes(&bytes[offset..offset + storage_len])?;
            inverted_lists.push(storage);
            offset += storage_len;
        }

        Some(Self {
            dim,
            distance,
            n_clusters,
            centroids,
            inverted_lists,
            total_vectors,
            is_trained,
        })
    }
}

impl Clone for IvfIndex {
    fn clone(&self) -> Self {
        Self {
            dim: self.dim,
            distance: self.distance,
            n_clusters: self.n_clusters,
            centroids: self.centroids.clone(),
            inverted_lists: self.inverted_lists.clone(),
            total_vectors: self.total_vectors,
            is_trained: self.is_trained,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_training_vectors(n: usize, dim: usize) -> Vec<DenseVector> {
        // Create vectors in 4 clusters around corners of unit hypercube
        (0..n)
            .map(|i| {
                let cluster = i % 4;
                let mut data = vec![0.0f32; dim];
                // Add cluster-specific offset
                for j in 0..dim {
                    data[j] = if (cluster >> (j % 2)) & 1 == 1 {
                        0.8 + (i as f32 * 0.001)
                    } else {
                        0.2 + (i as f32 * 0.001)
                    };
                }
                DenseVector::new(data)
            })
            .collect()
    }

    #[test]
    fn test_ivf_new() {
        let index = IvfIndex::new(128, 10, Distance::L2);

        assert_eq!(index.dim(), 128);
        assert_eq!(index.n_clusters(), 10);
        assert!(!index.is_trained());
        assert!(index.is_empty());
    }

    #[test]
    fn test_ivf_train() {
        let mut index = IvfIndex::new(4, 4, Distance::L2);

        let training = create_training_vectors(100, 4);
        assert!(index.train(&training, 10));
        assert!(index.is_trained());
        assert_eq!(index.n_clusters(), 4);
    }

    #[test]
    fn test_ivf_add() {
        let mut index = IvfIndex::new(3, 2, Distance::L2);

        let training = vec![
            DenseVector::new(vec![0.0, 0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0, 1.0]),
        ];
        index.train(&training, 10);

        assert!(index.add(1, DenseVector::new(vec![0.1, 0.1, 0.1])));
        assert!(index.add(2, DenseVector::new(vec![0.9, 0.9, 0.9])));

        assert_eq!(index.len(), 2);
    }

    #[test]
    fn test_ivf_add_before_train() {
        let mut index = IvfIndex::new(3, 2, Distance::L2);

        // Should fail - not trained
        assert!(!index.add(1, DenseVector::new(vec![0.1, 0.1, 0.1])));
    }

    #[test]
    fn test_ivf_search() {
        let mut index = IvfIndex::new(3, 2, Distance::L2);

        let training = vec![
            DenseVector::new(vec![0.0, 0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0, 1.0]),
        ];
        index.train(&training, 10);

        // Add vectors near (0,0,0)
        index.add(1, DenseVector::new(vec![0.1, 0.0, 0.0]));
        index.add(2, DenseVector::new(vec![0.0, 0.1, 0.0]));

        // Add vectors near (1,1,1)
        index.add(3, DenseVector::new(vec![0.9, 1.0, 1.0]));
        index.add(4, DenseVector::new(vec![1.0, 0.9, 1.0]));

        // Search near (0,0,0) with 1 probe (should find cluster 0)
        let query = DenseVector::new(vec![0.05, 0.05, 0.0]);
        let results = index.search(&query, 2, 1);

        assert!(!results.is_empty());
        // Should find vectors 1 and 2 (in cluster near origin)
        let found_ids: Vec<_> = results.iter().map(|r| r.id).collect();
        assert!(found_ids.contains(&1) || found_ids.contains(&2));
    }

    #[test]
    fn test_ivf_search_all_probes() {
        let mut index = IvfIndex::new(3, 2, Distance::L2);

        let training = vec![
            DenseVector::new(vec![0.0, 0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0, 1.0]),
        ];
        index.train(&training, 10);

        index.add(1, DenseVector::new(vec![0.0, 0.0, 0.0]));
        index.add(2, DenseVector::new(vec![1.0, 1.0, 1.0]));

        // Search with all probes
        let query = DenseVector::new(vec![0.5, 0.5, 0.5]);
        let results = index.search(&query, 2, 2); // Probe all clusters

        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_ivf_cluster_sizes() {
        let mut index = IvfIndex::new(2, 2, Distance::L2);

        let training = vec![
            DenseVector::new(vec![0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0]),
        ];
        index.train(&training, 10);

        // Add to cluster 0
        index.add(1, DenseVector::new(vec![0.0, 0.0]));
        index.add(2, DenseVector::new(vec![0.1, 0.1]));

        // Add to cluster 1
        index.add(3, DenseVector::new(vec![1.0, 1.0]));

        let sizes = index.cluster_sizes();
        assert_eq!(sizes.len(), 2);
        assert_eq!(sizes.iter().sum::<usize>(), 3);
    }

    #[test]
    fn test_ivf_roundtrip() {
        let mut index = IvfIndex::new(3, 2, Distance::Cosine);

        let training = vec![
            DenseVector::new(vec![1.0, 0.0, 0.0]),
            DenseVector::new(vec![0.0, 1.0, 0.0]),
        ];
        index.train(&training, 10);

        index.add(1, DenseVector::new(vec![1.0, 0.1, 0.0]));
        index.add(2, DenseVector::new(vec![0.1, 1.0, 0.0]));

        let bytes = index.to_bytes();
        let recovered = IvfIndex::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.dim(), index.dim());
        assert_eq!(recovered.n_clusters(), index.n_clusters());
        assert_eq!(recovered.len(), index.len());
        assert!(recovered.is_trained());

        // Verify search works
        let query = DenseVector::new(vec![1.0, 0.0, 0.0]);
        let results = recovered.search(&query, 1, 2);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_ivf_clear_vectors() {
        let mut index = IvfIndex::new(3, 2, Distance::L2);

        let training = vec![
            DenseVector::new(vec![0.0, 0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0, 1.0]),
        ];
        index.train(&training, 10);

        index.add(1, DenseVector::new(vec![0.0, 0.0, 0.0]));
        index.add(2, DenseVector::new(vec![1.0, 1.0, 1.0]));

        index.clear_vectors();

        assert!(index.is_empty());
        assert!(index.is_trained()); // Still trained
        assert_eq!(index.n_clusters(), 2);
    }

    #[test]
    fn test_ivf_clear_all() {
        let mut index = IvfIndex::new(3, 2, Distance::L2);

        let training = vec![
            DenseVector::new(vec![0.0, 0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0, 1.0]),
        ];
        index.train(&training, 10);

        index.add(1, DenseVector::new(vec![0.0, 0.0, 0.0]));

        index.clear();

        assert!(index.is_empty());
        assert!(!index.is_trained());
    }

    #[test]
    fn test_ivf_batch_search() {
        let mut index = IvfIndex::new(2, 2, Distance::L2);

        let training = vec![
            DenseVector::new(vec![0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0]),
        ];
        index.train(&training, 10);

        index.add(1, DenseVector::new(vec![0.0, 0.0]));
        index.add(2, DenseVector::new(vec![1.0, 1.0]));

        let queries = vec![
            DenseVector::new(vec![0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0]),
        ];

        let all_results = index.search_batch(&queries, 1, 2);

        assert_eq!(all_results.len(), 2);
        assert_eq!(all_results[0][0].id, 1);
        assert_eq!(all_results[1][0].id, 2);
    }

    #[test]
    fn test_ivf_few_training_vectors() {
        let mut index = IvfIndex::new(3, 10, Distance::L2); // Request more clusters than vectors

        let training = vec![
            DenseVector::new(vec![0.0, 0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0, 1.0]),
        ];

        assert!(index.train(&training, 10));

        // Should have at most 2 clusters (number of training vectors)
        assert!(index.n_clusters() <= 2);
    }

    #[test]
    fn test_ivf_centroid_access() {
        let mut index = IvfIndex::new(2, 2, Distance::L2);

        let training = vec![
            DenseVector::new(vec![0.0, 0.0]),
            DenseVector::new(vec![1.0, 1.0]),
        ];
        index.train(&training, 10);

        assert!(index.centroid(0).is_some());
        assert!(index.centroid(1).is_some());
        assert!(index.centroid(99).is_none());
    }
}
