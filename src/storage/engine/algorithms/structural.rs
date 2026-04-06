//! Structural Graph Algorithms
//!
//! Algorithms for analyzing graph structure:
//! - HITS: Hubs and Authorities
//! - Strongly Connected Components (Tarjan's)
//! - Weakly Connected Components
//! - Triangle Counting
//! - Clustering Coefficient

use std::collections::{HashMap, HashSet, VecDeque};

use super::super::graph_store::GraphStore;

// ============================================================================
// HITS (Hubs and Authorities)
// ============================================================================

/// HITS algorithm: Identifies hubs and authorities
///
/// - Authorities: Nodes that are pointed to by many hubs (valuable targets)
/// - Hubs: Nodes that point to many authorities (good pivot points)
pub struct HITS {
    /// Maximum iterations
    pub max_iterations: usize,
    /// Convergence threshold
    pub epsilon: f64,
}

impl Default for HITS {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            epsilon: 1e-6,
        }
    }
}

/// Result of HITS computation
#[derive(Debug, Clone)]
pub struct HITSResult {
    /// Node ID → hub score
    pub hub_scores: HashMap<String, f64>,
    /// Node ID → authority score
    pub authority_scores: HashMap<String, f64>,
    /// Iterations until convergence
    pub iterations: usize,
    /// Whether converged
    pub converged: bool,
}

impl HITSResult {
    /// Get top N hubs
    pub fn top_hubs(&self, n: usize) -> Vec<(String, f64)> {
        let mut sorted: Vec<_> = self
            .hub_scores
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(n);
        sorted
    }

    /// Get top N authorities
    pub fn top_authorities(&self, n: usize) -> Vec<(String, f64)> {
        let mut sorted: Vec<_> = self
            .authority_scores
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(n);
        sorted
    }
}

impl HITS {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute HITS hub and authority scores
    pub fn compute(&self, graph: &GraphStore) -> HITSResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let n = nodes.len();

        if n == 0 {
            return HITSResult {
                hub_scores: HashMap::new(),
                authority_scores: HashMap::new(),
                iterations: 0,
                converged: true,
            };
        }

        // Build adjacency
        let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
        let mut incoming: HashMap<String, Vec<String>> = HashMap::new();

        for node in &nodes {
            let out: Vec<String> = graph
                .outgoing_edges(node)
                .into_iter()
                .map(|(_, t, _)| t)
                .collect();
            outgoing.insert(node.clone(), out);

            let inc: Vec<String> = graph
                .incoming_edges(node)
                .into_iter()
                .map(|(_, s, _)| s)
                .collect();
            incoming.insert(node.clone(), inc);
        }

        // Initialize scores
        let init = 1.0 / (n as f64).sqrt();
        let mut hub: HashMap<String, f64> = nodes.iter().map(|id| (id.clone(), init)).collect();
        let mut auth: HashMap<String, f64> = nodes.iter().map(|id| (id.clone(), init)).collect();

        let mut converged = false;
        let mut iterations = 0;

        for iter in 0..self.max_iterations {
            iterations = iter + 1;

            // Update authority scores: auth(p) = sum of hub(q) for all q that point to p
            let mut new_auth: HashMap<String, f64> = HashMap::new();
            for node in &nodes {
                let sum: f64 = incoming
                    .get(node)
                    .map(|inc| inc.iter().map(|s| hub.get(s).copied().unwrap_or(0.0)).sum())
                    .unwrap_or(0.0);
                new_auth.insert(node.clone(), sum);
            }

            // Normalize authority scores
            let auth_norm: f64 = new_auth.values().map(|v| v * v).sum::<f64>().sqrt();
            if auth_norm > 0.0 {
                for v in new_auth.values_mut() {
                    *v /= auth_norm;
                }
            }

            // Update hub scores: hub(p) = sum of auth(q) for all q that p points to
            let mut new_hub: HashMap<String, f64> = HashMap::new();
            for node in &nodes {
                let sum: f64 = outgoing
                    .get(node)
                    .map(|out| {
                        out.iter()
                            .map(|t| new_auth.get(t).copied().unwrap_or(0.0))
                            .sum()
                    })
                    .unwrap_or(0.0);
                new_hub.insert(node.clone(), sum);
            }

            // Normalize hub scores
            let hub_norm: f64 = new_hub.values().map(|v| v * v).sum::<f64>().sqrt();
            if hub_norm > 0.0 {
                for v in new_hub.values_mut() {
                    *v /= hub_norm;
                }
            }

            // Check convergence
            let hub_diff: f64 = nodes
                .iter()
                .map(|id| {
                    (hub.get(id).copied().unwrap_or(0.0) - new_hub.get(id).copied().unwrap_or(0.0))
                        .abs()
                })
                .sum();

            hub = new_hub;
            auth = new_auth;

            if hub_diff < self.epsilon {
                converged = true;
                break;
            }
        }

        HITSResult {
            hub_scores: hub,
            authority_scores: auth,
            iterations,
            converged,
        }
    }
}

// ============================================================================
// Strongly Connected Components (Tarjan's Algorithm)
// ============================================================================

/// Strongly connected components using Tarjan's algorithm
///
/// In a directed graph, an SCC is a maximal set of nodes where every node
/// is reachable from every other node.
pub struct StronglyConnectedComponents;

/// Result of SCC computation
#[derive(Debug, Clone)]
pub struct SCCResult {
    /// List of strongly connected components (sets of node IDs)
    pub components: Vec<Vec<String>>,
    /// Number of SCCs
    pub count: usize,
}

impl SCCResult {
    /// Get the largest SCC
    pub fn largest(&self) -> Option<&Vec<String>> {
        self.components.iter().max_by_key(|c| c.len())
    }

    /// Find which SCC a node belongs to
    pub fn component_of(&self, node_id: &str) -> Option<usize> {
        self.components
            .iter()
            .position(|c| c.contains(&node_id.to_string()))
    }
}

impl StronglyConnectedComponents {
    /// Find all strongly connected components using Tarjan's algorithm
    pub fn find(graph: &GraphStore) -> SCCResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();

        let mut index_counter = 0;
        let mut stack: Vec<String> = Vec::new();
        let mut on_stack: HashSet<String> = HashSet::new();
        let mut indices: HashMap<String, usize> = HashMap::new();
        let mut lowlinks: HashMap<String, usize> = HashMap::new();
        let mut components: Vec<Vec<String>> = Vec::new();

        fn strongconnect(
            graph: &GraphStore,
            node: &str,
            index_counter: &mut usize,
            stack: &mut Vec<String>,
            on_stack: &mut HashSet<String>,
            indices: &mut HashMap<String, usize>,
            lowlinks: &mut HashMap<String, usize>,
            components: &mut Vec<Vec<String>>,
        ) {
            indices.insert(node.to_string(), *index_counter);
            lowlinks.insert(node.to_string(), *index_counter);
            *index_counter += 1;
            stack.push(node.to_string());
            on_stack.insert(node.to_string());

            for (_, neighbor, _) in graph.outgoing_edges(node) {
                if !indices.contains_key(&neighbor) {
                    // Neighbor not visited, recurse
                    strongconnect(
                        graph,
                        &neighbor,
                        index_counter,
                        stack,
                        on_stack,
                        indices,
                        lowlinks,
                        components,
                    );
                    let neighbor_ll = *lowlinks.get(&neighbor).unwrap();
                    let node_ll = lowlinks.get_mut(node).unwrap();
                    *node_ll = (*node_ll).min(neighbor_ll);
                } else if on_stack.contains(&neighbor) {
                    // Neighbor on stack, update lowlink
                    let neighbor_idx = *indices.get(&neighbor).unwrap();
                    let node_ll = lowlinks.get_mut(node).unwrap();
                    *node_ll = (*node_ll).min(neighbor_idx);
                }
            }

            // If node is root of SCC
            if lowlinks.get(node) == indices.get(node) {
                let mut component = Vec::new();
                loop {
                    let w = stack.pop().unwrap();
                    on_stack.remove(&w);
                    component.push(w.clone());
                    if w == node {
                        break;
                    }
                }
                components.push(component);
            }
        }

        for node in &nodes {
            if !indices.contains_key(node) {
                strongconnect(
                    graph,
                    node,
                    &mut index_counter,
                    &mut stack,
                    &mut on_stack,
                    &mut indices,
                    &mut lowlinks,
                    &mut components,
                );
            }
        }

        // Sort components by size descending
        components.sort_by(|a, b| b.len().cmp(&a.len()));

        SCCResult {
            count: components.len(),
            components,
        }
    }
}

// ============================================================================
// Weakly Connected Components
// ============================================================================

/// Weakly connected components - treats directed graph as undirected
///
/// A weakly connected component is a set of nodes where there is a path
/// between any two nodes when ignoring edge direction.
pub struct WeaklyConnectedComponents;

/// Result of weakly connected components
#[derive(Debug, Clone)]
pub struct WCCResult {
    /// Each component as a list of node IDs
    pub components: Vec<Vec<String>>,
    /// Number of components
    pub count: usize,
    /// Node ID → component index
    pub node_to_component: HashMap<String, usize>,
}

impl WCCResult {
    /// Get the largest component
    pub fn largest(&self) -> Option<&Vec<String>> {
        self.components.iter().max_by_key(|c| c.len())
    }

    /// Get nodes in the same component as the given node
    pub fn component_of(&self, node: &str) -> Option<&Vec<String>> {
        self.node_to_component
            .get(node)
            .and_then(|&i| self.components.get(i))
    }
}

impl WeaklyConnectedComponents {
    /// Find all weakly connected components
    pub fn find(graph: &GraphStore) -> WCCResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();

        // Build undirected adjacency
        let mut neighbors: HashMap<String, Vec<String>> = HashMap::new();
        for node in &nodes {
            let mut nbrs: Vec<String> = Vec::new();
            for (_, target, _) in graph.outgoing_edges(node) {
                if target != *node {
                    nbrs.push(target);
                }
            }
            for (_, source, _) in graph.incoming_edges(node) {
                if source != *node && !nbrs.contains(&source) {
                    nbrs.push(source);
                }
            }
            neighbors.insert(node.clone(), nbrs);
        }

        // BFS to find components
        let mut visited: HashSet<String> = HashSet::new();
        let mut components: Vec<Vec<String>> = Vec::new();
        let mut node_to_component: HashMap<String, usize> = HashMap::new();

        for node in &nodes {
            if visited.contains(node) {
                continue;
            }

            let mut component: Vec<String> = Vec::new();
            let mut queue: VecDeque<String> = VecDeque::new();
            queue.push_back(node.clone());
            visited.insert(node.clone());

            while let Some(current) = queue.pop_front() {
                component.push(current.clone());

                if let Some(nbrs) = neighbors.get(&current) {
                    for nbr in nbrs {
                        if !visited.contains(nbr) {
                            visited.insert(nbr.clone());
                            queue.push_back(nbr.clone());
                        }
                    }
                }
            }

            let component_idx = components.len();
            for n in &component {
                node_to_component.insert(n.clone(), component_idx);
            }
            components.push(component);
        }

        // Sort by size descending
        let mut indexed: Vec<(usize, Vec<String>)> = components.into_iter().enumerate().collect();
        indexed.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

        // Rebuild with new indices
        let mut new_node_to_component: HashMap<String, usize> = HashMap::new();
        let new_components: Vec<Vec<String>> = indexed
            .into_iter()
            .enumerate()
            .map(|(new_idx, (_, comp))| {
                for n in &comp {
                    new_node_to_component.insert(n.clone(), new_idx);
                }
                comp
            })
            .collect();

        WCCResult {
            count: new_components.len(),
            components: new_components,
            node_to_component: new_node_to_component,
        }
    }
}

// ============================================================================
// Triangle Counting
// ============================================================================

/// Count triangles in the graph
///
/// Triangles indicate tightly connected clusters.
/// High triangle count in attack graphs suggests multiple redundant paths.
pub struct TriangleCounting;

/// Result of triangle counting
#[derive(Debug, Clone)]
pub struct TriangleResult {
    /// Total number of triangles
    pub count: usize,
    /// Node ID → number of triangles containing this node
    pub per_node: HashMap<String, usize>,
    /// The triangles themselves (as triples of node IDs)
    pub triangles: Vec<(String, String, String)>,
}

impl TriangleCounting {
    /// Count all triangles in the graph (treating as undirected)
    pub fn count(graph: &GraphStore) -> TriangleResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();

        // Build undirected adjacency
        let mut neighbors: HashMap<String, HashSet<String>> = HashMap::new();
        for node in &nodes {
            let mut nbrs: HashSet<String> = HashSet::new();
            for (_, target, _) in graph.outgoing_edges(node) {
                nbrs.insert(target);
            }
            for (_, source, _) in graph.incoming_edges(node) {
                nbrs.insert(source);
            }
            neighbors.insert(node.clone(), nbrs);
        }

        let mut triangles: Vec<(String, String, String)> = Vec::new();
        let mut per_node: HashMap<String, usize> = nodes.iter().map(|n| (n.clone(), 0)).collect();

        // For each node, check if any two neighbors are connected
        for node in &nodes {
            if let Some(node_nbrs) = neighbors.get(node) {
                let nbr_list: Vec<&String> = node_nbrs.iter().collect();
                for i in 0..nbr_list.len() {
                    for j in (i + 1)..nbr_list.len() {
                        let a = nbr_list[i];
                        let b = nbr_list[j];

                        // Check if a and b are connected
                        if neighbors.get(a).map(|s| s.contains(b)).unwrap_or(false) {
                            // Sort to avoid duplicates
                            let mut triple = vec![node.clone(), a.clone(), b.clone()];
                            triple.sort();

                            // Check if we've already found this triangle
                            let is_new = !triangles.iter().any(|(x, y, z)| {
                                let mut existing = vec![x.clone(), y.clone(), z.clone()];
                                existing.sort();
                                existing == triple
                            });

                            if is_new {
                                triangles.push((
                                    triple[0].clone(),
                                    triple[1].clone(),
                                    triple[2].clone(),
                                ));
                                *per_node.entry(triple[0].clone()).or_insert(0) += 1;
                                *per_node.entry(triple[1].clone()).or_insert(0) += 1;
                                *per_node.entry(triple[2].clone()).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
        }

        TriangleResult {
            count: triangles.len(),
            per_node,
            triangles,
        }
    }
}

// ============================================================================
// Clustering Coefficient
// ============================================================================

/// Local and global clustering coefficient
///
/// Measures how much neighbors of a node are connected to each other.
/// High clustering = tightly connected neighborhood = potential attack surface.
pub struct ClusteringCoefficient;

/// Result of clustering coefficient computation
#[derive(Debug, Clone)]
pub struct ClusteringResult {
    /// Node ID → local clustering coefficient (0 to 1)
    pub local: HashMap<String, f64>,
    /// Global clustering coefficient (average of local)
    pub global: f64,
}

impl ClusteringResult {
    /// Get nodes with highest clustering
    pub fn top(&self, n: usize) -> Vec<(String, f64)> {
        let mut sorted: Vec<_> = self.local.iter().map(|(k, v)| (k.clone(), *v)).collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(n);
        sorted
    }
}

impl ClusteringCoefficient {
    /// Compute local and global clustering coefficients
    pub fn compute(graph: &GraphStore) -> ClusteringResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();

        // Build undirected adjacency
        let mut neighbors: HashMap<String, HashSet<String>> = HashMap::new();
        for node in &nodes {
            let mut nbrs: HashSet<String> = HashSet::new();
            for (_, target, _) in graph.outgoing_edges(node) {
                if target != *node {
                    nbrs.insert(target);
                }
            }
            for (_, source, _) in graph.incoming_edges(node) {
                if source != *node {
                    nbrs.insert(source);
                }
            }
            neighbors.insert(node.clone(), nbrs);
        }

        let mut local: HashMap<String, f64> = HashMap::new();

        for node in &nodes {
            if let Some(node_nbrs) = neighbors.get(node) {
                let k = node_nbrs.len();
                if k < 2 {
                    local.insert(node.clone(), 0.0);
                    continue;
                }

                // Count edges between neighbors
                let mut edges_between = 0;
                let nbr_list: Vec<&String> = node_nbrs.iter().collect();
                for i in 0..nbr_list.len() {
                    for j in (i + 1)..nbr_list.len() {
                        if neighbors
                            .get(nbr_list[i])
                            .map(|s| s.contains(nbr_list[j]))
                            .unwrap_or(false)
                        {
                            edges_between += 1;
                        }
                    }
                }

                // Local clustering coefficient = 2 * edges_between / (k * (k-1))
                let max_edges = k * (k - 1) / 2;
                let cc = if max_edges > 0 {
                    edges_between as f64 / max_edges as f64
                } else {
                    0.0
                };
                local.insert(node.clone(), cc);
            } else {
                local.insert(node.clone(), 0.0);
            }
        }

        // Global clustering coefficient (average of local)
        let global = if !local.is_empty() {
            local.values().sum::<f64>() / local.len() as f64
        } else {
            0.0
        };

        ClusteringResult { local, global }
    }
}
