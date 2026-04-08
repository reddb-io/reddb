//! IVF (Inverted File Index) for Vector Search
//!
//! Clustering-based approximate nearest neighbor search.
//! Partitions vectors into clusters (Voronoi cells) and only searches
//! the most relevant clusters at query time.
//!
//! # Design
//!
//! - k-means clustering to build centroids
//! - Each vector assigned to its nearest centroid
//! - At query time, probe only `nprobe` nearest clusters
//! - Trade-off: more probes = better recall, slower search
//!
//! # Example
//!
//! ```ignore
//! let mut ivf = IvfIndex::new(IvfConfig {
//!     n_lists: 100,      // Number of clusters
//!     n_probes: 10,      // Clusters to search
//!     dimension: 384,
//! });
//!
//! // Train on sample vectors
//! ivf.train(&training_vectors);
//!
//! // Add vectors
//! ivf.add_batch(&vectors);
//!
//! // Search
//! let results = ivf.search(&query, 10);
//! ```

use std::collections::HashMap;

use super::distance::{cmp_f32, l2_squared_simd, DistanceResult};
use super::hnsw::NodeId;

/// IVF configuration
#[derive(Clone, Debug)]
pub struct IvfConfig {
    /// Number of clusters (Voronoi cells)
    pub n_lists: usize,
    /// Number of clusters to probe at query time
    pub n_probes: usize,
    /// Vector dimension
    pub dimension: usize,
    /// Maximum k-means iterations during training
    pub max_iterations: usize,
    /// Convergence threshold for k-means
    pub convergence_threshold: f32,
}

impl Default for IvfConfig {
    fn default() -> Self {
        Self {
            n_lists: 100,
            n_probes: 10,
            dimension: 128,
            max_iterations: 50,
            convergence_threshold: 1e-4,
        }
    }
}

impl IvfConfig {
    pub fn new(dimension: usize, n_lists: usize) -> Self {
        Self {
            n_lists,
            n_probes: (n_lists / 10).max(1),
            dimension,
            ..Default::default()
        }
    }

    pub fn with_probes(mut self, n_probes: usize) -> Self {
        self.n_probes = n_probes;
        self
    }
}

/// A cluster containing vectors
#[derive(Clone)]
struct IvfList {
    /// Centroid of this cluster
    centroid: Vec<f32>,
    /// Vector IDs in this cluster
    ids: Vec<NodeId>,
    /// Vectors in this cluster (stored for search)
    vectors: Vec<Vec<f32>>,
}

impl IvfList {
    fn new(centroid: Vec<f32>) -> Self {
        Self {
            centroid,
            ids: Vec::new(),
            vectors: Vec::new(),
        }
    }

    fn add(&mut self, id: NodeId, vector: Vec<f32>) {
        self.ids.push(id);
        self.vectors.push(vector);
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// IVF Index for approximate nearest neighbor search
pub struct IvfIndex {
    config: IvfConfig,
    /// Cluster lists
    lists: Vec<IvfList>,
    /// Mapping from vector ID to list index
    id_to_list: HashMap<NodeId, usize>,
    /// Whether the index has been trained
    trained: bool,
    /// Total vector count
    count: usize,
    /// Next auto-generated ID
    next_id: NodeId,
}

impl IvfIndex {
    /// Create a new IVF index (untrained)
    pub fn new(config: IvfConfig) -> Self {
        Self {
            config,
            lists: Vec::new(),
            id_to_list: HashMap::new(),
            trained: false,
            count: 0,
            next_id: 0,
        }
    }

    /// Create with default config for given dimension
    pub fn with_dimension(dimension: usize) -> Self {
        Self::new(IvfConfig::new(dimension, 100))
    }

    /// Train the index using k-means clustering
    pub fn train(&mut self, vectors: &[Vec<f32>]) {
        if vectors.is_empty() {
            return;
        }

        let n_lists = self.config.n_lists.min(vectors.len());

        // Initialize centroids using k-means++
        let centroids = self.kmeans_plusplus_init(vectors, n_lists);

        // Run k-means
        let final_centroids = self.kmeans(vectors, centroids);

        // Create lists
        self.lists = final_centroids.into_iter().map(IvfList::new).collect();

        self.trained = true;
    }

    /// K-means++ initialization for better centroid starting points
    fn kmeans_plusplus_init(&self, vectors: &[Vec<f32>], k: usize) -> Vec<Vec<f32>> {
        let mut centroids = Vec::with_capacity(k);

        if vectors.is_empty() || k == 0 {
            return centroids;
        }

        // First centroid: random (use middle for determinism)
        centroids.push(vectors[vectors.len() / 2].clone());

        // Subsequent centroids: weighted by distance to nearest existing centroid
        for _ in 1..k {
            let mut distances: Vec<f32> = vectors
                .iter()
                .map(|v| {
                    centroids
                        .iter()
                        .map(|c| l2_squared_simd(v, c))
                        .fold(f32::MAX, f32::min)
                })
                .collect();

            // Normalize to probabilities
            let total: f32 = distances.iter().sum();
            if total > 0.0 {
                for d in &mut distances {
                    *d /= total;
                }
            }

            // Select based on cumulative probability (deterministic: use max distance)
            let max_idx = distances
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| cmp_f32(**a, **b))
                .map(|(i, _)| i)
                .unwrap_or(0);

            centroids.push(vectors[max_idx].clone());
        }

        centroids
    }

    /// Run k-means clustering
    fn kmeans(&self, vectors: &[Vec<f32>], mut centroids: Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        let dim = self.config.dimension;
        let k = centroids.len();

        for _ in 0..self.config.max_iterations {
            // Assign vectors to nearest centroid
            let mut assignments: Vec<Vec<usize>> = vec![Vec::new(); k];
            for (i, vector) in vectors.iter().enumerate() {
                let nearest = self.find_nearest_centroid(vector, &centroids);
                assignments[nearest].push(i);
            }

            // Compute new centroids
            let mut new_centroids = Vec::with_capacity(k);
            let mut max_shift: f32 = 0.0;

            for (cluster_idx, indices) in assignments.iter().enumerate() {
                if indices.is_empty() {
                    // Keep old centroid if cluster is empty
                    new_centroids.push(centroids[cluster_idx].clone());
                    continue;
                }

                // Average of all vectors in cluster
                let mut new_centroid = vec![0.0f32; dim];
                for &idx in indices {
                    for (j, val) in vectors[idx].iter().enumerate() {
                        if j < dim {
                            new_centroid[j] += val;
                        }
                    }
                }
                for val in &mut new_centroid {
                    *val /= indices.len() as f32;
                }

                // Track centroid shift
                let shift = l2_squared_simd(&new_centroid, &centroids[cluster_idx]).sqrt();
                max_shift = max_shift.max(shift);

                new_centroids.push(new_centroid);
            }

            centroids = new_centroids;

            // Check convergence
            if max_shift < self.config.convergence_threshold {
                break;
            }
        }

        centroids
    }

    /// Find nearest centroid index
    fn find_nearest_centroid(&self, vector: &[f32], centroids: &[Vec<f32>]) -> usize {
        centroids
            .iter()
            .enumerate()
            .map(|(i, c)| (i, l2_squared_simd(vector, c)))
            .min_by(|(_, a), (_, b)| cmp_f32(*a, *b))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    /// Find k nearest centroids
    fn find_nearest_centroids(&self, vector: &[f32], k: usize) -> Vec<usize> {
        let mut distances: Vec<(usize, f32)> = self
            .lists
            .iter()
            .enumerate()
            .map(|(i, list)| (i, l2_squared_simd(vector, &list.centroid)))
            .collect();

        distances.sort_by(|(_, a), (_, b)| cmp_f32(*a, *b));
        distances.into_iter().take(k).map(|(i, _)| i).collect()
    }

    /// Add a single vector
    pub fn add(&mut self, vector: Vec<f32>) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        self.add_with_id(id, vector);
        id
    }

    /// Add a vector with specific ID
    pub fn add_with_id(&mut self, id: NodeId, vector: Vec<f32>) {
        if !self.trained || self.lists.is_empty() {
            // Auto-train with a single cluster if not trained
            if self.lists.is_empty() {
                self.lists.push(IvfList::new(vector.clone()));
                self.trained = true;
            }
        }

        let list_idx = self.find_nearest_centroid(
            &vector,
            &self
                .lists
                .iter()
                .map(|l| l.centroid.clone())
                .collect::<Vec<_>>(),
        );

        self.lists[list_idx].add(id, vector);
        self.id_to_list.insert(id, list_idx);
        self.count += 1;
    }

    /// Add multiple vectors
    pub fn add_batch(&mut self, vectors: Vec<Vec<f32>>) -> Vec<NodeId> {
        vectors.into_iter().map(|v| self.add(v)).collect()
    }

    /// Add multiple vectors with IDs
    pub fn add_batch_with_ids(&mut self, items: Vec<(NodeId, Vec<f32>)>) {
        for (id, vector) in items {
            self.add_with_id(id, vector);
        }
    }

    /// Remove a vector by ID
    pub fn remove(&mut self, id: NodeId) -> bool {
        if let Some(list_idx) = self.id_to_list.remove(&id) {
            let list = &mut self.lists[list_idx];
            if let Some(pos) = list.ids.iter().position(|&x| x == id) {
                list.ids.remove(pos);
                list.vectors.remove(pos);
                self.count = self.count.saturating_sub(1);
                return true;
            }
        }
        false
    }

    /// Search for k nearest neighbors
    pub fn search(&self, query: &[f32], k: usize) -> Vec<DistanceResult> {
        self.search_with_probes(query, k, self.config.n_probes)
    }

    /// Search with custom number of probes
    pub fn search_with_probes(
        &self,
        query: &[f32],
        k: usize,
        n_probes: usize,
    ) -> Vec<DistanceResult> {
        if self.lists.is_empty() {
            return Vec::new();
        }

        let probes = self.find_nearest_centroids(query, n_probes);

        // Collect candidates from probed clusters
        let mut candidates: Vec<DistanceResult> = Vec::new();
        for list_idx in probes {
            let list = &self.lists[list_idx];
            for (i, vector) in list.vectors.iter().enumerate() {
                let distance = l2_squared_simd(query, vector).sqrt();
                candidates.push(DistanceResult::new(list.ids[i], distance));
            }
        }

        // Sort and return top k
        candidates.sort_by(|a, b| cmp_f32(a.distance, b.distance));
        candidates.truncate(k);
        candidates
    }

    /// Get a vector by ID
    pub fn get(&self, id: NodeId) -> Option<&[f32]> {
        if let Some(&list_idx) = self.id_to_list.get(&id) {
            let list = &self.lists[list_idx];
            if let Some(pos) = list.ids.iter().position(|&x| x == id) {
                return Some(&list.vectors[pos]);
            }
        }
        None
    }

    /// Check if index contains an ID
    pub fn contains(&self, id: NodeId) -> bool {
        self.id_to_list.contains_key(&id)
    }

    /// Get total vector count
    pub fn len(&self) -> usize {
        self.count
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Get number of clusters
    pub fn n_lists(&self) -> usize {
        self.lists.len()
    }

    /// Get cluster statistics
    pub fn stats(&self) -> IvfStats {
        let sizes: Vec<usize> = self.lists.iter().map(|l| l.len()).collect();
        let non_empty = sizes.iter().filter(|&&s| s > 0).count();

        let avg = if non_empty > 0 {
            sizes.iter().sum::<usize>() as f64 / non_empty as f64
        } else {
            0.0
        };

        let max = sizes.iter().copied().max().unwrap_or(0);
        let min = sizes.iter().filter(|&&s| s > 0).copied().min().unwrap_or(0);

        IvfStats {
            total_vectors: self.count,
            n_lists: self.lists.len(),
            non_empty_lists: non_empty,
            avg_list_size: avg,
            max_list_size: max,
            min_list_size: min,
            dimension: self.config.dimension,
            trained: self.trained,
        }
    }

    /// Serialize the index to bytes for storage
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"IVF1");
        bytes.extend_from_slice(&(self.config.n_lists as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.config.n_probes as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.config.dimension as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.config.max_iterations as u32).to_le_bytes());
        bytes.extend_from_slice(&self.config.convergence_threshold.to_le_bytes());
        bytes.push(if self.trained { 1 } else { 0 });
        bytes.extend_from_slice(&(self.count as u64).to_le_bytes());
        bytes.extend_from_slice(&self.next_id.to_le_bytes());
        bytes.extend_from_slice(&(self.lists.len() as u32).to_le_bytes());

        for list in &self.lists {
            bytes.extend_from_slice(&(list.centroid.len() as u32).to_le_bytes());
            for value in &list.centroid {
                bytes.extend_from_slice(&value.to_le_bytes());
            }

            bytes.extend_from_slice(&(list.ids.len() as u32).to_le_bytes());
            for id in &list.ids {
                bytes.extend_from_slice(&id.to_le_bytes());
            }

            bytes.extend_from_slice(&(list.vectors.len() as u32).to_le_bytes());
            for vector in &list.vectors {
                bytes.extend_from_slice(&(vector.len() as u32).to_le_bytes());
                for value in vector {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
        }

        bytes
    }

    /// Deserialize an index from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 41 {
            return Err("data too short".to_string());
        }
        if &bytes[0..4] != b"IVF1" {
            return Err("invalid IVF magic".to_string());
        }

        let mut pos = 4usize;
        let read_u32 = |buf: &[u8], pos: &mut usize| -> Result<u32, String> {
            if *pos + 4 > buf.len() {
                return Err("truncated IVF payload".to_string());
            }
            let value = u32::from_le_bytes([
                buf[*pos],
                buf[*pos + 1],
                buf[*pos + 2],
                buf[*pos + 3],
            ]);
            *pos += 4;
            Ok(value)
        };
        let read_u64 = |buf: &[u8], pos: &mut usize| -> Result<u64, String> {
            if *pos + 8 > buf.len() {
                return Err("truncated IVF payload".to_string());
            }
            let value = u64::from_le_bytes([
                buf[*pos],
                buf[*pos + 1],
                buf[*pos + 2],
                buf[*pos + 3],
                buf[*pos + 4],
                buf[*pos + 5],
                buf[*pos + 6],
                buf[*pos + 7],
            ]);
            *pos += 8;
            Ok(value)
        };
        let read_f32 = |buf: &[u8], pos: &mut usize| -> Result<f32, String> {
            if *pos + 4 > buf.len() {
                return Err("truncated IVF payload".to_string());
            }
            let value = f32::from_le_bytes([
                buf[*pos],
                buf[*pos + 1],
                buf[*pos + 2],
                buf[*pos + 3],
            ]);
            *pos += 4;
            Ok(value)
        };

        let config = IvfConfig {
            n_lists: read_u32(bytes, &mut pos)? as usize,
            n_probes: read_u32(bytes, &mut pos)? as usize,
            dimension: read_u32(bytes, &mut pos)? as usize,
            max_iterations: read_u32(bytes, &mut pos)? as usize,
            convergence_threshold: read_f32(bytes, &mut pos)?,
        };
        if pos >= bytes.len() {
            return Err("truncated IVF payload".to_string());
        }
        let trained = bytes[pos] == 1;
        pos += 1;
        let count = read_u64(bytes, &mut pos)? as usize;
        let next_id = read_u64(bytes, &mut pos)?;
        let list_count = read_u32(bytes, &mut pos)? as usize;

        let mut lists = Vec::with_capacity(list_count);
        let mut id_to_list = HashMap::new();
        for list_idx in 0..list_count {
            let centroid_len = read_u32(bytes, &mut pos)? as usize;
            let mut centroid = Vec::with_capacity(centroid_len);
            for _ in 0..centroid_len {
                centroid.push(read_f32(bytes, &mut pos)?);
            }

            let id_count = read_u32(bytes, &mut pos)? as usize;
            let mut ids = Vec::with_capacity(id_count);
            for _ in 0..id_count {
                let id = read_u64(bytes, &mut pos)?;
                id_to_list.insert(id, list_idx);
                ids.push(id);
            }

            let vector_count = read_u32(bytes, &mut pos)? as usize;
            let mut vectors = Vec::with_capacity(vector_count);
            for _ in 0..vector_count {
                let vector_len = read_u32(bytes, &mut pos)? as usize;
                let mut vector = Vec::with_capacity(vector_len);
                for _ in 0..vector_len {
                    vector.push(read_f32(bytes, &mut pos)?);
                }
                vectors.push(vector);
            }

            lists.push(IvfList {
                centroid,
                ids,
                vectors,
            });
        }

        Ok(Self {
            config,
            lists,
            id_to_list,
            trained,
            count,
            next_id,
        })
    }
}

/// IVF index statistics
#[derive(Debug, Clone)]
pub struct IvfStats {
    pub total_vectors: usize,
    pub n_lists: usize,
    pub non_empty_lists: usize,
    pub avg_list_size: f64,
    pub max_list_size: usize,
    pub min_list_size: usize,
    pub dimension: usize,
    pub trained: bool,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vector(dim: usize, seed: u64) -> Vec<f32> {
        // Simple deterministic "random" for testing
        (0..dim)
            .map(|i| ((seed * 1103515245 + i as u64 * 12345) % 1000) as f32 / 1000.0)
            .collect()
    }

    #[test]
    fn test_ivf_basic() {
        let mut ivf = IvfIndex::new(IvfConfig::new(8, 4));

        // Generate training vectors
        let training: Vec<Vec<f32>> = (0..100).map(|i| random_vector(8, i)).collect();

        ivf.train(&training);
        assert!(ivf.trained);
        assert_eq!(ivf.n_lists(), 4);

        // Add vectors
        for (i, v) in training.iter().enumerate() {
            ivf.add_with_id(i as u64, v.clone());
        }

        assert_eq!(ivf.len(), 100);
    }

    #[test]
    fn test_ivf_search() {
        let dim = 8;
        let mut ivf = IvfIndex::new(IvfConfig {
            n_lists: 4,
            n_probes: 2,
            dimension: dim,
            ..Default::default()
        });

        // Create clustered data
        let mut vectors = Vec::new();
        for cluster in 0..4 {
            let base = cluster as f32 * 10.0;
            for i in 0..25 {
                let mut v = vec![base; dim];
                v[0] += i as f32 * 0.01;
                vectors.push(v);
            }
        }

        ivf.train(&vectors);

        for (i, v) in vectors.iter().enumerate() {
            ivf.add_with_id(i as u64, v.clone());
        }

        // Search for vector near cluster 0
        let query = vec![0.05; dim];
        let results = ivf.search(&query, 5);

        assert!(!results.is_empty());
        // Results should be from cluster 0 (IDs 0-24)
        for r in &results {
            assert!(r.id < 25);
        }
    }

    #[test]
    fn test_ivf_remove() {
        let mut ivf = IvfIndex::new(IvfConfig::new(4, 2));

        ivf.add_with_id(1, vec![1.0, 0.0, 0.0, 0.0]);
        ivf.add_with_id(2, vec![0.0, 1.0, 0.0, 0.0]);
        ivf.add_with_id(3, vec![0.0, 0.0, 1.0, 0.0]);

        assert_eq!(ivf.len(), 3);
        assert!(ivf.contains(2));

        assert!(ivf.remove(2));
        assert_eq!(ivf.len(), 2);
        assert!(!ivf.contains(2));
    }

    #[test]
    fn test_ivf_stats() {
        let mut ivf = IvfIndex::new(IvfConfig::new(4, 3));

        let training: Vec<Vec<f32>> = vec![
            vec![0.0, 0.0, 0.0, 0.0],
            vec![1.0, 0.0, 0.0, 0.0],
            vec![2.0, 0.0, 0.0, 0.0],
        ];

        ivf.train(&training);

        for (i, v) in training.iter().enumerate() {
            ivf.add_with_id(i as u64, v.clone());
        }

        let stats = ivf.stats();
        assert_eq!(stats.total_vectors, 3);
        assert_eq!(stats.n_lists, 3);
        assert!(stats.trained);
    }
}
