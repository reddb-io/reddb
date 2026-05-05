//! PageRank Algorithms
//!
//! PageRank for identifying critical nodes in the graph.
//! High PageRank nodes are important because many other nodes link to them.
//!
//! Includes:
//! - PageRank: Standard PageRank algorithm
//! - PersonalizedPageRank: Teleport only to specified seed nodes

use std::collections::HashMap;

use super::super::graph_store::GraphStore;

// ============================================================================
// PageRank
// ============================================================================

/// PageRank algorithm for identifying critical nodes
///
/// Nodes with high PageRank are "important" because many other nodes link to them.
/// In attack path analysis, high PageRank nodes are critical chokepoints.
pub struct PageRank {
    /// Damping factor (probability of following a link vs teleporting)
    pub alpha: f64,
    /// Convergence threshold
    pub epsilon: f64,
    /// Maximum iterations
    pub max_iterations: usize,
}

impl Default for PageRank {
    fn default() -> Self {
        Self {
            alpha: 0.85,
            epsilon: 1e-6,
            max_iterations: 100,
        }
    }
}

/// Result of PageRank computation
#[derive(Debug, Clone)]
pub struct PageRankResult {
    /// Node ID → PageRank score
    pub scores: HashMap<String, f64>,
    /// Number of iterations until convergence
    pub iterations: usize,
    /// Whether the algorithm converged
    pub converged: bool,
}

impl PageRankResult {
    /// Get top N nodes by PageRank score
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

impl PageRank {
    /// Create PageRank with default parameters
    pub fn new() -> Self {
        Self::default()
    }

    /// Set damping factor (default: 0.85)
    pub fn alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    /// Set convergence threshold (default: 1e-6)
    pub fn epsilon(mut self, epsilon: f64) -> Self {
        self.epsilon = epsilon;
        self
    }

    /// Set maximum iterations (default: 100)
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Run PageRank on the graph
    pub fn run(&self, graph: &GraphStore) -> PageRankResult {
        // Collect all nodes
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let n = nodes.len();

        if n == 0 {
            return PageRankResult {
                scores: HashMap::new(),
                iterations: 0,
                converged: true,
            };
        }

        // Build adjacency: node_id → outgoing targets
        let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
        for node_id in &nodes {
            let edges = graph.outgoing_edges(node_id);
            let targets: Vec<String> = edges.into_iter().map(|(_, target, _)| target).collect();
            outgoing.insert(node_id.clone(), targets);
        }

        // Initialize scores uniformly
        let initial_score = 1.0 / n as f64;
        let mut scores: HashMap<String, f64> =
            nodes.iter().map(|id| (id.clone(), initial_score)).collect();

        let teleport = (1.0 - self.alpha) / n as f64;
        let mut converged = false;
        let mut iterations = 0;

        for iter in 0..self.max_iterations {
            iterations = iter + 1;
            let mut new_scores: HashMap<String, f64> = HashMap::new();

            // Calculate new scores
            for node_id in &nodes {
                let mut score = teleport;

                // Sum contributions from incoming edges
                for (source_id, targets) in &outgoing {
                    if targets.contains(node_id) {
                        let source_score = scores.get(source_id).copied().unwrap_or(0.0);
                        let out_degree = targets.len() as f64;
                        if out_degree > 0.0 {
                            score += self.alpha * source_score / out_degree;
                        }
                    }
                }

                new_scores.insert(node_id.clone(), score);
            }

            // Handle dangling nodes (no outgoing edges) - distribute their score
            let dangling_sum: f64 = nodes
                .iter()
                .filter(|id| outgoing.get(*id).map(|v| v.is_empty()).unwrap_or(true))
                .map(|id| scores.get(id).copied().unwrap_or(0.0))
                .sum();

            let dangling_contrib = self.alpha * dangling_sum / n as f64;
            for score in new_scores.values_mut() {
                *score += dangling_contrib;
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

        PageRankResult {
            scores,
            iterations,
            converged,
        }
    }
}

// ============================================================================
// Personalized PageRank
// ============================================================================

/// Personalized PageRank - teleport only to specified seed nodes
///
/// Useful for finding nodes that are "important" relative to a specific
/// entry point (e.g., "what hosts are most reachable from this compromised server?")
pub struct PersonalizedPageRank {
    /// Damping factor
    pub alpha: f64,
    /// Convergence threshold
    pub epsilon: f64,
    /// Maximum iterations
    pub max_iterations: usize,
    /// Seed nodes (teleportation targets)
    seeds: Vec<String>,
    /// Seed weights (must sum to 1.0)
    weights: Vec<f64>,
}

impl PersonalizedPageRank {
    /// Create personalized PageRank with uniform seed weights
    pub fn new(seeds: Vec<String>) -> Self {
        let n = seeds.len().max(1) as f64;
        let weights = vec![1.0 / n; seeds.len()];
        Self {
            alpha: 0.85,
            epsilon: 1e-6,
            max_iterations: 100,
            seeds,
            weights,
        }
    }

    /// Create with custom seed weights (must sum to 1.0)
    pub fn with_weights(seeds: Vec<String>, weights: Vec<f64>) -> Self {
        // Normalize weights to sum to 1.0
        let sum: f64 = weights.iter().sum();
        let normalized = if sum > 0.0 {
            weights.iter().map(|w| w / sum).collect()
        } else {
            vec![1.0 / seeds.len().max(1) as f64; seeds.len()]
        };

        Self {
            alpha: 0.85,
            epsilon: 1e-6,
            max_iterations: 100,
            seeds,
            weights: normalized,
        }
    }

    /// Set damping factor
    pub fn alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    /// Set convergence threshold
    pub fn epsilon(mut self, epsilon: f64) -> Self {
        self.epsilon = epsilon;
        self
    }

    /// Set maximum iterations
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Run personalized PageRank
    pub fn run(&self, graph: &GraphStore) -> PageRankResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let n = nodes.len();

        if n == 0 || self.seeds.is_empty() {
            return PageRankResult {
                scores: HashMap::new(),
                iterations: 0,
                converged: true,
            };
        }

        // Build seed weight lookup
        let seed_weights: HashMap<String, f64> = self
            .seeds
            .iter()
            .zip(self.weights.iter())
            .map(|(s, w)| (s.clone(), *w))
            .collect();

        // Build adjacency
        let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
        for node_id in &nodes {
            let edges = graph.outgoing_edges(node_id);
            let targets: Vec<String> = edges.into_iter().map(|(_, target, _)| target).collect();
            outgoing.insert(node_id.clone(), targets);
        }

        // Initialize scores - start concentrated on seeds
        let mut scores: HashMap<String, f64> = HashMap::new();
        for node_id in &nodes {
            let initial = seed_weights.get(node_id).copied().unwrap_or(0.0);
            scores.insert(node_id.clone(), initial);
        }

        let mut converged = false;
        let mut iterations = 0;

        for iter in 0..self.max_iterations {
            iterations = iter + 1;
            let mut new_scores: HashMap<String, f64> = HashMap::new();

            for node_id in &nodes {
                // Teleport: go to seeds with their weights
                let teleport =
                    (1.0 - self.alpha) * seed_weights.get(node_id).copied().unwrap_or(0.0);
                let mut score = teleport;

                // Sum contributions from incoming edges
                for (source_id, targets) in &outgoing {
                    if targets.contains(node_id) {
                        let source_score = scores.get(source_id).copied().unwrap_or(0.0);
                        let out_degree = targets.len() as f64;
                        if out_degree > 0.0 {
                            score += self.alpha * source_score / out_degree;
                        }
                    }
                }

                new_scores.insert(node_id.clone(), score);
            }

            // Handle dangling nodes - distribute to seeds
            let dangling_sum: f64 = nodes
                .iter()
                .filter(|id| outgoing.get(*id).map(|v| v.is_empty()).unwrap_or(true))
                .map(|id| scores.get(id).copied().unwrap_or(0.0))
                .sum();

            for (seed, weight) in &seed_weights {
                if let Some(score) = new_scores.get_mut(seed) {
                    *score += self.alpha * dangling_sum * weight;
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

        PageRankResult {
            scores,
            iterations,
            converged,
        }
    }
}
