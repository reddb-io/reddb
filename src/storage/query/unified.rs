//! Unified Query Executor
//!
//! Executes parsed RQL queries against both table and graph storage,
//! returning unified results that can contain rows, nodes, edges, and paths.
//!
//! # Architecture
//!
//! The executor is a tree of specialized executors:
//! - `UnifiedExecutor`: Entry point, dispatches to sub-executors
//! - `TableExecutor`: Scans tables, applies filters, sorts
//! - `GraphExecutor`: Matches graph patterns, traverses edges
//! - `JoinExecutor`: Merges table and graph results via GraphTableIndex
//! - `PathExecutor`: Runs graph traversals (BFS/DFS)
//!
//! # Example
//!
//! ```ignore
//! use redblue::storage::query::{parse, UnifiedExecutor, UnifiedResult};
//!
//! let query = parse("FROM hosts h JOIN GRAPH (h)-[:HAS_SERVICE]->(s) RETURN h.ip, s.port")?;
//! let executor = UnifiedExecutor::new(table_store, graph_store, index);
//! let result = executor.execute(&query)?;
//!
//! for record in result.records {
//!     println!("{:?}", record);
//! }
//! ```

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, Filter, GraphPattern, GraphQuery, JoinQuery,
    JoinType, NodePattern, NodeSelector, PathQuery, Projection, QueryExpr, TableQuery,
};
use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType, GraphStore, StoredNode};
use crate::storage::engine::graph_table_index::GraphTableIndex;
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
}

impl MatchedNode {
    /// Create from a stored node
    pub fn from_stored(node: &StoredNode) -> Self {
        Self {
            id: node.id.clone(),
            label: node.label.clone(),
            node_type: node.node_type,
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

/// Unified Query Executor
pub struct UnifiedExecutor {
    /// Graph storage
    graph: Arc<GraphStore>,
    /// Graph-table index for joins
    index: Arc<GraphTableIndex>,
}

impl UnifiedExecutor {
    /// Create a new executor
    pub fn new(graph: Arc<GraphStore>, index: Arc<GraphTableIndex>) -> Self {
        Self { graph, index }
    }

    /// Execute a query directly against a graph reference
    ///
    /// This is a convenience method for simple graph-only queries.
    /// For table joins, use `new()` with proper Arc ownership.
    pub fn execute_on(
        graph: &GraphStore,
        query: &QueryExpr,
    ) -> Result<UnifiedResult, ExecutionError> {
        // Create a temporary executor with empty index
        // This works for graph and path queries, but not table/join queries
        let temp = Self {
            graph: Arc::new(GraphStore::new()), // Placeholder - we'll access graph directly
            index: Arc::new(GraphTableIndex::new()),
        };

        match query {
            QueryExpr::Graph(q) => temp.exec_graph_on(graph, q),
            QueryExpr::Path(q) => temp.exec_path_on(graph, q),
            QueryExpr::Table(_) => Err(ExecutionError::new(
                "Table queries require proper executor initialization",
            )),
            QueryExpr::Join(_) => Err(ExecutionError::new(
                "Join queries require proper executor initialization",
            )),
            QueryExpr::Vector(_) => Err(ExecutionError::new(
                "Vector queries require VectorStore integration",
            )),
            QueryExpr::Hybrid(_) => Err(ExecutionError::new(
                "Hybrid queries require VectorStore integration",
            )),
        }
    }

    /// Execute a graph query on a specific graph reference
    fn exec_graph_on(
        &self,
        graph: &GraphStore,
        query: &GraphQuery,
    ) -> Result<UnifiedResult, ExecutionError> {
        let mut result = UnifiedResult::empty();

        // Get all nodes that match the pattern
        for pattern_node in &query.pattern.nodes {
            let matching_nodes: Vec<_> = if let Some(ref node_type) = pattern_node.node_type {
                graph.nodes_of_type(node_type.clone())
            } else {
                graph.iter_nodes().collect()
            };

            // Filter and add matching nodes
            for node in matching_nodes {
                // Property filters use label matching (since StoredNode doesn't have properties HashMap)
                let mut matches = true;
                for prop_filter in &pattern_node.properties {
                    // Match against label for now since we don't have a properties field
                    // Convert Value to string for comparison
                    let filter_str = match &prop_filter.value {
                        Value::Text(s) => s.clone(),
                        Value::Integer(i) => i.to_string(),
                        Value::Float(f) => f.to_string(),
                        Value::Boolean(b) => b.to_string(),
                        _ => String::new(),
                    };
                    matches = matches && node.label.contains(&filter_str);
                }

                if matches {
                    let mut record = UnifiedRecord::new();
                    record.set_node(&pattern_node.alias, MatchedNode::from_stored(&node));
                    result.records.push(record);
                }
            }
        }

        result.stats.nodes_scanned = result.records.len() as u64;
        Ok(result)
    }

    /// Execute a path query on a specific graph reference
    fn exec_path_on(
        &self,
        graph: &GraphStore,
        query: &PathQuery,
    ) -> Result<UnifiedResult, ExecutionError> {
        let mut result = UnifiedResult::empty();

        // BFS to find paths
        let mut queue: VecDeque<(String, GraphPath)> = VecDeque::new();
        let mut visited: HashSet<String> = HashSet::new();

        // Get start node IDs from selector
        let start_ids = self.resolve_selector_on(graph, &query.from);

        for start in start_ids {
            queue.push_back((start.clone(), GraphPath::start(&start)));
            visited.insert(start);
        }

        let target_ids: HashSet<_> = self
            .resolve_selector_on(graph, &query.to)
            .into_iter()
            .collect();
        let max_len = query.max_length as usize;

        while let Some((current, path)) = queue.pop_front() {
            if path.len() > max_len {
                continue;
            }

            if target_ids.contains(&current) && !path.is_empty() {
                let mut record = UnifiedRecord::new();
                record.paths.push(path.clone());
                result.records.push(record);
                continue;
            }

            // Expand to neighbors
            for (edge_type, neighbor, weight) in graph.outgoing_edges(&current) {
                // Check via filter (query.via is a Vec, not Option)
                if !query.via.is_empty() && !query.via.contains(&edge_type) {
                    continue;
                }

                if !visited.contains(&neighbor) {
                    visited.insert(neighbor.clone());
                    let edge = MatchedEdge::from_tuple(&current, edge_type, &neighbor, weight);
                    let new_path = path.extend(edge, &neighbor);
                    queue.push_back((neighbor, new_path));
                }
            }
        }

        result.stats.edges_scanned = visited.len() as u64;
        Ok(result)
    }

    /// Resolve a node selector to IDs on a specific graph
    fn resolve_selector_on(&self, graph: &GraphStore, selector: &NodeSelector) -> Vec<String> {
        match selector {
            NodeSelector::ById(id) => vec![id.clone()],
            NodeSelector::ByType {
                node_type,
                filter: _,
            } => graph
                .nodes_of_type(node_type.clone())
                .into_iter()
                .map(|n| n.id)
                .collect(),
            NodeSelector::ByRow { table, row_id } => {
                if let Some((table_id, row_id)) = match (table.as_str().parse::<u16>(), *row_id) {
                    (Ok(table_id), row_id) => Some((table_id, row_id)),
                    _ => None,
                } {
                    let mut ids = Vec::new();

                    // Fast path: query the bidirectional graph-table index first
                    if let Some(node_id) = self.index.get_node_for_row(table_id, row_id) {
                        ids.push(node_id);
                    }

                    // Fallback path: for callers that don't register index mappings yet,
                    // scan graph nodes directly by table_ref row linkage.
                    if ids.is_empty() {
                        ids.extend(graph.iter_nodes().filter_map(|node| {
                            let Some(table_ref) = node.table_ref else {
                                return None;
                            };
                            if table_ref.table_id == table_id && table_ref.row_id == row_id {
                                Some(node.id)
                            } else {
                                None
                            }
                        }));
                    }

                    ids
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Execute a query
    pub fn execute(&self, query: &QueryExpr) -> Result<UnifiedResult, ExecutionError> {
        match query {
            QueryExpr::Table(q) => self.exec_table(q),
            QueryExpr::Graph(q) => self.exec_graph(q),
            QueryExpr::Join(q) => self.exec_join(q),
            QueryExpr::Path(q) => self.exec_path(q),
            QueryExpr::Vector(_) => {
                // Vector execution requires VectorStore integration
                // This will be implemented in the VectorExecutor
                Err(ExecutionError::new(
                    "Vector queries not yet implemented in UnifiedExecutor",
                ))
            }
            QueryExpr::Hybrid(_) => {
                // Hybrid execution requires both structured and vector execution
                // This will be implemented in the HybridExecutor
                Err(ExecutionError::new(
                    "Hybrid queries not yet implemented in UnifiedExecutor",
                ))
            }
        }
    }

    /// Execute a table query
    /// Note: Without actual table storage access, this returns empty result.
    /// In production, this would integrate with the table storage engine.
    fn exec_table(&self, _query: &TableQuery) -> Result<UnifiedResult, ExecutionError> {
        // Table execution requires table storage integration
        // For now, return empty result
        Ok(UnifiedResult::empty())
    }

    /// Execute a graph query
    fn exec_graph(&self, query: &GraphQuery) -> Result<UnifiedResult, ExecutionError> {
        let mut result = UnifiedResult::empty();
        let mut stats = QueryStats::default();

        // Match the pattern
        let matches = self.match_pattern(&query.pattern, &mut stats)?;

        // Apply filter
        let filtered: Vec<_> = matches
            .into_iter()
            .filter(|m| self.eval_filter_on_match(&query.filter, m))
            .collect();

        // Build result records with projections
        for matched in filtered {
            let record = self.project_match(&matched, &query.return_);
            result.push(record);
        }

        result.stats = stats;
        Ok(result)
    }

    /// Match a graph pattern
    fn match_pattern(
        &self,
        pattern: &GraphPattern,
        stats: &mut QueryStats,
    ) -> Result<Vec<PatternMatch>, ExecutionError> {
        if pattern.nodes.is_empty() {
            return Ok(Vec::new());
        }

        // Start with first node pattern
        let first = &pattern.nodes[0];
        let mut matches = self.find_matching_nodes(first, stats)?;

        // Extend matches for each edge pattern
        for edge_pattern in &pattern.edges {
            matches = self.extend_matches(matches, edge_pattern, &pattern.nodes, stats)?;
        }

        Ok(matches)
    }

    /// Find nodes matching a pattern
    fn find_matching_nodes(
        &self,
        pattern: &NodePattern,
        stats: &mut QueryStats,
    ) -> Result<Vec<PatternMatch>, ExecutionError> {
        let mut matches = Vec::new();

        // Iterate through all nodes
        for node in self.graph.iter_nodes() {
            stats.nodes_scanned += 1;

            // Check type filter
            if let Some(ref node_type) = pattern.node_type {
                if node.node_type != *node_type {
                    continue;
                }
            }

            // Check property filters (id and label only in this storage model)
            let mut match_props = true;
            for prop_filter in &pattern.properties {
                if !self.eval_node_property_filter(&node, prop_filter) {
                    match_props = false;
                    break;
                }
            }

            if match_props {
                let mut pm = PatternMatch::new();
                pm.nodes
                    .insert(pattern.alias.clone(), MatchedNode::from_stored(&node));
                matches.push(pm);
            }
        }

        Ok(matches)
    }

    /// Extend matches by following an edge pattern
    fn extend_matches(
        &self,
        matches: Vec<PatternMatch>,
        edge_pattern: &EdgePattern,
        node_patterns: &[NodePattern],
        stats: &mut QueryStats,
    ) -> Result<Vec<PatternMatch>, ExecutionError> {
        let mut extended = Vec::new();

        // Find the target node pattern
        let target_pattern = node_patterns
            .iter()
            .find(|n| n.alias == edge_pattern.to)
            .ok_or_else(|| {
                ExecutionError::new(format!(
                    "Node alias '{}' not found in pattern",
                    edge_pattern.to
                ))
            })?;

        for pm in matches {
            // Get the source node
            let source_node = pm.nodes.get(&edge_pattern.from).ok_or_else(|| {
                ExecutionError::new(format!(
                    "Source node '{}' not found in match",
                    edge_pattern.from
                ))
            })?;

            // Get adjacent edges - returns Vec<(GraphEdgeType, String, f32)>
            // For outgoing: (edge_type, target_id, weight)
            // For incoming: (edge_type, source_id, weight)
            let edges: Vec<(GraphEdgeType, String, f32, bool)> = match edge_pattern.direction {
                EdgeDirection::Outgoing => {
                    self.graph
                        .outgoing_edges(&source_node.id)
                        .into_iter()
                        .map(|(et, target, w)| (et, target, w, true)) // is_outgoing = true
                        .collect()
                }
                EdgeDirection::Incoming => {
                    self.graph
                        .incoming_edges(&source_node.id)
                        .into_iter()
                        .map(|(et, source, w)| (et, source, w, false)) // is_outgoing = false
                        .collect()
                }
                EdgeDirection::Both => {
                    let mut all: Vec<_> = self
                        .graph
                        .outgoing_edges(&source_node.id)
                        .into_iter()
                        .map(|(et, target, w)| (et, target, w, true))
                        .collect();
                    all.extend(
                        self.graph
                            .incoming_edges(&source_node.id)
                            .into_iter()
                            .map(|(et, source, w)| (et, source, w, false)),
                    );
                    all
                }
            };

            for (etype, other_id, weight, is_outgoing) in edges {
                stats.edges_scanned += 1;

                // Check edge type filter
                if let Some(ref edge_type) = edge_pattern.edge_type {
                    if etype != *edge_type {
                        continue;
                    }
                }

                // The target is the other node
                let target_id = &other_id;

                if let Some(target_node) = self.graph.get_node(target_id) {
                    // Check target node type
                    if let Some(ref node_type) = target_pattern.node_type {
                        if target_node.node_type != *node_type {
                            continue;
                        }
                    }

                    // Check target property filters
                    let mut match_props = true;
                    for prop_filter in &target_pattern.properties {
                        if !self.eval_node_property_filter(&target_node, prop_filter) {
                            match_props = false;
                            break;
                        }
                    }

                    if match_props {
                        let mut new_pm = pm.clone();
                        new_pm.nodes.insert(
                            target_pattern.alias.clone(),
                            MatchedNode::from_stored(&target_node),
                        );
                        if let Some(ref alias) = edge_pattern.alias {
                            // Create edge with proper from/to direction
                            let edge = if is_outgoing {
                                MatchedEdge::from_tuple(&source_node.id, etype, target_id, weight)
                            } else {
                                MatchedEdge::from_tuple(target_id, etype, &source_node.id, weight)
                            };
                            new_pm.edges.insert(alias.clone(), edge);
                        }
                        extended.push(new_pm);
                    }
                }
            }
        }

        Ok(extended)
    }

    /// Evaluate a property filter on a stored node
    /// StoredNode only has id, label, node_type - no arbitrary properties
    fn eval_node_property_filter(
        &self,
        node: &StoredNode,
        filter: &super::ast::PropertyFilter,
    ) -> bool {
        let value = match filter.name.as_str() {
            "id" => Value::Text(node.id.clone()),
            "label" => Value::Text(node.label.clone()),
            _ => return false, // No other properties available
        };

        self.compare_values(&value, &filter.op, &filter.value)
    }

    /// Compare two values with an operator
    fn compare_values(&self, left: &Value, op: &CompareOp, right: &Value) -> bool {
        match op {
            CompareOp::Eq => left == right,
            CompareOp::Ne => left != right,
            CompareOp::Lt => self.value_lt(left, right),
            CompareOp::Le => self.value_lt(left, right) || left == right,
            CompareOp::Gt => self.value_lt(right, left),
            CompareOp::Ge => self.value_lt(right, left) || left == right,
        }
    }

    /// Less-than comparison for values
    fn value_lt(&self, left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Integer(a), Value::Integer(b)) => a < b,
            (Value::Float(a), Value::Float(b)) => a < b,
            (Value::Integer(a), Value::Float(b)) => (*a as f64) < *b,
            (Value::Float(a), Value::Integer(b)) => *a < (*b as f64),
            (Value::Text(a), Value::Text(b)) => a < b,
            (Value::Timestamp(a), Value::Timestamp(b)) => a < b,
            _ => false,
        }
    }

    /// Evaluate a filter on a pattern match
    fn eval_filter_on_match(&self, filter: &Option<Filter>, matched: &PatternMatch) -> bool {
        match filter {
            None => true,
            Some(f) => self.eval_filter(f, matched),
        }
    }

    /// Evaluate a filter expression
    fn eval_filter(&self, filter: &Filter, matched: &PatternMatch) -> bool {
        match filter {
            Filter::Compare { field, op, value } => {
                let actual = self.get_field_value(field, matched);
                match actual {
                    Some(v) => self.compare_values(&v, op, value),
                    None => false,
                }
            }
            Filter::And(left, right) => {
                self.eval_filter(left, matched) && self.eval_filter(right, matched)
            }
            Filter::Or(left, right) => {
                self.eval_filter(left, matched) || self.eval_filter(right, matched)
            }
            Filter::Not(inner) => !self.eval_filter(inner, matched),
            Filter::IsNull(field) => self.get_field_value(field, matched).is_none(),
            Filter::IsNotNull(field) => self.get_field_value(field, matched).is_some(),
            Filter::In { field, values } => match self.get_field_value(field, matched) {
                Some(v) => values.contains(&v),
                None => false,
            },
            Filter::Between { field, low, high } => match self.get_field_value(field, matched) {
                Some(v) => !self.value_lt(&v, low) && !self.value_lt(high, &v),
                None => false,
            },
            Filter::Like { field, pattern } => match self.get_field_value(field, matched) {
                Some(Value::Text(s)) => self.match_like(&s, pattern),
                _ => false,
            },
            Filter::StartsWith { field, prefix } => match self.get_field_value(field, matched) {
                Some(Value::Text(s)) => s.starts_with(prefix),
                _ => false,
            },
            Filter::EndsWith { field, suffix } => match self.get_field_value(field, matched) {
                Some(Value::Text(s)) => s.ends_with(suffix),
                _ => false,
            },
            Filter::Contains { field, substring } => match self.get_field_value(field, matched) {
                Some(Value::Text(s)) => s.contains(substring),
                _ => false,
            },
        }
    }

    /// Get a field value from a pattern match
    fn get_field_value(&self, field: &FieldRef, matched: &PatternMatch) -> Option<Value> {
        match field {
            FieldRef::NodeId { alias } => {
                matched.nodes.get(alias).map(|n| Value::Text(n.id.clone()))
            }
            FieldRef::NodeProperty { alias, property } => {
                matched.nodes.get(alias).and_then(|n| {
                    match property.as_str() {
                        "id" => Some(Value::Text(n.id.clone())),
                        "label" => Some(Value::Text(n.label.clone())),
                        // No other properties available in MatchedNode
                        _ => None,
                    }
                })
            }
            FieldRef::EdgeProperty { alias, property } => {
                matched.edges.get(alias).and_then(|e| {
                    match property.as_str() {
                        "weight" => Some(Value::Float(e.weight as f64)),
                        "from" => Some(Value::Text(e.from.clone())),
                        "to" => Some(Value::Text(e.to.clone())),
                        // No other properties available in MatchedEdge
                        _ => None,
                    }
                })
            }
            FieldRef::TableColumn { .. } => {
                // Table columns not available in graph-only match
                None
            }
        }
    }

    /// Simple LIKE pattern matching (% and _ wildcards)
    fn match_like(&self, text: &str, pattern: &str) -> bool {
        // Simple implementation: convert % to .* and _ to .
        let regex_pattern = pattern.replace('%', ".*").replace('_', ".");

        // Basic match without regex (for simplicity)
        if pattern.starts_with('%') && pattern.ends_with('%') {
            let inner = &pattern[1..pattern.len() - 1];
            text.contains(inner)
        } else if pattern.starts_with('%') {
            let suffix = &pattern[1..];
            text.ends_with(suffix)
        } else if pattern.ends_with('%') {
            let prefix = &pattern[..pattern.len() - 1];
            text.starts_with(prefix)
        } else {
            text == pattern || regex_pattern == text
        }
    }

    /// Project a match into a result record
    fn project_match(&self, matched: &PatternMatch, projections: &[Projection]) -> UnifiedRecord {
        let mut record = UnifiedRecord::new();

        // Copy all matched nodes and edges
        record.nodes = matched.nodes.clone();
        record.edges = matched.edges.clone();

        // Extract projected values
        for proj in projections {
            match proj {
                Projection::Field(field, alias) => {
                    if let Some(value) = self.get_field_value(field, matched) {
                        let key = alias.clone().unwrap_or_else(|| self.field_to_string(field));
                        record.set(&key, value);
                    }
                }
                Projection::All => {
                    // For All projection, include all node basic info
                    for (alias, node) in &matched.nodes {
                        record.set(&format!("{}.id", alias), Value::Text(node.id.clone()));
                        record.set(&format!("{}.label", alias), Value::Text(node.label.clone()));
                    }
                }
                Projection::Column(col) => {
                    // Try to find a matching column in nodes
                    for (_, node) in &matched.nodes {
                        match col.as_str() {
                            "id" => record.set(col, Value::Text(node.id.clone())),
                            "label" => record.set(col, Value::Text(node.label.clone())),
                            _ => {}
                        }
                    }
                }
                Projection::Alias(col, alias) => {
                    for (_, node) in &matched.nodes {
                        match col.as_str() {
                            "id" => record.set(alias, Value::Text(node.id.clone())),
                            "label" => record.set(alias, Value::Text(node.label.clone())),
                            _ => {}
                        }
                    }
                }
                _ => {} // Function and Expression projections not supported yet
            }
        }

        record
    }

    /// Convert a field reference to a string key
    fn field_to_string(&self, field: &FieldRef) -> String {
        match field {
            FieldRef::NodeId { alias } => format!("{}.id", alias),
            FieldRef::NodeProperty { alias, property } => format!("{}.{}", alias, property),
            FieldRef::EdgeProperty { alias, property } => format!("{}.{}", alias, property),
            FieldRef::TableColumn { table, column } => {
                if table.is_empty() {
                    column.clone()
                } else {
                    format!("{}.{}", table, column)
                }
            }
        }
    }

    /// Execute a join query
    fn exec_join(&self, query: &JoinQuery) -> Result<UnifiedResult, ExecutionError> {
        // Execute left side
        let left_result = self.execute(&query.left)?;

        // Execute right side
        let right_result = self.execute(&query.right)?;

        // Perform the join
        let mut result = UnifiedResult::empty();

        // For each left record, find matching right records
        for left in &left_result.records {
            let left_value = self.get_join_value(&query.on.left_field, left);

            for right in &right_result.records {
                let right_value = self.get_join_value(&query.on.right_field, right);

                if left_value == right_value {
                    // Merge records
                    let mut merged = left.clone();
                    merged.nodes.extend(right.nodes.clone());
                    merged.edges.extend(right.edges.clone());
                    merged.values.extend(right.values.clone());
                    result.push(merged);
                }
            }

            // Handle outer joins
            if matches!(query.join_type, JoinType::LeftOuter) {
                // If no matches found for this left record, still include it
                if !right_result
                    .records
                    .iter()
                    .any(|r| self.get_join_value(&query.on.right_field, r) == left_value)
                {
                    result.push(left.clone());
                }
            }
        }

        Ok(result)
    }

    /// Get a value for join condition
    fn get_join_value(&self, field: &FieldRef, record: &UnifiedRecord) -> Option<Value> {
        match field {
            FieldRef::TableColumn { column, .. } => record.values.get(column).cloned(),
            FieldRef::NodeId { alias } => {
                record.nodes.get(alias).map(|n| Value::Text(n.id.clone()))
            }
            FieldRef::NodeProperty { alias, property } => {
                record
                    .nodes
                    .get(alias)
                    .and_then(|n| match property.as_str() {
                        "id" => Some(Value::Text(n.id.clone())),
                        "label" => Some(Value::Text(n.label.clone())),
                        _ => None,
                    })
            }
            FieldRef::EdgeProperty { alias, property } => {
                record
                    .edges
                    .get(alias)
                    .and_then(|e| match property.as_str() {
                        "weight" => Some(Value::Float(e.weight as f64)),
                        "from" => Some(Value::Text(e.from.clone())),
                        "to" => Some(Value::Text(e.to.clone())),
                        _ => None,
                    })
            }
        }
    }

    /// Execute a path query
    fn exec_path(&self, query: &PathQuery) -> Result<UnifiedResult, ExecutionError> {
        let mut result = UnifiedResult::empty();
        let mut stats = QueryStats::default();

        // Find start nodes
        let start_nodes = self.resolve_selector(&query.from, &mut stats)?;

        // Find target nodes
        let target_nodes: HashSet<String> = self
            .resolve_selector(&query.to, &mut stats)?
            .into_iter()
            .collect();

        // BFS to find paths
        for start_id in start_nodes {
            let paths = self.bfs_paths(
                &start_id,
                &target_nodes,
                &query.via,
                query.max_length,
                &mut stats,
            )?;

            for path in paths {
                // Apply filter if present
                if query.filter.is_some() {
                    // Path filtering would require converting path to match
                    // For now, include all paths
                }

                let mut record = UnifiedRecord::new();
                record.paths.push(path);
                result.push(record);
            }
        }

        result.stats = stats;
        Ok(result)
    }

    /// Resolve a node selector to node IDs
    fn resolve_selector(
        &self,
        selector: &NodeSelector,
        stats: &mut QueryStats,
    ) -> Result<Vec<String>, ExecutionError> {
        match selector {
            NodeSelector::ById(id) => Ok(vec![id.clone()]),
            NodeSelector::ByType { node_type, filter } => {
                let mut nodes = Vec::new();
                for node in self.graph.iter_nodes() {
                    stats.nodes_scanned += 1;
                    if node.node_type == *node_type {
                        let matches_filter = filter
                            .as_ref()
                            .map(|f| self.eval_node_property_filter(&node, f))
                            .unwrap_or(true);
                        if matches_filter {
                            nodes.push(node.id.clone());
                        }
                    }
                }
                Ok(nodes)
            }
            NodeSelector::ByRow { row_id, .. } => {
                // Use graph-table index to find linked node
                // For now, try direct lookup with table_id=0
                if let Some(node_id) = self.index.get_node_for_row(0, *row_id) {
                    Ok(vec![node_id])
                } else {
                    Ok(Vec::new())
                }
            }
        }
    }

    /// BFS to find paths between nodes
    fn bfs_paths(
        &self,
        start: &str,
        targets: &HashSet<String>,
        via: &[GraphEdgeType],
        max_length: u32,
        stats: &mut QueryStats,
    ) -> Result<Vec<GraphPath>, ExecutionError> {
        let mut paths = Vec::new();
        let mut queue: VecDeque<GraphPath> = VecDeque::new();
        let mut visited: HashSet<String> = HashSet::new();

        queue.push_back(GraphPath::start(start));
        visited.insert(start.to_string());

        while let Some(current_path) = queue.pop_front() {
            let current_node = current_path.nodes.last().unwrap();

            // Check if we've reached a target
            if targets.contains(current_node) && !current_path.is_empty() {
                paths.push(current_path.clone());
                continue;
            }

            // Don't extend beyond max length
            if current_path.len() >= max_length as usize {
                continue;
            }

            // Get outgoing edges - returns Vec<(GraphEdgeType, String, f32)>
            for (edge_type, target_id, weight) in self.graph.outgoing_edges(current_node) {
                stats.edges_scanned += 1;

                // Check edge type filter
                if !via.is_empty() && !via.contains(&edge_type) {
                    continue;
                }

                // Skip if already visited (prevent cycles)
                if visited.contains(&target_id) {
                    continue;
                }

                let edge = MatchedEdge::from_tuple(current_node, edge_type, &target_id, weight);
                let new_path = current_path.extend(edge, &target_id);
                visited.insert(target_id.clone());
                queue.push_back(new_path);
            }
        }

        Ok(paths)
    }
}

/// Internal pattern match state
#[derive(Debug, Clone, Default)]
struct PatternMatch {
    nodes: HashMap<String, MatchedNode>,
    edges: HashMap<String, MatchedEdge>,
}

impl PatternMatch {
    fn new() -> Self {
        Self::default()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_graph() -> Arc<GraphStore> {
        let graph = GraphStore::new();

        // Add some test nodes
        graph.add_node("host:192.168.1.1", "192.168.1.1", GraphNodeType::Host);
        graph.add_node("host:192.168.1.2", "192.168.1.2", GraphNodeType::Host);
        graph.add_node("svc:ssh:22", "SSH", GraphNodeType::Service);
        graph.add_node("svc:http:80", "HTTP", GraphNodeType::Service);

        // Add some edges
        graph.add_edge(
            "host:192.168.1.1",
            "svc:ssh:22",
            GraphEdgeType::HasService,
            1.0,
        );
        graph.add_edge(
            "host:192.168.1.1",
            "svc:http:80",
            GraphEdgeType::HasService,
            1.0,
        );
        graph.add_edge(
            "host:192.168.1.1",
            "host:192.168.1.2",
            GraphEdgeType::ConnectsTo,
            1.0,
        );

        Arc::new(graph)
    }

    fn create_test_index() -> Arc<GraphTableIndex> {
        Arc::new(GraphTableIndex::new())
    }

    #[test]
    fn test_simple_graph_query() {
        let graph = create_test_graph();
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        let query = QueryExpr::graph()
            .node(super::super::ast::NodePattern::new("h").of_type(GraphNodeType::Host))
            .return_field(FieldRef::node_id("h"))
            .build();

        let result = executor.execute(&query).unwrap();
        assert_eq!(result.records.len(), 2); // Two hosts
    }

    #[test]
    fn test_graph_query_with_edge() {
        let graph = create_test_graph();
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        let query = QueryExpr::graph()
            .node(super::super::ast::NodePattern::new("h").of_type(GraphNodeType::Host))
            .node(super::super::ast::NodePattern::new("s").of_type(GraphNodeType::Service))
            .edge(super::super::ast::EdgePattern::new("h", "s").of_type(GraphEdgeType::HasService))
            .return_field(FieldRef::node_id("h"))
            .return_field(FieldRef::node_id("s"))
            .build();

        let result = executor.execute(&query).unwrap();
        assert_eq!(result.records.len(), 2); // Two service connections from host 192.168.1.1
    }

    #[test]
    fn test_path_query() {
        let graph = create_test_graph();
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        let query = QueryExpr::path(
            NodeSelector::by_id("host:192.168.1.1"),
            NodeSelector::by_id("host:192.168.1.2"),
        )
        .via(GraphEdgeType::ConnectsTo)
        .max_length(5)
        .build();

        let result = executor.execute(&query).unwrap();
        assert_eq!(result.records.len(), 1); // One path
        assert_eq!(result.records[0].paths[0].nodes.len(), 2); // Two nodes in path
    }

    #[test]
    fn test_unified_result() {
        let mut result = UnifiedResult::with_columns(vec!["ip".to_string(), "port".to_string()]);

        let mut record = UnifiedRecord::new();
        record.set("ip", Value::Text("192.168.1.1".to_string()));
        record.set("port", Value::Integer(22));

        result.push(record);

        assert_eq!(result.len(), 1);
        assert_eq!(
            result.records[0].get("ip"),
            Some(&Value::Text("192.168.1.1".to_string()))
        );
    }

    #[test]
    fn test_matched_node() {
        let node = StoredNode {
            id: "host:1".to_string(),
            label: "192.168.1.1".to_string(),
            node_type: GraphNodeType::Host,
            flags: 0,
            out_edge_count: 0,
            in_edge_count: 0,
            page_id: 0,
            slot: 0,
            table_ref: None,
            vector_ref: None,
        };

        let matched = MatchedNode::from_stored(&node);
        assert_eq!(matched.id, "host:1");
        assert_eq!(matched.label, "192.168.1.1");
        assert_eq!(matched.node_type, GraphNodeType::Host);
    }

    #[test]
    fn test_graph_path() {
        let path = GraphPath::start("node:1");
        assert!(path.is_empty());
        assert_eq!(path.nodes.len(), 1);

        let edge = MatchedEdge {
            from: "node:1".to_string(),
            to: "node:2".to_string(),
            edge_type: GraphEdgeType::ConnectsTo,
            weight: 1.5,
        };

        let extended = path.extend(edge, "node:2");
        assert_eq!(extended.len(), 1);
        assert_eq!(extended.nodes.len(), 2);
        assert!((extended.total_weight - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_matched_edge_from_tuple() {
        let edge = MatchedEdge::from_tuple("node:1", GraphEdgeType::HasService, "node:2", 0.5);
        assert_eq!(edge.from, "node:1");
        assert_eq!(edge.to, "node:2");
        assert_eq!(edge.edge_type, GraphEdgeType::HasService);
        assert!((edge.weight - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_unified_record_operations() {
        let mut record = UnifiedRecord::new();

        // Test set and get
        record.set("name", Value::Text("test".to_string()));
        assert_eq!(record.get("name"), Some(&Value::Text("test".to_string())));
        assert_eq!(record.get("missing"), None);

        // Test set_node and get_node
        let node = MatchedNode {
            id: "n1".to_string(),
            label: "Node 1".to_string(),
            node_type: GraphNodeType::Host,
        };
        record.set_node("h", node.clone());
        assert_eq!(record.get_node("h").unwrap().id, "n1");
        assert!(record.get_node("missing").is_none());

        // Test set_edge
        let edge = MatchedEdge::from_tuple("a", GraphEdgeType::ConnectsTo, "b", 1.0);
        record.set_edge("e", edge);
        assert!(record.edges.contains_key("e"));
    }

    #[test]
    fn test_unified_result_empty() {
        let result = UnifiedResult::empty();
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(result.columns.is_empty());
    }

    #[test]
    fn test_path_query_no_path() {
        let graph = GraphStore::new();
        // Create disconnected nodes
        let _ = graph.add_node("a", "Node A", GraphNodeType::Host);
        let _ = graph.add_node("b", "Node B", GraphNodeType::Host);
        // No edge between them

        let graph = Arc::new(graph);
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        let query = QueryExpr::path(NodeSelector::by_id("a"), NodeSelector::by_id("b"))
            .max_length(5)
            .build();

        let result = executor.execute(&query).unwrap();
        assert!(result.is_empty()); // No path exists
    }

    #[test]
    fn test_table_query_empty() {
        let graph = create_test_graph();
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        // Table queries return empty without table storage
        let query = QueryExpr::table("hosts").build();
        let result = executor.execute(&query).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_query_stats_tracking() {
        let graph = create_test_graph();
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        let query = QueryExpr::graph()
            .node(super::super::ast::NodePattern::new("h").of_type(GraphNodeType::Host))
            .return_field(FieldRef::node_id("h"))
            .build();

        let result = executor.execute(&query).unwrap();
        // Stats should track scanned nodes
        assert!(result.stats.nodes_scanned > 0);
    }

    #[test]
    fn test_path_max_length_limit() {
        let graph = GraphStore::new();
        // Create a chain: a -> b -> c -> d
        let _ = graph.add_node("a", "A", GraphNodeType::Host);
        let _ = graph.add_node("b", "B", GraphNodeType::Host);
        let _ = graph.add_node("c", "C", GraphNodeType::Host);
        let _ = graph.add_node("d", "D", GraphNodeType::Host);
        let _ = graph.add_edge("a", "b", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("b", "c", GraphEdgeType::ConnectsTo, 1.0);
        let _ = graph.add_edge("c", "d", GraphEdgeType::ConnectsTo, 1.0);

        let graph = Arc::new(graph);
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        // max_length=2 should not reach d (3 hops away)
        let query = QueryExpr::path(NodeSelector::by_id("a"), NodeSelector::by_id("d"))
            .max_length(2)
            .build();

        let result = executor.execute(&query).unwrap();
        assert!(result.is_empty());

        // max_length=3 should reach d
        let query = QueryExpr::path(NodeSelector::by_id("a"), NodeSelector::by_id("d"))
            .max_length(3)
            .build();

        let result = executor.execute(&query).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.records[0].paths[0].len(), 3); // 3 edges
    }

    #[test]
    fn test_graph_query_empty_pattern() {
        let graph = create_test_graph();
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        // Empty pattern should return empty result
        let query = QueryExpr::Graph(GraphQuery {
            pattern: GraphPattern {
                nodes: vec![],
                edges: vec![],
            },
            filter: None,
            return_: vec![],
        });

        let result = executor.execute(&query).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_execution_error_display() {
        let err = ExecutionError::new("Test error");
        assert_eq!(format!("{}", err), "Execution error: Test error");
    }

    #[test]
    fn test_graph_path_multi_hop() {
        let path = GraphPath::start("a");

        let edge1 = MatchedEdge::from_tuple("a", GraphEdgeType::ConnectsTo, "b", 1.0);
        let path = path.extend(edge1, "b");

        let edge2 = MatchedEdge::from_tuple("b", GraphEdgeType::ConnectsTo, "c", 2.0);
        let path = path.extend(edge2, "c");

        assert_eq!(path.len(), 2);
        assert_eq!(path.nodes.len(), 3);
        assert_eq!(path.nodes, vec!["a", "b", "c"]);
        assert!((path.total_weight - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_node_selector_by_type() {
        let graph = GraphStore::new();
        let _ = graph.add_node("host:1", "Host 1", GraphNodeType::Host);
        let _ = graph.add_node("host:2", "Host 2", GraphNodeType::Host);
        let _ = graph.add_node("svc:1", "Service 1", GraphNodeType::Service);

        let graph = Arc::new(graph);
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        // Path from any host to any service
        let query = QueryExpr::path(
            NodeSelector::by_type(GraphNodeType::Host),
            NodeSelector::by_type(GraphNodeType::Service),
        )
        .max_length(1)
        .build();

        // No edges, so no paths
        let result = executor.execute(&query).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_join_query_execution() {
        let graph = create_test_graph();
        let index = create_test_index();
        let executor = UnifiedExecutor::new(graph, index);

        // Join two graph queries
        let left = QueryExpr::graph()
            .node(super::super::ast::NodePattern::new("h").of_type(GraphNodeType::Host))
            .return_field(FieldRef::node_id("h"))
            .build();

        let right = QueryExpr::graph()
            .node(super::super::ast::NodePattern::new("s").of_type(GraphNodeType::Service))
            .return_field(FieldRef::node_id("s"))
            .build();

        // Create join - both return results but no matching condition
        let join = QueryExpr::Join(JoinQuery {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            on: super::super::ast::JoinCondition {
                left_field: FieldRef::node_prop("h", "id"),
                right_field: FieldRef::node_prop("s", "id"),
            },
        });

        let result = executor.execute(&join).unwrap();
        // No matches because host ids != service ids
        assert!(result.is_empty());
    }
}
