use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::{
    ExecutionError, GraphPath, MatchedEdge, MatchedNode, QueryStats, UnifiedRecord, UnifiedResult,
};
use crate::storage::engine::graph_store::{GraphEdgeType, GraphStore, StoredNode};
use crate::storage::engine::graph_table_index::GraphTableIndex;
use crate::storage::query::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, Filter, GraphPattern, GraphQuery, JoinQuery,
    JoinType, NodePattern, NodeSelector, PathQuery, Projection, QueryExpr, TableQuery,
};
use crate::storage::query::sql_lowering::{
    effective_graph_filter, effective_graph_projections, effective_path_filter,
};
use crate::storage::schema::Value;

pub struct UnifiedExecutor {
    /// Graph storage
    graph: Arc<GraphStore>,
    /// Graph-table index for joins
    index: Arc<GraphTableIndex>,
    /// Optional node properties loaded from the unified entity store
    node_properties: Arc<HashMap<String, HashMap<String, Value>>>,
}

impl UnifiedExecutor {
    /// Create a new executor
    pub fn new(graph: Arc<GraphStore>, index: Arc<GraphTableIndex>) -> Self {
        Self::new_with_node_properties(graph, index, HashMap::new())
    }

    /// Create a new executor with node properties
    pub fn new_with_node_properties(
        graph: Arc<GraphStore>,
        index: Arc<GraphTableIndex>,
        node_properties: HashMap<String, HashMap<String, Value>>,
    ) -> Self {
        Self {
            graph,
            index,
            node_properties: Arc::new(node_properties),
        }
    }

    fn matched_node(&self, node: &StoredNode) -> MatchedNode {
        let mut node = MatchedNode::from_stored(node);
        if let Some(properties) = self.node_properties.get(&node.id) {
            node.properties = properties.clone();
        }
        node
    }

    fn node_stored_property_value(node: &StoredNode, property: &str) -> Option<Value> {
        if let Some(properties) = match property {
            "id" => Some(Value::Text(node.id.clone())),
            "label" => Some(Value::Text(node.label.clone())),
            "type" | "node_type" => Some(Value::Text(format!("{:?}", node.node_type))),
            _ => None,
        } {
            return Some(properties);
        }

        None
    }

    fn node_property_value(&self, node: &StoredNode, property: &str) -> Option<Value> {
        self.node_properties
            .get(&node.id)
            .and_then(|properties| properties.get(property).cloned())
            .or_else(|| Self::node_stored_property_value(node, property))
    }

    fn node_property_value_by_id(&self, node_id: &str, property: &str) -> Option<Value> {
        if property == "id" {
            return Some(Value::Text(node_id.to_string()));
        }
        if property == "label" {
            if let Some(node) = self.graph.get_node(node_id).as_ref() {
                return Some(Value::Text(node.label.clone()));
            }
            return None;
        }
        if property == "type" || property == "node_type" {
            return self
                .graph
                .get_node(node_id)
                .map(|node| Value::Text(format!("{:?}", node.node_type)));
        }
        self.node_properties
            .get(node_id)
            .and_then(|properties| properties.get(property).cloned())
    }

    /// Execute a query directly against a graph reference
    ///
    /// This is a convenience method for simple graph-only queries.
    /// For table joins, use `new()` with proper Arc ownership.
    pub fn execute_on(
        graph: &GraphStore,
        query: &QueryExpr,
    ) -> Result<UnifiedResult, ExecutionError> {
        Self::execute_on_with_node_properties(graph, query, HashMap::new())
    }

    /// Execute a query directly against a graph reference with custom node properties
    pub fn execute_on_with_node_properties(
        graph: &GraphStore,
        query: &QueryExpr,
        node_properties: HashMap<String, HashMap<String, Value>>,
    ) -> Result<UnifiedResult, ExecutionError> {
        let temp = Self::new_with_node_properties(
            Arc::new(GraphStore::new()),
            Arc::new(GraphTableIndex::new()),
            node_properties,
        );

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
            QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)
            | QueryExpr::CreateIndex(_)
            | QueryExpr::DropIndex(_)
            | QueryExpr::ProbabilisticCommand(_)
            | QueryExpr::Ask(_)
            | QueryExpr::SetConfig { .. }
            | QueryExpr::ShowConfig { .. }
            | QueryExpr::SetTenant(_)
            | QueryExpr::ShowTenant
            | QueryExpr::CreateTimeSeries(_)
            | QueryExpr::DropTimeSeries(_)
            | QueryExpr::CreateQueue(_)
            | QueryExpr::DropQueue(_)
            | QueryExpr::QueueCommand(_)
            | QueryExpr::CreateTree(_)
            | QueryExpr::DropTree(_)
            | QueryExpr::TreeCommand(_)
            | QueryExpr::ExplainAlter(_)
            | QueryExpr::TransactionControl(_)
            | QueryExpr::MaintenanceCommand(_)
            | QueryExpr::CreateSchema(_)
            | QueryExpr::DropSchema(_)
            | QueryExpr::CreateSequence(_)
            | QueryExpr::DropSequence(_)
            | QueryExpr::CopyFrom(_)
            | QueryExpr::CreateView(_)
            | QueryExpr::DropView(_)
            | QueryExpr::RefreshMaterializedView(_)
            | QueryExpr::CreatePolicy(_)
            | QueryExpr::DropPolicy(_)
            | QueryExpr::CreateServer(_)
            | QueryExpr::DropServer(_)
            | QueryExpr::CreateForeignTable(_)
            | QueryExpr::DropForeignTable(_) => Err(ExecutionError::new(
                "DML/DDL/Command statements are not supported in UnifiedExecutor",
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
                graph.nodes_of_type(*node_type)
            } else {
                graph.iter_nodes().collect()
            };

            // Filter and add matching nodes
            for node in matching_nodes {
                let mut matches = true;
                for prop_filter in &pattern_node.properties {
                    if !self.eval_node_property_filter(&node, prop_filter) {
                        matches = false;
                        break;
                    }
                }

                if matches {
                    let mut record = UnifiedRecord::new();
                    record.set_node(&pattern_node.alias, self.matched_node(&node));
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
                .nodes_of_type(*node_type)
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
                            let table_ref = node.table_ref?;
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
            QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)
            | QueryExpr::CreateIndex(_)
            | QueryExpr::DropIndex(_)
            | QueryExpr::ProbabilisticCommand(_)
            | QueryExpr::Ask(_)
            | QueryExpr::SetConfig { .. }
            | QueryExpr::ShowConfig { .. }
            | QueryExpr::SetTenant(_)
            | QueryExpr::ShowTenant
            | QueryExpr::CreateTimeSeries(_)
            | QueryExpr::DropTimeSeries(_)
            | QueryExpr::CreateQueue(_)
            | QueryExpr::DropQueue(_)
            | QueryExpr::QueueCommand(_)
            | QueryExpr::CreateTree(_)
            | QueryExpr::DropTree(_)
            | QueryExpr::TreeCommand(_)
            | QueryExpr::ExplainAlter(_)
            | QueryExpr::TransactionControl(_)
            | QueryExpr::MaintenanceCommand(_)
            | QueryExpr::CreateSchema(_)
            | QueryExpr::DropSchema(_)
            | QueryExpr::CreateSequence(_)
            | QueryExpr::DropSequence(_)
            | QueryExpr::CopyFrom(_)
            | QueryExpr::CreateView(_)
            | QueryExpr::DropView(_)
            | QueryExpr::RefreshMaterializedView(_)
            | QueryExpr::CreatePolicy(_)
            | QueryExpr::DropPolicy(_)
            | QueryExpr::CreateServer(_)
            | QueryExpr::DropServer(_)
            | QueryExpr::CreateForeignTable(_)
            | QueryExpr::DropForeignTable(_) => Err(ExecutionError::new(
                "DML/DDL/Command statements are not supported in UnifiedExecutor",
            )),
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
        let effective_filter = effective_graph_filter(query);
        let effective_projections = effective_graph_projections(query);
        let filtered: Vec<_> = matches
            .into_iter()
            .filter(|m| self.eval_filter_on_match(&effective_filter, m))
            .collect();

        // Build result records with projections
        for matched in filtered {
            let record = self.project_match(&matched, &effective_projections);
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

            // Check property filters
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
                    .insert(pattern.alias.clone(), self.matched_node(&node));
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
                            self.matched_node(&target_node),
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
    fn eval_node_property_filter(
        &self,
        node: &StoredNode,
        filter: &crate::storage::query::ast::PropertyFilter,
    ) -> bool {
        let Some(value) = self.node_property_value(node, filter.name.as_str()) else {
            return false;
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
            Filter::CompareFields { left, op, right } => {
                let l = self.get_field_value(left, matched);
                let r = self.get_field_value(right, matched);
                match (l, r) {
                    (Some(lv), Some(rv)) => self.compare_values(&lv, op, &rv),
                    _ => false,
                }
            }
            Filter::CompareExpr { .. } => {
                // The unified graph-level executor doesn't yet carry
                // the `UnifiedRecord` context that expr_eval needs.
                // Return false (conservative — the predicate is
                // treated as unmatched) until the executor is
                // upgraded in Week 5.
                false
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

    /// Simple LIKE pattern matching (% and _ wildcards)
    fn match_like(&self, text: &str, pattern: &str) -> bool {
        // Simple implementation: convert % to .* and _ to .
        let regex_pattern = pattern.replace('%', ".*").replace('_', ".");

        // Basic match without regex (for simplicity)
        if pattern.starts_with('%') && pattern.ends_with('%') {
            let inner = &pattern[1..pattern.len() - 1];
            text.contains(inner)
        } else if let Some(suffix) = pattern.strip_prefix('%') {
            text.ends_with(suffix)
        } else if let Some(prefix) = pattern.strip_suffix('%') {
            text.starts_with(prefix)
        } else {
            text == pattern || regex_pattern == text
        }
    }

    /// Get a field value from a pattern match
    fn get_field_value(&self, field: &FieldRef, matched: &PatternMatch) -> Option<Value> {
        match field {
            FieldRef::NodeId { alias } => {
                matched.nodes.get(alias).map(|n| Value::Text(n.id.clone()))
            }
            FieldRef::NodeProperty { alias, property } => {
                matched
                    .nodes
                    .get(alias)
                    .and_then(|n| match property.as_str() {
                        "id" => Some(Value::Text(n.id.clone())),
                        "label" => Some(Value::Text(n.label.clone())),
                        "type" | "node_type" => Some(Value::Text(format!("{:?}", n.node_type))),
                        _ => n.properties.get(property).cloned(),
                    })
            }
            FieldRef::EdgeProperty { alias, property } => {
                matched
                    .edges
                    .get(alias)
                    .and_then(|e| match property.as_str() {
                        "weight" => Some(Value::Float(e.weight as f64)),
                        "from" => Some(Value::Text(e.from.clone())),
                        "to" => Some(Value::Text(e.to.clone())),
                        _ => None,
                    })
            }
            FieldRef::TableColumn { .. } => {
                // Table columns not available in graph-only match
                None
            }
        }
    }

    /// Get a value for join condition
    fn get_join_value(&self, field: &FieldRef, record: &UnifiedRecord) -> Option<Value> {
        match field {
            FieldRef::TableColumn { column, .. } => record.values.get(column.as_str()).cloned(),
            FieldRef::NodeId { alias } => record
                .nodes
                .get(alias)
                .map(|node| Value::Text(node.id.clone())),
            FieldRef::NodeProperty { alias, property } => {
                record
                    .nodes
                    .get(alias)
                    .and_then(|n| match property.as_str() {
                        "id" => Some(Value::Text(n.id.clone())),
                        "label" => Some(Value::Text(n.label.clone())),
                        "type" | "node_type" => Some(Value::Text(format!("{:?}", n.node_type))),
                        _ => n.properties.get(property).cloned(),
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

    /// Get an index-agnostic view of matched records for projections
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
                    for node in matched.nodes.values() {
                        match col.as_str() {
                            "id" => record.set(col, Value::Text(node.id.clone())),
                            "label" => record.set(col, Value::Text(node.label.clone())),
                            _ => {}
                        }
                    }
                }
                Projection::Alias(col, alias) => {
                    for node in matched.nodes.values() {
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
                if effective_path_filter(query).is_some() {
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
            let Some(current_node) = current_path.nodes.last() else {
                continue;
            };

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
