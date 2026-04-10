use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType, GraphStore, StoredNode};
use crate::storage::engine::graph_table_index::GraphTableIndex;
use crate::storage::query::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, Filter, GraphPattern, GraphQuery, JoinQuery,
    JoinType, NodePattern, NodeSelector, PathQuery, Projection, QueryExpr, TableQuery,
};
use crate::storage::schema::Value;

/// Execution error
#[derive(Debug, Clone)]
pub struct ExecutionError {
    pub message: String,
}

impl ExecutionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Execution error: {}", self.message)
    }
}

impl std::error::Error for ExecutionError {}

/// Result of a unified query
#[derive(Debug, Clone, Default)]
pub struct UnifiedResult {
    /// Column names for table data
    pub columns: Vec<String>,
    /// Result records
    pub records: Vec<UnifiedRecord>,
    /// Query statistics
    pub stats: QueryStats,
}

impl UnifiedResult {
    /// Create an empty result
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create a result with columns
    pub fn with_columns(columns: Vec<String>) -> Self {
        Self {
            columns,
            records: Vec::new(),
            stats: QueryStats::default(),
        }
    }

    /// Add a record
    pub fn push(&mut self, record: UnifiedRecord) {
        self.records.push(record);
    }

    /// Number of records
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Is the result empty?
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// A single result record containing mixed data
#[derive(Debug, Clone, Default)]
pub struct UnifiedRecord {
    /// Column values (for table data)
    pub values: HashMap<String, Value>,
    /// Matched nodes (for graph data)
    pub nodes: HashMap<String, MatchedNode>,
    /// Matched edges (for graph data)
    pub edges: HashMap<String, MatchedEdge>,
    /// Paths (for path queries)
    pub paths: Vec<GraphPath>,
    /// Vector search results
    pub vector_results: Vec<VectorSearchResult>,
}

impl UnifiedRecord {
    /// Create an empty record
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a column value
    pub fn set(&mut self, column: &str, value: Value) {
        self.values.insert(column.to_string(), value);
    }

    /// Get a column value
    pub fn get(&self, column: &str) -> Option<&Value> {
        self.values.get(column)
    }

    /// Set a matched node
    pub fn set_node(&mut self, alias: &str, node: MatchedNode) {
        self.nodes.insert(alias.to_string(), node);
    }

    /// Get a matched node
    pub fn get_node(&self, alias: &str) -> Option<&MatchedNode> {
        self.nodes.get(alias)
    }

    /// Set a matched edge
    pub fn set_edge(&mut self, alias: &str, edge: MatchedEdge) {
        self.edges.insert(alias.to_string(), edge);
    }
}

/// A matched node from graph query
#[derive(Debug, Clone)]
pub struct MatchedNode {
    pub id: String,
    pub label: String,
    pub node_type: GraphNodeType,
    pub properties: HashMap<String, Value>,
}

impl MatchedNode {
    /// Create from a stored node
    pub fn from_stored(node: &StoredNode) -> Self {
        Self {
            id: node.id.clone(),
            label: node.label.clone(),
            node_type: node.node_type,
            properties: HashMap::new(),
        }
    }
}

/// A matched edge from graph query
#[derive(Debug, Clone)]
pub struct MatchedEdge {
    pub from: String,
    pub to: String,
    pub edge_type: GraphEdgeType,
    pub weight: f32,
}

impl MatchedEdge {
    /// Create from edge tuple (type, target_id, weight) with source
    pub fn from_tuple(source: &str, edge_type: GraphEdgeType, target: &str, weight: f32) -> Self {
        Self {
            from: source.to_string(),
            to: target.to_string(),
            edge_type,
            weight,
        }
    }
}

/// A vector search result
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    /// Vector ID
    pub id: u64,
    /// Collection name
    pub collection: String,
    /// Distance to query vector
    pub distance: f32,
    /// The vector data (if requested)
    pub vector: Option<Vec<f32>>,
    /// Metadata (if requested)
    pub metadata: Option<HashMap<String, Value>>,
    /// Linked node ID (if cross-referenced)
    pub linked_node: Option<String>,
    /// Linked table row (table, row_id)
    pub linked_row: Option<(String, u64)>,
}

impl VectorSearchResult {
    /// Create a new vector search result
    pub fn new(id: u64, collection: impl Into<String>, distance: f32) -> Self {
        Self {
            id,
            collection: collection.into(),
            distance,
            vector: None,
            metadata: None,
            linked_node: None,
            linked_row: None,
        }
    }

    /// Include vector data
    pub fn with_vector(mut self, vector: Vec<f32>) -> Self {
        self.vector = Some(vector);
        self
    }

    /// Include metadata
    pub fn with_metadata(mut self, metadata: HashMap<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Link to a graph node
    pub fn with_linked_node(mut self, node_id: impl Into<String>) -> Self {
        self.linked_node = Some(node_id.into());
        self
    }

    /// Link to a table row
    pub fn with_linked_row(mut self, table: impl Into<String>, row_id: u64) -> Self {
        self.linked_row = Some((table.into(), row_id));
        self
    }
}

impl Default for VectorSearchResult {
    fn default() -> Self {
        Self::new(0, String::new(), 0.0)
    }
}

/// A path through the graph
#[derive(Debug, Clone)]
pub struct GraphPath {
    /// Sequence of node IDs
    pub nodes: Vec<String>,
    /// Sequence of edges (node_ids.len() - 1)
    pub edges: Vec<MatchedEdge>,
    /// Total path weight
    pub total_weight: f32,
}

impl GraphPath {
    /// Create a new path starting from a node
    pub fn start(node_id: &str) -> Self {
        Self {
            nodes: vec![node_id.to_string()],
            edges: Vec::new(),
            total_weight: 0.0,
        }
    }

    /// Extend the path with an edge and node
    pub fn extend(&self, edge: MatchedEdge, node_id: &str) -> Self {
        let mut new_path = self.clone();
        new_path.total_weight += edge.weight;
        new_path.edges.push(edge);
        new_path.nodes.push(node_id.to_string());
        new_path
    }

    /// Path length (number of edges)
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Is the path empty (no edges)?
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

/// Query execution statistics
#[derive(Debug, Clone, Default)]
pub struct QueryStats {
    /// Number of nodes scanned
    pub nodes_scanned: u64,
    /// Number of edges scanned
    pub edges_scanned: u64,
    /// Number of rows scanned
    pub rows_scanned: u64,
    /// Execution time in microseconds
    pub exec_time_us: u64,
}
