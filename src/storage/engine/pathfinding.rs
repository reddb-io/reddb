//! Path Finding Algorithms for RedDB
//!
//! Shortest path and graph traversal algorithms optimized for attack path analysis:
//! - BFS: Unweighted shortest paths
//! - DFS: Deep exploration with backtracking
//! - Dijkstra: Weighted shortest paths (non-negative weights)
//! - A*: Heuristic-guided shortest paths
//! - Bellman-Ford: Handles negative weights
//! - All Shortest Paths: Find all minimum-length paths
//! - K-Shortest Paths: Find k best paths (Yen's algorithm)
//!
//! # Security Use Cases
//!
//! - **Attack Path Analysis**: Find shortest exploitation paths
//! - **Lateral Movement**: Discover movement paths between hosts
//! - **Privilege Escalation**: Map paths to high-value targets
//! - **Risk Assessment**: Weight paths by exploit difficulty

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use super::graph_store::{GraphEdgeType, GraphStore};

// ============================================================================
// Path Result Types
// ============================================================================

/// A path through the graph
#[derive(Debug, Clone)]
pub struct Path {
    /// Ordered list of node IDs from source to target
    pub nodes: Vec<String>,
    /// Total weight/cost of the path
    pub total_weight: f64,
    /// Edge types along the path
    pub edge_types: Vec<GraphEdgeType>,
}

impl Path {
    /// Create a path with a single node (start)
    pub fn start(node: &str) -> Self {
        Self {
            nodes: vec![node.to_string()],
            total_weight: 0.0,
            edge_types: Vec::new(),
        }
    }

    /// Extend path with a new node
    pub fn extend(&self, node: &str, edge_type: GraphEdgeType, weight: f64) -> Self {
        let mut nodes = self.nodes.clone();
        nodes.push(node.to_string());
        let mut edge_types = self.edge_types.clone();
        edge_types.push(edge_type);
        Self {
            nodes,
            total_weight: self.total_weight + weight,
            edge_types,
        }
    }

    /// Path length (number of edges)
    pub fn len(&self) -> usize {
        self.nodes.len().saturating_sub(1)
    }

    /// Check if path is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Get source node
    pub fn source(&self) -> Option<&str> {
        self.nodes.first().map(|s| s.as_str())
    }

    /// Get target node
    pub fn target(&self) -> Option<&str> {
        self.nodes.last().map(|s| s.as_str())
    }
}

/// Result of a shortest path query
#[derive(Debug, Clone)]
pub struct ShortestPathResult {
    /// The shortest path (None if no path exists)
    pub path: Option<Path>,
    /// Number of nodes visited during search
    pub nodes_visited: usize,
}

impl ShortestPathResult {
    /// Check if a path was found
    pub fn found(&self) -> bool {
        self.path.is_some()
    }

    /// Get the path length
    pub fn distance(&self) -> Option<usize> {
        self.path.as_ref().map(|p| p.len())
    }

    /// Get the total weight
    pub fn total_weight(&self) -> Option<f64> {
        self.path.as_ref().map(|p| p.total_weight)
    }
}

/// Result of all shortest paths query
#[derive(Debug, Clone)]
pub struct AllPathsResult {
    /// All shortest paths found
    pub paths: Vec<Path>,
    /// Number of nodes visited during search
    pub nodes_visited: usize,
}

// ============================================================================
// BFS - Breadth-First Search
// ============================================================================

/// Breadth-First Search for unweighted shortest paths
///
/// Use when all edges have equal weight (or you only care about hop count).
/// Time: O(V + E), Space: O(V)
pub struct BFS;

mod bfs_impl;

// ============================================================================
// DFS - Depth-First Search
// ============================================================================

/// Depth-First Search for deep graph exploration
///
/// Use for finding paths, detecting cycles, topological sorting.
/// Time: O(V + E), Space: O(V)
pub struct DFS;

mod dfs_impl;
// ============================================================================
// Dijkstra - Weighted Shortest Paths
// ============================================================================

/// State for Dijkstra's priority queue
#[derive(Clone)]
struct DijkstraState {
    node: String,
    cost: f64,
    path: Path,
}

impl Eq for DijkstraState {}

impl PartialEq for DijkstraState {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node && (self.cost - other.cost).abs() < f64::EPSILON
    }
}

impl Ord for DijkstraState {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering for min-heap
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for DijkstraState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Dijkstra's Algorithm for weighted shortest paths
///
/// Use when edges have non-negative weights (e.g., exploit difficulty).
/// Time: O((V + E) log V), Space: O(V)
pub struct Dijkstra;

mod dijkstra_impl;

// ============================================================================
// A* - Heuristic Shortest Paths
// ============================================================================

/// A* state for priority queue
#[derive(Clone)]
struct AStarState {
    node: String,
    g_cost: f64, // Actual cost from start
    f_cost: f64, // g_cost + heuristic
    path: Path,
}

impl Eq for AStarState {}

impl PartialEq for AStarState {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node && (self.f_cost - other.f_cost).abs() < f64::EPSILON
    }
}

impl Ord for AStarState {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .f_cost
            .partial_cmp(&self.f_cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for AStarState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A* Algorithm for heuristic-guided shortest paths
///
/// Use when you have a heuristic estimate of distance to target.
/// Faster than Dijkstra when heuristic is good.
/// Time: O((V + E) log V) typical, Space: O(V)
pub struct AStar;

mod astar_impl;

// ============================================================================
// Bellman-Ford - Handles Negative Weights
// ============================================================================

/// Result of Bellman-Ford algorithm
#[derive(Debug, Clone)]
pub struct BellmanFordResult {
    /// Shortest path to target (None if no path or negative cycle)
    pub path: Option<Path>,
    /// Distances from source to all reachable nodes
    pub distances: HashMap<String, f64>,
    /// Whether a negative cycle was detected
    pub has_negative_cycle: bool,
    /// Nodes visited during computation
    pub nodes_visited: usize,
}

/// Bellman-Ford Algorithm for graphs with negative weights
///
/// Use when edges can have negative weights.
/// Also detects negative cycles.
/// Time: O(V * E), Space: O(V)
pub struct BellmanFord;

mod bellman_ford_impl;

// ============================================================================
// K-Shortest Paths (Yen's Algorithm)
// ============================================================================

/// K-Shortest Paths using Yen's Algorithm
///
/// Find the k shortest loopless paths from source to target.
/// Useful for finding alternative attack paths.
/// Time: O(k * V * (V + E) log V), Space: O(k * V)
pub struct KShortestPaths;

mod k_shortest_impl;

/// Candidate path for k-shortest paths
struct PathCandidate {
    path: Path,
}

impl Eq for PathCandidate {}

impl PartialEq for PathCandidate {
    fn eq(&self, other: &Self) -> bool {
        (self.path.total_weight - other.path.total_weight).abs() < f64::EPSILON
    }
}

impl Ord for PathCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .path
            .total_weight
            .partial_cmp(&self.path.total_weight)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for PathCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ============================================================================
// All Shortest Paths
// ============================================================================

/// Find all shortest paths (same minimum length) between two nodes
pub struct AllShortestPaths;

mod all_shortest_impl;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::graph_store::GraphNodeType;

    fn create_simple_graph() -> GraphStore {
        let graph = GraphStore::new();

        // A -> B -> C
        // |    |
        // v    v
        // D -> E
        graph.add_node("A", "Node A", GraphNodeType::Host);
        graph.add_node("B", "Node B", GraphNodeType::Host);
        graph.add_node("C", "Node C", GraphNodeType::Host);
        graph.add_node("D", "Node D", GraphNodeType::Host);
        graph.add_node("E", "Node E", GraphNodeType::Host);

        graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("B", "C", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("A", "D", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("B", "E", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("D", "E", GraphEdgeType::ConnectsTo, 1.0);

        graph
    }

    fn create_weighted_graph() -> GraphStore {
        let graph = GraphStore::new();

        // A --(1)--> B --(2)--> D
        // |          ^
        // (4)        (1)
        // v          |
        // C --(1)--> E
        graph.add_node("A", "Node A", GraphNodeType::Host);
        graph.add_node("B", "Node B", GraphNodeType::Host);
        graph.add_node("C", "Node C", GraphNodeType::Host);
        graph.add_node("D", "Node D", GraphNodeType::Host);
        graph.add_node("E", "Node E", GraphNodeType::Host);

        graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("A", "C", GraphEdgeType::ConnectsTo, 4.0);
        graph.add_edge("B", "D", GraphEdgeType::ConnectsTo, 2.0);
        graph.add_edge("C", "E", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("E", "B", GraphEdgeType::ConnectsTo, 1.0);

        graph
    }

    // BFS Tests
    #[test]
    fn test_bfs_shortest_path() {
        let graph = create_simple_graph();
        let result = BFS::shortest_path(&graph, "A", "C");

        assert!(result.found());
        assert_eq!(result.distance(), Some(2)); // A -> B -> C
    }

    #[test]
    fn test_bfs_same_source_target() {
        let graph = create_simple_graph();
        let result = BFS::shortest_path(&graph, "A", "A");

        assert!(result.found());
        assert_eq!(result.distance(), Some(0));
    }

    #[test]
    fn test_bfs_no_path() {
        let graph = GraphStore::new();
        graph.add_node("A", "A", GraphNodeType::Host);
        graph.add_node("B", "B", GraphNodeType::Host);
        // No edge between A and B

        let result = BFS::shortest_path(&graph, "A", "B");
        assert!(!result.found());
    }

    #[test]
    fn test_bfs_reachable() {
        let graph = create_simple_graph();
        let reachable = BFS::reachable(&graph, "A", 2);

        assert!(reachable.iter().any(|(n, _)| n == "A"));
        assert!(reachable.iter().any(|(n, _)| n == "B"));
        assert!(reachable.iter().any(|(n, _)| n == "D"));
    }

    // DFS Tests
    #[test]
    fn test_dfs_find_path() {
        let graph = create_simple_graph();
        let result = DFS::find_path(&graph, "A", "E");

        assert!(result.found());
        // Path exists (either A->B->E or A->D->E)
    }

    #[test]
    fn test_dfs_all_paths() {
        let graph = create_simple_graph();
        let result = DFS::all_paths(&graph, "A", "E", 3);

        assert!(!result.paths.is_empty());
        // Should find both A->B->E and A->D->E
        assert!(result.paths.len() >= 2);
    }

    #[test]
    fn test_dfs_topological_sort() {
        let graph = GraphStore::new();
        graph.add_node("A", "A", GraphNodeType::Host);
        graph.add_node("B", "B", GraphNodeType::Host);
        graph.add_node("C", "C", GraphNodeType::Host);
        graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("B", "C", GraphEdgeType::ConnectsTo, 1.0);

        let result = DFS::topological_sort(&graph);
        assert!(result.is_some());

        let order = result.unwrap();
        let a_pos = order.iter().position(|n| n == "A").unwrap();
        let b_pos = order.iter().position(|n| n == "B").unwrap();
        let c_pos = order.iter().position(|n| n == "C").unwrap();
        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }

    // Dijkstra Tests
    #[test]
    fn test_dijkstra_weighted() {
        let graph = create_weighted_graph();
        let result = Dijkstra::shortest_path(&graph, "A", "D");

        assert!(result.found());
        // Shortest path should be A->B->D (weight 3) not A->C->E->B->D (weight 7)
        assert_eq!(result.total_weight(), Some(3.0));
    }

    #[test]
    fn test_dijkstra_all_paths() {
        let graph = create_weighted_graph();
        let paths = Dijkstra::shortest_paths_from(&graph, "A");

        assert!(paths.contains_key("A"));
        assert!(paths.contains_key("B"));
        assert!(paths.contains_key("D"));
    }

    // A* Tests
    #[test]
    fn test_astar_zero_heuristic() {
        let graph = create_weighted_graph();
        let result = AStar::shortest_path_no_heuristic(&graph, "A", "D");

        // Should match Dijkstra result
        assert!(result.found());
        assert_eq!(result.total_weight(), Some(3.0));
    }

    // Bellman-Ford Tests
    #[test]
    fn test_bellman_ford_positive() {
        let graph = create_weighted_graph();
        let result = BellmanFord::shortest_path(&graph, "A", "D");

        assert!(!result.has_negative_cycle);
        assert!(result.path.is_some());
    }

    // K-Shortest Paths Tests
    #[test]
    fn test_k_shortest_paths() {
        let graph = create_simple_graph();
        let paths = KShortestPaths::find(&graph, "A", "E", 2);

        assert!(!paths.is_empty());
        // Should find at least 2 different paths to E
    }

    // All Shortest Paths Tests
    #[test]
    fn test_all_shortest_paths() {
        let graph = create_simple_graph();
        let result = AllShortestPaths::find(&graph, "A", "E");

        // Both A->B->E and A->D->E have length 2
        assert!(result.paths.len() >= 2);
        for path in &result.paths {
            assert_eq!(path.len(), 2);
        }
    }
}
