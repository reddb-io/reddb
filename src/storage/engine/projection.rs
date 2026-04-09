//! Graph Projections for RedDB
//!
//! Provides graph projection capabilities similar to Neo4j GDS:
//! - Native projections: Copy subgraph with filtering
//! - Cypher/query-based projections: Project from traversal results
//! - Property projections: Select specific properties
//! - Aggregated relationships: Combine parallel edges
//!
//! Projections create lightweight views over the graph for efficient
//! algorithm execution without modifying the original data.

use std::collections::{HashMap, HashSet};

use super::graph_store::{GraphEdgeType, GraphNodeType, GraphStore, StoredNode};

// ============================================================================
// Projection Filter Predicates
// ============================================================================

/// Node filter specification
#[derive(Clone, Default)]
pub struct NodeFilter {
    /// Include only nodes with these labels
    pub labels: Option<Vec<GraphNodeType>>,
    /// Include only nodes with these IDs
    pub ids: Option<HashSet<String>>,
}

impl NodeFilter {
    /// Create an empty filter (include all nodes)
    pub fn all() -> Self {
        Self::default()
    }

    /// Filter by node labels
    pub fn with_labels(mut self, labels: Vec<GraphNodeType>) -> Self {
        self.labels = Some(labels);
        self
    }

    /// Filter by node IDs
    pub fn with_ids(mut self, ids: HashSet<String>) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Check if a node matches this filter
    pub fn matches(&self, node: &StoredNode) -> bool {
        // Check labels
        if let Some(ref labels) = self.labels {
            if !labels.contains(&node.node_type) {
                return false;
            }
        }

        // Check IDs
        if let Some(ref ids) = self.ids {
            if !ids.contains(&node.id) {
                return false;
            }
        }

        true
    }
}

/// Edge filter specification
#[derive(Clone, Default)]
pub struct EdgeFilter {
    /// Include only edges with these types
    pub edge_types: Option<Vec<GraphEdgeType>>,
    /// Minimum edge weight
    pub min_weight: Option<f32>,
    /// Maximum edge weight
    pub max_weight: Option<f32>,
}

impl EdgeFilter {
    /// Create an empty filter (include all edges)
    pub fn all() -> Self {
        Self::default()
    }

    /// Filter by edge types
    pub fn with_types(mut self, types: Vec<GraphEdgeType>) -> Self {
        self.edge_types = Some(types);
        self
    }

    /// Filter by minimum weight
    pub fn with_min_weight(mut self, weight: f32) -> Self {
        self.min_weight = Some(weight);
        self
    }

    /// Filter by maximum weight
    pub fn with_max_weight(mut self, weight: f32) -> Self {
        self.max_weight = Some(weight);
        self
    }

    /// Check if an edge matches this filter
    pub fn matches(&self, edge_type: &GraphEdgeType, weight: f32) -> bool {
        // Check edge types
        if let Some(ref types) = self.edge_types {
            if !types.contains(edge_type) {
                return false;
            }
        }

        // Check weight bounds
        if let Some(min) = self.min_weight {
            if weight < min {
                return false;
            }
        }

        if let Some(max) = self.max_weight {
            if weight > max {
                return false;
            }
        }

        true
    }
}

// ============================================================================
// Property Projection
// ============================================================================

/// Specifies which properties to include in the projection
#[derive(Clone, Default)]
pub struct PropertyProjection {
    /// Whether to include node label
    pub include_label: bool,
    /// Whether to include edge weight
    pub include_weight: bool,
}

impl PropertyProjection {
    /// Include all properties
    pub fn all() -> Self {
        Self {
            include_label: true,
            include_weight: true,
        }
    }

    /// Create minimal projection
    pub fn minimal() -> Self {
        Self {
            include_label: false,
            include_weight: false,
        }
    }
}

// ============================================================================
// Edge Aggregation
// ============================================================================

/// Strategy for aggregating parallel edges
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregationStrategy {
    /// Keep all edges (no aggregation)
    None,
    /// Keep only one edge, use sum of weights
    SumWeight,
    /// Keep only one edge, use average weight
    AvgWeight,
    /// Keep only one edge, use minimum weight
    MinWeight,
    /// Keep only one edge, use maximum weight
    MaxWeight,
    /// Count the number of parallel edges
    Count,
}

// ============================================================================
// Graph Projection
// ============================================================================

/// A projected view of a graph
///
/// Projections are lightweight copies optimized for algorithm execution.
/// They don't modify the original graph.
pub struct GraphProjection {
    /// Projected nodes (id → node)
    nodes: HashMap<String, ProjectedNode>,
    /// Outgoing edges (source_id → [(target_id, edge_type, weight)])
    outgoing: HashMap<String, Vec<(String, GraphEdgeType, f32)>>,
    /// Incoming edges (target_id → [(source_id, edge_type, weight)])
    incoming: HashMap<String, Vec<(String, GraphEdgeType, f32)>>,
    /// Projection statistics
    stats: ProjectionStats,
}

/// A projected node with minimal data for algorithms
#[derive(Clone, Debug)]
pub struct ProjectedNode {
    pub id: String,
    pub label: String,
    pub node_type: Option<GraphNodeType>,
}

/// Statistics about the projection
#[derive(Clone, Debug, Default)]
pub struct ProjectionStats {
    /// Number of nodes in projection
    pub node_count: usize,
    /// Number of edges in projection
    pub edge_count: usize,
    /// Number of nodes filtered out
    pub nodes_filtered: usize,
    /// Number of edges filtered out
    pub edges_filtered: usize,
    /// Number of edges aggregated
    pub edges_aggregated: usize,
}

impl GraphProjection {
    /// Create a native projection from a graph with filters
    pub fn native(
        graph: &GraphStore,
        node_filter: NodeFilter,
        edge_filter: EdgeFilter,
        property_projection: PropertyProjection,
        aggregation: AggregationStrategy,
    ) -> Self {
        let mut nodes: HashMap<String, ProjectedNode> = HashMap::new();
        let mut outgoing: HashMap<String, Vec<(String, GraphEdgeType, f32)>> = HashMap::new();
        let mut incoming: HashMap<String, Vec<(String, GraphEdgeType, f32)>> = HashMap::new();
        let mut stats = ProjectionStats::default();

        // Collect matching nodes
        let mut node_ids: HashSet<String> = HashSet::new();
        for node in graph.iter_nodes() {
            if node_filter.matches(&node) {
                let projected = ProjectedNode {
                    id: node.id.clone(),
                    label: node.label.clone(),
                    node_type: if property_projection.include_label {
                        Some(node.node_type)
                    } else {
                        None
                    },
                };
                node_ids.insert(node.id.clone());
                nodes.insert(node.id.clone(), projected);
                stats.node_count += 1;
            } else {
                stats.nodes_filtered += 1;
            }
        }

        // Collect matching edges (both endpoints must be in projection)
        // Group edges by (source, target) for potential aggregation
        let mut edge_groups: HashMap<(String, String), Vec<(GraphEdgeType, f32)>> = HashMap::new();

        for node_id in &node_ids {
            for (edge_type, target, weight) in graph.outgoing_edges(node_id) {
                if !node_ids.contains(&target) {
                    continue;
                }

                if edge_filter.matches(&edge_type, weight) {
                    let key = (node_id.clone(), target);
                    edge_groups
                        .entry(key)
                        .or_default()
                        .push((edge_type, weight));
                } else {
                    stats.edges_filtered += 1;
                }
            }
        }

        // Apply aggregation
        for ((source, target), edges) in edge_groups {
            match aggregation {
                AggregationStrategy::None => {
                    // Keep all edges
                    for (edge_type, weight) in edges {
                        outgoing.entry(source.clone()).or_default().push((
                            target.clone(),
                            edge_type,
                            weight,
                        ));
                        incoming.entry(target.clone()).or_default().push((
                            source.clone(),
                            edge_type,
                            weight,
                        ));
                        stats.edge_count += 1;
                    }
                }
                _ => {
                    // Aggregate to single edge
                    if let Some((first_type, _)) = edges.first() {
                        let weight = match aggregation {
                            AggregationStrategy::SumWeight => edges.iter().map(|(_, w)| w).sum(),
                            AggregationStrategy::AvgWeight => {
                                let sum: f32 = edges.iter().map(|(_, w)| w).sum();
                                sum / edges.len() as f32
                            }
                            AggregationStrategy::MinWeight => {
                                edges.iter().map(|(_, w)| *w).fold(f32::INFINITY, f32::min)
                            }
                            AggregationStrategy::MaxWeight => edges
                                .iter()
                                .map(|(_, w)| *w)
                                .fold(f32::NEG_INFINITY, f32::max),
                            AggregationStrategy::Count => edges.len() as f32,
                            AggregationStrategy::None => unreachable!(),
                        };

                        if edges.len() > 1 {
                            stats.edges_aggregated += edges.len() - 1;
                        }

                        outgoing.entry(source.clone()).or_default().push((
                            target.clone(),
                            *first_type,
                            weight,
                        ));
                        incoming
                            .entry(target)
                            .or_default()
                            .push((source, *first_type, weight));
                        stats.edge_count += 1;
                    }
                }
            }
        }

        Self {
            nodes,
            outgoing,
            incoming,
            stats,
        }
    }

    /// Create a projection from a list of node IDs (induced subgraph)
    pub fn from_nodes(graph: &GraphStore, node_ids: &[String]) -> Self {
        let id_set: HashSet<String> = node_ids.iter().cloned().collect();
        let node_filter = NodeFilter::all().with_ids(id_set);
        Self::native(
            graph,
            node_filter,
            EdgeFilter::all(),
            PropertyProjection::all(),
            AggregationStrategy::None,
        )
    }

    /// Create a projection from traversal path results
    pub fn from_paths(graph: &GraphStore, paths: &[Vec<String>]) -> Self {
        let mut node_ids: HashSet<String> = HashSet::new();
        for path in paths {
            node_ids.extend(path.iter().cloned());
        }
        let node_filter = NodeFilter::all().with_ids(node_ids);
        Self::native(
            graph,
            node_filter,
            EdgeFilter::all(),
            PropertyProjection::all(),
            AggregationStrategy::None,
        )
    }

    /// Create an undirected projection (each edge becomes bidirectional)
    pub fn undirected(
        graph: &GraphStore,
        node_filter: NodeFilter,
        edge_filter: EdgeFilter,
    ) -> Self {
        let mut projection = Self::native(
            graph,
            node_filter,
            edge_filter,
            PropertyProjection::all(),
            AggregationStrategy::SumWeight,
        );

        // Add reverse edges
        let mut additional: Vec<(String, String, GraphEdgeType, f32)> = Vec::new();

        for (source, edges) in &projection.outgoing {
            for (target, edge_type, weight) in edges {
                // Check if reverse edge already exists
                let has_reverse = projection
                    .outgoing
                    .get(target)
                    .map(|e| e.iter().any(|(t, _, _)| t == source))
                    .unwrap_or(false);

                if !has_reverse {
                    additional.push((target.clone(), source.clone(), *edge_type, *weight));
                }
            }
        }

        for (source, target, edge_type, weight) in additional {
            projection
                .outgoing
                .entry(source.clone())
                .or_default()
                .push((target.clone(), edge_type, weight));
            projection
                .incoming
                .entry(target)
                .or_default()
                .push((source, edge_type, weight));
            projection.stats.edge_count += 1;
        }

        projection
    }

    /// Get projection statistics
    pub fn stats(&self) -> &ProjectionStats {
        &self.stats
    }

    /// Get number of nodes
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get number of edges
    pub fn edge_count(&self) -> usize {
        self.stats.edge_count
    }

    /// Get a node by ID
    pub fn get_node(&self, id: &str) -> Option<&ProjectedNode> {
        self.nodes.get(id)
    }

    /// Check if node exists
    pub fn has_node(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    /// Iterate over all nodes
    pub fn iter_nodes(&self) -> impl Iterator<Item = &ProjectedNode> {
        self.nodes.values()
    }

    /// Get node IDs
    pub fn node_ids(&self) -> impl Iterator<Item = &String> {
        self.nodes.keys()
    }

    /// Get outgoing edges from a node
    pub fn outgoing(&self, node_id: &str) -> &[(String, GraphEdgeType, f32)] {
        self.outgoing
            .get(node_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get incoming edges to a node
    pub fn incoming(&self, node_id: &str) -> &[(String, GraphEdgeType, f32)] {
        self.incoming
            .get(node_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get out-degree of a node
    pub fn out_degree(&self, node_id: &str) -> usize {
        self.outgoing.get(node_id).map(|v| v.len()).unwrap_or(0)
    }

    /// Get in-degree of a node
    pub fn in_degree(&self, node_id: &str) -> usize {
        self.incoming.get(node_id).map(|v| v.len()).unwrap_or(0)
    }

    /// Get neighbors (outgoing targets)
    pub fn neighbors(&self, node_id: &str) -> Vec<&str> {
        self.outgoing
            .get(node_id)
            .map(|edges| edges.iter().map(|(t, _, _)| t.as_str()).collect())
            .unwrap_or_default()
    }

    /// Get neighbors with weights
    pub fn neighbors_weighted(&self, node_id: &str) -> Vec<(&str, f32)> {
        self.outgoing
            .get(node_id)
            .map(|edges| edges.iter().map(|(t, _, w)| (t.as_str(), *w)).collect())
            .unwrap_or_default()
    }

    /// Get all neighbors (both directions)
    pub fn all_neighbors(&self, node_id: &str) -> HashSet<&str> {
        let mut neighbors: HashSet<&str> = HashSet::new();

        if let Some(edges) = self.outgoing.get(node_id) {
            for (target, _, _) in edges {
                neighbors.insert(target.as_str());
            }
        }

        if let Some(edges) = self.incoming.get(node_id) {
            for (source, _, _) in edges {
                neighbors.insert(source.as_str());
            }
        }

        neighbors
    }
}

// ============================================================================
// Projection Builder
// ============================================================================

/// Builder for creating graph projections with fluent API
pub struct ProjectionBuilder<'a> {
    graph: &'a GraphStore,
    node_filter: NodeFilter,
    edge_filter: EdgeFilter,
    property_projection: PropertyProjection,
    aggregation: AggregationStrategy,
    undirected: bool,
}

impl<'a> ProjectionBuilder<'a> {
    /// Create a new projection builder
    pub fn new(graph: &'a GraphStore) -> Self {
        Self {
            graph,
            node_filter: NodeFilter::all(),
            edge_filter: EdgeFilter::all(),
            property_projection: PropertyProjection::all(),
            aggregation: AggregationStrategy::None,
            undirected: false,
        }
    }

    /// Filter nodes by label
    pub fn with_node_labels(mut self, labels: Vec<GraphNodeType>) -> Self {
        self.node_filter = self.node_filter.with_labels(labels);
        self
    }

    /// Filter nodes by IDs
    pub fn with_node_ids(mut self, ids: HashSet<String>) -> Self {
        self.node_filter = self.node_filter.with_ids(ids);
        self
    }

    /// Filter edges by type
    pub fn with_edge_types(mut self, types: Vec<GraphEdgeType>) -> Self {
        self.edge_filter = self.edge_filter.with_types(types);
        self
    }

    /// Filter edges by minimum weight
    pub fn with_min_weight(mut self, weight: f32) -> Self {
        self.edge_filter = self.edge_filter.with_min_weight(weight);
        self
    }

    /// Filter edges by maximum weight
    pub fn with_max_weight(mut self, weight: f32) -> Self {
        self.edge_filter = self.edge_filter.with_max_weight(weight);
        self
    }

    /// Set edge aggregation strategy
    pub fn aggregate(mut self, strategy: AggregationStrategy) -> Self {
        self.aggregation = strategy;
        self
    }

    /// Make the projection undirected
    pub fn undirected(mut self) -> Self {
        self.undirected = true;
        self
    }

    /// Build the projection
    pub fn build(self) -> GraphProjection {
        if self.undirected {
            GraphProjection::undirected(self.graph, self.node_filter, self.edge_filter)
        } else {
            GraphProjection::native(
                self.graph,
                self.node_filter,
                self.edge_filter,
                self.property_projection,
                self.aggregation,
            )
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_graph() -> GraphStore {
        let graph = GraphStore::new();

        let _ = graph.add_node("A", "Server A", GraphNodeType::Host);
        let _ = graph.add_node("B", "Server B", GraphNodeType::Host);
        let _ = graph.add_node("C", "DB Server", GraphNodeType::Service);
        let _ = graph.add_node("D", "Web Server", GraphNodeType::Service);

        let _ = graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("A", "C", GraphEdgeType::ConnectsTo, 2.0);
        let _ = graph.add_edge("B", "C", GraphEdgeType::AuthAccess, 1.5);
        let _ = graph.add_edge("B", "D", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("C", "D", GraphEdgeType::ConnectsTo, 0.5);

        graph
    }

    #[test]
    fn test_full_projection() {
        let graph = create_test_graph();
        let projection = GraphProjection::native(
            &graph,
            NodeFilter::all(),
            EdgeFilter::all(),
            PropertyProjection::all(),
            AggregationStrategy::None,
        );

        assert_eq!(projection.node_count(), 4);
        assert_eq!(projection.edge_count(), 5);
    }

    #[test]
    fn test_node_label_filter() {
        let graph = create_test_graph();
        let projection = GraphProjection::native(
            &graph,
            NodeFilter::all().with_labels(vec![GraphNodeType::Host]),
            EdgeFilter::all(),
            PropertyProjection::all(),
            AggregationStrategy::None,
        );

        assert_eq!(projection.node_count(), 2); // A and B
        assert!(projection.has_node("A"));
        assert!(projection.has_node("B"));
        assert!(!projection.has_node("C"));
        assert!(!projection.has_node("D"));
    }

    #[test]
    fn test_edge_type_filter() {
        let graph = create_test_graph();
        let projection = GraphProjection::native(
            &graph,
            NodeFilter::all(),
            EdgeFilter::all().with_types(vec![GraphEdgeType::ConnectsTo]),
            PropertyProjection::all(),
            AggregationStrategy::None,
        );

        // A->B, A->C, B->D, C->D are ConnectsTo, B->C is HasAccess
        assert_eq!(projection.edge_count(), 4);
    }

    #[test]
    fn test_weight_filter() {
        let graph = create_test_graph();
        let projection = GraphProjection::native(
            &graph,
            NodeFilter::all(),
            EdgeFilter::all().with_min_weight(1.0),
            PropertyProjection::all(),
            AggregationStrategy::None,
        );

        // Edges with weight >= 1.0: A->B(1.0), A->C(2.0), B->C(1.5), B->D(1.0)
        assert_eq!(projection.edge_count(), 4);
    }

    #[test]
    fn test_projection_builder() {
        let graph = create_test_graph();
        let projection = ProjectionBuilder::new(&graph)
            .with_node_labels(vec![GraphNodeType::Service])
            .build();

        assert_eq!(projection.node_count(), 2); // C and D
    }

    #[test]
    fn test_undirected_projection() {
        let graph = create_test_graph();
        let projection = ProjectionBuilder::new(&graph).undirected().build();

        // Each edge should be traversable in both directions
        assert!(projection.neighbors("A").contains(&"B"));
        // Reverse edge should also exist
        let b_neighbors = projection.neighbors("B");
        assert!(b_neighbors.contains(&"A")); // Reverse of A->B
    }

    #[test]
    fn test_from_nodes() {
        let graph = create_test_graph();
        let projection = GraphProjection::from_nodes(&graph, &["A".to_string(), "B".to_string()]);

        assert_eq!(projection.node_count(), 2);
        // Only edge A->B should be included
        assert_eq!(projection.edge_count(), 1);
    }

    #[test]
    fn test_neighbors() {
        let graph = create_test_graph();
        let projection = GraphProjection::native(
            &graph,
            NodeFilter::all(),
            EdgeFilter::all(),
            PropertyProjection::all(),
            AggregationStrategy::None,
        );

        let a_neighbors = projection.neighbors("A");
        assert!(a_neighbors.contains(&"B"));
        assert!(a_neighbors.contains(&"C"));
        assert_eq!(a_neighbors.len(), 2);
    }

    #[test]
    fn test_degrees() {
        let graph = create_test_graph();
        let projection = GraphProjection::native(
            &graph,
            NodeFilter::all(),
            EdgeFilter::all(),
            PropertyProjection::all(),
            AggregationStrategy::None,
        );

        assert_eq!(projection.out_degree("A"), 2); // A -> B, C
        assert_eq!(projection.in_degree("D"), 2); // B, C -> D
        assert_eq!(projection.out_degree("D"), 0); // D has no outgoing
    }
}
