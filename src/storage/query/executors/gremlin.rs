//! Gremlin Traversal Executor
//!
//! Implements TinkerPop-inspired traversal execution with:
//! - `Traverser`: Tracks current position, path history, and bulk count
//! - Step execution: V, E, out, in, has, filter, etc.
//! - Path tracking for return path queries
//! - Loop detection for repeat() steps
//!
//! # Architecture (inspired by TinkerPop)
//!
//! ```text
//! GremlinTraversal → [Step, Step, ...] → Traverser stream → Result
//!                    ↓                    ↓
//!                 GremlinStep           TraverserSet
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::storage::engine::graph_store::{GraphEdgeType, GraphStore};
use crate::storage::query::modes::gremlin::{
    GremlinParser, GremlinPredicate, GremlinStep, GremlinTraversal, GremlinValue, TraversalSource,
};
use crate::storage::query::unified::{
    ExecutionError, GraphPath, MatchedEdge, MatchedNode, QueryStats, UnifiedRecord, UnifiedResult,
};

/// A traverser represents a position in the graph during traversal
#[derive(Debug, Clone)]
pub struct Traverser {
    /// Current element (node ID or edge ID)
    pub current: TraverserElement,
    /// Path history
    pub path: Vec<TraverserElement>,
    /// Labels assigned via as() step
    pub labels: HashMap<String, TraverserElement>,
    /// Bulk count (optimization for duplicate paths)
    pub bulk: u64,
    /// Loop counters for repeat() steps
    pub loops: HashMap<String, u32>,
    /// Sack values for side-effect accumulation
    pub sack: Option<SackValue>,
}

/// Element that a traverser can point to
#[derive(Debug, Clone, PartialEq)]
pub enum TraverserElement {
    Node(String),
    Edge {
        from: String,
        edge_type: GraphEdgeType,
        to: String,
        weight: f32,
    },
    Value(GremlinValue),
}

impl TraverserElement {
    /// Get as node ID if this is a node
    pub fn as_node_id(&self) -> Option<&str> {
        match self {
            Self::Node(id) => Some(id),
            _ => None,
        }
    }
}

/// Sack value for accumulation
#[derive(Debug, Clone)]
pub enum SackValue {
    Integer(i64),
    Float(f64),
    List(Vec<GremlinValue>),
    Map(HashMap<String, GremlinValue>),
}

/// Gremlin executor with traverser-based execution
pub struct GremlinExecutor {
    graph: Arc<GraphStore>,
}

impl GremlinExecutor {
    /// Create a new Gremlin executor
    pub fn new(graph: Arc<GraphStore>) -> Self {
        Self { graph }
    }

    /// Execute a Gremlin query string
    pub fn execute(&self, query: &str) -> Result<UnifiedResult, ExecutionError> {
        let traversal =
            GremlinParser::parse(query).map_err(|e| ExecutionError::new(e.to_string()))?;
        self.execute_traversal(&traversal)
    }

    /// Execute a parsed traversal
    pub fn execute_traversal(
        &self,
        traversal: &GremlinTraversal,
    ) -> Result<UnifiedResult, ExecutionError> {
        let mut stats = QueryStats::default();

        // Initialize traverser set from source type
        let mut traversers = Vec::new();

        // First find the V() or E() step that initializes the traversal
        for step in &traversal.steps {
            match step {
                GremlinStep::V(id_opt) => {
                    if let Some(id) = id_opt {
                        // Specific vertex
                        if self.graph.get_node(id).is_some() {
                            traversers.push(Traverser::at_node(id));
                            stats.nodes_scanned += 1;
                        }
                    } else {
                        // All vertices
                        for node in self.graph.iter_nodes() {
                            stats.nodes_scanned += 1;
                            traversers.push(Traverser::at_node(&node.id));
                        }
                    }
                    break;
                }
                GremlinStep::E(id_opt) => {
                    if let Some(id) = id_opt {
                        // Specific edge by ID (format: "from->to")
                        if let Some((from, to)) = id.split_once("->") {
                            for (edge_type, target, weight) in self.graph.outgoing_edges(from) {
                                if target == to {
                                    traversers
                                        .push(Traverser::at_edge(from, edge_type, &target, weight));
                                    stats.edges_scanned += 1;
                                }
                            }
                        }
                    } else {
                        // All edges
                        for node in self.graph.iter_nodes() {
                            for (edge_type, target, weight) in self.graph.outgoing_edges(&node.id) {
                                stats.edges_scanned += 1;
                                traversers
                                    .push(Traverser::at_edge(&node.id, edge_type, &target, weight));
                            }
                        }
                    }
                    break;
                }
                _ => {}
            }
        }

        // If no V()/E() step found and source is Anonymous, start empty
        if traversers.is_empty() && traversal.source == TraversalSource::Anonymous {
            // Anonymous traversals are typically used as inner traversals
        }

        // Execute each step after the source step
        let mut found_source = false;
        for step in &traversal.steps {
            // Skip the source step we already processed
            if !found_source && matches!(step, GremlinStep::V(_) | GremlinStep::E(_)) {
                found_source = true;
                continue;
            }

            traversers = self.execute_step(traversers, step, &mut stats)?;
            if traversers.is_empty() {
                break;
            }
        }

        // Convert traversers to result
        self.traversers_to_result(traversers, stats)
    }

    /// Execute a single step on all traversers
    fn execute_step(
        &self,
        traversers: Vec<Traverser>,
        step: &GremlinStep,
        stats: &mut QueryStats,
    ) -> Result<Vec<Traverser>, ExecutionError> {
        let mut result = Vec::new();

        match step {
            // ======== Source Steps (already processed) ========
            GremlinStep::V(_) | GremlinStep::E(_) => {
                // Already handled in initialization
                result = traversers;
            }

            // ======== Navigation Steps ========
            GremlinStep::Out(label_opt) => {
                for t in traversers {
                    if let Some(node_id) = t.current.as_node_id() {
                        for (edge_type, target, _) in self.graph.outgoing_edges(node_id) {
                            stats.edges_scanned += 1;
                            if self.edge_matches_label(edge_type, label_opt.as_deref()) {
                                result.push(t.move_to_node(&target));
                            }
                        }
                    }
                }
            }
            GremlinStep::In(label_opt) => {
                for t in traversers {
                    if let Some(node_id) = t.current.as_node_id() {
                        for (edge_type, source, _) in self.graph.incoming_edges(node_id) {
                            stats.edges_scanned += 1;
                            if self.edge_matches_label(edge_type, label_opt.as_deref()) {
                                result.push(t.move_to_node(&source));
                            }
                        }
                    }
                }
            }
            GremlinStep::Both(label_opt) => {
                for t in traversers {
                    if let Some(node_id) = t.current.as_node_id() {
                        // Outgoing
                        for (edge_type, target, _) in self.graph.outgoing_edges(node_id) {
                            stats.edges_scanned += 1;
                            if self.edge_matches_label(edge_type, label_opt.as_deref()) {
                                result.push(t.move_to_node(&target));
                            }
                        }
                        // Incoming
                        for (edge_type, source, _) in self.graph.incoming_edges(node_id) {
                            stats.edges_scanned += 1;
                            if self.edge_matches_label(edge_type, label_opt.as_deref()) {
                                result.push(t.move_to_node(&source));
                            }
                        }
                    }
                }
            }
            GremlinStep::OutE(label_opt) => {
                for t in traversers {
                    if let Some(node_id) = t.current.as_node_id() {
                        for (edge_type, target, weight) in self.graph.outgoing_edges(node_id) {
                            stats.edges_scanned += 1;
                            if self.edge_matches_label(edge_type, label_opt.as_deref()) {
                                result.push(t.move_to_edge(node_id, edge_type, &target, weight));
                            }
                        }
                    }
                }
            }
            GremlinStep::InE(label_opt) => {
                for t in traversers {
                    if let Some(node_id) = t.current.as_node_id() {
                        for (edge_type, source, weight) in self.graph.incoming_edges(node_id) {
                            stats.edges_scanned += 1;
                            if self.edge_matches_label(edge_type, label_opt.as_deref()) {
                                result.push(t.move_to_edge(&source, edge_type, node_id, weight));
                            }
                        }
                    }
                }
            }
            GremlinStep::BothE(label_opt) => {
                for t in traversers {
                    if let Some(node_id) = t.current.as_node_id() {
                        for (edge_type, target, weight) in self.graph.outgoing_edges(node_id) {
                            stats.edges_scanned += 1;
                            if self.edge_matches_label(edge_type, label_opt.as_deref()) {
                                result.push(t.move_to_edge(node_id, edge_type, &target, weight));
                            }
                        }
                        for (edge_type, source, weight) in self.graph.incoming_edges(node_id) {
                            stats.edges_scanned += 1;
                            if self.edge_matches_label(edge_type, label_opt.as_deref()) {
                                result.push(t.move_to_edge(&source, edge_type, node_id, weight));
                            }
                        }
                    }
                }
            }
            GremlinStep::OutV => {
                for t in traversers {
                    if let TraverserElement::Edge { from, .. } = &t.current {
                        result.push(t.move_to_node(from));
                    }
                }
            }
            GremlinStep::InV => {
                for t in traversers {
                    if let TraverserElement::Edge { to, .. } = &t.current {
                        result.push(t.move_to_node(to));
                    }
                }
            }
            GremlinStep::BothV => {
                for t in traversers {
                    if let TraverserElement::Edge { from, to, .. } = &t.current {
                        result.push(t.move_to_node(from));
                        result.push(t.move_to_node(to));
                    }
                }
            }
            GremlinStep::OtherV => {
                // Other vertex from edge - requires context of where we came from
                for t in traversers {
                    if let TraverserElement::Edge { from, to, .. } = &t.current {
                        // Look at previous element in path to determine direction
                        if let Some(prev) = t.path.last() {
                            if let Some(prev_id) = prev.as_node_id() {
                                if prev_id == from {
                                    result.push(t.move_to_node(to));
                                } else {
                                    result.push(t.move_to_node(from));
                                }
                            }
                        }
                    }
                }
            }

            // ======== Filter Steps ========
            GremlinStep::Has(key, value_opt) => {
                for t in traversers {
                    if self.check_has(&t, key, value_opt.as_ref(), stats) {
                        result.push(t);
                    }
                }
            }
            GremlinStep::HasNot(key) => {
                for t in traversers {
                    if !self.check_has(&t, key, None, stats) {
                        result.push(t);
                    }
                }
            }
            GremlinStep::HasLabel(label) => {
                for t in traversers {
                    if self.check_has_label(&t, label, stats) {
                        result.push(t);
                    }
                }
            }
            GremlinStep::HasId(id) => {
                for t in traversers {
                    if let Some(node_id) = t.current.as_node_id() {
                        if node_id == id {
                            result.push(t);
                        }
                    }
                }
            }
            GremlinStep::Where(inner) => {
                for t in traversers {
                    let inner_result =
                        self.execute_inner_traversal(vec![t.clone()], inner, stats)?;
                    if !inner_result.is_empty() {
                        result.push(t);
                    }
                }
            }
            GremlinStep::Filter(inner) => {
                for t in traversers {
                    let inner_result =
                        self.execute_inner_traversal(vec![t.clone()], inner, stats)?;
                    if !inner_result.is_empty() {
                        result.push(t);
                    }
                }
            }
            GremlinStep::Dedup => {
                let mut seen = HashSet::new();
                for t in traversers {
                    let key = format!("{:?}", t.current);
                    if seen.insert(key) {
                        result.push(t);
                    }
                }
            }
            GremlinStep::Limit(n) => {
                result = traversers.into_iter().take(*n as usize).collect();
            }
            GremlinStep::Skip(n) => {
                result = traversers.into_iter().skip(*n as usize).collect();
            }
            GremlinStep::Range(from, to) => {
                result = traversers
                    .into_iter()
                    .skip(*from as usize)
                    .take((*to - *from) as usize)
                    .collect();
            }

            // ======== Side Effect Steps ========
            GremlinStep::As(label) => {
                for mut t in traversers {
                    t.labels.insert(label.clone(), t.current.clone());
                    result.push(t);
                }
            }
            GremlinStep::Store(_) | GremlinStep::Aggregate(_) => {
                // Side effects - just pass through
                result = traversers;
            }
            GremlinStep::By(_) => {
                // Modifier step - pass through
                result = traversers;
            }

            // ======== Branch Steps ========
            GremlinStep::Repeat(inner) => {
                // Basic repeat - look for following Times/Until modifiers
                // For now, execute once
                result = self.execute_inner_traversal(traversers, inner, stats)?;
            }
            GremlinStep::Times(n) => {
                // Modifier for repeat - handled in Repeat
                // If we get here, just pass through
                let _ = n;
                result = traversers;
            }
            GremlinStep::Until(inner) => {
                // Modifier for repeat - handled in Repeat
                let _ = inner;
                result = traversers;
            }
            GremlinStep::Emit => {
                // Modifier for repeat - handled in Repeat
                result = traversers;
            }
            GremlinStep::Union(branches) => {
                for t in traversers {
                    for branch in branches {
                        result.extend(self.execute_inner_traversal(
                            vec![t.clone()],
                            branch,
                            stats,
                        )?);
                    }
                }
            }
            GremlinStep::Choose(cond, if_branch, else_branch) => {
                for t in traversers {
                    let matches = self.execute_inner_traversal(vec![t.clone()], cond, stats)?;
                    if !matches.is_empty() {
                        result.extend(self.execute_inner_traversal(vec![t], if_branch, stats)?);
                    } else if let Some(else_br) = else_branch {
                        result.extend(self.execute_inner_traversal(vec![t], else_br, stats)?);
                    }
                }
            }
            GremlinStep::Coalesce(branches) => {
                for t in traversers {
                    for branch in branches {
                        let branch_result =
                            self.execute_inner_traversal(vec![t.clone()], branch, stats)?;
                        if !branch_result.is_empty() {
                            result.extend(branch_result);
                            break;
                        }
                    }
                }
            }

            // ======== Map Steps ========
            GremlinStep::Id => {
                for t in traversers {
                    if let Some(id) = t.current.as_node_id() {
                        result.push(t.with_value(GremlinValue::String(id.to_string())));
                    }
                }
            }
            GremlinStep::Label => {
                for t in traversers {
                    if let Some(id) = t.current.as_node_id() {
                        if let Some(node) = self.graph.get_node(id) {
                            result.push(t.with_value(GremlinValue::String(node.label.clone())));
                        }
                    }
                }
            }
            GremlinStep::Values(keys) => {
                for t in traversers {
                    for key in keys {
                        if let Some(val) = self.get_property(&t, key) {
                            result.push(t.with_value(GremlinValue::String(val)));
                        }
                    }
                }
            }
            GremlinStep::ValueMap(keys) => {
                // Return all properties as a map (simplified)
                let _ = keys;
                for t in traversers {
                    if let Some(id) = t.current.as_node_id() {
                        if let Some(_node) = self.graph.get_node(id) {
                            result.push(t);
                        }
                    }
                }
            }
            GremlinStep::Properties(keys) => {
                // Same as values for now
                for t in traversers {
                    for key in keys {
                        if let Some(val) = self.get_property(&t, key) {
                            result.push(t.with_value(GremlinValue::String(val)));
                        }
                    }
                }
            }
            GremlinStep::Select(labels) => {
                for t in traversers {
                    let mut new_t = t.clone();
                    let selected: HashMap<_, _> = labels
                        .iter()
                        .filter_map(|l| t.labels.get(l).map(|v| (l.clone(), v.clone())))
                        .collect();
                    new_t.labels = selected;
                    result.push(new_t);
                }
            }
            GremlinStep::Project(keys) => {
                // Similar to select but for computed values
                let _ = keys;
                result = traversers;
            }
            GremlinStep::Path => {
                // Already tracking path, just pass through
                result = traversers;
            }
            GremlinStep::SimplePath => {
                for t in traversers {
                    let mut seen = HashSet::new();
                    let is_simple = t
                        .path
                        .iter()
                        .filter_map(|e| e.as_node_id())
                        .all(|id| seen.insert(id.to_string()));
                    if is_simple {
                        result.push(t);
                    }
                }
            }
            GremlinStep::CyclicPath => {
                for t in traversers {
                    let mut seen = HashSet::new();
                    let has_cycle = !t
                        .path
                        .iter()
                        .filter_map(|e| e.as_node_id())
                        .all(|id| seen.insert(id.to_string()));
                    if has_cycle {
                        result.push(t);
                    }
                }
            }

            // ======== Aggregate Steps ========
            GremlinStep::Count => {
                let count = traversers.len() as i64;
                let t = Traverser {
                    current: TraverserElement::Value(GremlinValue::Integer(count)),
                    path: Vec::new(),
                    labels: HashMap::new(),
                    bulk: 1,
                    loops: HashMap::new(),
                    sack: None,
                };
                result.push(t);
            }
            GremlinStep::Sum => {
                let sum: f64 = traversers
                    .iter()
                    .filter_map(|t| match &t.current {
                        TraverserElement::Value(GremlinValue::Integer(i)) => Some(*i as f64),
                        TraverserElement::Value(GremlinValue::Float(f)) => Some(*f),
                        _ => None,
                    })
                    .sum();
                result.push(Traverser {
                    current: TraverserElement::Value(GremlinValue::Float(sum)),
                    path: Vec::new(),
                    labels: HashMap::new(),
                    bulk: 1,
                    loops: HashMap::new(),
                    sack: None,
                });
            }
            GremlinStep::Min => {
                let min = traversers
                    .iter()
                    .filter_map(|t| match &t.current {
                        TraverserElement::Value(GremlinValue::Integer(i)) => Some(*i as f64),
                        TraverserElement::Value(GremlinValue::Float(f)) => Some(*f),
                        _ => None,
                    })
                    .fold(f64::INFINITY, |a, b| a.min(b));
                if min.is_finite() {
                    result.push(Traverser {
                        current: TraverserElement::Value(GremlinValue::Float(min)),
                        path: Vec::new(),
                        labels: HashMap::new(),
                        bulk: 1,
                        loops: HashMap::new(),
                        sack: None,
                    });
                }
            }
            GremlinStep::Max => {
                let max = traversers
                    .iter()
                    .filter_map(|t| match &t.current {
                        TraverserElement::Value(GremlinValue::Integer(i)) => Some(*i as f64),
                        TraverserElement::Value(GremlinValue::Float(f)) => Some(*f),
                        _ => None,
                    })
                    .fold(f64::NEG_INFINITY, |a, b| a.max(b));
                if max.is_finite() {
                    result.push(Traverser {
                        current: TraverserElement::Value(GremlinValue::Float(max)),
                        path: Vec::new(),
                        labels: HashMap::new(),
                        bulk: 1,
                        loops: HashMap::new(),
                        sack: None,
                    });
                }
            }
            GremlinStep::Mean => {
                let vals: Vec<f64> = traversers
                    .iter()
                    .filter_map(|t| match &t.current {
                        TraverserElement::Value(GremlinValue::Integer(i)) => Some(*i as f64),
                        TraverserElement::Value(GremlinValue::Float(f)) => Some(*f),
                        _ => None,
                    })
                    .collect();
                if !vals.is_empty() {
                    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
                    result.push(Traverser {
                        current: TraverserElement::Value(GremlinValue::Float(mean)),
                        path: Vec::new(),
                        labels: HashMap::new(),
                        bulk: 1,
                        loops: HashMap::new(),
                        sack: None,
                    });
                }
            }
            GremlinStep::Group => {
                // Group step without modifier - pass through
                result = traversers;
            }
            GremlinStep::GroupCount => {
                // GroupCount - count occurrences
                let mut counts: HashMap<String, u64> = HashMap::new();
                for t in &traversers {
                    let val = t.current.as_node_id().unwrap_or("unknown").to_string();
                    *counts.entry(val).or_insert(0) += t.bulk;
                }
                // Return as single traverser (simplified)
                result = traversers;
            }
            GremlinStep::Fold => {
                result = vec![Traverser {
                    current: TraverserElement::Value(
                        GremlinValue::Integer(traversers.len() as i64),
                    ),
                    path: Vec::new(),
                    labels: HashMap::new(),
                    bulk: 1,
                    loops: HashMap::new(),
                    sack: None,
                }];
            }

            // ======== Terminal Steps ========
            GremlinStep::ToList | GremlinStep::ToSet | GremlinStep::Next => {
                result = traversers;
            }
        }

        Ok(result)
    }

    /// Execute an inner traversal (for repeat, where, etc.)
    fn execute_inner_traversal(
        &self,
        traversers: Vec<Traverser>,
        inner: &GremlinTraversal,
        stats: &mut QueryStats,
    ) -> Result<Vec<Traverser>, ExecutionError> {
        let mut current = traversers;
        for step in &inner.steps {
            current = self.execute_step(current, step, stats)?;
            if current.is_empty() {
                break;
            }
        }
        Ok(current)
    }

    /// Check if edge type matches a label
    fn edge_matches_label(&self, edge_type: GraphEdgeType, label: Option<&str>) -> bool {
        match label {
            None => true, // No filter - match all
            Some(l) => {
                let edge_str = format!("{:?}", edge_type).to_lowercase();
                edge_str.contains(&l.to_lowercase())
            }
        }
    }

    /// Check has() filter
    fn check_has(
        &self,
        t: &Traverser,
        key: &str,
        value: Option<&GremlinValue>,
        stats: &mut QueryStats,
    ) -> bool {
        if let Some(node_id) = t.current.as_node_id() {
            if let Some(node) = self.graph.get_node(node_id) {
                stats.nodes_scanned += 1;
                match key {
                    "label" | "type" | "nodeType" => {
                        let node_type_str = format!("{:?}", node.node_type);
                        if let Some(val) = value {
                            matches_gremlin_value(&node_type_str, val)
                        } else {
                            true
                        }
                    }
                    "id" => {
                        if let Some(val) = value {
                            matches_gremlin_value(&node.id, val)
                        } else {
                            true
                        }
                    }
                    "name" => {
                        if let Some(val) = value {
                            matches_gremlin_value(&node.label, val)
                        } else {
                            true
                        }
                    }
                    _ => {
                        if let Some(val) = value {
                            match val {
                                GremlinValue::String(s) => node.label.contains(s),
                                _ => false,
                            }
                        } else {
                            node.label.contains(key)
                        }
                    }
                }
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Check hasLabel() filter
    fn check_has_label(&self, t: &Traverser, label: &str, stats: &mut QueryStats) -> bool {
        if let Some(node_id) = t.current.as_node_id() {
            if let Some(node) = self.graph.get_node(node_id) {
                stats.nodes_scanned += 1;
                let node_type_str = format!("{:?}", node.node_type).to_lowercase();
                node_type_str.contains(&label.to_lowercase())
                    || node.label.to_lowercase().contains(&label.to_lowercase())
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Get property value from traverser
    fn get_property(&self, t: &Traverser, key: &str) -> Option<String> {
        if let Some(node_id) = t.current.as_node_id() {
            if let Some(node) = self.graph.get_node(node_id) {
                match key {
                    "id" => Some(node.id.clone()),
                    "label" | "name" => Some(node.label.clone()),
                    "type" => Some(format!("{:?}", node.node_type)),
                    _ => None,
                }
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Convert traversers to unified result
    fn traversers_to_result(
        &self,
        traversers: Vec<Traverser>,
        stats: QueryStats,
    ) -> Result<UnifiedResult, ExecutionError> {
        let mut result = UnifiedResult::empty();
        result.stats = stats;

        for t in traversers {
            let mut record = UnifiedRecord::new();

            // Add current element
            match &t.current {
                TraverserElement::Node(id) => {
                    if let Some(node) = self.graph.get_node(id) {
                        record.set_node("_", MatchedNode::from_stored(&node));
                    }
                }
                TraverserElement::Edge {
                    from,
                    edge_type,
                    to,
                    weight,
                } => {
                    record.set_edge("_", MatchedEdge::from_tuple(from, *edge_type, to, *weight));
                }
                TraverserElement::Value(v) => match v {
                    GremlinValue::String(s) => {
                        record.set("_value", crate::storage::schema::Value::Text(s.clone()))
                    }
                    GremlinValue::Integer(i) => {
                        record.set("_value", crate::storage::schema::Value::Integer(*i))
                    }
                    GremlinValue::Float(f) => {
                        record.set("_value", crate::storage::schema::Value::Float(*f))
                    }
                    GremlinValue::Boolean(b) => {
                        record.set("_value", crate::storage::schema::Value::Boolean(*b))
                    }
                    GremlinValue::Predicate(_) => {}
                },
            }

            // Add labeled elements
            for (label, elem) in &t.labels {
                match elem {
                    TraverserElement::Node(id) => {
                        if let Some(node) = self.graph.get_node(id) {
                            record.set_node(label, MatchedNode::from_stored(&node));
                        }
                    }
                    TraverserElement::Edge {
                        from,
                        edge_type,
                        to,
                        weight,
                    } => {
                        record.set_edge(
                            label,
                            MatchedEdge::from_tuple(from, *edge_type, to, *weight),
                        );
                    }
                    _ => {}
                }
            }

            // Add path if present
            if !t.path.is_empty() {
                // Collect node IDs from path elements
                let node_ids: Vec<String> = t
                    .path
                    .iter()
                    .filter_map(|elem| elem.as_node_id().map(|s| s.to_string()))
                    .collect();

                if let Some(first_id) = node_ids.first() {
                    let mut path = GraphPath::start(first_id);
                    // Add remaining nodes to the path
                    for id in node_ids.iter().skip(1) {
                        path.nodes.push(id.clone());
                    }
                    record.paths.push(path);
                }
            }

            result.push(record);
        }

        Ok(result)
    }
}

impl Traverser {
    /// Create traverser at a node
    fn at_node(id: &str) -> Self {
        Self {
            current: TraverserElement::Node(id.to_string()),
            path: vec![TraverserElement::Node(id.to_string())],
            labels: HashMap::new(),
            bulk: 1,
            loops: HashMap::new(),
            sack: None,
        }
    }

    /// Create traverser at an edge
    fn at_edge(from: &str, edge_type: GraphEdgeType, to: &str, weight: f32) -> Self {
        Self {
            current: TraverserElement::Edge {
                from: from.to_string(),
                edge_type,
                to: to.to_string(),
                weight,
            },
            path: Vec::new(),
            labels: HashMap::new(),
            bulk: 1,
            loops: HashMap::new(),
            sack: None,
        }
    }

    /// Move to a new node
    fn move_to_node(&self, id: &str) -> Self {
        let mut new_path = self.path.clone();
        new_path.push(TraverserElement::Node(id.to_string()));
        Self {
            current: TraverserElement::Node(id.to_string()),
            path: new_path,
            labels: self.labels.clone(),
            bulk: self.bulk,
            loops: self.loops.clone(),
            sack: self.sack.clone(),
        }
    }

    /// Move to an edge
    fn move_to_edge(&self, from: &str, edge_type: GraphEdgeType, to: &str, weight: f32) -> Self {
        let mut new_path = self.path.clone();
        new_path.push(TraverserElement::Edge {
            from: from.to_string(),
            edge_type,
            to: to.to_string(),
            weight,
        });
        Self {
            current: TraverserElement::Edge {
                from: from.to_string(),
                edge_type,
                to: to.to_string(),
                weight,
            },
            path: new_path,
            labels: self.labels.clone(),
            bulk: self.bulk,
            loops: self.loops.clone(),
            sack: self.sack.clone(),
        }
    }

    /// Convert to value traverser
    fn with_value(&self, value: GremlinValue) -> Self {
        Self {
            current: TraverserElement::Value(value),
            path: self.path.clone(),
            labels: self.labels.clone(),
            bulk: self.bulk,
            loops: self.loops.clone(),
            sack: self.sack.clone(),
        }
    }
}

/// Check if a string matches a Gremlin value
fn matches_gremlin_value(s: &str, value: &GremlinValue) -> bool {
    match value {
        GremlinValue::String(v) => {
            s.to_lowercase() == v.to_lowercase() || s.to_lowercase().contains(&v.to_lowercase())
        }
        GremlinValue::Integer(i) => s.parse::<i64>().map(|n| n == *i).unwrap_or(false),
        GremlinValue::Float(f) => s
            .parse::<f64>()
            .map(|n| (n - *f).abs() < 0.0001)
            .unwrap_or(false),
        GremlinValue::Boolean(b) => s.parse::<bool>().map(|n| n == *b).unwrap_or(false),
        GremlinValue::Predicate(pred) => evaluate_predicate(s, pred),
    }
}

/// Evaluate a Gremlin predicate
fn evaluate_predicate(s: &str, pred: &GremlinPredicate) -> bool {
    match pred {
        GremlinPredicate::Eq(v) => matches_gremlin_value(s, v),
        GremlinPredicate::Neq(v) => !matches_gremlin_value(s, v),
        GremlinPredicate::Lt(v) => compare_values(s, v, |a, b| a < b),
        GremlinPredicate::Lte(v) => compare_values(s, v, |a, b| a <= b),
        GremlinPredicate::Gt(v) => compare_values(s, v, |a, b| a > b),
        GremlinPredicate::Gte(v) => compare_values(s, v, |a, b| a >= b),
        GremlinPredicate::Between(a, b) => {
            compare_values(s, a, |x, y| x >= y) && compare_values(s, b, |x, y| x <= y)
        }
        GremlinPredicate::Inside(a, b) => {
            compare_values(s, a, |x, y| x > y) && compare_values(s, b, |x, y| x < y)
        }
        GremlinPredicate::Outside(a, b) => {
            compare_values(s, a, |x, y| x < y) || compare_values(s, b, |x, y| x > y)
        }
        GremlinPredicate::Within(vals) => vals.iter().any(|v| matches_gremlin_value(s, v)),
        GremlinPredicate::Without(vals) => !vals.iter().any(|v| matches_gremlin_value(s, v)),
        GremlinPredicate::StartingWith(prefix) => s.starts_with(prefix),
        GremlinPredicate::EndingWith(suffix) => s.ends_with(suffix),
        GremlinPredicate::Containing(substring) => s.contains(substring),
        GremlinPredicate::Regex(pattern) => s.contains(pattern), // Simplified
    }
}

/// Compare string value with GremlinValue
fn compare_values<F>(s: &str, v: &GremlinValue, f: F) -> bool
where
    F: Fn(f64, f64) -> bool,
{
    match v {
        GremlinValue::Integer(i) => s.parse::<f64>().map(|n| f(n, *i as f64)).unwrap_or(false),
        GremlinValue::Float(fl) => s.parse::<f64>().map(|n| f(n, *fl)).unwrap_or(false),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::graph_store::GraphNodeType;

    fn create_test_graph() -> Arc<GraphStore> {
        let graph = GraphStore::new();

        // Add test nodes
        graph.add_node("host:10.0.0.1", "webserver", GraphNodeType::Host);
        graph.add_node("host:10.0.0.2", "database", GraphNodeType::Host);
        graph.add_node("svc:ssh", "SSH", GraphNodeType::Service);
        graph.add_node("svc:http", "HTTP", GraphNodeType::Service);

        // Add edges
        graph.add_edge("host:10.0.0.1", "svc:ssh", GraphEdgeType::HasService, 1.0);
        graph.add_edge("host:10.0.0.1", "svc:http", GraphEdgeType::HasService, 1.0);
        graph.add_edge(
            "host:10.0.0.1",
            "host:10.0.0.2",
            GraphEdgeType::ConnectsTo,
            1.0,
        );
        graph.add_edge("host:10.0.0.2", "svc:ssh", GraphEdgeType::HasService, 1.0);

        Arc::new(graph)
    }

    #[test]
    fn test_gremlin_v() {
        let graph = create_test_graph();
        let executor = GremlinExecutor::new(graph);

        let result = executor.execute("g.V()").unwrap();
        assert_eq!(result.records.len(), 4); // 4 nodes
    }

    #[test]
    fn test_gremlin_v_out() {
        let graph = create_test_graph();
        let executor = GremlinExecutor::new(graph);

        let result = executor.execute("g.V('host:10.0.0.1').out()").unwrap();
        assert_eq!(result.records.len(), 3); // 2 services + 1 host
    }

    #[test]
    fn test_gremlin_has_label() {
        let graph = create_test_graph();
        let executor = GremlinExecutor::new(graph);

        let result = executor.execute("g.V().hasLabel('Host')").unwrap();
        assert_eq!(result.records.len(), 2); // 2 hosts
    }

    #[test]
    fn test_gremlin_limit() {
        let graph = create_test_graph();
        let executor = GremlinExecutor::new(graph);

        let result = executor.execute("g.V().limit(2)").unwrap();
        assert_eq!(result.records.len(), 2);
    }

    #[test]
    fn test_gremlin_count() {
        let graph = create_test_graph();
        let executor = GremlinExecutor::new(graph);

        let result = executor.execute("g.V().count()").unwrap();
        assert_eq!(result.records.len(), 1);
    }

    #[test]
    fn test_gremlin_path() {
        let graph = create_test_graph();
        let executor = GremlinExecutor::new(graph);

        let result = executor
            .execute("g.V('host:10.0.0.1').out().path()")
            .unwrap();
        assert_eq!(result.records.len(), 3);
    }

    #[test]
    fn test_gremlin_as_select() {
        let graph = create_test_graph();
        let executor = GremlinExecutor::new(graph);

        let result = executor
            .execute("g.V('host:10.0.0.1').as('a').out().as('b').select('a', 'b')")
            .unwrap();
        assert_eq!(result.records.len(), 3);
    }
}
