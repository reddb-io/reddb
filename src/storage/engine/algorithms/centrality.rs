//! Centrality Algorithms
//!
//! Centrality measures for identifying important nodes:
//! - Betweenness Centrality: Nodes on many shortest paths (chokepoints)
//! - Closeness Centrality: Nodes close to all others (good attack starting points)
//! - Degree Centrality: Nodes with many connections
//! - Eigenvector Centrality: Nodes connected to other important nodes

use std::collections::{HashMap, HashSet, VecDeque};

use super::super::graph_store::GraphStore;

// ============================================================================
// Betweenness Centrality (Brandes' Algorithm)
// ============================================================================

/// Betweenness centrality computation using Brandes' algorithm
///
/// Betweenness centrality measures how often a node lies on shortest paths.
/// High betweenness nodes are chokepoints - critical for network flow.
pub struct BetweennessCentrality;

/// Result of betweenness centrality computation
#[derive(Debug, Clone)]
pub struct BetweennessResult {
    /// Node ID → betweenness centrality score
    pub scores: HashMap<String, f64>,
    /// Whether scores are normalized (divided by (n-1)(n-2))
    pub normalized: bool,
}

impl BetweennessResult {
    /// Get top N nodes by betweenness centrality
    pub fn top(&self, n: usize) -> Vec<(String, f64)> {
        let mut sorted: Vec<_> = self.scores.iter().map(|(k, v)| (k.clone(), *v)).collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(n);
        sorted
    }

    /// Get score for a specific node
    pub fn score(&self, node_id: &str) -> Option<f64> {
        self.scores.get(node_id).copied()
    }
}

impl BetweennessCentrality {
    /// Compute betweenness centrality for all nodes
    ///
    /// Uses Brandes' algorithm: O(V*E) time, O(V) space
    pub fn compute(graph: &GraphStore, normalize: bool) -> BetweennessResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let n = nodes.len();

        if n < 2 {
            return BetweennessResult {
                scores: nodes.into_iter().map(|id| (id, 0.0)).collect(),
                normalized: normalize,
            };
        }

        let mut centrality: HashMap<String, f64> =
            nodes.iter().map(|id| (id.clone(), 0.0)).collect();

        // Brandes' algorithm
        for source in &nodes {
            // Single-source shortest paths
            let mut stack: Vec<String> = Vec::new();
            let mut predecessors: HashMap<String, Vec<String>> = HashMap::new();
            let mut sigma: HashMap<String, f64> =
                nodes.iter().map(|id| (id.clone(), 0.0)).collect();
            let mut dist: HashMap<String, i64> = nodes.iter().map(|id| (id.clone(), -1)).collect();

            sigma.insert(source.clone(), 1.0);
            dist.insert(source.clone(), 0);

            let mut queue: VecDeque<String> = VecDeque::new();
            queue.push_back(source.clone());

            // BFS
            while let Some(v) = queue.pop_front() {
                stack.push(v.clone());
                let v_dist = *dist.get(&v).unwrap_or(&0);

                for (_, w, _) in graph.outgoing_edges(&v) {
                    // w found for first time?
                    if *dist.get(&w).unwrap_or(&-1) < 0 {
                        queue.push_back(w.clone());
                        dist.insert(w.clone(), v_dist + 1);
                    }

                    // Shortest path to w via v?
                    if *dist.get(&w).unwrap_or(&0) == v_dist + 1 {
                        let sigma_v = *sigma.get(&v).unwrap_or(&0.0);
                        let sigma_w = sigma.entry(w.clone()).or_insert(0.0);
                        *sigma_w += sigma_v;
                        predecessors.entry(w.clone()).or_default().push(v.clone());
                    }
                }
            }

            // Accumulation
            let mut delta: HashMap<String, f64> =
                nodes.iter().map(|id| (id.clone(), 0.0)).collect();

            while let Some(w) = stack.pop() {
                if let Some(preds) = predecessors.get(&w) {
                    let sigma_w = *sigma.get(&w).unwrap_or(&1.0);
                    let delta_w = *delta.get(&w).unwrap_or(&0.0);

                    for v in preds {
                        let sigma_v = *sigma.get(v).unwrap_or(&1.0);
                        let d = (sigma_v / sigma_w) * (1.0 + delta_w);
                        *delta.entry(v.clone()).or_insert(0.0) += d;
                    }
                }

                if w != *source {
                    let c = centrality.entry(w.clone()).or_insert(0.0);
                    *c += *delta.get(&w).unwrap_or(&0.0);
                }
            }
        }

        // Normalize if requested
        if normalize && n > 2 {
            let norm_factor = 1.0 / ((n - 1) * (n - 2)) as f64;
            for score in centrality.values_mut() {
                *score *= norm_factor;
            }
        }

        BetweennessResult {
            scores: centrality,
            normalized: normalize,
        }
    }
}

// ============================================================================
// Closeness Centrality
// ============================================================================

/// Closeness centrality measures how close a node is to all other nodes
///
/// High closeness = can reach all nodes quickly = good attack starting point
/// Low closeness = isolated, harder to reach
pub struct ClosenessCentrality;

/// Result of closeness centrality computation
#[derive(Debug, Clone)]
pub struct ClosenessResult {
    /// Node ID → closeness centrality (0 to 1, higher = more central)
    pub scores: HashMap<String, f64>,
}

impl ClosenessResult {
    /// Get top N nodes by closeness centrality
    pub fn top(&self, n: usize) -> Vec<(String, f64)> {
        let mut sorted: Vec<_> = self.scores.iter().map(|(k, v)| (k.clone(), *v)).collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(n);
        sorted
    }
}

impl ClosenessCentrality {
    /// Compute closeness centrality for all nodes
    ///
    /// Closeness = (n-1) / sum(shortest_path_distances)
    /// For disconnected graphs, uses harmonic closeness variant
    pub fn compute(graph: &GraphStore) -> ClosenessResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let n = nodes.len();

        if n <= 1 {
            return ClosenessResult {
                scores: nodes.into_iter().map(|id| (id, 1.0)).collect(),
            };
        }

        let mut scores: HashMap<String, f64> = HashMap::new();

        for source in &nodes {
            // BFS to find shortest paths from this node
            let mut distances: HashMap<String, usize> = HashMap::new();
            let mut queue: VecDeque<(String, usize)> = VecDeque::new();

            queue.push_back((source.clone(), 0));
            distances.insert(source.clone(), 0);

            while let Some((current, dist)) = queue.pop_front() {
                for (_, neighbor, _) in graph.outgoing_edges(&current) {
                    if !distances.contains_key(&neighbor) {
                        distances.insert(neighbor.clone(), dist + 1);
                        queue.push_back((neighbor, dist + 1));
                    }
                }
            }

            // Calculate closeness (harmonic variant for disconnected graphs)
            let sum_reciprocal: f64 = distances
                .iter()
                .filter(|(k, _)| *k != source)
                .map(|(_, d)| 1.0 / (*d as f64))
                .sum();

            let closeness = sum_reciprocal / (n - 1) as f64;
            scores.insert(source.clone(), closeness);
        }

        ClosenessResult { scores }
    }
}

// ============================================================================
// Degree Centrality
// ============================================================================

/// Degree centrality measures node importance by connection count
///
/// In security analysis:
/// - High in-degree: Popular target (many paths lead here)
/// - High out-degree: Key pivot point (can reach many targets)
pub struct DegreeCentrality;

/// Result of degree centrality computation
#[derive(Debug, Clone)]
pub struct DegreeCentralityResult {
    /// Node ID → in-degree
    pub in_degree: HashMap<String, usize>,
    /// Node ID → out-degree
    pub out_degree: HashMap<String, usize>,
    /// Node ID → total degree (in + out)
    pub total_degree: HashMap<String, usize>,
}

impl DegreeCentralityResult {
    /// Get nodes sorted by total degree
    pub fn top_by_total(&self, n: usize) -> Vec<(String, usize)> {
        let mut sorted: Vec<_> = self
            .total_degree
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(n);
        sorted
    }

    /// Get nodes sorted by in-degree
    pub fn top_by_in_degree(&self, n: usize) -> Vec<(String, usize)> {
        let mut sorted: Vec<_> = self
            .in_degree
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(n);
        sorted
    }

    /// Get nodes sorted by out-degree
    pub fn top_by_out_degree(&self, n: usize) -> Vec<(String, usize)> {
        let mut sorted: Vec<_> = self
            .out_degree
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(n);
        sorted
    }
}

impl DegreeCentrality {
    /// Compute degree centrality for all nodes
    pub fn compute(graph: &GraphStore) -> DegreeCentralityResult {
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut out_degree: HashMap<String, usize> = HashMap::new();

        // Initialize all nodes with 0 degree
        for node in graph.iter_nodes() {
            in_degree.insert(node.id.clone(), 0);
            out_degree.insert(node.id.clone(), 0);
        }

        // Count degrees
        for node in graph.iter_nodes() {
            let out_count = graph.outgoing_edges(&node.id).len();
            out_degree.insert(node.id.clone(), out_count);

            // Count incoming edges by iterating targets
            for (_, target, _) in graph.outgoing_edges(&node.id) {
                *in_degree.entry(target).or_insert(0) += 1;
            }
        }

        // Calculate total degree
        let total_degree: HashMap<String, usize> = in_degree
            .keys()
            .map(|k| {
                let total = in_degree.get(k).unwrap_or(&0) + out_degree.get(k).unwrap_or(&0);
                (k.clone(), total)
            })
            .collect();

        DegreeCentralityResult {
            in_degree,
            out_degree,
            total_degree,
        }
    }
}

// ============================================================================
// Eigenvector Centrality (Power Iteration)
// ============================================================================

/// Eigenvector centrality: importance based on neighbor importance
///
/// Like PageRank but without damping. A node is important if connected
/// to other important nodes.
pub struct EigenvectorCentrality {
    /// Convergence threshold
    pub epsilon: f64,
    /// Maximum iterations
    pub max_iterations: usize,
}

impl Default for EigenvectorCentrality {
    fn default() -> Self {
        Self {
            epsilon: 1e-6,
            max_iterations: 100,
        }
    }
}

/// Result of eigenvector centrality computation
#[derive(Debug, Clone)]
pub struct EigenvectorResult {
    /// Node ID → eigenvector centrality score
    pub scores: HashMap<String, f64>,
    /// Number of iterations
    pub iterations: usize,
    /// Whether converged
    pub converged: bool,
}

impl EigenvectorResult {
    /// Get top N nodes by eigenvector centrality
    pub fn top(&self, n: usize) -> Vec<(String, f64)> {
        let mut sorted: Vec<_> = self.scores.iter().map(|(k, v)| (k.clone(), *v)).collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(n);
        sorted
    }
}

impl EigenvectorCentrality {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute eigenvector centrality using power iteration
    pub fn compute(&self, graph: &GraphStore) -> EigenvectorResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let n = nodes.len();

        if n == 0 {
            return EigenvectorResult {
                scores: HashMap::new(),
                iterations: 0,
                converged: true,
            };
        }

        // Build adjacency (treat as undirected for eigenvector centrality)
        let mut neighbors: HashMap<String, Vec<String>> = HashMap::new();
        for node in &nodes {
            let mut node_neighbors: HashSet<String> = HashSet::new();
            for (_, target, _) in graph.outgoing_edges(node) {
                node_neighbors.insert(target);
            }
            for (_, source, _) in graph.incoming_edges(node) {
                node_neighbors.insert(source);
            }
            neighbors.insert(node.clone(), node_neighbors.into_iter().collect());
        }

        // Initialize scores uniformly
        let init_score = 1.0 / (n as f64).sqrt();
        let mut scores: HashMap<String, f64> =
            nodes.iter().map(|id| (id.clone(), init_score)).collect();

        let mut converged = false;
        let mut iterations = 0;

        for iter in 0..self.max_iterations {
            iterations = iter + 1;
            let mut new_scores: HashMap<String, f64> = HashMap::new();

            // Calculate new scores (sum of neighbor scores)
            for node in &nodes {
                let sum: f64 = neighbors
                    .get(node)
                    .map(|nbrs| {
                        nbrs.iter()
                            .map(|n| scores.get(n).copied().unwrap_or(0.0))
                            .sum()
                    })
                    .unwrap_or(0.0);
                new_scores.insert(node.clone(), sum);
            }

            // Normalize (L2 norm)
            let norm: f64 = new_scores.values().map(|v| v * v).sum::<f64>().sqrt();
            if norm > 0.0 {
                for score in new_scores.values_mut() {
                    *score /= norm;
                }
            }

            // Check convergence
            let diff: f64 = nodes
                .iter()
                .map(|id| {
                    let old = scores.get(id).copied().unwrap_or(0.0);
                    let new = new_scores.get(id).copied().unwrap_or(0.0);
                    (old - new).abs()
                })
                .sum();

            scores = new_scores;

            if diff < self.epsilon {
                converged = true;
                break;
            }
        }

        EigenvectorResult {
            scores,
            iterations,
            converged,
        }
    }
}
