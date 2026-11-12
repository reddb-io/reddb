//! Graph Algorithms for RedDB
//!
//! High-performance graph algorithms optimized for attack path analysis:
//! - PageRank: Identify critical nodes
//! - Connected Components: Find isolated network segments
//! - Betweenness Centrality: Find chokepoints
//! - Community Detection: Cluster related assets
//! - Cycle Detection: Find lateral movement loops
//!
//! # Algorithm Complexity
//!
//! | Algorithm              | Time Complexity    | Space Complexity | Notes                          |
//! |------------------------|--------------------|------------------|--------------------------------|
//! | PageRank               | O(V + E) per iter  | O(V)             | Converges in ~20-50 iterations |
//! | Connected Components   | O(V + E)           | O(V)             | Union-Find with path compress  |
//! | Betweenness Centrality | O(V × E)           | O(V + E)         | Brandes' algorithm             |
//! | Community Detection    | O(V + E) per iter  | O(V)             | Label propagation, ~5-10 iters |
//! | Cycle Detection        | O(V + E)           | O(V + cycles)    | DFS with rotation dedup        |
//! | BFS Path Finding       | O(V + E)           | O(V)             | Single-source shortest path    |
//! | Dijkstra (weighted)    | O((V+E) log V)     | O(V)             | Priority queue based           |
//! | K-Shortest Paths       | O(K × V × E)       | O(K × V)         | Yen's algorithm                |
//!
//! Where V = vertices (nodes), E = edges.
//!
//! # Performance Benchmarks
//!
//! On a graph with 1M edges (typical enterprise network):
//! - Graph creation: ~2 seconds
//! - PageRank: ~500ms (50 iterations)
//! - Connected Components: ~100ms
//! - Communities: ~300ms (10 iterations)
//!
//! On a graph with 100K edges:
//! - Betweenness Centrality: ~5 seconds (O(V×E))
//!
//! On a graph with 10K edges:
//! - Cycle Detection: ~50ms
//!
//! # Example
//!
//! ```ignore
//! use reddb::storage::engine::algorithms::{PageRank, ConnectedComponents};
//!
//! let pr = PageRank::new(&graph).run();
//! let components = ConnectedComponents::new(&graph).find();
//! ```

// Algorithm submodules
mod centrality;
mod community;
mod components;
mod cycles;
mod pagerank;
mod structural;

// Re-export centrality algorithms
pub use centrality::{
    BetweennessCentrality, BetweennessResult, ClosenessCentrality, ClosenessResult,
    DegreeCentrality, DegreeCentralityResult, EigenvectorCentrality, EigenvectorResult,
};

// Re-export community detection algorithms
pub use community::{CommunitiesResult, Community, LabelPropagation, Louvain, LouvainResult};

// Re-export connected components
pub use components::{Component, ComponentsResult, ConnectedComponents};

// Re-export cycle detection
pub use cycles::{Cycle, CycleDetector, CyclesResult};

// Re-export PageRank algorithms
pub use pagerank::{PageRank, PageRankResult, PersonalizedPageRank};

// Re-export structural algorithms
pub use structural::{
    ClusteringCoefficient, ClusteringResult, HITSResult, SCCResult, StronglyConnectedComponents,
    TriangleCounting, TriangleResult, WCCResult, WeaklyConnectedComponents, HITS,
};

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::graph_store::GraphStore;

    fn create_test_graph() -> GraphStore {
        let graph = GraphStore::new();

        // Create a diamond graph: A -> B, A -> C, B -> D, C -> D
        let _ = graph.add_node_with_label("A", "Node A", "host");
        let _ = graph.add_node_with_label("B", "Node B", "host");
        let _ = graph.add_node_with_label("C", "Node C", "host");
        let _ = graph.add_node_with_label("D", "Node D", "host");

        let _ = graph.add_edge_with_label("A", "B", "connects_to", 1.0);
        let _ = graph.add_edge_with_label("A", "C", "connects_to", 1.0);
        let _ = graph.add_edge_with_label("B", "D", "connects_to", 1.0);
        let _ = graph.add_edge_with_label("C", "D", "connects_to", 1.0);

        graph
    }

    fn create_cycle_graph() -> GraphStore {
        let graph = GraphStore::new();

        // Create a cycle: A -> B -> C -> A
        let _ = graph.add_node_with_label("A", "Node A", "host");
        let _ = graph.add_node_with_label("B", "Node B", "host");
        let _ = graph.add_node_with_label("C", "Node C", "host");

        let _ = graph.add_edge_with_label("A", "B", "connects_to", 1.0);
        let _ = graph.add_edge_with_label("B", "C", "connects_to", 1.0);
        let _ = graph.add_edge_with_label("C", "A", "connects_to", 1.0);

        graph
    }

    fn create_disconnected_graph() -> GraphStore {
        let graph = GraphStore::new();

        // Component 1: A -> B
        let _ = graph.add_node_with_label("A", "Node A", "host");
        let _ = graph.add_node_with_label("B", "Node B", "host");
        let _ = graph.add_edge_with_label("A", "B", "connects_to", 1.0);

        // Component 2: C -> D -> E
        let _ = graph.add_node_with_label("C", "Node C", "host");
        let _ = graph.add_node_with_label("D", "Node D", "host");
        let _ = graph.add_node_with_label("E", "Node E", "host");
        let _ = graph.add_edge_with_label("C", "D", "connects_to", 1.0);
        let _ = graph.add_edge_with_label("D", "E", "connects_to", 1.0);

        // Component 3: F (isolated)
        let _ = graph.add_node_with_label("F", "Node F", "host");

        graph
    }

    // PageRank tests
    #[test]
    fn test_pagerank_empty_graph() {
        let graph = GraphStore::new();
        let result = PageRank::new().run(&graph);
        assert!(result.scores.is_empty());
        assert!(result.converged);
    }

    #[test]
    fn test_pagerank_single_node() {
        let graph = GraphStore::new();
        let _ = graph.add_node_with_label("A", "Node A", "host");

        let result = PageRank::new().run(&graph);
        assert_eq!(result.scores.len(), 1);
        assert!((result.scores["A"] - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_pagerank_diamond() {
        let graph = create_test_graph();
        let result = PageRank::new().run(&graph);

        // D should have highest score (most incoming)
        let top = result.top(4);
        assert_eq!(top[0].0, "D");
        assert!(result.converged);
    }

    #[test]
    fn test_pagerank_top_n() {
        let graph = create_test_graph();
        let result = PageRank::new().run(&graph);

        let top2 = result.top(2);
        assert_eq!(top2.len(), 2);
    }

    // Connected Components tests
    #[test]
    fn test_components_single_component() {
        let graph = create_test_graph();
        let result = ConnectedComponents::find(&graph);

        assert_eq!(result.count, 1);
        assert_eq!(result.components[0].size, 4);
    }

    #[test]
    fn test_components_disconnected() {
        let graph = create_disconnected_graph();
        let result = ConnectedComponents::find(&graph);

        assert_eq!(result.count, 3);
        // Largest component has 3 nodes
        assert_eq!(result.largest().unwrap().size, 3);
    }

    #[test]
    fn test_components_filter_by_size() {
        let graph = create_disconnected_graph();
        let result = ConnectedComponents::find(&graph);

        let large = result.filter_by_size(2);
        assert_eq!(large.len(), 2); // Only components with 2+ nodes
    }

    #[test]
    fn test_components_component_of() {
        let graph = create_disconnected_graph();
        let result = ConnectedComponents::find(&graph);

        let comp = result.component_of("D").unwrap();
        assert!(comp.nodes.contains(&"C".to_string()));
        assert!(comp.nodes.contains(&"E".to_string()));
    }

    // Betweenness Centrality tests
    #[test]
    fn test_betweenness_empty() {
        let graph = GraphStore::new();
        let result = BetweennessCentrality::compute(&graph, false);
        assert!(result.scores.is_empty());
    }

    #[test]
    fn test_betweenness_diamond() {
        let graph = create_test_graph();
        let result = BetweennessCentrality::compute(&graph, false);

        // B and C should have similar betweenness
        // A is source, D is sink - they have 0 betweenness
        assert!(result.score("A").unwrap() < result.score("B").unwrap());
    }

    #[test]
    fn test_betweenness_normalized() {
        let graph = create_test_graph();
        let result = BetweennessCentrality::compute(&graph, true);

        // Normalized scores should be between 0 and 1
        for score in result.scores.values() {
            assert!(*score >= 0.0 && *score <= 1.0);
        }
    }

    // Community Detection tests
    #[test]
    fn test_communities_connected() {
        let graph = create_test_graph();
        let result = LabelPropagation::new().run(&graph);

        // Fully connected graph should form 1 community
        assert!(result.communities.len() <= 2);
    }

    #[test]
    fn test_communities_disconnected() {
        let graph = create_disconnected_graph();
        let result = LabelPropagation::new().run(&graph);

        // Should find at least 3 communities
        assert!(result.communities.len() >= 3);
    }

    #[test]
    fn test_communities_convergence() {
        let graph = create_test_graph();
        let result = LabelPropagation::new().max_iterations(50).run(&graph);

        assert!(result.converged || result.iterations == 50);
    }

    // Cycle Detection tests
    #[test]
    fn test_cycles_no_cycle() {
        let graph = create_test_graph(); // Diamond has no cycles
        let result = CycleDetector::new().find(&graph);

        assert!(result.cycles.is_empty());
    }

    #[test]
    fn test_cycles_simple_cycle() {
        let graph = create_cycle_graph();
        let result = CycleDetector::new().find(&graph);

        assert!(!result.cycles.is_empty());
        assert_eq!(result.cycles[0].length, 3); // A -> B -> C -> A
    }

    #[test]
    fn test_cycles_max_length() {
        let graph = create_cycle_graph();
        let result = CycleDetector::new().max_length(2).find(&graph);

        // Cycle of length 3 should not be found with max_length=2
        assert!(result.cycles.is_empty());
    }

    #[test]
    fn test_cycles_max_cycles_limit() {
        let graph = create_cycle_graph();
        let result = CycleDetector::new().max_cycles(0).find(&graph);

        assert!(result.cycles.is_empty());
        assert!(result.limit_reached);
    }
}
