//! Connected Components Algorithms
//!
//! Find connected components in graphs using Union-Find with path compression.
//! Useful for identifying isolated network segments.

use std::collections::HashMap;

use super::super::graph_store::GraphStore;

// ============================================================================
// Union-Find Data Structure
// ============================================================================

/// Union-Find data structure with path compression
pub(crate) struct UnionFind {
    parent: HashMap<String, String>,
    rank: HashMap<String, usize>,
}

impl UnionFind {
    pub fn new() -> Self {
        Self {
            parent: HashMap::new(),
            rank: HashMap::new(),
        }
    }

    pub fn make_set(&mut self, x: &str) {
        if !self.parent.contains_key(x) {
            self.parent.insert(x.to_string(), x.to_string());
            self.rank.insert(x.to_string(), 0);
        }
    }

    pub fn find(&mut self, x: &str) -> String {
        let parent = self.parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if parent != x {
            // Path compression
            let root = self.find(&parent);
            self.parent.insert(x.to_string(), root.clone());
            root
        } else {
            x.to_string()
        }
    }

    pub fn union(&mut self, x: &str, y: &str) {
        let root_x = self.find(x);
        let root_y = self.find(y);

        if root_x == root_y {
            return;
        }

        let rank_x = *self.rank.get(&root_x).unwrap_or(&0);
        let rank_y = *self.rank.get(&root_y).unwrap_or(&0);

        // Union by rank
        if rank_x < rank_y {
            self.parent.insert(root_x, root_y);
        } else if rank_x > rank_y {
            self.parent.insert(root_y, root_x);
        } else {
            self.parent.insert(root_y, root_x.clone());
            self.rank.insert(root_x, rank_x + 1);
        }
    }
}

// ============================================================================
// Connected Components
// ============================================================================

/// Connected components finder
pub struct ConnectedComponents;

/// A connected component in the graph
#[derive(Debug, Clone)]
pub struct Component {
    /// Component ID (representative node)
    pub id: String,
    /// Nodes in this component
    pub nodes: Vec<String>,
    /// Size of the component
    pub size: usize,
}

/// Result of connected components computation
#[derive(Debug, Clone)]
pub struct ComponentsResult {
    /// List of components, sorted by size descending
    pub components: Vec<Component>,
    /// Total number of components
    pub count: usize,
}

impl ComponentsResult {
    /// Get the largest component
    pub fn largest(&self) -> Option<&Component> {
        self.components.first()
    }

    /// Get components with at least min_size nodes
    pub fn filter_by_size(&self, min_size: usize) -> Vec<&Component> {
        self.components
            .iter()
            .filter(|c| c.size >= min_size)
            .collect()
    }

    /// Find which component a node belongs to
    pub fn component_of(&self, node_id: &str) -> Option<&Component> {
        self.components
            .iter()
            .find(|c| c.nodes.contains(&node_id.to_string()))
    }
}

impl ConnectedComponents {
    /// Find all connected components in the graph (treating edges as undirected)
    pub fn find(graph: &GraphStore) -> ComponentsResult {
        let mut uf = UnionFind::new();

        // Add all nodes
        for node in graph.iter_nodes() {
            uf.make_set(&node.id);
        }

        // Union nodes connected by edges (both directions)
        for node in graph.iter_nodes() {
            for (_, target, _) in graph.outgoing_edges(&node.id) {
                uf.union(&node.id, &target);
            }
        }

        // Group nodes by their root
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for node in graph.iter_nodes() {
            let root = uf.find(&node.id);
            groups.entry(root).or_default().push(node.id.clone());
        }

        // Build components
        let mut components: Vec<Component> = groups
            .into_iter()
            .map(|(id, nodes)| {
                let size = nodes.len();
                Component { id, nodes, size }
            })
            .collect();

        // Sort by size descending
        components.sort_by(|a, b| b.size.cmp(&a.size));

        let count = components.len();
        ComponentsResult { components, count }
    }
}
