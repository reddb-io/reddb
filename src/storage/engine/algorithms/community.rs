//! Community Detection Algorithms
//!
//! Algorithms for detecting communities/clusters in graphs:
//! - Label Propagation: Fast, simple community detection
//! - Louvain: Modularity optimization for quality communities

use std::collections::{HashMap, HashSet};

use super::super::graph_store::GraphStore;

// ============================================================================
// Label Propagation
// ============================================================================

/// Label Propagation Algorithm for community detection
///
/// Nodes adopt the most common label among their neighbors.
/// Fast and scales well, but results can be non-deterministic.
pub struct LabelPropagation {
    /// Maximum iterations
    pub max_iterations: usize,
}

impl Default for LabelPropagation {
    fn default() -> Self {
        Self {
            max_iterations: 100,
        }
    }
}

/// A community of nodes
#[derive(Debug, Clone)]
pub struct Community {
    /// Community label (typically the ID of a founding member)
    pub label: String,
    /// Nodes in this community
    pub nodes: Vec<String>,
    /// Size of the community
    pub size: usize,
}

/// Result of community detection
#[derive(Debug, Clone)]
pub struct CommunitiesResult {
    /// Detected communities, sorted by size descending
    pub communities: Vec<Community>,
    /// Number of iterations until convergence
    pub iterations: usize,
    /// Whether the algorithm converged
    pub converged: bool,
}

impl CommunitiesResult {
    /// Get the largest community
    pub fn largest(&self) -> Option<&Community> {
        self.communities.first()
    }

    /// Find which community a node belongs to
    pub fn community_of(&self, node_id: &str) -> Option<&Community> {
        self.communities
            .iter()
            .find(|c| c.nodes.contains(&node_id.to_string()))
    }
}

impl LabelPropagation {
    /// Create with default parameters
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum iterations
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Run label propagation on the graph
    pub fn run(&self, graph: &GraphStore) -> CommunitiesResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();

        if nodes.is_empty() {
            return CommunitiesResult {
                communities: Vec::new(),
                iterations: 0,
                converged: true,
            };
        }

        // Initialize: each node gets its own label
        let mut labels: HashMap<String, String> =
            nodes.iter().map(|id| (id.clone(), id.clone())).collect();

        let mut converged = false;
        let mut iterations = 0;

        for iter in 0..self.max_iterations {
            iterations = iter + 1;
            let mut changed = false;

            // Process nodes in order (could shuffle for randomization)
            for node_id in &nodes {
                // Count neighbor labels
                let mut label_counts: HashMap<String, usize> = HashMap::new();

                // Outgoing edges
                for (_, neighbor, _) in graph.outgoing_edges(node_id) {
                    if let Some(label) = labels.get(&neighbor) {
                        *label_counts.entry(label.clone()).or_insert(0) += 1;
                    }
                }

                // Incoming edges (treat as undirected)
                for (_, neighbor, _) in graph.incoming_edges(node_id) {
                    if let Some(label) = labels.get(&neighbor) {
                        *label_counts.entry(label.clone()).or_insert(0) += 1;
                    }
                }

                // Find most common label
                if let Some((best_label, _)) =
                    label_counts.into_iter().max_by_key(|(_, count)| *count)
                {
                    let current = labels.get(node_id).cloned().unwrap_or_default();
                    if best_label != current {
                        labels.insert(node_id.clone(), best_label);
                        changed = true;
                    }
                }
            }

            if !changed {
                converged = true;
                break;
            }
        }

        // Group nodes by label
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for (node_id, label) in &labels {
            groups
                .entry(label.clone())
                .or_default()
                .push(node_id.clone());
        }

        // Build communities
        let mut communities: Vec<Community> = groups
            .into_iter()
            .map(|(label, nodes)| {
                let size = nodes.len();
                Community { label, nodes, size }
            })
            .collect();

        // Sort by size descending
        communities.sort_by(|a, b| b.size.cmp(&a.size));

        CommunitiesResult {
            communities,
            iterations,
            converged,
        }
    }
}

// ============================================================================
// Louvain Community Detection
// ============================================================================

/// Louvain algorithm for community detection
///
/// A greedy algorithm that optimizes modularity - a measure of how well
/// the network is partitioned into communities where nodes are densely
/// connected within communities but sparsely between them.
pub struct Louvain {
    /// Resolution parameter (higher = smaller communities)
    pub resolution: f64,
    /// Maximum iterations per phase
    pub max_iterations: usize,
    /// Minimum modularity improvement to continue
    pub min_improvement: f64,
}

impl Default for Louvain {
    fn default() -> Self {
        Self {
            resolution: 1.0,
            max_iterations: 10,
            min_improvement: 1e-6,
        }
    }
}

/// Result of Louvain community detection
#[derive(Debug, Clone)]
pub struct LouvainResult {
    /// Node ID → community ID
    pub communities: HashMap<String, usize>,
    /// Number of communities found
    pub count: usize,
    /// Final modularity score (-0.5 to 1.0, higher = better)
    pub modularity: f64,
    /// Number of passes/phases completed
    pub passes: usize,
}

impl LouvainResult {
    /// Get all nodes in a specific community
    pub fn get_community(&self, community_id: usize) -> Vec<String> {
        self.communities
            .iter()
            .filter(|(_, &c)| c == community_id)
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Get community sizes
    pub fn community_sizes(&self) -> HashMap<usize, usize> {
        let mut sizes: HashMap<usize, usize> = HashMap::new();
        for &c in self.communities.values() {
            *sizes.entry(c).or_insert(0) += 1;
        }
        sizes
    }
}

impl Louvain {
    /// Create new Louvain with default parameters
    pub fn new() -> Self {
        Self::default()
    }

    /// Set resolution parameter (default: 1.0)
    pub fn resolution(mut self, resolution: f64) -> Self {
        self.resolution = resolution;
        self
    }

    /// Set maximum iterations per phase (default: 10)
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Run Louvain community detection (treats graph as undirected)
    pub fn run(&self, graph: &GraphStore) -> LouvainResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();

        if nodes.is_empty() {
            return LouvainResult {
                communities: HashMap::new(),
                count: 0,
                modularity: 0.0,
                passes: 0,
            };
        }

        // Build undirected weighted adjacency
        let mut weights: HashMap<(String, String), f64> = HashMap::new();
        let mut node_strength: HashMap<String, f64> = HashMap::new();
        let mut total_weight = 0.0;

        for node in &nodes {
            for (_, target, _) in graph.outgoing_edges(node) {
                if node != &target {
                    let key = if node < &target {
                        (node.clone(), target.clone())
                    } else {
                        (target.clone(), node.clone())
                    };

                    let w = weights.entry(key).or_insert(0.0);
                    *w += 1.0; // Can use edge weight if available
                }
            }
        }

        // Calculate node strengths and total weight
        for ((a, b), w) in &weights {
            *node_strength.entry(a.clone()).or_insert(0.0) += w;
            *node_strength.entry(b.clone()).or_insert(0.0) += w;
            total_weight += w;
        }

        if total_weight == 0.0 {
            // No edges - each node is its own community
            let communities: HashMap<String, usize> = nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), i))
                .collect();
            return LouvainResult {
                count: nodes.len(),
                communities,
                modularity: 0.0,
                passes: 0,
            };
        }

        // Initialize: each node in its own community
        let mut communities: HashMap<String, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();

        // Community total weights
        let mut comm_total: HashMap<usize, f64> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (i, *node_strength.get(n).unwrap_or(&0.0)))
            .collect();

        // Community internal weights
        let mut comm_internal: HashMap<usize, f64> = HashMap::new();

        let mut passes = 0;
        let mut improved = true;

        while improved && passes < self.max_iterations {
            improved = false;
            passes += 1;

            for node in &nodes {
                let current_comm = *communities.get(node).unwrap();
                let node_w = *node_strength.get(node).unwrap_or(&0.0);

                // Calculate weights to each neighboring community
                let mut neighbor_comm_weights: HashMap<usize, f64> = HashMap::new();

                for ((a, b), w) in &weights {
                    if a == node {
                        let neighbor_comm = *communities.get(b).unwrap();
                        *neighbor_comm_weights.entry(neighbor_comm).or_insert(0.0) += w;
                    } else if b == node {
                        let neighbor_comm = *communities.get(a).unwrap();
                        *neighbor_comm_weights.entry(neighbor_comm).or_insert(0.0) += w;
                    }
                }

                // Try moving to each neighboring community
                let mut best_comm = current_comm;
                let mut best_delta = 0.0;

                // First, calculate delta for removing from current community
                let current_internal = neighbor_comm_weights
                    .get(&current_comm)
                    .copied()
                    .unwrap_or(0.0);
                let current_total = *comm_total.get(&current_comm).unwrap_or(&0.0);

                for (&target_comm, &weight_to_target) in &neighbor_comm_weights {
                    if target_comm == current_comm {
                        continue;
                    }

                    let target_total = *comm_total.get(&target_comm).unwrap_or(&0.0);

                    let delta = (weight_to_target - current_internal) / total_weight
                        - self.resolution * node_w * (target_total - current_total + node_w)
                            / (2.0 * total_weight * total_weight);

                    if delta > best_delta + self.min_improvement {
                        best_delta = delta;
                        best_comm = target_comm;
                    }
                }

                // Move node if beneficial
                if best_comm != current_comm {
                    improved = true;

                    // Update community totals
                    *comm_total.entry(current_comm).or_insert(0.0) -= node_w;
                    *comm_total.entry(best_comm).or_insert(0.0) += node_w;

                    // Update community internals
                    let current_internal = neighbor_comm_weights
                        .get(&current_comm)
                        .copied()
                        .unwrap_or(0.0);
                    *comm_internal.entry(current_comm).or_insert(0.0) -= current_internal;

                    let new_internal = neighbor_comm_weights
                        .get(&best_comm)
                        .copied()
                        .unwrap_or(0.0);
                    *comm_internal.entry(best_comm).or_insert(0.0) += new_internal;

                    communities.insert(node.clone(), best_comm);
                }
            }
        }

        // Renumber communities to be contiguous
        let unique_communities: Vec<usize> = {
            let c: HashSet<usize> = communities.values().copied().collect();
            let mut v: Vec<usize> = c.into_iter().collect();
            v.sort();
            v
        };

        let comm_map: HashMap<usize, usize> = unique_communities
            .iter()
            .enumerate()
            .map(|(new, &old)| (old, new))
            .collect();

        let remapped: HashMap<String, usize> = communities
            .into_iter()
            .map(|(n, c)| (n, *comm_map.get(&c).unwrap_or(&0)))
            .collect();

        // Calculate final modularity
        let modularity =
            self.calculate_modularity(&remapped, &weights, &node_strength, total_weight);

        LouvainResult {
            count: unique_communities.len(),
            communities: remapped,
            modularity,
            passes,
        }
    }

    /// Calculate modularity of a partition
    fn calculate_modularity(
        &self,
        communities: &HashMap<String, usize>,
        weights: &HashMap<(String, String), f64>,
        node_strength: &HashMap<String, f64>,
        total_weight: f64,
    ) -> f64 {
        if total_weight == 0.0 {
            return 0.0;
        }

        let mut q = 0.0;

        // Sum over all edges within same community
        for ((a, b), w) in weights {
            let ca = communities.get(a).unwrap();
            let cb = communities.get(b).unwrap();

            if ca == cb {
                let ka = node_strength.get(a).unwrap_or(&0.0);
                let kb = node_strength.get(b).unwrap_or(&0.0);
                q += w - self.resolution * ka * kb / (2.0 * total_weight);
            }
        }

        q / total_weight
    }
}
