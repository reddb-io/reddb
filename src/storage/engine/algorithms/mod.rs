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
    use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType, GraphStore};

    fn create_test_graph() -> GraphStore {
        let graph = GraphStore::new();

        // Create a diamond graph: A -> B, A -> C, B -> D, C -> D
        let _ = graph.add_node("A", "Node A", GraphNodeType::Host);
        let _ = graph.add_node("B", "Node B", GraphNodeType::Host);
        let _ = graph.add_node("C", "Node C", GraphNodeType::Host);
        let _ = graph.add_node("D", "Node D", GraphNodeType::Host);

        let _ = graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("A", "C", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("B", "D", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("C", "D", GraphEdgeType::ConnectsTo, 1.0);

        graph
    }

    fn create_cycle_graph() -> GraphStore {
        let graph = GraphStore::new();

        // Create a cycle: A -> B -> C -> A
        let _ = graph.add_node("A", "Node A", GraphNodeType::Host);
        let _ = graph.add_node("B", "Node B", GraphNodeType::Host);
        let _ = graph.add_node("C", "Node C", GraphNodeType::Host);

        let _ = graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("B", "C", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("C", "A", GraphEdgeType::ConnectsTo, 1.0);

        graph
    }

    fn create_disconnected_graph() -> GraphStore {
        let graph = GraphStore::new();

        // Component 1: A -> B
        let _ = graph.add_node("A", "Node A", GraphNodeType::Host);
        let _ = graph.add_node("B", "Node B", GraphNodeType::Host);
        let _ = graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);

        // Component 2: C -> D -> E
        let _ = graph.add_node("C", "Node C", GraphNodeType::Host);
        let _ = graph.add_node("D", "Node D", GraphNodeType::Host);
        let _ = graph.add_node("E", "Node E", GraphNodeType::Host);
        let _ = graph.add_edge("C", "D", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("D", "E", GraphEdgeType::ConnectsTo, 1.0);

        // Component 3: F (isolated)
        let _ = graph.add_node("F", "Node F", GraphNodeType::Host);

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
        let _ = graph.add_node("A", "Node A", GraphNodeType::Host);

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

    // ========================================================================
    // Performance Benchmarks (1M edges < 5 seconds target)
    // ========================================================================

    /// Generate a large graph for performance testing
    fn create_large_graph(node_count: usize, edge_multiplier: usize) -> GraphStore {
        let graph = GraphStore::new();

        // Create nodes
        for i in 0..node_count {
            let node_id = format!("n{}", i);
            let label = format!("Node {}", i);
            let _ = graph.add_node(&node_id, &label, GraphNodeType::Host);
        }

        // Create edges (scale-free network style - some nodes have many connections)
        let mut edge_count = 0;
        for i in 0..node_count {
            // Each node connects to edge_multiplier random nodes
            for j in 1..=edge_multiplier {
                let target = (i + j * 17) % node_count; // Pseudo-random but deterministic
                if target != i {
                    let source_id = format!("n{}", i);
                    let target_id = format!("n{}", target);
                    let _ = graph.add_edge(&source_id, &target_id, GraphEdgeType::ConnectsTo, 1.0);
                    edge_count += 1;
                }
            }
        }

        eprintln!(
            "Created graph with {} nodes and {} edges",
            node_count, edge_count
        );
        graph
    }

    /// Benchmark: 1M edges graph creation should be fast
    #[test]
    #[ignore] // Run with: cargo test bench_graph_creation --release -- --ignored --nocapture
    fn bench_graph_creation_1m_edges() {
        use std::time::Instant;

        // Target: 1M edges = ~100K nodes with 10 edges each
        let node_count = 100_000;
        let edge_multiplier = 10;

        let start = Instant::now();
        let graph = create_large_graph(node_count, edge_multiplier);
        let elapsed = start.elapsed();

        let stats = graph.stats();
        eprintln!("Graph creation: {:?}", elapsed);
        eprintln!("Nodes: {}, Edges: {}", stats.node_count, stats.edge_count);

        // Target: < 5 seconds
        assert!(elapsed.as_secs() < 5, "Graph creation took {:?}", elapsed);
    }

    /// Benchmark: PageRank on 1M edges
    #[test]
    #[ignore]
    fn bench_pagerank_1m_edges() {
        use std::time::Instant;

        let graph = create_large_graph(100_000, 10);

        let start = Instant::now();
        let result = PageRank::new().run(&graph);
        let elapsed = start.elapsed();

        eprintln!("PageRank: {:?} ({} iterations)", elapsed, result.iterations);
        eprintln!("Top 5 nodes by rank:");
        let mut scores: Vec<_> = result.scores.iter().collect();
        scores.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
        for (node, score) in scores.iter().take(5) {
            eprintln!("  {}: {:.6}", node, score);
        }

        // Target: < 5 seconds
        assert!(elapsed.as_secs() < 5, "PageRank took {:?}", elapsed);
    }

    /// Benchmark: Connected Components on 1M edges
    #[test]
    #[ignore]
    fn bench_components_1m_edges() {
        use std::time::Instant;

        let graph = create_large_graph(100_000, 10);

        let start = Instant::now();
        let result = ConnectedComponents::find(&graph);
        let elapsed = start.elapsed();

        eprintln!("Connected Components: {:?}", elapsed);
        eprintln!("Found {} components", result.count);

        // Target: < 5 seconds
        assert!(
            elapsed.as_secs() < 5,
            "Connected Components took {:?}",
            elapsed
        );
    }

    /// Benchmark: Betweenness Centrality on smaller graph (O(V*E) complexity)
    #[test]
    #[ignore]
    fn bench_betweenness_100k_edges() {
        use std::time::Instant;

        // Betweenness is O(V*E), so we use a smaller graph
        let graph = create_large_graph(10_000, 10);

        let start = Instant::now();
        let result = BetweennessCentrality::compute(&graph, true);
        let elapsed = start.elapsed();

        eprintln!("Betweenness Centrality: {:?}", elapsed);
        let max_score = result.top(1).first().map(|(_, s)| *s).unwrap_or(0.0);
        eprintln!("Max centrality: {:.6}", max_score);

        // Target: < 30 seconds for 100K edges
        assert!(elapsed.as_secs() < 30, "Betweenness took {:?}", elapsed);
    }

    /// Benchmark: Community Detection on 1M edges
    #[test]
    #[ignore]
    fn bench_communities_1m_edges() {
        use std::time::Instant;

        let graph = create_large_graph(100_000, 10);

        let start = Instant::now();
        let result = LabelPropagation::new().run(&graph);
        let elapsed = start.elapsed();

        eprintln!(
            "Community Detection: {:?} ({} iterations)",
            elapsed, result.iterations
        );
        eprintln!("Found {} communities", result.communities.len());

        // Target: < 5 seconds
        assert!(
            elapsed.as_secs() < 5,
            "Community Detection took {:?}",
            elapsed
        );
    }

    /// Benchmark: Cycle Detection on smaller graph (cycle detection is expensive)
    #[test]
    #[ignore]
    fn bench_cycles_10k_edges() {
        use std::time::Instant;

        // Cycle detection is expensive, use smaller graph
        let graph = create_large_graph(1_000, 10);

        let start = Instant::now();
        let result = CycleDetector::new()
            .max_length(5)
            .max_cycles(100)
            .find(&graph);
        let elapsed = start.elapsed();

        eprintln!("Cycle Detection: {:?}", elapsed);
        eprintln!(
            "Found {} cycles (limit reached: {})",
            result.cycles.len(),
            result.limit_reached
        );

        // Target: < 10 seconds
        assert!(elapsed.as_secs() < 10, "Cycle Detection took {:?}", elapsed);
    }

    /// Comprehensive benchmark suite
    #[test]
    #[ignore]
    fn bench_full_suite() {
        use std::time::Instant;

        eprintln!("\n=== Graph Intelligence Benchmark Suite ===\n");

        // Create test graphs
        let small_graph = create_large_graph(1_000, 10);
        let medium_graph = create_large_graph(10_000, 10);
        let large_graph = create_large_graph(100_000, 10);

        eprintln!(
            "Small graph:  {} nodes, {} edges",
            small_graph.stats().node_count,
            small_graph.stats().edge_count
        );
        eprintln!(
            "Medium graph: {} nodes, {} edges",
            medium_graph.stats().node_count,
            medium_graph.stats().edge_count
        );
        eprintln!(
            "Large graph:  {} nodes, {} edges\n",
            large_graph.stats().node_count,
            large_graph.stats().edge_count
        );

        // PageRank benchmarks
        let start = Instant::now();
        let _ = PageRank::new().run(&small_graph);
        eprintln!("PageRank (10K edges):  {:?}", start.elapsed());

        let start = Instant::now();
        let _ = PageRank::new().run(&medium_graph);
        eprintln!("PageRank (100K edges): {:?}", start.elapsed());

        let start = Instant::now();
        let _ = PageRank::new().run(&large_graph);
        eprintln!("PageRank (1M edges):   {:?}", start.elapsed());

        eprintln!();

        // Connected Components benchmarks
        let start = Instant::now();
        let _ = ConnectedComponents::find(&small_graph);
        eprintln!("Components (10K edges):  {:?}", start.elapsed());

        let start = Instant::now();
        let _ = ConnectedComponents::find(&medium_graph);
        eprintln!("Components (100K edges): {:?}", start.elapsed());

        let start = Instant::now();
        let _ = ConnectedComponents::find(&large_graph);
        eprintln!("Components (1M edges):   {:?}", start.elapsed());

        eprintln!();

        // Community Detection benchmarks
        let start = Instant::now();
        let _ = LabelPropagation::new().run(&small_graph);
        eprintln!("Communities (10K edges):  {:?}", start.elapsed());

        let start = Instant::now();
        let _ = LabelPropagation::new().run(&medium_graph);
        eprintln!("Communities (100K edges): {:?}", start.elapsed());

        let start = Instant::now();
        let _ = LabelPropagation::new().run(&large_graph);
        eprintln!("Communities (1M edges):   {:?}", start.elapsed());

        eprintln!();

        // Betweenness (only on smaller graphs due to O(V*E) complexity)
        let start = Instant::now();
        let _ = BetweennessCentrality::compute(&small_graph, true);
        eprintln!("Betweenness (10K edges):  {:?}", start.elapsed());

        let start = Instant::now();
        let _ = BetweennessCentrality::compute(&medium_graph, true);
        eprintln!("Betweenness (100K edges): {:?}", start.elapsed());

        eprintln!("\n=== Benchmark Complete ===");
    }
}
