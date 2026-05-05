//! Standalone vector clustering: K-Means and DBSCAN.
//!
//! Extracted from IVF internals and extended with DBSCAN for
//! density-based clustering without a pre-specified K.

use super::simd_distance::l2_squared_simd;

/// Result of a clustering operation.
#[derive(Debug, Clone)]
pub struct ClusterResult {
    /// Cluster assignment for each input vector (index → cluster_id).
    /// -1 means noise (DBSCAN only).
    pub assignments: Vec<i32>,
    /// Centroid vectors (one per cluster). Empty for DBSCAN noise points.
    pub centroids: Vec<Vec<f32>>,
    /// Number of clusters found.
    pub k: usize,
    /// Per-cluster sizes.
    pub cluster_sizes: Vec<usize>,
    /// Iterations used (K-Means) or 0 (DBSCAN).
    pub iterations: usize,
    /// Whether the algorithm converged (K-Means only).
    pub converged: bool,
}

// ── K-Means ─────────────────────────────────────────────────────────────────

/// K-Means++ clustering.
///
/// Partitions `vectors` into `k` clusters by iteratively assigning each vector
/// to its nearest centroid and recomputing centroids as cluster means.
/// Uses K-Means++ initialization for better starting centroids.
pub fn kmeans(
    vectors: &[Vec<f32>],
    k: usize,
    max_iterations: usize,
    convergence_threshold: f32,
) -> ClusterResult {
    if vectors.is_empty() || k == 0 {
        return ClusterResult {
            assignments: Vec::new(),
            centroids: Vec::new(),
            k: 0,
            cluster_sizes: Vec::new(),
            iterations: 0,
            converged: true,
        };
    }

    let k = k.min(vectors.len());
    let dim = vectors[0].len();

    // K-Means++ initialization
    let mut centroids = kmeans_plusplus_init(vectors, k);

    let mut assignments = vec![0i32; vectors.len()];
    let mut iterations = 0;
    let mut converged = false;
    let use_parallel = vectors.len() > 1000
        && std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(1)
            > 1;

    for iter in 0..max_iterations {
        iterations = iter + 1;

        // Assign each vector to nearest centroid (parallel for large datasets)
        if use_parallel {
            std::thread::scope(|s| {
                let chunk_size = (vectors.len() / 4).max(256);
                let chunks: Vec<_> = assignments.chunks_mut(chunk_size).enumerate().collect();
                let handles: Vec<_> = chunks
                    .into_iter()
                    .map(|(chunk_idx, chunk)| {
                        let centroids = &centroids;
                        let vectors = &vectors;
                        let offset = chunk_idx * chunk_size;
                        s.spawn(move || {
                            for (j, assignment) in chunk.iter_mut().enumerate() {
                                let i = offset + j;
                                if i < vectors.len() {
                                    *assignment =
                                        find_nearest_centroid(&vectors[i], centroids) as i32;
                                }
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    let _ = h.join();
                }
            });
        } else {
            for (i, vector) in vectors.iter().enumerate() {
                assignments[i] = find_nearest_centroid(vector, &centroids) as i32;
            }
        }

        let mut cluster_groups: Vec<Vec<usize>> = vec![Vec::new(); k];
        for (i, &a) in assignments.iter().enumerate() {
            cluster_groups[a as usize].push(i);
        }

        // Recompute centroids
        let mut max_shift: f32 = 0.0;
        let mut new_centroids = Vec::with_capacity(k);

        for (cluster_idx, indices) in cluster_groups.iter().enumerate() {
            if indices.is_empty() {
                new_centroids.push(centroids[cluster_idx].clone());
                continue;
            }

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

            let shift = l2_squared_simd(&new_centroid, &centroids[cluster_idx]).sqrt();
            max_shift = max_shift.max(shift);
            new_centroids.push(new_centroid);
        }

        centroids = new_centroids;

        if max_shift < convergence_threshold {
            converged = true;
            break;
        }
    }

    let cluster_sizes: Vec<usize> = (0..k)
        .map(|c| assignments.iter().filter(|&&a| a == c as i32).count())
        .collect();

    ClusterResult {
        assignments,
        centroids,
        k,
        cluster_sizes,
        iterations,
        converged,
    }
}

fn kmeans_plusplus_init(vectors: &[Vec<f32>], k: usize) -> Vec<Vec<f32>> {
    let mut centroids = Vec::with_capacity(k);
    if vectors.is_empty() || k == 0 {
        return centroids;
    }

    centroids.push(vectors[vectors.len() / 2].clone());

    for _ in 1..k {
        let distances: Vec<f32> = vectors
            .iter()
            .map(|v| {
                centroids
                    .iter()
                    .map(|c| l2_squared_simd(v, c))
                    .fold(f32::MAX, f32::min)
            })
            .collect();

        let max_idx = distances
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        centroids.push(vectors[max_idx].clone());
    }

    centroids
}

fn find_nearest_centroid(vector: &[f32], centroids: &[Vec<f32>]) -> usize {
    centroids
        .iter()
        .enumerate()
        .map(|(i, c)| (i, l2_squared_simd(vector, c)))
        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

// ── DBSCAN ──────────────────────────────────────────────────────────────────

/// DBSCAN (Density-Based Spatial Clustering of Applications with Noise).
///
/// Finds clusters of arbitrary shape based on density. Points that are not
/// reachable from any dense region are labeled as noise (-1).
///
/// - `eps`: maximum distance between two points to be neighbors (L2)
/// - `min_points`: minimum neighbors to form a dense region
pub fn dbscan(vectors: &[Vec<f32>], eps: f32, min_points: usize) -> ClusterResult {
    const UNVISITED: i32 = -2;
    const NOISE: i32 = -1;

    let n = vectors.len();
    if n == 0 {
        return ClusterResult {
            assignments: Vec::new(),
            centroids: Vec::new(),
            k: 0,
            cluster_sizes: Vec::new(),
            iterations: 0,
            converged: true,
        };
    }

    let eps_sq = eps * eps;
    let mut assignments = vec![UNVISITED; n];
    let mut visited = vec![false; n];
    let mut cluster_id: i32 = 0;

    for i in 0..n {
        if visited[i] {
            continue;
        }

        visited[i] = true;
        let neighbors = range_query(vectors, i, eps_sq);

        if neighbors.len() < min_points {
            assignments[i] = NOISE;
            continue;
        }

        // Start new cluster
        assignments[i] = cluster_id;
        let mut seed_set: Vec<usize> = neighbors;
        let mut j = 0;

        while j < seed_set.len() {
            let q = seed_set[j];
            j += 1;

            if !visited[q] {
                visited[q] = true;

                let q_neighbors = range_query(vectors, q, eps_sq);
                if q_neighbors.len() >= min_points {
                    for &neighbor in &q_neighbors {
                        if matches!(assignments[neighbor], UNVISITED | NOISE)
                            && !seed_set.contains(&neighbor)
                        {
                            seed_set.push(neighbor);
                        }
                    }
                }
            }

            if matches!(assignments[q], UNVISITED | NOISE) {
                assignments[q] = cluster_id;
            }
        }

        cluster_id += 1;
    }

    for assignment in &mut assignments {
        if *assignment == UNVISITED {
            *assignment = NOISE;
        }
    }

    let k = cluster_id as usize;

    // Compute centroids for each cluster
    let dim = vectors[0].len();
    let mut centroids = Vec::with_capacity(k);
    let mut cluster_sizes = Vec::with_capacity(k);

    for c in 0..k {
        let members: Vec<usize> = assignments
            .iter()
            .enumerate()
            .filter(|(_, &a)| a == c as i32)
            .map(|(i, _)| i)
            .collect();

        cluster_sizes.push(members.len());

        if members.is_empty() {
            centroids.push(vec![0.0; dim]);
            continue;
        }

        let mut centroid = vec![0.0f32; dim];
        for &idx in &members {
            for (j, val) in vectors[idx].iter().enumerate() {
                if j < dim {
                    centroid[j] += val;
                }
            }
        }
        for val in &mut centroid {
            *val /= members.len() as f32;
        }
        centroids.push(centroid);
    }

    ClusterResult {
        assignments,
        centroids,
        k,
        cluster_sizes,
        iterations: 0,
        converged: true,
    }
}

/// Find all points within eps_sq (squared L2) distance of vectors[idx].
fn range_query(vectors: &[Vec<f32>], idx: usize, eps_sq: f32) -> Vec<usize> {
    let point = &vectors[idx];
    vectors
        .iter()
        .enumerate()
        .filter(|(_, v)| l2_squared_simd(point, v) <= eps_sq)
        .map(|(i, _)| i)
        .collect()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kmeans_basic() {
        let vectors = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.1],
            vec![0.2, 0.0],
            vec![10.0, 10.0],
            vec![10.1, 10.1],
            vec![10.2, 10.0],
        ];
        let result = kmeans(&vectors, 2, 100, 0.001);
        assert_eq!(result.k, 2);
        assert_eq!(result.assignments.len(), 6);
        // First 3 should be in one cluster, last 3 in another
        assert_eq!(result.assignments[0], result.assignments[1]);
        assert_eq!(result.assignments[1], result.assignments[2]);
        assert_eq!(result.assignments[3], result.assignments[4]);
        assert_eq!(result.assignments[4], result.assignments[5]);
        assert_ne!(result.assignments[0], result.assignments[3]);
    }

    #[test]
    fn test_kmeans_single_cluster() {
        let vectors = vec![vec![1.0, 1.0], vec![1.1, 1.1], vec![0.9, 0.9]];
        let result = kmeans(&vectors, 1, 10, 0.001);
        assert_eq!(result.k, 1);
        assert!(result.assignments.iter().all(|&a| a == 0));
    }

    #[test]
    fn test_kmeans_empty() {
        let result = kmeans(&[], 5, 10, 0.001);
        assert_eq!(result.k, 0);
    }

    #[test]
    fn test_dbscan_basic() {
        let vectors = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.0],
            vec![0.0, 0.1],
            vec![10.0, 10.0],
            vec![10.1, 10.0],
            vec![10.0, 10.1],
            vec![100.0, 100.0], // noise
        ];
        let result = dbscan(&vectors, 0.5, 2);
        assert_eq!(result.k, 2);
        // First 3 in one cluster
        assert_eq!(result.assignments[0], result.assignments[1]);
        assert_eq!(result.assignments[1], result.assignments[2]);
        // Last 3 (minus noise) in another cluster
        assert_eq!(result.assignments[3], result.assignments[4]);
        assert_eq!(result.assignments[4], result.assignments[5]);
        // Two distinct clusters
        assert_ne!(result.assignments[0], result.assignments[3]);
        // Noise point
        assert_eq!(result.assignments[6], -1);
    }

    #[test]
    fn test_dbscan_all_noise() {
        let vectors = vec![vec![0.0, 0.0], vec![100.0, 100.0], vec![200.0, 200.0]];
        let result = dbscan(&vectors, 0.1, 2);
        assert_eq!(result.k, 0);
        assert!(result.assignments.iter().all(|&a| a == -1));
    }

    #[test]
    fn test_dbscan_single_cluster() {
        let vectors = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.0],
            vec![0.2, 0.0],
            vec![0.3, 0.0],
        ];
        let result = dbscan(&vectors, 0.15, 2);
        assert_eq!(result.k, 1);
        assert!(result.assignments.iter().all(|&a| a == 0));
    }

    #[test]
    fn test_dbscan_relabels_noise_point_when_later_core_expands_cluster() {
        let vectors = vec![
            vec![0.0, 0.0],
            vec![0.08, 0.0],
            vec![0.16, 0.0],
            vec![10.0, 10.0],
        ];

        let result = dbscan(&vectors, 0.09, 3);

        assert_eq!(result.k, 1);
        assert_eq!(result.assignments[0], 0);
        assert_eq!(result.assignments[1], 0);
        assert_eq!(result.assignments[2], 0);
        assert_eq!(result.assignments[3], -1);
        assert_eq!(result.cluster_sizes, vec![3]);
    }

    #[test]
    fn test_dbscan_expands_density_connected_chain() {
        let vectors = vec![
            vec![0.0, 0.0],
            vec![0.08, 0.0],
            vec![0.16, 0.0],
            vec![0.24, 0.0],
        ];

        let result = dbscan(&vectors, 0.09, 3);

        assert_eq!(result.k, 1);
        assert!(result.assignments.iter().all(|&assignment| assignment == 0));
        assert_eq!(result.cluster_sizes, vec![4]);
    }
}
