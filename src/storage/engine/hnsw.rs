//! HNSW (Hierarchical Navigable Small World) Index
//!
//! A from-scratch implementation of the HNSW algorithm for approximate
//! nearest neighbor search. No external dependencies.
//!
//! # Algorithm Overview
//!
//! HNSW builds a multi-layer graph where:
//! - Layer 0 contains all nodes
//! - Higher layers contain progressively fewer nodes
//! - Each layer is a navigable small world graph
//!
//! Search starts from the top layer and greedily descends,
//! using each layer to quickly approach the target region.
//!
//! # References
//!
//! - Original paper: "Efficient and robust approximate nearest neighbor search
//!   using Hierarchical Navigable Small World graphs" (Malkov & Yashunin, 2018)

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use super::distance::{
    cmp_f32, distance_simd, DistanceMetric, DistanceResult, ReverseDistanceResult,
};

/// Node identifier in the HNSW graph
pub type NodeId = u64;

/// HNSW index configuration parameters
#[derive(Debug, Clone)]
pub struct HnswConfig {
    /// Maximum number of connections per node (except layer 0)
    pub m: usize,
    /// Maximum connections at layer 0 (typically 2*M)
    pub m_max0: usize,
    /// Size of dynamic candidate list during construction
    pub ef_construction: usize,
    /// Size of dynamic candidate list during search (can be adjusted)
    pub ef_search: usize,
    /// Normalization factor for layer assignment (1/ln(M))
    pub ml: f64,
    /// Distance metric to use
    pub metric: DistanceMetric,
}

impl Default for HnswConfig {
    fn default() -> Self {
        let m = 16;
        Self {
            m,
            m_max0: m * 2,
            ef_construction: 100,
            ef_search: 50,
            ml: 1.0 / (m as f64).ln(),
            metric: DistanceMetric::L2,
        }
    }
}

impl HnswConfig {
    /// Create a new configuration with custom M value
    pub fn with_m(m: usize) -> Self {
        Self {
            m,
            m_max0: m * 2,
            ml: 1.0 / (m as f64).ln(),
            ..Default::default()
        }
    }

    /// Set the distance metric
    pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Set ef_construction
    pub fn with_ef_construction(mut self, ef: usize) -> Self {
        self.ef_construction = ef;
        self
    }

    /// Set ef_search
    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.ef_search = ef;
        self
    }
}

/// A node in the HNSW graph
#[derive(Debug, Clone)]
struct HnswNode {
    /// The node's ID
    id: NodeId,
    /// The node's vector data
    vector: Vec<f32>,
    /// Maximum layer this node appears in
    max_layer: usize,
    /// Connections at each layer (layer index -> neighbor IDs)
    connections: Vec<Vec<NodeId>>,
}

impl HnswNode {
    fn new(id: NodeId, vector: Vec<f32>, max_layer: usize) -> Self {
        let mut connections = Vec::with_capacity(max_layer + 1);
        for _ in 0..=max_layer {
            connections.push(Vec::new());
        }
        Self {
            id,
            vector,
            max_layer,
            connections,
        }
    }
}

/// HNSW Index for approximate nearest neighbor search
pub struct HnswIndex {
    /// Configuration parameters
    config: HnswConfig,
    /// All nodes in the index
    nodes: HashMap<NodeId, HnswNode>,
    /// Entry point (node with highest layer)
    entry_point: Option<NodeId>,
    /// Maximum layer in the graph
    max_layer: usize,
    /// Vector dimension
    dimension: usize,
    /// Next available node ID
    next_id: AtomicU64,
    /// Simple RNG state for layer assignment
    rng_state: u64,
}

impl HnswIndex {
    /// Create a new empty HNSW index
    pub fn new(dimension: usize, config: HnswConfig) -> Self {
        Self {
            config,
            nodes: HashMap::new(),
            entry_point: None,
            max_layer: 0,
            dimension,
            next_id: AtomicU64::new(0),
            rng_state: 0x853c49e6748fea9b, // Random seed
        }
    }

    /// Create with default configuration
    pub fn with_dimension(dimension: usize) -> Self {
        Self::new(dimension, HnswConfig::default())
    }

    /// Get the number of vectors in the index
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Get the vector for a node ID
    pub fn get_vector(&self, id: NodeId) -> Option<&[f32]> {
        self.nodes.get(&id).map(|n| n.vector.as_slice())
    }

    /// Insert a vector and return its assigned ID
    pub fn insert(&mut self, vector: Vec<f32>) -> NodeId {
        let id = self.next_id.fetch_add(1, AtomicOrdering::SeqCst);
        self.insert_with_id(id, vector);
        id
    }

    /// Insert a vector with a specific ID
    pub fn insert_with_id(&mut self, id: NodeId, vector: Vec<f32>) {
        assert_eq!(
            vector.len(),
            self.dimension,
            "Vector dimension mismatch: expected {}, got {}",
            self.dimension,
            vector.len()
        );

        // Assign random layer using exponential distribution
        let node_layer = self.random_layer();

        // Create the node
        let node = HnswNode::new(id, vector, node_layer);

        if self.entry_point.is_none() {
            // First node - just add it
            self.nodes.insert(id, node);
            self.entry_point = Some(id);
            self.max_layer = node_layer;
            return;
        }

        let entry_point = self.entry_point.unwrap();
        let vector = self.nodes.get(&id).map(|n| n.vector.clone());

        // We need to insert the node first so we can access its vector
        // But we need to find neighbors first... let's clone the vector
        let vector = node.vector.clone();
        self.nodes.insert(id, node);

        // Find entry point for search
        let mut current = entry_point;

        // Traverse from top layer down to node_layer + 1
        // This finds the best entry point for the insertion layer
        for layer in (node_layer + 1..=self.max_layer).rev() {
            current = self.search_layer_single(&vector, current, layer);
        }

        // For layers from node_layer down to 0, find and connect to neighbors
        for layer in (0..=node_layer.min(self.max_layer)).rev() {
            // Find ef_construction nearest neighbors at this layer
            let neighbors = self.search_layer(&vector, current, self.config.ef_construction, layer);

            // Select M best neighbors
            let m = if layer == 0 {
                self.config.m_max0
            } else {
                self.config.m
            };
            let selected: Vec<NodeId> = neighbors.into_iter().take(m).map(|r| r.id).collect();

            // Connect node to selected neighbors
            if let Some(node) = self.nodes.get_mut(&id) {
                node.connections[layer] = selected.clone();
            }

            // Add bidirectional connections
            for &neighbor_id in &selected {
                self.add_connection(neighbor_id, id, layer);
            }

            // Update current for next layer
            if let Some(&first) = selected.first() {
                current = first;
            }
        }

        // Update entry point if new node has higher layer
        if node_layer > self.max_layer {
            self.entry_point = Some(id);
            self.max_layer = node_layer;
        }
    }

    /// Search for k nearest neighbors
    pub fn search(&self, query: &[f32], k: usize) -> Vec<DistanceResult> {
        self.search_with_ef(query, k, self.config.ef_search)
    }

    /// Search with custom ef parameter
    pub fn search_with_ef(&self, query: &[f32], k: usize, ef: usize) -> Vec<DistanceResult> {
        if self.entry_point.is_none() {
            return Vec::new();
        }

        let entry_point = self.entry_point.unwrap();
        let mut current = entry_point;

        // Traverse from top layer down to layer 1
        for layer in (1..=self.max_layer).rev() {
            current = self.search_layer_single(query, current, layer);
        }

        // Search layer 0 with ef candidates
        let candidates = self.search_layer(query, current, ef.max(k), 0);

        // Return top k
        candidates.into_iter().take(k).collect()
    }

    /// Search with a filter (bitset of allowed IDs)
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        filter: &HashSet<NodeId>,
    ) -> Vec<DistanceResult> {
        self.search_filtered_with_ef(query, k, filter, self.config.ef_search)
    }

    /// Search with filter and custom ef
    pub fn search_filtered_with_ef(
        &self,
        query: &[f32],
        k: usize,
        filter: &HashSet<NodeId>,
        ef: usize,
    ) -> Vec<DistanceResult> {
        if self.entry_point.is_none() || filter.is_empty() {
            return Vec::new();
        }

        let entry_point = self.entry_point.unwrap();
        let mut current = entry_point;

        // Traverse from top layer down to layer 1
        for layer in (1..=self.max_layer).rev() {
            current = self.search_layer_single(query, current, layer);
        }

        // Search layer 0 with filter
        // We need to search with higher ef to account for filtered results
        let expanded_ef = (ef * 2).max(k * 4);
        let candidates = self.search_layer(query, current, expanded_ef, 0);

        // Filter and return top k
        candidates
            .into_iter()
            .filter(|r| filter.contains(&r.id))
            .take(k)
            .collect()
    }

    // =========================================================================
    // Private methods
    // =========================================================================

    /// Generate a random layer using exponential distribution
    fn random_layer(&mut self) -> usize {
        // Simple xorshift64 PRNG
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;

        // Convert to uniform [0, 1)
        let uniform = (self.rng_state as f64) / (u64::MAX as f64);

        // Exponential distribution: -ln(uniform) * ml
        let level = (-uniform.ln() * self.config.ml).floor() as usize;

        level
    }

    /// Search a single layer for the closest node (greedy)
    fn search_layer_single(&self, query: &[f32], entry: NodeId, layer: usize) -> NodeId {
        let mut current = entry;
        let mut current_dist = self.compute_distance(query, current);

        loop {
            let mut changed = false;

            if let Some(node) = self.nodes.get(&current) {
                if layer < node.connections.len() {
                    for &neighbor in &node.connections[layer] {
                        let dist = self.compute_distance(query, neighbor);
                        if dist < current_dist {
                            current_dist = dist;
                            current = neighbor;
                            changed = true;
                        }
                    }
                }
            }

            if !changed {
                break;
            }
        }

        current
    }

    /// Search a layer for ef nearest neighbors
    fn search_layer(
        &self,
        query: &[f32],
        entry: NodeId,
        ef: usize,
        layer: usize,
    ) -> Vec<DistanceResult> {
        let entry_dist = self.compute_distance(query, entry);

        // Candidates: min-heap of nodes to explore (closest first)
        let mut candidates: BinaryHeap<Reverse<DistanceResult>> = BinaryHeap::new();
        candidates.push(Reverse(DistanceResult::new(entry, entry_dist)));

        // Results: max-heap of found neighbors (furthest first for pruning)
        let mut results: BinaryHeap<ReverseDistanceResult> = BinaryHeap::new();
        results.push(ReverseDistanceResult(DistanceResult::new(
            entry, entry_dist,
        )));

        // Visited set
        let mut visited: HashSet<NodeId> = HashSet::new();
        visited.insert(entry);

        while let Some(Reverse(current)) = candidates.pop() {
            // Get furthest result distance for pruning
            let furthest_dist = results.peek().map(|r| r.0.distance).unwrap_or(f32::MAX);

            // If current is further than our furthest result, we're done
            if current.distance > furthest_dist {
                break;
            }

            // Explore neighbors
            if let Some(node) = self.nodes.get(&current.id) {
                if layer < node.connections.len() {
                    for &neighbor_id in &node.connections[layer] {
                        if visited.contains(&neighbor_id) {
                            continue;
                        }
                        visited.insert(neighbor_id);

                        let dist = self.compute_distance(query, neighbor_id);
                        let furthest_dist =
                            results.peek().map(|r| r.0.distance).unwrap_or(f32::MAX);

                        // If this neighbor is closer than our furthest result, or we need more results
                        if dist < furthest_dist || results.len() < ef {
                            candidates.push(Reverse(DistanceResult::new(neighbor_id, dist)));
                            results.push(ReverseDistanceResult(DistanceResult::new(
                                neighbor_id,
                                dist,
                            )));

                            // Prune results to ef size
                            while results.len() > ef {
                                results.pop();
                            }
                        }
                    }
                }
            }
        }

        // Convert results to sorted vector (closest first)
        let mut result_vec: Vec<DistanceResult> = results.into_iter().map(|r| r.0).collect();
        result_vec.sort_by(|a, b| cmp_f32(a.distance, b.distance));
        result_vec
    }

    /// Add a bidirectional connection, pruning if necessary
    fn add_connection(&mut self, from: NodeId, to: NodeId, layer: usize) {
        let max_connections = if layer == 0 {
            self.config.m_max0
        } else {
            self.config.m
        };

        if let Some(node) = self.nodes.get_mut(&from) {
            // Ensure we have enough layers
            while node.connections.len() <= layer {
                node.connections.push(Vec::new());
            }

            // Add connection if not already present
            if !node.connections[layer].contains(&to) {
                node.connections[layer].push(to);

                // Prune if too many connections
                if node.connections[layer].len() > max_connections {
                    self.prune_connections(from, layer, max_connections);
                }
            }
        }
    }

    /// Prune connections to max_connections using simple heuristic
    fn prune_connections(&mut self, node_id: NodeId, layer: usize, max_connections: usize) {
        let node_vector = self.nodes.get(&node_id).map(|n| n.vector.clone());
        let node_vector = match node_vector {
            Some(v) => v,
            None => return,
        };

        // Get all neighbors with distances
        let neighbors: Vec<NodeId> = self
            .nodes
            .get(&node_id)
            .map(|n| n.connections[layer].clone())
            .unwrap_or_default();

        let mut scored: Vec<DistanceResult> = neighbors
            .iter()
            .filter_map(|&neighbor_id| {
                let dist = self.compute_distance(&node_vector, neighbor_id);
                Some(DistanceResult::new(neighbor_id, dist))
            })
            .collect();

        // Sort by distance (closest first)
        scored.sort_by(|a, b| cmp_f32(a.distance, b.distance));

        // Keep only max_connections closest
        let kept: Vec<NodeId> = scored
            .into_iter()
            .take(max_connections)
            .map(|r| r.id)
            .collect();

        if let Some(node) = self.nodes.get_mut(&node_id) {
            node.connections[layer] = kept;
        }
    }

    /// Compute distance between query and a node (SIMD-accelerated)
    fn compute_distance(&self, query: &[f32], node_id: NodeId) -> f32 {
        match self.nodes.get(&node_id) {
            Some(node) => distance_simd(query, &node.vector, self.config.metric),
            None => f32::MAX,
        }
    }

    // ========================================================================
    // Batch Operations
    // ========================================================================

    /// Insert multiple vectors at once
    ///
    /// More efficient than individual inserts for large batches
    pub fn insert_batch(&mut self, vectors: Vec<Vec<f32>>) -> Vec<NodeId> {
        vectors.into_iter().map(|v| self.insert(v)).collect()
    }

    /// Insert multiple vectors with specific IDs
    pub fn insert_batch_with_ids(&mut self, items: Vec<(NodeId, Vec<f32>)>) {
        for (id, vector) in items {
            self.insert_with_id(id, vector);
        }
    }

    // ========================================================================
    // Delete Operations
    // ========================================================================

    /// Remove a node from the index
    ///
    /// Note: This is a "soft" delete - the node is marked as deleted and
    /// excluded from search results, but connections are not fully repaired.
    /// For better performance after many deletes, rebuild the index.
    pub fn delete(&mut self, id: NodeId) -> bool {
        if self.nodes.remove(&id).is_none() {
            return false;
        }

        // Update entry point if we deleted it
        if self.entry_point == Some(id) {
            self.entry_point = self.nodes.keys().next().copied();

            // Update max_layer based on new entry point
            if let Some(ep) = self.entry_point {
                self.max_layer = self.nodes.get(&ep).map(|n| n.max_layer).unwrap_or(0);
            } else {
                self.max_layer = 0;
            }
        }

        // Remove references to this node from other nodes' connections
        for node in self.nodes.values_mut() {
            for layer_connections in node.connections.iter_mut() {
                layer_connections.retain(|&neighbor| neighbor != id);
            }
        }

        true
    }

    /// Check if a node exists
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    // ========================================================================
    // Adaptive Search
    // ========================================================================

    /// Search with adaptive ef based on index size
    ///
    /// Automatically adjusts ef_search based on the number of results needed
    /// and the size of the index for better accuracy/speed tradeoff.
    pub fn search_adaptive(&self, query: &[f32], k: usize) -> Vec<DistanceResult> {
        // Adaptive ef: at least k, scales with log of index size
        let n = self.nodes.len();
        let adaptive_ef = if n < 100 {
            k.max(10)
        } else if n < 10000 {
            k.max(50)
        } else if n < 100000 {
            k.max(100)
        } else {
            k.max(200)
        };

        self.search_with_ef(query, k, adaptive_ef)
    }

    // ========================================================================
    // Index Statistics
    // ========================================================================

    /// Get statistics about the index
    pub fn stats(&self) -> HnswStats {
        let mut layer_counts = vec![0usize; self.max_layer + 1];
        let mut total_connections = 0usize;
        let mut max_connections = 0usize;
        let mut min_connections = usize::MAX;

        for node in self.nodes.values() {
            for layer in 0..=node.max_layer {
                layer_counts[layer] += 1;
                let conns = node.connections[layer].len();
                total_connections += conns;
                max_connections = max_connections.max(conns);
                if conns > 0 {
                    min_connections = min_connections.min(conns);
                }
            }
        }

        if self.nodes.is_empty() {
            min_connections = 0;
        }

        HnswStats {
            node_count: self.nodes.len(),
            dimension: self.dimension,
            max_layer: self.max_layer,
            layer_counts,
            total_connections,
            avg_connections: if self.nodes.is_empty() {
                0.0
            } else {
                total_connections as f64 / self.nodes.len() as f64
            },
            max_connections,
            min_connections,
            entry_point: self.entry_point,
        }
    }

    // ========================================================================
    // Persistence
    // ========================================================================

    /// Serialize the index to bytes for storage
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Magic number and version
        bytes.extend_from_slice(b"HNSW");
        bytes.extend_from_slice(&1u32.to_le_bytes()); // version

        // Config
        bytes.extend_from_slice(&(self.dimension as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.config.m as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.config.m_max0 as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.config.ef_construction as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.config.ef_search as u32).to_le_bytes());
        bytes.extend_from_slice(&self.config.ml.to_le_bytes());
        bytes.push(match self.config.metric {
            DistanceMetric::L2 => 0,
            DistanceMetric::Cosine => 1,
            DistanceMetric::InnerProduct => 2,
        });

        // Index state
        bytes.extend_from_slice(&(self.max_layer as u32).to_le_bytes());
        bytes.extend_from_slice(&self.entry_point.unwrap_or(u64::MAX).to_le_bytes());

        // Node count
        bytes.extend_from_slice(&(self.nodes.len() as u64).to_le_bytes());

        // Nodes
        for (&id, node) in &self.nodes {
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.extend_from_slice(&(node.max_layer as u32).to_le_bytes());

            // Vector
            for &val in &node.vector {
                bytes.extend_from_slice(&val.to_le_bytes());
            }

            // Connections per layer
            for layer in 0..=node.max_layer {
                let conns = &node.connections[layer];
                bytes.extend_from_slice(&(conns.len() as u32).to_le_bytes());
                for &conn in conns {
                    bytes.extend_from_slice(&conn.to_le_bytes());
                }
            }
        }

        bytes
    }

    /// Deserialize index from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 8 {
            return Err("Data too short".to_string());
        }

        // Check magic number
        if &bytes[0..4] != b"HNSW" {
            return Err("Invalid magic number".to_string());
        }

        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if version != 1 {
            return Err(format!("Unsupported version: {}", version));
        }

        let mut pos = 8;

        // Config
        let dimension = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let m = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let m_max0 = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let ef_construction = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let ef_search = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let ml = f64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let metric = match bytes[pos] {
            0 => DistanceMetric::L2,
            1 => DistanceMetric::Cosine,
            2 => DistanceMetric::InnerProduct,
            _ => return Err("Invalid distance metric".to_string()),
        };
        pos += 1;

        let config = HnswConfig {
            m,
            m_max0,
            ef_construction,
            ef_search,
            ml,
            metric,
        };

        // Index state
        let max_layer = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let ep_value = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let entry_point = if ep_value == u64::MAX {
            None
        } else {
            Some(ep_value)
        };

        // Node count
        let node_count = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap()) as usize;
        pos += 8;

        let mut nodes = HashMap::new();
        let mut max_id = 0u64;

        for _ in 0..node_count {
            let id = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;
            max_id = max_id.max(id);

            let level = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;

            let mut vector = Vec::with_capacity(dimension);
            for _ in 0..dimension {
                vector.push(f32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()));
                pos += 4;
            }

            let mut connections = vec![Vec::new(); level + 1];
            for layer in 0..=level {
                let conn_count =
                    u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;

                for _ in 0..conn_count {
                    let conn = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
                    pos += 8;
                    connections[layer].push(conn);
                }
            }

            nodes.insert(
                id,
                HnswNode {
                    id,
                    max_layer,
                    vector,
                    connections,
                },
            );
        }

        Ok(Self {
            config,
            nodes,
            entry_point,
            max_layer,
            dimension,
            next_id: AtomicU64::new(max_id + 1),
            rng_state: 12345, // Reset RNG
        })
    }
}

/// Statistics about an HNSW index
#[derive(Debug, Clone)]
pub struct HnswStats {
    /// Number of vectors in the index
    pub node_count: usize,
    /// Vector dimension
    pub dimension: usize,
    /// Maximum layer in the graph
    pub max_layer: usize,
    /// Number of nodes per layer
    pub layer_counts: Vec<usize>,
    /// Total number of connections
    pub total_connections: usize,
    /// Average connections per node
    pub avg_connections: f64,
    /// Maximum connections on any node
    pub max_connections: usize,
    /// Minimum connections on any node
    pub min_connections: usize,
    /// Entry point node ID
    pub entry_point: Option<NodeId>,
}

/// Bitset for efficient filtering
#[derive(Debug, Clone)]
pub struct Bitset {
    bits: Vec<u64>,
    len: usize,
}

impl Bitset {
    /// Create a new bitset with capacity for n elements
    pub fn with_capacity(n: usize) -> Self {
        let num_words = (n + 63) / 64;
        Self {
            bits: vec![0; num_words],
            len: n,
        }
    }

    /// Create a bitset with all bits set
    pub fn all(n: usize) -> Self {
        let num_words = (n + 63) / 64;
        let mut bits = vec![u64::MAX; num_words];

        // Clear excess bits in last word
        if n % 64 != 0 {
            let last_idx = num_words - 1;
            let valid_bits = n % 64;
            bits[last_idx] = (1u64 << valid_bits) - 1;
        }

        Self { bits, len: n }
    }

    /// Set a bit
    pub fn set(&mut self, idx: usize) {
        if idx < self.len {
            let word = idx / 64;
            let bit = idx % 64;
            self.bits[word] |= 1u64 << bit;
        }
    }

    /// Clear a bit
    pub fn clear(&mut self, idx: usize) {
        if idx < self.len {
            let word = idx / 64;
            let bit = idx % 64;
            self.bits[word] &= !(1u64 << bit);
        }
    }

    /// Check if a bit is set
    pub fn is_set(&self, idx: usize) -> bool {
        if idx >= self.len {
            return false;
        }
        let word = idx / 64;
        let bit = idx % 64;
        (self.bits[word] & (1u64 << bit)) != 0
    }

    /// Convert to HashSet for use with HNSW filter
    pub fn to_hashset(&self) -> HashSet<NodeId> {
        let mut set = HashSet::new();
        for i in 0..self.len {
            if self.is_set(i) {
                set.insert(i as NodeId);
            }
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vector(dim: usize, seed: u64) -> Vec<f32> {
        let mut state = seed;
        (0..dim)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state as f32) / (u64::MAX as f32)
            })
            .collect()
    }

    #[test]
    fn test_empty_index() {
        let index = HnswIndex::with_dimension(128);
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);

        let results = index.search(&vec![0.0; 128], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_single_insert() {
        let mut index = HnswIndex::with_dimension(3);
        let id = index.insert(vec![1.0, 2.0, 3.0]);

        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
        assert!(index.get_vector(id).is_some());
    }

    #[test]
    fn test_exact_match() {
        let mut index = HnswIndex::with_dimension(3);
        index.insert(vec![1.0, 0.0, 0.0]);
        index.insert(vec![0.0, 1.0, 0.0]);
        index.insert(vec![0.0, 0.0, 1.0]);

        // Search for exact match
        let results = index.search(&vec![1.0, 0.0, 0.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].distance, 0.0);
    }

    #[test]
    fn test_nearest_neighbor() {
        let mut index = HnswIndex::with_dimension(2);
        index.insert_with_id(0, vec![0.0, 0.0]);
        index.insert_with_id(1, vec![1.0, 0.0]);
        index.insert_with_id(2, vec![2.0, 0.0]);
        index.insert_with_id(3, vec![3.0, 0.0]);

        // Search for something close to (0.9, 0.0)
        let results = index.search(&vec![0.9, 0.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 1); // Should find (1.0, 0.0)
    }

    #[test]
    fn test_k_nearest() {
        let mut index = HnswIndex::with_dimension(2);
        for i in 0..10 {
            index.insert_with_id(i, vec![i as f32, 0.0]);
        }

        // Search for 3 nearest to (4.5, 0.0)
        let results = index.search(&vec![4.5, 0.0], 3);
        assert_eq!(results.len(), 3);

        // Should find 4, 5, and either 3 or 6
        let ids: HashSet<_> = results.iter().map(|r| r.id).collect();
        assert!(ids.contains(&4));
        assert!(ids.contains(&5));
    }

    #[test]
    fn test_filtered_search() {
        let mut index = HnswIndex::with_dimension(2);
        for i in 0..10 {
            index.insert_with_id(i, vec![i as f32, 0.0]);
        }

        // Only allow even IDs
        let filter: HashSet<NodeId> = [0, 2, 4, 6, 8].iter().copied().collect();

        // Search for nearest to (5.0, 0.0) with filter
        let results = index.search_filtered(&vec![5.0, 0.0], 2, &filter);

        // Should find 4 and 6 (closest even numbers to 5)
        assert_eq!(results.len(), 2);
        let ids: HashSet<_> = results.iter().map(|r| r.id).collect();
        assert!(ids.contains(&4));
        assert!(ids.contains(&6));
    }

    #[test]
    fn test_cosine_distance() {
        let config = HnswConfig::default().with_metric(DistanceMetric::Cosine);
        let mut index = HnswIndex::new(3, config);

        // Insert normalized vectors
        index.insert_with_id(0, vec![1.0, 0.0, 0.0]);
        index.insert_with_id(1, vec![0.0, 1.0, 0.0]);
        index.insert_with_id(2, vec![0.707, 0.707, 0.0]); // 45 degrees

        // Search for something at 45 degrees
        let results = index.search(&vec![0.707, 0.707, 0.0], 1);
        assert_eq!(results[0].id, 2);
    }

    #[test]
    fn test_many_vectors() {
        let dim = 64;
        let n: usize = 1000;

        let mut index = HnswIndex::with_dimension(dim);

        // Insert many random vectors
        for i in 0..n {
            let vector = random_vector(dim, i as u64);
            index.insert_with_id(i as u64, vector);
        }

        assert_eq!(index.len(), n);

        // Search should return k results
        let query = random_vector(dim, 12345);
        let results = index.search(&query, 10);
        assert_eq!(results.len(), 10);

        // Results should be sorted by distance
        for i in 1..results.len() {
            assert!(results[i].distance >= results[i - 1].distance);
        }
    }

    #[test]
    fn test_bitset() {
        let mut bs = Bitset::with_capacity(100);

        bs.set(0);
        bs.set(50);
        bs.set(99);

        assert!(bs.is_set(0));
        assert!(bs.is_set(50));
        assert!(bs.is_set(99));
        assert!(!bs.is_set(1));
        assert!(!bs.is_set(64));

        bs.clear(50);
        assert!(!bs.is_set(50));
    }

    #[test]
    fn test_bitset_all() {
        let bs = Bitset::all(100);

        for i in 0..100 {
            assert!(bs.is_set(i));
        }
        assert!(!bs.is_set(100)); // Out of bounds
    }
}
